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
//! nothing an old view queued can reach the next one. The exception is a
//! page's new-window request, which is a tab of its own and must survive its
//! view being dropped (a tab switch right after the click): it goes inline
//! into a channel the pane owns, drained by a task that lives as long as the
//! pane ([`BrowserPane::new`]), which opens at most one tab per second for
//! it ([`new_window_allowed`]). Within one view, a
//! load failure carries the number of the navigation it belongs to, so one
//! still queued when the user starts another navigation is dropped
//! ([`failure_is_current`]).
//!
//! **Placement.** The tab body is a `canvas` whose prepaint reads the pane
//! (no update, no notify) and hands its bounds to `set_frame`, so the page
//! follows gpui layout every frame without feeding back into it.
//!
//! The pane talks to the workspace through [`BrowserEvent`]s: a changed URL
//! or title (relabel the tab, save), and a page's request for a new window.

mod webview;

use std::cell::Cell;
use std::time::{Duration, Instant};

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

pub(crate) use webview::{set_app_shortcuts, window_is_minimized_or_hidden};
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

/// Whether the native page is on screen. It sits above everything gpui
/// draws, so it shows only when nothing else should be there:
/// - `tab_active`: the workspace shows this tab (`set_shown`: the selected
///   worktree's active tab, the center drawn, the window up);
/// - `pane_visible`: the tab body is the page's slot (it has a page, not the
///   blank-tab prompt);
/// - `overlay_open`: no gpui menu, popover or dialog is open over the window
///   (`HarnessWorkspace::overlay_open`), which would otherwise draw under it;
/// - `load_failed`: the error view isn't drawn in its place.
pub(crate) fn web_view_visible(
    tab_active: bool,
    pane_visible: bool,
    overlay_open: bool,
    load_failed: bool,
) -> bool {
    tab_active && pane_visible && !overlay_open && !load_failed
}

/// Whether gpui's focus is on the page's side of its tab: in the tab
/// (`in_pane`, the pane's focus handle or anything under it) but not in its
/// address bar (`in_address`). The key monitor's gpui input
/// ([`webview::route_key`]).
fn gpui_focus_on_page(in_pane: bool, in_address: bool) -> bool {
    in_pane && !in_address
}

/// What a pending "the page takes the keyboard" request (`focus_page`) does
/// this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refocus {
    /// Give the page the keyboard; the request is done.
    Take,
    /// The request is done; the page doesn't take the keyboard.
    Drop,
    /// Leave the request as it is.
    Keep,
}

/// Whether the page re-takes the keyboard now, from the pending request and
/// where gpui's focus is (see [`gpui_focus_on_page`]). Back on the page's
/// side of the tab it takes it; in the address bar the request is dropped,
/// since the user chose to type there. Anywhere else (the tools panel after
/// ⌘⇧F, an overlay's input) the request waits: frames keep coming meanwhile
/// (a cursor blink is enough), and consuming it then would leave the page
/// without the keyboard once gpui's focus comes back by keyboard.
fn page_refocus(pending: bool, in_pane: bool, in_address: bool) -> Refocus {
    if !pending {
        Refocus::Keep
    } else if in_address {
        Refocus::Drop
    } else if gpui_focus_on_page(in_pane, in_address) {
        Refocus::Take
    } else {
        Refocus::Keep
    }
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
    /// The failing URL (when WebKit names it), the reason, and the view's
    /// navigation number at the time ([`failure_is_current`]).
    Failed(Option<String>, String, u64),
    Notice(String),
    /// The user clicked into the page.
    Clicked,
}

/// Least time between two new tabs one page opens (`target=_blank`,
/// `window.open`). WebKit already wants a user gesture for `window.open`
/// (`webview.rs`), but one gesture can run a script that opens many.
const NEW_WINDOW_INTERVAL: Duration = Duration::from_secs(1);

/// Whether a page's new-window request may open a tab, given when its last
/// one did. At most one per [`NEW_WINDOW_INTERVAL`]; the pane drops (and
/// logs) the rest.
fn new_window_allowed(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|t| now.saturating_duration_since(t) >= NEW_WINDOW_INTERVAL)
}

/// Whether a failure tagged with navigation `tagged` belongs to the view's
/// `current` navigation. One from an earlier navigation, still queued when
/// the user loaded another address (or reloaded, or went back), must not
/// replace the new page with its error.
fn failure_is_current(tagged: u64, current: u64) -> bool {
    tagged == current
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
    /// A gpui overlay is open over the window (see `set_overlay_open`).
    overlay_open: bool,
    native: Option<Native>,
    address: Entity<Editor>,
    focus_handle: FocusHandle,
    /// Hand the page keyboard focus once its view is next placed with gpui's
    /// focus on the page's side of the tab ([`page_refocus`]): after Enter
    /// or Esc in the address bar, an overlay that hid the page, or gpui's
    /// focus leaving the tab while the page had the keyboard.
    focus_page: Cell<bool>,
    /// Creating the native view failed. Latched so render doesn't retry
    /// every frame; the in-tab Retry clears it.
    view_failed: bool,
    /// Hands the page's keyboard back when gpui's focus leaves the tab.
    _focus_out: gpui::Subscription,
    /// Takes the keyboard from the page when the address bar gets gpui's
    /// focus.
    _address_focus_in: gpui::Subscription,
    /// Puts the page's URL back in the address bar when the user leaves it.
    _address_focus_out: gpui::Subscription,
    /// Where every view of this pane sends its page's new-window requests;
    /// drained by `_new_windows`, which lives as long as the pane, so a
    /// request outlives the view that made it.
    new_window_tx: mpsc::UnboundedSender<String>,
    _new_windows: Task<()>,
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
        let focus_handle = cx.focus_handle();
        // gpui's focus moving (⌘⇧F into the tools panel, say) never moves
        // AppKit's first responder; without this the page would keep the
        // keyboard, and its ⌘-keys, under the panel's search box.
        // gpui also reports a window going inactive (⌘-Tab away) as focus
        // out; the page keeps the keyboard for when the user comes back.
        // A page that had it asks for it back for when gpui's focus returns
        // to the tab without a click (Esc, the panel toggled off): see
        // `page_refocus`.
        let focus_out = cx.on_focus_out(&focus_handle, window, |pane, _, window, _| {
            if !window.is_window_active() {
                return;
            }
            if let Some(native) = &pane.native {
                native.view.set_pane_focused(false);
                if native.view.release_focus() {
                    pane.focus_page.set(true);
                }
            }
        });
        // The address bar is inside the pane's focus handle, so clicking it
        // from the page fires no focus-out, and gpui's view never takes
        // AppKit's first responder on a click: the page would keep the
        // typing, ⌘A and ⌘V meant for the address bar. The user chose the
        // address bar, so no request to give the page the keyboard back.
        let address_focus_in =
            cx.on_focus_in(&address.focus_handle(cx), window, |pane, _, _| {
                pane.focus_page.set(false);
                if let Some(native) = &pane.native {
                    native.view.set_pane_focused(false);
                    native.view.release_focus();
                }
            });
        // While the address bar has focus `sync_address` leaves the user's
        // text alone, so a page that moved meanwhile (a redirect, a
        // single-page app's `pushState`) left the bar behind. Leaving the
        // bar puts the page's URL back. Enter has already set the bar to the
        // address it loaded. A window going inactive (⌘-Tab away) also
        // reports focus out; a half-typed address stays for when the user
        // comes back.
        let address_focus_out =
            cx.on_focus_out(&address.focus_handle(cx), window, |pane, _, window, cx| {
                if window.is_window_active() {
                    pane.reset_address(window, cx);
                }
            });
        let (new_window_tx, mut new_window_rx) = mpsc::unbounded::<String>();
        let new_windows = cx.spawn_in(window, async move |this, cx| {
            let mut last_opened = None;
            while let Some(url) = new_window_rx.next().await {
                let now = Instant::now();
                if !new_window_allowed(last_opened, now) {
                    log::info!("browser: dropped a page's new window (one tab per second)");
                    continue;
                }
                last_opened = Some(now);
                if this
                    .update(cx, |_, cx| cx.emit(BrowserEvent::NewWindow(url)))
                    .is_err()
                {
                    break;
                }
            }
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
            overlay_open: false,
            native: None,
            address,
            focus_handle,
            focus_page: Cell::new(false),
            view_failed: false,
            _focus_out: focus_out,
            _address_focus_in: address_focus_in,
            _address_focus_out: address_focus_out,
            new_window_tx,
            _new_windows: new_windows,
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

    /// Whether a gpui menu, popover or dialog is open over the window. The
    /// page hides meanwhile rather than being dropped (dropping it reloads
    /// it, losing its state on every ＋ menu); the tab body shows the theme
    /// background in its place. A page holding the keyboard hands it to
    /// gpui while hidden, so the overlay's own inputs and Esc work, and gets
    /// it back once it shows with gpui's focus on the page's side of this
    /// tab ([`page_refocus`]). Idempotent, and it never notifies: the
    /// workspace calls it while rendering.
    pub(crate) fn set_overlay_open(&mut self, open: bool) {
        if self.overlay_open == open {
            return;
        }
        self.overlay_open = open;
        if open
            && let Some(native) = &self.native
            && native.view.hide()
        {
            self.focus_page.set(true);
        }
    }

    /// [`web_view_visible`] for this pane; `slot` is whether the tab body is
    /// the page's slot this frame.
    fn page_visible(&self, slot: bool) -> bool {
        web_view_visible(self.shown, slot, self.overlay_open, self.failure.is_some())
    }

    /// Create the native view if the tab is shown and has a page. Loading it
    /// is a fresh attempt, so a stale error is cleared.
    fn ensure_native(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.shown || self.native.is_some() || self.view_failed {
            return;
        }
        let Some(url) = self.url.clone() else {
            return;
        };
        let (tx, mut rx) = mpsc::unbounded();
        match NativeWebView::new(window, &url, page_events(tx, self.new_window_tx.clone())) {
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
                self.view_failed = true;
            }
        }
    }

    fn apply(&mut self, ev: PageEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Callbacks queued before the view was dropped can still be in
        // flight; none means anything without it. A stale `Loading(true)`,
        // say, would leave a Stop button on a tab with no page. (New-window
        // requests, which do, come through `new_window_tx` instead.)
        if self.native.is_none() {
            return;
        }
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
            PageEvent::Failed(url, reason, navigation) => {
                let current = self.native.as_ref().map(|n| n.view.navigation());
                if !current.is_some_and(|current| failure_is_current(navigation, current)) {
                    return;
                }
                let url = url
                    .or_else(|| self.attempted.clone())
                    .or_else(|| self.url.clone())
                    .unwrap_or_default();
                self.failure = Some(LoadFailure { url, reason });
                self.loading = false;
            }
            PageEvent::Notice(text) => self.show_notice(text, cx),
            // AppKit already gave the page the keyboard; gpui's focus follows
            // to the tab (out of the address bar or the tools panel), so
            // the key monitor routes the page's ⌘-keys to it. A pending
            // request to hand it the keyboard is already met.
            PageEvent::Clicked => {
                self.focus_page.set(false);
                window.focus(&self.focus_handle, cx);
            }
        }
        cx.notify();
    }

    /// Show the page's URL in the address bar, unless the user is typing
    /// (leaving the bar catches it up: `_address_focus_out`).
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

    /// The in-tab Retry after the web view couldn't be created: clear the
    /// latch so the next render tries again.
    fn retry_view(&mut self, cx: &mut Context<Self>) {
        self.view_failed = false;
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
        if self.view_failed {
            return error_view(
                hx,
                "Couldn't open a web view".to_string(),
                div()
                    .id("browser-view-retry")
                    .child("Retry")
                    .on_click(cx.listener(|pane, _, _, cx| pane.retry_view(cx))),
            );
        }
        if let Some(failure) = &self.failure {
            return error_view(
                hx,
                format!("Couldn't load {}: {}", failure.url, failure.reason),
                div()
                    .id("browser-error-reload")
                    .child("Reload")
                    .on_click(cx.listener(|pane, _, window, cx| pane.reload(window, cx))),
            );
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
        // its first frame lands, so it never flashes at a zero rect, and
        // only while `web_view_visible` says so.
        let pane = cx.entity();
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let pane = pane.read(cx);
                        let Some(native) = &pane.native else {
                            return;
                        };
                        let in_pane = pane.focus_handle.contains_focused(window, cx);
                        let in_address = pane.address.focus_handle(cx).contains_focused(window, cx);
                        // The key monitor's gpui input (`route_key`).
                        native
                            .view
                            .set_pane_focused(gpui_focus_on_page(in_pane, in_address));
                        if !pane.page_visible(true) {
                            native.view.set_visible(false);
                            return;
                        }
                        native.view.set_frame(bounds, window);
                        native.view.set_visible(true);
                        // Never while gpui's focus is elsewhere (an overlay
                        // may have closed by moving it, say to a tab rename)
                        // or in the address bar: the page must not take
                        // their keys.
                        match page_refocus(pane.focus_page.get(), in_pane, in_address) {
                            Refocus::Take => {
                                pane.focus_page.set(false);
                                native.view.focus();
                            }
                            Refocus::Drop => pane.focus_page.set(false),
                            Refocus::Keep => {}
                        }
                    },
                    |_, _, _, _| {},
                )
                .size_full(),
            )
            .into_any_element()
    }
}

/// The callbacks for one native view: each only forwards to the view's
/// channel, or for a new-window request to the pane's own `new_windows`.
fn page_events(
    tx: mpsc::UnboundedSender<PageEvent>,
    new_windows: mpsc::UnboundedSender<String>,
) -> NativeEvents {
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
        on_failed: Box::new(move |url, reason, navigation| {
            let _ = failed_tx.unbounded_send(PageEvent::Failed(url, reason, navigation));
        }),
        on_notice: sender(PageEvent::Notice),
        on_new_window: Box::new(move |url| {
            let _ = new_windows.unbounded_send(url);
        }),
        on_click: {
            let tx = tx.clone();
            Box::new(move || {
                let _ = tx.unbounded_send(PageEvent::Clicked);
            })
        },
        ..NativeEvents::default()
    }
}

/// A message in place of the page, with one action under it (`button`
/// brings its own id, label and click handler; this styles it).
fn error_view(
    hx: &HarnessTheme,
    message: String,
    button: gpui::Stateful<gpui::Div>,
) -> gpui::AnyElement {
    div()
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
                .child(message),
        )
        .child(
            button
                .px_3()
                .py_1()
                .rounded_md()
                .bg(tint(hx.accent, 0.16))
                .text_sm()
                .text_color(hx.accent)
                .cursor_pointer()
                .hover(|d| d.bg(tint(hx.accent, 0.24))),
        )
        .into_any_element()
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
        // The error, the blank page and overlays are gpui-drawn; nothing
        // native may sit over them. The page's own slot re-shows it in
        // prepaint.
        if !self.page_visible(self.url.is_some())
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
    fn only_the_current_navigation_s_failure_shows() {
        assert!(failure_is_current(3, 3));
        // Queued from the navigation before the user's new address.
        assert!(!failure_is_current(2, 3));
        // The counter wraps rather than overflowing.
        assert!(!failure_is_current(u64::MAX, 0));
    }

    #[test]
    fn a_new_window_request_outlives_its_view_s_channel() {
        let (tx, rx) = mpsc::unbounded();
        let (new_tx, mut new_rx) = mpsc::unbounded();
        let events = page_events(tx, new_tx);
        // The view is dropped (a tab switch): its channel and task go.
        drop(rx);
        (events.on_new_window)("http://localhost:3000/new".into());
        assert_eq!(new_rx.try_recv().ok().as_deref(), Some("http://localhost:3000/new"));
    }

    #[test]
    fn a_page_opens_at_most_one_tab_per_second() {
        let t0 = Instant::now();
        assert!(new_window_allowed(None, t0));
        // A script's burst after one click: the rest are dropped.
        assert!(!new_window_allowed(Some(t0), t0));
        assert!(!new_window_allowed(Some(t0), t0 + Duration::from_millis(999)));
        assert!(new_window_allowed(Some(t0), t0 + NEW_WINDOW_INTERVAL));
        // A clock that reads earlier than the last open never panics.
        assert!(!new_window_allowed(Some(t0 + NEW_WINDOW_INTERVAL), t0));
    }

    #[test]
    fn web_view_hidden_unless_active_visible_and_unobstructed() {
        // The whole truth table, spelled out: (tab_active, pane_visible,
        // overlay_open, load_failed) -> visible.
        let table = [
            ((false, false, false, false), false),
            ((false, false, false, true), false),
            ((false, false, true, false), false),
            ((false, false, true, true), false),
            ((false, true, false, false), false),
            ((false, true, false, true), false),
            ((false, true, true, false), false),
            ((false, true, true, true), false),
            ((true, false, false, false), false),
            ((true, false, false, true), false),
            ((true, false, true, false), false),
            ((true, false, true, true), false),
            ((true, true, false, false), true),
            ((true, true, false, true), false),
            ((true, true, true, false), false),
            ((true, true, true, true), false),
        ];
        for ((tab_active, pane_visible, overlay_open, load_failed), visible) in table {
            assert_eq!(
                web_view_visible(tab_active, pane_visible, overlay_open, load_failed),
                visible,
                "tab_active={tab_active} pane_visible={pane_visible} \
                 overlay_open={overlay_open} load_failed={load_failed}"
            );
        }
    }

    #[test]
    fn the_address_bar_is_not_the_page_side_of_the_tab() {
        // (in_pane, in_address) -> the page's ⌘-keys go to it. The address
        // bar sits inside the pane's focus handle, so `in_address` implies
        // `in_pane`; (false, true) can't happen and stays not-the-page.
        let table = [
            ((false, false), false),
            ((false, true), false),
            ((true, false), true),
            // Clicked into the address bar: its ⌘A/⌘V are gpui's.
            ((true, true), false),
        ];
        for ((in_pane, in_address), on_page) in table {
            assert_eq!(
                gpui_focus_on_page(in_pane, in_address),
                on_page,
                "in_pane={in_pane} in_address={in_address}"
            );
        }
        // What the key monitor makes of it, with the page still holding
        // AppKit's first responder: ⌘V in the address bar is the bar's.
        use webview::{KeyRoute, route_key};
        let in_address = gpui_focus_on_page(true, true);
        assert_eq!(route_key(true, in_address, true, false), KeyRoute::AsUsual);
        assert_eq!(route_key(true, gpui_focus_on_page(true, false), true, false), KeyRoute::Page);
    }

    #[test]
    fn the_page_retakes_the_keyboard_only_once_gpui_focus_is_back_on_it() {
        // (pending, in_pane, in_address) -> what the request does this frame.
        use Refocus::{Drop, Keep, Take};
        let table = [
            // Nothing asked for.
            ((false, false, false), Keep),
            ((false, false, true), Keep),
            ((false, true, false), Keep),
            ((false, true, true), Keep),
            // Focus is elsewhere (the tools panel after ⌘⇧F, an overlay's
            // input): the request waits for it to come back, frame after
            // frame.
            ((true, false, false), Keep),
            // Can't happen (the address bar is in the pane); never take.
            ((true, false, true), Drop),
            // Back in the tab (Esc, panel toggle, tab activation): the page
            // has the keyboard again with no click.
            ((true, true, false), Take),
            // Back in the address bar: it keeps the keyboard.
            ((true, true, true), Drop),
        ];
        for ((pending, in_pane, in_address), refocus) in table {
            assert_eq!(
                page_refocus(pending, in_pane, in_address),
                refocus,
                "pending={pending} in_pane={in_pane} in_address={in_address}"
            );
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
