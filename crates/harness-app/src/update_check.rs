//! "About openOrchestra" and "Check for updates…" (the app menu).
//!
//! The update check runs only when the user picks the menu item: one request
//! to GitHub's public releases API, nothing sent but the request itself, and
//! never in the background. It goes through the system `curl` so the app
//! doesn't grow an HTTP client (zed's stays blocked, see workspace_host.rs).

use std::path::Path;
use std::process::Command;

use gpui::{App, ClipboardItem, PromptLevel, Window};

/// This build's version: package-mac.sh stamps the bundle from the same
/// `Cargo.toml` field, so the two always agree.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const REPO_URL: &str = "https://github.com/omarpr/openorchestra";
const RELEASES_URL: &str = "https://github.com/omarpr/openorchestra/releases";
const LATEST_API: &str = "https://api.github.com/repos/omarpr/openorchestra/releases/latest";
const BREW_UPGRADE: &str = "brew upgrade --cask openorchestra";

/// The newest published release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub url: String,
}

/// `1.2.3` from a tag (`v1.2.3` too). `None` for anything else, including
/// pre-releases, which the "latest" endpoint never returns anyway.
fn parse_version(tag: &str) -> Option<(u64, u64, u64)> {
    let t = tag.trim();
    let t = t.strip_prefix('v').unwrap_or(t);
    let mut parts = t.split('.');
    let v = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(v)
}

/// Whether `latest` is newer than `current`. An unparseable version on
/// either side is never "newer": no update offer we can't justify.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// The release in a `releases/latest` API response.
pub fn parse_latest(json: &[u8]) -> Result<Release, String> {
    let v: serde_json::Value =
        serde_json::from_slice(json).map_err(|_| "GitHub sent an unexpected response.".to_string())?;
    let tag = v["tag_name"]
        .as_str()
        .ok_or_else(|| "No published release was found.".to_string())?;
    if parse_version(tag).is_none() {
        return Err(format!("The latest release has an unexpected version ({tag})."));
    }
    let tag = tag.trim();
    let url = v["html_url"]
        .as_str()
        .filter(|u| u.starts_with(RELEASES_URL))
        .map(str::to_string)
        .unwrap_or_else(|| format!("{RELEASES_URL}/latest"));
    Ok(Release {
        version: tag.strip_prefix('v').unwrap_or(tag).to_string(),
        url,
    })
}

/// Ask GitHub for the latest release. Blocking: run it off the main thread.
pub fn fetch_latest() -> Result<Release, String> {
    let out = Command::new("/usr/bin/curl")
        .args([
            // Must come first: ignore ~/.curlrc, which could add headers to
            // stdout (`include`) and break parsing, or change the request.
            "-q",
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--max-time",
            "15",
            "--proto",
            "=https",
            "--header",
            "Accept: application/vnd.github+json",
            "--user-agent",
            &format!("openOrchestra/{VERSION}"),
            LATEST_API,
        ])
        .output()
        .map_err(|e| format!("Could not run curl: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim().trim_start_matches("curl: ").trim();
        return Err(if err.is_empty() {
            "Could not reach GitHub.".to_string()
        } else {
            format!("Could not reach GitHub ({err}).")
        });
    }
    parse_latest(&out.stdout)
}

/// Whether this copy came from the Homebrew cask (upgrade with brew, so
/// brew's record of the installed version stays right).
fn installed_with_homebrew() -> bool {
    ["/opt/homebrew/Caskroom/openorchestra", "/usr/local/Caskroom/openorchestra"]
        .iter()
        .any(|p| Path::new(p).exists())
}

/// The About panel's credits: the one-line description, then a clickable
/// link to the repository, small, grey and centered like the panel's own text.
/// Built from attributes rather than HTML: HTML import goes through WebKit,
/// which must run on the main thread and can't be exercised by a unit test.
#[cfg(target_os = "macos")]
fn about_credits() -> Option<objc2::rc::Retained<objc2::runtime::AnyObject>> {
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::msg_send;
    use objc2_foundation::{NSRange, NSString};

    const BLURB: &str =
        "A native macOS app to run coding agents in parallel git worktrees, with Zed built in.";
    const LINK_TEXT: &str = "github.com/omarpr/openorchestra";
    let text = format!("{BLURB}\n\n{LINK_TEXT}");
    // NSRange counts UTF-16 units.
    let utf16 = |s: &str| s.encode_utf16().count();
    let all = NSRange::new(0, utf16(&text));
    let link = NSRange::new(utf16(BLURB) + 2, utf16(LINK_TEXT));

    let class = |name: &std::ffi::CStr| AnyClass::get(name);
    // SAFETY: Foundation/AppKit messages with valid arguments and ranges
    // inside the string they apply to.
    unsafe {
        let string: Allocated<AnyObject> = msg_send![class(c"NSMutableAttributedString")?, alloc];
        let string: Option<Retained<AnyObject>> =
            msg_send![string, initWithString: &*NSString::from_str(&text)];
        let string = string?;
        let add = |name: &str, value: &AnyObject, range: NSRange| {
            let name = NSString::from_str(name);
            let _: () = msg_send![&*string, addAttribute: &*name, value: value, range: range];
        };
        let font: Retained<AnyObject> = msg_send![class(c"NSFont")?, systemFontOfSize: 11.0f64];
        let grey: Retained<AnyObject> = msg_send![class(c"NSColor")?, secondaryLabelColor];
        let centered: Retained<AnyObject> = msg_send![class(c"NSMutableParagraphStyle")?, new];
        // NSTextAlignmentCenter differs by architecture: Apple silicon uses
        // the iOS numbering (center = 1, right = 2), Intel the old AppKit one
        // (right = 1, center = 2). The universal build carries both slices.
        let center: isize = if cfg!(target_arch = "aarch64") { 1 } else { 2 };
        let _: () = msg_send![&*centered, setAlignment: center];
        let url: Option<Retained<AnyObject>> =
            msg_send![class(c"NSURL")?, URLWithString: &*NSString::from_str(REPO_URL)];
        add("NSFont", &font, all);
        add("NSColor", &grey, all);
        add("NSParagraphStyle", &centered, all);
        let url = url?;
        add("NSLink", &url, link);
        Some(string)
    }
}

/// The standard macOS About panel: the app icon (the logo) at full size, the
/// name, the version, and the credits above.
#[cfg(target_os = "macos")]
fn show_native_about() -> bool {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::msg_send;
    use objc2_foundation::NSString;

    let (Some(app_class), Some(dict_class)) = (
        AnyClass::get(c"NSApplication"),
        AnyClass::get(c"NSMutableDictionary"),
    ) else {
        return false;
    };
    // SAFETY: plain AppKit/Foundation messages with valid arguments, sent on
    // the main thread (menu actions run there).
    unsafe {
        let options: Retained<AnyObject> = msg_send![dict_class, new];
        let set = |key: &str, value: &AnyObject| {
            let key = NSString::from_str(key);
            let _: () = msg_send![&*options, setObject: value, forKey: &*key];
        };
        set("ApplicationName", &NSString::from_str("openOrchestra"));
        set("ApplicationVersion", &NSString::from_str(VERSION));
        set("Copyright", &NSString::from_str("Free software under the GPL-3.0 license."));
        if let Some(credits) = about_credits() {
            set("Credits", &credits);
        }
        let app: Retained<AnyObject> = msg_send![app_class, sharedApplication];
        let _: () = msg_send![&*app, activateIgnoringOtherApps: true];
        let _: () = msg_send![&*app, orderFrontStandardAboutPanelWithOptions: &*options];
    }
    true
}

pub fn show_about(window: &mut Window, cx: &mut App) {
    #[cfg(target_os = "macos")]
    if show_native_about() {
        return;
    }
    let detail = format!(
        "Version {VERSION}\n\n\
         A native macOS app to run coding agents in parallel git worktrees, with Zed built in.\n\n\
         Free software under the GPL-3.0 license."
    );
    let answer = window.prompt(
        PromptLevel::Info,
        "openOrchestra",
        Some(&detail),
        &["OK", "View on GitHub"],
        cx,
    );
    cx.spawn(async move |cx| {
        if answer.await.ok() == Some(1) {
            cx.update(|cx| cx.open_url(REPO_URL));
        }
    })
    .detach();
}

pub fn check_for_updates(window: &mut Window, cx: &mut App) {
    let task = cx.background_executor().spawn(async move { fetch_latest() });
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let result = task.await;
        handle
            .update(cx, |_, window, cx| show_result(result, window, cx))
            .ok();
    })
    .detach();
}

fn show_result(result: Result<Release, String>, window: &mut Window, cx: &mut App) {
    match result {
        Ok(r) if is_newer(&r.version, VERSION) => {
            let brew = installed_with_homebrew();
            let detail = if brew {
                format!(
                    "You have {VERSION}. You installed openOrchestra with Homebrew, so upgrade with:\n\n{BREW_UPGRADE}"
                )
            } else {
                format!("You have {VERSION}. Download the new version from GitHub and replace the app in Applications.")
            };
            let buttons: &[&str] = if brew {
                &["Copy Command", "Release Notes", "Later"]
            } else {
                &["Download", "Later"]
            };
            let answer = window.prompt(
                PromptLevel::Info,
                &format!("openOrchestra {} is available", r.version),
                Some(&detail),
                buttons,
                cx,
            );
            cx.spawn(async move |cx| {
                let choice = answer.await.ok();
                cx.update(|cx| match (brew, choice) {
                    (true, Some(0)) => {
                        cx.write_to_clipboard(ClipboardItem::new_string(BREW_UPGRADE.to_string()))
                    }
                    (true, Some(1)) | (false, Some(0)) => cx.open_url(&r.url),
                    _ => {}
                });
            })
            .detach();
        }
        Ok(_) => {
            // Only "OK": nothing to do with the answer, so stop listening.
            // The alert stays up either way.
            drop(window.prompt(
                PromptLevel::Info,
                "You’re up to date",
                Some(&format!("openOrchestra {VERSION} is the latest version.")),
                &["OK"],
                cx,
            ));
        }
        Err(e) => {
            let answer = window.prompt(
                PromptLevel::Warning,
                "Couldn’t check for updates",
                Some(&e),
                &["OK", "Open Releases Page"],
                cx,
            );
            cx.spawn(async move |cx| {
                if answer.await.ok() == Some(1) {
                    cx.update(|cx| cx.open_url(RELEASES_URL));
                }
            })
            .detach();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("1.0.1", "1.0.0"));
        assert!(is_newer("1.10.0", "1.9.9"));
        assert!(is_newer("v2.0.0", "1.99.99"));
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("0.9.0", "1.0.0"));
        // Nothing we can't parse is ever offered as an update.
        assert!(!is_newer("1.1.0-beta.1", "1.0.0"));
        assert!(!is_newer("nightly", "1.0.0"));
        assert!(!is_newer("1.1", "1.0.0"));
        assert!(!is_newer("1.1.0.0", "1.0.0"));
    }

    #[test]
    fn this_build_has_a_comparable_version() {
        assert!(parse_version(VERSION).is_some(), "{VERSION}");
    }

    #[test]
    fn reads_the_latest_release() {
        let r = parse_latest(
            br#"{"tag_name":"1.2.0","html_url":"https://github.com/omarpr/openorchestra/releases/tag/1.2.0"}"#,
        )
        .unwrap();
        assert_eq!(r.version, "1.2.0");
        assert_eq!(r.url, "https://github.com/omarpr/openorchestra/releases/tag/1.2.0");
        assert_eq!(parse_latest(br#"{"tag_name":"v1.2.0"}"#).unwrap().version, "1.2.0");
    }

    #[test]
    fn never_opens_a_link_outside_the_releases_page() {
        let r = parse_latest(br#"{"tag_name":"1.2.0","html_url":"https://evil.example/x"}"#).unwrap();
        assert_eq!(r.url, "https://github.com/omarpr/openorchestra/releases/latest");
    }

    /// Runs every AppKit message the credits send, so a wrong selector or
    /// argument type fails here (objc2 checks encodings in debug builds)
    /// rather than when someone opens About.
    #[cfg(target_os = "macos")]
    #[test]
    fn builds_the_about_credits() {
        use objc2::msg_send;
        use objc2::rc::Retained;
        use objc2::runtime::AnyObject;
        use objc2_foundation::NSString;
        let credits = about_credits().expect("credits");
        let text: Retained<NSString> = unsafe { msg_send![&*credits, string] };
        assert!(text.to_string().ends_with("github.com/omarpr/openorchestra"));
        let (mut range, at) = (objc2_foundation::NSRange::new(0, 0), text.length() - 1);
        let link: Option<Retained<AnyObject>> = unsafe {
            msg_send![&*credits, attribute: &*NSString::from_str("NSLink"), atIndex: at, effectiveRange: &mut range]
        };
        assert!(link.is_some(), "the repository link is clickable");
        assert_eq!(range.location + range.length, text.length());
    }

    #[test]
    fn explains_bad_responses() {
        assert!(parse_latest(b"not json").is_err());
        assert!(parse_latest(br#"{"message":"Not Found"}"#).is_err());
        assert!(parse_latest(br#"{"tag_name":"latest"}"#).is_err());
    }
}
