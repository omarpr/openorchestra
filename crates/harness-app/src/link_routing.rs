//! Decides which links open in the in-app browser tab: local and dev
//! addresses only, gated by the "Open links in a browser tab" setting.

use std::sync::atomic::{AtomicBool, Ordering};

use url::{Host, Url};

static ROUTING_ENABLED: AtomicBool = AtomicBool::new(true);

/// Mirror of the `links_in_browser_tab` pref, readable from anywhere without
/// threading the workspace through. Set at prefs load and when the toggle saves.
pub fn set_routing_enabled(on: bool) {
    ROUTING_ENABLED.store(on, Ordering::Relaxed);
}

#[allow(dead_code)] // first caller arrives with link routing (Task 4)
pub fn routing_enabled() -> bool {
    ROUTING_ENABLED.load(Ordering::Relaxed)
}

/// Whether `url` should open in the in-app browser tab rather than the
/// system browser.
#[allow(dead_code)] // first caller arrives with link routing (Task 4)
pub fn should_route(url: &str, enabled: bool) -> bool {
    enabled && is_local_url(url)
}

/// True for http(s) URLs whose host is a loopback, `.localhost`, `.local`,
/// private-range (10/8, 172.16/12, 192.168/16), Tailscale CGNAT (100.64/10)
/// or `.ts.net` address. The host comes from `url::Url`, never string
/// prefixes, so `localhost.evil.com` and `localhost@evil.com` stay external.
#[allow(dead_code)] // first caller arrives with link routing (Task 4)
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

    // The only test that touches the process-global flag.
    #[test]
    fn routing_follows_the_setting_at_call_time() {
        set_routing_enabled(false);
        assert!(!should_route("http://localhost:3000", routing_enabled()));
        set_routing_enabled(true);
        assert!(should_route("http://localhost:3000", routing_enabled()));
    }
}
