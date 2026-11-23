//! Input to a booted simulator through SimulatorKit's private legacy HID
//! client (macOS only). Home for now; touch, swipe and keys build on the same
//! [`Client`] and [`Message`].
//!
//! The calls were measured in the Device controls spike (see "Spike findings"
//! in docs/superpowers/specs/2026-11-23-device-tab-controls-design.md):
//!
//! - SimulatorKit is `dlopen`ed from the selected Xcode's
//!   `Contents/SharedFrameworks`, after CoreSimulator. The
//!   `IndigoHIDMessageFor…` builders come from `dlsym`; each `calloc`s a
//!   message, or returns NULL.
//! - `SimulatorKit.SimDeviceLegacyHIDClient` is a real class, made with
//!   `initWithDevice:error:` on the `SimDevice` that `sim_display` finds. A
//!   client is made per action: after a send error (the guest's HID port
//!   died) it stays dead, and making one takes only 2–24 ms.
//! - `sendWithMessage:freeWhenDone:completionQueue:completion:` sends on the
//!   calling thread and, given `freeWhenDone: YES`, frees the message. Its
//!   completion runs only when a queue is passed too, with nil or the error.
//! - Every message targets [`TARGET`] (0x32). SimulatorKit's own button
//!   target (0x33) is ignored by the guest, and wheel events at 0x35 crashed
//!   the guest's backboardd: never send any other target.
//! - As in `sim_display`, messages are sent through a typed `objc_msgSend`
//!   after checking the selector, inside `objc2::exception::catch`, and the
//!   completion block carries its signature.

// Nothing calls Home until the bar (the Device controls plan's Task 3).
#![allow(dead_code)]

use std::ffi::{CStr, CString, c_void};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};
use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::Duration;

use block2::{ManualBlockEncoding, RcBlock};
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{msg_send, sel};
use objc2_foundation::{NSClassFromString, NSError, NSString};

use super::StreamError;
use super::sim_display::{
    booted_device, catching, check_paths, developer_dir, framework_paths, load_core_simulator,
    msg_send_fn, ns_error,
};
use super::simctl::simctl;

/// Relative to the developer dir's parent (`…/Xcode.app/Contents`).
const SIMULATOR_KIT: &str = "SharedFrameworks/SimulatorKit.framework/SimulatorKit";
const CLIENT_CLASS: &str = "SimulatorKit.SimDeviceLegacyHIDClient";
const INIT_NAME: &str = "-[SimDeviceLegacyHIDClient initWithDevice:error:]";
const SEND_NAME: &str =
    "-[SimDeviceLegacyHIDClient sendWithMessage:freeWhenDone:completionQueue:completion:]";
/// The builders the HID input needs: Home's, and the touch and keyboard
/// ones. Home is offered only when all of them are there.
const BUILDERS: [&CStr; 3] = [
    c"IndigoHIDMessageForButton",
    c"IndigoHIDMessageForMouseNSEvent",
    c"IndigoHIDMessageForKeyboardArbitrary",
];
/// The only target ever sent (see the module docs).
const TARGET: u32 = 0x32;
/// `IndigoHIDMessageForButton` key codes and ops.
const BUTTON_HOME: u32 = 0;
const DOWN: u32 = 1;
const UP: u32 = 2;
/// How long a send waits for the client's completion.
const ACK_TIMEOUT: Duration = Duration::from_secs(5);
const SPRINGBOARD: &str = "com.apple.springboard";

/// `IndigoHIDMessage *IndigoHIDMessageForButton(uint32_t keyCode, uint32_t op, uint32_t target)`.
type ButtonFn = unsafe extern "C" fn(u32, u32, u32) -> *mut c_void;

/// What loading SimulatorKit found.
struct Hid {
    developer_dir: PathBuf,
    class: &'static AnyClass,
    button: ButtonFn,
}

/// Where SimulatorKit is for the developer dir `xcode-select -p` gives.
fn simulator_kit_path(developer_dir: &Path) -> Option<PathBuf> {
    Some(developer_dir.parent()?.join(SIMULATOR_KIT))
}

/// Home's button presses, as (key code, op): down, then straight up.
fn home_press() -> [(u32, u32); 2] {
    [(BUTTON_HOME, DOWN), (BUTTON_HOME, UP)]
}

/// Whether the private HID input is there. Loads SimulatorKit on the first
/// call, which blocks (`xcode-select`): call it off the main thread.
pub(crate) fn hid_available() -> bool {
    loaded().is_ok()
}

/// Presses the Home button of booted simulator `udid`. Without the private
/// HID input it brings SpringBoard forward instead (`simctl launch`), which
/// shows the home screen but is not a button press. Blocks: call it off the
/// main thread.
pub(crate) fn press_home(udid: &str) -> Result<(), StreamError> {
    let hid = match loaded() {
        Ok(hid) => hid,
        Err(why) => {
            log::debug!("device: no HID input ({why:?}); Home brings SpringBoard forward");
            return launch_springboard(udid);
        }
    };
    autoreleasepool(|_| {
        let client = Client::connect(hid, udid)?;
        for (key_code, op) in home_press() {
            client.send(hid.button(key_code, op)?)?;
        }
        Ok(())
    })
}

/// The public stand-in for Home: `simctl launch <udid> com.apple.springboard`.
fn launch_springboard(udid: &str) -> Result<(), StreamError> {
    simctl(&["launch", udid, SPRINGBOARD]).map(drop).map_err(|why| {
        if why.starts_with("Invalid device") {
            StreamError::DeviceNotFound
        } else {
            StreamError::Failed(why)
        }
    })
}

/// SimulatorKit, loaded and checked once per process.
fn loaded() -> Result<&'static Hid, StreamError> {
    static HID: OnceLock<Result<Hid, StreamError>> = OnceLock::new();
    HID.get_or_init(load).as_ref().map_err(Clone::clone)
}

fn load() -> Result<Hid, StreamError> {
    let developer_dir = developer_dir()?;
    check_paths(&framework_paths(&developer_dir), |p| p.exists())?;
    load_core_simulator()?;
    let path = simulator_kit_path(&developer_dir)
        .filter(|p| p.exists())
        .ok_or(StreamError::Unsupported("SimulatorKit.framework"))?;
    let path = CString::new(path.to_string_lossy().into_owned())
        .map_err(|_| StreamError::Unsupported("SimulatorKit.framework"))?;
    // SAFETY: a valid C string; the handle is kept for the process.
    let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW) };
    if handle.is_null() {
        // SAFETY: dlerror returns NULL or a C string valid until the next dl
        // call on this thread.
        let why = unsafe {
            let err = libc::dlerror();
            (!err.is_null()).then(|| CStr::from_ptr(err).to_string_lossy().into_owned())
        };
        return Err(StreamError::Failed(format!(
            "loading SimulatorKit: {}",
            why.unwrap_or_else(|| "unknown error".into())
        )));
    }

    let class = NSClassFromString(&NSString::from_str(CLIENT_CLASS))
        .ok_or(StreamError::Unsupported(CLIENT_CLASS))?;
    for (sel, name) in [(init_sel(), INIT_NAME), (send_sel(), SEND_NAME)] {
        if !class.responds_to(sel) {
            return Err(StreamError::Unsupported(name));
        }
    }

    let mut button = ptr::null_mut();
    for name in BUILDERS {
        // SAFETY: a live handle and a C string.
        let symbol = unsafe { libc::dlsym(handle, name.as_ptr()) };
        if symbol.is_null() {
            return Err(StreamError::Unsupported(name.to_str().unwrap_or("IndigoHIDMessage")));
        }
        if name == c"IndigoHIDMessageForButton" {
            button = symbol;
        }
    }
    // SAFETY: the symbol is `IndigoHIDMessageForButton`, of type `ButtonFn`
    // (from the spike's disassembly and runs).
    let button = unsafe { std::mem::transmute::<*mut c_void, ButtonFn>(button) };
    Ok(Hid {
        developer_dir,
        class,
        button,
    })
}

fn init_sel() -> Sel {
    sel!(initWithDevice:error:)
}

fn send_sel() -> Sel {
    sel!(sendWithMessage:freeWhenDone:completionQueue:completion:)
}

impl Hid {
    /// A button event at [`TARGET`].
    fn button(&self, key_code: u32, op: u32) -> Result<Message, StreamError> {
        // SAFETY: plain integers in; a `calloc`ed message or NULL out.
        let raw = unsafe { (self.button)(key_code, op, TARGET) };
        Message::new(raw, "IndigoHIDMessageForButton")
    }
}

/// An `IndigoHIDMessage` a builder `calloc`ed. Freed on drop unless sent:
/// sending hands it to the client.
struct Message(NonNull<c_void>);

impl Message {
    /// `Failed` for a NULL message, which must never be sent (the touch
    /// builder returns NULL for a move too soon after the last).
    fn new(raw: *mut c_void, builder: &str) -> Result<Self, StreamError> {
        NonNull::new(raw)
            .map(Message)
            .ok_or_else(|| StreamError::Failed(format!("{builder} made no message")))
    }

    fn into_raw(self) -> *mut c_void {
        let raw = self.0.as_ptr();
        std::mem::forget(self);
        raw
    }

    /// The `u32` at `offset`, within the 0x20-byte header and the event
    /// count's 0xA0-byte events.
    #[cfg(test)]
    fn read_u32(&self, offset: usize) -> u32 {
        // SAFETY: the count byte is inside the header every message has.
        let count = unsafe { *self.0.as_ptr().cast::<u8>().add(0x1C) };
        assert!(offset + 4 <= 0x20 + usize::from(count) * 0xA0, "{offset:#x} past the message");
        // SAFETY: in bounds, checked above; read unaligned to be safe.
        unsafe { self.0.as_ptr().cast::<u8>().add(offset).cast::<u32>().read_unaligned() }
    }
}

impl Drop for Message {
    fn drop(&mut self) {
        // SAFETY: the builders `calloc` the message, and it wasn't sent.
        unsafe { libc::free(self.0.as_ptr()) }
    }
}

/// The send's completion, `void (^)(NSError *)`.
struct CompletionEncoding;
// SAFETY: the encoding matches the arguments and return type.
unsafe impl ManualBlockEncoding for CompletionEncoding {
    type Arguments = (*mut AnyObject,);
    type Return = ();
    const ENCODING_CSTR: &'static CStr = c"v16@?0@\"NSError\"8";
}

/// A HID client for one booted simulator, made per action.
struct Client {
    client: Retained<AnyObject>,
    /// Where completions run: without a queue they never do.
    queue: DispatchRetained<DispatchQueue>,
}

impl Client {
    /// A client for booted device `udid`; `DeviceNotFound` if it isn't one.
    fn connect(hid: &Hid, udid: &str) -> Result<Client, StreamError> {
        let device = booted_device(&hid.developer_dir, udid)?;
        type Alloc = unsafe extern "C-unwind" fn(*const AnyClass, Sel) -> *mut AnyObject;
        type Init = unsafe extern "C-unwind" fn(
            *mut AnyObject,
            Sel,
            *const AnyObject,
            *mut *mut NSError,
        ) -> *mut AnyObject;
        // SAFETY: `+alloc` is `@16@0:8`; `-initWithDevice:error:` is
        // `@32@0:8@16^@24`, checked by `load`.
        let (alloc, init) = unsafe { (msg_send_fn::<Alloc>(), msg_send_fn::<Init>()) };
        let mut error: *mut NSError = ptr::null_mut();
        let raw = catching(|| unsafe {
            let fresh = alloc(hid.class, sel!(alloc));
            if fresh.is_null() {
                return fresh;
            }
            init(fresh, init_sel(), Retained::as_ptr(&device), &mut error)
        })?;
        // SAFETY: `init` returns +1 (or nil, having released the
        // allocation), so this takes ownership.
        if let Some(client) = unsafe { Retained::from_raw(raw) } {
            let queue = DispatchQueue::new("openOrchestra.device-hid", DispatchQueueAttr::SERIAL);
            return Ok(Client { client, queue });
        }
        // SAFETY: an autoreleased error (or nil) from the failed init.
        Err(match unsafe { Retained::retain(error) } {
            Some(error) => ns_error(error),
            None => StreamError::Failed("SimulatorKit made no HID client".into()),
        })
    }

    /// Sends `message` and waits for the client to say it went (the send
    /// itself returns at once). An error means the guest's HID port is gone,
    /// and this client with it.
    fn send(&self, message: Message) -> Result<(), StreamError> {
        let (tx, rx) = mpsc::channel();
        let completion = RcBlock::with_encoding::<_, _, _, CompletionEncoding>(
            move |error: *mut AnyObject| {
                let _ = tx.send(completion_error(error));
            },
        );
        type Send = unsafe extern "C-unwind" fn(
            *const AnyObject,
            Sel,
            *mut c_void,
            bool,
            *const DispatchQueue,
            *const c_void,
        );
        // SAFETY: `v44@0:8^{IndigoHIDMessageStruct=…}16B24@28@?36`, checked
        // by `load`.
        let send = unsafe { msg_send_fn::<Send>() };
        // The client owns the message from here. Should the send raise
        // part-way, it is leaked rather than risk freeing it twice.
        let raw = message.into_raw();
        catching(|| unsafe {
            send(
                Retained::as_ptr(&self.client),
                send_sel(),
                raw,
                true,
                &*self.queue,
                ptr::from_ref(&*completion).cast(),
            )
        })?;
        match rx.recv_timeout(ACK_TIMEOUT) {
            Ok(None) => Ok(()),
            Ok(Some(why)) => Err(StreamError::Failed(why)),
            Err(_) => Err(StreamError::Failed(format!(
                "the simulator didn't take the input within {} s",
                ACK_TIMEOUT.as_secs()
            ))),
        }
    }
}

/// The completion's error, described: `…HIDError.machPortInvalid` and the
/// like. Runs on the client's queue, so it never panics.
fn completion_error(error: *mut AnyObject) -> Option<String> {
    // SAFETY: an `NSError` (or nil) the block holds for its duration.
    let error = unsafe { error.as_ref() }?;
    let described = std::panic::catch_unwind(AssertUnwindSafe(|| {
        catching(|| {
            // SAFETY: `-description` returns an NSString.
            let text: Retained<NSString> = unsafe { msg_send![error, description] };
            text.to_string()
        })
        .ok()
    }));
    Some(
        described
            .ok()
            .flatten()
            .unwrap_or_else(|| "the simulator refused the input".into()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simulator_kit_is_in_the_selected_xcodes_shared_frameworks() {
        assert_eq!(
            simulator_kit_path(Path::new("/Applications/Xcode-beta.app/Contents/Developer")),
            Some(PathBuf::from(
                "/Applications/Xcode-beta.app/Contents/SharedFrameworks/SimulatorKit.framework/SimulatorKit"
            ))
        );
        assert_eq!(simulator_kit_path(Path::new("/")), None);
    }

    #[test]
    fn home_is_the_home_button_down_then_up() {
        assert_eq!(home_press(), [(BUTTON_HOME, DOWN), (BUTTON_HOME, UP)]);
        assert_eq!((BUTTON_HOME, DOWN, UP), (0, 1, 2));
    }

    #[test]
    fn home_messages_target_0x32_only() {
        // Builds the messages (no device needed) where this Mac's Xcode has
        // the HID builders; elsewhere there is nothing to check.
        let Ok(hid) = loaded() else {
            eprintln!("skipped: no SimulatorKit HID input here");
            return;
        };
        for (key_code, op) in home_press() {
            let message = hid.button(key_code, op).unwrap();
            // Header: event stride and count, then one button event.
            assert_eq!(message.read_u32(0x18), 0xA0);
            assert_eq!(message.read_u32(0x1C) & 0xFF, 1);
            assert_eq!(message.read_u32(0x20), 2, "button/key event type");
            assert_eq!(message.read_u32(0x30), key_code);
            assert_eq!(message.read_u32(0x34), op);
            assert_eq!(message.read_u32(0x38), 0x32, "target");
        }
    }

    fn png(udid: &str) -> Vec<u8> {
        use super::super::controls::{Control, ControlOutcome, run_public};
        match run_public(udid, Control::Screenshot) {
            Ok(ControlOutcome::Png(bytes)) => bytes,
            other => panic!("screenshot: {other:?}"),
        }
    }

    /// The mean brightness (0-255) of the middle of a screenshot: Settings
    /// is near white (245 in the spike), the home screen's wallpaper much
    /// darker (139).
    fn brightness(png: &[u8]) -> f64 {
        let image = image::load_from_memory(png).unwrap().to_luma8();
        let (w, h) = image.dimensions();
        let (mut sum, mut n) = (0.0, 0.0);
        for y in (h / 5..h * 4 / 5).step_by(7) {
            for x in (w / 5..w * 4 / 5).step_by(7) {
                sum += f64::from(image.get_pixel(x, y)[0]);
                n += 1.0;
            }
        }
        sum / n
    }

    fn launch(udid: &str, bundle: &str) {
        simctl(&["launch", udid, bundle]).unwrap();
        // A first launch shows a placeholder for a moment.
        std::thread::sleep(Duration::from_secs(3));
    }

    /// Manual, on a simulator created and booted for the purpose (never
    /// anyone else's):
    /// `OO_TEST_SIM_UDID=<udid> cargo test rotate_and_home_on -- --ignored --nocapture`.
    /// Turns it four times in Safari, then presses Home over Settings, by the
    /// HID path and by the public stand-in.
    ///
    /// The HID Home passes only on a boot where the guest kept its legacy
    /// HID services: on iOS 27.2 most boots log "dtuhidd state changed to
    /// active" and disconnect them, and the presses are then ignored though
    /// they complete without error (Task 2's report has the details).
    #[test]
    #[ignore]
    fn rotate_and_home_on_a_live_device() {
        use super::super::controls::{rotate, rotate_available};
        use super::super::sim_display::{ScreenEvent, ScreenStream};
        const NO_DEVICE: &str = "00000000-0000-0000-0000-000000000000";

        let udid = std::env::var("OO_TEST_SIM_UDID").expect("set OO_TEST_SIM_UDID");
        assert!(rotate_available(), "devicectl can't rotate");
        assert!(hid_available(), "no HID input: {:?}", loaded().err());

        // A device fresh from boot reports "unknown"; start from portrait.
        // Safari turns with the device (the home screen and Settings don't).
        let set = ["devicectl", "device", "orientation", "set", "--device", &udid, "portrait"];
        let ran = crate::ports::run_bounded("xcrun", &set, Duration::from_secs(5)).unwrap();
        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        launch(&udid, "com.apple.mobilesafari");
        let (tx, rx) = mpsc::channel();
        let stream = ScreenStream::start(&udid, move |event| {
            if let ScreenEvent::Orientation(o) = event {
                let _ = tx.send(o);
            }
        })
        .unwrap();
        let start = rx.recv_timeout(Duration::from_secs(2)).expect("the starting orientation");
        assert_eq!(start, 1, "portrait");
        let mut seen = Vec::new();
        for turn in 1..=4 {
            rotate(&udid).unwrap();
            std::thread::sleep(Duration::from_millis(1500));
            let now: Vec<u32> = rx.try_iter().collect();
            eprintln!("turn {turn}: {now:?}");
            seen.extend(now);
        }
        drop(stream);
        // Safari refuses upside down, so one turn changes nothing: 3, -, 4, 1.
        assert!(seen.contains(&3) && seen.contains(&4), "{seen:?}");
        assert_eq!(seen.last(), Some(&start), "four turns come back round: {seen:?}");

        launch(&udid, "com.apple.Preferences");
        let settings = png(&udid);
        press_home(&udid).unwrap();
        std::thread::sleep(Duration::from_secs(1));
        let (settings, home) = (brightness(&settings), brightness(&png(&udid)));
        eprintln!("brightness: Settings {settings:.0}, after Home {home:.0}");
        assert!(settings - home > 50.0, "Home left Settings in front");

        launch(&udid, "com.apple.Preferences");
        let settings = png(&udid);
        launch_springboard(&udid).unwrap();
        std::thread::sleep(Duration::from_secs(1));
        let (settings, home) = (brightness(&settings), brightness(&png(&udid)));
        eprintln!("brightness: Settings {settings:.0}, after SpringBoard {home:.0}");
        assert!(settings - home > 50.0, "SpringBoard left Settings in front");

        assert_eq!(press_home(NO_DEVICE), Err(StreamError::DeviceNotFound));
        assert_eq!(launch_springboard(NO_DEVICE), Err(StreamError::DeviceNotFound));
        let why = rotate(NO_DEVICE).unwrap_err();
        assert!(why.starts_with("The specified device was not found."), "{why}");
    }
}
