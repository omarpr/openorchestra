//! The thread that owns the Device tab's [`ScreenStream`] (macOS only).
//!
//! `ScreenStream::start` blocks (`xcode-select`, then CoreSimulator's XPC)
//! and the stream isn't `Send`, so neither belongs on the UI thread. One
//! named thread per window starts and drops streams on request and reports
//! back with the generation it was asked for; frames go from the stream's
//! callback queue straight into a [`FrameSlot`], with one wake per burst,
//! already turned upright for the device's orientation.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use futures::channel::mpsc::UnboundedSender;
use objc2_core_foundation::CFRetained;
use objc2_io_surface::IOSurfaceRef;

use super::convert::{PixelConverter, SendPixelBuffer};
use super::sim_display::{ScreenEvent, ScreenStream};
use super::{FrameSlot, Quarter, StreamError, quarter_for};

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

enum Request {
    Start {
        generation: u64,
        udid: String,
        frames: Frames,
    },
    Stop,
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
            udid,
            frames,
        });
    }

    pub(crate) fn stop(&self) {
        let _ = self.requests.send(Request::Stop);
    }
}

fn run(rx: Receiver<Request>, to_ui: UnboundedSender<StreamMsg>) {
    let mut current: Option<ScreenStream> = None;
    while let Ok(mut request) = rx.recv() {
        // Only the newest request matters: each start replaces the stream
        // and a stop ends it, so starts queued behind a later one are skipped.
        while let Ok(newer) = rx.try_recv() {
            request = newer;
        }
        // Drop the old stream before starting the next: one registration at
        // a time.
        current = None;
        let Request::Start {
            generation,
            udid,
            frames,
        } = request
        else {
            continue;
        };
        match start(generation, &udid, frames, to_ui.clone()) {
            Ok(stream) => {
                current = Some(stream);
                let _ = to_ui.unbounded_send(StreamMsg::Started(generation));
            }
            Err(e) => {
                let _ = to_ui.unbounded_send(StreamMsg::Failed(generation, e));
            }
        }
    }
    drop(current);
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
