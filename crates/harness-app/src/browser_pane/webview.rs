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
//!
//! **The conductor shows through the page** (Task 11). The page sits above
//! everything gpui draws, so the always-on conductor would vanish behind it
//! wherever they overlap, and hiding the page for him would blank it
//! constantly (he talks all the time). Instead the page is cut away where he
//! is. Each page lives in a container `NSView` of ours (`PageContainer`): the
//! container is what is added to gpui's view, framed, shown and hidden, and
//! the `WKWebView` fills it, so WebKit's own layers are never touched.
//! - **Rects.** Each frame, the conductor's wrapper element reports the laid
//!   out bounds of his figure and his bubble or badge (gpui's
//!   `on_children_prepainted`) through [`set_punch_through`], in gpui window
//!   coordinates, per window; no conductor reports none. He prepaints after
//!   the page's slot, so a changed set recuts that window's pages right
//!   then, and `set_frame` recuts a page that moved from the last set.
//!   [`holes_in_page`] subtracts the page's origin (the same top-left
//!   window coordinates `set_frame` takes; the container is flipped, so
//!   those are its own) and drops holes that miss the page: no holes, no
//!   mask, as before.
//! - **Mask.** The container's layer gets a `CAShapeLayer` mask: its bounds
//!   with the holes cut out by the even-odd rule, rounded like the bubble
//!   and badge, converted with `convertRectToLayer:` rather than a guess at
//!   the layer's flip. It is redone only when [`mask_changed`] says so, with
//!   implicit animations off.
//! - **Pointer.** The container's `hitTest:` answers nil inside a hole
//!   ([`in_any_hole`]), so AppKit hands a click, drag or scroll there to
//!   gpui's view and he can be dragged or clicked over the page. The monitor's
//!   `page_hit` asks this same hit test, so a click in a hole never counts as
//!   a page click (no gesture stamp, no focus move to the tab). `hitTest:`
//!   doesn't steer tracking areas, so WebKit may still see the mouse move
//!   over a hole (its hover effects, its cursor).
//!
//! The holes are rects: around his figure, a square sprite, the tab's own
//! background shows rather than the page.
//!
//! **Developer tools: WebKit's Web Inspector, through private API** (Task
//! 12). WebKit has no public API to open the inspector or to offer Inspect
//! Element, so two private pieces are used, each found at runtime:
//! - **Inspect Element** in the page's context menu: the WKPreferences key
//!   `developerExtrasEnabled`, set through KVC (`setValue:forKey:`, which
//!   finds `_setDeveloperExtrasEnabled:`) on the configuration's
//!   preferences before the view is made, as WebKit-based apps commonly do.
//!   KVC raises `NSUndefinedKeyException` for an unknown key, and an ObjC
//!   exception must not unwind through Rust, so the key is validated
//!   instead of caught: it is set only if the preferences answer
//!   `_setDeveloperExtrasEnabled:` or `setDeveloperExtrasEnabled:` (KVC's
//!   setter search; [`developer_extras_settable`]).
//! - **The address bar's Developer tools button**: `-[WKWebView _inspector]`
//!   returns the page's `_WKInspector`, whose `show`, `close`, `isVisible`
//!   (and, optionally, `isConnected`, `inspectorWebView`, `detach`) were
//!   found by introspecting the runtime on macOS 27.3, not guessed.
//!   [`inspector_available`] checks once, on the classes, that each needed
//!   method exists with the shape we send it (no arguments; returns
//!   nothing, a BOOL or an object: [`has_method`]); every call checks its
//!   method again on the object it is sent to. Missing anything, the button
//!   hides and the app logs it once.
//!
//! Acceptable because the app ships as a DMG and through Homebrew, not the
//! Mac App Store, whose review rejects private API. It degrades rather than
//! breaks: on a macOS that renamed or dropped any of it, Inspect Element
//! and/or the button are simply absent; no message is ever sent that the
//! object doesn't answer, so nothing crashes or throws. `isInspectable`
//! stays on, so Safari's Develop menu still attaches either way.
//!
//! The inspector is the page's: dropping the view (the tab hidden or
//! closed, see `mod.rs`) closes it first, and the page shown again starts
//! with it closed (it doesn't reopen on its own). WebKit may dock a new
//! inspector beside the page (its preference "starts attached"); the pane
//! moves one it docked into its own window once, as it opens
//! ([`settle_inspector`]), and leaves alone one the user docks later from
//! the inspector's toolbar. Docked, it is native and above gpui like the
//! page, next to it in the page's container. The key monitor acts
//! only on events inside a page of ours ([`registered_page`]); the
//! inspector is a `WKWebView` but not registered, so its keys and clicks
//! go on untouched, in its own window or docked.

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

/// Which view of `chain` is one of our live pages, if any: `chain` is a view
/// and its superviews, nearest first (by address), `pages` the registered
/// pages' web views. The key monitor acts only on an event whose
/// first responder (a key) or hit view (a click) is inside one of our
/// pages. A Web Inspector is a `WKWebView` too, in its own window or docked
/// beside the page in the same container, but it is not registered, so its
/// keys and clicks go on untouched: never to a page, never routed to gpui.
pub(crate) fn registered_page(chain: &[usize], pages: &[usize]) -> Option<usize> {
    chain.iter().position(|view| pages.contains(view))
}

/// Where a page's Web Inspector is, as far as AppKit can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InspectorPlace {
    /// WebKit can't say (`inspectorWebView` is missing).
    Unsupported,
    /// Its view isn't in a window yet.
    NotPlaced,
    /// Docked in the page's own window, beside the page.
    Docked,
    /// In a window of its own.
    OwnWindow,
}

/// What to do with an inspector that just opened (`pending`) and is at
/// `place`: whether to detach it into its own window, and whether to ask
/// again next time. WebKit docks a new inspector inside the page's area by
/// default; its own window is preferred, so one WebKit docked is detached,
/// once. Nothing pending is never touched, so a user who docks it from the
/// inspector's own toolbar keeps it docked until it closes.
pub(crate) fn settle_inspector(pending: bool, place: InspectorPlace) -> (bool, bool) {
    match (pending, place) {
        (true, InspectorPlace::Docked) => (true, false),
        (true, InspectorPlace::NotPlaced) => (false, true),
        _ => (false, false),
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
    /// committed (`didCommitNavigation`; `false`), or the URL changed in
    /// place with nothing loading (`pushState`, a `#fragment`; `true`, the
    /// same document, whose title is still its own). What browsing history
    /// records.
    pub on_commit: Box<dyn Fn(String, bool)>,
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
            on_commit: Box::new(|_, _| {}),
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

/// A rect the page is cut away from so gpui's drawing under it shows (and
/// takes the pointer) there: the conductor's figure, his bubble or badge.
/// In gpui window coordinates (logical points, top-left origin) as
/// [`set_punch_through`] takes them; [`holes_in_page`] makes them
/// page-local. `radius` rounds the corners, as the element draws them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Hole {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub radius: f32,
}

impl Hole {
    pub(crate) fn from_bounds(bounds: gpui::Bounds<gpui::Pixels>, radius: f32) -> Self {
        Self {
            x: f32::from(bounds.origin.x),
            y: f32::from(bounds.origin.y),
            w: f32::from(bounds.size.width),
            h: f32::from(bounds.size.height),
            radius,
        }
    }
}

/// The window's `holes` that touch a page placed at `page` (x, y, width,
/// height, in the same window coordinates `set_frame` takes), moved into
/// the page's own top-left coordinates. Holes that miss the page or have
/// no area are dropped, so a conductor off the page means no mask at all.
/// Corner radii are clamped to half the shorter side (CoreGraphics refuses
/// more). Holes are not clipped to the page: the mask's even-odd fill only
/// matters inside it.
pub(crate) fn holes_in_page(holes: &[Hole], page: (f32, f32, f32, f32)) -> Vec<Hole> {
    let (px, py, pw, ph) = page;
    holes
        .iter()
        .map(|h| Hole {
            x: h.x - px,
            y: h.y - py,
            radius: h.radius.min(h.w / 2.0).min(h.h / 2.0).max(0.0),
            ..*h
        })
        .filter(|h| h.w > 0.0 && h.h > 0.0 && h.x < pw && h.x + h.w > 0.0 && h.y < ph && h.y + h.h > 0.0)
        .collect()
}

/// Whether the page-local point (`x`, `y`, top-left origin) falls in one
/// of the page-local `holes`, rounded corners included: the container's
/// `hitTest:` answers nil there, so the click goes to gpui.
pub(crate) fn in_any_hole(holes: &[Hole], x: f32, y: f32) -> bool {
    holes.iter().any(|h| {
        if x < h.x || x >= h.x + h.w || y < h.y || y >= h.y + h.h {
            return false;
        }
        // Inside the rect; in a corner square, it must be inside the arc.
        let r = h.radius.min(h.w / 2.0).min(h.h / 2.0).max(0.0);
        let cx = x.clamp(h.x + r, h.x + h.w - r);
        let cy = y.clamp(h.y + r, h.y + h.h - r);
        (x - cx).powi(2) + (y - cy).powi(2) <= r * r
    })
}

/// What a page's mask cuts: its size and its page-local holes. `None` is
/// no mask (no holes).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PageMask {
    pub width: f32,
    pub height: f32,
    pub holes: Vec<Hole>,
}

/// The mask for a page of `width` × `height` with page-local `holes`.
pub(crate) fn page_mask(holes: Vec<Hole>, width: f32, height: f32) -> Option<PageMask> {
    (!holes.is_empty()).then_some(PageMask { width, height, holes })
}

/// Whether the applied mask must be redone for `next`: a hole appeared,
/// went or moved, or the page changed size (the mask's outer rect is the
/// page's bounds). Sub-hundredth-of-a-point float noise is no change, so a
/// resting conductor never touches the layer.
pub(crate) fn mask_changed(applied: Option<&PageMask>, next: Option<&PageMask>) -> bool {
    const EPSILON: f32 = 0.01;
    let near = |a: f32, b: f32| (a - b).abs() < EPSILON;
    match (applied, next) {
        (None, None) => false,
        (Some(a), Some(b)) => {
            !(near(a.width, b.width)
                && near(a.height, b.height)
                && a.holes.len() == b.holes.len()
                && a.holes.iter().zip(&b.holes).all(|(p, q)| {
                    near(p.x, q.x)
                        && near(p.y, q.y)
                        && near(p.w, q.w)
                        && near(p.h, q.h)
                        && near(p.radius, q.radius)
                }))
        }
        _ => true,
    }
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
pub(crate) use mac::{
    NativeWebView, inspector_available, set_punch_through, window_is_minimized_or_hidden,
};

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
    use objc2::runtime::{AnyClass, AnyObject, NSObject, ProtocolObject, Sel};
    use objc2::{
        ClassType as _, DefinedClass, MainThreadMarker, MainThreadOnly, Message as _, define_class, msg_send, sel,
    };
    use objc2_app_kit::{
        NSApplication, NSAutoresizingMaskOptions, NSEvent, NSEventMask, NSEventModifierFlags,
        NSEventType, NSResponder, NSView, NSWindow,
    };
    use objc2_core_graphics::CGMutablePath;
    use objc2_foundation::{
        NSBundle, NSDictionary, NSError, NSHTTPURLResponse, NSKeyValueChangeKey,
        NSKeyValueObservingOptions, NSNumber, NSObjectNSKeyValueCoding,
        NSObjectNSKeyValueObserverRegistration, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
        NSURL, NSURLRequest, ns_string,
    };
    use objc2_quartz_core::{CAShapeLayer, CATransaction, kCAFillRuleEvenOdd};
    use objc2_web_kit::{
        WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate,
        WKNavigationResponse, WKNavigationResponsePolicy, WKNavigationType, WKPreferences,
        WKUIDelegate, WKWebView, WKWebViewConfiguration, WKWebsiteDataStore, WKWindowFeatures,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::{
        APP_CHORDS, DOWNLOAD_UNSUPPORTED, Hole, InspectorPlace, KeyRoute, NavDecision, NavOrigin,
        NativeEvents, PageMask, decide_download, decide_navigation, decide_new_window, event_chords, flip_y,
        holes_in_page, in_any_hole, is_benign_failure, is_download, mask_changed, page_mask,
        registered_page, response_origin, route_key, safari_app_name, user_clicked, user_started,
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

    /// The private WKPreferences key behind right-click → Inspect Element
    /// (see the module docs). KVC finds it through `_setDeveloperExtrasEnabled:`.
    const DEVELOPER_EXTRAS_KEY: &str = "developerExtrasEnabled";

    /// Whether KVC can set [`DEVELOPER_EXTRAS_KEY`] without raising: KVC's
    /// setter search (`set<Key>:`, then `_set<Key>:`) finds a method. The
    /// ivar fallback is never relied on, so an unknown key can't reach
    /// `setValue:forUndefinedKey:` (which raises `NSUndefinedKeyException`).
    fn developer_extras_settable(responds: impl Fn(Sel) -> bool) -> bool {
        responds(sel!(_setDeveloperExtrasEnabled:)) || responds(sel!(setDeveloperExtrasEnabled:))
    }

    /// Turns on WebKit's developer extras (the context menu's Inspect
    /// Element, and what `_WKInspector` needs to connect) for a web view
    /// about to be made with `prefs`. Validated first; skipped (and logged
    /// once) if this WebKit lacks it.
    fn enable_developer_extras(prefs: &WKPreferences) {
        if !developer_extras_settable(|s| prefs.respondsToSelector(s)) {
            log_devtools_missing("WKPreferences developerExtrasEnabled");
            return;
        }
        // SAFETY: the key resolves to a BOOL setter (checked above), and KVC
        // unboxes the NSNumber for it.
        unsafe {
            prefs.setValue_forKey(
                Some(&NSNumber::new_bool(true)),
                &NSString::from_str(DEVELOPER_EXTRAS_KEY),
            );
        }
    }

    /// The first private piece of the inspector API this WebKit lacks, if
    /// any. Asked of the classes, so no web view is needed.
    fn devtools_missing() -> Option<&'static str> {
        if !developer_extras_settable(|s| WKPreferences::class().responds_to(s)) {
            return Some("WKPreferences developerExtrasEnabled");
        }
        if !WKWebView::class().responds_to(sel!(_inspector)) {
            return Some("-[WKWebView _inspector]");
        }
        if !has_method(WKWebView::class(), sel!(_inspector), Returns::Object) {
            return Some("-[WKWebView _inspector] returning an object");
        }
        let Some(inspector) = AnyClass::get(c"_WKInspector") else {
            return Some("_WKInspector");
        };
        [
            (sel!(show), Returns::Nothing, "-[_WKInspector show]"),
            (sel!(close), Returns::Nothing, "-[_WKInspector close]"),
            (sel!(isVisible), Returns::Bool, "-[_WKInspector isVisible]"),
        ]
        .into_iter()
        .find(|(sel, returns, _)| !has_method(inspector, *sel, *returns))
        .map(|(_, _, name)| name)
    }

    /// What a private no-argument method returns, as we send it.
    #[derive(Clone, Copy)]
    enum Returns {
        Nothing,
        Bool,
        Object,
    }

    /// Whether `class`'s instances have `sel` with the shape we send it: no
    /// arguments (beyond self and _cmd), returning `returns`. Checked before
    /// every private message, so a WebKit that changed one is skipped rather
    /// than sent a message of the wrong type (which objc2 turns into a panic
    /// in debug builds, and undefined behaviour otherwise).
    fn has_method(class: &AnyClass, sel: Sel, returns: Returns) -> bool {
        class.instance_method(sel).is_some_and(|m| {
            let ret = m.return_type();
            let ok = match returns {
                Returns::Nothing => ret.to_bytes() == b"v",
                // BOOL is `B` on arm64 and `c` (signed char) on x86_64.
                Returns::Bool => matches!(ret.to_bytes(), b"B" | b"c"),
                Returns::Object => ret.to_bytes() == b"@",
            };
            ok && m.arguments_count() == 2
        })
    }

    /// Logs, once per process, that the developer tools are off for want of
    /// `what`.
    fn log_devtools_missing(what: &str) {
        static LOGGED: std::sync::Once = std::sync::Once::new();
        LOGGED.call_once(|| {
            log::warn!("browser: no developer tools; this WebKit has no {what}");
        });
    }

    /// Whether this WebKit has the private inspector API the Developer tools
    /// button needs (see the module docs); checked once. Without it the
    /// button hides, and Safari's Develop menu (`isInspectable`) remains.
    pub(crate) fn inspector_available() -> bool {
        static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *AVAILABLE.get_or_init(|| match devtools_missing() {
            Some(what) => {
                log_devtools_missing(what);
                false
            }
            None => true,
        })
    }

    /// Whether `object` answers `sel` with the shape we send it
    /// ([`has_method`]): every private message below is sent only after
    /// this says yes.
    fn responds(object: &AnyObject, sel: Sel, returns: Returns) -> bool {
        has_method(object.class(), sel, returns)
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
                            // Posted after `on_url`, so the pane already
                            // has the new URL when it hears of the commit.
                            let same_page = !unsafe { view.isLoading() };
                            let committed = url.clone();
                            self.post(move |e| (e.on_url)(url));
                            if same_page {
                                self.post(move |e| (e.on_commit)(committed, true));
                            }
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
                    self.post(move |e| (e.on_commit)(url, false));
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
        /// The view holding the page, which carries its punch-through mask.
        container: Retained<PageContainer>,
    }

    thread_local! {
        static PAGES: RefCell<Vec<PageEntry>> = const { RefCell::new(Vec::new()) };
        /// Each window's punch-through holes ([`set_punch_through`]), by
        /// the address of its gpui view. A window with none has no entry.
        static HOLES: RefCell<Vec<(usize, Vec<Hole>)>> = const { RefCell::new(Vec::new()) };
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

    /// The page of ours holding `window`'s keyboard focus (itself or a view
    /// inside it), if any. A Web Inspector holding it is no page of ours
    /// ([`registered_page`]).
    fn focused_page(window: &NSWindow) -> Option<Retained<WKWebView>> {
        our_page(window.firstResponder()?.downcast::<NSView>().ok()?)
    }

    /// The page of ours that is `view` or holds it ([`registered_page`]).
    fn our_page(view: Retained<NSView>) -> Option<Retained<WKWebView>> {
        let mut chain = vec![view];
        while let Some(up) = chain.last().and_then(|v| unsafe { v.superview() }) {
            chain.push(up);
        }
        let addrs: Vec<usize> = chain.iter().map(|v| Retained::as_ptr(v) as usize).collect();
        let pages: Vec<usize> = PAGES.with(|p| p.borrow().iter().map(|e| e.view).collect());
        let at = registered_page(&addrs, &pages)?;
        chain.swap_remove(at).downcast::<WKWebView>().ok()
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
            .and_then(our_page)?;
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

    /// The page container's state.
    struct ContainerIvars {
        /// Where `set_frame` last put the page, in gpui window coordinates
        /// (x, y, width, height); `None` before its first frame.
        page: Cell<Option<(f32, f32, f32, f32)>>,
        /// The mask in place, page-local; `hitTest:` reads its holes too, so
        /// clicks fall through exactly where the page is cut away. `None`:
        /// no mask.
        mask: RefCell<Option<PageMask>>,
    }

    define_class!(
        // The page's container (see the module docs): a plain layer-backed
        // view whose only subview is the WKWebView, so the mask and the hit
        // test are ours and WebKit's own layers stay untouched. Flipped, so
        // its coordinates are the page-local top-left ones the holes use.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "HarnessPageContainer"]
        #[ivars = ContainerIvars]
        struct PageContainer;

        impl PageContainer {
            #[unsafe(method(isFlipped))]
            fn is_flipped(&self) -> bool {
                true
            }

            /// Nothing here inside a hole, so AppKit goes on to gpui's view
            /// under it: the conductor gets the click or drag. The key
            /// monitor's `page_hit` asks this same hit test, so it never
            /// counts such a click as the page's.
            #[unsafe(method_id(hitTest:))]
            fn hit_test(&self, point: NSPoint) -> Option<Retained<NSView>> {
                // `point` is in the superview's coordinates.
                let parent = unsafe { self.superview() };
                let local = self.convertPoint_fromView(point, parent.as_deref());
                let in_hole = self
                    .ivars()
                    .mask
                    .borrow()
                    .as_ref()
                    .is_some_and(|m| in_any_hole(&m.holes, local.x as f32, local.y as f32));
                if in_hole {
                    None
                } else {
                    unsafe { msg_send![super(self), hitTest: point] }
                }
            }
        }
    );

    impl PageContainer {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(ContainerIvars {
                page: Cell::new(None),
                mask: RefCell::new(None),
            });
            let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
            this.setWantsLayer(true);
            this
        }

        /// Recuts the mask from the window's holes and where the page now
        /// is, if that changed it ([`mask_changed`]); otherwise the layer
        /// isn't touched.
        fn refresh_mask(&self) {
            let Some(page) = self.ivars().page.get() else {
                return;
            };
            let Some(parent) = (unsafe { self.superview() }) else {
                return;
            };
            let holes = window_holes(Retained::as_ptr(&parent) as usize);
            let next = page_mask(holes_in_page(&holes, page), page.2, page.3);
            if !mask_changed(self.ivars().mask.borrow().as_ref(), next.as_ref()) {
                return;
            }
            if self.apply_mask(next.as_ref()) {
                *self.ivars().mask.borrow_mut() = next;
            }
        }

        /// Sets (or, for `None`, removes) the layer mask: the container's
        /// bounds with the holes cut out by the even-odd rule. Holes never
        /// overlap each other (the conductor's bubble and badge sit beside
        /// him); two that did would cancel where they meet. With implicit
        /// animations off, so the cut follows him instead of easing after
        /// him. `false` when there is no layer yet: nothing was applied.
        fn apply_mask(&self, mask: Option<&PageMask>) -> bool {
            let Some(layer) = self.layer() else {
                return false;
            };
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            match mask {
                None => unsafe { layer.setMask(None) },
                Some(mask) => {
                    // AppKit's conversion to the layer's own coordinates,
                    // rather than a guess at whether it is flipped.
                    let outer = self.convertRectToLayer(self.bounds());
                    let path = CGMutablePath::new();
                    let local = |r: NSRect| {
                        NSRect::new(
                            NSPoint::new(r.origin.x - outer.origin.x, r.origin.y - outer.origin.y),
                            r.size,
                        )
                    };
                    unsafe { CGMutablePath::add_rect(Some(&path), ptr::null(), local(outer)) };
                    for hole in &mask.holes {
                        let rect = local(self.convertRectToLayer(NSRect::new(
                            NSPoint::new(hole.x as f64, hole.y as f64),
                            NSSize::new(hole.w as f64, hole.h as f64),
                        )));
                        // `holes_in_page` clamped the radius to what
                        // CoreGraphics accepts.
                        let r = hole.radius as f64;
                        unsafe {
                            if r > 0.0 {
                                CGMutablePath::add_rounded_rect(Some(&path), ptr::null(), rect, r, r);
                            } else {
                                CGMutablePath::add_rect(Some(&path), ptr::null(), rect);
                            }
                        }
                    }
                    let shape = layer
                        .mask()
                        .and_then(|m| m.downcast::<CAShapeLayer>().ok())
                        .unwrap_or_else(CAShapeLayer::new);
                    shape.setFrame(outer);
                    shape.setFillRule(unsafe { kCAFillRuleEvenOdd });
                    shape.setPath(Some(&path));
                    if let Some(window) = self.window() {
                        shape.setContentsScale(window.backingScaleFactor());
                    }
                    unsafe { layer.setMask(Some(&shape)) };
                }
            }
            CATransaction::commit();
            true
        }
    }

    /// The holes [`set_punch_through`] last gave for the window whose gpui
    /// view is at `parent`.
    fn window_holes(parent: usize) -> Vec<Hole> {
        HOLES.with(|h| {
            h.borrow()
                .iter()
                .find(|(view, _)| *view == parent)
                .map(|(_, holes)| holes.clone())
                .unwrap_or_default()
        })
    }

    /// The gpui view of `window`, which the pages are added to.
    fn gpui_view(window: &Window) -> Option<Retained<NSView>> {
        // Window has an inherent `window_handle()` (gpui's own handle
        // type); this is the raw-window-handle one.
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
            return None;
        };
        // SAFETY: gpui hands out its live GPUIView, an NSView subclass,
        // valid while `window` is borrowed. We retain it for the call.
        unsafe { Retained::retain(appkit.ns_view.as_ptr().cast::<NSView>()) }
    }

    /// Where gpui draws something that must show through, and take the
    /// pointer, over any page in `window`: rects in window coordinates
    /// (see [`Hole`]). Called with every frame's rects (empty for none); a
    /// set like the last one costs a comparison. A change recuts each of
    /// the window's pages, and `set_frame` recuts a page that moved.
    pub(crate) fn set_punch_through(window: &Window, holes: Vec<Hole>) {
        let Some(parent) = gpui_view(window) else {
            return;
        };
        let parent = Retained::as_ptr(&parent) as usize;
        let changed = HOLES.with(|h| {
            let mut h = h.borrow_mut();
            let at = h.iter().position(|(view, _)| *view == parent);
            match (at, holes.is_empty()) {
                (Some(i), _) if h[i].1 == holes => false,
                (None, true) => false,
                (Some(i), true) => {
                    h.remove(i);
                    true
                }
                (Some(i), false) => {
                    h[i].1 = holes;
                    true
                }
                (None, false) => {
                    h.push((parent, holes));
                    true
                }
            }
        });
        if !changed {
            return;
        }
        let containers: Vec<Retained<PageContainer>> =
            PAGES.with(|pages| pages.borrow().iter().map(|p| p.container.clone()).collect());
        for container in containers {
            if unsafe { container.superview() }
                .is_some_and(|v| Retained::as_ptr(&v) as usize == parent)
            {
                container.refresh_mask();
            }
        }
    }

    /// A `WKWebView` living on top of the gpui window's view. Main thread
    /// only (it is neither `Send` nor `Sync`).
    pub(crate) struct NativeWebView {
        view: Retained<WKWebView>,
        /// The view holding `view`, the one added to gpui's view: it is
        /// what is framed, shown and hidden, and it carries the mask.
        container: Retained<PageContainer>,
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
                // Right-click → Inspect Element, and the Developer tools
                // button's inspector (private; see the module docs).
                enable_developer_extras(&config.preferences());
                WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), NSRect::ZERO, &config)
            };
            // Safari's Web Inspector; macOS 13.3+ (we support 12).
            if view.respondsToSelector(sel!(setInspectable:)) {
                unsafe { view.setInspectable(true) };
            }
            // The page fills its container, which is what moves.
            view.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewWidthSizable
                    | NSAutoresizingMaskOptions::ViewHeightSizable,
            );
            let container = PageContainer::new(mtm);
            container.addSubview(&view);
            container.setHidden(true);
            // Keep the top edge pinned while the window resizes, until the
            // next frame's set_frame lands; otherwise an unflipped parent
            // drags the page down with its bottom edge for that frame.
            container.setAutoresizingMask(if parent.isFlipped() {
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
            // hit-testing reaches it first (outside its holes).
            parent.addSubview(&container);

            let pane_focused = Rc::new(Cell::new(false));
            PAGES.with(|pages| {
                pages.borrow_mut().push(PageEntry {
                    view: Retained::as_ptr(&view) as usize,
                    pane_focused: pane_focused.clone(),
                    gesture,
                    events: Rc::downgrade(&events),
                    container: container.clone(),
                })
            });
            let this = Self { view, container, delegate, _events: events, pane_focused, navigation };
            if !url.is_empty() {
                this.load(url);
            }
            Ok(this)
        }

        /// Places the view at `bounds`, in gpui window coordinates (logical
        /// points, top-left origin), which are the parent view's points.
        /// `_window` is part of the interface; the parent's own bounds and
        /// `isFlipped` are what AppKit positions against, so those are used.
        /// The page's holes follow: it is recut if the move changed them.
        pub(crate) fn set_frame(&self, bounds: Bounds<Pixels>, _window: &Window) {
            let Some(parent) = (unsafe { self.container.superview() }) else {
                return;
            };
            let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let y = if parent.isFlipped() {
                y
            } else {
                flip_y(y, h, parent.bounds().size.height as f32)
            };
            let size = NSSize::new(w as f64, h as f64);
            self.container
                .setFrame(NSRect::new(NSPoint::new(x as f64, y as f64), size));
            self.view.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), size));
            let page = (f32::from(bounds.origin.x), f32::from(bounds.origin.y), w, h);
            self.container.ivars().page.set(Some(page));
            self.container.refresh_mask();
        }

        pub(crate) fn set_visible(&self, visible: bool) {
            if visible {
                self.container.setHidden(false);
            } else {
                self.hide();
            }
        }

        /// Hides the page, first handing the keyboard back to gpui's view if
        /// the page had it (left to AppKit, it goes to whatever the next key
        /// view is). Returns whether it had it, so it can be given back.
        pub(crate) fn hide(&self) -> bool {
            let had_focus = self.release_focus();
            self.container.setHidden(true);
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
                // gpui's view, not the container.
                let parent = unsafe { self.container.superview() };
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

        /// The page's `_WKInspector` (private; see the module docs), or
        /// `None` where WebKit lacks the API or has no inspector for it.
        fn inspector(&self) -> Option<Retained<AnyObject>> {
            if !inspector_available()
                || !self.view.respondsToSelector(sel!(_inspector))
                || !has_method(self.view.class(), sel!(_inspector), Returns::Object)
            {
                return None;
            }
            // SAFETY: answered above; `-[WKWebView _inspector]` takes no
            // arguments and returns an object or nil (`@16@0:8`).
            unsafe { msg_send![&*self.view, _inspector] }
        }

        /// Whether the page's Web Inspector is open, in its own window or
        /// docked. `false` where the API is missing.
        pub(crate) fn inspector_visible(&self) -> bool {
            self.inspector()
                .filter(|i| responds(i, sel!(isVisible), Returns::Bool))
                // SAFETY: answered; `isVisible` returns a BOOL (`B16@0:8`).
                .is_some_and(|i| unsafe { msg_send![&*i, isVisible] })
        }

        /// The Developer tools button: closes the page's Web Inspector if it
        /// is open, else shows it (WebKit gives it its own window unless it
        /// docks it; the pane then detaches it: [`settle_inspector`]). A
        /// no-op where the API is missing (the button is hidden then).
        pub(crate) fn toggle_inspector(&self) {
            let Some(inspector) = self.inspector() else {
                return;
            };
            if self.inspector_visible() {
                if responds(&inspector, sel!(close), Returns::Nothing) {
                    // SAFETY: answered; `close` takes and returns nothing.
                    let _: () = unsafe { msg_send![&*inspector, close] };
                }
            } else if responds(&inspector, sel!(show), Returns::Nothing) {
                // SAFETY: answered; `show` takes and returns nothing.
                let _: () = unsafe { msg_send![&*inspector, show] };
            }
        }

        /// Where the page's inspector is ([`InspectorPlace`]): docked when
        /// its own web view (`inspectorWebView`) is in the page's window.
        pub(crate) fn inspector_place(&self) -> InspectorPlace {
            let Some(inspector) = self
                .inspector()
                .filter(|i| responds(i, sel!(inspectorWebView), Returns::Object))
            else {
                return InspectorPlace::Unsupported;
            };
            // SAFETY: answered; `inspectorWebView` returns a WKWebView or
            // nil (`@16@0:8`).
            let view: Option<Retained<WKWebView>> =
                unsafe { msg_send![&*inspector, inspectorWebView] };
            match view.and_then(|v| v.window()) {
                None => InspectorPlace::NotPlaced,
                Some(window) => {
                    let docked = self.view.window().is_some_and(|w| ptr::eq(&*w, &*window));
                    if docked {
                        InspectorPlace::Docked
                    } else {
                        InspectorPlace::OwnWindow
                    }
                }
            }
        }

        /// Moves a docked inspector into its own window. Only called when
        /// [`Self::inspector_place`] says it is docked.
        pub(crate) fn detach_inspector(&self) {
            if let Some(inspector) = self.inspector().filter(|i| responds(i, sel!(detach), Returns::Nothing)) {
                // SAFETY: answered; `detach` takes and returns nothing.
                let _: () = unsafe { msg_send![&*inspector, detach] };
            }
        }

        /// Closes the page's inspector if it is open or connected: the
        /// inspector belongs to this view, which is going away.
        fn close_inspector(&self) {
            let Some(inspector) = self.inspector() else {
                return;
            };
            let connected = responds(&inspector, sel!(isConnected), Returns::Bool)
                // SAFETY: answered; `isConnected` returns a BOOL.
                && unsafe { msg_send![&*inspector, isConnected] };
            if (connected || self.inspector_visible())
                && responds(&inspector, sel!(close), Returns::Nothing)
            {
                // SAFETY: answered; `close` takes and returns nothing.
                let _: () = unsafe { msg_send![&*inspector, close] };
            }
        }
    }

    /// The window is minimized or the app is hidden (⌘H), as opposed to
    /// merely covered by another window or on another Space: gpui reports
    /// all of those as one `WindowVisibility::Hidden`.
    pub(crate) fn window_is_minimized_or_hidden(window: &Window) -> bool {
        let Some(mtm) = MainThreadMarker::new() else {
            return false;
        };
        let Some(view) = gpui_view(window) else {
            return false;
        };
        view.window().is_some_and(|w| w.isMiniaturized())
            || NSApplication::sharedApplication(mtm).isHidden()
    }

    impl Drop for NativeWebView {
        fn drop(&mut self) {
            // The inspector is this page's: it closes first, while the page
            // is still whole. The next view (the tab shown again) starts
            // with it closed.
            self.close_inspector();
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
            self.container.removeFromSuperview();
        }
    }

    #[cfg(test)]
    mod tests {
        use objc2::ClassType as _;

        use super::{Delegate, PageContainer};

        /// Registering the class runs objc2's debug-build checks: the KVO
        /// override's type encoding must match NSObject's, and every delegate
        /// selector must exist in its protocol. A typo or a wrong
        /// `observeValueForKeyPath:` signature fails here, not in the app.
        #[test]
        #[cfg(debug_assertions)]
        fn delegate_class_registers() {
            assert_eq!(Delegate::class().name().to_str(), Ok("HarnessWebViewDelegate"));
        }

        /// Likewise for the page container: its `isFlipped` and `hitTest:`
        /// overrides must match NSView's signatures.
        #[test]
        #[cfg(debug_assertions)]
        fn container_class_registers() {
            assert_eq!(PageContainer::class().name().to_str(), Ok("HarnessPageContainer"));
        }

        /// The shape check every private message goes through, on methods
        /// whose shapes are known.
        #[test]
        fn private_messages_are_checked_for_their_shape() {
            use objc2::runtime::NSObject;
            use objc2::sel;

            use super::{Returns, has_method};
            let class = NSObject::class();
            assert!(has_method(class, sel!(description), Returns::Object));
            assert!(!has_method(class, sel!(description), Returns::Nothing));
            assert!(!has_method(class, sel!(description), Returns::Bool));
            assert!(has_method(class, sel!(isProxy), Returns::Bool));
            // One argument: not a message we would send bare.
            assert!(!has_method(class, sel!(respondsToSelector:), Returns::Bool));
            assert!(!has_method(class, sel!(harnessNoSuchMethod), Returns::Nothing));
        }

        /// The runtime check of the private inspector API, on the classes
        /// alone (no web view, no window). It must not panic or throw
        /// whatever this macOS has; the button's availability is exactly its
        /// verdict. Not asserted present: a future macOS may drop it.
        #[test]
        fn the_inspector_api_check_runs_without_a_web_view() {
            use super::{devtools_missing, inspector_available};
            let missing = devtools_missing();
            eprintln!("inspector API missing: {missing:?}");
            assert_eq!(inspector_available(), missing.is_none());
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

    fn hole(x: f32, y: f32, w: f32, h: f32, radius: f32) -> Hole {
        Hole { x, y, w, h, radius }
    }

    #[test]
    fn holes_move_into_the_pages_own_coordinates() {
        // A page below a 40pt bar and right of a 200pt sidebar, 600×500.
        let page = (200.0, 40.0, 600.0, 500.0);
        // The figure near the window's bottom-right corner, over the page.
        let figure = hole(700.0, 460.0, 64.0, 64.0, 0.0);
        assert_eq!(holes_in_page(&[figure], page), [hole(500.0, 420.0, 64.0, 64.0, 0.0)]);
        // Half off the page's right edge: kept, not clipped.
        let edge = hole(780.0, 100.0, 64.0, 64.0, 8.0);
        assert_eq!(holes_in_page(&[edge], page), [hole(580.0, 60.0, 64.0, 64.0, 8.0)]);
        // Over the sidebar, or just touching the page's edge: no hole.
        let sidebar = hole(20.0, 460.0, 64.0, 64.0, 0.0);
        let touching = hole(136.0, 100.0, 64.0, 64.0, 0.0);
        assert!(holes_in_page(&[sidebar, touching], page).is_empty());
        // Zero area (an empty bubble mid-layout): no hole.
        assert!(holes_in_page(&[hole(300.0, 100.0, 0.0, 40.0, 0.0)], page).is_empty());
        // The badge's pill radius (`rounded_full`) clamps to half its height.
        let badge = hole(500.0, 300.0, 120.0, 20.0, f32::MAX);
        assert_eq!(holes_in_page(&[badge], page), [hole(300.0, 260.0, 120.0, 20.0, 10.0)]);
        assert_eq!(holes_in_page(&[hole(300.0, 100.0, 10.0, 40.0, -3.0)], page)[0].radius, 0.0);
        // Order is kept.
        assert_eq!(holes_in_page(&[figure, sidebar, edge], page).len(), 2);
    }

    #[test]
    fn a_point_is_in_a_hole_only_inside_its_rounded_rect() {
        let holes = [hole(100.0, 100.0, 64.0, 64.0, 0.0), hole(300.0, 50.0, 230.0, 80.0, 8.0)];
        assert!(in_any_hole(&holes, 100.0, 100.0));
        assert!(in_any_hole(&holes, 163.9, 163.9));
        assert!(in_any_hole(&holes, 400.0, 90.0));
        // Just outside each edge.
        assert!(!in_any_hole(&holes, 99.9, 120.0));
        assert!(!in_any_hole(&holes, 164.0, 120.0));
        assert!(!in_any_hole(&holes, 120.0, 164.0));
        // Between the two, and with no holes at all.
        assert!(!in_any_hole(&holes, 200.0, 100.0));
        assert!(!in_any_hole(&[], 120.0, 120.0));
        // The bubble's corner, cut by its 8pt radius: the very corner is
        // the page's, a point inside the arc is the bubble's.
        assert!(!in_any_hole(&holes, 300.5, 50.5));
        assert!(in_any_hole(&holes, 303.0, 53.0));
        assert!(in_any_hole(&holes, 300.5, 58.0));
    }

    #[test]
    fn the_mask_changes_only_when_its_holes_or_size_do() {
        let a = hole(500.0, 420.0, 64.0, 64.0, 0.0);
        let mask = page_mask(vec![a], 600.0, 500.0);
        assert_eq!(
            mask,
            Some(PageMask { width: 600.0, height: 500.0, holes: vec![a] })
        );
        // No holes is no mask.
        assert_eq!(page_mask(Vec::new(), 600.0, 500.0), None);
        assert!(!mask_changed(None, None));
        // Every frame of a resting conductor: nothing to do.
        assert!(!mask_changed(mask.as_ref(), mask.as_ref()));
        let noise = page_mask(vec![hole(500.001, 420.0, 64.0, 64.0, 0.0)], 600.0, 500.0);
        assert!(!mask_changed(mask.as_ref(), noise.as_ref()));
        // He appears, goes, moves, or his bubble opens.
        assert!(mask_changed(None, mask.as_ref()));
        assert!(mask_changed(mask.as_ref(), None));
        let moved = page_mask(vec![hole(499.0, 420.0, 64.0, 64.0, 0.0)], 600.0, 500.0);
        assert!(mask_changed(mask.as_ref(), moved.as_ref()));
        let bubble = hole(270.0, 400.0, 230.0, 60.0, 8.0);
        let opened = page_mask(vec![bubble, a], 600.0, 500.0);
        assert!(mask_changed(mask.as_ref(), opened.as_ref()));
        // The page resized under a still conductor: the outer rect moved.
        let taller = page_mask(vec![a], 600.0, 520.0);
        assert!(mask_changed(mask.as_ref(), taller.as_ref()));
    }

    #[test]
    fn the_monitor_acts_only_for_our_own_pages() {
        // Views by address: a page's inner view, the page, its container,
        // gpui's view; an inspector's inner view and its web view.
        let (inner, page, container, gpui) = (10, 11, 12, 13);
        let (insp_inner, insp, insp_frame) = (20, 21, 22);
        let pages = [page, 99];
        // A click or the keyboard inside the page: the page.
        assert_eq!(registered_page(&[inner, page, container, gpui], &pages), Some(1));
        assert_eq!(registered_page(&[page, container, gpui], &pages), Some(0));
        // The inspector in its own window: a WKWebView too, but not ours.
        assert_eq!(registered_page(&[insp_inner, insp, insp_frame], &pages), None);
        // Docked beside the page, in the same container and window.
        assert_eq!(registered_page(&[insp_inner, insp, container, gpui], &pages), None);
        // gpui's own view (a hole, the address bar), and no page at all.
        assert_eq!(registered_page(&[gpui], &pages), None);
        assert_eq!(registered_page(&[inner, page, container, gpui], &[]), None);
        assert_eq!(registered_page(&[], &pages), None);
    }

    #[test]
    fn a_newly_opened_inspector_is_moved_to_its_own_window_once() {
        use InspectorPlace::{Docked, NotPlaced, OwnWindow, Unsupported};
        // (pending, place) -> (detach, still pending).
        // Nothing pending (closed, or already settled): never touched, so
        // the user docking it from its own toolbar sticks.
        for place in [Unsupported, NotPlaced, Docked, OwnWindow] {
            assert_eq!(settle_inspector(false, place), (false, false), "{place:?}");
        }
        // Just opened, docked by WebKit: detach, once.
        assert_eq!(settle_inspector(true, Docked), (true, false));
        // Already in its own window: nothing to do.
        assert_eq!(settle_inspector(true, OwnWindow), (false, false));
        // Not in any window yet: ask again next time.
        assert_eq!(settle_inspector(true, NotPlaced), (false, true));
        // WebKit can't say where it is: leave it where it is.
        assert_eq!(settle_inspector(true, Unsupported), (false, false));
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
