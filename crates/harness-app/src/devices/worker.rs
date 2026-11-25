//! The thread that owns the Device tab's stream (macOS only): a
//! simulator's [`ScreenStream`] or an emulator's [`AndroidStream`].
//!
//! `ScreenStream::start` blocks (`xcode-select`, then CoreSimulator's XPC)
//! and the stream isn't `Send`, so neither belongs on the UI thread; nor
//! does connecting to an emulator, which this thread does on the stream's
//! own tokio runtime before a thread of the stream's pumps its frames. One
//! named thread per window starts and drops streams on request (one at a
//! time) and reports back with the generation it was asked for; frames go
//! from the stream's callback straight into a [`FrameSlot`], with one wake
//! per burst, already upright (an emulator sends them so).

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use futures::channel::mpsc::UnboundedSender;
use objc2_core_foundation::CFRetained;
use objc2_io_surface::IOSurfaceRef;

use super::android::stream::{AndroidStream, StreamEvent, native_long_side, request_box};
use super::convert::{PixelConverter, SendPixelBuffer};
use super::sim_display::{ScreenEvent, ScreenStream};
use super::{AndroidRunning, FrameSlot, Quarter, StreamError, quarter_for};

/// Where a stream's frames land, each with the turn that made it upright
/// (the frame around it turns too); the UI takes from it on a
/// [`StreamMsg::Frame`].
pub(crate) type Frames = Arc<FrameSlot<(SendPixelBuffer, Quarter)>>;

/// What the worker and the stream tell the UI. Each carries the generation
/// of the start it belongs to; the UI drops anything not current.
pub(crate) enum StreamMsg {
    Started(u64),
    Failed(u64, StreamError),
    /// A new frame is in that start's [`Frames`].
    Frame(u64),
    Gone(u64),
}

/// What to stream.
enum Target {
    Simulator(String),
    /// An emulator, in a square box of `side` device pixels (0: its
    /// screen's own size).
    Emulator { dev: AndroidRunning, side: u32 },
}

enum Request {
    Start {
        generation: u64,
        target: Target,
        frames: Frames,
    },
    /// Ask the emulator streaming under `generation` for frames of a new
    /// size: the same generation and slot, so the picture stays until the
    /// new size's first frame replaces it.
    Resize { generation: u64, side: u32 },
    Stop,
}

/// What the thread runs.
enum Current {
    Simulator { _stream: ScreenStream },
    Emulator {
        generation: u64,
        dev: AndroidRunning,
        /// The box asked for.
        size: (u32, u32),
        frames: Frames,
        _stream: AndroidStream,
    },
}

/// The newer of two queued requests. A start or stop replaces whatever
/// came before; a resize goes into the queued start of its own emulator
/// stream (or replaces an older resize), and is dropped after anything
/// else, being for a stream that is going.
fn coalesce(earlier: Request, later: Request) -> Request {
    let Request::Resize { generation, side } = later else {
        return later;
    };
    match earlier {
        Request::Start { generation: g, target: Target::Emulator { dev, .. }, frames }
            if g == generation =>
        {
            Request::Start { generation, target: Target::Emulator { dev, side }, frames }
        }
        Request::Resize { .. } => later,
        other => other,
    }
}

/// The handle the UI keeps. Dropping it ends the thread, which drops the
/// stream it holds there.
pub(crate) struct StreamWorker {
    requests: Sender<Request>,
}

impl StreamWorker {
    pub(crate) fn spawn(to_ui: UnboundedSender<StreamMsg>) -> std::io::Result<Self> {
        let (requests, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("oo-device-stream".into())
            .spawn(move || run(rx, to_ui))?;
        Ok(Self { requests })
    }

    pub(crate) fn start(&self, generation: u64, udid: String, frames: Frames) {
        let _ = self.requests.send(Request::Start {
            generation,
            target: Target::Simulator(udid),
            frames,
        });
    }

    /// Streams the emulator `dev`, asking for a square box of `side`
    /// device pixels (0: its screen's own size). It reports `Started` with
    /// its first frame.
    pub(crate) fn start_android(
        &self,
        generation: u64,
        dev: AndroidRunning,
        side: u32,
        frames: Frames,
    ) {
        let _ = self.requests.send(Request::Start {
            generation,
            target: Target::Emulator { dev, side },
            frames,
        });
    }

    /// Asks the emulator streaming under `generation` for a box of `side`.
    pub(crate) fn resize(&self, generation: u64, side: u32) {
        let _ = self.requests.send(Request::Resize { generation, side });
    }

    pub(crate) fn stop(&self) {
        let _ = self.requests.send(Request::Stop);
    }
}

fn run(rx: Receiver<Request>, to_ui: UnboundedSender<StreamMsg>) {
    let mut current: Option<Current> = None;
    while let Ok(mut request) = rx.recv() {
        // Only the newest request matters: each start replaces the stream
        // and a stop ends it, so starts queued behind a later one are skipped.
        while let Ok(newer) = rx.try_recv() {
            request = coalesce(request, newer);
        }
        let (generation, target, frames) = match request {
            Request::Stop => {
                current = None;
                continue;
            }
            Request::Start { generation, target, frames } => {
                // Drop the old stream before starting the next: one
                // registration (or call) at a time.
                current = None;
                (generation, target, frames)
            }
            Request::Resize { generation, side } => {
                let Some(Current::Emulator { generation: g, dev, size, frames, .. }) = &current
                else {
                    continue;
                };
                if *g != generation || emulator_box(dev, side) == *size {
                    continue;
                }
                let (dev, frames) = (dev.clone(), frames.clone());
                current = None;
                (generation, Target::Emulator { dev, side }, frames)
            }
        };
        let started = match target {
            Target::Simulator(udid) => start(generation, &udid, frames, to_ui.clone()).map(|s| {
                let _ = to_ui.unbounded_send(StreamMsg::Started(generation));
                Current::Simulator { _stream: s }
            }),
            Target::Emulator { dev, side } => {
                let size = emulator_box(&dev, side);
                start_android(generation, &dev, size, frames.clone(), to_ui.clone()).map(|s| {
                    Current::Emulator { generation, dev, size, frames, _stream: s }
                })
            }
        };
        match started {
            Ok(stream) => current = Some(stream),
            Err(e) => {
                let _ = to_ui.unbounded_send(StreamMsg::Failed(generation, e));
            }
        }
    }
    drop(current);
}

/// The box to ask `dev` for: a square of `side`, no bigger than its
/// screen (from the AVD's `config.ini`, when it can be read).
fn emulator_box(dev: &AndroidRunning, side: u32) -> (u32, u32) {
    let native = std::fs::read_to_string(dev.avd_dir.join("config.ini"))
        .ok()
        .and_then(|config| native_long_side(&config));
    request_box(side, native)
}

/// Streams the emulator `dev` into `frames`, converting each frame to 420f
/// on the stream's thread. `Started` goes with the first frame: a call
/// that opens and ends without one counts as a failure, so it can't reset
/// the tab's backoff. An end is `Gone` when the emulator exited, else a
/// failure with its reason.
fn start_android(
    generation: u64,
    dev: &AndroidRunning,
    size: (u32, u32),
    frames: Frames,
    to_ui: UnboundedSender<StreamMsg>,
) -> Result<AndroidStream, StreamError> {
    let mut converter = PixelConverter::new().map_err(StreamError::Failed)?;
    let (mut first, mut logged_error) = (true, false);
    AndroidStream::start(dev, size, move |event| match event {
        StreamEvent::Frame { width, height, rgba, turn } => {
            match converter.convert_rgba(rgba, width as usize, height as usize) {
                Ok(buffer) => {
                    // Drawn as it comes; the turn maps touches back.
                    if frames.put((SendPixelBuffer(buffer), turn)) {
                        let _ = to_ui.unbounded_send(StreamMsg::Frame(generation));
                    }
                    if std::mem::take(&mut first) {
                        let _ = to_ui.unbounded_send(StreamMsg::Started(generation));
                    }
                }
                Err(e) if !logged_error => {
                    logged_error = true;
                    log::warn!("device: converting an emulator frame failed: {e}");
                }
                Err(_) => {}
            }
        }
        StreamEvent::Ended { gone: true, why } => {
            log::debug!("device: the emulator shut down ({why})");
            let _ = to_ui.unbounded_send(StreamMsg::Gone(generation));
        }
        StreamEvent::Ended { gone: false, why } => {
            let _ = to_ui.unbounded_send(StreamMsg::Failed(generation, StreamError::Failed(why)));
        }
    })
}

/// The surface the simulator draws into, kept between its `Surface` event
/// and the `Frame` events that say it changed.
struct Picture(CFRetained<IOSurfaceRef>);

// SAFETY: CoreFoundation retain and release are thread-safe, and the surface
// is only read (by the transfer), on the stream's serial queue.
unsafe impl Send for Picture {}

struct Converting {
    converter: PixelConverter,
    picture: Option<Picture>,
    /// The device's `uiOrientation`, as last reported: frames are turned
    /// upright by it.
    orientation: u32,
    logged_error: bool,
}

fn start(
    generation: u64,
    udid: &str,
    frames: Frames,
    to_ui: UnboundedSender<StreamMsg>,
) -> Result<ScreenStream, StreamError> {
    let converter = PixelConverter::new().map_err(StreamError::Failed)?;
    let state = Mutex::new(Converting {
        converter,
        picture: None,
        orientation: 1,
        logged_error: false,
    });
    ScreenStream::start(udid, move |event| {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        match event {
            ScreenEvent::Surface(surface) => state.picture = Some(Picture(surface)),
            ScreenEvent::Frame => {}
            // Redrawn turned at once, in case no frame follows the turn.
            ScreenEvent::Orientation(o) => state.orientation = o,
            ScreenEvent::Gone => {
                state.picture = None;
                let _ = to_ui.unbounded_send(StreamMsg::Gone(generation));
                return;
            }
        }
        let Converting {
            converter,
            picture,
            orientation,
            logged_error,
        } = &mut *state;
        let Some(Picture(surface)) = picture.as_ref() else {
            return;
        };
        // The surface stays portrait when the device turns (see "Spike
        // findings" in the Device controls design).
        let turn = quarter_for(*orientation);
        match converter.convert(surface, turn) {
            Ok(buffer) => {
                if frames.put((SendPixelBuffer(buffer), turn)) {
                    let _ = to_ui.unbounded_send(StreamMsg::Frame(generation));
                }
            }
            Err(e) if !*logged_error => {
                *logged_error = true;
                log::warn!("device: converting a simulator frame failed: {e}");
            }
            Err(_) => {}
        }
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use objc2_core_video::{
        CVPixelBuffer, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
        CVPixelBufferGetHeight, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
        CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    };

    use super::*;
    use crate::devices::controls::{Control, ControlOutcome, rotate, run_public};

    fn emulator(side: u32) -> Target {
        let dev = AndroidRunning {
            pid: 7,
            avd_id: "P".into(),
            name: "P".into(),
            grpc_port: 8554,
            token: None,
            serial_port: 5554,
            avd_dir: "/nowhere".into(),
        };
        Target::Emulator { dev, side }
    }

    fn shape(r: &Request) -> (char, u64, u32) {
        match r {
            Request::Start { generation, target: Target::Emulator { side, .. }, .. } => {
                ('E', *generation, *side)
            }
            Request::Start { generation, target: Target::Simulator(_), .. } => ('S', *generation, 0),
            Request::Resize { generation, side } => ('R', *generation, *side),
            Request::Stop => ('-', 0, 0),
        }
    }

    /// A resize queued behind its stream's start goes into the start
    /// (never losing the start), a later start or stop wins, and a resize
    /// for another stream is dropped.
    #[test]
    fn a_resize_goes_into_the_start_queued_before_it() {
        let frames = || -> Frames { Arc::new(FrameSlot::new()) };
        let start = |g, target| Request::Start { generation: g, target, frames: frames() };
        let resize = |g, side| Request::Resize { generation: g, side };
        let cases = [
            (start(2, emulator(800)), resize(2, 900), ('E', 2, 900)),
            (start(2, emulator(800)), resize(1, 900), ('E', 2, 800)),
            (start(2, Target::Simulator("u".into())), resize(2, 900), ('S', 2, 0)),
            (resize(2, 800), resize(2, 900), ('R', 2, 900)),
            (Request::Stop, resize(2, 900), ('-', 0, 0)),
            (resize(2, 800), Request::Stop, ('-', 0, 0)),
            (resize(2, 800), start(3, emulator(700)), ('E', 3, 700)),
        ];
        for (earlier, later, want) in cases {
            assert_eq!(shape(&coalesce(earlier, later)), want);
        }
    }

    /// Manual, on an AVD booted for the purpose (never anyone else's): start
    /// `OO_TEST_AVD=<avd> cargo test android_worker_streams -- --ignored
    /// --nocapture`, then boot the AVD. Streams it through the worker from
    /// the moment it is listed (as Boot does), then, once booted: a resize
    /// in the same generation, a turn (frames change shape in one stream)
    /// and a stop.
    #[test]
    #[ignore]
    fn android_worker_streams_resizes_and_stops() {
        use objc2_core_video::{CVPixelBufferGetHeight, CVPixelBufferGetWidth};
        use std::time::Instant;
        let avd = std::env::var("OO_TEST_AVD").expect("set OO_TEST_AVD");
        let adb = format!("{}/Library/Android/sdk/platform-tools/adb", std::env::var("HOME").unwrap());
        let t0 = Instant::now();
        let dev = loop {
            if let Some(d) = crate::devices::android::discovery::running_emulators()
                .into_iter()
                .find(|r| r.avd_id == avd)
            {
                break d;
            }
            assert!(t0.elapsed() < Duration::from_secs(120), "never listed");
            std::thread::sleep(Duration::from_millis(100));
        };
        let listed = Instant::now();
        eprintln!("listed after {:?}", t0.elapsed());
        let (to_ui, mut from_worker) = futures::channel::mpsc::unbounded();
        let worker = StreamWorker::spawn(to_ui).unwrap();
        let frames: Frames = Arc::new(FrameSlot::new());
        let size_of = |frames: &Frames| {
            frames.take().map(|(b, _)| (CVPixelBufferGetWidth(&b.0), CVPixelBufferGetHeight(&b.0)))
        };
        // As the tab does: a failure is tried again a few seconds later.
        let mut generation = 1;
        worker.start_android(generation, dev.clone(), 1600, frames.clone());
        let mut got_frame = None;
        while listed.elapsed() < Duration::from_secs(120) {
            match from_worker.try_recv() {
                Ok(StreamMsg::Frame(g)) if g == generation => {
                    let size = size_of(&frames);
                    eprintln!("{:?} frame {size:?}", listed.elapsed());
                    got_frame = got_frame.or(size);
                }
                Ok(StreamMsg::Started(g)) if g == generation => {
                    eprintln!("{:?} started", listed.elapsed());
                    assert!(got_frame.is_some(), "Started before a frame");
                    break;
                }
                Ok(StreamMsg::Failed(g, e)) if g == generation => {
                    eprintln!("{:?} failed: {e:?}", listed.elapsed());
                    std::thread::sleep(Duration::from_secs(3));
                    generation += 1;
                    worker.start_android(generation, dev.clone(), 1600, frames.clone());
                }
                Ok(StreamMsg::Gone(g)) => panic!("gone ({g})"),
                Ok(_) => {}
                Err(futures::channel::mpsc::TryRecvError::Closed) => panic!("worker ended"),
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        let first = got_frame.expect("a frame");
        assert!(first.0.max(first.1) <= 1600, "{first:?}");
        // Wait for the boot to finish before turning it.
        let booted = || {
            std::process::Command::new(&adb)
                .args(["-s", &format!("emulator-{}", dev.serial_port), "shell", "getprop", "sys.boot_completed"])
                .output()
                .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "1")
        };
        while !booted() {
            assert!(listed.elapsed() < Duration::from_secs(180), "never booted");
            std::thread::sleep(Duration::from_millis(500));
        }
        eprintln!("booted after {:?} listed", listed.elapsed());
        std::thread::sleep(Duration::from_secs(3));
        // Drain, then wait for the frame that matches.
        let wait_for = |rx: &mut futures::channel::mpsc::UnboundedReceiver<StreamMsg>,
                        want: &dyn Fn((usize, usize)) -> bool,
                        what: &str| {
            let at = Instant::now();
            while at.elapsed() < Duration::from_secs(10) {
                match rx.try_recv() {
                    Ok(StreamMsg::Frame(g)) => {
                        assert_eq!(g, generation, "a frame of another generation");
                        if let Some(size) = size_of(&frames) {
                            eprintln!("{what}: frame {size:?} after {:?}", at.elapsed());
                            if want(size) {
                                return size;
                            }
                        }
                    }
                    Ok(StreamMsg::Failed(_, e)) => panic!("{what}: failed {e:?}"),
                    Ok(StreamMsg::Gone(_)) => panic!("{what}: gone"),
                    Ok(StreamMsg::Started(g)) => assert_eq!(g, generation),
                    Err(futures::channel::mpsc::TryRecvError::Closed) => panic!("worker ended"),
                    Err(_) => std::thread::sleep(Duration::from_millis(20)),
                }
            }
            panic!("{what}: no such frame in 10 s");
        };
        while from_worker.try_recv().is_ok() {}
        let _ = frames.take();
        worker.resize(generation, 800);
        let small = wait_for(&mut from_worker, &|(w, h)| w.max(h) <= 800, "resize to 800");
        assert_eq!(small.0.max(small.1), 800, "{small:?}");
        let serial = format!("emulator-{}", dev.serial_port);
        let turned = std::process::Command::new(&adb).args(["-s", &serial, "emu", "rotate"]).status();
        assert!(turned.unwrap().success());
        wait_for(&mut from_worker, &|(w, h)| w > h, "rotate");
        worker.stop();
        std::thread::sleep(Duration::from_millis(500));
        while from_worker.try_recv().is_ok() {}
        let _ = frames.take();
        let at = Instant::now();
        let _ = std::process::Command::new(&adb).args(["-s", &serial, "emu", "rotate"]).status();
        while at.elapsed() < Duration::from_secs(2) {
            if let Ok(StreamMsg::Frame(_)) = from_worker.try_recv() {
                panic!("a frame after the stop");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        eprintln!("stopped: silent for 2 s across a turn");
    }

    /// A coarse grid of luma samples, row by row.
    const GRID: usize = 32;

    fn frame_grid(buffer: &CVPixelBuffer) -> (usize, usize, Vec<f64>) {
        let (w, h) = (CVPixelBufferGetWidth(buffer), CVPixelBufferGetHeight(buffer));
        let flags = CVPixelBufferLockFlags::ReadOnly;
        // SAFETY: a live buffer, locked for the reads and unlocked after.
        assert_eq!(unsafe { CVPixelBufferLockBaseAddress(buffer, flags) }, 0);
        let luma = CVPixelBufferGetBaseAddressOfPlane(buffer, 0).cast::<u8>();
        let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0);
        let mut grid = Vec::new();
        for gy in 0..GRID {
            for gx in 0..GRID {
                let (x, y) = ((2 * gx + 1) * w / (2 * GRID), (2 * gy + 1) * h / (2 * GRID));
                // SAFETY: (x, y) is inside plane 0, a byte per pixel.
                grid.push(f64::from(unsafe { *luma.add(y * stride + x) }));
            }
        }
        // SAFETY: as above.
        assert_eq!(unsafe { CVPixelBufferUnlockBaseAddress(buffer, flags) }, 0);
        (w, h, grid)
    }

    fn png_grid(png: &[u8]) -> (usize, usize, Vec<f64>) {
        let image = image::load_from_memory(png).unwrap().to_luma8();
        let (w, h) = (image.width() as usize, image.height() as usize);
        let mut grid = Vec::new();
        for gy in 0..GRID {
            for gx in 0..GRID {
                let (x, y) = ((2 * gx + 1) * w / (2 * GRID), (2 * gy + 1) * h / (2 * GRID));
                grid.push(f64::from(image.get_pixel(x as u32, y as u32)[0]));
            }
        }
        (w, h, grid)
    }

    fn mean_diff(a: &[f64], b: impl Iterator<Item = f64>) -> f64 {
        a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f64>() / a.len() as f64
    }

    /// Manual, on a simulator created and booted for the purpose (never
    /// anyone else's): `OO_TEST_SIM_UDID=<udid> cargo test
    /// turned_frames_match -- --ignored --nocapture`. Turns it in Safari and
    /// checks each streamed frame is upright: the size of `simctl io
    /// screenshot` (already upright) and its picture, far closer to it than
    /// to the same picture upside down (what the opposite quarter turn
    /// would draw).
    #[test]
    #[ignore]
    fn turned_frames_match_the_screenshot() {
        let udid = std::env::var("OO_TEST_SIM_UDID").expect("set OO_TEST_SIM_UDID");
        let set = ["devicectl", "device", "orientation", "set", "--device", &udid, "portrait"];
        let ran = crate::ports::run_bounded("xcrun", &set, Duration::from_secs(5)).unwrap();
        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        crate::devices::simctl::simctl(&["launch", &udid, "com.apple.mobilesafari"]).unwrap();
        std::thread::sleep(Duration::from_secs(3));

        let frames: Frames = Arc::new(FrameSlot::new());
        let (to_ui, _from_worker) = futures::channel::mpsc::unbounded();
        let stream = start(1, &udid, frames.clone(), to_ui).unwrap();
        let (mut sizes, mut seen) = (Vec::new(), Vec::new());
        for turn in 0..4 {
            if turn > 0 {
                rotate(&udid).unwrap();
            }
            std::thread::sleep(Duration::from_millis(2000));
            let frame = frames.take().expect("a frame since the turn");
            let (fw, fh, frame) = frame_grid(&frame.0.0);
            let png = match run_public(&udid, Control::Screenshot, None) {
                Ok(ControlOutcome::Png(png)) => png,
                other => panic!("no screenshot: {other:?}"),
            };
            let (sw, sh, shot) = png_grid(&png);
            let get = ["devicectl", "device", "orientation", "get", "--device", &udid, "-j", "-"];
            let got = crate::ports::run_bounded("xcrun", &get, Duration::from_secs(5)).unwrap();
            let json: serde_json::Value = serde_json::from_str(&got.stdout).unwrap_or_default();
            let device = json["result"]["deviceOrientation"].as_str().unwrap_or("?").to_string();
            let upright = mean_diff(&frame, shot.iter().copied());
            let upside_down = mean_diff(&frame, shot.iter().rev().copied());
            eprintln!(
                "turn {turn} ({device}): frame {fw}×{fh}, screenshot {sw}×{sh}, \
                 luma diff {upright:.1} (upside down {upside_down:.1})"
            );
            assert_eq!((fw, fh), (sw, sh), "turn {turn}");
            assert!(upright < 12.0 && upright * 3.0 < upside_down, "turn {turn}");
            sizes.push((fw, fh));
            seen.push(device);
        }
        drop(stream);
        // Safari turns to both landscapes (and refuses upside down).
        assert!(sizes.iter().any(|&(w, h)| w > h), "{sizes:?}");
        assert!(seen.iter().any(|o| o == "landscapeRight"), "{seen:?}");
        assert!(seen.iter().any(|o| o == "landscapeLeft"), "{seen:?}");
    }
}
