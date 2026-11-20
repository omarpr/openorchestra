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

use crate::link_routing;

/// Callbacks from the page, owned by the [`NativeWebView`] that runs them.
///
/// Every callback is posted to the main dispatch queue, never run inline. KVO
/// fires synchronously inside our own `load`/`reload` calls, so an inline
/// callback that updated the gpui entity calling `load` would re-borrow it and
/// panic. Callbacks queued when the view is dropped are discarded.
#[allow(dead_code)] // first built by the browser tab (Task 6)
pub(crate) struct NativeEvents {
    /// The page title; empty when the page has none.
    pub on_title: Box<dyn Fn(String)>,
    /// The page URL, including `pushState` changes.
    pub on_url: Box<dyn Fn(String)>,
    pub on_loading: Box<dyn Fn(bool)>,
    /// A load failed (WebKit's own message, e.g. "Could not connect to the
    /// server."). Cancellations, including our own policy cancels, are not
    /// failures.
    pub on_failed: Box<dyn Fn(String)>,
    /// `target=_blank` or `window.open` asked for an http(s) page.
    pub on_new_window: Box<dyn Fn(String)>,
    /// Navigation policy: http(s) loads in the page (see
    /// [`decide_navigation`]); downloads and every other scheme go here.
    /// Called inline (it doesn't touch gpui).
    pub open_external: fn(&str),
}

impl Default for NativeEvents {
    fn default() -> Self {
        Self {
            on_title: Box::new(|_| {}),
            on_url: Box::new(|_| {}),
            on_loading: Box::new(|_| {}),
            on_failed: Box::new(|_| {}),
            on_new_window: Box::new(|_| {}),
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
}

/// http(s) loads in the page. `about:`, `data:` and `blob:` load too: they
/// are the page's own plumbing (the blank tab, iframes, generated content)
/// and must never reach the default browser. Any other scheme (`mailto:`,
/// `zed:`, `file:` …) leaves the app when the main frame navigates to it,
/// and is dropped when a subframe does, so an iframe can't launch apps.
pub(crate) fn decide_navigation(url: &str, main_frame: bool) -> NavDecision {
    match scheme(url).as_deref() {
        // WebKit hands us its own URLs; one `url` can't parse is left to
        // WebKit, which reports it through on_failed if it can't load it.
        None => NavDecision::Load,
        Some("http" | "https" | "about" | "data" | "blob") => NavDecision::Load,
        Some(_) if main_frame => NavDecision::External,
        Some(_) => NavDecision::Ignore,
    }
}

/// A new-window request: http(s) becomes a new tab (`Load`), other schemes
/// leave the app, and `window.open()` with no URL or `about:blank` (scripts
/// that write into the new window) is ignored: there is no window to give it.
pub(crate) fn decide_new_window(url: &str) -> NavDecision {
    match decide_navigation(url, true) {
        NavDecision::Load if is_web(url) => NavDecision::Load,
        NavDecision::Load => NavDecision::Ignore,
        other => other,
    }
}

/// Where a download goes: the default browser, which fetches it again, so
/// only http(s) can be handed over (`External`). Page-generated downloads
/// (`blob:`, `data:`, e.g. "export JSON") exist only inside the page and are
/// dropped (`Ignore`); they must never reach `open`.
pub(crate) fn decide_download(url: &str) -> NavDecision {
    if is_web(url) {
        NavDecision::External
    } else {
        NavDecision::Ignore
    }
}

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

fn scheme(url: &str) -> Option<String> {
    url::Url::parse(url).ok().map(|u| u.scheme().to_owned())
}

fn is_web(url: &str) -> bool {
    matches!(scheme(url).as_deref(), Some("http" | "https"))
}

#[cfg(target_os = "macos")]
#[allow(unused_imports)] // first used by the browser tab (Task 6)
pub(crate) use mac::NativeWebView;

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    use std::rc::{Rc, Weak};

    use anyhow::{Context as _, anyhow, bail};
    use block2::DynBlock;
    use dispatch2::{DispatchQueue, MainThreadBound};
    use gpui::{Bounds, Pixels, Window};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
    use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{NSAutoresizingMaskOptions, NSView};
    use objc2_foundation::{
        NSDictionary, NSError, NSHTTPURLResponse, NSKeyValueChangeKey,
        NSKeyValueObservingOptions, NSObjectNSKeyValueObserverRegistration, NSObjectProtocol,
        NSPoint, NSRect, NSSize, NSString, NSURL, NSURLRequest, ns_string,
    };
    use objc2_web_kit::{
        WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate,
        WKNavigationResponse, WKNavigationResponsePolicy, WKUIDelegate, WKWebView,
        WKWebViewConfiguration, WKWebsiteDataStore, WKWindowFeatures,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::{
        NavDecision, NativeEvents, decide_download, decide_navigation, decide_new_window,
        flip_y, is_benign_failure, is_download,
    };

    /// The WKWebView properties we observe. All are documented KVO-compliant.
    const KVO_KEYS: [&str; 3] = ["title", "URL", "loading"];

    /// Tags our KVO registrations so we only ever react to (and remove) our own.
    static KVO_TAG: u8 = 0;

    fn kvo_context() -> *mut c_void {
        &KVO_TAG as *const u8 as *mut c_void
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
                // main-frame load: web URLs go on to createWebView (below),
                // anything else leaves the app here.
                let main_frame =
                    unsafe { action.targetFrame() }.is_none_or(|f| unsafe { f.isMainFrame() });
                // `<a download>` and the like. macOS 11.3+; we require 12.
                let decision = if unsafe { action.shouldPerformDownload() } {
                    decide_download(&url)
                } else {
                    decide_navigation(&url, main_frame)
                };
                let policy = match decision {
                    NavDecision::Load => WKNavigationActionPolicy::Allow,
                    NavDecision::External | NavDecision::Ignore => {
                        WKNavigationActionPolicy::Cancel
                    }
                };
                handler.call((policy,));
                if decision == NavDecision::External {
                    self.open_external(&url);
                }
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
                    if decide_download(&url) == NavDecision::External {
                        self.open_external(&url);
                    }
                } else {
                    handler.call((WKNavigationResponsePolicy::Allow,));
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
                self.post(|e| (e.on_failed)("The page stopped unexpectedly.".to_string()));
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
                match decide_new_window(&url) {
                    NavDecision::Load => self.post(move |e| (e.on_new_window)(url)),
                    NavDecision::External => self.open_external(&url),
                    NavDecision::Ignore => {}
                }
                // We never hand WebKit a second view; the app opens a tab.
                None
            }
        }
    );

    impl Delegate {
        fn new(events: Weak<NativeEvents>, mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(Ivars { events });
            unsafe { msg_send![super(this), init] }
        }

        /// Queues `f` on the main dispatch queue (see [`NativeEvents`]). It is
        /// skipped if the NativeWebView is gone by then.
        fn post(&self, f: impl FnOnce(&NativeEvents) + 'static) {
            let job = MainThreadBound::new((self.ivars().events.clone(), f), self.mtm());
            DispatchQueue::main().exec_async(move || {
                // The main queue only ever runs on the main thread.
                let mtm = MainThreadMarker::new().expect("main queue off the main thread");
                let (events, f) = job.into_inner(mtm);
                if let Some(events) = events.upgrade() {
                    f(&events);
                }
            });
        }

        fn open_external(&self, url: &str) {
            if let Some(events) = self.ivars().events.upgrade() {
                (events.open_external)(url);
            }
        }

        fn failed(&self, error: &NSError) {
            if is_benign_failure(&error.domain().to_string(), error.code()) {
                return;
            }
            let message = error.localizedDescription().to_string();
            self.post(move |e| (e.on_failed)(message));
        }
    }

    /// A `WKWebView` living on top of the gpui window's view. Main thread
    /// only (it is neither `Send` nor `Sync`).
    #[allow(dead_code)] // first built by the browser tab (Task 6)
    pub(crate) struct NativeWebView {
        view: Retained<WKWebView>,
        /// Owned here: the web view only holds its delegates weakly.
        delegate: Retained<Delegate>,
        _events: Rc<NativeEvents>,
    }

    #[allow(dead_code)] // first used by the browser tab (Task 6)
    impl NativeWebView {
        /// Adds a hidden web view to `window`'s view and starts loading `url`
        /// (nothing, for an empty `url`). Call `set_frame` and
        /// `set_visible(true)` to show it.
        pub(crate) fn new(window: &Window, url: &str, events: NativeEvents) -> anyhow::Result<Self> {
            let mtm = MainThreadMarker::new().context("web view created off the main thread")?;
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
            let delegate = Delegate::new(Rc::downgrade(&events), mtm);
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

            let this = Self { view, delegate, _events: events };
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
            self.view.setHidden(!visible);
        }

        /// Loads an absolute URL. One NSURL can't parse is reported through
        /// `on_failed`.
        pub(crate) fn load(&self, url: &str) {
            match NSURL::URLWithString(&NSString::from_str(url)) {
                Some(ns_url) => unsafe {
                    self.view.loadRequest(&NSURLRequest::requestWithURL(&ns_url));
                },
                None => {
                    let message = format!("Not a valid address: {url}");
                    self.delegate.post(move |e| (e.on_failed)(message));
                }
            }
        }

        pub(crate) fn back(&self) {
            unsafe { self.view.goBack() };
        }

        pub(crate) fn forward(&self) {
            unsafe { self.view.goForward() };
        }

        pub(crate) fn reload(&self) {
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

        pub(crate) fn url(&self) -> Option<String> {
            let url = url_string(unsafe { self.view.URL() });
            (!url.is_empty()).then_some(url)
        }

        pub(crate) fn can_go_back(&self) -> bool {
            unsafe { self.view.canGoBack() }
        }

        pub(crate) fn can_go_forward(&self) -> bool {
            unsafe { self.view.canGoForward() }
        }
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
            // The view goes away on every tab switch. If it (or anything in
            // it) has the keyboard, hand it back to gpui's view first, or
            // gpui stops getting keys until the user clicks.
            if let Some(window) = self.view.window() {
                let page_has_focus = window
                    .firstResponder()
                    .and_then(|r| r.downcast::<NSView>().ok())
                    .is_some_and(|v| v.isDescendantOf(&self.view));
                if page_has_focus {
                    let parent = unsafe { self.view.superview() };
                    window.makeFirstResponder(parent.as_deref().map(|p| &**p));
                }
            }
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
        (events.on_failed)("Could not connect to the server.".into());
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
            assert_eq!(decide_navigation(u, true), NavDecision::Load, "{u}");
            assert_eq!(decide_navigation(u, false), NavDecision::Load, "{u}");
        }
    }

    #[test]
    fn other_schemes_leave_from_the_main_frame_only() {
        for u in ["mailto:a@b.c", "zed://file/x", "file:///etc/hosts", "vscode://x", "tel:123"] {
            assert_eq!(decide_navigation(u, true), NavDecision::External, "{u}");
            assert_eq!(decide_navigation(u, false), NavDecision::Ignore, "{u}");
        }
    }

    #[test]
    fn new_windows_open_web_pages_only() {
        assert_eq!(decide_new_window("http://localhost:3000/a"), NavDecision::Load);
        assert_eq!(decide_new_window("https://example.com"), NavDecision::Load);
        assert_eq!(decide_new_window("mailto:a@b.c"), NavDecision::External);
        assert_eq!(decide_new_window(""), NavDecision::Ignore);
        assert_eq!(decide_new_window("about:blank"), NavDecision::Ignore);
        assert_eq!(decide_new_window("data:text/html,hi"), NavDecision::Ignore);
    }

    #[test]
    fn only_web_downloads_leave_the_app() {
        assert_eq!(decide_download("https://example.com/a.zip"), NavDecision::External);
        assert_eq!(decide_download("http://localhost:3000/dl"), NavDecision::External);
        // Page-generated: nothing outside the page can fetch these.
        assert_eq!(decide_download("blob:http://localhost:3000/1f2e"), NavDecision::Ignore);
        assert_eq!(decide_download("data:application/json,{}"), NavDecision::Ignore);
        assert_eq!(decide_download("about:blank"), NavDecision::Ignore);
        assert_eq!(decide_download(""), NavDecision::Ignore);
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
    fn cancellations_are_not_failures() {
        assert!(is_benign_failure("NSURLErrorDomain", -999));
        assert!(is_benign_failure("WebKitErrorDomain", 102));
        assert!(is_benign_failure("WebKitErrorDomain", 204));
        // Connection refused: the dev server isn't up yet. A real failure.
        assert!(!is_benign_failure("NSURLErrorDomain", -1004));
        assert!(!is_benign_failure("WebKitErrorDomain", -999));
    }
}
