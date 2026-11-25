//! An Android emulator's live picture: its gRPC `streamScreenshot`.
//!
//! The emulator pushes RGBA8888 frames, rows top down (whatever the proto
//! says), already turned to the device's orientation, and only when the
//! screen changes: a still screen sends one frame and then nothing, so
//! silence is not a stall. Each frame's real size is in its `format`,
//! never the size asked for, which is only a bounding box the emulator
//! fits the device's aspect into (see "Spike findings" in the Android
//! Device tab design).
//!
//! Every call carries `authorization: Bearer <grpc.token>` from the
//! emulator's discovery file; without it every method is refused.
//!
//! [`AndroidStream::start`] connects and opens the call on the calling
//! thread (the device worker) on a current-thread tokio runtime, bounded by
//! [`CONNECT_TIMEOUT`], then hands that runtime to a thread of its own that
//! pumps frames until the call ends or the handle is dropped. It never
//! reconnects: an end is reported once, and the Device tab decides.

use std::time::Duration;

use tonic::Request;
use tonic::metadata::MetadataValue;

use super::grpc::emulator_controller_client::EmulatorControllerClient;
use super::grpc::image_format::ImgFormat;
use super::grpc::rotation::SkinRotation;
use super::grpc::{Image, ImageFormat, MAX_DECODING_MESSAGE_SIZE};
use crate::devices::{AndroidRunning, Quarter, StreamError};

/// The client every call to an emulator goes through.
pub(crate) type Client = EmulatorControllerClient<tonic::transport::Channel>;

/// How long connecting and opening the call may take.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// After the call ends, how long the emulator's process gets to exit
/// (`setVmState(SHUTDOWN)` takes about 2 s) before the end is put down to
/// something else.
const EXIT_GRACE: Duration = Duration::from_secs(3);
/// The same when the call ended `Unavailable` ("Cancelling all calls"):
/// an emulator shutting down cancels its calls, then saves its Quick Boot
/// snapshot (by default) or powers off (our Shut down) for 3 to 9 s more
/// before its process exits.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(20);
const EXIT_POLL: Duration = Duration::from_millis(250);
/// Wait this long for a usable frame after a visible startup frame fails.
const FRAME_RECOVERY: Duration = Duration::from_secs(3);

/// What to make of a call that ended, `waited` after its end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AfterEnd {
    /// The emulator's process exited: it shut down.
    Gone,
    /// Ask again after [`EXIT_POLL`].
    Wait,
    /// It still runs: the end was something else.
    Failed,
}

/// Whether an emulator whose call ended (`unavailable`: with
/// `Unavailable`) shut down, `waited` after the end, its process being
/// `alive` or not: gone once it exits; while it runs, waited for up to
/// [`SHUTDOWN_GRACE`] after a cancelled call, else [`EXIT_GRACE`].
fn after_end(alive: bool, unavailable: bool, waited: Duration) -> AfterEnd {
    let grace = if unavailable { SHUTDOWN_GRACE } else { EXIT_GRACE };
    if !alive {
        AfterEnd::Gone
    } else if waited >= grace {
        AfterEnd::Failed
    } else {
        AfterEnd::Wait
    }
}

/// `message` with the emulator's bearer token, when it wrote one. Without
/// one the call goes out bare and the emulator's refusal says why.
pub(crate) fn authorized<T>(message: T, token: Option<&str>) -> Result<Request<T>, String> {
    let mut request = Request::new(message);
    if let Some(token) = token {
        let value = MetadataValue::try_from(format!("Bearer {token}"))
            .map_err(|_| "The emulator's gRPC token isn't usable as a header".to_string())?;
        request.metadata_mut().insert("authorization", value);
    }
    Ok(request)
}

/// The longer side of the AVD's screen, from its `config.ini`
/// (`hw.lcd.width = 1080`, `hw.lcd.height = 2400`).
pub(crate) fn native_long_side(config_ini: &str) -> Option<u32> {
    config_ini
        .lines()
        .filter_map(|line| line.split_once('='))
        .filter(|(key, _)| matches!(key.trim(), "hw.lcd.width" | "hw.lcd.height"))
        .map(|(_, value)| value.trim().parse::<u32>().ok())
        .try_fold(None::<u32>, |longest, side| Some(longest.max(Some(side?))))
        .flatten()
        .filter(|&side| side > 0)
}

/// The AVD's screen in its natural orientation, `(hw.lcd.width,
/// hw.lcd.height)` from its `config.ini`: the device pixels touches are
/// given in, whichever way it is turned.
pub(crate) fn native_size(config_ini: &str) -> Option<(u32, u32)> {
    let value = |name: &str| {
        config_ini
            .lines()
            .filter_map(|line| line.split_once('='))
            .find(|(key, _)| key.trim() == name)
            .and_then(|(_, value)| value.trim().parse::<u32>().ok())
            .filter(|&v| v > 0)
    };
    Some((value("hw.lcd.width")?, value("hw.lcd.height")?))
}

/// The turn that made a frame upright, from the `SkinRotation` it came
/// with, as the simulator's turns are counted (how far clockwise the
/// natural screen was turned): what maps a point on the picture back to
/// the natural screen (`device_input::view_to_device`).
pub(crate) fn quarter_for_rotation(rotation: i32) -> Quarter {
    // Landscape (z = 90) has the picture's top at the natural screen's
    // right edge, as the simulator's uiOrientation 4.
    match SkinRotation::try_from(rotation) {
        Ok(SkinRotation::Landscape) => Quarter::Cw270,
        Ok(SkinRotation::ReversePortrait) => Quarter::Cw180,
        Ok(SkinRotation::ReverseLandscape) => Quarter::Cw90,
        Ok(SkinRotation::Portrait) | Err(_) => Quarter::None,
    }
}

/// How long after an emulator is listed a connection it doesn't answer
/// (refused, or timed out) is put down to its boot.
pub(crate) const BOOT_GRACE: Duration = Duration::from_secs(60);

/// A connection that failed, `why`, as the tab should take it:
/// [`StreamError::Starting`] when the emulator wasn't answering
/// (`not_answering`: refused or timed out, as opposed to a refusal such as
/// a bad token) and was listed less than [`BOOT_GRACE`] ago
/// (`listed_for`), else a failure.
pub(crate) fn connect_failure(
    not_answering: bool,
    listed_for: Option<Duration>,
    why: String,
) -> StreamError {
    match listed_for {
        Some(listed) if not_answering && listed < BOOT_GRACE => StreamError::Starting(why),
        _ => StreamError::Failed(why),
    }
}

/// Room for the rest of an `Image` message around its pixels.
const MESSAGE_HEADROOM: u64 = 64 * 1024;

/// The largest square box whose largest possible frame (a square screen,
/// side × side RGBA) still decodes: 4093 at 64 MiB.
pub(crate) const MAX_SIDE: u32 =
    ((MAX_DECODING_MESSAGE_SIZE as u64 - MESSAGE_HEADROOM) / 4).isqrt() as u32;

/// The box to ask for: a square whose side is the picture area's longer
/// side (`side`, device pixels), so the one stream keeps its resolution
/// when the device turns. Never past the screen's own longer side, when
/// known, nor past [`MAX_SIDE`], so no frame is too big to decode. 0 (not
/// measured yet) asks for the screen's own size, or [`MAX_SIDE`] when that
/// isn't known: never an unbounded 0 × 0.
pub(crate) fn request_box(side: u32, native_long: Option<u32>) -> (u32, u32) {
    let side = match (side, native_long) {
        (0, Some(native)) => native,
        (0, None) => MAX_SIDE,
        (side, Some(native)) => side.min(native),
        (side, None) => side,
    };
    let side = side.clamp(1, MAX_SIDE);
    (side, side)
}

/// What a stream reports, on its own thread.
pub(crate) enum StreamEvent {
    /// The display is inactive. The RPC stays open for a later picture.
    Inactive,
    /// A frame: `width` × `height` RGBA8888, rows top down, upright, and
    /// the turn that made it so ([`quarter_for_rotation`]).
    Frame { width: u32, height: u32, rgba: Vec<u8>, turn: Quarter },
    /// The call ended (reported once, never after a drop). `gone`: the
    /// emulator's process exited, so it shut down; otherwise `why` says
    /// what ended it.
    Ended { gone: bool, why: String },
}

/// A running `streamScreenshot` call. Dropping it ends the call.
pub(crate) struct AndroidStream {
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    pump: Option<std::thread::JoinHandle<()>>,
}

impl AndroidStream {
    /// Connects to `dev` and asks for frames fitting `size`, then calls
    /// `on_event` with each, on a thread of the stream's own. A callback
    /// error means a frame could not be displayed. Blocks for up
    /// to [`CONNECT_TIMEOUT`]. A refused call (a bad token, say) is an
    /// error here; [`StreamError::DeviceNotFound`] when the emulator has
    /// already gone.
    pub(crate) fn start(
        dev: &AndroidRunning,
        size: (u32, u32),
        on_event: impl FnMut(StreamEvent) -> Result<(), String> + Send + 'static,
    ) -> Result<AndroidStream, StreamError> {
        if !super::discovery::still_emulator(dev.pid) {
            return Err(StreamError::DeviceNotFound);
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| StreamError::Failed(format!("starting the stream's runtime: {e}")))?;
        let format = ImageFormat {
            format: ImgFormat::Rgba8888 as i32,
            width: size.0,
            height: size.1,
            ..Default::default()
        };
        let request = authorized(format, dev.token.as_deref()).map_err(StreamError::Failed)?;
        let port = dev.grpc_port;
        // The emulator's refusal quotes the token it got; the note and the
        // log mustn't. The same scrub applies when the call ends later.
        let token = dev.token.clone();
        let scrub = token.clone();
        // Usual for a while after an emulator is listed: its port takes the
        // connection (or refuses it) before it answers.
        let listed_for = super::discovery::listed_for(dev.pid);
        let failed = move |(not_answering, why): (bool, String)| {
            connect_failure(not_answering, listed_for, without_token(&why, token.as_deref()))
        };
        let frames = runtime
            .block_on(async { tokio::time::timeout(CONNECT_TIMEOUT, open(port, request)).await })
            .unwrap_or_else(|_| {
                let waited = CONNECT_TIMEOUT.as_secs();
                Err((true, format!("The emulator isn't answering on port {port} (waited {waited} s)")))
            })
            .map_err(failed)?;
        let (cancel, cancelled) = tokio::sync::oneshot::channel::<()>();
        let pid = dev.pid;
        let pump = std::thread::Builder::new()
            .name("oo-android-stream".into())
            .spawn(move || {
                runtime.block_on(async move {
                    let pump = std::pin::pin!(pump(frames, pid, scrub, on_event));
                    // Either ends it: a drop (the cancel's sender goes too),
                    // or the call.
                    futures::future::select(cancelled, pump).await;
                });
            })
            .map_err(|e| StreamError::Failed(format!("starting the stream thread: {e}")))?;
        Ok(AndroidStream { cancel: Some(cancel), pump: Some(pump) })
    }
}

impl Drop for AndroidStream {
    /// Ends the call and waits for its thread, so no frame of it lands
    /// after this.
    fn drop(&mut self) {
        drop(self.cancel.take());
        if let Some(pump) = self.pump.take() {
            let _ = pump.join();
        }
    }
}

/// Connects to the emulator's gRPC port and opens the stream. An error
/// says whether the emulator wasn't answering (the connection failed, or
/// the call was `Unavailable`) rather than refusing, and why.
async fn open(
    port: u16,
    request: Request<ImageFormat>,
) -> Result<tonic::Streaming<Image>, (bool, String)> {
    let mut client = connect(port).await?;
    let response = client.stream_screenshot(request).await.map_err(|s| {
        (s.code() == tonic::Code::Unavailable, status_text(&s))
    })?;
    Ok(response.into_inner())
}

/// Connects to the emulator's gRPC port on this machine. An error says
/// whether the emulator wasn't answering (rather than refusing), and why.
pub(crate) async fn connect(port: u16) -> Result<Client, (bool, String)> {
    let channel = tonic::transport::Endpoint::from_shared(format!("http://127.0.0.1:{port}"))
        .map_err(|e| (false, error_text(&e)))?
        .connect_timeout(CONNECT_TIMEOUT)
        .connect()
        .await
        .map_err(|e| (true, format!("Couldn't connect to the emulator: {}", error_text(&e))))?;
    Ok(EmulatorControllerClient::new(channel).max_decoding_message_size(MAX_DECODING_MESSAGE_SIZE))
}

fn image_event(image: Image) -> Result<StreamEvent, String> {
    // The emulator represents an inactive display with an empty zero-size
    // image, which may omit its format entirely.
    if image.image.is_empty() && image.format.as_ref().is_none_or(|f| f.width == 0 && f.height == 0) {
        return Ok(StreamEvent::Inactive);
    }
    let format = image.format.ok_or_else(|| "the image has no format".to_string())?;
    if format.format != ImgFormat::Rgba8888 as i32 {
        return Err("the image is not RGBA".into());
    }
    let turn = format.rotation.map_or(Quarter::None, |r| quarter_for_rotation(r.rotation));
    Ok(StreamEvent::Frame { width: format.width, height: format.height, rgba: image.image, turn })
}

/// Wait for a usable first visible frame after a failed conversion. Empty
/// inactive images and healthy idle displays can wait indefinitely.
async fn pump_frames(
    mut message: impl AsyncFnMut() -> Result<Option<Image>, tonic::Status>,
    on_event: &mut impl FnMut(StreamEvent) -> Result<(), String>,
    recovery: Duration,
) -> Result<Option<tonic::Status>, String> {
    let mut needs_picture = true;
    let mut recovering: Option<(tokio::time::Instant, String)> = None;
    loop {
        let next = match &recovering {
            Some((deadline, why)) => {
                let timed_out = || format!("Couldn't display the emulator's first frame: {why}");
                // A ready receive can win timeout_at even after its deadline.
                if tokio::time::Instant::now() >= *deadline {
                    return Err(timed_out());
                }
                tokio::time::timeout_at(*deadline, message()).await.map_err(|_| timed_out())?
            }
            None => message().await,
        };
        let image = match next {
            Ok(Some(image)) => image,
            Ok(None) => return Ok(None),
            Err(status) => return Ok(Some(status)),
        };
        let event = image_event(image);
        if matches!(event, Ok(StreamEvent::Inactive)) {
            needs_picture = true;
            recovering = None;
            let _ = on_event(StreamEvent::Inactive);
            continue;
        }
        match event.and_then(&mut *on_event) {
            Ok(()) => {
                needs_picture = false;
                recovering = None;
            }
            Err(why) if needs_picture && recovering.is_none() => {
                recovering = Some((tokio::time::Instant::now() + recovery, why));
            }
            Err(_) => {}
        }
    }
}

/// Hands each frame to `on_event` until the call ends, then reports the
/// end once.
async fn pump(
    mut frames: tonic::Streaming<Image>,
    pid: u32,
    token: Option<String>,
    mut on_event: impl FnMut(StreamEvent) -> Result<(), String>,
) {
    let result = pump_frames(async || frames.message().await, &mut on_event, FRAME_RECOVERY).await;
    let (why, unavailable) = match result {
        Ok(None) => ("The emulator ended the picture".to_string(), false),
        Ok(Some(status)) => {
            let text = without_token(&status_text(&status), token.as_deref());
            (text, status.code() == tonic::Code::Unavailable)
        }
        Err(why) => {
            let _ = on_event(StreamEvent::Ended { gone: false, why });
            return;
        }
    };
    drop(frames);
    // An emulator shutting down closes the call before its process exits.
    // A drop ends the wait (at its sleep), as it ends the call.
    let mut waited = Duration::ZERO;
    let gone = loop {
        match after_end(super::discovery::still_emulator(pid), unavailable, waited) {
            AfterEnd::Gone => break true,
            AfterEnd::Failed => break false,
            AfterEnd::Wait => {
                tokio::time::sleep(EXIT_POLL).await;
                waited += EXIT_POLL;
            }
        }
    };
    let _ = on_event(StreamEvent::Ended { gone, why });
}

/// `text` with `token` (when there is one) blanked out.
pub(crate) fn without_token(text: &str, token: Option<&str>) -> String {
    match token.filter(|t| !t.is_empty()) {
        Some(token) => text.replace(token, "…"),
        None => text.to_string(),
    }
}

/// A gRPC refusal as one line: "Unauthenticated: Missing the
/// 'authorization' header".
pub(crate) fn status_text(status: &tonic::Status) -> String {
    let message = status.message().lines().next().unwrap_or_default().trim();
    if message.is_empty() {
        format!("{:?}", status.code())
    } else {
        format!("{:?}: {message}", status.code())
    }
}

/// An error and its causes as one line ("transport error: tcp connect
/// error: Connection refused (os error 61)"), each said once.
fn error_text(error: &(dyn std::error::Error + 'static)) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut next = Some(error);
    while let Some(e) = next {
        let text = e.to_string();
        let line = text.lines().next().unwrap_or_default().trim().to_string();
        if !line.is_empty() && !parts.iter().any(|p| p.contains(&line)) {
            parts.push(line);
        }
        next = e.source();
    }
    parts.join(": ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stream_request_carries_the_token() {
        let request = authorized((), Some("c2VjcmV0dG9rZW4=")).unwrap();
        assert_eq!(
            request.metadata().get("authorization").unwrap().to_str().unwrap(),
            "Bearer c2VjcmV0dG9rZW4="
        );
        // No token in the discovery file: the call goes out without one.
        let bare = authorized((), None).unwrap();
        assert!(bare.metadata().get("authorization").is_none());
        // A token that can't be a header is an error, not a panic.
        assert!(authorized((), Some("bad\ntoken")).is_err());
    }

    /// An emulator saving its Quick Boot snapshot on exit cancels its
    /// calls ("Unavailable: Cancelling all calls") and runs on for 3 to
    /// 9 s: that end waits for it to exit, up to about 20 s, and reads as a
    /// shutdown. Any other end gets the short grace.
    #[test]
    fn an_emulator_saving_on_exit_is_waited_for() {
        let secs = Duration::from_secs;
        // Exited: gone, however the call ended.
        assert_eq!(after_end(false, true, secs(0)), AfterEnd::Gone);
        assert_eq!(after_end(false, false, secs(2)), AfterEnd::Gone);
        assert_eq!(after_end(false, true, secs(15)), AfterEnd::Gone);
        // Cancelled and still running: waited for, past the short grace.
        assert_eq!(after_end(true, true, secs(3)), AfterEnd::Wait);
        assert_eq!(after_end(true, true, secs(9)), AfterEnd::Wait);
        assert_eq!(after_end(true, true, secs(19)), AfterEnd::Wait);
        // But not for ever.
        assert_eq!(after_end(true, true, secs(20)), AfterEnd::Failed);
        assert_eq!(after_end(true, true, secs(60)), AfterEnd::Failed);
        // Any other end: the short grace.
        assert_eq!(after_end(true, false, secs(2)), AfterEnd::Wait);
        assert_eq!(after_end(true, false, secs(3)), AfterEnd::Failed);
    }

    #[test]
    fn a_connect_failure_soon_after_listing_is_starting() {
        let secs = |s| Some(Duration::from_secs(s));
        let starting = |e: StreamError| matches!(e, StreamError::Starting(_));
        assert!(starting(connect_failure(true, secs(0), "t".into())));
        assert!(starting(connect_failure(true, secs(59), "t".into())));
        // Past the boot, or answering with a refusal: a failure.
        assert_eq!(connect_failure(true, secs(60), "t".into()), StreamError::Failed("t".into()));
        assert_eq!(
            connect_failure(false, secs(1), "Unauthenticated: x".into()),
            StreamError::Failed("Unauthenticated: x".into())
        );
        // When it was listed is unknown: a failure, which backs off.
        assert_eq!(connect_failure(true, None, "t".into()), StreamError::Failed("t".into()));
    }

    /// No box asked for can bring a frame the client would refuse to
    /// decode: the largest frame a square box of side s allows is s × s
    /// RGBA, plus the message around it.
    #[test]
    fn the_box_never_asks_for_a_frame_over_the_decode_limit() {
        let fits = |(w, h): (u32, u32)| {
            u64::from(w) * u64::from(h) * 4 + 64 * 1024 <= MAX_DECODING_MESSAGE_SIZE as u64
        };
        for side in [0, 1600, 2560, 3840, 4096, 5000, 8192, u32::MAX] {
            for native in [None, Some(2400), Some(2800), Some(3840), Some(7680)] {
                let asked = request_box(side, native);
                assert!(asked.0 > 0 && fits(asked), "{side} {native:?} -> {asked:?}");
            }
        }
        // A 2800×1752 tablet and a 4K TV get their full size.
        assert_eq!(request_box(5000, Some(2800)), (2800, 2800));
        assert_eq!(request_box(0, Some(3840)), (3840, 3840));
        assert_eq!(MAX_SIDE, 4093);
    }

    #[test]
    fn the_box_is_a_square_of_the_longer_side_capped_at_the_screen() {
        let config = "avd.ini.encoding = UTF-8\nhw.lcd.density = 420\n\
                      hw.lcd.height = 2400\nhw.lcd.width = 1080\nhw.ramSize = 2048\n";
        assert_eq!(native_long_side(config), Some(2400));
        assert_eq!(native_long_side("hw.lcd.width=2856\r\nhw.lcd.height=1280\r\n"), Some(2856));
        assert_eq!(native_long_side("hw.lcd.width = 1080\n"), Some(1080));
        assert_eq!(native_long_side("hw.lcd.width = x\n"), None);
        assert_eq!(native_long_side(""), None);
        assert_eq!(request_box(1600, Some(2400)), (1600, 1600));
        assert_eq!(request_box(3000, Some(2400)), (2400, 2400));
        assert_eq!(request_box(3000, None), (3000, 3000));
        // Not measured yet: the screen's own size.
        assert_eq!(request_box(0, Some(2400)), (2400, 2400));
    }

    #[test]
    fn touches_scale_to_the_natural_screen_from_config_ini() {
        let config = "hw.lcd.density = 420\nhw.lcd.height = 2400\nhw.lcd.width = 1080\n";
        assert_eq!(native_size(config), Some((1080, 2400)));
        // A tablet whose natural way is landscape stays so.
        assert_eq!(native_size("hw.lcd.width=2560\r\nhw.lcd.height=1600\r\n"), Some((2560, 1600)));
        assert_eq!(native_size("hw.lcd.width = 1080\n"), None);
        assert_eq!(native_size("hw.lcd.width = 0\nhw.lcd.height = 2400\n"), None);
    }

    #[test]
    fn inactive_display_is_distinct_from_a_bad_visible_frame() {
        assert!(matches!(image_event(Image::default()), Ok(StreamEvent::Inactive)));
        let frame = |width, height| Image {
            format: Some(ImageFormat {
                format: ImgFormat::Rgba8888 as i32,
                width,
                height,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(matches!(image_event(frame(0, 0)), Ok(StreamEvent::Inactive)));
        assert!(matches!(image_event(frame(2, 2)), Ok(StreamEvent::Frame { .. })));
    }

    #[test]
    fn a_bad_first_visible_frame_times_out_but_an_inactive_display_waits() {
        use std::collections::VecDeque;
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let rgba = |bytes| Image {
            format: Some(ImageFormat {
                format: ImgFormat::Rgba8888 as i32, width: 2, height: 2, ..Default::default()
            }),
            image: vec![0; bytes],
            ..Default::default()
        };
        let recovery = Duration::from_millis(20);
        for (frames, expected) in [
            (vec![rgba(5)], true),
            (vec![rgba(5), rgba(16)], false),
            (vec![rgba(5), Image::default()], false),
            (vec![Image::default()], false),
        ] {
            let mut frames = VecDeque::from(frames);
            let mut display = |event| match event {
                StreamEvent::Frame { rgba, .. } if rgba.len() != 16 => Err("bad pixels".into()),
                _ => Ok(()),
            };
            let result = rt.block_on(async {
                tokio::time::timeout(Duration::from_millis(70), pump_frames(
                    async || match frames.pop_front() {
                        Some(frame) => Ok(Some(frame)),
                        None => futures::future::pending().await,
                    },
                    &mut display,
                    recovery,
                )).await
            });
            if expected {
                assert!(matches!(result, Ok(Err(why)) if why.contains("bad pixels")));
            } else {
                assert!(result.is_err(), "{result:?}");
            }
        }
    }

    #[test]
    fn consecutive_invalid_frames_cannot_extend_startup_recovery() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let result = rt.block_on(pump_frames(
            async || Ok(Some(Image { image: vec![1], ..Default::default() })),
            &mut |_| Ok(()),
            Duration::from_millis(20),
        ));
        assert!(matches!(result, Err(why) if why.contains("no format")));
    }

    #[test]
    fn an_error_is_one_line_of_its_causes() {
        let status = tonic::Status::unauthenticated("Missing the 'authorization' header\nmore");
        assert_eq!(status_text(&status), "Unauthenticated: Missing the 'authorization' header");
        assert_eq!(status_text(&tonic::Status::unavailable("")), "Unavailable");
        let inner = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "Connection refused");
        #[derive(Debug)]
        struct Outer(std::io::Error);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("tcp connect error")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        assert_eq!(error_text(&Outer(inner)), "tcp connect error: Connection refused");
        // The emulator quotes a refused token: it never reaches the note.
        let refused = "Unauthenticated: The token `Bearer s3cr3t=` is invalid";
        assert_eq!(
            without_token(refused, Some("s3cr3t=")),
            "Unauthenticated: The token `Bearer …` is invalid"
        );
        assert_eq!(without_token(refused, None), refused);
    }

    /// Manual, on an AVD booted for the purpose (never anyone else's):
    /// `OO_TEST_AVD=Medium_Phone_API_35 cargo test android_stream_delivers_frames
    /// -- --ignored --nocapture`. Streams its picture into the converter
    /// and checks frames come, upright-sized, as 420f buffers.
    #[test]
    #[ignore]
    fn android_stream_delivers_frames() {
        let avd = std::env::var("OO_TEST_AVD").expect("set OO_TEST_AVD");
        let dev = super::super::discovery::running_emulators()
            .into_iter()
            .find(|r| r.avd_id == avd)
            .expect("the AVD is running");
        let native = std::fs::read_to_string(dev.avd_dir.join("config.ini"))
            .ok()
            .and_then(|c| native_long_side(&c));
        let size = request_box(1600, native);
        let (tx, rx) = std::sync::mpsc::channel();
        let mut converter = crate::devices::convert::PixelConverter::new().unwrap();
        let started = std::time::Instant::now();
        let stream = AndroidStream::start(&dev, size, move |event| {
            let report = match event {
                StreamEvent::Frame { width, height, rgba, .. } => {
                    let bytes = rgba.len();
                    let at = std::time::Instant::now();
                    let out = converter.convert_rgba(rgba, width as usize, height as usize);
                    let took = at.elapsed();
                    Ok((width, height, bytes, out.map(|b| {
                        (
                            objc2_core_video::CVPixelBufferGetWidth(&b),
                            objc2_core_video::CVPixelBufferGetHeight(&b),
                            objc2_core_video::CVPixelBufferGetPixelFormatType(&b),
                        )
                    }), took))
                }
                StreamEvent::Inactive => return Ok(()),
                StreamEvent::Ended { gone, why } => Err((gone, why)),
            };
            let _ = tx.send(report);
            Ok(())
        })
        .unwrap();
        eprintln!("connected in {:?}, asked for {size:?} (native long side {native:?})", started.elapsed());
        let first = rx.recv_timeout(Duration::from_secs(10)).expect("a frame within 10 s");
        let (w, h, bytes, out, took) = first.expect("a frame, not an end");
        eprintln!("first frame {w}×{h}, {bytes} bytes, after {:?}; converted in {took:?}", started.elapsed());
        assert!(w > 0 && h > 0);
        assert!(w.max(h) <= size.0.max(1) || size.0 == 0, "{w}×{h} inside {size:?}");
        assert!(bytes >= (w * h * 4) as usize);
        let (bw, bh, format) = out.expect("converted");
        assert_eq!((bw, bh), (w as usize, h as usize));
        assert_eq!(format, objc2_core_video::kCVPixelFormatType_420YpCbCr8BiPlanarFullRange);
        drop(stream);
        // Dropped: nothing more, not even an end.
        let start = std::time::Instant::now();
        while let Ok(late) = rx.recv_timeout(Duration::from_millis(200)) {
            assert!(late.is_ok(), "an end reported after the drop");
            assert!(start.elapsed() < Duration::from_secs(1), "frames after the drop");
        }
        // A wrong token is refused, as a note.
        let wrong = AndroidRunning { token: Some("wrong".into()), ..dev.clone() };
        match AndroidStream::start(&wrong, size, |_| Ok(())) {
            Err(StreamError::Failed(why)) => {
                eprintln!("wrong token: {why}");
                assert!(why.starts_with("Unauthenticated"), "{why}");
                assert!(!why.contains("wrong"), "the token is in the note: {why}");
            }
            Err(e) => panic!("{e:?}"),
            Ok(_) => panic!("a wrong token streamed"),
        }
        // With OO_TEST_AVD_KILL=1, shuts the AVD down mid-stream (adb `emu
        // kill`): the stream reports it gone, once.
        if std::env::var("OO_TEST_AVD_KILL").is_err() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let _stream = AndroidStream::start(&dev, size, move |event| {
            if let StreamEvent::Ended { gone, why } = event {
                let _ = tx.send((gone, why));
            }
            Ok(())
        })
        .unwrap();
        let serial = format!("emulator-{}", dev.serial_port);
        let adb = super::super::controls::adb().expect("adb in the selected Android SDK");
        let killed = std::process::Command::new(adb).args(["-s", &serial, "emu", "kill"]).status();
        assert!(killed.unwrap().success());
        let at = std::time::Instant::now();
        // An emulator saving its snapshot on exit takes a while.
        let (gone, why) = rx.recv_timeout(Duration::from_secs(30)).expect("an end within 30 s");
        eprintln!("ended after {:?}: gone {gone}, {why}", at.elapsed());
        assert!(gone, "{why}");
        assert!(rx.recv_timeout(Duration::from_secs(1)).is_err(), "reported twice");
    }
}
