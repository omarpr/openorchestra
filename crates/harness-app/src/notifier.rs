//! Native OS notifications when an agent needs input (architecture §4.6, A5).
//!
//! Fed from the [`Bus`](agent_runtime::Bus) that every adapter publishes on, so
//! this is agent-neutral: it does not know or care whether the signal arrived
//! as a status file, an SSE event, or a hook shim.
//!
//! Three rules, each of which exists because breaking it makes the feature
//! worse than not having it:
//!
//! 1. **Only exact signals may claim "input needed"** (R11). Enforced upstream
//!    by `AgentSignal::should_notify`, so a best-effort adapter reporting
//!    turn-complete can never be dressed up as a request for input.
//! 2. **Suppress when the user is already looking at that worktree.** Telling
//!    someone about the prompt they are staring at is pure noise.
//! 3. **Only on a transition into Waiting**, never on a repeat, so a flapping
//!    adapter cannot spam the user.
//!
//! macOS delivery goes through `osascript`. That is deliberate: the
//! `UNUserNotificationCenter` route §4.6 names requires the process to be a
//! properly registered bundle, which is true for `openOrchestra.app` but *not*
//! for `cargo run` during development — so the native path would silently do
//! nothing exactly when a developer is trying to test it. `osascript` works in
//! both, with no new dependency. Linux uses `notify-send` for the same reason
//! (`notify-rust` remains the tidier long-term option).

use std::path::Path;
use std::process::{Command, Stdio};

/// What kind of attention an agent wants — drives the message wording.
#[derive(Clone, Copy)]
pub enum AgentAlert {
    NeedsAttention,
    Finished,
}

/// Post an agent notification.
///
/// On macOS, when running as a real bundle, this goes through
/// `UNUserNotificationCenter`: the banner names the agent, carries a **View**
/// button (activates the app and navigates to the worktree) and a **Dismiss**
/// button, and clicking the body acts like View. The worktree id rides in the
/// notification request identifier, so the response handler needs no other
/// state. Everywhere else — dev binaries, other OSes — it falls back to the
/// buttonless system path (osascript / notify-send).
pub fn notify_agent(
    agent: &str,
    worktree_name: &str,
    repo: &str,
    worktree_id: &str,
    alert: AgentAlert,
    sound: &str,
) {
    let (title, body) = alert_text(agent, worktree_name, repo, alert);
    log::info!("NOTIFY posting: {title}: {body}");
    #[cfg(target_os = "macos")]
    {
        if mac::post(&title, &body, worktree_id, sound) {
            return;
        }
    }
    let _ = (worktree_id, sound);
    post(&title, &body);
}

/// The navigation channel: the platform delegate pushes a worktree id when
/// the user clicks View (or the banner body); the workspace polls and
/// selects that worktree.
static NAV: std::sync::OnceLock<(
    crossbeam_channel::Sender<String>,
    crossbeam_channel::Receiver<String>,
)> = std::sync::OnceLock::new();

fn nav() -> &'static (
    crossbeam_channel::Sender<String>,
    crossbeam_channel::Receiver<String>,
) {
    NAV.get_or_init(crossbeam_channel::unbounded)
}

/// The receiver the workspace polls for View clicks.
pub fn nav_rx() -> crossbeam_channel::Receiver<String> {
    nav().1.clone()
}

/// One-time platform setup. Call from the main thread at app start.
pub fn init() {
    #[cfg(target_os = "macos")]
    mac::init(nav().0.clone());
}

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::OnceLock;

    use block2::{DynBlock, RcBlock};
    use crossbeam_channel::Sender;
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
    use objc2_foundation::{NSBundle, NSObject, NSObjectProtocol, NSSet, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
        UNNotificationAction, UNNotificationActionOptions, UNNotificationCategory,
        UNNotificationCategoryOptions, UNNotificationDefaultActionIdentifier,
        UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
        UNUserNotificationCenter, UNUserNotificationCenterDelegate,
    };

    const CATEGORY: &str = "harness-agent-attention";
    const ACTION_VIEW: &str = "harness-view";
    const ACTION_DISMISS: &str = "harness-dismiss";
    const ID_PREFIX: &str = "harness-wt:";

    struct Ivars {
        nav: Sender<String>,
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "HarnessNotificationDelegate"]
        #[ivars = Ivars]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UNUserNotificationCenterDelegate for Delegate {
            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(
                &self,
                _center: &UNUserNotificationCenter,
                response: &UNNotificationResponse,
                completion: &DynBlock<dyn Fn()>,
            ) {
                let action = response.actionIdentifier();
                let is_view = action.isEqualToString(&NSString::from_str(ACTION_VIEW))
                    || action.isEqualToString(unsafe { UNNotificationDefaultActionIdentifier });
                if is_view {
                    let id = response.notification().request().identifier().to_string();
                    if let Some(worktree) = id.strip_prefix(ID_PREFIX) {
                        // The id carries a uuid suffix after '#' so repeated
                        // notifications for one worktree do not coalesce.
                        let worktree = worktree.split('#').next().unwrap_or(worktree);
                        let _ = self.ivars().nav.send(worktree.to_string());
                    }
                }
                completion.call(());
            }

            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &UNUserNotificationCenter,
                _notification: &UNNotification,
                completion: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
            ) {
                // Show even while the app is frontmost: the user may be in a
                // different worktree tab, which is exactly the case that
                // needs the banner.
                completion.call((UNNotificationPresentationOptions::Banner,));
            }
        }
    );

    impl Delegate {
        fn new(nav: Sender<String>) -> Retained<Self> {
            let this = Self::alloc().set_ivars(Ivars { nav });
            unsafe { msg_send![super(this), init] }
        }
    }

    static DELEGATE: OnceLock<Retained<Delegate>> = OnceLock::new();
    static READY: OnceLock<bool> = OnceLock::new();

    // Retained<Delegate> is not Send/Sync by default; the delegate is only
    // touched from init (main thread) and by the system afterwards.
    unsafe impl Send for Ivars {}
    unsafe impl Sync for Ivars {}

    fn bundled() -> bool {
        NSBundle::mainBundle().bundleIdentifier().is_some()
    }

    pub fn init(nav: Sender<String>) {
        if !bundled() {
            // The framework requires a real bundle; the dev binary keeps the
            // osascript fallback.
            let _ = READY.set(false);
            return;
        }
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let delegate = DELEGATE.get_or_init(|| Delegate::new(nav));
        center.setDelegate(Some(ProtocolObject::from_ref(&**delegate)));

        let view = UNNotificationAction::actionWithIdentifier_title_options(
            &NSString::from_str(ACTION_VIEW),
            &NSString::from_str("View"),
            UNNotificationActionOptions::Foreground,
        );
        let dismiss = UNNotificationAction::actionWithIdentifier_title_options(
            &NSString::from_str(ACTION_DISMISS),
            &NSString::from_str("Dismiss"),
            UNNotificationActionOptions::empty(),
        );
        let category =
            UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
                &NSString::from_str(CATEGORY),
                &objc2_foundation::NSArray::from_retained_slice(&[view, dismiss]),
                &objc2_foundation::NSArray::new(),
                UNNotificationCategoryOptions::empty(),
            );
        center.setNotificationCategories(&NSSet::from_retained_slice(&[category]));

        let done = RcBlock::new(
            |granted: objc2::runtime::Bool, _err: *mut objc2_foundation::NSError| {
                log::info!("notification authorization granted = {}", granted.as_bool());
            },
        );
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
            &done,
        );
        let _ = READY.set(true);
    }

    /// Post through the notification center. Returns false when unavailable
    /// (unbundled dev run) so the caller can fall back.
    pub fn post(title: &str, body: &str, worktree_id: &str, sound: &str) -> bool {
        if !READY.get().copied().unwrap_or(false) {
            return false;
        }
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        content.setCategoryIdentifier(&NSString::from_str(CATEGORY));
        // The chosen sound, resolved from its logical name. `soundNamed`
        // searches the bundle's Resources *and* the Library/Sounds
        // directories, so one call covers both our originals and the macOS
        // system sounds. Silence is expressed by setting no sound at all —
        // there is no "quiet" sound to name.
        {
            use crate::sounds::{self, Source};
            let chosen = sounds::resolve(sound);
            match chosen.source {
                Source::Silent => {}
                Source::SystemDefault => content.setSound(Some(
                    &objc2_user_notifications::UNNotificationSound::defaultSound(),
                )),
                Source::Bundled | Source::System => {
                    if let Some(file) = sounds::file_name(chosen) {
                        content.setSound(Some(
                            &objc2_user_notifications::UNNotificationSound::soundNamed(
                                &NSString::from_str(&file),
                            ),
                        ));
                    }
                }
            }
        }
        // Unique per posting (nanos suffix) or macOS replaces the previous
        // banner for the same identifier.
        let id = format!(
            "{ID_PREFIX}{worktree_id}#{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        );
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&id),
            &content,
            None,
        );
        let done = RcBlock::new(|err: *mut objc2_foundation::NSError| {
            if !err.is_null() {
                log::warn!("notification post failed");
            }
        });
        center.addNotificationRequest_withCompletionHandler(&request, Some(&done));
        true
    }
}

// The buttonless fallback (dev binaries, other OSes). Fire-and-forget: a
// failure to notify must never disturb the app, so every error path is
// swallowed after being logged.
#[cfg(target_os = "macos")]
fn post(title: &str, body: &str) {
    // AppleScript string literals take double quotes and backslash escapes;
    // anything else in a branch or repo name would break the script, so escape
    // rather than interpolate raw.
    let script = format!(
        "display notification \"{}\" with title \"{}\" sound name \"Funk\"",
        escape(body),
        escape(title)
    );
    if let Err(e) = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        log::warn!("could not post notification: {e}");
    }
}

#[cfg(target_os = "linux")]
fn post(title: &str, body: &str) {
    // `--` so a title or body starting with a dash cannot parse as a flag,
    // matching the arg-injection discipline used for git and gh.
    if let Err(e) = Command::new("notify-send")
        .arg("--app-name=openOrchestra")
        .arg("--")
        .arg(title)
        .arg(body)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        log::warn!("could not post notification: {e}");
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn post(title: &str, body: &str) {
    log::info!("notification: {title} — {body}");
}

/// Escape a string for embedding in an AppleScript double-quoted literal.
#[cfg(target_os = "macos")]
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Human-readable label for a worktree, preferring its editable name.
/// Title and body for an agent alert. The repo is parenthesised only when
/// known: an empty one rendered as a stray "()" in every banner.
pub fn alert_text(agent: &str, worktree_name: &str, repo: &str, alert: AgentAlert) -> (String, String) {
    let who = if repo.is_empty() {
        worktree_name.to_string()
    } else {
        format!("{worktree_name} ({repo})")
    };
    match alert {
        AgentAlert::NeedsAttention => (
            format!("{agent} needs your attention!"),
            format!("{who} is waiting on you"),
        ),
        AgentAlert::Finished => (
            format!("{agent} finished — your turn"),
            format!("{who} is ready for review"),
        ),
    }
}

pub fn label_for(name: &str, path: &Path) -> String {
    if name.is_empty() {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_alert_never_shows_empty_parentheses() {
        use super::{alert_text, AgentAlert};
        let (_, body) = alert_text("Claude Code", "main", "msrosa-tutoring", AgentAlert::NeedsAttention);
        assert_eq!(body, "main (msrosa-tutoring) is waiting on you");
        let (_, body) = alert_text("Claude Code", "scratch", "", AgentAlert::Finished);
        assert_eq!(body, "scratch is ready for review");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn applescript_literals_are_escaped() {
        // A branch called `feat/"quoted"` would otherwise terminate the
        // AppleScript string and change what gets executed.
        assert_eq!(super::escape(r#"a "b" c"#), r#"a \"b\" c"#);
        assert_eq!(super::escape(r"back\slash"), r"back\\slash");
    }

    #[test]
    fn label_falls_back_to_the_directory_name() {
        let path = std::path::Path::new("/wt/app/nav");
        assert_eq!(super::label_for("", path), "nav");
        assert_eq!(super::label_for("Navigation", path), "Navigation");
    }
}
