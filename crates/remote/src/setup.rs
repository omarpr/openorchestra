//! Plain-HTTP setup port (7080, only while "Add a device" is open): the
//! platform-detecting setup page and the public CA. Nothing else — never the
//! API or the PWA.

use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

use crate::api::{peer_filter, security_headers, trusted_filter, AppState};
use crate::server::Shared;

pub(crate) fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/setup", get(page))
        .route("/ca.crt", get(ca_crt))
        .route("/ca.mobileconfig", get(ca_mobileconfig))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(middleware::from_fn_with_state(
            shared.clone(),
            trusted_filter,
        ))
        .layer(middleware::from_fn(peer_filter))
        .layer(middleware::from_fn(security_headers))
        .with_state(shared)
}

async fn ca_crt(State(s): AppState) -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/x-x509-ca-cert"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"openOrchestra-Remote-CA.crt\"",
            ),
        ],
        s.ca.pem.clone(),
    )
        .into_response()
}

async fn ca_mobileconfig(State(s): AppState) -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/x-apple-aspen-config"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"openOrchestra-Remote.mobileconfig\"",
            ),
        ],
        s.ca.mobileconfig.clone(),
    )
        .into_response()
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Apple: `https://<LocalHostName>.local:<port>/`; Android:
/// `https://<primary IP>:<port>/` (the default-route interface's address, see
/// `certs::primary_ipv4`), else the first non-loopback SAN IP. Falls back to
/// whatever the cert has.
fn links(s: &Shared) -> (String, String) {
    let (hosts, ips) = s.sans.read().unwrap_or_else(|e| e.into_inner()).clone();
    let primary = s
        .status
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .primary_ip
        .filter(|p| ips.contains(p));
    let port = s.port;
    let ip_url = |ip: &IpAddr| match ip {
        IpAddr::V4(v4) => format!("https://{v4}:{port}/"),
        IpAddr::V6(v6) => format!("https://[{v6}]:{port}/"),
    };
    let lan_ip = primary
        .iter()
        .chain(ips.iter().filter(|ip| !ip.is_loopback()))
        .next()
        .or(ips.first())
        .map(ip_url)
        .unwrap_or_else(|| format!("https://127.0.0.1:{port}/"));
    let apple = hosts
        .iter()
        .find(|h| h.ends_with(".local"))
        .map(|h| format!("https://{h}:{port}/"))
        .unwrap_or_else(|| lan_ip.clone());
    (apple, lan_ip)
}

/// An iPhone/iPad browser that is not Safari (Chrome, Firefox, Edge, the
/// Google app, …). iOS installs configuration profiles only from Safari, so
/// these get told to reopen the page there.
pub(crate) fn ios_non_safari(ua: &str) -> bool {
    let ios = ["iPhone", "iPad", "iPod"].iter().any(|d| ua.contains(d));
    let other = [
        "CriOS",
        "FxiOS",
        "EdgiOS",
        "OPiOS",
        "GSA/",
        "DuckDuckGo",
        "FBAN",
        "Instagram",
    ]
    .iter()
    .any(|b| ua.contains(b));
    ios && other
}

async fn page(State(s): AppState, headers: axum::http::HeaderMap) -> Response {
    let (apple, android) = links(&s);
    let (apple, android) = (escape(&apple), escape(&android));
    let header_str = |name| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    };
    let banner = if ios_non_safari(header_str(header::USER_AGENT)) {
        let here = escape(&format!("http://{}/setup", header_str(header::HOST)));
        format!(
            r#"<div id="use-safari" class="warn">
  <b>Open this page in Safari.</b> iPhone and iPad only install certificate profiles from Safari —
  this browser can’t. Copy this address into Safari:
  <input readonly value="{here}" onclick="this.select()">
</div>"#
        )
    } else {
        String::new()
    };
    let html = format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>openOrchestra Remote — set up this device</title>
<style>
  :root {{ color-scheme: light dark; }}
  body {{ font: 17px/1.45 -apple-system, system-ui, sans-serif; margin: 0 auto; max-width: 36rem; padding: 1.25rem; }}
  h1 {{ font-size: 1.4rem; }}
  a.btn {{ display: block; text-align: center; padding: .9rem; min-height: 44px; box-sizing: border-box; border-radius: .6rem; background: #2563eb; color: #fff; text-decoration: none; margin: .75rem 0; }}
  ol li {{ margin: .4rem 0; }}
  .hidden {{ display: none; }}
  .warn {{ border: 2px solid #d97706; border-radius: .6rem; padding: .8rem; margin: 1rem 0; }}
  .warn input {{ display: block; width: 100%; box-sizing: border-box; margin-top: .5rem; padding: .6rem; font: 15px ui-monospace, monospace; }}
</style>
</head>
<body>
<h1>Set up openOrchestra Remote</h1>
{banner}
<p class="warn"><b>Check the certificate before you trust it.</b> This page comes over an unencrypted
connection, so it can't vouch for itself. Your device's own certificate screen can: compare the
<b>SHA-256 fingerprint</b> it shows with the one in the “Add a device” window on your Mac. They must
match exactly. If they don't, stop: someone on this network may be interfering.</p>

<section id="apple">
  <h2>iPhone / iPad</h2>
  <p>Do steps 1–2 in <b>Safari</b>: iOS won’t install a certificate profile from Chrome or other browsers.</p>
  <ol>
    <li><a class="btn" href="/ca.mobileconfig">Download the certificate profile</a> and tap <b>Allow</b> when asked.</li>
    <li>Open the <b>Settings</b> app → <b>Profile Downloaded</b> (near the top). If it isn’t there, look in <b>General → VPN &amp; Device Management</b>. iOS discards a downloaded profile after a few minutes; download it again if it’s gone.</li>
    <li><b>Before tapping Install</b>: tap <b>More Details</b> → the certificate, and check its <b>SHA-256</b> fingerprint against your Mac. Then <b>Install</b>.</li>
    <li>Then <b>Settings → General → About → Certificate Trust Settings</b> and turn on full trust for “openOrchestra Remote CA”.</li>
    <li><a class="btn" href="{apple}">Open openOrchestra Remote</a>, then Share → <b>Add to Home Screen</b>, and pair from the home-screen app.</li>
  </ol>
</section>

<section id="android">
  <h2>Android</h2>
  <ol>
    <li><a class="btn" href="/ca.crt">Download the CA certificate</a></li>
    <li>Open <b>Settings → Security → Encryption &amp; credentials → Install a certificate → CA certificate</b> and pick the downloaded file.</li>
    <li>Then <b>Encryption &amp; credentials → User credentials</b> (or <b>Trusted credentials → User</b>) → <b>openOrchestra Remote CA</b>, and check its <b>SHA-256</b> fingerprint against your Mac. If it doesn't match, remove it.</li>
    <li><a class="btn" href="{android}">Open openOrchestra Remote</a> in Chrome, then menu → <b>Install app</b>, and pair from the installed app.</li>
  </ol>
  <p>Tip: reserve this Mac’s IP address on your router. If it changes, re-add the app.</p>
</section>

<script>
  var ua = navigator.userAgent;
  var apple = /iPhone|iPad|iPod/.test(ua) || (/Macintosh/.test(ua) && navigator.maxTouchPoints > 1);
  var android = /Android/.test(ua);
  if (apple) document.getElementById('android').className = 'hidden';
  if (android) document.getElementById('apple').className = 'hidden';
</script>
</body>
</html>
"#
    );
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        html,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::ios_non_safari;

    #[test]
    fn only_non_safari_ios_browsers_are_told_to_switch() {
        let safari = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_1 like Mac OS X) AppleWebKit/605.1.15 \
            (KHTML, like Gecko) Version/18.1 Mobile/15E148 Safari/604.1";
        let chrome = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_1 like Mac OS X) AppleWebKit/605.1.15 \
            (KHTML, like Gecko) CriOS/131.0.6778.73 Mobile/15E148 Safari/604.1";
        let google_app = "Mozilla/5.0 (iPad; CPU OS 18_1 like Mac OS X) AppleWebKit/605.1.15 \
            (KHTML, like Gecko) GSA/340.0.700 Mobile/15E148 Safari/604.1";
        let android_chrome = "Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 \
            (KHTML, like Gecko) Chrome/131.0.0.0 Mobile Safari/537.36";
        assert!(!ios_non_safari(safari));
        assert!(ios_non_safari(chrome));
        assert!(ios_non_safari(google_app));
        assert!(!ios_non_safari(android_chrome));
        assert!(!ios_non_safari(""));
    }
}
