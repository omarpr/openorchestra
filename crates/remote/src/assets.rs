//! The PWA, embedded from `web/remote/dist` at compile time. When the web
//! client wasn't built (cfg `oo_remote_no_web`, set by `build.rs`), every
//! page request gets a 503 saying how to build it.

#[cfg(not(oo_remote_no_web))]
use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

/// Content-Security-Policy for every HTML response.
pub const CSP: &str = "default-src 'self'; connect-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; frame-ancestors 'none'";

/// Whether `web/remote/dist` was embedded into this build.
pub const WEB_CLIENT_BUILT: bool = cfg!(not(oo_remote_no_web));

#[cfg(not(oo_remote_no_web))]
#[derive(rust_embed::RustEmbed)]
#[folder = "../../web/remote/dist"]
struct Dist;

fn with_headers(mut resp: Response, content_type: &str, cache: &'static str) -> Response {
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(content_type) {
        h.insert(header::CONTENT_TYPE, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    if content_type.starts_with("text/html") {
        h.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CSP),
        );
    }
    resp
}

#[cfg_attr(oo_remote_no_web, allow(dead_code))]
fn content_type(path: &str) -> &'static str {
    match path
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("webp") => "image/webp",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("txt") => "text/plain; charset=utf-8",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Stamp this Mac's name into the shell and the manifest, so each Mac's
/// home-screen app is labelled with the Mac it talks to (every Mac serves
/// the same build, from its own origin). Other files pass through.
#[cfg_attr(oo_remote_no_web, allow(dead_code))]
fn personalize(path: &str, body: Vec<u8>, host: &str) -> Vec<u8> {
    let host = host.trim();
    if host.is_empty() {
        return body;
    }
    match path {
        "index.html" => {
            let Ok(html) = String::from_utf8(body.clone()) else {
                return body;
            };
            let h = escape_attr(host);
            html.replacen(
                r#"<meta name="apple-mobile-web-app-title" content="openOrchestra" />"#,
                &format!(r#"<meta name="apple-mobile-web-app-title" content="{h}" />"#),
                1,
            )
            .replacen(
                "<title>openOrchestra</title>",
                &format!(
                    r#"<title>{h} · openOrchestra</title><meta name="oo-host" content="{h}" />"#
                ),
                1,
            )
            .into_bytes()
        }
        "manifest.webmanifest" => {
            let Ok(mut m) = serde_json::from_slice::<serde_json::Value>(&body) else {
                return body;
            };
            if let Some(o) = m.as_object_mut() {
                o.insert("name".into(), format!("{host} · openOrchestra").into());
                o.insert("short_name".into(), host.into());
            }
            serde_json::to_vec(&m).unwrap_or(body)
        }
        _ => body,
    }
}

#[cfg(oo_remote_no_web)]
pub(crate) async fn serve(_s: crate::api::AppState, _uri: Uri) -> Response {
    let body = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>openOrchestra Remote</title></head>\
<body style=\"font-family: system-ui, sans-serif; padding: 1.5rem\">\
<h1>Web client not built</h1>\
<p>Web client not built — run <code>make web</code>, then rebuild openOrchestra.</p>\
</body></html>";
    with_headers(
        (StatusCode::SERVICE_UNAVAILABLE, body).into_response(),
        "text/html; charset=utf-8",
        "no-store",
    )
}

#[cfg(not(oo_remote_no_web))]
pub(crate) async fn serve(State(s): crate::api::AppState, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let (path, file) = match Dist::get(path) {
        Some(f) => (path, f),
        // Client-side routes (no extension) get the shell.
        None if !path.rsplit('/').next().unwrap_or("").contains('.') => {
            match Dist::get("index.html") {
                Some(f) => ("index.html", f),
                None => return StatusCode::NOT_FOUND.into_response(),
            }
        }
        None => return StatusCode::NOT_FOUND.into_response(),
    };
    // Hashed build output is immutable; the shell, manifest and service
    // worker must revalidate so updates land.
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    with_headers(
        personalize(path, file.data.into_owned(), &s.host_name).into_response(),
        content_type(path),
        cache,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types() {
        assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
        assert_eq!(
            content_type("manifest.webmanifest"),
            "application/manifest+json"
        );
        assert_eq!(content_type("sw.js"), "text/javascript; charset=utf-8");
        assert_eq!(content_type("x"), "application/octet-stream");
    }

    const SHELL: &str = r#"<head><meta name="apple-mobile-web-app-title" content="openOrchestra" />
<title>openOrchestra</title></head>"#;

    #[test]
    fn shell_is_named_after_the_mac() {
        let out = String::from_utf8(personalize(
            "index.html",
            SHELL.into(),
            "Omar's \"Mini\" <2>",
        ))
        .unwrap();
        assert!(out.contains(r#"<meta name="apple-mobile-web-app-title" content="Omar's &quot;Mini&quot; &lt;2&gt;" />"#), "{out}");
        assert!(
            out.contains("<title>Omar's &quot;Mini&quot; &lt;2&gt; · openOrchestra</title>"),
            "{out}"
        );
        assert!(
            out.contains(r#"<meta name="oo-host" content="Omar's &quot;Mini&quot; &lt;2&gt;" />"#),
            "{out}"
        );
    }

    #[test]
    fn manifest_is_named_after_the_mac() {
        let m =
            br#"{"name":"openOrchestra","short_name":"openOrchestra","start_url":"/"}"#.to_vec();
        let v: serde_json::Value =
            serde_json::from_slice(&personalize("manifest.webmanifest", m, "Studio")).unwrap();
        assert_eq!(v["name"], "Studio · openOrchestra");
        assert_eq!(v["short_name"], "Studio");
        assert_eq!(v["start_url"], "/");
    }

    /// Guards the exact strings `personalize` looks for in the real build.
    #[cfg(not(oo_remote_no_web))]
    #[test]
    fn built_shell_and_manifest_get_the_mac_name() {
        let html = Dist::get("index.html").unwrap().data.into_owned();
        let html = String::from_utf8(personalize("index.html", html, "Studio")).unwrap();
        assert!(html.contains(r#"apple-mobile-web-app-title" content="Studio""#));
        assert!(html.contains(r#"<meta name="oo-host" content="Studio" />"#));
        let m = Dist::get("manifest.webmanifest").unwrap().data.into_owned();
        let v: serde_json::Value =
            serde_json::from_slice(&personalize("manifest.webmanifest", m, "Studio")).unwrap();
        assert_eq!(v["short_name"], "Studio");
    }

    #[test]
    fn other_files_and_blank_names_pass_through() {
        assert_eq!(personalize("sw.js", b"x".to_vec(), "Studio"), b"x");
        assert_eq!(
            personalize("index.html", SHELL.into(), "  "),
            SHELL.as_bytes()
        );
    }
}
