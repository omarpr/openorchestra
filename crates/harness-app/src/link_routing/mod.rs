//! Decides which links open in the in-app browser tab: local and dev
//! addresses only, gated by the "Open links in a browser tab" setting. The
//! interception itself is [`LinkRoutingPlatform`], which gpui starts with.

mod platform;

pub use platform::{LinkRoutingPlatform, take_routed_receiver};

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use url::{Host, Url};

static ROUTING_ENABLED: AtomicBool = AtomicBool::new(true);

/// Mirror of the `links_in_browser_tab` pref, readable from anywhere without
/// threading the workspace through. Set at prefs load and when the toggle saves.
pub fn set_routing_enabled(on: bool) {
    ROUTING_ENABLED.store(on, Ordering::Relaxed);
}

pub fn routing_enabled() -> bool {
    ROUTING_ENABLED.load(Ordering::Relaxed)
}

/// Whether `url` should open in the in-app browser tab rather than the
/// system browser.
pub fn should_route(url: &str, enabled: bool) -> bool {
    enabled && is_local_url(url)
}

/// True for http(s) URLs whose host is a loopback, `.localhost`, `.local`,
/// private-range (10/8, 172.16/12, 192.168/16), Tailscale CGNAT (100.64/10)
/// or `.ts.net` address. The host comes from `url::Url`, never string
/// prefixes, so `localhost.evil.com` and `localhost@evil.com` stay external.
pub fn is_local_url(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    match parsed.host() {
        Some(Host::Domain(d)) => {
            [".localhost", ".local", ".ts.net"]
                .iter()
                .any(|s| d.ends_with(s))
                || d == "localhost"
        }
        Some(Host::Ipv4(ip)) => {
            let [a, b, ..] = ip.octets();
            a == 127
                || a == 10
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 100 && (64..=127).contains(&b))
        }
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Hands `url` to the system (the real platform's `open_url`): the default
/// browser for web pages, the owning app for any other scheme. Bypasses
/// routing, so the browser tab can send a page out without it bouncing back
/// in. Only absolute URLs are passed on. At most one open per
/// [`OPEN_INTERVAL`]; extras are dropped, so a page in a script loop can't
/// launch apps without bound. For page-originated opens; a user's own click
/// goes through [`open_external_now`].
pub fn open_external(url: &str) {
    let Some(url) = launch_target(url) else {
        log::warn!("open_external: not an absolute, openable URL");
        return;
    };
    {
        let mut last = LAST_OPEN.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if !open_allowed(*last, now) {
            log::info!("open_external: dropped a {}: link (rate limit)", scheme_of(&url));
            return;
        }
        *last = Some(now);
    }
    open_now(&url);
}

/// [`open_external`] without the rate limit, for something the user did
/// themselves: the browser tab's "Open in default browser" button, or an
/// app address (`mailto:`, `vscode:` …) typed into its address bar. Same
/// argument safety: only absolute URLs, never `file:`, `javascript:` or a
/// page-internal `about:`, `data:` or `blob:` URL.
pub fn open_external_now(url: &str) {
    match launch_target(url) {
        Some(url) => open_now(&url),
        None => log::warn!("open_external_now: not an absolute, openable URL"),
    }
}

/// What the system is given for `url`: its normalized absolute form, unless
/// it is a local file (opening `file:///…/x.app` launches it), a script, or
/// a page's own plumbing (`about:`, `data:`, `blob:`): the browser tab loads
/// those inside the page (`webview.rs`), and a top-level `data:` page's
/// content is the page's to choose, so "Open in default browser" on one
/// must not hand it to the system.
fn launch_target(url: &str) -> Option<String> {
    let url = normalized_absolute_url(url)?;
    match scheme_of(&url) {
        "file" | "javascript" | "about" | "data" | "blob" => None,
        _ => Some(url),
    }
}

/// Opens `url` through the real platform (`NSWorkspace openURL:` on macOS),
/// never through the routing wrapper. Without one on this thread (unit tests,
/// or a caller off the main thread) it's dropped with a log line rather than
/// shelled out, so a test run can never launch the user's browser.
fn open_now(url: &str) {
    if !platform::open_with_inner(url) {
        log::warn!("open_external: no platform on this thread; dropped a {}: link", scheme_of(url));
    }
}

/// Minimum time between two [`open_external`] launches.
const OPEN_INTERVAL: Duration = Duration::from_secs(1);

static LAST_OPEN: Mutex<Option<Instant>> = Mutex::new(None);

fn open_allowed(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|t| now.saturating_duration_since(t) >= OPEN_INTERVAL)
}

/// `url` as `url::Url` serializes it, when it is absolute (`url::Url` refuses
/// relative references). That normalized form is what the system is given.
fn normalized_absolute_url(url: &str) -> Option<String> {
    Url::parse(url).ok().map(|u| u.as_str().to_owned())
}

fn scheme_of(url: &str) -> &str {
    url.split(':').next().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_local_and_dev_hosts() {
        for u in [
            "http://localhost:3000",
            "HTTP://LOCALHOST:3000/x",
            "https://app.localhost",
            "http://127.0.0.1:8080",
            "http://127.4.5.6",
            "http://[::1]:5173",
            "http://printer.local",
            "http://10.1.2.3",
            "http://172.16.0.1",
            "http://172.31.255.255",
            "http://192.168.1.10:3000",
            "https://mac.tail1234.ts.net",
            "http://100.64.0.1",
            "http://100.127.255.255",
            "http://evil.com@localhost/",
        ] {
            assert!(is_local_url(u), "{u}");
        }
    }

    #[test]
    fn leaves_public_and_look_alike_hosts_external() {
        for u in [
            "https://github.com/omarpr/openorchestra",
            "http://localhost.evil.com",
            "http://10.0.0.1.evil.com",
            "http://localhost@evil.com/",
            "http://172.32.0.1",
            "http://100.128.0.1",
            "https://ts.net.evil.com",
            "file:///etc/hosts",
            "mailto:a@localhost",
            "zed://x",
            "not a url",
        ] {
            assert!(!is_local_url(u), "{u}");
        }
    }

    #[test]
    fn should_route_needs_the_setting_and_a_local_url() {
        assert!(should_route("http://localhost:3000", true));
        assert!(!should_route("http://localhost:3000", false));
        assert!(!should_route("https://github.com", true));
    }

    #[test]
    fn open_external_takes_only_absolute_urls() {
        for u in ["http://localhost:3000", "https://github.com", "mailto:a@b.c", "zed://x"] {
            assert!(normalized_absolute_url(u).is_some(), "{u}");
        }
        for u in ["", "/relative/path", "//host/x", "-n", "not a url"] {
            assert!(normalized_absolute_url(u).is_none(), "{u}");
        }
    }

    #[test]
    fn open_external_passes_the_normalized_url() {
        assert_eq!(
            normalized_absolute_url("HTTP://Example.COM:80/a b").as_deref(),
            Some("http://example.com/a%20b")
        );
        assert_eq!(normalized_absolute_url("mailto:a@b.c").as_deref(), Some("mailto:a@b.c"));
    }

    #[test]
    fn user_opens_refuse_local_files_scripts_and_page_internal_urls() {
        assert_eq!(launch_target("mailto:a@b.c").as_deref(), Some("mailto:a@b.c"));
        assert_eq!(
            launch_target("HTTP://Example.COM/a b").as_deref(),
            Some("http://example.com/a%20b")
        );
        assert_eq!(launch_target("vscode://file/x").as_deref(), Some("vscode://file/x"));
        for u in [
            "file:///etc/hosts",
            "FILE:///Applications/Calc.app",
            "javascript:alert(1)",
            "about:blank",
            "ABOUT:srcdoc",
            "data:text/html,<h1>hi</h1>",
            "blob:http://localhost:3000/1f2e",
            "",
            "-n",
            "/relative",
        ] {
            assert!(launch_target(u).is_none(), "{u}");
        }
    }

    #[test]
    fn open_external_allows_one_open_per_second() {
        let t0 = Instant::now();
        assert!(open_allowed(None, t0));
        assert!(!open_allowed(Some(t0), t0));
        assert!(!open_allowed(Some(t0), t0 + Duration::from_millis(999)));
        assert!(open_allowed(Some(t0), t0 + OPEN_INTERVAL));
        // A clock that reads earlier than the last open never panics.
        assert!(!open_allowed(Some(t0 + OPEN_INTERVAL), t0));
    }

    // The only test that touches the process-global flag.
    #[test]
    fn routing_follows_the_setting_at_call_time() {
        set_routing_enabled(false);
        assert!(!should_route("http://localhost:3000", routing_enabled()));
        set_routing_enabled(true);
        assert!(should_route("http://localhost:3000", routing_enabled()));
    }
}
