//! Native WebKit view for the browser tab: a `WKWebView` added as a subview of
//! the gpui window's own NSView, positioned from gpui layout bounds.
//!
//! **Library choice: `objc2-web-kit`, not `wry`** (Task 5 spike). Both are
//! WebKit and both build against the `objc2` 0.6 already in the lockfile
//! (`wry` 0.55 bumps it 0.6.3 → 0.6.5, no second 0.6.x copy). `wry` was
//! dropped on interface fit, not on duplicates:
//! - It owns the `WKNavigationDelegate` and exposes no load-failure callback
//!   (no `didFailProvisionalNavigation`), which the tab needs for its
//!   "connection refused, Reload" state. Adding one means replacing its
//!   delegate, which breaks its own handlers.
//! - It has no back/forward/stop/can-go-back/can-go-forward, and no URL change
//!   signal for `pushState` (single-page apps); those need the raw
//!   `WKWebView` and KVO anyway.
//! - Its `WKWebView` subclass (`wry_web_view.rs`) makes `performKeyEquivalent:`
//!   return NO for every child web view, so ⌘-key events skip the page (its
//!   own FIXME: JS can't see them) and ⌘C/⌘V would hinge on a native Edit
//!   menu. The tab wants the page to get clipboard keys. It also overrides
//!   drag-and-drop.
//!
//! `objc2-web-kit` 0.3.2 adds one crate to the lockfile (its `objc2-app-kit`,
//! `block2` and `objc2-foundation` are already there through gpui_macos), and
//! gives the full delegate and KVO surface directly.
//!
//! **Navigation policy is fail-safe** (Task 5 review ruling): a page must not
//! be able to launch apps or local files on its own.
//! - `file:`, `javascript:` and unclassifiable URLs are never handed out.
//! - App schemes (`mailto:`, `vscode:`, `zed:` …) go to the system only for a
//!   link click whose source frame is the main frame and whose target is the
//!   main frame or a new window. Script navigations, iframes and redirects
//!   the user didn't click are ignored with a log line (scheme only).
//!   Accepted cost: "open in app" redirect flows that weren't started by a
//!   click don't launch; "Open in default browser" is the way out.
//! - A "click" is a `LinkActivated` navigation within [`GESTURE_WINDOW`] of
//!   a real user gesture on that page ([`user_clicked`]). WebKit reports a
//!   script's `a.click()` as `LinkActivated` too, and the public
//!   `WKNavigationAction` API carries no gesture flag, so the key monitor
//!   below stamps each page's last gesture: a left mouse-down or mouse-up on
//!   it, or Return/Space while it has the keyboard (keyboard activation of a
//!   focused link or button). Known limits: a script click that runs within
//!   the window of a real gesture on the same page still counts, and so
//!   does any `LinkActivated` navigation right after typing Space or Return
//!   into the page.
//! - Downloads go out only from the main frame, and only http(s);
//!   `blob:`/`data:` downloads report "not supported" through `on_notice`
//!   (not a load failure: the page stays). `<a download>` goes only on a
//!   click, as above. A response the server marks as a download carries no
//!   navigation type, so it is judged by the main-frame navigation that
//!   fetched it, the last one the policy let load: it goes only if that
//!   one followed a real gesture on the page within [`GESTURE_WINDOW`] (a
//!   link, a form's button, a script answering the click) or the app
//!   started it (the address bar, a restore, a routed link). "The app
//!   started it" is read as a nil `sourceFrame`, on the assumption, not yet
//!   observed in the app, that WebKit reports `loadRequest` that way and
//!   never a page's own navigation; if a page's navigation could arrive
//!   with no source frame, its attachment would go out. A script's own
//!   navigation to an attachment is otherwise ignored. The default browser
//!   fetches the file again itself.
//! - `link_routing::open_external` also rate-limits to one open per second.
//! - New windows (each a tab here) need a user gesture:
//!   `javaScriptCanOpenWindowsAutomatically` is off (macOS defaults it on),
//!   and the pane opens at most one tab per page per second
//!   (`new_window_allowed` in `mod.rs`).
//!
//! **⌘-chords over a focused page: one app-wide local `NSEvent` monitor**
//! (Task 7), not a `performKeyEquivalent:` override. The dispatch order was
//! measured, not assumed: a Swift probe of this exact hierarchy (a parent
//! view whose `performKeyEquivalent:` never calls super, as gpui's doesn't,
//! with a `WKWebView` subview as first responder) shows AppKit offers a
//! ⌘-chord to a local monitor, then to gpui's view, then to the main menu,
//! and only then to the page as `keyDown:`. The page's own
//! `performKeyEquivalent:` never runs, so an override on it (or on a
//! container under gpui's view) would never be called. App chords therefore
//! already reached gpui; what broke was the other direction, gpui eating the
//! page's keys. The global ⌘C (copy diff lines) swallowed page copy, and with
//! gpui's focus left in the address bar (click it, then click the page)
//! ⌘V/⌘A/⌘← edited the address bar instead of the page.
//!
//! A chord is the page's only while the page holds AppKit's first responder
//! *and* gpui's focus is on the page's side of its tab: in the tab but not
//! in its address bar ([`route_key`]). gpui moving its focus never moves
//! AppKit's first responder (gpui's view takes it only at window creation
//! and on a simple-fullscreen switch, never on a click), so after ⌘⇧F
//! focused the tools panel's search box, or a click focused the address
//! bar, the page could still hold it and take their typing and ⌘C/⌘V. The
//! pane keeps the two in step:
//! - gpui's focus leaving the tab, or entering its address bar, hands the
//!   first responder back to gpui's view (`set_pane_focused` and
//!   `release_focus`, from focus-out and focus-in listeners);
//! - leaving the tab while the page had it also asks for the page to get it
//!   back once gpui's focus returns to the page's side without a click (Esc,
//!   the panel toggled off); returning to the address bar drops the request;
//! - a click into the page (the monitor also watches left mouse-downs, and
//!   stamps them as the page's last gesture) moves gpui's focus to the tab
//!   (`on_click`);
//! - activating a tab with a page (a tab click, ⌘1..9, a routed or Ports
//!   open, the restored active tab) puts gpui's focus on the tab and asks
//!   for the page to get the first responder (`activate_focus`), so it
//!   takes typing with no click. A blank tab focuses its address bar.
//!
//! Bindings without ⌘ (`ctrl-q`) are untouched: the monitor lets them by,
//! and gpui sees them first through its view's `performKeyEquivalent:` as
//! before. Plain typing never passes gpui's key equivalents at all; it goes
//! straight to the page's `keyDown:`.
//!
//! When it is the page's, the monitor:
//! - lets a chord from `shortcuts::app_chords` (Global scope, with ⌘, the
//!   user's rebindings applied; see [`set_app_shortcuts`]) through to gpui;
//! - hands any other ⌘-chord to the page's `performKeyEquivalent:`, so the
//!   page's script sees the `keydown` first, as in Safari, and gpui never
//!   sees it. WebKit sends a chord the page leaves alone back through
//!   `[NSApp sendEvent:]` as the same `NSEvent`; the probe saw the monitor
//!   get it a second time. The monitor recognizes it and sends the editing
//!   action (`copy:`, `paste:`, `cut:`, `selectAll:`, `undo:`, `redo:`) to
//!   the page, or offers any other chord to the main menu, then (untaken)
//!   back to AppKit's usual path, so ⌘` still cycles windows. The editing keys go
//!   as actions, not through the menu, because gpui's Undo/Redo menu items
//!   are gpui actions the page can't receive.
//!
//! The monitor exists for the life of the app, so dropping a view from
//! inside it (⌘W closing the tab) can't pull it out from under itself.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use crate::link_routing;
use crate::shortcuts::Chord;

thread_local! {
    /// What `shortcuts::app_chords` gave for the installed keymap.
    static APP_CHORDS: RefCell<Vec<Chord>> = const { RefCell::new(Vec::new()) };
}

/// The ⌘-chords that reach the app while a page has the keyboard. Called
/// whenever the keymap is (re)installed, so rebindings apply over pages too.
pub(crate) fn set_app_shortcuts(chords: Vec<Chord>) {
    APP_CHORDS.with(|c| *c.borrow_mut() = chords);
}

/// Where the key monitor sends a keyDown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyRoute {
    /// AppKit's usual path: gpui's view first, then the menu, then the
    /// first responder.
    AsUsual,
    /// The page's `performKeyEquivalent:`, never gpui.
    Page,
}

/// The monitor's routing decision. A ⌘-chord goes to the page only when the
/// page has AppKit's keyboard (`page_has_keyboard`) *and* gpui's focus is on
/// the page's side of its tab, in the tab but not its address bar
/// (`page_has_gpui_focus`), and it isn't one of the app's chords. gpui
/// moving its focus never moves AppKit's first responder, so the page can
/// still hold it after ⌘⇧F (say) focused the tools panel's search box, or a
/// click focused the address bar; the second input keeps the page from
/// taking their keys.
pub(crate) fn route_key(
    page_has_keyboard: bool,
    page_has_gpui_focus: bool,
    command: bool,
    app_chord: bool,
) -> KeyRoute {
    if page_has_keyboard && page_has_gpui_focus && command && !app_chord {
        KeyRoute::Page
    } else {
        KeyRoute::AsUsual
    }
}

/// The catalog's name (`t`, `]`, `down`) for the key of a ⌘-chord. `chars`
/// is what the key types with ⌘ alone (`charactersByApplyingModifiers:`),
/// which is the layout's command keys (QWERTY for Dvorak-QWERTY⌘) and leaves
/// out shift (`]`, not `}`). When that isn't ASCII (a Cyrillic or Greek
/// layout), the key's US position names it, as gpui does for such layouts.
/// `None` for a key neither can name: never an app shortcut.
pub(crate) fn chord_key(chars: &str, key_code: u16) -> Option<String> {
    let mut it = chars.chars();
    let named = match (it.next(), it.next()) {
        (Some(c), None) => match c {
            '\u{f700}' => Some("up"),
            '\u{f701}' => Some("down"),
            '\u{f702}' => Some("left"),
            '\u{f703}' => Some("right"),
            '\u{f728}' => Some("delete"),
            '\r' | '\u{3}' => Some("enter"),
            '\t' | '\u{19}' => Some("tab"),
            '\u{1b}' => Some("escape"),
            '\u{7f}' => Some("backspace"),
            ' ' => Some("space"),
            c if c.is_ascii_graphic() => return Some(c.to_ascii_lowercase().to_string()),
            _ => None,
        },
        _ => None,
    };
    named.or_else(|| us_key(key_code)).map(str::to_string)
}

/// The ways the ⌘-chord being pressed can be written. The catalog writes ⇧
/// with a non-letter as the base key plus shift (`cmd-shift-]`); gpui reads
/// such a keypress as the shifted character without shift (`cmd-}`), and
/// that is how a chord rebound by pressing it is stored. Both, so either
/// matches; letters keep shift in both, so they get one. `chars` is what the
/// key types with ⌘ alone, `shifted` with ⌘⇧. The first is the catalog form.
pub(crate) fn event_chords(
    chars: &str,
    shifted: &str,
    key_code: u16,
    cmd: bool,
    shift: bool,
    alt: bool,
    ctrl: bool,
) -> Vec<Chord> {
    let mut out = Vec::new();
    if let Some(key) = chord_key(chars, key_code) {
        out.push(Chord::new(&key, cmd, shift, alt, ctrl));
    }
    let mut it = shifted.chars();
    if shift
        && let (Some(c), None) = (it.next(), it.next())
        && c.is_ascii_graphic()
        && !c.is_ascii_alphabetic()
    {
        out.push(Chord::new(&c.to_string(), cmd, false, alt, ctrl));
    }
    out
}

/// The US-layout character of a virtual key code (`kVK_ANSI_*`).
fn us_key(key_code: u16) -> Option<&'static str> {
    const KEYS: [(u16, &str); 47] = [
        (0, "a"), (1, "s"), (2, "d"), (3, "f"), (4, "h"), (5, "g"), (6, "z"), (7, "x"),
        (8, "c"), (9, "v"), (11, "b"), (12, "q"), (13, "w"), (14, "e"), (15, "r"),
        (16, "y"), (17, "t"), (18, "1"), (19, "2"), (20, "3"), (21, "4"), (22, "6"),
        (23, "5"), (24, "="), (25, "9"), (26, "7"), (27, "-"), (28, "8"), (29, "0"),
        (30, "]"), (31, "o"), (32, "u"), (33, "["), (34, "i"), (35, "p"), (37, "l"),
        (38, "j"), (39, "'"), (40, "k"), (41, ";"), (42, "\\"), (43, ","), (44, "/"),
        (45, "n"), (46, "m"), (47, "."), (50, "`"),
    ];
    KEYS.iter().find(|(code, _)| *code == key_code).map(|(_, key)| *key)
}

/// Callbacks from the page, owned by the [`NativeWebView`] that runs them.
///
/// Every callback but `on_new_window` and `open_external` is posted to the
/// main dispatch queue, never run inline. KVO fires synchronously inside our
/// own `load`/`reload` calls, so an inline callback that updated the gpui
/// entity calling `load` would re-borrow it and panic. Callbacks queued when
/// the view is dropped are discarded.
pub(crate) struct NativeEvents {
    /// The page title; empty when the page has none.
    pub on_title: Box<dyn Fn(String)>,
    /// The page URL, including `pushState` changes. It also moves to an
    /// address WebKit is only trying (and falls back if that fails), so it
    /// is not a visit: see `on_commit`.
    pub on_url: Box<dyn Fn(String)>,
    /// The page is now at this URL for real: a top-level navigation
    /// committed (`didCommitNavigation`), or the URL changed in place with
    /// nothing loading (`pushState`, a `#fragment`). What browsing history
    /// records.
    pub on_commit: Box<dyn Fn(String)>,
    pub on_loading: Box<dyn Fn(bool)>,
    /// A page load failed: the URL that failed, when WebKit names it (the
    /// view's own URL has already fallen back to the last committed page by
    /// then), and WebKit's message, e.g. "Could not connect to the server.".
    /// Cancellations, including our own policy cancels, are not failures.
    /// The last argument is the view's navigation number when WebKit
    /// reported it ([`NativeWebView::navigation`]), read before the
    /// main-queue hop, so a failure that lands after the next
    /// `load`/`reload`/`back`/`forward` can be told apart.
    pub on_failed: Box<dyn Fn(Option<String>, String, u64)>,
    /// Something the user should hear about that is not a load failure; the
    /// page stays as it is. Today only [`DOWNLOAD_UNSUPPORTED`].
    pub on_notice: Box<dyn Fn(String)>,
    /// `target=_blank` or `window.open` asked for an http(s) page. Called
    /// inline, so the request can't be lost to the view being dropped
    /// before a post would run; it must only queue (it can't touch gpui).
    pub on_new_window: Box<dyn Fn(String)>,
    /// The user clicked into the page: gpui's focus should follow (AppKit's
    /// first responder already has), so the tab, not whatever gpui had
    /// focused before, owns the keys.
    pub on_click: Box<dyn Fn()>,
    /// Navigation policy hand-off: http(s) loads in the page; what
    /// [`decide_navigation`] and [`decide_download`] send out (clicked
    /// app-scheme links, clicked main-frame http(s) downloads) goes here.
    /// Called inline (it doesn't touch gpui).
    pub open_external: fn(&str),
}

impl Default for NativeEvents {
    fn default() -> Self {
        Self {
            on_title: Box::new(|_| {}),
            on_url: Box::new(|_| {}),
            on_commit: Box::new(|_| {}),
            on_loading: Box::new(|_| {}),
            on_failed: Box::new(|_, _, _| {}),
            on_notice: Box::new(|_| {}),
            on_new_window: Box::new(|_| {}),
            on_click: Box::new(|| {}),
            open_external: link_routing::open_external,
        }
    }
}

/// What to do with a navigation the page asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NavDecision {
    /// Let WebKit load it here.
    Load,
    /// Cancel it and hand the URL to the system.
    External,
    /// Cancel it quietly.
    Ignore,
    /// Cancel it and tell the user through `on_notice` (a download we can't
    /// hand over), so the click isn't dead.
    Unsupported,
}

/// How long after a real gesture on a page (see the module docs) a link
/// activation still counts as the user's click. Link activation follows the
/// mouse-up or the key press within a frame or two; a second leaves room for
/// a busy page.
pub(crate) const GESTURE_WINDOW: Duration = Duration::from_secs(1);

/// Whether a navigation of `link_activated` type (`LinkActivated`) is the
/// user's click: one within [`GESTURE_WINDOW`] of the page's last real
/// gesture. WebKit reports a script's `a.click()` as link activation too.
pub(crate) fn user_clicked(link_activated: bool, last_gesture: Option<Instant>, now: Instant) -> bool {
    link_activated && last_gesture.is_some_and(|t| now.saturating_duration_since(t) <= GESTURE_WINDOW)
}

/// Where a navigation comes from, as WebKit reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NavOrigin {
    /// The user clicked a link ([`user_clicked`]: `LinkActivated` right
    /// after a real gesture on the page), as opposed to a script
    /// (`location = …`, `a.click()` with no gesture), a form, a reload or
    /// our own `load`. For a download response, [`response_origin`]: the
    /// user started the navigation that fetched it.
    pub user_initiated: bool,
    /// The frame that started it (`sourceFrame`) is the main frame.
    pub from_main_frame: bool,
}

impl NavOrigin {
    /// Only a click in the top-level page may leave the app.
    fn may_leave_app(self) -> bool {
        self.user_initiated && self.from_main_frame
    }
}

/// Fail-safe navigation policy (Task 5 review ruling):
/// - http(s) loads in the page, as do `about:`, `data:` and `blob:`: the
///   page's own plumbing (the blank tab, iframes, generated content), which
///   must never reach the default browser.
/// - `file:`, `javascript:` and anything we can't classify are ignored,
///   never handed out: `open file:///…/x.app` would launch local files.
/// - Any other scheme (`mailto:`, `tel:`, `vscode:`, `zed:`, `slack:` …)
///   leaves the app only when the user clicked a link in the main frame and
///   the target is the main frame (or a new window). Scripts, iframes and
///   navigations the user didn't click are ignored with a log line, so a page
///   can't launch apps on its own. Cost: an "open in app" redirect chain that
///   wasn't started by a click won't launch; the tab's "Open in default
///   browser" button is the way out.
pub(crate) fn decide_navigation(url: &str, to_main_frame: bool, origin: NavOrigin) -> NavDecision {
    match scheme(url).as_deref() {
        Some("http" | "https" | "about" | "data" | "blob") => NavDecision::Load,
        None | Some("file" | "javascript") => {
            log::info!("browser: ignoring a navigation to {}", scheme_label(url));
            NavDecision::Ignore
        }
        Some(_) if to_main_frame && origin.may_leave_app() => NavDecision::External,
        Some(s) => {
            log::info!("browser: not opening {s}: no link click in the main frame");
            NavDecision::Ignore
        }
    }
}

/// A new-window request: http(s) becomes a new tab (`Load`), other schemes
/// follow [`decide_navigation`], and `window.open()` with no URL or
/// `about:blank` (scripts that write into the new window) is ignored: there
/// is no window to give it.
pub(crate) fn decide_new_window(url: &str, origin: NavOrigin) -> NavDecision {
    match decide_navigation(url, true, origin) {
        NavDecision::Load if is_web(url) => NavDecision::Load,
        NavDecision::Load => NavDecision::Ignore,
        other => other,
    }
}

/// Where a download goes. Only the main frame's downloads are handed out, and
/// only those the user asked for: a clicked `<a download>`, or a response
/// whose navigation the user started ([`response_origin`]). The default
/// browser fetches the file again, so only http(s) can go (`External`). Page-generated downloads (`blob:`,
/// `data:`, e.g. "export JSON") exist only inside the page: `Unsupported`.
pub(crate) fn decide_download(url: &str, origin: NavOrigin) -> NavDecision {
    if !origin.from_main_frame {
        log::info!("browser: ignoring a download from a subframe ({})", scheme_label(url));
        NavDecision::Ignore
    } else if !is_web(url) {
        log::info!("browser: downloads from {} aren't supported", scheme_label(url));
        NavDecision::Unsupported
    } else if !origin.user_initiated {
        log::info!("browser: ignoring a download the user didn't click for");
        NavDecision::Ignore
    } else {
        NavDecision::External
    }
}

/// Whether a main-frame navigation counts as the user's when its response
/// turns out to be a download: the app started it (`app_initiated`: no
/// source frame, assumed to mean the address bar, a restore or a routed
/// link; see the module docs), or it
/// came within [`GESTURE_WINDOW`] of a real gesture on the page, whatever
/// its type (a link, a form's submit button, a script answering the click).
/// A script on its own (a timer, page load) never counts.
pub(crate) fn user_started(app_initiated: bool, last_gesture: Option<Instant>, now: Instant) -> bool {
    app_initiated || user_clicked(true, last_gesture, now)
}

/// The origin [`decide_download`] gets for a response WebKit marks as a
/// download. A response has no navigation type; `started` is
/// [`user_started`] for the last main-frame navigation the policy let load
/// (false when there was none), the one that fetched it.
pub(crate) fn response_origin(for_main_frame: bool, started: bool) -> NavOrigin {
    NavOrigin {
        user_initiated: for_main_frame && started,
        from_main_frame: for_main_frame,
    }
}

/// The text `on_notice` gets for an [`NavDecision::Unsupported`] download.
pub(crate) const DOWNLOAD_UNSUPPORTED: &str = "Downloads from this page aren't supported yet";

/// A response WebKit can't show, or one the server marks as an attachment,
/// is a download.
pub(crate) fn is_download(can_show_mime: bool, content_disposition: Option<&str>) -> bool {
    !can_show_mime
        || content_disposition.is_some_and(|d| {
            d.trim_start()
                .get(..10)
                .is_some_and(|p| p.eq_ignore_ascii_case("attachment"))
        })
}

/// Errors that are not load failures: a cancelled load (`stop`, a new
/// navigation, or our own policy cancel), a load interrupted by a policy
/// change (our download hand-off), and a plug-in (media) taking over.
pub(crate) fn is_benign_failure(domain: &str, code: isize) -> bool {
    matches!(
        (domain, code),
        ("NSURLErrorDomain", -999) | ("WebKitErrorDomain", 102 | 204)
    )
}

/// gpui's top-left `y` for a rect of `height` → the bottom-left `y` an
/// unflipped NSView of `parent_height` uses.
pub(crate) fn flip_y(bounds_y: f32, height: f32, parent_height: f32) -> f32 {
    parent_height - bounds_y - height
}

/// Safari's version when it can't be read from Safari.app (Safari 26
/// shipped with macOS 26, the fall 2025 release). Only the fallback; the
/// installed Safari's own version is preferred.
pub(crate) const SAFARI_FALLBACK_VERSION: &str = "26.0";

/// What WebKit appends to its user agent so sites see Safari:
/// `Version/<major.minor> Safari/605.1.15`, the token Safari itself sends.
/// WKWebView's default agent stops at "(KHTML, like Gecko)", and sites such
/// as Google then serve their legacy page. Only the suffix is set, never a
/// full custom agent, so WebKit keeps its own platform and engine parts.
/// `version` is Safari.app's `CFBundleShortVersionString`.
pub(crate) fn safari_app_name(version: Option<&str>) -> String {
    let major_minor = version
        .map(|v| v.split('.').take(2).collect::<Vec<_>>().join("."))
        .filter(|v| {
            !v.is_empty()
                && v.split('.')
                    .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        });
    format!(
        "Version/{} Safari/605.1.15",
        major_minor.as_deref().unwrap_or(SAFARI_FALLBACK_VERSION)
    )
}

fn scheme(url: &str) -> Option<String> {
    url::Url::parse(url).ok().map(|u| u.scheme().to_owned())
}

fn is_web(url: &str) -> bool {
    matches!(scheme(url).as_deref(), Some("http" | "https"))
}

/// The scheme alone, for log lines: never the URL, whose query may carry
/// tokens.
fn scheme_label(url: &str) -> String {
    scheme(url).map_or_else(|| "an unparsable URL".to_string(), |s| format!("{s}:"))
}

#[cfg(target_os = "macos")]
pub(crate) use mac::{NativeWebView, window_is_minimized_or_hidden};

#[cfg(target_os = "macos")]
mod mac {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::ffi::c_void;
    use std::ptr::{self, NonNull};
    use std::rc::{Rc, Weak};
    use std::time::Instant;

    use anyhow::{Context as _, anyhow, bail};
    use block2::{DynBlock, RcBlock};
    use dispatch2::{DispatchQueue, MainThreadBound};
    use gpui::{Bounds, Pixels, Window};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject, ProtocolObject, Sel};
    use objc2::{
        DefinedClass, MainThreadMarker, MainThreadOnly, Message as _, define_class, msg_send, sel,
    };
    use objc2_app_kit::{
        NSApplication, NSAutoresizingMaskOptions, NSEvent, NSEventMask, NSEventModifierFlags,
        NSEventType, NSView, NSWindow,
    };
    use objc2_foundation::{
        NSBundle, NSDictionary, NSError, NSHTTPURLResponse, NSKeyValueChangeKey,
        NSKeyValueObservingOptions, NSObjectNSKeyValueObserverRegistration, NSObjectProtocol,
        NSPoint, NSRect, NSSize, NSString, NSURL, NSURLRequest, ns_string,
    };
    use objc2_web_kit::{
        WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate,
        WKNavigationResponse, WKNavigationResponsePolicy, WKNavigationType, WKUIDelegate, WKWebView,
        WKWebViewConfiguration, WKWebsiteDataStore, WKWindowFeatures,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::{
        APP_CHORDS, DOWNLOAD_UNSUPPORTED, KeyRoute, NavDecision, NavOrigin, NativeEvents,
        decide_download, decide_navigation, decide_new_window, event_chords, flip_y,
        is_benign_failure, is_download, response_origin, route_key, safari_app_name, user_clicked,
        user_started,
    };
    use crate::shortcuts::{Chord, PageEdit, is_app_shortcut, page_edit};

    /// The WKWebView properties we observe. All are documented KVO-compliant.
    const KVO_KEYS: [&str; 3] = ["title", "URL", "loading"];

    /// Tags our KVO registrations so we only ever react to (and remove) our own.
    static KVO_TAG: u8 = 0;

    fn kvo_context() -> *mut c_void {
        &KVO_TAG as *const u8 as *mut c_void
    }

    /// The user-agent suffix (see [`safari_app_name`]), from the installed
    /// Safari's version, read once.
    fn user_agent_app_name() -> &'static str {
        static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        NAME.get_or_init(|| {
            let version = NSBundle::bundleWithPath(ns_string!("/Applications/Safari.app"))
                .and_then(|b| b.objectForInfoDictionaryKey(ns_string!("CFBundleShortVersionString")))
                .and_then(|v| v.downcast::<NSString>().ok())
                .map(|v| v.to_string());
            safari_app_name(version.as_deref())
        })
    }

    /// Whether the user clicked a link ([`user_clicked`], from the page's
    /// last real gesture), and whether the main frame started it. A nil
    /// `sourceFrame` (an app-initiated load) is not the main frame.
    fn origin_of(action: &WKNavigationAction, last_gesture: Option<Instant>) -> NavOrigin {
        let link_activated = unsafe { action.navigationType() } == WKNavigationType::LinkActivated;
        NavOrigin {
            user_initiated: user_clicked(link_activated, last_gesture, Instant::now()),
            from_main_frame: unsafe { action.sourceFrame() }.is_some_and(|f| unsafe { f.isMainFrame() }),
        }
    }

    fn url_string(url: Option<Retained<NSURL>>) -> String {
        url.and_then(|u| u.absoluteString())
            .map(|s| s.to_string())
            .unwrap_or_default()
    }

    struct Ivars {
        /// Weak: the NativeWebView owns the events. WebKit may still hold the
        /// delegate for a moment after the view is dropped; nothing reaches
        /// the callbacks then.
        events: Weak<NativeEvents>,
        /// The page's last real gesture, stamped by the key monitor; shared
        /// with its `PageEntry`.
        gesture: Rc<Cell<Option<Instant>>>,
        /// The view's navigation number (see [`NativeWebView::navigation`]).
        navigation: Rc<Cell<u64>>,
        /// [`user_started`] for the last main-frame navigation the policy
        /// let load: what a download response it fetches is judged by.
        main_user_started: Cell<bool>,
    }

    define_class!(
        // Navigation delegate, UI delegate and KVO observer in one object.
        // The web view holds it weakly (delegates) or not at all (KVO);
        // NativeWebView owns it, and it holds nothing back: no cycle.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "HarnessWebViewDelegate"]
        #[ivars = Ivars]
        struct Delegate;

        impl Delegate {
            #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
            fn observe_value(
                &self,
                key_path: Option<&NSString>,
                object: Option<&AnyObject>,
                _change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>,
                context: *mut c_void,
            ) {
                // Only our own registrations exist on this private class, so
                // anything else is ignored rather than passed to NSObject
                // (whose default raises).
                if context != kvo_context() {
                    return;
                }
                let (Some(key), Some(view)) =
                    (key_path, object.and_then(|o| o.downcast_ref::<WKWebView>()))
                else {
                    return;
                };
                // Read the current value rather than the change dictionary.
                match key.to_string().as_str() {
                    "title" => {
                        let title = unsafe { view.title() }
                            .map(|t| t.to_string())
                            .unwrap_or_default();
                        self.post(move |e| (e.on_title)(title));
                    }
                    "URL" => {
                        let url = url_string(unsafe { view.URL() });
                        if !url.is_empty() {
                            // Nothing loading: a same-page change, which
                            // commits nothing but is where the page now is.
                            if !unsafe { view.isLoading() } {
                                let committed = url.clone();
                                self.post(move |e| (e.on_commit)(committed));
                            }
                            self.post(move |e| (e.on_url)(url));
                        }
                    }
                    "loading" => {
                        let loading = unsafe { view.isLoading() };
                        self.post(move |e| (e.on_loading)(loading));
                    }
                    _ => {}
                }
            }
        }

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl WKNavigationDelegate for Delegate {
            #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
            fn decide_action(
                &self,
                _web_view: &WKWebView,
                action: &WKNavigationAction,
                handler: &DynBlock<dyn Fn(WKNavigationActionPolicy)>,
            ) {
                let url = url_string(unsafe { action.request() }.URL());
                // No target frame = a new-window request. Judge it like a
                // main-frame load: web URLs go on to createWebView (below).
                let to_main_frame =
                    unsafe { action.targetFrame() }.is_none_or(|f| unsafe { f.isMainFrame() });
                let origin = origin_of(action, self.ivars().gesture.get());
                // `<a download>` and the like. macOS 11.3+; we require 12.
                let decision = if unsafe { action.shouldPerformDownload() } {
                    decide_download(&url, origin)
                } else {
                    decide_navigation(&url, to_main_frame, origin)
                };
                // A main-frame load (not a new window, which becomes a tab
                // of its own): remember who started it for its response.
                if decision == NavDecision::Load
                    && unsafe { action.targetFrame() }.is_some_and(|f| unsafe { f.isMainFrame() })
                {
                    let app_initiated = unsafe { action.sourceFrame() }.is_none();
                    self.ivars().main_user_started.set(user_started(
                        app_initiated,
                        self.ivars().gesture.get(),
                        Instant::now(),
                    ));
                }
                let policy = match decision {
                    NavDecision::Load => WKNavigationActionPolicy::Allow,
                    _ => WKNavigationActionPolicy::Cancel,
                };
                handler.call((policy,));
                self.act_on(decision, &url);
            }

            #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
            fn decide_response(
                &self,
                _web_view: &WKWebView,
                navigation: &WKNavigationResponse,
                handler: &DynBlock<dyn Fn(WKNavigationResponsePolicy)>,
            ) {
                let response = unsafe { navigation.response() };
                let disposition = response
                    .downcast_ref::<NSHTTPURLResponse>()
                    .and_then(|r| r.valueForHTTPHeaderField(ns_string!("Content-Disposition")))
                    .map(|d| d.to_string());
                if is_download(unsafe { navigation.canShowMIMEType() }, disposition.as_deref()) {
                    // Never WKNavigationResponsePolicy::Download: the default
                    // browser does downloads, so no download delegate here.
                    handler.call((WKNavigationResponsePolicy::Cancel,));
                    let url = url_string(response.URL());
                    // A response carries no navigation type: judge it by the
                    // main-frame navigation that fetched it.
                    let origin = response_origin(
                        unsafe { navigation.isForMainFrame() },
                        self.ivars().main_user_started.get(),
                    );
                    self.act_on(decide_download(&url, origin), &url);
                } else {
                    handler.call((WKNavigationResponsePolicy::Allow,));
                }
            }

            #[unsafe(method(webView:didCommitNavigation:))]
            fn did_commit(&self, web_view: &WKWebView, _navigation: Option<&WKNavigation>) {
                let url = url_string(unsafe { web_view.URL() });
                if !url.is_empty() {
                    self.post(move |e| (e.on_commit)(url));
                }
            }

            #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
            fn did_fail_provisional(
                &self,
                _web_view: &WKWebView,
                _navigation: Option<&WKNavigation>,
                error: &NSError,
            ) {
                self.failed(error);
            }

            #[unsafe(method(webView:didFailNavigation:withError:))]
            fn did_fail(
                &self,
                _web_view: &WKWebView,
                _navigation: Option<&WKNavigation>,
                error: &NSError,
            ) {
                self.failed(error);
            }

            #[unsafe(method(webViewWebContentProcessDidTerminate:))]
            fn content_process_terminated(&self, _web_view: &WKWebView) {
                let navigation = self.ivars().navigation.get();
                self.post(move |e| {
                    (e.on_failed)(None, "The page stopped unexpectedly.".to_string(), navigation)
                });
            }
        }

        unsafe impl WKUIDelegate for Delegate {
            #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
            fn create_web_view(
                &self,
                _web_view: &WKWebView,
                _configuration: &WKWebViewConfiguration,
                action: &WKNavigationAction,
                _features: &WKWindowFeatures,
            ) -> Option<Retained<WKWebView>> {
                let url = url_string(unsafe { action.request() }.URL());
                match decide_new_window(&url, origin_of(action, self.ivars().gesture.get())) {
                    // Inline (see `on_new_window`): a tab switch right after
                    // the click drops this view, and a post would be skipped.
                    NavDecision::Load => {
                        if let Some(events) = self.ivars().events.upgrade() {
                            (events.on_new_window)(url);
                        }
                    }
                    other => self.act_on(other, &url),
                }
                // We never hand WebKit a second view; the app opens a tab.
                None
            }
        }
    );

    impl Delegate {
        fn new(
            events: Weak<NativeEvents>,
            gesture: Rc<Cell<Option<Instant>>>,
            navigation: Rc<Cell<u64>>,
            mtm: MainThreadMarker,
        ) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(Ivars {
                events,
                gesture,
                navigation,
                main_user_started: Cell::new(false),
            });
            unsafe { msg_send![super(this), init] }
        }

        /// Queues `f` on the main dispatch queue (see [`post_event`]).
        fn post(&self, f: impl FnOnce(&NativeEvents) + 'static) {
            post_event(self.ivars().events.clone(), self.mtm(), f);
        }

        /// Carries out a cancel-side decision (the policy handler has
        /// already been called).
        fn act_on(&self, decision: NavDecision, url: &str) {
            match decision {
                NavDecision::External => {
                    if let Some(events) = self.ivars().events.upgrade() {
                        (events.open_external)(url);
                    }
                }
                NavDecision::Unsupported => {
                    self.post(|e| (e.on_notice)(DOWNLOAD_UNSUPPORTED.to_string()));
                }
                NavDecision::Load | NavDecision::Ignore => {}
            }
        }

        fn failed(&self, error: &NSError) {
            if is_benign_failure(&error.domain().to_string(), error.code()) {
                return;
            }
            let message = error.localizedDescription().to_string();
            // NSURLErrorFailingURLStringErrorKey's value. WebKit sets it on
            // its load errors; without it the pane falls back to what it
            // asked for last.
            let url = error
                .userInfo()
                .objectForKey(ns_string!("NSErrorFailingURLStringKey"))
                .and_then(|v| v.downcast::<NSString>().ok())
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty());
            // Read now: by the time the post runs, the pane may have started
            // another navigation.
            let navigation = self.ivars().navigation.get();
            self.post(move |e| (e.on_failed)(url, message, navigation));
        }
    }

    /// Queues `f` on the main dispatch queue (see [`NativeEvents`]). It is
    /// skipped if the NativeWebView is gone by then.
    fn post_event(
        events: Weak<NativeEvents>,
        mtm: MainThreadMarker,
        f: impl FnOnce(&NativeEvents) + 'static,
    ) {
        let job = MainThreadBound::new((events, f), mtm);
        DispatchQueue::main().exec_async(move || {
            // The main queue only ever runs on the main thread.
            let mtm = MainThreadMarker::new().expect("main queue off the main thread");
            let (events, f) = job.into_inner(mtm);
            if let Some(events) = events.upgrade() {
                f(&events);
            }
        });
    }

    /// A live page, as the monitor looks it up from its view.
    struct PageEntry {
        /// The `WKWebView`'s address; the entry goes when the view does.
        view: usize,
        /// gpui's focus is on this page's side of its tab: in the tab, not
        /// its address bar (see [`route_key`]).
        pane_focused: Rc<Cell<bool>>,
        /// The page's last real gesture (see the module docs).
        gesture: Rc<Cell<Option<Instant>>>,
        events: Weak<NativeEvents>,
    }

    thread_local! {
        static PAGES: RefCell<Vec<PageEntry>> = const { RefCell::new(Vec::new()) };
        static KEY_MONITOR: Cell<bool> = const { Cell::new(false) };
        /// The ⌘-chords lately handed to a page, newest last, retained so a
        /// pointer can't be reused by another event. WebKit sends back the
        /// ones the page leaves alone as these same objects.
        static FORWARDED: RefCell<VecDeque<Retained<NSEvent>>> =
            const { RefCell::new(VecDeque::new()) };
    }

    /// More than a page could have in flight at once (one per keypress,
    /// each settled within a web-process round trip).
    const FORWARDED_CAP: usize = 8;

    /// Installs the key monitor (see the module docs) once per process.
    fn install_key_monitor() {
        if KEY_MONITOR.replace(true) {
            return;
        }
        let handler = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: AppKit hands the handler a live event for the call.
            let ev = unsafe { event.as_ref() };
            if ev.r#type() == NSEventType::LeftMouseDown {
                note_page_click(ev);
                return event.as_ptr();
            }
            if ev.r#type() == NSEventType::LeftMouseUp {
                // A link activates on mouse-up; a press held a while still
                // counts.
                if let Some(entry) = page_hit(ev) {
                    entry.gesture.set(Some(Instant::now()));
                }
                return event.as_ptr();
            }
            note_page_key_gesture(ev);
            if route_page_key(ev) {
                ptr::null_mut()
            } else {
                event.as_ptr()
            }
        });
        let mask = NSEventMask::KeyDown | NSEventMask::LeftMouseDown | NSEventMask::LeftMouseUp;
        // SAFETY: the block has the handler signature AppKit documents.
        let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &handler) };
        match monitor {
            // Never removed: one monitor serves every web view in every
            // window, for the life of the app.
            Some(monitor) => std::mem::forget(monitor),
            None => {
                log::warn!("browser: no key monitor; ⌘-keys over a page go to the app");
                KEY_MONITOR.set(false);
            }
        }
    }

    /// The monitor's call for one keyDown: `true` consumes it.
    fn route_page_key(event: &NSEvent) -> bool {
        let Some(mtm) = MainThreadMarker::new() else {
            return false;
        };
        if take_forwarded(event) {
            // The page's script left it alone. An editing key becomes its
            // action on the page, never gpui's (its global ⌘C would copy
            // diff lines). Anything else is offered to the menu, and if the
            // menu doesn't take it either, AppKit finishes it as usual (⌘`
            // cycles windows after key equivalents fail).
            let app = NSApplication::sharedApplication(mtm);
            if let Some(edit) = event_chords_of(event).first().and_then(page_edit) {
                // SAFETY: a standard responder action, sent with no sender
                // to the key window's first responder (the page). Nobody
                // taking it lets AppKit go on rather than swallowing the key.
                return unsafe { app.sendAction_to_from(edit_action(edit), None, None) };
            }
            return app.mainMenu().is_some_and(|menu| menu.performKeyEquivalent(event));
        }
        let command = event.modifierFlags().contains(NSEventModifierFlags::Command);
        if !command {
            return false;
        }
        let Some(page) = event.window(mtm).as_deref().and_then(focused_page) else {
            return false;
        };
        let pane_focused = PAGES.with(|pages| {
            let addr = Retained::as_ptr(&page) as usize;
            pages
                .borrow()
                .iter()
                .any(|p| p.view == addr && p.pane_focused.get())
        });
        let for_app = APP_CHORDS.with(|app| {
            let app = app.borrow();
            event_chords_of(event).iter().any(|c| is_app_shortcut(c, &app))
        });
        if route_key(true, pane_focused, command, for_app) != KeyRoute::Page {
            return false;
        }
        FORWARDED.with(|f| {
            let mut f = f.borrow_mut();
            if f.len() == FORWARDED_CAP {
                f.pop_front();
            }
            f.push_back(event.retain());
        });
        // WebKit takes it only as the window's first responder; otherwise
        // it goes on as before.
        let taken = page.performKeyEquivalent(event);
        if !taken {
            take_forwarded(event);
        }
        taken
    }

    /// Whether `event` is one we handed to a page, forgetting it if so.
    fn take_forwarded(event: &NSEvent) -> bool {
        FORWARDED.with(|f| {
            let mut f = f.borrow_mut();
            match f.iter().position(|e| ptr::eq(&**e, event)) {
                Some(i) => {
                    f.remove(i);
                    true
                }
                None => false,
            }
        })
    }

    /// [`event_chords`] for a key event.
    fn event_chords_of(event: &NSEvent) -> Vec<Chord> {
        let flags = event.modifierFlags();
        let typed = |mods: NSEventModifierFlags| {
            event
                .charactersByApplyingModifiers(mods)
                .map(|s| s.to_string())
                .unwrap_or_default()
        };
        event_chords(
            &typed(NSEventModifierFlags::Command),
            &typed(NSEventModifierFlags::Command | NSEventModifierFlags::Shift),
            event.keyCode(),
            flags.contains(NSEventModifierFlags::Command),
            flags.contains(NSEventModifierFlags::Shift),
            flags.contains(NSEventModifierFlags::Option),
            flags.contains(NSEventModifierFlags::Control),
        )
    }

    fn edit_action(edit: PageEdit) -> Sel {
        match edit {
            PageEdit::Copy => sel!(copy:),
            PageEdit::Paste => sel!(paste:),
            PageEdit::Cut => sel!(cut:),
            PageEdit::SelectAll => sel!(selectAll:),
            PageEdit::Undo => sel!(undo:),
            PageEdit::Redo => sel!(redo:),
        }
    }

    /// The web view holding `window`'s keyboard focus (itself or a view
    /// inside it), if any.
    fn focused_page(window: &NSWindow) -> Option<Retained<WKWebView>> {
        page_containing(window.firstResponder()?.downcast::<NSView>().ok()?)
    }

    /// The web view that is `view` or holds it.
    fn page_containing(mut view: Retained<NSView>) -> Option<Retained<WKWebView>> {
        loop {
            match view.downcast::<WKWebView>() {
                Ok(page) => return Some(page),
                Err(other) => view = unsafe { other.superview() }?,
            }
        }
    }

    /// What the monitor needs of the page a mouse event lands on, if it
    /// lands on one of ours. Hidden pages aren't hit.
    struct PageHit {
        pane_focused: Rc<Cell<bool>>,
        gesture: Rc<Cell<Option<Instant>>>,
        events: Weak<NativeEvents>,
    }

    fn page_hit(event: &NSEvent) -> Option<PageHit> {
        let mtm = MainThreadMarker::new()?;
        let page = event
            .window(mtm)
            .and_then(|w| w.contentView())
            .and_then(|content| content.hitTest(event.locationInWindow()))
            .and_then(page_containing)?;
        page_entry(&page)
    }

    fn page_entry(page: &WKWebView) -> Option<PageHit> {
        let addr = page as *const WKWebView as usize;
        PAGES.with(|pages| {
            pages.borrow().iter().find(|p| p.view == addr).map(|p| PageHit {
                pane_focused: p.pane_focused.clone(),
                gesture: p.gesture.clone(),
                events: p.events.clone(),
            })
        })
    }

    /// A left mouse-down: when it lands on one of our pages, that page's tab
    /// takes gpui's focus (the click itself goes on to the page), and the
    /// page's last gesture is now.
    fn note_page_click(event: &NSEvent) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Some(entry) = page_hit(event) else {
            return;
        };
        entry.gesture.set(Some(Instant::now()));
        // Now, not when gpui's focus lands: a ⌘V right after the click must
        // already go to the page.
        entry.pane_focused.set(true);
        post_event(entry.events, mtm, |e| (e.on_click)());
    }

    /// Return, keypad Enter or Space while a page has the keyboard: the keys
    /// that activate a focused link or button, so the page's last gesture is
    /// now.
    fn note_page_key_gesture(event: &NSEvent) {
        const RETURN: u16 = 36;
        const SPACE: u16 = 49;
        const KEYPAD_ENTER: u16 = 76;
        if !matches!(event.keyCode(), RETURN | SPACE | KEYPAD_ENTER) {
            return;
        }
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        if let Some(entry) = event
            .window(mtm)
            .as_deref()
            .and_then(focused_page)
            .and_then(|page| page_entry(&page))
        {
            entry.gesture.set(Some(Instant::now()));
        }
    }

    /// A `WKWebView` living on top of the gpui window's view. Main thread
    /// only (it is neither `Send` nor `Sync`).
    pub(crate) struct NativeWebView {
        view: Retained<WKWebView>,
        /// Owned here: the web view only holds its delegates weakly.
        delegate: Retained<Delegate>,
        _events: Rc<NativeEvents>,
        /// gpui's focus is on this page's side of its tab (not its address
        /// bar); shared with the monitor through `PAGES`.
        pane_focused: Rc<Cell<bool>>,
        /// Bumped by every navigation the pane starts; shared with the
        /// delegate, which tags failures with it.
        navigation: Rc<Cell<u64>>,
    }

    impl NativeWebView {
        /// Adds a hidden web view to `window`'s view and starts loading `url`
        /// (nothing, for an empty `url`). Call `set_frame` and
        /// `set_visible(true)` to show it.
        pub(crate) fn new(window: &Window, url: &str, events: NativeEvents) -> anyhow::Result<Self> {
            let mtm = MainThreadMarker::new().context("web view created off the main thread")?;
            install_key_monitor();
            // Window has an inherent `window_handle()` (gpui's own handle
            // type); this is the raw-window-handle one.
            let handle = HasWindowHandle::window_handle(window)
                .map_err(|e| anyhow!("window has no native handle: {e}"))?;
            let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
                bail!("window is not an AppKit window");
            };
            // SAFETY: gpui hands out its live GPUIView, an NSView subclass,
            // valid while `window` is borrowed. We retain it for the call.
            let parent = unsafe { Retained::retain(appkit.ns_view.as_ptr().cast::<NSView>()) }
                .context("window handle has no view")?;

            let view = unsafe {
                let config = WKWebViewConfiguration::new(mtm);
                // One persistent store for every tab: cookies and storage
                // survive restarts and are shared across browser tabs.
                config.setWebsiteDataStore(&WKWebsiteDataStore::defaultDataStore(mtm));
                // Sites serve their full page only to an agent that names
                // Safari; WebKit appends this to its own agent.
                config.setApplicationNameForUserAgent(Some(&NSString::from_str(
                    user_agent_app_name(),
                )));
                // macOS defaults this to YES: any script could open windows
                // (each a new tab here) with no click. NO (iOS's default)
                // keeps `window.open` to a user gesture.
                config
                    .preferences()
                    .setJavaScriptCanOpenWindowsAutomatically(false);
                WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), NSRect::ZERO, &config)
            };
            // Safari's Web Inspector; macOS 13.3+ (we support 12).
            if view.respondsToSelector(sel!(setInspectable:)) {
                unsafe { view.setInspectable(true) };
            }
            view.setHidden(true);
            // Keep the top edge pinned while the window resizes, until the
            // next frame's set_frame lands; otherwise an unflipped parent
            // drags the page down with its bottom edge for that frame.
            view.setAutoresizingMask(if parent.isFlipped() {
                NSAutoresizingMaskOptions::ViewMaxYMargin
            } else {
                NSAutoresizingMaskOptions::ViewMinYMargin
            });

            let events = Rc::new(events);
            let gesture = Rc::new(Cell::new(None));
            let navigation = Rc::new(Cell::new(0));
            let delegate =
                Delegate::new(Rc::downgrade(&events), gesture.clone(), navigation.clone(), mtm);
            unsafe {
                view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*delegate)));
                view.setUIDelegate(Some(ProtocolObject::from_ref(&*delegate)));
                for key in KVO_KEYS {
                    view.addObserver_forKeyPath_options_context(
                        &delegate,
                        &NSString::from_str(key),
                        NSKeyValueObservingOptions::New,
                        kvo_context(),
                    );
                }
            }
            // Last subview, so it draws above gpui's Metal layer and AppKit
            // hit-testing reaches it first.
            parent.addSubview(&view);

            let pane_focused = Rc::new(Cell::new(false));
            PAGES.with(|pages| {
                pages.borrow_mut().push(PageEntry {
                    view: Retained::as_ptr(&view) as usize,
                    pane_focused: pane_focused.clone(),
                    gesture,
                    events: Rc::downgrade(&events),
                })
            });
            let this = Self { view, delegate, _events: events, pane_focused, navigation };
            if !url.is_empty() {
                this.load(url);
            }
            Ok(this)
        }

        /// Places the view at `bounds`, in gpui window coordinates (logical
        /// points, top-left origin), which are the parent view's points.
        /// `_window` is part of the interface; the parent's own bounds and
        /// `isFlipped` are what AppKit positions against, so those are used.
        pub(crate) fn set_frame(&self, bounds: Bounds<Pixels>, _window: &Window) {
            let Some(parent) = (unsafe { self.view.superview() }) else {
                return;
            };
            let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let y = if parent.isFlipped() {
                y
            } else {
                flip_y(y, h, parent.bounds().size.height as f32)
            };
            self.view.setFrame(NSRect::new(
                NSPoint::new(x as f64, y as f64),
                NSSize::new(w as f64, h as f64),
            ));
        }

        pub(crate) fn set_visible(&self, visible: bool) {
            if visible {
                self.view.setHidden(false);
            } else {
                self.hide();
            }
        }

        /// Hides the page, first handing the keyboard back to gpui's view if
        /// the page had it (left to AppKit, it goes to whatever the next key
        /// view is). Returns whether it had it, so it can be given back.
        pub(crate) fn hide(&self) -> bool {
            let had_focus = self.release_focus();
            self.view.setHidden(true);
            had_focus
        }

        /// Whether gpui's focus is on this page's side of its tab (in the
        /// tab, not its address bar): an input to the key monitor's
        /// [`route_key`]. Only a flag; giving up the first responder is
        /// [`Self::release_focus`].
        pub(crate) fn set_pane_focused(&self, focused: bool) {
            self.pane_focused.set(focused);
        }

        /// If the page (or anything in it) has the keyboard, give it to
        /// gpui's view, or gpui gets no keys until the user clicks.
        pub(crate) fn release_focus(&self) -> bool {
            let Some(window) = self.view.window() else {
                return false;
            };
            let page_has_focus = window
                .firstResponder()
                .and_then(|r| r.downcast::<NSView>().ok())
                .is_some_and(|v| v.isDescendantOf(&self.view));
            if page_has_focus {
                let parent = unsafe { self.view.superview() };
                window.makeFirstResponder(parent.as_deref().map(|p| &**p));
            }
            page_has_focus
        }

        /// The number of the navigation the pane last started here
        /// (`load`, `reload`, `back`, `forward` each bump it). `on_failed`
        /// carries the number current when WebKit reported the failure.
        pub(crate) fn navigation(&self) -> u64 {
            self.navigation.get()
        }

        fn begin_navigation(&self) {
            self.navigation.set(self.navigation.get().wrapping_add(1));
        }

        /// Loads an absolute URL. One NSURL can't parse is reported through
        /// `on_failed`.
        pub(crate) fn load(&self, url: &str) {
            self.begin_navigation();
            match NSURL::URLWithString(&NSString::from_str(url)) {
                Some(ns_url) => unsafe {
                    self.view.loadRequest(&NSURLRequest::requestWithURL(&ns_url));
                },
                None => {
                    let url = url.to_string();
                    let navigation = self.navigation();
                    self.delegate.post(move |e| {
                        (e.on_failed)(Some(url), "not a valid address".to_string(), navigation)
                    });
                }
            }
        }

        pub(crate) fn back(&self) {
            self.begin_navigation();
            unsafe { self.view.goBack() };
        }

        pub(crate) fn forward(&self) {
            self.begin_navigation();
            unsafe { self.view.goForward() };
        }

        pub(crate) fn reload(&self) {
            self.begin_navigation();
            unsafe { self.view.reload() };
        }

        pub(crate) fn stop(&self) {
            unsafe { self.view.stopLoading() };
        }

        /// Gives the page keyboard focus.
        pub(crate) fn focus(&self) {
            if let Some(window) = self.view.window() {
                window.makeFirstResponder(Some(&self.view));
            }
        }

        pub(crate) fn can_go_back(&self) -> bool {
            unsafe { self.view.canGoBack() }
        }

        pub(crate) fn can_go_forward(&self) -> bool {
            unsafe { self.view.canGoForward() }
        }
    }

    /// The window is minimized or the app is hidden (⌘H), as opposed to
    /// merely covered by another window or on another Space: gpui reports
    /// all of those as one `WindowVisibility::Hidden`.
    pub(crate) fn window_is_minimized_or_hidden(window: &Window) -> bool {
        let Some(mtm) = MainThreadMarker::new() else {
            return false;
        };
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return false;
        };
        let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
            return false;
        };
        // SAFETY: as in `NativeWebView::new`, gpui's live view, retained for
        // the call.
        let Some(view) = (unsafe { Retained::retain(appkit.ns_view.as_ptr().cast::<NSView>()) })
        else {
            return false;
        };
        view.window().is_some_and(|w| w.isMiniaturized())
            || NSApplication::sharedApplication(mtm).isHidden()
    }

    impl Drop for NativeWebView {
        fn drop(&mut self) {
            unsafe {
                for key in KVO_KEYS {
                    self.view.removeObserver_forKeyPath_context(
                        &self.delegate,
                        &NSString::from_str(key),
                        kvo_context(),
                    );
                }
                self.view.setNavigationDelegate(None);
                self.view.setUIDelegate(None);
                self.view.stopLoading();
            }
            // The view goes away on every tab switch; the keyboard goes back
            // to gpui's view first.
            self.release_focus();
            let addr = Retained::as_ptr(&self.view) as usize;
            PAGES.with(|pages| pages.borrow_mut().retain(|p| p.view != addr));
            self.view.removeFromSuperview();
        }
    }

    #[cfg(test)]
    mod tests {
        use objc2::ClassType as _;

        use super::Delegate;

        /// Registering the class runs objc2's debug-build checks: the KVO
        /// override's type encoding must match NSObject's, and every delegate
        /// selector must exist in its protocol. A typo or a wrong
        /// `observeValueForKeyPath:` signature fails here, not in the app.
        #[test]
        #[cfg(debug_assertions)]
        fn delegate_class_registers() {
            assert_eq!(Delegate::class().name().to_str(), Ok("HarnessWebViewDelegate"));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    #[test]
    #[cfg(debug_assertions)]
    fn native_events_build_and_flip_y() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let log = seen.clone();
        let events = NativeEvents {
            on_title: Box::new(move |t| log.borrow_mut().push(format!("title {t}"))),
            ..NativeEvents::default()
        };
        (events.on_title)("Home".into());
        (events.on_url)("http://localhost:3000/".into());
        (events.on_loading)(true);
        (events.on_failed)(None, "Could not connect to the server.".into(), 0);
        (events.on_notice)(DOWNLOAD_UNSUPPORTED.into());
        (events.on_new_window)("http://localhost:3000/new".into());
        assert_eq!(*seen.borrow(), ["title Home"]);

        // A 100pt-tall rect at the top of an 800pt view sits at y=700 in
        // AppKit's bottom-left coordinates.
        assert_eq!(flip_y(0.0, 100.0, 800.0), 700.0);
        // ...and one flush with the bottom sits at y=0.
        assert_eq!(flip_y(700.0, 100.0, 800.0), 0.0);
        // A tab body below a 40pt strip, 20pt above the window bottom.
        assert_eq!(flip_y(40.0, 540.5, 600.5), 20.0);
    }

    /// A link the user clicked in the top-level page.
    const CLICK: NavOrigin = NavOrigin { user_initiated: true, from_main_frame: true };
    /// `location = …` / a redirect / our own load, from the top-level page.
    const SCRIPT: NavOrigin = NavOrigin { user_initiated: false, from_main_frame: true };
    /// A link the user clicked inside an iframe (even one targeting `_top`).
    const SUBFRAME_CLICK: NavOrigin = NavOrigin { user_initiated: true, from_main_frame: false };

    #[test]
    fn web_schemes_load_in_the_page() {
        for u in [
            "http://localhost:3000/",
            "https://example.com/x",
            "about:blank",
            "about:srcdoc",
            "data:text/html,hi",
            "blob:http://localhost:3000/1f2e",
        ] {
            for origin in [CLICK, SCRIPT, SUBFRAME_CLICK] {
                assert_eq!(decide_navigation(u, true, origin), NavDecision::Load, "{u}");
                assert_eq!(decide_navigation(u, false, origin), NavDecision::Load, "{u}");
            }
        }
    }

    #[test]
    fn app_schemes_leave_only_on_a_main_frame_click() {
        for u in ["mailto:a@b.c", "zed://file/x", "vscode://x", "slack://open", "tel:123"] {
            assert_eq!(decide_navigation(u, true, CLICK), NavDecision::External, "{u}");
            // Script `location = …` or a redirect the user didn't click.
            assert_eq!(decide_navigation(u, true, SCRIPT), NavDecision::Ignore, "{u}");
            // A click inside an iframe, including one that navigates `top`.
            assert_eq!(decide_navigation(u, true, SUBFRAME_CLICK), NavDecision::Ignore, "{u}");
            // A main-frame click that targets a subframe.
            assert_eq!(decide_navigation(u, false, CLICK), NavDecision::Ignore, "{u}");
        }
    }

    #[test]
    fn file_javascript_and_unknown_never_leave() {
        for u in ["file:///etc/hosts", "file:///Applications/Calc.app", "javascript:alert(1)", ""] {
            for origin in [CLICK, SCRIPT, SUBFRAME_CLICK] {
                assert_eq!(decide_navigation(u, true, origin), NavDecision::Ignore, "{u}");
            }
        }
    }

    #[test]
    fn new_windows_open_web_pages_only() {
        assert_eq!(decide_new_window("http://localhost:3000/a", CLICK), NavDecision::Load);
        // window.open() from script still gets a tab: a click handler's
        // window.open isn't `LinkActivated`. What keeps a script from opening
        // windows with no gesture is WebKit, with
        // `javaScriptCanOpenWindowsAutomatically` off (`NativeWebView::new`),
        // plus the pane's one-tab-per-second limit (`new_window_allowed`).
        assert_eq!(decide_new_window("https://example.com", SCRIPT), NavDecision::Load);
        assert_eq!(decide_new_window("mailto:a@b.c", CLICK), NavDecision::External);
        assert_eq!(decide_new_window("mailto:a@b.c", SCRIPT), NavDecision::Ignore);
        assert_eq!(decide_new_window("mailto:a@b.c", SUBFRAME_CLICK), NavDecision::Ignore);
        assert_eq!(decide_new_window("file:///etc/hosts", CLICK), NavDecision::Ignore);
        assert_eq!(decide_new_window("", CLICK), NavDecision::Ignore);
        assert_eq!(decide_new_window("about:blank", CLICK), NavDecision::Ignore);
        assert_eq!(decide_new_window("data:text/html,hi", CLICK), NavDecision::Ignore);
    }

    #[test]
    fn only_clicked_main_frame_web_downloads_leave_the_app() {
        assert_eq!(decide_download("https://example.com/a.zip", CLICK), NavDecision::External);
        assert_eq!(decide_download("http://localhost:3000/dl", CLICK), NavDecision::External);
        // Subframe downloads (hidden iframes, attachment responses) stay in.
        assert_eq!(decide_download("https://example.com/a.zip", SUBFRAME_CLICK), NavDecision::Ignore);
        assert_eq!(decide_download("blob:http://localhost:3000/1f2e", SUBFRAME_CLICK), NavDecision::Ignore);
        // A script-started download in the main frame.
        assert_eq!(decide_download("https://example.com/a.zip", SCRIPT), NavDecision::Ignore);
        // Page-generated: nothing outside the page can fetch these; tell the user.
        assert_eq!(decide_download("blob:http://localhost:3000/1f2e", CLICK), NavDecision::Unsupported);
        assert_eq!(decide_download("data:application/json,{}", CLICK), NavDecision::Unsupported);
        assert_eq!(decide_download("file:///etc/hosts", CLICK), NavDecision::Unsupported);
        assert_eq!(decide_download("", CLICK), NavDecision::Unsupported);
    }

    #[test]
    fn a_download_response_goes_only_if_the_user_started_its_navigation() {
        let t0 = Instant::now();
        let url = "https://example.com/report.csv";
        // The address bar, a restore or a routed link: no source frame.
        assert!(user_started(true, None, t0));
        // A link, a form's button, or a script answering the click.
        assert!(user_started(false, Some(t0), t0 + Duration::from_millis(300)));
        // A script on its own: no gesture, or one long before.
        assert!(!user_started(false, None, t0));
        assert!(!user_started(false, Some(t0), t0 + GESTURE_WINDOW + Duration::from_millis(1)));

        let started = response_origin(true, true);
        assert_eq!(decide_download(url, started), NavDecision::External);
        // A script's `location = "/report.csv"` with no gesture.
        assert_eq!(decide_download(url, response_origin(true, false)), NavDecision::Ignore);
        // A subframe's attachment, even after a click.
        assert_eq!(decide_download(url, response_origin(false, true)), NavDecision::Ignore);
        assert!(!response_origin(false, true).user_initiated);
    }

    #[test]
    fn a_link_activation_is_a_click_only_right_after_a_real_gesture() {
        let t0 = Instant::now();
        // A real click or Return on a focused link, then WebKit's activation.
        assert!(user_clicked(true, Some(t0), t0));
        assert!(user_clicked(true, Some(t0), t0 + Duration::from_millis(300)));
        assert!(user_clicked(true, Some(t0), t0 + GESTURE_WINDOW));
        // A script's `a.click()` with no gesture, or long after the last one.
        assert!(!user_clicked(true, None, t0));
        assert!(!user_clicked(true, Some(t0), t0 + GESTURE_WINDOW + Duration::from_millis(1)));
        // Not a link activation (a script's `location = …`, a reload), even
        // right after a click.
        assert!(!user_clicked(false, Some(t0), t0));
        // A clock that reads earlier than the gesture never panics.
        assert!(user_clicked(true, Some(t0 + Duration::from_millis(5)), t0));
    }

    #[test]
    fn downloads_are_unshowable_or_attachments() {
        assert!(is_download(false, None));
        assert!(is_download(true, Some("attachment; filename=\"a.zip\"")));
        assert!(is_download(true, Some(" Attachment")));
        assert!(!is_download(true, None));
        assert!(!is_download(true, Some("inline")));
        assert!(!is_download(true, Some("attach")));
    }

    #[test]
    fn user_agent_names_safari_like_safari_does() {
        assert_eq!(safari_app_name(Some("27.3")), "Version/27.3 Safari/605.1.15");
        // Safari's own token carries major.minor only.
        assert_eq!(safari_app_name(Some("18.6.1")), "Version/18.6 Safari/605.1.15");
        assert_eq!(safari_app_name(Some("26")), "Version/26 Safari/605.1.15");
        // No Safari, or a version we can't read: the fallback.
        let fallback = format!("Version/{SAFARI_FALLBACK_VERSION} Safari/605.1.15");
        assert_eq!(safari_app_name(None), fallback);
        assert_eq!(safari_app_name(Some("")), fallback);
        assert_eq!(safari_app_name(Some("beta 3")), fallback);
    }

    #[test]
    fn key_events_name_keys_the_way_the_catalog_does() {
        // US layout, ⌘ applied: the character on the key.
        assert_eq!(chord_key("t", 17).as_deref(), Some("t"));
        assert_eq!(chord_key("T", 17).as_deref(), Some("t"));
        assert_eq!(chord_key("]", 30).as_deref(), Some("]"));
        assert_eq!(chord_key(",", 43).as_deref(), Some(","));
        assert_eq!(chord_key("1", 18).as_deref(), Some("1"));
        assert_eq!(chord_key("\u{f701}", 125).as_deref(), Some("down"));
        assert_eq!(chord_key("\u{f700}", 126).as_deref(), Some("up"));
        assert_eq!(chord_key("\u{f702}", 123).as_deref(), Some("left"));
        assert_eq!(chord_key("\r", 36).as_deref(), Some("enter"));
        // A layout with no ASCII on the key (Russian ⌘е): the US key.
        assert_eq!(chord_key("е", 17).as_deref(), Some("t"));
        assert_eq!(chord_key("х", 33).as_deref(), Some("["));
        assert_eq!(chord_key("", 13).as_deref(), Some("w"));
        // Neither: unknown, so not an app shortcut.
        assert_eq!(chord_key("é", 200), None);
    }

    #[test]
    fn shifted_punctuation_matches_both_ways_a_chord_is_written() {
        use crate::shortcuts::{Chord, is_app_shortcut};
        // ⌘⇧] on a US layout: "]" with ⌘ alone, "}" with ⌘⇧.
        let pressed = event_chords("]", "}", 30, true, true, false, false);
        // The catalog writes it `cmd-shift-]`; a chord rebound by pressing
        // it is stored the way gpui reads keys, `cmd-}`.
        for stored in ["cmd-shift-]", "cmd-}"] {
            let app = [Chord::parse(stored).unwrap()];
            assert!(pressed.iter().any(|c| is_app_shortcut(c, &app)), "{stored}");
        }
        // ⌘⇧7 rebound by pressing it is stored as `cmd-&`.
        let app = [Chord::parse("cmd-&").unwrap()];
        let pressed = event_chords("7", "&", 26, true, true, false, false);
        assert!(pressed.iter().any(|c| is_app_shortcut(c, &app)));
        // Letters keep shift in both namings: one chord, ⌘⇧Z is still redo.
        let pressed = event_chords("z", "Z", 6, true, true, false, false);
        assert_eq!(pressed, [Chord::new("z", true, true, false, false)]);
        // No shift, one chord.
        assert_eq!(event_chords("t", "T", 17, true, false, false, false).len(), 1);
    }

    #[test]
    fn only_a_focused_pages_own_chords_go_to_the_page() {
        // (page_has_keyboard, page_has_gpui_focus, command, app_chord) -> route.
        // The second input is false with gpui's focus in the tab's address
        // bar as well as outside the tab (`gpui_focus_on_page` in mod.rs).
        use KeyRoute::{AsUsual, Page};
        let table = [
            ((false, false, false, false), AsUsual),
            ((false, false, false, true), AsUsual),
            ((false, false, true, false), AsUsual),
            ((false, false, true, true), AsUsual),
            ((false, true, false, false), AsUsual),
            ((false, true, false, true), AsUsual),
            ((false, true, true, false), AsUsual),
            ((false, true, true, true), AsUsual),
            ((true, false, false, false), AsUsual),
            ((true, false, false, true), AsUsual),
            // The page kept AppKit's first responder after gpui's focus left
            // the tab (⌘⇧F into the tools panel) or moved to the address bar
            // (a click on it): the panel or the address bar gets its keys.
            ((true, false, true, false), AsUsual),
            ((true, false, true, true), AsUsual),
            // Typing without ⌘ never passes through the monitor's routing.
            ((true, true, false, false), AsUsual),
            ((true, true, false, true), AsUsual),
            ((true, true, true, false), Page),
            // An app chord reaches gpui even over a focused page.
            ((true, true, true, true), AsUsual),
        ];
        for ((page, pane, command, app), route) in table {
            assert_eq!(
                route_key(page, pane, command, app),
                route,
                "page={page} pane={pane} command={command} app={app}"
            );
        }
    }

    #[test]
    fn cancellations_are_not_failures() {
        assert!(is_benign_failure("NSURLErrorDomain", -999));
        assert!(is_benign_failure("WebKitErrorDomain", 102));
        assert!(is_benign_failure("WebKitErrorDomain", 204));
        // Connection refused: the dev server isn't up yet. A real failure.
        assert!(!is_benign_failure("NSURLErrorDomain", -1004));
        assert!(!is_benign_failure("WebKitErrorDomain", -999));
    }
}
