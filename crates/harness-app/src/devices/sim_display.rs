//! A booted simulator's screen, live, through Apple's private CoreSimulator
//! API (macOS only). Watch-only: it registers for the screen's callbacks and
//! nothing else; it never boots, resizes, rotates or sends input.
//!
//! The call chain was measured in the Device tab spike (see "Spike findings"
//! in docs/superpowers/specs/2026-11-22-device-tab-ios-design.md):
//! `+[SimServiceContext sharedServiceContextForDeveloperDir:error:]` →
//! `defaultDeviceSetWithError:` → `devicesByUDID[udid]` → `io` → `ioPorts` →
//! each port's `descriptor`. The main display is the descriptor whose
//! `state.displayClass` is 0; it takes
//! `registerScreenCallbacksWithUUID:callbackQueue:frameCallback:surfacesChangedCallback:propertiesChangedCallback:`.
//!
//! - Only CoreSimulator is loaded. SimulatorKit isn't needed for the screen;
//!   its presence beside the selected developer dir is only the check that a
//!   full Xcode, not the Command Line Tools, is selected.
//! - The ports, descriptors and their state are ROCK remote proxies whose
//!   classes are generated at run time and answer by forwarding: their
//!   classes have none of these methods, so objc2's `msg_send!` (which checks
//!   the class's method in debug builds) can't be used on them. They're sent
//!   to straight through `objc_msgSend` instead, after `respondsToSelector:`.
//! - The callback blocks must carry a signature, or registering raises an
//!   Objective-C exception; the surfaces block's object arguments must be
//!   encoded `@`, or it is silently never called.
//! - Every message that can raise runs inside `objc2::exception::catch`.

use std::ffi::{CStr, CString, c_void};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use block2::{ManualBlockEncoding, RcBlock};
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, Sel};
use objc2::{AllocAnyThread, exception, msg_send, sel};
use objc2_core_foundation::CFRetained;
use objc2_foundation::{NSClassFromString, NSError, NSString, NSUUID};
use objc2_io_surface::IOSurfaceRef;

use super::StreamError;
use crate::ports::run_bounded;

const CORE_SIMULATOR: &str =
    "/Library/Developer/PrivateFrameworks/CoreSimulator.framework/CoreSimulator";
/// Relative to the developer dir's parent (`…/Xcode.app/Contents`).
const SIMULATOR_KIT: &str = "SharedFrameworks/SimulatorKit.framework/SimulatorKit";
/// `-[SimDevice state]` of a booted device.
const BOOTED: u64 = 3;
/// `displayClass` of the device's own screen (the others are external
/// displays: TV out, wireless, resizable).
const MAIN_DISPLAY_CLASS: u16 = 0;
const XCODE_SELECT_TIMEOUT: Duration = Duration::from_secs(5);
const REGISTER_NAME: &str = "-[SimScreen registerScreenCallbacksWithUUID:callbackQueue:frameCallback:surfacesChangedCallback:propertiesChangedCallback:]";

/// What the simulator's main screen did.
pub(crate) enum ScreenEvent {
    /// A new or resized surface: the full rectangular picture, updated in
    /// place from now on.
    Surface(CFRetained<IOSurfaceRef>),
    /// New pixels in the current surface.
    Frame,
    /// The device shut down or the display went away.
    Gone,
}

/// The frameworks the stream needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrameworkPaths {
    pub core_simulator: PathBuf,
    pub simulator_kit: PathBuf,
}

/// Where the frameworks are for the developer dir `xcode-select -p` gives
/// (`…/Xcode.app/Contents/Developer`).
pub(crate) fn framework_paths(developer_dir: &Path) -> FrameworkPaths {
    let contents = developer_dir.parent().unwrap_or(developer_dir);
    FrameworkPaths {
        core_simulator: PathBuf::from(CORE_SIMULATOR),
        simulator_kit: contents.join(SIMULATOR_KIT),
    }
}

/// `NoXcode` unless both frameworks exist: a Command Line Tools developer dir
/// has no SimulatorKit beside it.
pub(crate) fn check_paths(
    paths: &FrameworkPaths,
    exists: impl Fn(&Path) -> bool,
) -> Result<(), StreamError> {
    if exists(&paths.core_simulator) && exists(&paths.simulator_kit) {
        Ok(())
    } else {
        Err(StreamError::NoXcode)
    }
}

/// The selected developer dir, from `xcode-select -p`.
pub(crate) fn developer_dir() -> Result<PathBuf, StreamError> {
    let ran =
        run_bounded("xcode-select", &["-p"], XCODE_SELECT_TIMEOUT).ok_or(StreamError::NoXcode)?;
    let dir = ran.stdout.trim();
    if ran.code != Some(0) || dir.is_empty() {
        return Err(StreamError::NoXcode);
    }
    Ok(PathBuf::from(dir))
}

/// `dlopen`s CoreSimulator, once per process.
fn load_core_simulator() -> Result<(), StreamError> {
    static LOADED: OnceLock<Result<(), StreamError>> = OnceLock::new();
    LOADED
        .get_or_init(|| {
            let path = CString::new(CORE_SIMULATOR).expect("no NUL in a constant path");
            // SAFETY: a valid C string; the handle is kept for the process.
            let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW) };
            if !handle.is_null() {
                return Ok(());
            }
            // SAFETY: dlerror returns NULL or a C string valid until the
            // next dl call on this thread.
            let why = unsafe {
                let err = libc::dlerror();
                if err.is_null() {
                    "unknown error".to_string()
                } else {
                    CStr::from_ptr(err).to_string_lossy().into_owned()
                }
            };
            Err(StreamError::Failed(format!("loading CoreSimulator: {why}")))
        })
        .clone()
}

/// A registration for one simulator's main-screen callbacks. Dropping it
/// unregisters them.
pub(crate) struct ScreenStream {
    screen: Retained<AnyObject>,
    registration: Retained<NSUUID>,
    shared: Arc<Shared>,
    _queue: DispatchRetained<DispatchQueue>,
}

/// What the callback blocks hold. It outlives the registration: a callback
/// already queued can still run just after unregistering, and finds
/// `stopped` set. A panicking `on_event` sets it too.
struct Shared {
    stopped: AtomicBool,
    on_event: Box<dyn Fn(ScreenEvent) + Send + Sync>,
}

impl Shared {
    fn emit(&self, event: ScreenEvent) {
        if !self.stopped.load(Ordering::Acquire) {
            self.deliver(event);
        }
    }

    /// Calls `on_event`, never letting a panic unwind out of the block into
    /// libdispatch (that would abort the app). After a panic the stream goes
    /// quiet.
    fn deliver(&self, event: ScreenEvent) {
        let delivered = std::panic::catch_unwind(AssertUnwindSafe(|| (self.on_event)(event)));
        if delivered.is_err() && !self.stopped.swap(true, Ordering::AcqRel) {
            log::error!("device: the screen event handler panicked; stopping the stream");
        }
    }

    /// `picture` is the surfaces callback's first argument: the full
    /// rectangular framebuffer (the second is the same picture with the
    /// device's rounded-corner mask). Nil when the device shut down.
    fn surfaces_changed(&self, picture: *mut AnyObject) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        let event = match NonNull::new(picture.cast::<IOSurfaceRef>()) {
            // SAFETY: the argument is an `IOSurface`, toll-free bridged to
            // `IOSurfaceRef`; retained to keep it past the callback.
            Some(surface) => ScreenEvent::Surface(unsafe { CFRetained::retain(surface) }),
            None => ScreenEvent::Gone,
        };
        self.deliver(event);
    }
}

impl ScreenStream {
    /// Starts watching the main screen of the booted simulator `udid`.
    /// `on_event` runs on a private serial dispatch queue, never on the main
    /// thread. The first `Surface` arrives just after this returns.
    pub(crate) fn start(
        udid: &str,
        on_event: impl Fn(ScreenEvent) + Send + Sync + 'static,
    ) -> Result<ScreenStream, StreamError> {
        let developer_dir = developer_dir()?;
        check_paths(&framework_paths(&developer_dir), |p| p.exists())?;
        load_core_simulator()?;
        autoreleasepool(|_| {
            let screen = find_main_screen(&developer_dir, udid)?;
            let shared = Arc::new(Shared {
                stopped: AtomicBool::new(false),
                on_event: Box::new(on_event),
            });
            let queue =
                DispatchQueue::new("openOrchestra.device-screen", DispatchQueueAttr::SERIAL);
            let registration = NSUUID::new();
            register(&screen, &registration, &queue, &shared)?;
            Ok(ScreenStream {
                screen,
                registration,
                shared,
                _queue: queue,
            })
        })
    }
}

impl Drop for ScreenStream {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::Release);
        autoreleasepool(|_| {
            let sel = sel!(unregisterScreenCallbacksWithUUID:);
            type Unregister = unsafe extern "C-unwind" fn(*const AnyObject, Sel, *const NSUUID);
            // SAFETY: `-unregisterScreenCallbacksWithUUID:` is
            // `v24@0:8@16`; `find_main_screen` checked the screen answers it.
            let unregister = unsafe { msg_send_fn::<Unregister>() };
            let result = catching(|| unsafe {
                unregister(
                    Retained::as_ptr(&self.screen),
                    sel,
                    Retained::as_ptr(&self.registration),
                )
            });
            if let Err(e) = result {
                log::warn!("device: unregistering the screen callbacks failed: {e:?}");
            }
        });
    }
}

/// The main display's descriptor (the `SimScreen`) of booted device `udid`.
fn find_main_screen(developer_dir: &Path, udid: &str) -> Result<Retained<AnyObject>, StreamError> {
    let context_class = NSClassFromString(&NSString::from_str("SimServiceContext"))
        .ok_or(StreamError::Unsupported("SimServiceContext"))?;
    let shared_context = sel!(sharedServiceContextForDeveloperDir:error:);
    // SAFETY: `+respondsToSelector:` takes a selector and returns BOOL.
    let class_responds: bool =
        catching(|| unsafe { msg_send![context_class, respondsToSelector: shared_context] })?;
    if !class_responds {
        return Err(StreamError::Unsupported(
            "+[SimServiceContext sharedServiceContextForDeveloperDir:error:]",
        ));
    }
    let dir = NSString::from_str(&developer_dir.to_string_lossy());
    // SAFETY: `@32@0:8@16^@24` (NSString *, NSError **), checked above.
    let context: Retained<AnyObject> = catching(|| unsafe {
        msg_send![context_class, sharedServiceContextForDeveloperDir: &*dir, error: _]
    })?
    .map_err(ns_error)?;

    require(
        &context,
        sel!(defaultDeviceSetWithError:),
        "-[SimServiceContext defaultDeviceSetWithError:]",
    )?;
    // SAFETY: `@24@0:8^@16` (NSError **), checked above.
    let device_set: Retained<AnyObject> =
        catching(|| unsafe { msg_send![&*context, defaultDeviceSetWithError: _] })?
            .map_err(ns_error)?;

    require(
        &device_set,
        sel!(devicesByUDID),
        "-[SimDeviceSet devicesByUDID]",
    )?;
    let devices = send_object(&device_set, sel!(devicesByUDID))?
        .ok_or_else(|| StreamError::Failed("no device list".into()))?;
    require(
        &devices,
        sel!(objectForKey:),
        "-[NSDictionary objectForKey:]",
    )?;
    let uuid = NSUUID::initWithUUIDString(NSUUID::alloc(), &NSString::from_str(udid))
        .ok_or(StreamError::DeviceNotFound)?;
    // SAFETY: `-objectForKey:` takes and returns an object, checked above.
    let device: Option<Retained<AnyObject>> =
        catching(|| unsafe { msg_send![&*devices, objectForKey: &*uuid] })?;
    let device = device.ok_or(StreamError::DeviceNotFound)?;

    require(&device, sel!(state), "-[SimDevice state]")?;
    type State = unsafe extern "C-unwind" fn(*const AnyObject, Sel) -> u64;
    // SAFETY: `-[SimDevice state]` is `Q16@0:8`, checked above.
    let state = unsafe { msg_send_fn::<State>() };
    if catching(|| unsafe { state(Retained::as_ptr(&device), sel!(state)) })? != BOOTED {
        return Err(StreamError::DeviceNotFound);
    }

    require(&device, sel!(io), "-[SimDevice io]")?;
    let io = send_object(&device, sel!(io))?
        .ok_or_else(|| StreamError::Failed("no device IO".into()))?;
    require(&io, sel!(ioPorts), "-[SimDeviceIOClient ioPorts]")?;
    let ports = send_object(&io, sel!(ioPorts))?
        .ok_or_else(|| StreamError::Failed("no IO ports".into()))?;
    require(&ports, sel!(count), "-[NSArray count]")?;
    require(&ports, sel!(objectAtIndex:), "-[NSArray objectAtIndex:]")?;
    // SAFETY: `-[NSArray count]` returns NSUInteger, checked above.
    let count: usize = catching(|| unsafe { msg_send![&*ports, count] })?;

    let register = sel!(registerScreenCallbacksWithUUID:callbackQueue:frameCallback:surfacesChangedCallback:propertiesChangedCallback:);
    let unregister = sel!(unregisterScreenCallbacksWithUUID:);
    let mut any_screen = false;
    // A screen that lacks what picks the main display.
    let mut missing = None;
    for i in 0..count {
        // SAFETY: `-objectAtIndex:` with an index below `count`.
        let port: Retained<AnyObject> =
            catching(|| unsafe { msg_send![&*ports, objectAtIndex: i] })?;
        if !responds(&port, sel!(descriptor))? {
            continue;
        }
        let Some(descriptor) = send_object(&port, sel!(descriptor))? else {
            continue;
        };
        if !responds(&descriptor, register)? || !responds(&descriptor, unregister)? {
            continue;
        }
        any_screen = true;
        if !responds(&descriptor, sel!(state))? {
            missing = Some("-[SimScreen state]");
            continue;
        }
        let Some(display) = send_object(&descriptor, sel!(state))? else {
            continue;
        };
        if !responds(&display, sel!(displayClass))? {
            missing = Some("-[SimDisplayDescriptorState displayClass]");
            continue;
        }
        type DisplayClass = unsafe extern "C-unwind" fn(*const AnyObject, Sel) -> u16;
        // SAFETY: `displayClass` is `S` (unsigned short), checked above.
        let display_class = unsafe { msg_send_fn::<DisplayClass>() };
        if catching(|| unsafe { display_class(Retained::as_ptr(&display), sel!(displayClass)) })?
            == MAIN_DISPLAY_CLASS
        {
            return Ok(descriptor);
        }
    }
    Err(match (missing, any_screen) {
        (Some(name), _) => StreamError::Unsupported(name),
        (None, true) => StreamError::Failed("the device has no main display".into()),
        (None, false) => StreamError::Unsupported(REGISTER_NAME),
    })
}

/// The frame callback, `void (^)(void)`.
struct FrameEncoding;
// SAFETY: the encoding matches the arguments and return type.
unsafe impl ManualBlockEncoding for FrameEncoding {
    type Arguments = ();
    type Return = ();
    const ENCODING_CSTR: &'static CStr = c"v8@?0";
}

/// The surfaces callback, `void (^)(IOSurface *framebuffer, IOSurface *masked)`.
struct SurfacesEncoding;
// SAFETY: as above; the objects must be `@`, not `^v`.
unsafe impl ManualBlockEncoding for SurfacesEncoding {
    type Arguments = (*mut AnyObject, *mut AnyObject);
    type Return = ();
    const ENCODING_CSTR: &'static CStr = c"v24@?0@\"IOSurface\"8@\"IOSurface\"16";
}

/// The properties callback, `void (^)(id<SimScreenProperties>)`.
struct PropertiesEncoding;
// SAFETY: as above.
unsafe impl ManualBlockEncoding for PropertiesEncoding {
    type Arguments = (*mut AnyObject,);
    type Return = ();
    const ENCODING_CSTR: &'static CStr = c"v16@?0@8";
}

/// Registers the three callbacks on `queue` under `registration`.
fn register(
    screen: &AnyObject,
    registration: &NSUUID,
    queue: &DispatchQueue,
    shared: &Arc<Shared>,
) -> Result<(), StreamError> {
    let frame = {
        let shared = Arc::clone(shared);
        RcBlock::with_encoding::<_, _, _, FrameEncoding>(move || shared.emit(ScreenEvent::Frame))
    };
    let surfaces = {
        let shared = Arc::clone(shared);
        RcBlock::with_encoding::<_, _, _, SurfacesEncoding>(
            move |picture: *mut AnyObject, _masked: *mut AnyObject| {
                shared.surfaces_changed(picture)
            },
        )
    };
    // Ignored: a resize is expected to arrive as new surfaces too. Unverified,
    // since the spike saw neither callback fire during toggles or animation.
    let properties = RcBlock::with_encoding::<_, _, _, PropertiesEncoding>(|_: *mut AnyObject| {});

    type Register = unsafe extern "C-unwind" fn(
        *const AnyObject,
        Sel,
        *const NSUUID,
        *const DispatchQueue,
        *const c_void,
        *const c_void,
        *const c_void,
    );
    // SAFETY: `v56@0:8@16@24@?32@?40@?48`; `find_main_screen` checked the
    // screen answers it. The screen copies the blocks.
    let send = unsafe { msg_send_fn::<Register>() };
    catching(|| unsafe {
        send(
            screen,
            sel!(registerScreenCallbacksWithUUID:callbackQueue:frameCallback:surfacesChangedCallback:propertiesChangedCallback:),
            registration,
            queue,
            ptr::from_ref(&*frame).cast(),
            ptr::from_ref(&*surfaces).cast(),
            ptr::from_ref(&*properties).cast(),
        )
    })
}

/// Runs `f`, turning an Objective-C exception into `Failed`.
fn catching<R>(f: impl FnOnce() -> R) -> Result<R, StreamError> {
    exception::catch(AssertUnwindSafe(f)).map_err(|e| {
        StreamError::Failed(match e {
            Some(e) => format!("Objective-C exception: {e}"),
            None => "Objective-C exception".into(),
        })
    })
}

fn ns_error(e: Retained<NSError>) -> StreamError {
    StreamError::Failed(e.localizedDescription().to_string())
}

/// `objc_msgSend` as the function type `F`.
///
/// # Safety
///
/// `F` must be an `unsafe extern "C-unwind" fn` whose arguments (receiver,
/// selector, then the method's) and return type match the method it is
/// called for.
unsafe fn msg_send_fn<F: Copy>() -> F {
    type Raw = unsafe extern "C-unwind" fn();
    let raw: Raw = objc2::ffi::objc_msgSend;
    assert_eq!(size_of::<F>(), size_of::<Raw>());
    // SAFETY: both are function pointers; the caller vouches for the type.
    unsafe { std::mem::transmute_copy::<Raw, F>(&raw) }
}

/// Whether `obj` answers `sel` (proxies included: they implement
/// `respondsToSelector:` themselves).
fn responds(obj: &AnyObject, sel: Sel) -> Result<bool, StreamError> {
    // SAFETY: `-respondsToSelector:` takes a selector and returns BOOL.
    catching(|| unsafe { msg_send![obj, respondsToSelector: sel] })
}

/// `Unsupported(name)` unless `obj` answers `sel`.
fn require(obj: &AnyObject, sel: Sel, name: &'static str) -> Result<(), StreamError> {
    if responds(obj, sel)? {
        Ok(())
    } else {
        Err(StreamError::Unsupported(name))
    }
}

/// Sends a no-argument message returning an object, straight through
/// `objc_msgSend` (see the module docs on proxies).
fn send_object(obj: &AnyObject, sel: Sel) -> Result<Option<Retained<AnyObject>>, StreamError> {
    type Getter = unsafe extern "C-unwind" fn(*const AnyObject, Sel) -> *mut AnyObject;
    // SAFETY: the callers' selectors are `@16@0:8` getters, checked with
    // `respondsToSelector:` first.
    let send = unsafe { msg_send_fn::<Getter>() };
    let raw = catching(|| unsafe { send(obj, sel) })?;
    // SAFETY: a +0 object (or nil); retained to own it.
    Ok(unsafe { Retained::retain(raw) })
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc::{self, Receiver};
    use std::time::Instant;

    use super::*;

    #[test]
    fn simulator_kit_is_found_beside_the_selected_xcode() {
        let paths = framework_paths(Path::new("/Applications/Xcode-beta.app/Contents/Developer"));
        assert_eq!(
            paths.simulator_kit,
            Path::new(
                "/Applications/Xcode-beta.app/Contents/SharedFrameworks/SimulatorKit.framework/SimulatorKit"
            )
        );
        assert_eq!(
            paths.core_simulator,
            Path::new("/Library/Developer/PrivateFrameworks/CoreSimulator.framework/CoreSimulator")
        );
    }

    #[test]
    fn a_command_line_tools_developer_dir_has_no_simulator_kit() {
        let paths = framework_paths(Path::new("/Library/Developer/CommandLineTools"));
        assert!(!paths.simulator_kit.exists());
        assert_eq!(
            check_paths(&paths, |p| p.exists()),
            Err(StreamError::NoXcode)
        );
        // CoreSimulator alone is not enough.
        assert_eq!(
            check_paths(&paths, |p| p == paths.core_simulator),
            Err(StreamError::NoXcode)
        );
        assert_eq!(check_paths(&paths, |_| true), Ok(()));
    }

    #[test]
    fn a_panicking_handler_is_contained_and_silences_the_stream() {
        let calls = Arc::new(AtomicUsize::new(0));
        let shared = Shared {
            stopped: AtomicBool::new(false),
            on_event: Box::new({
                let calls = Arc::clone(&calls);
                move |_| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    // Unwinds like a panic, without the panic hook's output.
                    std::panic::resume_unwind(Box::new("handler bug"));
                }
            }),
        };
        shared.emit(ScreenEvent::Frame);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(shared.stopped.load(Ordering::SeqCst));
        shared.emit(ScreenEvent::Frame);
        shared.surfaces_changed(std::ptr::null_mut());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "events after the panic");
    }

    /// What a callback saw, sent back to the test thread.
    #[derive(Debug)]
    enum Seen {
        Surface { width: usize, height: usize },
        Frame,
        Gone,
    }

    /// Events within `window`, plus whether any arrived on the main thread.
    fn collect(rx: &Receiver<(Seen, bool)>, window: Duration) -> (Vec<Seen>, bool) {
        let deadline = Instant::now() + window;
        let mut seen = Vec::new();
        let mut on_main = false;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok((event, main)) => {
                    on_main |= main;
                    seen.push(event);
                }
                Err(_) => break,
            }
        }
        (seen, on_main)
    }

    fn set_appearance(udid: &str, mode: &str) {
        let status = std::process::Command::new("xcrun")
            .args(["simctl", "ui", udid, "appearance", mode])
            .status()
            .expect("xcrun");
        assert!(status.success(), "simctl ui appearance {mode}");
    }

    /// Manual, on a simulator created and booted for the purpose (light
    /// appearance, as new ones are):
    /// `OO_TEST_SIM_UDID=<udid> cargo test live_stream -- --ignored`
    #[test]
    #[ignore]
    fn live_stream_delivers_frames() {
        let udid = std::env::var("OO_TEST_SIM_UDID").expect("set OO_TEST_SIM_UDID");
        let (tx, rx) = mpsc::channel();
        let stream = ScreenStream::start(&udid, move |event| {
            let seen = match event {
                ScreenEvent::Surface(s) => Seen::Surface {
                    width: s.width(),
                    height: s.height(),
                },
                ScreenEvent::Frame => Seen::Frame,
                ScreenEvent::Gone => Seen::Gone,
            };
            // SAFETY: no preconditions.
            let main = unsafe { libc::pthread_main_np() } != 0;
            let _ = tx.send((seen, main));
        })
        .expect("start the stream");

        let (first, on_main) = collect(&rx, Duration::from_secs(1));
        assert!(
            matches!(first.first(), Some(Seen::Surface { width, height }) if *width > 0 && *height > 0),
            "first event should be the surface: {first:?}"
        );
        let mut surfaces = first
            .iter()
            .filter(|e| matches!(e, Seen::Surface { .. }))
            .count();
        let mut off_main = !on_main;
        for mode in ["dark", "light"] {
            set_appearance(&udid, mode);
            let (seen, on_main) = collect(&rx, Duration::from_secs(1));
            off_main &= !on_main;
            let frames = seen.iter().filter(|e| matches!(e, Seen::Frame)).count();
            surfaces += seen
                .iter()
                .filter(|e| matches!(e, Seen::Surface { .. }))
                .count();
            assert!(frames >= 1, "no frame after appearance {mode}: {seen:?}");
            assert!(!seen.iter().any(|e| matches!(e, Seen::Gone)), "{seen:?}");
            eprintln!("appearance {mode}: {frames} frames");
        }
        assert_eq!(surfaces, 1, "one surface for the whole session");
        assert!(off_main, "a callback ran on the main thread");

        drop(stream);
        set_appearance(&udid, "dark");
        let (late, _) = collect(&rx, Duration::from_secs(1));
        set_appearance(&udid, "light");
        assert!(late.is_empty(), "events after drop: {late:?}");
    }
}
