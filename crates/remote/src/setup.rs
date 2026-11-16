//! Plain-HTTP setup port (7080, only while "Add a phone" is open): the
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

use crate::api::{peer_filter, AppState};
use crate::server::Shared;

pub(crate) fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/setup", get(page))
        .route("/ca.crt", get(ca_crt))
        .route("/ca.mobileconfig", get(ca_mobileconfig))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(middleware::from_fn(peer_filter))
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

async fn page(State(s): AppState) -> Response {
    let (apple, android) = links(&s);
    let fp = escape(&s.ca.fingerprint);
    let (apple, android) = (escape(&apple), escape(&android));
    let html = format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>openOrchestra Remote — set up this phone</title>
<style>
  :root {{ color-scheme: light dark; }}
  body {{ font: 17px/1.45 -apple-system, system-ui, sans-serif; margin: 0 auto; max-width: 36rem; padding: 1.25rem; }}
  h1 {{ font-size: 1.4rem; }}
  .fp {{ font: 13px ui-monospace, monospace; word-break: break-all; padding: .6rem; border: 1px solid #8884; border-radius: .5rem; }}
  a.btn {{ display: block; text-align: center; padding: .9rem; min-height: 44px; box-sizing: border-box; border-radius: .6rem; background: #2563eb; color: #fff; text-decoration: none; margin: .75rem 0; }}
  ol li {{ margin: .4rem 0; }}
  .hidden {{ display: none; }}
</style>
</head>
<body>
<h1>Set up openOrchestra Remote</h1>
<p>Check that this fingerprint matches the one in openOrchestra → Settings → Remote:</p>
<p class="fp">{fp}</p>

<section id="apple">
  <h2>iPhone / iPad</h2>
  <ol>
    <li><a class="btn" href="/ca.mobileconfig">Download the certificate profile</a></li>
    <li>Open <b>Settings</b> → <b>Profile Downloaded</b> → <b>Install</b>.</li>
    <li>Then <b>Settings → General → About → Certificate Trust Settings</b> and turn on full trust for “openOrchestra Remote CA”.</li>
    <li><a class="btn" href="{apple}">Open openOrchestra Remote</a> in Safari, then Share → <b>Add to Home Screen</b>, and pair from the home-screen app.</li>
  </ol>
</section>

<section id="android">
  <h2>Android</h2>
  <ol>
    <li><a class="btn" href="/ca.crt">Download the CA certificate</a></li>
    <li>Open <b>Settings → Security → Encryption &amp; credentials → Install a certificate → CA certificate</b> and pick the downloaded file.</li>
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
