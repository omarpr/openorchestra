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
//! load failure carries the number of the navigation that produced it
//! (`webview::Navigations`, matched by its `WKNavigation`), so one from an
//! earlier navigation, or still queued when the user or the page starts
//! another, is dropped ([`failure_is_current`]).
//!
//! **Placement.** The tab body is a `canvas` whose prepaint reads the pane
//! (no update, no notify) and hands its bounds to `set_frame`, so the page
//! follows gpui layout every frame without feeding back into it. The
//! conductor is cut out of the page where they overlap, along his outline,
//! so he shows through it and takes the pointer there
//! ([`set_punch_through`], `webview.rs`).
//!
//! The pane talks to the workspace through [`BrowserEvent`]s: a changed URL
//! (relabel the tab, save), a changed title (relabel only; the next save
//! carries it), and a page's request for a new window.
//!
//! **History.** Each pane belongs to a project ([`BrowserProject`], set by
//! the workspace) and records its committed pages into that project's
//! history ([`history`]), shared by every browser tab through one
//! [`BrowserHistory`] entity. A tab's first page view is a visit when the
//! page was typed or opened (Ports, a routed link, a new window), not when
//! it comes back (a restore, a tab switch): [`PageLoad`]. Typing in the address bar suggests pages from
//! it: a list gpui draws over the page, so while it is open the page hides
//! as it does for any other overlay ([`Self::page_visible`]).
//!
//! **Developer tools.** The address bar's Developer tools button toggles
//! WebKit's Web Inspector for the page, and right-click → Inspect Element
//! opens it on an element (private WebKit API, guarded: `webview.rs`). The
//! button is hidden where WebKit lacks that API and disabled without a page
//! view (a blank tab). Its on state is polled from WebKit
//! ([`INSPECTOR_POLL`]), since Inspect Element and the inspector's own close
//! button change it too. The inspector lives and dies with the native view:
//! hiding or closing the tab closes it and resets the button, and the tab
//! shown again does not reopen it.
//!
//! It opens docked inside the tab, at the bottom by default. Its own
//! toolbar docks it left, right or bottom, or pops it out into a window;
//! the pane remembers the last of those the user picked
//! ([`InspectorPlacement`], `prefs.json`, through [`BrowserEvent::InspectorPlaced`])
//! and moves each newly opened inspector there once ([`settle_inspector`]):
//! where it opened, and the result of the pane's own move, are never taken
//! for the user's choice.

mod history;
mod webview;

pub(crate) use history::BrowserHistory;

use std::cell::Cell;
use std::time::{Duration, Instant};

use editor::Editor;
use futures::StreamExt as _;
use futures::channel::mpsc;
use editor::EditorEvent;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton,
    ParentElement, Render, Styled, Task, Window, canvas, div, prelude::*, px, svg,
};

use crate::link_routing;
use crate::theme::{HarnessTheme, theme, tint};
use crate::workspace::{LookKind, browser_label, kind_icon, tip};

pub(crate) use webview::{
    Hole, Sprite, set_app_shortcuts, set_punch_through, window_is_minimized_or_hidden,
};
use webview::{
    DockSide, InspectorMove, InspectorPlace, NativeEvents, NativeWebView, inspector_available,
};

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
/// - With no scheme: `http://` for local and dev hosts
///   ([`link_routing::is_local_url`]: localhost, `.localhost`, `.local`,
///   loopback, unspecified (`0.0.0.0`, `[::]`) and private addresses),
///   `https://` otherwise, Tailscale `.ts.net` names included.
/// - `http`/`https` load; `file:`, `javascript:` and the page-internal
///   `about:`, `data:`, `blob:` are refused; any other scheme goes out.
/// - The unspecified address (`0.0.0.0`, `[::]`) loads through loopback,
///   since WebKit refuses it ([`link_routing::loopback_for_unspecified`]).
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
                .then(|| Address::Web(link_routing::loopback_for_unspecified(parsed.as_str()))),
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
    Some(Address::Web(link_routing::loopback_for_unspecified(
        parsed.as_str(),
    )))
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
            .and_then(|u| {
                u.host_str()
                    .map(|h| h.strip_suffix('.').unwrap_or(h).ends_with(".ts.net"))
            })
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

/// Whether the page takes the keyboard now, from the pending request and
/// where gpui's focus is (see [`gpui_focus_on_page`]). Back on the page's
/// side of the tab (or put there by activating the tab) it takes it; in the address bar the request is dropped,
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

/// A docked inspector holding AppKit's keyboard (`inspector_has_keyboard`)
/// with gpui's focus off the page's side of its tab ([`gpui_focus_on_page`]):
/// whether gpui's focus is pulled to the tab, and the page's pending
/// keyboard request after that (`pending`, the pane's `focus_page`). A pull
/// drops the request, as a click into the page does: the inspector has the
/// keyboard, and a request left pending would make the next frame's
/// [`page_refocus`] hand it to the page instead.
fn inspector_focus_pull(
    pending: bool,
    inspector_has_keyboard: bool,
    in_pane: bool,
    in_address: bool,
) -> (bool, bool) {
    if inspector_has_keyboard && !gpui_focus_on_page(in_pane, in_address) {
        (true, false)
    } else {
        (false, pending)
    }
}

/// Where the keyboard goes when a browser tab is activated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivationFocus {
    /// A blank tab: gpui's focus goes to the address bar, to type one.
    AddressBar,
    /// A tab with a page: gpui's focus goes to the pane, and the page asks
    /// for AppKit's keyboard too (`focus_page`), so typing reaches it with
    /// no click.
    Page,
}

fn activation_focus(has_page: bool) -> ActivationFocus {
    if has_page {
        ActivationFocus::Page
    } else {
        ActivationFocus::AddressBar
    }
}

/// The address bar's Developer tools button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DevToolsButton {
    /// WebKit lacks the private inspector API (`webview::inspector_available`).
    Hidden,
    /// No page view to inspect: a blank tab, or one not on screen.
    Disabled,
    /// Shows the inspector.
    Off,
    /// The inspector is open (its own window or docked); closes it.
    On,
}

/// The button from whether WebKit has the inspector API (`available`), the
/// tab has a native page view (`has_page_view`), and that page's inspector is
/// open (`inspector_open`, polled from WebKit: the button, Inspect Element
/// and the inspector's own close button all change it).
fn devtools_button(available: bool, has_page_view: bool, inspector_open: bool) -> DevToolsButton {
    match (available, has_page_view, inspector_open) {
        (false, _, _) => DevToolsButton::Hidden,
        (true, false, _) => DevToolsButton::Disabled,
        (true, true, false) => DevToolsButton::Off,
        (true, true, true) => DevToolsButton::On,
    }
}

/// How often a page's inspector is asked whether it is open, for the
/// button's state: Inspect Element and the inspector window's close button
/// change it without telling the pane.
const INSPECTOR_POLL: Duration = Duration::from_millis(500);

/// Where the user keeps a page's Web Inspector: docked in the tab on one
/// side, or in its own window. Remembered in `prefs.json`
/// (`inspector_placement`) and mirrored here ([`set_inspector_placement`]),
/// so each tab reopens it there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum InspectorPlacement {
    #[default]
    Bottom,
    Left,
    Right,
    Window,
}

impl InspectorPlacement {
    fn index(self) -> u8 {
        match self {
            InspectorPlacement::Bottom => 0,
            InspectorPlacement::Left => 1,
            InspectorPlacement::Right => 2,
            InspectorPlacement::Window => 3,
        }
    }

    fn from_index(i: u8) -> Self {
        match i {
            1 => InspectorPlacement::Left,
            2 => InspectorPlacement::Right,
            3 => InspectorPlacement::Window,
            _ => InspectorPlacement::Bottom,
        }
    }

    /// Reads `prefs.json`'s value leniently: a placement this build doesn't
    /// know (a newer build's) is the default, rather than failing the whole
    /// prefs file.
    pub(crate) fn deserialize_lenient<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::Deserialize as _;
        Ok(serde_json::Value::deserialize(d)
            .ok()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default())
    }
}

static INSPECTOR_PLACEMENT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Mirror of the `inspector_placement` pref, readable by every pane without
/// threading the workspace through. Set when prefs load and whenever a
/// pane sees the user move an inspector.
pub(crate) fn set_inspector_placement(placement: InspectorPlacement) {
    INSPECTOR_PLACEMENT.store(placement.index(), std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn inspector_placement() -> InspectorPlacement {
    InspectorPlacement::from_index(INSPECTOR_PLACEMENT.load(std::sync::atomic::Ordering::Relaxed))
}

/// The placement an inspector at `place` is in, if it can be told.
fn placement_of(place: InspectorPlace) -> Option<InspectorPlacement> {
    match place {
        InspectorPlace::Docked(Some(DockSide::Bottom)) => Some(InspectorPlacement::Bottom),
        InspectorPlace::Docked(Some(DockSide::Left)) => Some(InspectorPlacement::Left),
        InspectorPlace::Docked(Some(DockSide::Right)) => Some(InspectorPlacement::Right),
        InspectorPlace::OwnWindow => Some(InspectorPlacement::Window),
        InspectorPlace::Unsupported
        | InspectorPlace::NotPlaced
        | InspectorPlace::Docked(None)
        | InspectorPlace::DockedElsewhere => None,
    }
}

/// Logs, once per process, that WebKit docked an inspector outside the
/// page's container (the pane then moves it to its own window).
fn log_docked_elsewhere() {
    static LOGGED: std::sync::Once = std::sync::Once::new();
    LOGGED.call_once(|| {
        log::warn!("browser: WebKit docked the inspector outside the tab; moving it to its own window");
    });
}

/// How the pane moves an inspector to `to`.
fn move_for(to: InspectorPlacement) -> InspectorMove {
    match to {
        InspectorPlacement::Bottom => InspectorMove::AttachBottom,
        InspectorPlacement::Left => InspectorMove::Dock(DockSide::Left),
        InspectorPlacement::Right => InspectorMove::Dock(DockSide::Right),
        InspectorPlacement::Window => InspectorMove::Detach,
    }
}

/// Where the pane is with an open inspector ([`settle_inspector`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InspectorWatch {
    /// No inspector open.
    Closed,
    /// It just opened; waiting to see where WebKit put it.
    Opening,
    /// The pane moved it to the user's placement; the next placement seen
    /// is the result, not the user's doing. The count is how many more
    /// times a left/right dock is asked for while it hasn't landed
    /// ([`DOCK_RETRIES`]); 0 for every other move.
    Moved(u8),
    /// Settled at this placement (`None`: unknown). A different one seen
    /// from now on is the user's doing, from the inspector's own toolbar.
    Watching(Option<InspectorPlacement>),
}

/// How many more polls ask the inspector's page again to dock left or
/// right while it hasn't: the request is a script in that page, which does
/// nothing if it runs before the page's `InspectorFrontendHost` is ready.
/// `attach`/`detach` act at once, so they are never repeated.
const DOCK_RETRIES: u8 = 3;

/// The [`InspectorWatch::Moved`] state after sending `to`.
fn moved_after(to: InspectorMove) -> InspectorWatch {
    match to {
        InspectorMove::Dock(_) => InspectorWatch::Moved(DOCK_RETRIES),
        InspectorMove::AttachBottom | InspectorMove::Detach => InspectorWatch::Moved(0),
    }
}

/// What the pane does on a poll that sees the open inspector at `place`,
/// with the user's placement `want`: the move to make (once per open, but
/// a left/right dock is asked again up to [`DOCK_RETRIES`] times while it
/// hasn't landed), the next state, and a placement to remember (only one
/// the user chose: never where it opened, nor the result of our own move).
fn settle_inspector(
    watch: InspectorWatch,
    want: InspectorPlacement,
    place: InspectorPlace,
) -> (Option<InspectorMove>, InspectorWatch, Option<InspectorPlacement>) {
    use InspectorWatch::*;
    let seen = placement_of(place);
    match (watch, place, seen) {
        (Closed, _, _) => (None, Closed, None),
        // WebKit can't say where it is: leave it alone for good.
        (_, InspectorPlace::Unsupported, _) => (None, Watching(None), None),
        // Docked somewhere other than beside the page (never expected):
        // its own window, once.
        (Opening, InspectorPlace::DockedElsewhere, _) => (Some(InspectorMove::Detach), Moved(0), None),
        (Moved(_), InspectorPlace::DockedElsewhere, _) => (None, Watching(None), None),
        // Not in a window yet, or mid-move: ask again next time.
        (Opening | Moved(_), _, None) => (None, watch, None),
        (Opening | Moved(_), _, Some(p)) if p == want => (None, Watching(Some(p)), None),
        // Elsewhere: moved. WebKit may refuse (a page too small to dock
        // beside); whatever comes of it is where it stays.
        (Opening, _, Some(_)) => {
            let to = move_for(want);
            (Some(to), moved_after(to), None)
        }
        // A left/right dock that hasn't landed: asked again, a few times.
        (Moved(left), _, Some(_)) if left > 0 => (Some(move_for(want)), Moved(left - 1), None),
        (Moved(_), _, Some(p)) => (None, Watching(Some(p)), None),
        // No baseline yet (it was never readable): this is one, not a move.
        (Watching(None), _, Some(p)) => (None, Watching(Some(p)), None),
        (Watching(Some(last)), _, Some(p)) if last != p => (None, Watching(Some(p)), Some(p)),
        (Watching(_), _, _) => (None, watch, None),
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
#[derive(Debug)]
pub(crate) enum BrowserEvent {
    /// The URL changed: relabel the tab and save.
    UrlChanged,
    /// Only the title changed: relabel the tab. No save of its own: a page
    /// can animate its title forever, and the next save (a URL change, an
    /// open, close or quit) reads the current title anyway.
    TitleChanged,
    /// The page asked for a new window (`target=_blank`, `window.open`).
    NewWindow(String),
    /// The user moved the page's Web Inspector (its toolbar's dock
    /// buttons): remember it in `prefs.json`.
    InspectorPlaced(InspectorPlacement),
}

impl BrowserEvent {
    /// Whether the workspace saves its tabs for this event.
    pub(crate) fn saves(&self) -> bool {
        matches!(self, BrowserEvent::UrlChanged)
    }
}

impl EventEmitter<BrowserEvent> for BrowserPane {}

/// One `NativeEvents` callback, carried over the channel.
enum PageEvent {
    Title(String),
    Url(String),
    /// The page is now at this URL for real (`NativeEvents::on_commit`);
    /// `true` for a change within the same document.
    Committed(String, bool),
    Loading(bool),
    /// The failing URL (when WebKit names it), the reason, and the number
    /// of the navigation that failed ([`failure_is_current`]).
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
/// the user loaded another address (or reloaded, or went back) or the page
/// navigated on its own, must not replace the new page with its error.
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
    /// Asks the page's inspector whether it is open, every
    /// [`INSPECTOR_POLL`] (`BrowserPane::poll_inspector`); none where WebKit
    /// lacks the API.
    _inspector_poll: Option<Task<()>>,
}

/// How long a notice (e.g. an unsupported download) stays in the bar.
const NOTICE_TTL: Duration = Duration::from_secs(5);

/// The project (repository) a browser tab belongs to, whose history it
/// records into and suggests from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BrowserProject {
    /// The history's key: the main checkout's path.
    pub key: String,
    /// The project's display name, for "Recent in …".
    pub name: String,
}

/// A server running in the selected task, as a blank tab lists it: the
/// Ports panel's row for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunningServer {
    pub port: u16,
    /// The Ports row's text after its port ("Python · localhost only").
    pub detail: String,
    /// What Ports' Open would load.
    pub url: String,
}

/// The title a commit gives its history entry, from the title the pane
/// holds. Only within the same document (`pushState`, a fragment), where it
/// is the page's own and may have changed before the URL did; a new
/// document's title still in hand can be the previous page's, and its own
/// title event names it. The caller records it against the committed URL,
/// not the pane's, which may lag.
fn commit_title(same_document: bool, title: Option<&str>) -> Option<&str> {
    title.filter(|_| same_document)
}

/// A title event: store `arrived` as the tab's title (`held`; blank is
/// none) and say whether the tab relabels (it changed) and which title
/// history gets. History gets every non-blank title, changed or not: a
/// restored tab starts with its saved title, and many sites share one
/// title across pages, so a "changed only" rule would leave their entries
/// untitled (`History::set_title` skips one it already has).
fn take_title(held: &mut Option<String>, arrived: String) -> (bool, Option<String>) {
    let arrived = Some(arrived).filter(|t| !t.trim().is_empty());
    let relabel = *held != arrived;
    *held = arrived.clone();
    (relabel, arrived)
}

/// Whether a native view's first commit (`committed`) is only the page it
/// was made to bring back (`reshow`: a tab shown again, or restored),
/// which reloads it; that isn't a visit. A redirect elsewhere, or a view
/// made for an address the user just entered or the app just opened
/// (`reshow` is `None`, [`reshow_page`]), is.
fn reshow_commit(reshow: Option<&str>, committed: &str) -> bool {
    reshow.is_some_and(|page| {
        history::recordable(committed).is_some_and(|c| history::recordable(page) == Some(c))
    })
}

/// Why a tab's next native view loads its page, which decides whether the
/// view's first commit is a visit ([`reshow_page`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PageLoad {
    /// An address the user entered (typed, a suggestion, a blank tab's
    /// row) or retried, before the tab had a view.
    Typed,
    /// A tab made to load its URL now: a routed link, Ports' Open, a
    /// page's new window (`open_browser_tab_in`).
    Opened,
    /// A tab restored from `browser-tabs.json` at launch: the page it was
    /// on, already recorded when it was visited.
    Restored,
    /// The tab shown again after a tab switch (hiding dropped its view).
    Reshown,
}

/// What the view being made now loads for (`next`), leaving `Reshown` for
/// every later view: only the first view of a typed or opened page is new.
fn take_load(next: &mut PageLoad) -> PageLoad {
    std::mem::replace(next, PageLoad::Reshown)
}

/// The page a view made to load `url` for `load` brings back, whose first
/// commit isn't a visit ([`reshow_commit`]): `None` for a page the user or
/// the app just asked for.
fn reshow_page(load: PageLoad, url: &str) -> Option<String> {
    matches!(load, PageLoad::Restored | PageLoad::Reshown).then(|| url.to_string())
}

/// How many recent pages a blank tab lists.
const RECENT_ON_BLANK: usize = 8;

/// The address bar's suggestion list: pages from the project's history
/// matching what was typed.
struct Suggestions {
    items: Vec<history::HistoryEntry>,
    /// The row ↑/↓ moved to; Enter loads it. `None` until the first ↑/↓,
    /// so Enter loads what was typed.
    highlight: Option<usize>,
}

/// The highlighted row after ↑ (`down` false) or ↓ in a list of `len`,
/// from `current`. ↓ from nothing takes the first row and ↑ the last;
/// moving past either end goes back to no row, where Enter loads what was
/// typed.
fn move_highlight(current: Option<usize>, len: usize, down: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match (current, down) {
        (None, true) => Some(0),
        (None, false) => Some(len - 1),
        (Some(i), true) => (i + 1 < len).then_some(i + 1),
        (Some(i), false) => i.checked_sub(1),
    }
}

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
    /// focus on the page's side of the tab ([`page_refocus`]): after the
    /// tab is activated ([`Self::activate_focus`]), Enter or Esc in the
    /// address bar, an overlay that hid the page, or gpui's focus leaving
    /// the tab while the page had the keyboard.
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
    /// Every project's browsing history (shared by all panes).
    history: Entity<BrowserHistory>,
    /// The project this tab records into; `None` until the workspace knows
    /// it (`set_project`).
    project: Option<BrowserProject>,
    /// The address bar's suggestion list, while open.
    suggestions: Option<Suggestions>,
    /// The page a newly made native view is bringing back (the tab shown
    /// again, or restored), whose first commit is not a visit
    /// ([`reshow_commit`]). `None` when the view loads a page the user just
    /// asked for.
    reshow: Option<String>,
    /// Why the next native view loads the page ([`take_load`]): a visit
    /// for a page just typed or opened, not for one coming back.
    next_load: PageLoad,
    /// What a blank tab lists under "Running in this task" (`set_running`).
    running: Vec<RunningServer>,
    /// The page's Web Inspector is open, as last polled
    /// (`poll_inspector`). Reset when the view is dropped, which closes it.
    inspector_open: bool,
    /// Where the open inspector is in settling to the user's placement
    /// ([`settle_inspector`]).
    inspector_watch: InspectorWatch,
    /// Opens and refreshes the suggestions as the user types.
    _address_edits: gpui::Subscription,
}

impl BrowserPane {
    /// A tab on `url` (`None`: blank), whose first page view loads for
    /// `load` (a visit or not; a blank tab's comes from `navigate`).
    pub(crate) fn new(
        url: Option<String>,
        title: Option<String>,
        load: PageLoad,
        history: Entity<BrowserHistory>,
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
                    pane.close_suggestions(cx);
                    pane.reset_address(window, cx);
                }
            });
        let address_edits = cx.subscribe_in(
            &address,
            window,
            |pane, _, event: &EditorEvent, window, cx| {
                if matches!(event, EditorEvent::BufferEdited) {
                    pane.update_suggestions(window, cx);
                }
            },
        );
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
            history,
            project: None,
            suggestions: None,
            reshow: None,
            next_load: load,
            running: Vec::new(),
            inspector_open: false,
            inspector_watch: InspectorWatch::Closed,
            _address_edits: address_edits,
        }
    }

    /// The project this tab belongs to (the workspace works it out from the
    /// tab's pane). Loads that project's history if it isn't yet.
    /// Idempotent, and it never notifies: the workspace calls it while
    /// rendering.
    pub(crate) fn set_project(&mut self, project: Option<BrowserProject>, cx: &mut Context<Self>) {
        if self.project == project {
            return;
        }
        if let Some(p) = &project {
            self.history.update(cx, |h, _| h.ensure_loaded(&p.key));
        }
        self.project = project;
    }

    /// Whether the tab has no page yet: the blank tab, which lists running
    /// servers and recent pages.
    pub(crate) fn is_blank(&self) -> bool {
        self.url.is_none()
    }

    /// The selected task's running servers, for the blank tab's list
    /// (empty hides it). Compare-and-set, and it never notifies: the
    /// workspace calls it while rendering, before this pane draws.
    pub(crate) fn set_running(&mut self, running: Vec<RunningServer>) {
        if self.running != running {
            self.running = running;
        }
    }

    /// The page committed `url`: a visit to record, unless it is already
    /// the project's latest page (a reload, or this tab's view coming back
    /// on a tab switch, which loads the page again).
    fn record_visit(&mut self, url: &str, cx: &mut Context<Self>) {
        let Some(project) = &self.project else {
            return;
        };
        let key = project.key.clone();
        self.history.update(cx, |h, cx| h.record(&key, url, cx));
    }

    /// `title` names the history entry of `url`.
    fn record_title(&mut self, url: &str, title: &str, cx: &mut Context<Self>) {
        let Some(project) = &self.project else {
            return;
        };
        let key = project.key.clone();
        self.history
            .update(cx, |h, cx| h.set_title(&key, url, title, cx));
    }

    /// The user typed in the address bar: show the history pages matching
    /// it, or close the list when nothing does. Our own `set_text` (the
    /// page's URL put back, an address loaded) edits the bar too; those
    /// leave it equal to the page's URL, or out of focus, and open nothing.
    fn update_suggestions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.address.read(cx).text(cx);
        let typing = self.address.focus_handle(cx).is_focused(window)
            && text.trim() != self.url.as_deref().unwrap_or_default();
        let items: Vec<history::HistoryEntry> = match (&self.project, typing) {
            (Some(project), true) => self
                .history
                .read(cx)
                .get(&project.key)
                .map(|h| {
                    h.suggest(&text, history::unix_now(), history::MAX_SUGGESTIONS)
                        .into_iter()
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        self.suggestions = (!items.is_empty()).then_some(Suggestions {
            items,
            highlight: None,
        });
        cx.notify();
    }

    fn close_suggestions(&mut self, cx: &mut Context<Self>) {
        if self.suggestions.take().is_some() {
            cx.notify();
        }
    }

    /// ↑/↓ in the address bar move through the suggestions; with none open
    /// they do what they did before (nothing, in a one-line field).
    fn select_suggestion(&mut self, down: bool, cx: &mut Context<Self>) {
        match &mut self.suggestions {
            Some(s) => {
                s.highlight = move_highlight(s.highlight, s.items.len(), down);
                cx.notify();
            }
            None => cx.propagate(),
        }
    }

    /// Load `url` (absolute, http(s)) in this tab as if typed: the bar shows
    /// it, the list closes and the page takes the keyboard once shown.
    fn load_here(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        self.suggestions = None;
        self.address
            .update(cx, |e, cx| e.set_text(url.as_str(), window, cx));
        self.navigate(url, cx);
        window.focus(&self.focus_handle, cx);
        self.focus_page.set(true);
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

    /// Where gpui's focus goes when the tab is activated (a tab click,
    /// ⌘1..9, a routed or Ports open, the restored active tab: all through
    /// `HarnessWorkspace::focus_active_pane`): the address bar of a blank
    /// tab; otherwise the pane, and the page asks for the keyboard, taken
    /// once its view is placed with gpui's focus still there
    /// ([`page_refocus`]).
    pub(crate) fn activate_focus(&self, cx: &App) -> FocusHandle {
        match activation_focus(self.url.is_some()) {
            ActivationFocus::AddressBar => self.address.focus_handle(cx),
            ActivationFocus::Page => {
                self.focus_page.set(true);
                self.focus_handle.clone()
            }
        }
    }

    /// Load `url` (absolute). Before the view exists it is loaded when the
    /// tab is next shown.
    pub(crate) fn navigate(&mut self, url: String, cx: &mut Context<Self>) {
        self.failure = None;
        self.attempted = Some(url.clone());
        if self.url.as_deref() != Some(url.as_str()) {
            self.url = Some(url.clone());
            cx.emit(BrowserEvent::UrlChanged);
        }
        match &self.native {
            Some(native) => native.view.load(&url),
            None => self.next_load = PageLoad::Typed,
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
            // Dropping the view closes its inspector first (`webview.rs`);
            // shown again, the page reloads with it closed.
            self.native = None;
            self.inspector_open = false;
            self.inspector_watch = InspectorWatch::Closed;
            self.loading = false;
            self.suggestions = None;
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
    /// the page's slot this frame. The address bar's suggestion list counts
    /// as an overlay: gpui draws it over the page.
    fn page_visible(&self, slot: bool) -> bool {
        web_view_visible(
            self.shown,
            slot,
            self.overlay_open || self.suggestions.is_some(),
            self.failure.is_some(),
        )
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
                let inspector_poll = inspector_available().then(|| {
                    cx.spawn_in(window, async move |this, cx| {
                        loop {
                            cx.background_executor().timer(INSPECTOR_POLL).await;
                            if this
                                .update_in(cx, |pane, window, cx| pane.poll_inspector(window, cx))
                                .is_err()
                            {
                                break;
                            }
                        }
                    })
                });
                self.failure = None;
                self.reshow = reshow_page(take_load(&mut self.next_load), &url);
                self.attempted = Some(url);
                self.inspector_open = false;
                self.inspector_watch = InspectorWatch::Closed;
                self.native = Some(Native {
                    view,
                    _events: events,
                    _inspector_poll: inspector_poll,
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
                let (relabel, record) = take_title(&mut self.title, title);
                if relabel {
                    cx.emit(BrowserEvent::TitleChanged);
                }
                if let (Some(url), Some(title)) = (self.url.clone(), record) {
                    self.record_title(&url, &title, cx);
                }
            }
            PageEvent::Url(url) => {
                if self.url.as_deref() != Some(url.as_str()) {
                    self.url = Some(url);
                    cx.emit(BrowserEvent::UrlChanged);
                }
                self.sync_address(window, cx);
            }
            PageEvent::Committed(url, same_document) => {
                if !reshow_commit(self.reshow.take().as_deref(), &url) {
                    self.record_visit(&url, cx);
                }
                if let Some(title) = commit_title(same_document, self.title.as_deref()) {
                    let title = title.to_string();
                    // The committed URL, never `self.url`, which may lag.
                    self.record_title(&url, &title, cx);
                }
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

    /// Enter in the address bar: the highlighted suggestion, else what was
    /// typed.
    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(url) = self
            .suggestions
            .take()
            .and_then(|s| s.highlight.and_then(|i| s.items.into_iter().nth(i)))
            .map(|e| e.url)
        {
            self.load_here(url, window, cx);
            return;
        }
        let typed = self.address.read(cx).text(cx);
        match normalize_address(&typed) {
            Some(Address::Web(url)) => self.load_here(url, window, cx),
            Some(Address::External(url)) => {
                link_routing::open_external_now(&url);
                self.reset_address(window, cx);
            }
            None if typed.trim().is_empty() => {}
            None => self.show_notice(format!("Can't open “{}” here", typed.trim()), cx),
        }
    }

    /// Esc in the address bar: close the suggestions; with none open, put
    /// the page's URL back and return to the page.
    fn cancel(&mut self, _: &menu::Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if self.suggestions.take().is_some() {
            cx.notify();
            return;
        }
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
                None => {
                    // A retry of a page that never loaded: a visit if it does.
                    self.next_load = PageLoad::Typed;
                    self.ensure_native(window, cx);
                }
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

    /// The Developer tools button: show the page's Web Inspector, or close
    /// it if open. The state is read back from WebKit right away; showing
    /// may land a moment later, which the next poll picks up. Opening puts
    /// gpui's focus on the page's side of the tab, so a docked inspector's
    /// ⌘-chords reach it (the key monitor's routing in `webview.rs`); closing
    /// one that had the keyboard hands it to the page.
    fn toggle_devtools(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(native) = &self.native {
            let opening = !self.inspector_open;
            if native.view.toggle_inspector() {
                self.focus_page.set(true);
            }
            if opening {
                // The inspector is about to take the keyboard (WebKit makes
                // it first responder when it docks): a pending request for
                // the page must not hand it to the page on the next frame
                // ([`inspector_focus_pull`]).
                self.focus_page.set(false);
                window.focus(&self.focus_handle, cx);
            }
        }
        self.poll_inspector(window, cx);
    }

    /// Catch up with the page's inspector: the button, Inspect Element, the
    /// inspector's own close button and its dock buttons all change it.
    /// One that just opened is moved once to the user's placement, and a
    /// move the user makes afterwards is remembered ([`settle_inspector`]).
    /// Notifies only on a change.
    ///
    /// A docked inspector that took the keyboard without a click (WebKit
    /// makes it first responder when it docks: Inspect Element, our own
    /// move) while gpui's focus is off the page's side of this tab pulls
    /// gpui's focus here, as a click in it would: its ⌘-chords then reach
    /// it, and leaving the tab takes its keyboard back.
    fn poll_inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(native) = &self.native else {
            return;
        };
        let open = native.view.inspector_visible();
        let changed = open != self.inspector_open;
        if changed {
            self.inspector_open = open;
            self.inspector_watch = if open { InspectorWatch::Opening } else { InspectorWatch::Closed };
            cx.notify();
        }
        if open {
            let place = native.view.inspector_place();
            if place == InspectorPlace::DockedElsewhere {
                log_docked_elsewhere();
            }
            let (to, next, chosen) = settle_inspector(self.inspector_watch, inspector_placement(), place);
            if let Some(to) = to {
                native.view.move_inspector(to);
            }
            self.inspector_watch = next;
            if let Some(chosen) = chosen {
                set_inspector_placement(chosen);
                cx.emit(BrowserEvent::InspectorPlaced(chosen));
            }
        }
        if open || changed {
            // Popped out or closed while it had the keyboard: AppKit leaves
            // it with the window; gpui's view gets it back.
            native.view.recover_focus();
        }
        if open {
            let in_pane = self.focus_handle.contains_focused(window, cx);
            let in_address = self.address.focus_handle(cx).contains_focused(window, cx);
            let (pull, pending) = inspector_focus_pull(
                self.focus_page.get(),
                native.view.inspector_has_focus(),
                in_pane,
                in_address,
            );
            self.focus_page.set(pending);
            if pull {
                window.focus(&self.focus_handle, cx);
            }
        }
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
                    // A one-line editor passes ↑/↓ on.
                    .on_action(cx.listener(|pane, _: &zed_actions::editor::MoveUp, _, cx| {
                        pane.select_suggestion(false, cx)
                    }))
                    .on_action(cx.listener(|pane, _: &zed_actions::editor::MoveDown, _, cx| {
                        pane.select_suggestion(true, cx)
                    }))
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
            .children(match devtools_button(
                inspector_available(),
                native.is_some(),
                self.inspector_open,
            ) {
                DevToolsButton::Hidden => None,
                state => {
                    let enabled = state != DevToolsButton::Disabled;
                    Some(
                        bar_toggle(
                            hx,
                            "browser-devtools",
                            ::ui::IconName::Code,
                            "Developer tools",
                            enabled,
                            state == DevToolsButton::On,
                        )
                        .when(enabled, |d| {
                            d.on_click(cx.listener(|pane, _, window, cx| pane.toggle_devtools(window, cx)))
                        }),
                    )
                }
            })
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

    /// The suggestion list under the address bar, over the page (hidden
    /// meanwhile: `page_visible`). Rows act on mouse-down: a click elsewhere
    /// takes gpui's focus out of the address bar, whose focus-out closes the
    /// list before a mouse-up could land.
    fn render_suggestions(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let suggestions = self.suggestions.as_ref()?;
        let mut list = div()
            .id("browser-suggestions")
            // Its padding and gaps must not pass presses (or hover) to the
            // blank tab's rows beneath.
            .occlude()
            .absolute()
            .top(px(32.0))
            // Under the address field: past back, forward and reload, and
            // short of Open in default browser (and Developer tools, when
            // shown: 24pt and a 4pt gap more).
            .left(px(92.0))
            .right(px(if inspector_available() { 64.0 } else { 36.0 }))
            .max_w(px(640.0))
            .flex()
            .flex_col()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(hx.border)
            .bg(hx.bg1)
            .shadow_md();
        for (i, entry) in suggestions.items.iter().enumerate() {
            let highlighted = suggestions.highlight == Some(i);
            let url = entry.url.clone();
            let title = if entry.title.is_empty() {
                history::display_url(&entry.url).to_string()
            } else {
                entry.title.clone()
            };
            list = list.child(
                div()
                    .id(("browser-suggestion", i))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_2()
                    .py_1()
                    .mx_1()
                    .rounded_md()
                    .cursor_pointer()
                    .when(highlighted, |d| d.bg(hx.surface_hover))
                    .hover(|d| d.bg(hx.surface_hover))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |pane, _, window, cx| {
                            cx.stop_propagation();
                            pane.load_here(url.clone(), window, cx);
                        }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .max_w(px(260.0))
                            .truncate()
                            .text_xs()
                            .text_color(hx.tx0)
                            .child(title),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(hx.tx2)
                            .child(history::display_url(&entry.url).to_string()),
                    ),
            );
        }
        Some(list.into_any_element())
    }

    /// The blank tab: what to type, then the servers running in the task
    /// and the project's recent pages, each a click away. A list with
    /// nothing in it isn't shown.
    fn render_blank(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> gpui::AnyElement {
        // Centred by auto margins, not `justify_center`: in a scroll
        // container that is shorter than the block, centring would push its
        // top out of reach; auto margins collapse to zero and it scrolls
        // from the top.
        let mut block = div()
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .max_w(px(520.0))
            .mx_auto()
            .my_auto()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .child(kind_icon(hx, LookKind::Browser, 40.0))
                    .child(div().text_sm().text_color(hx.tx1).child("Open a page"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(hx.tx2)
                            .child("Type an address above, or pick one below."),
                    ),
            );
        if !self.running.is_empty() {
            block = block.child(blank_section_label(hx, "RUNNING IN THIS TASK".into()));
            for (i, server) in self.running.iter().enumerate() {
                let url = server.url.clone();
                block = block.child(
                    blank_row(hx, ("browser-blank-running", i))
                        .on_click(cx.listener(move |pane, _, window, cx| {
                            if let Some(Address::Web(url)) = normalize_address(&url) {
                                pane.load_here(url, window, cx);
                            }
                        }))
                        .child(
                            div()
                                .flex_none()
                                .w(px(56.0))
                                .font_family(hx.font_mono)
                                .text_sm()
                                .text_color(hx.tx0)
                                .child(format!(":{}", server.port)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(hx.tx2)
                                .child(server.detail.clone()),
                        ),
                );
            }
        }
        let recent = self.project.as_ref().and_then(|project| {
            let history = self.history.read(cx).get(&project.key)?;
            let recent = history.recent(RECENT_ON_BLANK);
            (!recent.is_empty()).then(|| (project.name.to_uppercase(), recent.to_vec()))
        });
        if let Some((name, recent)) = recent {
            let now = history::unix_now();
            block = block.child(blank_section_label(hx, format!("RECENT IN {name}")));
            for (i, entry) in recent.into_iter().enumerate() {
                let shown_url = history::display_url(&entry.url).to_string();
                let title = if entry.title.is_empty() {
                    shown_url.clone()
                } else {
                    entry.title.clone()
                };
                let when = history::relative_time(entry.last_visited, now);
                let url = entry.url;
                block = block.child(
                    blank_row(hx, ("browser-blank-recent", i))
                        .tooltip(tip(url.clone()))
                        .on_click(cx.listener(move |pane, _, window, cx| {
                            pane.load_here(url.clone(), window, cx);
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(hx.tx0)
                                .child(title),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(hx.tx2)
                                .child(shown_url),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(hx.tx2)
                                .child(when),
                        ),
                );
            }
        }
        div()
            .id("browser-blank")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .px_4()
            .py_6()
            .overflow_y_scroll()
            .child(block)
            .into_any_element()
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
            return self.render_blank(hx, cx);
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
        on_commit: {
            let tx = tx.clone();
            Box::new(move |url, same_document| {
                let _ = tx.unbounded_send(PageEvent::Committed(url, same_document));
            })
        },
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

/// A small upper-case label over one of the blank tab's lists, styled like
/// the app's section headers.
fn blank_section_label(hx: &HarnessTheme, label: String) -> gpui::Div {
    div()
        .pt_5()
        .pb_1()
        .px_2()
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(hx.tx2)
        .truncate()
        .child(label)
}

/// One clickable row in a blank tab's list (the caller adds its cells and
/// click).
fn blank_row(hx: &HarnessTheme, id: impl Into<gpui::ElementId>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_3()
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .hover(|d| d.bg(hx.surface))
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
    bar_toggle(hx, id, icon, label, enabled, false)
}

/// [`bar_button`] for a toggle: while `on`, it shows the app's selected
/// state (an accent tint behind an accent icon).
fn bar_toggle(
    hx: &HarnessTheme,
    id: &'static str,
    icon: ::ui::IconName,
    label: &'static str,
    enabled: bool,
    on: bool,
) -> gpui::Stateful<gpui::Div> {
    let color = match (enabled, on) {
        (false, _) => tint(hx.tx2, 0.5),
        (true, true) => hx.accent,
        (true, false) => hx.tx1,
    };
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
        .when(on, |d| d.bg(tint(hx.accent, 0.16)))
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
            .relative()
            .flex_col()
            .size_full()
            .bg(hx.bg0)
            .child(self.render_bar(&hx, cx))
            .child(self.render_body(&hx, cx))
            .children(self.render_suggestions(&hx, cx))
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
        // What dev servers bound to every interface print: WebKit refuses
        // all-zero hosts, so they load through loopback.
        assert_eq!(normalize_address("0.0.0.0:3000"), web("http://127.0.0.1:3000/"));
        assert_eq!(normalize_address("0.0.0.0"), web("http://127.0.0.1/"));
        assert_eq!(normalize_address("[::]:5173"), web("http://[::1]:5173/"));
        assert_eq!(normalize_address("[::]"), web("http://[::1]/"));
        assert_eq!(
            normalize_address("http://0.0.0.0:3000/a?b#c"),
            web("http://127.0.0.1:3000/a?b#c")
        );
        assert_eq!(normalize_address("[fd7a:115c:a1e0::1]"), web("http://[fd7a:115c:a1e0::1]/"));
        assert_eq!(normalize_address("[::ffff:127.0.0.1]"), web("http://[::ffff:7f00:1]/"));
        assert_eq!(normalize_address("localhost."), web("http://localhost./"));
        assert_eq!(normalize_address("mac.tail1.ts.net."), web("https://mac.tail1.ts.net./"));
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
    fn a_failure_shows_only_while_its_navigation_is_the_latest() {
        let (a, b, c) = (0xa0, 0xb0, 0xc0);
        let shown = |navs: &webview::Navigations, tag: Option<u64>| {
            tag.is_some_and(|tag| failure_is_current(tag, navs.current()))
        };

        // The user loads B while A is loading; A fails late.
        let mut navs = webview::Navigations::default();
        navs.begin(Some(a));
        navs.begin(Some(b));
        assert!(!shown(&navs, navs.failure(Some(a))));
        // WebKit reports B starting (again) and committing: still B's.
        assert!(!navs.started(Some(b)));
        let b_failed = navs.failure(Some(b));
        assert!(shown(&navs, b_failed));

        // A fails after B started, B having given no WKNavigation to the
        // pane (or started before the pane's call returned).
        let mut navs = webview::Navigations::default();
        navs.begin(Some(a));
        navs.begin(None);
        assert!(!shown(&navs, navs.failure(Some(a))));
        assert!(navs.started(Some(b)));
        assert!(!shown(&navs, navs.failure(Some(a))));
        assert!(shown(&navs, navs.failure(Some(b))));

        // A page-initiated C (a link, a script) supersedes A: A's failure
        // reported after C started is dropped at once, and one queued
        // before C started is dropped when it arrives.
        let mut navs = webview::Navigations::default();
        navs.begin(Some(a));
        let queued = navs.failure(Some(a));
        assert!(shown(&navs, queued));
        assert!(navs.started(Some(c)));
        assert!(!shown(&navs, queued));
        assert!(!shown(&navs, navs.failure(Some(a))));
        assert!(shown(&navs, navs.failure(Some(c))));

        // Nil navigations: a nil start is a new, unknown one, and a nil
        // failure belongs to it only.
        let mut navs = webview::Navigations::default();
        navs.begin(Some(a));
        assert!(!shown(&navs, navs.failure(None)));
        assert!(navs.started(None));
        assert!(shown(&navs, navs.failure(None)));
        assert!(!shown(&navs, navs.failure(Some(a))));
        assert!(navs.started(None));
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
            // On the page's side of the tab: back there (Esc, the panel
            // toggled off) or put there by activating the tab
            // (`activate_focus`). The page takes the keyboard with no click.
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
    fn activating_a_tab_gives_its_page_the_keyboard_unless_it_is_blank() {
        assert_eq!(activation_focus(false), ActivationFocus::AddressBar);
        assert_eq!(activation_focus(true), ActivationFocus::Page);
        // The request then waits for gpui's focus on the page's side of
        // the tab, which activation just put there.
        assert_eq!(page_refocus(true, true, false), Refocus::Take);
    }

    #[test]
    fn arrows_walk_the_suggestions_and_past_the_ends_back_to_the_typed_text() {
        assert_eq!(move_highlight(None, 3, true), Some(0));
        assert_eq!(move_highlight(Some(0), 3, true), Some(1));
        assert_eq!(move_highlight(Some(2), 3, true), None);
        assert_eq!(move_highlight(None, 3, false), Some(2));
        assert_eq!(move_highlight(Some(1), 3, false), Some(0));
        assert_eq!(move_highlight(Some(0), 3, false), None);
        assert_eq!(move_highlight(None, 0, true), None);
        assert_eq!(move_highlight(None, 0, false), None);
    }

    #[test]
    fn only_a_same_document_commit_titles_its_entry() {
        assert_eq!(commit_title(true, Some("Docs")), Some("Docs"));
        assert_eq!(commit_title(false, Some("Previous page")), None);
        assert_eq!(commit_title(true, None), None);
    }

    #[test]
    fn every_title_reaches_history_but_only_a_change_relabels() {
        let mut held = Some("Docs".to_string());
        // The same title as held (a restored tab, a site-wide title).
        assert_eq!(take_title(&mut held, "Docs".into()), (false, Some("Docs".into())));
        assert_eq!(take_title(&mut held, "API".into()), (true, Some("API".into())));
        assert_eq!(held.as_deref(), Some("API"));
        assert_eq!(take_title(&mut held, "  ".into()), (true, None));
        assert_eq!(held, None);
        assert_eq!(take_title(&mut held, String::new()), (false, None));
    }

    #[test]
    fn a_page_coming_back_is_not_a_visit_but_a_redirect_is() {
        let page = Some("http://localhost:8765/");
        assert!(reshow_commit(page, "http://localhost:8765/"));
        assert!(reshow_commit(page, "HTTP://LOCALHOST:8765"));
        assert!(!reshow_commit(page, "http://localhost:8765/login"));
        // A view made for what the user just entered.
        assert!(!reshow_commit(None, "http://localhost:8765/"));
        assert!(!reshow_commit(Some("about:blank"), "about:blank"));
    }

    /// Whether each of a tab's first two native views' first commit of
    /// `url` is recorded, for a tab whose first view loads because of
    /// `first`: what `ensure_native` and the `Committed` handler do.
    fn visits(first: PageLoad, url: &str) -> [bool; 2] {
        let mut next = first;
        [(); 2].map(|_| !reshow_commit(reshow_page(take_load(&mut next), url).as_deref(), url))
    }

    #[test]
    fn a_tab_s_first_load_is_a_visit_unless_it_brings_a_page_back() {
        let url = "http://localhost:8765/";
        // Typed into the address bar (or a suggestion, or a retry).
        assert_eq!(visits(PageLoad::Typed, url), [true, false]);
        // Ports' Open, a routed link, a page's new window.
        assert_eq!(visits(PageLoad::Opened, url), [true, false]);
        // Restored at launch: last session already recorded it.
        assert_eq!(visits(PageLoad::Restored, url), [false, false]);
        // Shown again after a tab switch.
        assert_eq!(visits(PageLoad::Reshown, url), [false, false]);
        // A restored page that redirects elsewhere is a visit there.
        assert!(!reshow_commit(
            reshow_page(PageLoad::Restored, url).as_deref(),
            "http://localhost:8765/login"
        ));
    }

    #[test]
    fn only_url_changes_save() {
        assert!(BrowserEvent::UrlChanged.saves());
        // An animated title would otherwise save, or keep pushing a
        // pending save back, forever.
        assert!(!BrowserEvent::TitleChanged.saves());
        // The new tab's own open saves.
        assert!(!BrowserEvent::NewWindow("http://localhost:3000/".into()).saves());
    }

    #[test]
    fn page_label_falls_back_to_the_host() {
        assert_eq!(page_label(None, None), "Browser");
        assert_eq!(page_label(Some("http://localhost:3000/x"), None), "Browser · localhost");
        assert_eq!(page_label(Some("http://localhost:3000/x"), Some("Vite")), "Browser · Vite");
        assert_eq!(page_label(Some("not a url"), None), "Browser");
    }

    #[test]
    fn a_docked_inspector_that_took_the_keyboard_pulls_focus_without_the_page_taking_it() {
        // (pending, inspector has keyboard, in pane, in address) ->
        // (pull gpui's focus to the tab, pending after).
        // The reported case: the page had the keyboard, the user clicked a
        // terminal (focus-out: pending = true), then Inspect Element docked
        // the inspector, which took the keyboard. The pull drops the
        // request, so the next frame leaves the keyboard with the inspector.
        let (pull, pending) = inspector_focus_pull(true, true, false, false);
        assert_eq!((pull, pending), (true, false));
        // gpui's focus is now on the page's side of the tab: no Take.
        assert_eq!(page_refocus(pending, true, false), Refocus::Keep);
        // (Left pending, it would have been handed to the page.)
        assert_eq!(page_refocus(true, true, false), Refocus::Take);
        // In the address bar (just clicked; it took the keyboard back
        // itself): pulled to the page's side, request dropped.
        assert_eq!(inspector_focus_pull(true, true, true, true), (true, false));
        // Already on the page's side: nothing to pull, request untouched.
        assert_eq!(inspector_focus_pull(true, true, true, false), (false, true));
        assert_eq!(inspector_focus_pull(false, true, true, false), (false, false));
        // The inspector doesn't hold the keyboard (closed, own window, or
        // the page has it): nothing changes.
        for pending in [false, true] {
            for (in_pane, in_address) in [(false, false), (true, false), (true, true)] {
                assert_eq!(inspector_focus_pull(pending, false, in_pane, in_address), (false, pending));
            }
        }
    }

    #[test]
    fn an_inspectors_placement_is_read_from_where_it_is() {
        use InspectorPlace::*;
        assert_eq!(placement_of(Docked(Some(DockSide::Bottom))), Some(InspectorPlacement::Bottom));
        assert_eq!(placement_of(Docked(Some(DockSide::Left))), Some(InspectorPlacement::Left));
        assert_eq!(placement_of(Docked(Some(DockSide::Right))), Some(InspectorPlacement::Right));
        assert_eq!(placement_of(OwnWindow), Some(InspectorPlacement::Window));
        for place in [Unsupported, NotPlaced, Docked(None), DockedElsewhere] {
            assert_eq!(placement_of(place), None, "{place:?}");
        }
    }

    /// Mutates the process-wide placement mirror (an atomic) and restores
    /// it; no other test may read it, since tests run in parallel.
    #[test]
    fn the_placement_mirror_round_trips() {
        let before = inspector_placement();
        for p in [
            InspectorPlacement::Left,
            InspectorPlacement::Right,
            InspectorPlacement::Window,
            InspectorPlacement::Bottom,
        ] {
            set_inspector_placement(p);
            assert_eq!(inspector_placement(), p);
        }
        set_inspector_placement(before);
    }

    #[test]
    fn an_opened_inspector_goes_to_the_users_placement_once_and_later_moves_are_remembered() {
        use InspectorPlacement as P;
        use InspectorWatch::*;
        let bottom = InspectorPlace::Docked(Some(DockSide::Bottom));
        let left = InspectorPlace::Docked(Some(DockSide::Left));
        let right = InspectorPlace::Docked(Some(DockSide::Right));
        let window = InspectorPlace::OwnWindow;
        let mid = InspectorPlace::Docked(None);

        // Closed: nothing, whatever WebKit says.
        for place in [bottom, window, InspectorPlace::NotPlaced] {
            assert_eq!(settle_inspector(Closed, P::Bottom, place), (None, Closed, None));
        }
        // Opening, not placed yet or mid-move: ask again next time.
        assert_eq!(settle_inspector(Opening, P::Bottom, InspectorPlace::NotPlaced), (None, Opening, None));
        assert_eq!(settle_inspector(Opening, P::Bottom, mid), (None, Opening, None));
        // Opened where the user wants it: settled, nothing remembered.
        assert_eq!(settle_inspector(Opening, P::Bottom, bottom), (None, Watching(Some(P::Bottom)), None));
        assert_eq!(settle_inspector(Opening, P::Window, window), (None, Watching(Some(P::Window)), None));
        // Opened elsewhere: moved, once, to the user's placement.
        assert_eq!(
            settle_inspector(Opening, P::Bottom, window),
            (Some(InspectorMove::AttachBottom), Moved(0), None)
        );
        assert_eq!(settle_inspector(Opening, P::Window, bottom), (Some(InspectorMove::Detach), Moved(0), None));
        assert_eq!(
            settle_inspector(Opening, P::Left, window),
            (Some(InspectorMove::Dock(DockSide::Left)), Moved(DOCK_RETRIES), None)
        );
        assert_eq!(
            settle_inspector(Opening, P::Right, left),
            (Some(InspectorMove::Dock(DockSide::Right)), Moved(DOCK_RETRIES), None)
        );
        // After our move: wherever it ended up is the baseline, never
        // remembered and never moved again (WebKit refuses to dock beside
        // a small page; no loop).
        assert_eq!(settle_inspector(Moved(0), P::Bottom, window), (None, Watching(Some(P::Window)), None));
        assert_eq!(settle_inspector(Moved(0), P::Left, left), (None, Watching(Some(P::Left)), None));
        assert_eq!(settle_inspector(Moved(2), P::Left, mid), (None, Moved(2), None));
        // A left/right dock that hasn't landed (its script ran before the
        // inspector's page was ready) is asked again, at most DOCK_RETRIES
        // times, then it stays where it is, not remembered.
        let dock_left = Some(InspectorMove::Dock(DockSide::Left));
        let mut watch = Moved(DOCK_RETRIES);
        for _ in 0..DOCK_RETRIES {
            let (to, next, chosen) = settle_inspector(watch, P::Left, window);
            assert_eq!((to, chosen), (dock_left, None));
            watch = next;
        }
        assert_eq!(watch, Moved(0));
        assert_eq!(settle_inspector(watch, P::Left, window), (None, Watching(Some(P::Window)), None));
        // Landing on a retry settles it there.
        assert_eq!(settle_inspector(Moved(1), P::Left, left), (None, Watching(Some(P::Left)), None));
        // Bottom/window moves are never repeated (attach/detach act at once;
        // a refused attach must not loop).
        assert_eq!(settle_inspector(Moved(0), P::Bottom, window), (None, Watching(Some(P::Window)), None));
        // Settled: a different placement is the user's, from the
        // inspector's toolbar; it is remembered. The same one is not.
        assert_eq!(settle_inspector(Watching(Some(P::Bottom)), P::Bottom, bottom), (None, Watching(Some(P::Bottom)), None));
        assert_eq!(
            settle_inspector(Watching(Some(P::Bottom)), P::Bottom, right),
            (None, Watching(Some(P::Right)), Some(P::Right))
        );
        assert_eq!(
            settle_inspector(Watching(Some(P::Right)), P::Bottom, window),
            (None, Watching(Some(P::Window)), Some(P::Window))
        );
        // Mid-move while watching: wait for where it lands.
        assert_eq!(settle_inspector(Watching(Some(P::Bottom)), P::Bottom, mid), (None, Watching(Some(P::Bottom)), None));
        // Docked outside the tab (never expected): its own window, once.
        let elsewhere = InspectorPlace::DockedElsewhere;
        assert_eq!(settle_inspector(Opening, P::Bottom, elsewhere), (Some(InspectorMove::Detach), Moved(0), None));
        assert_eq!(settle_inspector(Moved(0), P::Bottom, elsewhere), (None, Watching(None), None));
        assert_eq!(settle_inspector(Moved(3), P::Left, elsewhere), (None, Watching(None), None));
        assert_eq!(settle_inspector(Watching(None), P::Bottom, elsewhere), (None, Watching(None), None));
        // A late result of that move, first readable now: the baseline,
        // not the user's choice.
        assert_eq!(settle_inspector(Watching(None), P::Bottom, window), (None, Watching(Some(P::Window)), None));
        // WebKit can't say where it is: left alone for good.
        for watch in [Opening, Moved(0), Moved(DOCK_RETRIES), Watching(Some(P::Left))] {
            assert_eq!(
                settle_inspector(watch, P::Bottom, InspectorPlace::Unsupported),
                (None, Watching(None), None)
            );
        }
    }

    #[test]
    fn the_developer_tools_button_follows_webkit_the_page_and_the_inspector() {
        use DevToolsButton::{Disabled, Hidden, Off, On};
        // (available, has_page_view, inspector_open) -> button.
        let table = [
            // WebKit lacks the private inspector API: no button at all,
            // whatever the tab shows.
            ((false, false, false), Hidden),
            ((false, true, false), Hidden),
            ((false, true, true), Hidden),
            // A blank tab, or one whose page isn't up (no native view).
            ((true, false, false), Disabled),
            // A stale "open" from a dropped view never shows as active.
            ((true, false, true), Disabled),
            ((true, true, false), Off),
            ((true, true, true), On),
        ];
        for ((available, page, open), button) in table {
            assert_eq!(
                devtools_button(available, page, open),
                button,
                "available={available} page={page} open={open}"
            );
        }
    }
}
