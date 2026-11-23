//! The thread that owns the Device tab's [`ScreenStream`] (macOS only).
//!
//! `ScreenStream::start` blocks (`xcode-select`, then CoreSimulator's XPC)
//! and the stream isn't `Send`, so neither belongs on the UI thread. One
//! named thread per window starts and drops streams on request and reports
//! back with the generation it was asked for; frames go from the stream's
//! callback queue straight into a [`FrameSlot`], with one wake per burst.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use futures::channel::mpsc::UnboundedSender;
use objc2_core_foundation::CFRetained;
use objc2_io_surface::IOSurfaceRef;

use super::convert::{PixelConverter, SendPixelBuffer};
use super::sim_display::{ScreenEvent, ScreenStream};
use super::{FrameSlot, StreamError};

/// Where a stream's frames land; the UI takes from it on a [`StreamMsg::Frame`].
pub(crate) type Frames = Arc<FrameSlot<SendPixelBuffer>>;

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
        logged_error: false,
    });
    ScreenStream::start(udid, move |event| {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        match event {
            ScreenEvent::Surface(surface) => state.picture = Some(Picture(surface)),
            ScreenEvent::Frame => {}
            ScreenEvent::Gone => {
                state.picture = None;
                let _ = to_ui.unbounded_send(StreamMsg::Gone(generation));
                return;
            }
        }
        let Converting {
            converter,
            picture,
            logged_error,
        } = &mut *state;
        let Some(Picture(surface)) = picture.as_ref() else {
            return;
        };
        match converter.convert(surface) {
            Ok(buffer) => {
                if frames.put(SendPixelBuffer(buffer)) {
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
