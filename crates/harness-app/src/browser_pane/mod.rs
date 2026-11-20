//! The browser tab: an address bar drawn by gpui over a native WebKit view
//! (`webview.rs`) that sits on top of the tab body.
//!
//! **Lifetime.** The native view exists only while the tab is shown
//! ([`BrowserPane::set_shown`]); the workspace decides that in one place
//! (`HarnessWorkspace::sync_browser_views`). Hiding drops the view and keeps
//! the URL and title, so a background tab costs ~0 memory and reloads when
//! shown again.
//!
//! **Callback bridge.** `NativeEvents` callbacks arrive on the main queue with
//! no gpui context. Each native view gets its own unbounded channel: the
//! callbacks only `unbounded_send` a [`PageEvent`], and a `spawn_in` task
//! holding a `WeakEntity<BrowserPane>` drains it and applies each event with
//! a window and context. Channel and task live and die with that one view, so
//! nothing an old view queued can reach the next one.
//!
//! **Placement.** The tab body is a `canvas` whose prepaint reads the pane
//! (no update, no notify) and hands its bounds to `set_frame`, so the page
//! follows gpui layout every frame without feeding back into it.
//!
//! The pane talks to the workspace through [`BrowserEvent`]s: a changed URL
//! or title (relabel the tab, save), and a page's request for a new window.

mod webview;

use std::cell::Cell;
use std::time::Duration;

use editor::Editor;
use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    Styled, Task, Window, canvas, div, prelude::*, px, svg,
};

use crate::link_routing;
use crate::theme::{HarnessTheme, theme, tint};
use crate::workspace::{browser_label, tip};

pub(crate) use webview::window_is_minimized_or_hidden;
use webview::{NativeEvents, NativeWebView};

/// What the address bar makes of what was typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Address {
    /// An http(s) page to load in the tab.
    Web(String),
    /// An app address (`mailto:`, `vscode:` …): handed to the system, never
    /// loaded in the page.
    External(String),
}

/// Typed input → an address, or `None` for nothing usable. Not a search box.
/// - `host:port` (digits after the first colon) is checked *before* URL
///   parsing, which would read `localhost:3000` as scheme `localhost`; it
///   gets `http://`.
/// - With no scheme: `http://` for local and dev hosts (localhost,
///   `.localhost`, `.local`, loopback and private addresses), `https://`
///   otherwise, Tailscale `.ts.net` names included.
/// - `http`/`https` load; `file:`, `javascript:` and the page-internal
///   `about:`, `data:`, `blob:` are refused; any other scheme goes out.
pub(crate) fn normalize_address(input: &str) -> Option<Address> {
    let input = input.trim();
    if input.is_empty() || input.chars().any(char::is_whitespace) {
        return None;
    }
    if !is_host_port(input) && has_scheme(input) {
        let parsed = url::Url::parse(input).ok()?;
        return match parsed.scheme() {
            "http" | "https" => parsed
                .host_str()
                .is_some_and(|h| !h.is_empty())
                .then(|| Address::Web(parsed.into())),
            "file" | "javascript" | "about" | "data" | "blob" => None,
            _ => Some(Address::External(parsed.into())),
        };
    }
    let scheme = if is_host_port(input) || wants_http(input) {
        "http"
    } else {
        "https"
    };
    let parsed = url::Url::parse(&format!("{scheme}://{input}")).ok()?;
    parsed.host_str()?;
    Some(Address::Web(parsed.into()))
}

/// `host:port`, optionally followed by a path, query or fragment:
/// `localhost:3000`, `192.168.1.5:8080/x`, `[::1]:5173`.
fn is_host_port(input: &str) -> bool {
    let (host, rest) = if input.starts_with('[') {
        match input.find(']') {
            Some(end) => input.split_at(end + 1),
            None => return false,
        }
    } else {
        match input.find(':') {
            Some(i) => input.split_at(i),
            None => return false,
        }
    };
    let Some(rest) = rest.strip_prefix(':') else {
        return false;
    };
    let port_len = rest.bytes().take_while(u8::is_ascii_digit).count();
    !host.is_empty()
        && !host.contains(['/', '?', '#', '@'])
        && port_len > 0
        && rest[port_len..]
            .chars()
            .next()
            .is_none_or(|c| matches!(c, '/' | '?' | '#'))
}

/// Starts with a URL scheme (`letter (letter|digit|+|-|.)* :`).
fn has_scheme(input: &str) -> bool {
    let Some((scheme, _)) = input.split_once(':') else {
        return false;
    };
    scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// A scheme-less local or dev host, which serves plain http. Tailscale names
/// are local to routing but serve https.
fn wants_http(input: &str) -> bool {
    let candidate = format!("http://{input}");
    link_routing::is_local_url(&candidate)
        && !url::Url::parse(&candidate)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.ends_with(".ts.net")))
            .unwrap_or(false)
}

/// The tab's name for a page: its title, else its host, else "Browser".
pub(crate) fn page_label(url: Option<&str>, title: Option<&str>) -> String {
    let host = url
        .and_then(|u| url::Url::parse(u).ok())
        .and_then(|u| u.host_str().map(str::to_owned));
    browser_label(title, host.as_deref())
}

/// What the pane tells the workspace.
pub(crate) enum BrowserEvent {
    /// The URL or title changed: relabel the tab and save.
    Changed,
    /// The page asked for a new window (`target=_blank`, `window.open`).
    NewWindow(String),
}

impl EventEmitter<BrowserEvent> for BrowserPane {}

/// One `NativeEvents` callback, carried over the channel.
enum PageEvent {
    Title(String),
    Url(String),
    Loading(bool),
    Failed(Option<String>, String),
    Notice(String),
    NewWindow(String),
}

/// A load that failed, shown in place of the page.
struct LoadFailure {
    url: String,
    reason: String,
}

/// The live native view and the task draining its callbacks.
struct Native {
    view: NativeWebView,
    _events: Task<()>,
}

/// How long a notice (e.g. an unsupported download) stays in the bar.
const NOTICE_TTL: Duration = Duration::from_secs(5);

pub(crate) struct BrowserPane {
    /// The page's URL; `None` for a blank tab.
    url: Option<String>,
    title: Option<String>,
    /// The last address the tab loaded on purpose, named by the error when
    /// WebKit doesn't say which URL failed.
    attempted: Option<String>,
    loading: bool,
    failure: Option<LoadFailure>,
    notice: Option<String>,
    _notice_timer: Option<Task<()>>,
    /// The workspace wants this tab on screen (see `set_shown`).
    shown: bool,
    native: Option<Native>,
    address: Entity<Editor>,
    focus_handle: FocusHandle,
    /// Hand the page keyboard focus once its view is next placed (after
    /// Enter or Esc in the address bar).
    focus_page: Cell<bool>,
}

impl BrowserPane {
    pub(crate) fn new(
        url: Option<String>,
        title: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let url = url.filter(|u| !u.is_empty());
        let address = cx.new(|cx| {
            let mut e = Editor::single_line(window, cx);
            e.set_placeholder_text("Enter an address, like localhost:3000", window, cx);
            if let Some(url) = &url {
                e.set_text(url.as_str(), window, cx);
            }
            e
        });
        Self {
            attempted: url.clone(),
            url,
            title: title.filter(|t| !t.trim().is_empty()),
            loading: false,
            failure: None,
            notice: None,
            _notice_timer: None,
            shown: false,
            native: None,
            address,
            focus_handle: cx.focus_handle(),
            focus_page: Cell::new(false),
        }
    }

    pub(crate) fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    pub(crate) fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// The tab's name, from the page title or host.
    pub(crate) fn label(&self) -> String {
        page_label(self.url(), self.title())
    }

    /// Where keyboard focus goes when the tab is activated: the address bar
    /// of a blank tab, the pane otherwise.
    pub(crate) fn focus_target(&self, cx: &App) -> FocusHandle {
        if self.url.is_none() {
            self.address.focus_handle(cx)
        } else {
            self.focus_handle.clone()
        }
    }

    /// Load `url` (absolute). Before the view exists it is loaded when the
    /// tab is next shown.
    pub(crate) fn navigate(&mut self, url: String, cx: &mut Context<Self>) {
        self.failure = None;
        self.attempted = Some(url.clone());
        if self.url.as_deref() != Some(url.as_str()) {
            self.url = Some(url.clone());
            cx.emit(BrowserEvent::Changed);
        }
        if let Some(native) = &self.native {
            native.view.load(&url);
        }
        cx.notify();
    }

    /// `true` creates the native view (when there is a page to show); `false`
    /// drops it, keeping the URL and title. Idempotent, and it never
    /// notifies: the workspace calls it while rendering.
    pub(crate) fn set_shown(&mut self, shown: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.shown == shown {
            return;
        }
        self.shown = shown;
        if shown {
            self.ensure_native(window, cx);
        } else {
            self.native = None;
            self.loading = false;
            // A focus request the page never got to take (e.g. Enter that
            // ended in an error) must not grab the keyboard on a later visit.
            self.focus_page.set(false);
        }
    }

    /// Create the native view if the tab is shown and has a page. Loading it
    /// is a fresh attempt, so a stale error is cleared.
    fn ensure_native(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.shown || self.native.is_some() {
            return;
        }
        let Some(url) = self.url.clone() else {
            return;
        };
        let (tx, mut rx) = mpsc::unbounded();
        match NativeWebView::new(window, &url, page_events(tx)) {
            Ok(view) => {
                let events = cx.spawn_in(window, async move |this, cx| {
                    while let Some(ev) = rx.next().await {
                        if this
                            .update_in(cx, |pane, window, cx| pane.apply(ev, window, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                });
                self.failure = None;
                self.attempted = Some(url);
                self.native = Some(Native {
                    view,
                    _events: events,
                });
            }
            Err(e) => {
                log::warn!("browser: could not create the web view: {e}");
                self.failure = Some(LoadFailure {
                    url,
                    reason: "the page view could not be created".to_string(),
                });
            }
        }
    }

    fn apply(&mut self, ev: PageEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            PageEvent::Title(title) => {
                let title = Some(title).filter(|t| !t.trim().is_empty());
                if title != self.title {
                    self.title = title;
                    cx.emit(BrowserEvent::Changed);
                }
            }
            PageEvent::Url(url) => {
                if self.url.as_deref() != Some(url.as_str()) {
                    self.url = Some(url);
                    cx.emit(BrowserEvent::Changed);
                }
                self.sync_address(window, cx);
            }
            PageEvent::Loading(loading) => self.loading = loading,
            PageEvent::Failed(url, reason) => {
                let url = url
                    .or_else(|| self.attempted.clone())
                    .or_else(|| self.url.clone())
                    .unwrap_or_default();
                self.failure = Some(LoadFailure { url, reason });
                self.loading = false;
            }
            PageEvent::Notice(text) => self.show_notice(text, cx),
            PageEvent::NewWindow(url) => cx.emit(BrowserEvent::NewWindow(url)),
        }
        cx.notify();
    }

    /// Show the page's URL in the address bar, unless the user is typing.
    fn sync_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.address.focus_handle(cx).is_focused(window) {
            self.reset_address(window, cx);
        }
    }

    /// Put the page's URL back in the address bar.
    fn reset_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.url.clone().unwrap_or_default();
        if self.address.read(cx).text(cx) != text {
            self.address
                .update(cx, |e, cx| e.set_text(text.as_str(), window, cx));
        }
    }

    fn show_notice(&mut self, text: String, cx: &mut Context<Self>) {
        self.notice = Some(text);
        // Replacing the timer cancels the previous one.
        self._notice_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NOTICE_TTL).await;
            let _ = this.update(cx, |pane, cx| {
                pane.notice = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Enter in the address bar.
    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.address.read(cx).text(cx);
        match normalize_address(&typed) {
            Some(Address::Web(url)) => {
                self.address
                    .update(cx, |e, cx| e.set_text(url.as_str(), window, cx));
                self.navigate(url, cx);
                window.focus(&self.focus_handle, cx);
                self.focus_page.set(true);
            }
            Some(Address::External(url)) => {
                link_routing::open_external_now(&url);
                self.reset_address(window, cx);
            }
            None if typed.trim().is_empty() => {}
            None => self.show_notice(format!("Can't open “{}” here", typed.trim()), cx),
        }
    }

    /// Esc in the address bar: put the page's URL back and return to the page.
    fn cancel(&mut self, _: &menu::Cancel, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        self.reset_address(window, cx);
        self.focus_page.set(true);
        cx.notify();
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        self.failure = None;
        if let Some(native) = &self.native {
            native.view.back();
        }
        cx.notify();
    }

    fn forward(&mut self, cx: &mut Context<Self>) {
        self.failure = None;
        if let Some(native) = &self.native {
            native.view.forward();
        }
        cx.notify();
    }

    /// Reload, or after a failed load try that address again: WebKit's own
    /// reload repeats the last *committed* page, not the one that failed.
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(failure) = self.failure.take() {
            self.attempted = Some(failure.url.clone());
            match &self.native {
                Some(native) => native.view.load(&failure.url),
                None => self.ensure_native(window, cx),
            }
        } else if let Some(native) = &self.native {
            native.view.reload();
        }
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(native) = &self.native {
            native.view.stop();
        }
        cx.notify();
    }

    fn render_bar(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        let native = self.native.as_ref().map(|n| &n.view);
        let can_back = native.is_some_and(|v| v.can_go_back());
        let can_forward = native.is_some_and(|v| v.can_go_forward());
        let has_page = self.url.is_some();
        let reload = if self.loading {
            bar_button(hx, "browser-stop", ::ui::IconName::Close, "Stop", true)
                .on_click(cx.listener(|pane, _, _, cx| pane.stop(cx)))
        } else {
            bar_button(hx, "browser-reload", ::ui::IconName::RotateCw, "Reload", has_page)
                .when(has_page, |d| {
                    d.on_click(cx.listener(|pane, _, window, cx| pane.reload(window, cx)))
                })
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .h(px(32.0))
            .px_2()
            .bg(hx.bg1)
            .border_b_1()
            .border_color(hx.border)
            .child(
                bar_button(hx, "browser-back", ::ui::IconName::ArrowLeft, "Back", can_back)
                    .when(can_back, |d| {
                        d.on_click(cx.listener(|pane, _, _, cx| pane.back(cx)))
                    }),
            )
            .child(
                bar_button(
                    hx,
                    "browser-forward",
                    ::ui::IconName::ArrowRight,
                    "Forward",
                    can_forward,
                )
                .when(can_forward, |d| {
                    d.on_click(cx.listener(|pane, _, _, cx| pane.forward(cx)))
                }),
            )
            .child(reload)
            .child(
                div()
                    .key_context("HarnessInput")
                    .on_action(cx.listener(Self::confirm))
                    .on_action(cx.listener(Self::cancel))
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .h(px(24.0))
                    .px_2()
                    .rounded_md()
                    .bg(hx.bg0)
                    .border_1()
                    .border_color(hx.border)
                    .text_xs()
                    .text_color(hx.tx0)
                    .child(self.address.clone()),
            )
            .children(self.notice.clone().map(|text| {
                div()
                    .flex_none()
                    .max_w(px(320.0))
                    .truncate()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .bg(tint(hx.amber, 0.14))
                    .text_xs()
                    .text_color(hx.amber)
                    .child(text)
            }))
            .child(
                bar_button(
                    hx,
                    "browser-open-external",
                    ::ui::IconName::ArrowUpRight,
                    "Open in default browser",
                    has_page,
                )
                .when(has_page, |d| {
                    d.on_click(cx.listener(|pane, _, _, _| {
                        if let Some(url) = pane.url() {
                            link_routing::open_external_now(url);
                        }
                    }))
                }),
            )
    }

    fn render_body(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> gpui::AnyElement {
        if let Some(failure) = &self.failure {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .gap_3()
                .px_6()
                .child(
                    div()
                        .max_w(px(560.0))
                        .text_sm()
                        .text_color(hx.tx1)
                        .child(format!("Couldn't load {}: {}", failure.url, failure.reason)),
                )
                .child(
                    div()
                        .id("browser-error-reload")
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(tint(hx.accent, 0.16))
                        .text_sm()
                        .text_color(hx.accent)
                        .cursor_pointer()
                        .hover(|d| d.bg(tint(hx.accent, 0.24)))
                        .child("Reload")
                        .on_click(cx.listener(|pane, _, window, cx| pane.reload(window, cx))),
                )
                .into_any_element();
        }
        if self.url.is_none() {
            return div()
                .flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(hx.tx2)
                .child("Type an address above, like localhost:3000")
                .into_any_element();
        }
        // The page's slot. Prepaint reads the pane (never updates it or
        // notifies, so placing the view can't trigger another frame) and
        // moves the native view onto these bounds. It is shown only after
        // its first frame lands, so it never flashes at a zero rect.
        let pane = cx.entity();
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let pane = pane.read(cx);
                        if let Some(native) = &pane.native {
                            native.view.set_frame(bounds, window);
                            native.view.set_visible(true);
                            if pane.focus_page.take() {
                                native.view.focus();
                            }
                        }
                    },
                    |_, _, _, _| {},
                )
                .size_full(),
            )
            .into_any_element()
    }
}

/// The callbacks for one native view: each only forwards to the channel.
fn page_events(tx: mpsc::UnboundedSender<PageEvent>) -> NativeEvents {
    let sender = |f: fn(String) -> PageEvent| {
        let tx = tx.clone();
        Box::new(move |s: String| {
            let _ = tx.unbounded_send(f(s));
        }) as Box<dyn Fn(String)>
    };
    let loading_tx = tx.clone();
    let failed_tx = tx.clone();
    NativeEvents {
        on_title: sender(PageEvent::Title),
        on_url: sender(PageEvent::Url),
        on_loading: Box::new(move |loading| {
            let _ = loading_tx.unbounded_send(PageEvent::Loading(loading));
        }),
        on_failed: Box::new(move |url, reason| {
            let _ = failed_tx.unbounded_send(PageEvent::Failed(url, reason));
        }),
        on_notice: sender(PageEvent::Notice),
        on_new_window: sender(PageEvent::NewWindow),
        ..NativeEvents::default()
    }
}

/// An icon button in the address bar; dimmed and inert when `enabled` is
/// false (the caller adds `on_click` only when it is true).
fn bar_button(
    hx: &HarnessTheme,
    id: &'static str,
    icon: ::ui::IconName,
    label: &'static str,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    let color = if enabled { hx.tx1 } else { tint(hx.tx2, 0.5) };
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(24.0))
        .rounded_md()
        .tooltip(tip(label))
        .child(svg().path(icon.path()).size(px(14.0)).text_color(color))
        .when(enabled, |d| {
            d.cursor_pointer().hover(|d| d.bg(hx.surface_hover))
        })
}

impl Focusable for BrowserPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BrowserPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A blank tab that just got its first address, or a retry after the
        // view could not be created.
        self.ensure_native(window, cx);
        // The error and the blank page are gpui-drawn; nothing native may
        // sit over them. The page's own slot re-shows it in prepaint.
        if (self.failure.is_some() || self.url.is_none())
            && let Some(native) = &self.native
        {
            native.view.set_visible(false);
        }
        let hx = theme(cx).clone();
        div()
            .track_focus(&self.focus_handle)
            .key_context("BrowserPane")
            .flex()
            .flex_col()
            .size_full()
            .bg(hx.bg0)
            .child(self.render_bar(&hx, cx))
            .child(self.render_body(&hx, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn web(s: &str) -> Option<Address> {
        Some(Address::Web(s.to_string()))
    }

    #[test]
    fn address_bar_normalizes_input() {
        assert_eq!(normalize_address("localhost:3000"), web("http://localhost:3000/"));
        assert_eq!(normalize_address("192.168.1.5:8080/x"), web("http://192.168.1.5:8080/x"));
        assert_eq!(normalize_address("myhost:4000"), web("http://myhost:4000/"));
        assert_eq!(normalize_address("example.com"), web("https://example.com/"));
        assert_eq!(normalize_address("https://a.b/c?d"), web("https://a.b/c?d"));
        assert_eq!(normalize_address("  "), None);
        assert_eq!(normalize_address("javascript:alert(1)"), None);
    }

    #[test]
    fn address_bar_local_hosts_default_to_http() {
        assert_eq!(normalize_address(" localhost "), web("http://localhost/"));
        assert_eq!(normalize_address("127.0.0.1/a"), web("http://127.0.0.1/a"));
        assert_eq!(normalize_address("[::1]:5173"), web("http://[::1]:5173/"));
        assert_eq!(normalize_address("printer.local"), web("http://printer.local/"));
        assert_eq!(normalize_address("app.localhost?x=1"), web("http://app.localhost/?x=1"));
        assert_eq!(normalize_address("10.0.0.5"), web("http://10.0.0.5/"));
        // Tailscale names serve https.
        assert_eq!(normalize_address("mac.tail1.ts.net"), web("https://mac.tail1.ts.net/"));
        assert_eq!(normalize_address("HTTP://LOCALHOST:3000"), web("http://localhost:3000/"));
    }

    #[test]
    fn address_bar_sends_app_schemes_out_and_refuses_local_files() {
        assert_eq!(
            normalize_address("mailto:a@b.c"),
            Some(Address::External("mailto:a@b.c".into()))
        );
        assert_eq!(
            normalize_address("vscode://file/x"),
            Some(Address::External("vscode://file/x".into()))
        );
        for refused in [
            "file:///etc/hosts",
            "FILE:///Applications/Calc.app",
            "about:blank",
            "data:text/html,hi",
            "blob:http://localhost/1",
            "https://",
            "http://exa mple.com",
        ] {
            assert_eq!(normalize_address(refused), None, "{refused}");
        }
    }

    #[test]
    fn page_label_falls_back_to_the_host() {
        assert_eq!(page_label(None, None), "Browser");
        assert_eq!(page_label(Some("http://localhost:3000/x"), None), "Browser · localhost");
        assert_eq!(page_label(Some("http://localhost:3000/x"), Some("Vite")), "Browser · Vite");
        assert_eq!(page_label(Some("not a url"), None), "Browser");
    }
}
