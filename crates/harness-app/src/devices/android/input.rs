//! Input to the Android emulator the Device tab shows: touches, keys and
//! pastes, through its gRPC `sendTouch`, `sendKey` and `setClipboard` (see
//! "Spike findings" 5 and 6 in the Android Device tab design).
//!
//! - **Touches** are device pixels of the screen in its natural
//!   orientation (`hw.lcd.width` × `hw.lcd.height`), whichever way it is
//!   turned: the UI maps a click back through the turn the frame came with
//!   to 0…1 ratios of that screen, and they are scaled here. A finger is
//!   down with pressure 1024 (the guest's range) and lifts with 0.
//! - **Keys** go one keystroke per call, as Chromium-style USB codes
//!   (`0x07 << 16 | usage`): the emulator's `text` drops non-ASCII and
//!   silently drops a whole call over its budget, so it is never used.
//! - **A paste** (⌘V) sets the emulator's clipboard, then presses Ctrl+V.
//! - Scrolling is a synthesised swipe, as on iOS: `injectWheel` does
//!   nothing on a phone AVD.
//!
//! One [`AndroidInput`] per watched emulator. The UI only queues; a thread
//! of its own connects on the first input (on a current-thread tokio
//! runtime), then sends in order, each run of moves cut to its last. The
//! first call that fails ends it, reported as a [`HelperEvent::Failed`]
//! like the simulator helper's, so the tab's retry and keyboard rules apply
//! unchanged. Dropping the handle lets it send what is already queued (a
//! lift, the last keys) and end.

use std::time::Duration;

use futures::channel::mpsc::UnboundedSender;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender as CmdSender};

use super::grpc::keyboard_event::{KeyCodeType, KeyEventType};
use super::grpc::{ClipData, KeyboardEvent, Touch, TouchEvent};
use super::stream::{Client, authorized, connect, native_size, status_text, without_token};
use crate::devices::AndroidRunning;
use crate::devices::input::{DeviceInput, HelperEvent, MAX_PASTE, TouchPhase};

/// A finger's pressure while down: the top of the guest's 0–1024 range.
pub(crate) const PRESSURE: i32 = 1024;
/// The one finger the mouse and the scroll drive.
pub(crate) const FINGER: i32 = 0;
/// The USB HID keyboard page, as the emulator wants it on each usage.
pub(crate) const USB_KEYBOARD: u32 = 0x07 << 16;
/// USB HID usages (keyboard page).
const LEFT_CONTROL: u16 = 0xE0;
const KEY_V: u16 = 0x19;
/// The key names that press Android's Home and Back buttons.
pub(crate) const HOME_KEY: &str = "GoHome";
pub(crate) const BACK_KEY: &str = "GoBack";
/// How long one input call may take before the emulator counts as not
/// answering.
const CALL_TIMEOUT: Duration = Duration::from_secs(3);
/// The guest's clipboard and its keys aren't kept in order with each
/// other: a paste's Ctrl+V sent straight after its clipboard, or the next
/// paste's clipboard sent straight after it, can paste the wrong text (seen
/// live: three pastes back to back gave the first, then the last twice).
/// So a paste waits this long after setting the clipboard, and again after
/// its Ctrl+V before anything else goes.
pub(crate) const PASTE_SETTLE: Duration = Duration::from_millis(250);

/// A point given as 0…1 ratios of the natural screen, in the device pixels
/// of a `(width, height)` screen; never off it.
pub(crate) fn device_point(x: f64, y: f64, (width, height): (u32, u32)) -> (i32, i32) {
    let pixel = |ratio: f64, side: u32| {
        let ratio = if ratio.is_nan() { 0.5 } else { ratio.clamp(0.0, 1.0) };
        let last = f64::from(side.max(1) - 1);
        (ratio * f64::from(side)).round().min(last) as i32
    };
    (pixel(x, width), pixel(y, height))
}

/// The touch event for the finger at device pixel `at`: pressed while
/// down or moving, released (pressure 0) when it lifts.
pub(crate) fn touch_event(phase: TouchPhase, (x, y): (i32, i32)) -> TouchEvent {
    let pressure = if phase == TouchPhase::Up { 0 } else { PRESSURE };
    TouchEvent {
        touches: vec![Touch { x, y, identifier: FINGER, pressure, ..Default::default() }],
        display: 0,
    }
}

/// A USB keyboard usage going down or up.
pub(crate) fn usb_key(usage: u16, down: bool) -> KeyboardEvent {
    let event_type = if down { KeyEventType::Keydown } else { KeyEventType::Keyup };
    KeyboardEvent {
        code_type: KeyCodeType::Usb as i32,
        event_type: event_type as i32,
        key_code: (USB_KEYBOARD | u32::from(usage)) as i32,
        ..Default::default()
    }
}

/// A press of the key named `name` (`HOME_KEY`, `BACK_KEY`).
pub(crate) fn named_key(name: &str) -> KeyboardEvent {
    KeyboardEvent {
        event_type: KeyEventType::Keypress as i32,
        key: name.to_string(),
        ..Default::default()
    }
}

/// What a paste of `text` sends: the clipboard, then Ctrl+V.
pub(crate) fn paste_calls(text: &str) -> (ClipData, Vec<KeyboardEvent>) {
    let keys = vec![
        usb_key(LEFT_CONTROL, true),
        usb_key(KEY_V, true),
        usb_key(KEY_V, false),
        usb_key(LEFT_CONTROL, false),
    ];
    (ClipData { text: text.to_string() }, keys)
}

/// What one paste puts on the clipboard: at most [`MAX_PASTE`] characters,
/// as on a simulator.
pub(crate) fn paste_text(text: &str) -> String {
    text.chars().take(MAX_PASTE).collect()
}

/// After a paste's keys failed with `sent` of them gone, the key that lets
/// go of Ctrl, if it was left held.
pub(crate) fn ctrl_release_after(sent: usize) -> Option<KeyboardEvent> {
    // Ctrl goes down first and up last (`paste_calls`).
    (1..4).contains(&sent).then(|| usb_key(LEFT_CONTROL, false))
}

/// One thing to send.
#[derive(Clone, Debug, PartialEq)]
enum Cmd {
    /// x and y are 0…1 ratios of the natural screen.
    Touch(TouchPhase, f64, f64),
    Key(u16, bool),
    Paste(String),
}

impl Cmd {
    fn is_move(&self) -> bool {
        matches!(self, Cmd::Touch(TouchPhase::Move, ..))
    }
}

/// Commands waiting to be sent, each run of moves cut to its last (as for
/// the simulator helper, `input::coalesce`).
fn coalesce(cmds: Vec<Cmd>) -> Vec<Cmd> {
    let mut out: Vec<Cmd> = Vec::with_capacity(cmds.len());
    for cmd in cmds {
        if cmd.is_move() && out.last().is_some_and(Cmd::is_move) {
            out.pop();
        }
        out.push(cmd);
    }
    out
}

/// Input to one running emulator.
pub(crate) struct AndroidInput {
    cmds: CmdSender<Cmd>,
}

impl AndroidInput {
    /// Starts input to `dev` on a thread of its own; what becomes of it
    /// arrives on `events`, tagged `serial`. Err only when the thread
    /// can't start.
    pub(crate) fn spawn(
        dev: &AndroidRunning,
        serial: u64,
        events: UnboundedSender<HelperEvent>,
    ) -> Result<AndroidInput, String> {
        let (cmds, rx) = tokio::sync::mpsc::unbounded_channel();
        let dev = dev.clone();
        std::thread::Builder::new()
            .name("oo-android-input".into())
            .spawn(move || {
                let token = dev.token.clone();
                if let Err(why) = run(&dev, serial, rx, &events) {
                    let why = without_token(&why, token.as_deref());
                    log::info!("device: emulator input stopped: {why}");
                    let _ = events.unbounded_send(HelperEvent::Failed(serial, why));
                }
            })
            .map_err(|e| format!("starting the emulator input thread: {e}"))?;
        Ok(AndroidInput { cmds })
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.cmds.send(cmd);
    }
}

impl DeviceInput for AndroidInput {
    fn touch(&self, phase: TouchPhase, x: f64, y: f64) {
        self.send(Cmd::Touch(phase, x, y));
    }

    fn key(&self, usage: u16, down: bool) {
        self.send(Cmd::Key(usage, down));
    }

    /// Through the emulator's clipboard, in one go: any text, up to
    /// [`MAX_PASTE`] characters.
    fn text(&self, text: &str) {
        self.send(Cmd::Paste(paste_text(text)));
    }
}

/// The input thread: connects on the first command, then sends each batch
/// until the handle is dropped (and what it queued is sent) or a call
/// fails. It waits on the runtime, which keeps the connection's own task
/// running meanwhile.
fn run(
    dev: &AndroidRunning,
    serial: u64,
    mut cmds: UnboundedReceiver<Cmd>,
    events: &UnboundedSender<HelperEvent>,
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("starting the input runtime: {e}"))?;
    let token = dev.token.as_deref();
    runtime.block_on(async {
        // Nothing is connected until there is something to send.
        let Some(first) = cmds.recv().await else {
            return Ok(());
        };
        if !super::discovery::still_emulator(dev.pid) {
            return Err("The emulator has shut down".to_string());
        }
        let size = std::fs::read_to_string(dev.avd_dir.join("config.ini"))
            .ok()
            .and_then(|config| native_size(&config))
            .ok_or_else(|| "The emulator's screen size isn't in its config.ini".to_string())?;
        let mut client = connect(dev.grpc_port).await.map_err(|(_, why)| why)?;
        let _ = events.unbounded_send(HelperEvent::Ready(serial));
        let mut worked = false;
        let mut batch = vec![first];
        loop {
            while let Ok(more) = cmds.try_recv() {
                batch.push(more);
            }
            for cmd in coalesce(std::mem::take(&mut batch)) {
                send(&mut client, cmd, size, token).await?;
                if !std::mem::replace(&mut worked, true) {
                    let _ = events.unbounded_send(HelperEvent::Worked(serial));
                }
            }
            // Between batches; None once the handle is gone and its queue
            // sent.
            match cmds.recv().await {
                Some(next) => batch.push(next),
                None => return Ok(()),
            }
        }
    })
}

/// Sends one command, each call bounded by [`CALL_TIMEOUT`].
async fn send(client: &mut Client, cmd: Cmd, size: (u32, u32), token: Option<&str>) -> Result<(), String> {
    match cmd {
        Cmd::Touch(phase, x, y) => {
            let event = touch_event(phase, device_point(x, y, size));
            within(client.send_touch(authorized(event, token)?)).await
        }
        Cmd::Key(usage, down) => within(client.send_key(authorized(usb_key(usage, down), token)?)).await,
        Cmd::Paste(text) => {
            let (clip, keys) = paste_calls(&text);
            within(client.set_clipboard(authorized(clip, token)?)).await?;
            tokio::time::sleep(PASTE_SETTLE).await;
            for (sent, key) in keys.into_iter().enumerate() {
                let went = match authorized(key, token) {
                    Ok(key) => within(client.send_key(key)).await,
                    Err(e) => Err(e),
                };
                if let Err(why) = went {
                    // Best effort: never leave Ctrl held on the device.
                    if let Some(up) = ctrl_release_after(sent)
                        && let Ok(up) = authorized(up, token)
                    {
                        let _ = within(client.send_key(up)).await;
                    }
                    return Err(why);
                }
            }
            tokio::time::sleep(PASTE_SETTLE).await;
            Ok(())
        }
    }
}

/// A call's outcome within [`CALL_TIMEOUT`], as one line.
async fn within<T>(
    call: impl std::future::Future<Output = Result<tonic::Response<T>, tonic::Status>>,
) -> Result<(), String> {
    match tokio::time::timeout(CALL_TIMEOUT, call).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(status)) => Err(status_text(&status)),
        Err(_) => Err(format!("The emulator didn't answer within {} s", CALL_TIMEOUT.as_secs())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_touch_is_device_pixels_with_full_pressure_until_it_lifts() {
        let size = (1080, 2400);
        assert_eq!(device_point(0.0, 0.0, size), (0, 0));
        assert_eq!(device_point(0.5, 0.5, size), (540, 1200));
        // The far edge is the last pixel, and nothing goes off the screen.
        assert_eq!(device_point(1.0, 1.0, size), (1079, 2399));
        assert_eq!(device_point(-0.2, 1.7, size), (0, 2399));
        assert_eq!(device_point(f64::NAN, 0.25, size), (540, 600));
        for (phase, pressure) in [(TouchPhase::Down, PRESSURE), (TouchPhase::Move, PRESSURE), (TouchPhase::Up, 0)] {
            let event = touch_event(phase, (300, 200));
            assert_eq!(event.display, 0);
            let [touch] = event.touches[..] else { panic!("{event:?}") };
            assert_eq!((touch.x, touch.y, touch.identifier, touch.pressure), (300, 200, FINGER, pressure));
        }
    }

    #[test]
    fn keys_are_usb_codes_one_keystroke_a_call() {
        let a = usb_key(0x04, true);
        assert_eq!(a.code_type, KeyCodeType::Usb as i32);
        assert_eq!(a.event_type, KeyEventType::Keydown as i32);
        assert_eq!(a.key_code, 0x070004);
        assert!(a.key.is_empty() && a.text.is_empty());
        let shift_up = usb_key(0xE1, false);
        assert_eq!((shift_up.event_type, shift_up.key_code), (KeyEventType::Keyup as i32, 0x0700E1));
    }

    #[test]
    fn back_and_home_use_key_names() {
        for (name, want) in [(HOME_KEY, "GoHome"), (BACK_KEY, "GoBack")] {
            let key = named_key(name);
            assert_eq!(key.key, want);
            assert_eq!(key.event_type, KeyEventType::Keypress as i32);
            assert_eq!(key.key_code, 0);
            assert!(key.text.is_empty());
        }
    }

    #[test]
    fn a_paste_is_the_clipboard_then_ctrl_v() {
        let (clip, keys) = paste_calls("héllo, wörld");
        assert_eq!(clip.text, "héllo, wörld");
        let sent: Vec<(i32, i32)> = keys.iter().map(|k| (k.event_type, k.key_code)).collect();
        let (down, up) = (KeyEventType::Keydown as i32, KeyEventType::Keyup as i32);
        assert_eq!(sent, [(down, 0x0700E0), (down, 0x070019), (up, 0x070019), (up, 0x0700E0)]);
        assert!(keys.iter().all(|k| k.code_type == KeyCodeType::Usb as i32 && k.text.is_empty()));
    }

    #[test]
    fn a_failed_paste_lets_go_of_ctrl() {
        // Nothing sent, or all four: Ctrl isn't held.
        assert!(ctrl_release_after(0).is_none());
        assert!(ctrl_release_after(4).is_none());
        // Ctrl went down and its up didn't go: let go of it.
        for sent in 1..=3 {
            let up = ctrl_release_after(sent).expect("Ctrl up");
            assert_eq!((up.event_type, up.key_code), (KeyEventType::Keyup as i32, 0x0700E0));
        }
    }

    #[test]
    fn an_android_paste_is_capped_like_ios() {
        assert_eq!(paste_text("héllo"), "héllo");
        let long = "é".repeat(MAX_PASTE + 50);
        assert_eq!(paste_text(&long).chars().count(), MAX_PASTE);
    }

    #[test]
    fn moves_are_coalesced_and_everything_else_kept_in_order() {
        use TouchPhase::*;
        let t = |p, x| Cmd::Touch(p, x, 0.5);
        let sent = coalesce(vec![
            t(Down, 0.1),
            t(Move, 0.2),
            t(Move, 0.3),
            t(Up, 0.3),
            Cmd::Key(4, true),
            t(Move, 0.4),
            Cmd::Paste("x".into()),
            t(Move, 0.5),
            t(Move, 0.6),
        ]);
        assert_eq!(
            sent,
            [t(Down, 0.1), t(Move, 0.3), t(Up, 0.3), Cmd::Key(4, true), t(Move, 0.4), Cmd::Paste("x".into()), t(Move, 0.6)]
        );
    }
}

