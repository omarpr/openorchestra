//! Input to a booted simulator through SimulatorKit's private legacy HID
//! client (macOS only): the loader, [`Client`] and [`Message`] that touch,
//! swipe and keys (the Device controls plan's Task 5) would build on. Nothing
//! uses it yet, and Home doesn't: on iOS 27.2 the guest ignores legacy HID
//! input on most boots (its CoreDevice `dtuhidd` disconnects the legacy
//! services) while every send still completes without error, so Home is the
//! public `simctl launch … com.apple.springboard` in `controls`.
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

// Kept for touch and keys (Task 5); nothing calls it yet.
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
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{msg_send, sel};
use objc2_foundation::{NSClassFromString, NSError, NSString};

use super::StreamError;
use super::sim_display::{
    booted_device, catching, check_paths, developer_dir, framework_paths, load_core_simulator,
    msg_send_fn, ns_error,
};

/// Relative to the developer dir's parent (`…/Xcode.app/Contents`).
const SIMULATOR_KIT: &str = "SharedFrameworks/SimulatorKit.framework/SimulatorKit";
const CLIENT_CLASS: &str = "SimulatorKit.SimDeviceLegacyHIDClient";
const INIT_NAME: &str = "-[SimDeviceLegacyHIDClient initWithDevice:error:]";
const SEND_NAME: &str =
    "-[SimDeviceLegacyHIDClient sendWithMessage:freeWhenDone:completionQueue:completion:]";
/// The builders touch and keys need. The HID input counts as there only
/// when all of them are.
const BUILDERS: [&CStr; 2] = [
    c"IndigoHIDMessageForMouseNSEvent",
    c"IndigoHIDMessageForKeyboardArbitrary",
];
/// The only target ever to be sent (see the module docs).
const TARGET: u32 = 0x32;
/// How long a send waits for the client's completion.
const ACK_TIMEOUT: Duration = Duration::from_secs(5);

/// What loading SimulatorKit found.
struct Hid {
    developer_dir: PathBuf,
    class: &'static AnyClass,
}

/// Where SimulatorKit is for the developer dir `xcode-select -p` gives.
fn simulator_kit_path(developer_dir: &Path) -> Option<PathBuf> {
    Some(developer_dir.parent()?.join(SIMULATOR_KIT))
}

/// Whether the private HID input is there (which says nothing about whether
/// the guest heeds it: see the module docs). Loads SimulatorKit on the first
/// call, which blocks (`xcode-select`): call it off the main thread.
pub(crate) fn hid_available() -> bool {
    loaded().is_ok()
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

    for name in BUILDERS {
        // SAFETY: a live handle and a C string.
        let symbol = unsafe { libc::dlsym(handle, name.as_ptr()) };
        if symbol.is_null() {
            return Err(StreamError::Unsupported(name.to_str().unwrap_or("IndigoHIDMessage")));
        }
    }
    Ok(Hid {
        developer_dir,
        class,
    })
}

fn init_sel() -> Sel {
    sel!(initWithDevice:error:)
}

fn send_sel() -> Sel {
    sel!(sendWithMessage:freeWhenDone:completionQueue:completion:)
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
}
