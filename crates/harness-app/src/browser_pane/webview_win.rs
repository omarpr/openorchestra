//! The browser tab on Windows: Microsoft Edge WebView2 (the Evergreen
//! runtime that ships with Windows 11) through `webview2-com`, the same
//! bindings `wry` uses, with the same interface as the macOS `WKWebView`.
//!
//! **Hosting.** Each page lives in a host window of ours (`OOWebViewHost`), a
//! `WS_CHILD` of gpui's window, and WebView2's own windowed controller fills
//! it. gpui draws through DirectComposition with a non-topmost target, so
//! child windows sit above everything gpui draws, as the `WKWebView` sits
//! above gpui's Metal layer. `set_frame` places the host in gpui's client
//! area (gpui's logical points × the window's scale; gpui's client area
//! covers its custom title bar, so the origins agree).
//!
//! **The conductor and tooltips show through** as on macOS, by the host's
//! window region (`SetWindowRgn`): the page rect less each hole (rounded
//! like the bubble), less his figure along its outline (the pixels of his
//! current frame with alpha ≥ 50%, as runs of a region). Windows routes the
//! pointer by the region too, so a click there reaches gpui. A region has
//! no partial alpha, so his anti-aliased edge is hard-cut where macOS
//! blends it.
//!
//! **Loader.** `webview2-com-sys` links `WebView2Loader.dll` on the GNU
//! toolchain. Importing it would stop the whole app from starting without
//! the DLL, so it is loaded at runtime instead: the build script copies it
//! next to the executable (`build.rs`), and without it only browser tabs
//! fail ("WebView2Loader.dll is missing"). The environment is created once,
//! asynchronously, with its user data under `%LOCALAPPDATA%\openOrchestra\
//! WebView2` (cookies and storage shared by every tab and kept across
//! restarts, as `WKWebsiteDataStore.defaultDataStore` does).
//!
//! **Policy.** The navigation rules are the shared ones
//! (`decide_navigation`, `decide_new_window`), fed by WebView2's own
//! `IsUserInitiated` (a real gesture on the page), which macOS has to infer
//! from a key monitor. New windows need a gesture and never open a window:
//! http(s) becomes a tab. Downloads are WebView2's own (its download bubble,
//! saved to the user's Downloads folder), which covers `blob:` and `data:`
//! too, unlike macOS.
//!
//! **Keys.** While the page has the keyboard, WebView2 gets every key in
//! its own process. Chords with Ctrl or Alt surface first as
//! `AcceleratorKeyPressed`; one of the app's (`shortcuts::app_chords`, the
//! Global scope, rebindings applied) is marked handled and re-posted to
//! gpui's window, so Alt+T opens a terminal over a page as anywhere else,
//! and everything else (Ctrl+C/V/A/Z, Ctrl+F find in page) stays the
//! page's. A click into the page is its `GotFocus` (`on_click`), and gpui's
//! focus leaving the tab hands the keyboard back (`release_focus`).
//!
//! **Developer tools, docked.** WebView2 has no API to dock, close or query
//! its own DevTools window, so the docked inspector is a second WebView2
//! in the page's host window, showing DevTools' own frontend for the page:
//! the browser runs its debugging server (`--remote-debugging-port=0`: a
//! free port on 127.0.0.1, written to `DevToolsActivePort` in the data
//! folder), `Target.getTargetInfo` names the page's target, and the second
//! view loads `http://127.0.0.1:<port>/devtools/inspector.html?ws=…/page/<id>`.
//! The host splits between them ([`split`]: 40% to the inspector on the
//! bottom, left or right). Its side is Settings → General's placement
//! (`InspectorPlacement`), followed live while open; "Window" opens
//! WebView2's own DevTools window instead, as does right-click → Inspect.
//! Cost: while the debugging server runs, any local process that finds the
//! port can drive these browser tabs over the DevTools protocol (as with a
//! Chrome started with `--remote-debugging-port`); the port is random and
//! loopback-only. So the server runs only when the placement is docked as
//! the first browser tab opens (the environment is made once per run, with
//! its arguments): with "Window" there is no listener, and choosing a dock
//! later opens DevTools in its window until the app restarts.

#![warn(dead_code)]

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::{Rc, Weak};
use std::sync::OnceLock;

use anyhow::{Context as _, anyhow, bail};
use gpui::{Bounds, Pixels, Window};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::{
    AcceleratorKeyPressedEventHandler, ContentLoadingEventHandler, CoTaskMemPWSTR,
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
    DocumentTitleChangedEventHandler, FocusChangedEventHandler, HistoryChangedEventHandler,
    NavigationCompletedEventHandler, NavigationStartingEventHandler, NewWindowRequestedEventHandler,
    SourceChangedEventHandler,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, CombineRgn, CreateRectRgn, CreateRoundRectRgn, DeleteObject, ExtCreateRegion, GetStockObject,
    HBRUSH, HRGN, RDH_RECTANGLES, RGN_DIFF, RGNDATA, RGNDATAHEADER, SetWindowRgn, WHITE_BRUSH,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END,
    VK_ESCAPE, VK_F1, VK_F24, VK_HOME, VK_LEFT, VK_MENU, VK_NEXT, VK_OEM_1, VK_OEM_2, VK_OEM_3,
    VK_OEM_4, VK_OEM_5, VK_OEM_6, VK_OEM_7, VK_OEM_COMMA, VK_OEM_MINUS, VK_OEM_PERIOD,
    VK_OEM_PLUS, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow, HWND_TOP, IsIconic,
    PostMessageW, RegisterClassW, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SW_HIDE, SW_SHOWNA,
    SetWindowPos, ShowWindow, WINDOW_EX_STYLE, WM_KEYDOWN, WM_SYSKEYDOWN, WNDCLASSW, WS_CHILD,
    WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
};
use windows::core::{HSTRING, Interface, PCWSTR, PWSTR, s, w};

use super::{
    APP_CHORDS, DockSide, Hole, HoleSource, HoleSources, InspectorMove, InspectorPlace, NativeEvents,
    NavDecision, NavOrigin, Navigations, PageMask, Sprite, SpriteMask, decide_navigation,
    decide_new_window, holes_in_page, mask_changed, mask_frame_changed, page_mask,
    sprite_in_page, snap_to_device,
};
use crate::shortcuts::{Chord, is_app_shortcut};

// ---- loader and environment -------------------------------------------------

type CreateEnvironment =
    unsafe extern "system" fn(PCWSTR, PCWSTR, *mut c_void, *mut c_void) -> windows::core::HRESULT;

/// `CreateCoreWebView2EnvironmentWithOptions` from `WebView2Loader.dll`,
/// loaded once (see the module docs).
fn loader() -> Result<CreateEnvironment, String> {
    static LOADER: OnceLock<Result<usize, String>> = OnceLock::new();
    LOADER
        .get_or_init(|| unsafe {
            // By full path, from beside the exe only (where build.rs and
            // `make dist` put it): by bare name, the DLL search order would
            // take a copy from the working folder or PATH.
            let path = std::env::current_exe()
                .ok()
                .and_then(|exe| Some(exe.parent()?.join("WebView2Loader.dll")))
                .filter(|p| p.is_file())
                .ok_or_else(|| "WebView2Loader.dll is missing next to the app".to_string())?;
            let dll = LoadLibraryW(&HSTRING::from(path.as_os_str()))
                .map_err(|e| format!("could not load {} ({e})", path.display()))?;
            GetProcAddress(dll, s!("CreateCoreWebView2EnvironmentWithOptions"))
                .map(|f| f as usize)
                .ok_or_else(|| "WebView2Loader.dll has no CreateCoreWebView2EnvironmentWithOptions".into())
        })
        .clone()
        // SAFETY: the address is that export, whose signature this is.
        .map(|f| unsafe { std::mem::transmute::<usize, CreateEnvironment>(f) })
}

enum Env {
    Idle,
    /// Created asynchronously; pages made meanwhile wait here.
    Pending(Vec<Weak<Inner>>),
    Ready(ICoreWebView2Environment),
    Failed(String),
}

thread_local! {
    static ENV: RefCell<Env> = const { RefCell::new(Env::Idle) };
    /// This run's environment was started with the debugging server (a
    /// `DevToolsActivePort` left by an earlier run must not be trusted).
    static DEBUG_SERVER: Cell<bool> = const { Cell::new(false) };
    /// Every live page, for recutting a window's pages when its holes change.
    static PAGES: RefCell<Vec<Weak<Inner>>> = const { RefCell::new(Vec::new()) };
    /// Per gpui window (its HWND): what shows through its pages.
    static PUNCH: RefCell<Vec<(isize, Punch)>> = const { RefCell::new(Vec::new()) };
}

fn user_data_folder() -> std::path::PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("openOrchestra")
        .join("WebView2")
}

/// Hands `page` a controller once the shared environment exists, starting
/// it on first use.
fn with_environment(page: &Rc<Inner>) -> anyhow::Result<()> {
    let start = ENV.with(|env| {
        let mut env = env.borrow_mut();
        match &mut *env {
            Env::Ready(e) => {
                let e = e.clone();
                drop(env);
                create_controller(&e, page);
                Ok(false)
            }
            Env::Pending(waiting) => {
                waiting.push(Rc::downgrade(page));
                Ok(false)
            }
            Env::Failed(why) => Err(anyhow!("{why}")),
            Env::Idle => {
                *env = Env::Pending(vec![Rc::downgrade(page)]);
                Ok(true)
            }
        }
    })?;
    if !start {
        return Ok(());
    }
    let create = match loader() {
        Ok(f) => f,
        Err(why) => {
            ENV.with(|env| *env.borrow_mut() = Env::Failed(why.clone()));
            bail!(why);
        }
    };
    let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(|result, env| {
        let waiting = ENV.with(|slot| {
            let mut slot = slot.borrow_mut();
            let waiting = match std::mem::replace(&mut *slot, Env::Idle) {
                Env::Pending(w) => w,
                _ => Vec::new(),
            };
            *slot = match (result, env) {
                (Ok(()), Some(env)) => Env::Ready(env),
                (Err(e), _) => Env::Failed(format!("WebView2 failed to start: {e}")),
                (Ok(()), None) => Env::Failed("WebView2 returned no environment".into()),
            };
            waiting
        });
        let ready = ENV.with(|slot| match &*slot.borrow() {
            Env::Ready(e) => Ok(e.clone()),
            Env::Failed(why) => Err(why.clone()),
            _ => Err("WebView2 environment vanished".into()),
        });
        match ready {
            Ok(env) => {
                for page in waiting.iter().filter_map(Weak::upgrade) {
                    create_controller(&env, &page);
                }
            }
            Err(why) => {
                log::warn!("browser: {why}");
                for page in waiting.iter().filter_map(Weak::upgrade) {
                    page.fail(&why);
                }
            }
        }
        Ok(())
    }));
    let folder = HSTRING::from(user_data_folder().as_os_str());
    // The docked inspector is DevTools' own frontend, served by the
    // browser's debugging server (see the module docs); port 0 lets it pick
    // a free port, which it writes to `DevToolsActivePort`.
    let options = webview2_com::CoreWebView2EnvironmentOptions::default();
    let docked = placement_side(crate::browser_pane::inspector_placement()).is_some();
    if docked {
        unsafe { options.set_additional_browser_arguments("--remote-debugging-port=0".to_string()) };
    }
    DEBUG_SERVER.with(|on| on.set(docked));
    let options: ICoreWebView2EnvironmentOptions = options.into();
    // SAFETY: the loader's documented signature; it AddRefs the handler
    // and the options.
    let hr = unsafe {
        create(
            PCWSTR::null(),
            PCWSTR(folder.as_ptr()),
            options.as_raw(),
            handler.as_raw(),
        )
    };
    if let Err(e) = hr.ok() {
        let why = format!("WebView2 is not available: {e}");
        ENV.with(|env| *env.borrow_mut() = Env::Failed(why.clone()));
        bail!(why);
    }
    Ok(())
}

fn create_controller(env: &ICoreWebView2Environment, page: &Rc<Inner>) {
    let weak = Rc::downgrade(page);
    let handler = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
        move |result, controller| {
            match (weak.upgrade(), result, controller) {
                (Some(page), Ok(()), Some(controller)) => page.attach(controller),
                (None, _, Some(controller)) => unsafe {
                    // The tab went away while WebView2 started.
                    let _ = controller.Close();
                },
                (Some(page), Err(e), _) => page.fail(&format!("WebView2 failed to open a page: {e}")),
                (Some(page), Ok(()), None) => page.fail("WebView2 returned no page"),
                (None, _, None) => {}
            }
            Ok(())
        },
    ));
    if let Err(e) = unsafe { env.CreateCoreWebView2Controller(page.host, &handler) } {
        page.fail(&format!("WebView2 failed to open a page: {e}"));
    }
}

// ---- host window ------------------------------------------------------------

const HOST_CLASS: PCWSTR = w!("OOWebViewHost");

fn register_host_class() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| unsafe {
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(host_proc),
            hInstance: GetModuleHandleW(None).unwrap_or_default().into(),
            // Pages are mostly white; this is what shows until the first
            // paint of a new page.
            hbrBackground: HBRUSH(GetStockObject(WHITE_BRUSH).0),
            lpszClassName: HOST_CLASS,
            ..Default::default()
        };
        RegisterClassW(&class);
    });
}

unsafe extern "system" fn host_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// gpui's HWND for `window`.
fn gpui_hwnd(window: &Window) -> Option<HWND> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut c_void)),
        _ => None,
    }
}

// ---- the page ---------------------------------------------------------------

/// A page's state, shared (weakly) with WebView2's callbacks.
struct Inner {
    /// gpui's window.
    parent: HWND,
    /// Ours: what is placed, shown, hidden and cut.
    host: HWND,
    controller: RefCell<Option<ICoreWebView2Controller>>,
    webview: RefCell<Option<ICoreWebView2>>,
    events: NativeEvents,
    /// What to do once the controller exists.
    pending_url: RefCell<Option<String>>,
    focus_when_ready: Cell<bool>,
    visible: Cell<bool>,
    /// The host's size in device pixels.
    size: Cell<(i32, i32)>,
    /// The page in gpui window coordinates (logical), for its holes.
    page: Cell<Option<(f32, f32, f32, f32)>>,
    scale: Cell<f32>,
    /// gpui's client origin on screen, to tell WebView2 when the window moved.
    screen_origin: Cell<(i32, i32)>,
    /// The page (WebView2's controller) has the keyboard.
    has_focus: Cell<bool>,
    pane_focused: Cell<bool>,
    navigation: RefCell<Navigations>,
    /// The URL each main-frame navigation went to, by WebView2's id, so a
    /// failure can name it.
    nav_urls: RefCell<Vec<(u64, String)>>,
    can_back: Cell<bool>,
    can_forward: Cell<bool>,
    /// The region last applied ([`PageMask`]), to skip unchanged recuts.
    mask: RefCell<Option<PageMask>>,
    /// Where the docked inspector is (`Some` from the moment it is asked
    /// for), its controller once WebView2 made it, whether it has the
    /// keyboard, and a counter that retires a slow open the user already
    /// closed.
    inspector_side: Cell<Option<DockSide>>,
    inspector: RefCell<Option<ICoreWebView2Controller>>,
    inspector_focus: Cell<bool>,
    inspector_generation: Cell<u64>,
}

/// Whether a new-window request came from the page's main frame (as
/// WKWebView's `sourceFrame` tells on macOS). Only a click there may hand a
/// link to another app; a frame WebView2 doesn't name (an older runtime)
/// counts as a subframe. http(s) opens a tab either way.
fn new_window_from_main_frame(args: &ICoreWebView2NewWindowRequestedEventArgs) -> bool {
    unsafe {
        args.cast::<ICoreWebView2NewWindowRequestedEventArgs3>()
            .and_then(|a| a.OriginalSourceFrameInfo())
            .and_then(|f| f.cast::<ICoreWebView2FrameInfo2>())
            .is_ok_and(|f| {
                let mut kind = COREWEBVIEW2_FRAME_KIND::default();
                f.FrameKind(&mut kind).is_ok() && kind == COREWEBVIEW2_FRAME_KIND_MAIN_FRAME
            })
    }
}

/// The debugging server's port, from `DevToolsActivePort` in the browser's
/// data folder (first line).
fn debug_port() -> Option<u16> {
    if !DEBUG_SERVER.with(Cell::get) {
        return None;
    }
    let file = user_data_folder().join("EBWebView").join("DevToolsActivePort");
    std::fs::read_to_string(file).ok()?.lines().next()?.trim().parse().ok()
}

/// The page's and the docked inspector's rects in a `w` × `h` host.
fn split(w: i32, h: i32, side: Option<DockSide>) -> (RECT, Option<RECT>) {
    let rect = |left, top, right, bottom| RECT { left, top, right, bottom };
    match side {
        None => (rect(0, 0, w, h), None),
        Some(DockSide::Bottom) => {
            let ih = (h * 2 / 5).max(h.min(180));
            (rect(0, 0, w, h - ih), Some(rect(0, h - ih, w, h)))
        }
        Some(DockSide::Left) => {
            let iw = (w * 2 / 5).max(w.min(320));
            (rect(iw, 0, w, h), Some(rect(0, 0, iw, h)))
        }
        Some(DockSide::Right) => {
            let iw = (w * 2 / 5).max(w.min(320));
            (rect(0, 0, w - iw, h), Some(rect(w - iw, 0, w, h)))
        }
    }
}

impl Inner {
    /// Sizes the page and a docked inspector to the host.
    fn layout(&self) {
        let (w, h) = self.size.get();
        let (page, inspector) = split(w, h, self.inspector_side.get());
        unsafe {
            if let Some(controller) = self.controller.borrow().as_ref() {
                let _ = controller.SetBounds(page);
            }
            if let (Some(controller), Some(rect)) = (self.inspector.borrow().as_ref(), inspector) {
                let _ = controller.SetBounds(rect);
            }
        }
    }

    /// Docks the inspector on `side` (re-docks an open one).
    fn open_inspector(self: &Rc<Self>, side: DockSide) {
        if self.inspector_side.get().is_some() {
            self.inspector_side.set(Some(side));
            self.layout();
            return;
        }
        let Some(webview) = self.webview.borrow().clone() else {
            return;
        };
        let Some(port) = debug_port() else {
            log::info!(
                "browser: no DevTools server (placement was Window when the browser started); \
                 opening the inspector in its own window"
            );
            unsafe {
                let _ = webview.OpenDevToolsWindow();
            }
            return;
        };
        self.inspector_side.set(Some(side));
        let generation = self.inspector_generation.get() + 1;
        self.inspector_generation.set(generation);
        let me = Rc::downgrade(self);
        // This page's DevTools target, then the frontend for it.
        let handler = webview2_com::CallDevToolsProtocolMethodCompletedHandler::create(Box::new(
            move |result, json| {
                let Some(me) = me.upgrade() else {
                    return Ok(());
                };
                if me.inspector_generation.get() != generation || me.inspector_side.get().is_none() {
                    return Ok(());
                }
                let target = result.ok().and_then(|()| {
                    let v: serde_json::Value = serde_json::from_str(&json).ok()?;
                    v["targetInfo"]["targetId"].as_str().map(str::to_string)
                });
                match target {
                    Some(target) => me.create_inspector(port, &target, generation),
                    None => {
                        log::warn!("browser: no DevTools target for the page");
                        me.inspector_side.set(None);
                    }
                }
                Ok(())
            },
        ));
        unsafe {
            if let Err(e) = webview.CallDevToolsProtocolMethod(w!("Target.getTargetInfo"), w!("{}"), &handler) {
                log::warn!("browser: DevTools target lookup failed: {e}");
                self.inspector_side.set(None);
            }
        }
    }

    fn create_inspector(self: &Rc<Self>, port: u16, target: &str, generation: u64) {
        let Some(env) = ENV.with(|e| match &*e.borrow() {
            Env::Ready(env) => Some(env.clone()),
            _ => None,
        }) else {
            return;
        };
        let url = format!(
            "http://127.0.0.1:{port}/devtools/inspector.html?ws=127.0.0.1:{port}/devtools/page/{target}"
        );
        let me = Rc::downgrade(self);
        let handler = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
            move |result, controller| {
                let Some(controller) = controller else {
                    return Ok(());
                };
                let live = me.upgrade().filter(|me| {
                    result.is_ok()
                        && me.inspector_generation.get() == generation
                        && me.inspector_side.get().is_some()
                });
                let Some(me) = live else {
                    unsafe {
                        let _ = controller.Close();
                    }
                    return Ok(());
                };
                unsafe {
                    let _ = controller.SetIsVisible(me.visible.get());
                    let weak = Rc::downgrade(&me);
                    let mut token = 0i64;
                    let _ = controller.add_GotFocus(
                        &FocusChangedEventHandler::create(Box::new(move |_, _| {
                            if let Some(me) = weak.upgrade() {
                                me.inspector_focus.set(true);
                                (me.events.on_click)();
                            }
                            Ok(())
                        })),
                        &mut token,
                    );
                    let weak = Rc::downgrade(&me);
                    let _ = controller.add_LostFocus(
                        &FocusChangedEventHandler::create(Box::new(move |_, _| {
                            if let Some(me) = weak.upgrade() {
                                me.inspector_focus.set(false);
                            }
                            Ok(())
                        })),
                        &mut token,
                    );
                    let weak = Rc::downgrade(&me);
                    let _ = controller.add_AcceleratorKeyPressed(
                        &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                            if let (Some(me), Some(args)) = (weak.upgrade(), args) {
                                me.route_accelerator(&args);
                            }
                            Ok(())
                        })),
                        &mut token,
                    );
                    if let Ok(webview) = controller.CoreWebView2() {
                        // A remote frontend starts in screencast mode (a
                        // mirror of the page beside the panels); the page
                        // is right there, so that setting starts off.
                        let _ = webview.AddScriptToExecuteOnDocumentCreated(
                            w!("try{for(const k of ['screencast-enabled','screencastEnabled'])localStorage.setItem(k,'false')}catch(e){}"),
                            None,
                        );
                        let _ = webview.Navigate(&HSTRING::from(url.as_str()));
                    }
                }
                *me.inspector.borrow_mut() = Some(controller);
                me.layout();
                Ok(())
            },
        ));
        if let Err(e) = unsafe { env.CreateCoreWebView2Controller(self.host, &handler) } {
            log::warn!("browser: could not open the inspector: {e}");
            self.inspector_side.set(None);
        }
    }

    /// Closes a docked inspector; returns whether it had the keyboard (which
    /// goes back to gpui's window first).
    fn close_inspector(&self) -> bool {
        self.inspector_generation.set(self.inspector_generation.get() + 1);
        self.inspector_side.set(None);
        let had_focus = self.inspector_focus.replace(false);
        if had_focus {
            unsafe {
                let _ = SetFocus(Some(self.parent));
            }
        }
        if let Some(controller) = self.inspector.borrow_mut().take() {
            unsafe {
                let _ = controller.Close();
            }
        }
        self.layout();
        had_focus
    }

    /// DevTools in WebView2's own window (the "Window" placement).
    fn inspector_window(&self) {
        if let Some(webview) = self.webview.borrow().as_ref() {
            unsafe {
                let _ = webview.OpenDevToolsWindow();
            }
        }
    }

    fn fail(&self, why: &str) {
        let navigation = self.navigation.borrow().current();
        (self.events.on_failed)(self.pending_url.borrow().clone(), why.to_string(), navigation);
    }

    fn attach(self: &Rc<Self>, controller: ICoreWebView2Controller) {
        let webview = match unsafe { controller.CoreWebView2() } {
            Ok(w) => w,
            Err(e) => {
                self.fail(&format!("WebView2 failed to open a page: {e}"));
                unsafe {
                    let _ = controller.Close();
                }
                return;
            }
        };
        unsafe {
            if let Ok(settings) = webview.Settings() {
                let _ = settings.SetIsStatusBarEnabled(true);
                let _ = settings.SetAreDevToolsEnabled(true);
            }
            let (w, h) = self.size.get();
            let _ = controller.SetBounds(RECT { left: 0, top: 0, right: w, bottom: h });
            let _ = controller.SetIsVisible(self.visible.get());
        }
        if let Err(e) = self.subscribe(&controller, &webview) {
            log::warn!("browser: WebView2 events: {e}");
        }
        *self.controller.borrow_mut() = Some(controller.clone());
        *self.webview.borrow_mut() = Some(webview);
        self.layout();
        if let Some(url) = self.pending_url.borrow_mut().take() {
            self.navigate(&url);
        }
        if self.focus_when_ready.replace(false) {
            unsafe {
                let _ = controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
            }
        }
    }

    fn subscribe(
        self: &Rc<Self>,
        controller: &ICoreWebView2Controller,
        webview: &ICoreWebView2,
    ) -> windows::core::Result<()> {
        let mut token = 0i64;
        unsafe {
            let me = Rc::downgrade(self);
            webview.add_DocumentTitleChanged(
                &DocumentTitleChangedEventHandler::create(Box::new(move |sender, _| {
                    if let (Some(me), Some(sender)) = (me.upgrade(), sender) {
                        let title = take(|p| sender.DocumentTitle(p));
                        (me.events.on_title)(title);
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            let me = Rc::downgrade(self);
            webview.add_SourceChanged(
                &SourceChangedEventHandler::create(Box::new(move |sender, args| {
                    if let (Some(me), Some(sender)) = (me.upgrade(), sender) {
                        let url = take(|p| sender.Source(p));
                        if !url.is_empty() {
                            (me.events.on_url)(url.clone());
                            // A new document commits through ContentLoading;
                            // this one changed its URL in place (pushState,
                            // a #fragment).
                            let mut new_document = windows::core::BOOL(1);
                            if let Some(args) = args {
                                let _ = args.IsNewDocument(&mut new_document);
                            }
                            if !new_document.as_bool() {
                                (me.events.on_commit)(url, true);
                            }
                        }
                        me.refresh_history();
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            let me = Rc::downgrade(self);
            webview.add_ContentLoading(
                &ContentLoadingEventHandler::create(Box::new(move |sender, args| {
                    if let (Some(me), Some(sender), Some(args)) = (me.upgrade(), sender, args) {
                        let mut error_page = windows::core::BOOL(0);
                        let _ = args.IsErrorPage(&mut error_page);
                        if !error_page.as_bool() {
                            let url = take(|p| sender.Source(p));
                            (me.events.on_commit)(url, false);
                        }
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            let me = Rc::downgrade(self);
            webview.add_HistoryChanged(
                &HistoryChangedEventHandler::create(Box::new(move |_, _| {
                    if let Some(me) = me.upgrade() {
                        me.refresh_history();
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            let me = Rc::downgrade(self);
            webview.add_NavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                    let (Some(me), Some(args)) = (me.upgrade(), args) else {
                        return Ok(());
                    };
                    let url = take(|p| args.Uri(p));
                    let mut user = windows::core::BOOL(0);
                    let _ = args.IsUserInitiated(&mut user);
                    let origin = NavOrigin { user_initiated: user.as_bool(), from_main_frame: true };
                    if !me.allow(&url, decide_navigation(&url, true, origin)) {
                        let _ = args.SetCancel(true);
                        return Ok(());
                    }
                    let mut id = 0u64;
                    let _ = args.NavigationId(&mut id);
                    me.navigation.borrow_mut().started(Some(id as usize));
                    {
                        let mut urls = me.nav_urls.borrow_mut();
                        urls.retain(|(n, _)| *n != id);
                        urls.push((id, url));
                        let excess = urls.len().saturating_sub(16);
                        urls.drain(..excess);
                    }
                    (me.events.on_loading)(true);
                    Ok(())
                })),
                &mut token,
            )?;

            // Iframes: the same rules, never leaving the app.
            let me = Rc::downgrade(self);
            webview.add_FrameNavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                    let (Some(me), Some(args)) = (me.upgrade(), args) else {
                        return Ok(());
                    };
                    let url = take(|p| args.Uri(p));
                    let origin = NavOrigin { user_initiated: false, from_main_frame: false };
                    if !me.allow(&url, decide_navigation(&url, false, origin)) {
                        let _ = args.SetCancel(true);
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            let me = Rc::downgrade(self);
            webview.add_NavigationCompleted(
                &NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                    let (Some(me), Some(args)) = (me.upgrade(), args) else {
                        return Ok(());
                    };
                    (me.events.on_loading)(false);
                    me.refresh_history();
                    let mut ok = windows::core::BOOL(1);
                    let _ = args.IsSuccess(&mut ok);
                    let mut status = COREWEBVIEW2_WEB_ERROR_STATUS_UNKNOWN;
                    let _ = args.WebErrorStatus(&mut status);
                    if ok.as_bool() {
                        return Ok(());
                    }
                    let Some(reason) = failure_reason(status) else {
                        return Ok(());
                    };
                    let mut id = 0u64;
                    let _ = args.NavigationId(&mut id);
                    let tagged = me.navigation.borrow().failure(Some(id as usize));
                    if let Some(number) = tagged {
                        let url = me
                            .nav_urls
                            .borrow()
                            .iter()
                            .find(|(n, _)| *n == id)
                            .map(|(_, u)| u.clone());
                        (me.events.on_failed)(url, reason.to_string(), number);
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            let me = Rc::downgrade(self);
            webview.add_NewWindowRequested(
                &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                    let (Some(me), Some(args)) = (me.upgrade(), args) else {
                        return Ok(());
                    };
                    // Never a window of WebView2's own: a tab, the system, or nothing.
                    let _ = args.SetHandled(true);
                    let url = take(|p| args.Uri(p));
                    let mut user = windows::core::BOOL(0);
                    let _ = args.IsUserInitiated(&mut user);
                    if !user.as_bool() {
                        log::info!("browser: ignoring a new window the user didn't click for");
                        return Ok(());
                    }
                    let origin = NavOrigin {
                        user_initiated: true,
                        from_main_frame: new_window_from_main_frame(&args),
                    };
                    match decide_new_window(&url, origin) {
                        NavDecision::Load => (me.events.on_new_window)(url),
                        decision => {
                            me.allow(&url, decision);
                        }
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            let me = Rc::downgrade(self);
            controller.add_GotFocus(
                &FocusChangedEventHandler::create(Box::new(move |_, _| {
                    if let Some(me) = me.upgrade() {
                        me.has_focus.set(true);
                        (me.events.on_click)();
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            let me = Rc::downgrade(self);
            controller.add_LostFocus(
                &FocusChangedEventHandler::create(Box::new(move |_, _| {
                    if let Some(me) = me.upgrade() {
                        me.has_focus.set(false);
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            let me = Rc::downgrade(self);
            controller.add_AcceleratorKeyPressed(
                &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                    if let (Some(me), Some(args)) = (me.upgrade(), args) {
                        me.route_accelerator(&args);
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
        Ok(())
    }

    /// Acts on a navigation decision; `true` when WebView2 should load it.
    fn allow(&self, url: &str, decision: NavDecision) -> bool {
        match decision {
            NavDecision::Load => true,
            NavDecision::External => {
                (self.events.open_external)(url);
                false
            }
            NavDecision::Ignore => false,
            NavDecision::Unsupported => {
                (self.events.on_notice)(super::DOWNLOAD_UNSUPPORTED.to_string());
                false
            }
        }
    }

    /// An app chord pressed over the page goes to gpui instead.
    fn route_accelerator(&self, args: &ICoreWebView2AcceleratorKeyPressedEventArgs) {
        let mut kind = COREWEBVIEW2_KEY_EVENT_KIND_KEY_UP;
        let mut vk = 0u32;
        let mut lparam = 0i32;
        unsafe {
            let _ = args.KeyEventKind(&mut kind);
            let _ = args.VirtualKey(&mut vk);
            let _ = args.KeyEventLParam(&mut lparam);
        }
        let system = kind == COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN;
        if !(system || kind == COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN) {
            return;
        }
        let down = |k: VIRTUAL_KEY| unsafe { GetKeyState(k.0 as i32) } < 0;
        let (ctrl, alt, shift) = (down(VK_CONTROL), down(VK_MENU), down(VK_SHIFT));
        if !(ctrl || alt) {
            return;
        }
        let Some(key) = vk_name(vk as u16) else {
            return;
        };
        let chord = Chord::new(&key, false, shift, alt, ctrl);
        let app = APP_CHORDS.with(|c| is_app_shortcut(&chord, &c.borrow()));
        if !app {
            return;
        }
        unsafe {
            let _ = args.SetHandled(true);
            let msg = if system { WM_SYSKEYDOWN } else { WM_KEYDOWN };
            let _ = PostMessageW(Some(self.parent), msg, WPARAM(vk as usize), LPARAM(lparam as isize));
        }
    }

    fn refresh_history(&self) {
        if let Some(webview) = self.webview.borrow().as_ref() {
            let mut back = windows::core::BOOL(0);
            let mut forward = windows::core::BOOL(0);
            unsafe {
                let _ = webview.CanGoBack(&mut back);
                let _ = webview.CanGoForward(&mut forward);
            }
            self.can_back.set(back.as_bool());
            self.can_forward.set(forward.as_bool());
        }
    }

    fn navigate(&self, url: &str) {
        let Some(webview) = self.webview.borrow().clone() else {
            *self.pending_url.borrow_mut() = Some(url.to_string());
            return;
        };
        if let Err(e) = unsafe { webview.Navigate(&HSTRING::from(url)) } {
            // WebView2 rejects what it can't parse (E_INVALIDARG).
            let navigation = self.navigation.borrow().current();
            log::info!("browser: WebView2 refused an address: {e}");
            (self.events.on_failed)(Some(url.to_string()), "not a valid address".into(), navigation);
        }
    }

    /// Recuts the host's region from its window's holes, if they changed.
    fn refresh_mask(&self) {
        let Some(page) = self.page.get() else {
            return;
        };
        let punch = window_punch(self.parent.0 as isize);
        let sprite = punch.sprite.as_ref().and_then(|s| sprite_in_page(s.mask(), page));
        let holes = punch.holes.combined(None);
        let next = page_mask(holes_in_page(&holes, page), sprite, page.2, page.3);
        {
            let applied = self.mask.borrow();
            if !mask_changed(applied.as_ref(), next.as_ref())
                && !mask_frame_changed(applied.as_ref(), next.as_ref())
            {
                return;
            }
        }
        let image = sprite.and(punch.sprite.as_ref()).map(|s| s.image.clone());
        self.apply_region(next.as_ref(), image.as_deref());
        *self.mask.borrow_mut() = next;
    }

    fn apply_region(&self, mask: Option<&PageMask>, image: Option<&gpui::RenderImage>) {
        let Some(mask) = mask else {
            unsafe {
                SetWindowRgn(self.host, None, true);
            }
            return;
        };
        let scale = self.scale.get();
        let px = |v: f32| (v * scale).round() as i32;
        unsafe {
            let (w, h) = self.size.get();
            let region = CreateRectRgn(0, 0, w, h);
            for hole in &mask.holes {
                let (l, t) = (px(hole.x), px(hole.y));
                let (r, b) = (px(hole.x + hole.w), px(hole.y + hole.h));
                let d = px(hole.radius * 2.0);
                let cut = if d > 1 {
                    CreateRoundRectRgn(l, t, r + 1, b + 1, d, d)
                } else {
                    CreateRectRgn(l, t, r, b)
                };
                CombineRgn(Some(region), Some(region), Some(cut), RGN_DIFF);
                let _ = DeleteObject(cut.into());
            }
            if let Some(sprite) = mask.sprite {
                let cut = image
                    .and_then(|image| figure_region(image, sprite, scale))
                    .unwrap_or_else(|| {
                        let r = sprite.rect;
                        CreateRectRgn(px(r.x), px(r.y), px(r.x + r.w), px(r.y + r.h))
                    });
                CombineRgn(Some(region), Some(region), Some(cut), RGN_DIFF);
                let _ = DeleteObject(cut.into());
            }
            // The system owns the region from here.
            SetWindowRgn(self.host, Some(region), true);
        }
    }
}

/// The opaque part (alpha ≥ 50%) of the conductor's current frame, scaled
/// onto his page-local rect in device pixels, as a region of row runs.
fn figure_region(image: &gpui::RenderImage, sprite: SpriteMask, scale: f32) -> Option<HRGN> {
    let size = image.size(0);
    let (fw, fh) = (size.width.0.max(0) as usize, size.height.0.max(0) as usize);
    let bgra = image.as_bytes(0)?;
    if fw == 0 || fh == 0 || bgra.len() != fw * fh * 4 {
        return None;
    }
    let rect = sprite.rect;
    let (x0, y0) = ((rect.x * scale).round() as i32, (rect.y * scale).round() as i32);
    let (dw, dh) = ((rect.w * scale).round() as i32, (rect.h * scale).round() as i32);
    if dw <= 0 || dh <= 0 {
        return None;
    }
    let mut rects: Vec<RECT> = Vec::new();
    for dy in 0..dh {
        let sy = (dy as usize * fh) / dh as usize;
        let mut run: Option<i32> = None;
        for dx in 0..=dw {
            let opaque = dx < dw && {
                let sx = (dx as usize * fw) / dw as usize;
                bgra[(sy * fw + sx) * 4 + 3] >= 128
            };
            match (opaque, run) {
                (true, None) => run = Some(dx),
                (false, Some(start)) => {
                    rects.push(RECT {
                        left: x0 + start,
                        top: y0 + dy,
                        right: x0 + dx,
                        bottom: y0 + dy + 1,
                    });
                    run = None;
                }
                _ => {}
            }
        }
    }
    if rects.is_empty() {
        return None;
    }
    // RGNDATA: the header, then the rects.
    let header = std::mem::size_of::<RGNDATAHEADER>();
    let bytes = header + rects.len() * std::mem::size_of::<RECT>();
    let mut buf = vec![0u64; bytes.div_ceil(8)];
    unsafe {
        let data = buf.as_mut_ptr() as *mut RGNDATA;
        (*data).rdh = RGNDATAHEADER {
            dwSize: header as u32,
            iType: RDH_RECTANGLES,
            nCount: rects.len() as u32,
            nRgnSize: (rects.len() * std::mem::size_of::<RECT>()) as u32,
            rcBound: RECT { left: x0, top: y0, right: x0 + dw, bottom: y0 + dh },
        };
        let dst = (data as *mut u8).add(header) as *mut RECT;
        std::ptr::copy_nonoverlapping(rects.as_ptr(), dst, rects.len());
        let region = ExtCreateRegion(None, bytes as u32, data);
        (!region.is_invalid()).then_some(region)
    }
}

/// WebView2's error status as the pane's message; `None` for a navigation
/// that didn't fail (a cancel, a stop, an HTTP error page the server sent).
fn failure_reason(status: COREWEBVIEW2_WEB_ERROR_STATUS) -> Option<&'static str> {
    Some(match status {
        COREWEBVIEW2_WEB_ERROR_STATUS_CANNOT_CONNECT
        | COREWEBVIEW2_WEB_ERROR_STATUS_SERVER_UNREACHABLE => "Could not connect to the server.",
        COREWEBVIEW2_WEB_ERROR_STATUS_HOST_NAME_NOT_RESOLVED => {
            "A server with the specified hostname could not be found."
        }
        COREWEBVIEW2_WEB_ERROR_STATUS_TIMEOUT => "The request timed out.",
        COREWEBVIEW2_WEB_ERROR_STATUS_DISCONNECTED => "The Internet connection appears to be offline.",
        COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_ABORTED
        | COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_RESET => "The network connection was lost.",
        COREWEBVIEW2_WEB_ERROR_STATUS_ERROR_HTTP_INVALID_SERVER_RESPONSE => {
            "The server sent an invalid response."
        }
        COREWEBVIEW2_WEB_ERROR_STATUS_REDIRECT_FAILED => "Too many redirects.",
        COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_COMMON_NAME_IS_INCORRECT
        | COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_EXPIRED
        | COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_IS_INVALID
        | COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_REVOKED
        | COREWEBVIEW2_WEB_ERROR_STATUS_CLIENT_CERTIFICATE_CONTAINS_ERRORS => {
            "The certificate for this server is invalid."
        }
        COREWEBVIEW2_WEB_ERROR_STATUS_UNEXPECTED_ERROR => "The page could not be loaded.",
        _ => return None,
    })
}

/// A string WebView2 hands out (CoTaskMem-allocated; freed here).
fn take(get: impl FnOnce(*mut PWSTR) -> windows::core::Result<()>) -> String {
    let mut raw = PWSTR::null();
    if get(&mut raw).is_err() {
        return String::new();
    }
    CoTaskMemPWSTR::from(raw).to_string()
}

/// gpui's key name for a virtual key, as the catalog writes it.
fn vk_name(vk: u16) -> Option<String> {
    let named = |k: VIRTUAL_KEY| k.0 == vk;
    Some(match vk {
        0x30..=0x39 | 0x41..=0x5A => (vk as u8 as char).to_ascii_lowercase().to_string(),
        _ if named(VK_OEM_1) => ";".into(),
        _ if named(VK_OEM_2) => "/".into(),
        _ if named(VK_OEM_3) => "`".into(),
        _ if named(VK_OEM_4) => "[".into(),
        _ if named(VK_OEM_5) => "\\".into(),
        _ if named(VK_OEM_6) => "]".into(),
        _ if named(VK_OEM_7) => "'".into(),
        _ if named(VK_OEM_COMMA) => ",".into(),
        _ if named(VK_OEM_PERIOD) => ".".into(),
        _ if named(VK_OEM_MINUS) => "-".into(),
        _ if named(VK_OEM_PLUS) => "=".into(),
        _ if named(VK_UP) => "up".into(),
        _ if named(VK_DOWN) => "down".into(),
        _ if named(VK_LEFT) => "left".into(),
        _ if named(VK_RIGHT) => "right".into(),
        _ if named(VK_HOME) => "home".into(),
        _ if named(VK_END) => "end".into(),
        _ if named(VK_PRIOR) => "pageup".into(),
        _ if named(VK_NEXT) => "pagedown".into(),
        _ if named(VK_RETURN) => "enter".into(),
        _ if named(VK_TAB) => "tab".into(),
        _ if named(VK_ESCAPE) => "escape".into(),
        _ if named(VK_BACK) => "backspace".into(),
        _ if named(VK_DELETE) => "delete".into(),
        _ if named(VK_SPACE) => "space".into(),
        _ if (VK_F1.0..=VK_F24.0).contains(&vk) => format!("f{}", vk - VK_F1.0 + 1),
        _ => return None,
    })
}

// ---- punch-through ------------------------------------------------------------

#[derive(Clone, Default)]
struct Punch {
    holes: HoleSources,
    sprite: Option<Sprite>,
}

fn window_punch(parent: isize) -> Punch {
    PUNCH.with(|p| {
        p.borrow()
            .iter()
            .find(|(w, _)| *w == parent)
            .map(|(_, punch)| punch.clone())
            .unwrap_or_default()
    })
}

fn update_punch(window: &Window, change: impl FnOnce(&mut Punch) -> bool) {
    let Some(parent) = gpui_hwnd(window) else {
        return;
    };
    let parent = parent.0 as isize;
    let changed = PUNCH.with(|p| {
        let mut p = p.borrow_mut();
        let at = p.iter().position(|(w, _)| *w == parent);
        let mut punch = at.map(|i| p[i].1.clone()).unwrap_or_default();
        if !change(&mut punch) {
            return false;
        }
        let empty = punch.holes.is_empty() && punch.sprite.is_none();
        match (at, empty) {
            (Some(i), true) => {
                p.remove(i);
            }
            (Some(i), false) => p[i].1 = punch,
            (None, true) => {}
            (None, false) => p.push((parent, punch)),
        }
        true
    });
    if !changed {
        return;
    }
    let pages: Vec<Rc<Inner>> =
        PAGES.with(|pages| pages.borrow().iter().filter_map(Weak::upgrade).collect());
    for page in pages.iter().filter(|p| p.parent.0 as isize == parent) {
        page.refresh_mask();
    }
}

/// See the macOS version: what gpui draws over `window`'s pages this frame.
pub(crate) fn set_punch_through(window: &Window, holes: Vec<Hole>, sprite: Option<Sprite>) {
    let scale = window.scale_factor();
    let sprite = sprite.map(|s| Sprite { rect: snap_to_device(s.rect, scale), ..s });
    update_punch(window, |punch| {
        let holes_changed = punch.holes.set(HoleSource::Conductor, holes);
        let sprite_changed = punch.sprite != sprite;
        punch.sprite = sprite;
        holes_changed || sprite_changed
    });
}

pub(crate) fn set_tooltip_hole(window: &Window, hole: Option<Hole>) {
    update_punch(window, |punch| punch.holes.set(HoleSource::Tooltip, hole.into_iter().collect()));
}

/// WebView2's DevTools window: always there to open (see the module docs).
pub(crate) fn inspector_available() -> bool {
    true
}

pub(crate) fn window_is_minimized_or_hidden(window: &Window) -> bool {
    gpui_hwnd(window).is_some_and(|hwnd| unsafe { IsIconic(hwnd) }.as_bool())
}

/// A WebView2 page in a child window of gpui's. Main thread only.
pub(crate) struct NativeWebView {
    inner: Rc<Inner>,
}

impl NativeWebView {
    /// Adds a hidden page to `window` and starts loading `url` (nothing for
    /// an empty one). WebView2 starts asynchronously; anything asked of the
    /// page before it is up is applied when it is.
    pub(crate) fn new(window: &Window, url: &str, events: NativeEvents) -> anyhow::Result<Self> {
        let parent = gpui_hwnd(window).context("window has no Win32 handle")?;
        register_host_class();
        let host = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                HOST_CLASS,
                w!(""),
                WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                0,
                0,
                0,
                0,
                Some(parent),
                None,
                GetModuleHandleW(None).ok().map(Into::into),
                None,
            )
        }
        .map_err(|e| anyhow!("could not create the page's window: {e}"))?;
        let inner = Rc::new(Inner {
            parent,
            host,
            controller: RefCell::new(None),
            webview: RefCell::new(None),
            events,
            pending_url: RefCell::new((!url.is_empty()).then(|| url.to_string())),
            focus_when_ready: Cell::new(false),
            visible: Cell::new(false),
            size: Cell::new((0, 0)),
            page: Cell::new(None),
            scale: Cell::new(window.scale_factor()),
            screen_origin: Cell::new((i32::MIN, i32::MIN)),
            has_focus: Cell::new(false),
            pane_focused: Cell::new(false),
            navigation: RefCell::new(Navigations::default()),
            nav_urls: RefCell::new(Vec::new()),
            can_back: Cell::new(false),
            can_forward: Cell::new(false),
            mask: RefCell::new(None),
            inspector_side: Cell::new(None),
            inspector: RefCell::new(None),
            inspector_focus: Cell::new(false),
            inspector_generation: Cell::new(0),
        });
        if !url.is_empty() {
            inner.navigation.borrow_mut().begin(None);
        }
        if let Err(e) = with_environment(&inner) {
            unsafe {
                let _ = DestroyWindow(host);
            }
            return Err(e);
        }
        PAGES.with(|pages| {
            let mut pages = pages.borrow_mut();
            pages.retain(|p| p.strong_count() > 0);
            pages.push(Rc::downgrade(&inner));
        });
        Ok(Self { inner })
    }

    /// Places the page at `bounds` (gpui window coordinates, logical).
    pub(crate) fn set_frame(&self, bounds: Bounds<Pixels>, window: &Window) {
        let scale = window.scale_factor();
        self.inner.scale.set(scale);
        let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let px = |v: f32| (v * scale).round() as i32;
        let (l, t, r, b) = (px(x), px(y), px(x + w), px(y + h));
        let size = (r - l, b - t);
        let mut origin = POINT::default();
        unsafe {
            let _ = ClientToScreen(self.inner.parent, &mut origin);
        }
        let window_moved = self.inner.screen_origin.replace((origin.x, origin.y)) != (origin.x, origin.y);
        if window_moved && let Some(controller) = self.inner.controller.borrow().as_ref() {
            unsafe {
                let _ = controller.NotifyParentWindowPositionChanged();
            }
        }
        let moved = self.inner.page.get() != Some((x, y, w, h));
        if moved {
            unsafe {
                let _ = SetWindowPos(
                    self.inner.host,
                    Some(HWND_TOP),
                    l,
                    t,
                    size.0,
                    size.1,
                    SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                );
            }
            if self.inner.size.replace(size) != size {
                self.inner.layout();
            }
            if let Some(controller) = self.inner.controller.borrow().as_ref() {
                // Where WebView2 puts its own popups (a <select>'s list).
                unsafe {
                    let _ = controller.NotifyParentWindowPositionChanged();
                }
            }
            self.inner.page.set(Some((x, y, w, h)));
        }
        self.inner.refresh_mask();
    }

    pub(crate) fn set_visible(&self, visible: bool) {
        if visible {
            self.inner.visible.set(true);
            unsafe {
                let _ = ShowWindow(self.inner.host, SW_SHOWNA);
                if let Some(controller) = self.inner.controller.borrow().as_ref() {
                    let _ = controller.SetIsVisible(true);
                }
                if let Some(controller) = self.inner.inspector.borrow().as_ref() {
                    let _ = controller.SetIsVisible(true);
                }
            }
        } else {
            self.hide();
        }
    }

    /// Hides the page, handing the keyboard back to gpui first; returns
    /// whether the page had it.
    pub(crate) fn hide(&self) -> bool {
        let had_focus = self.release_focus();
        self.inner.visible.set(false);
        unsafe {
            if let Some(controller) = self.inner.controller.borrow().as_ref() {
                let _ = controller.SetIsVisible(false);
            }
            if let Some(controller) = self.inner.inspector.borrow().as_ref() {
                let _ = controller.SetIsVisible(false);
            }
            let _ = ShowWindow(self.inner.host, SW_HIDE);
        }
        had_focus
    }

    pub(crate) fn set_pane_focused(&self, focused: bool) {
        self.inner.pane_focused.set(focused);
    }

    /// If the page has the keyboard, give it to gpui's window.
    pub(crate) fn release_focus(&self) -> bool {
        self.inner.focus_when_ready.set(false);
        let page = self.inner.has_focus.replace(false);
        let inspector = self.inner.inspector_focus.replace(false);
        let had = page || inspector;
        if had {
            unsafe {
                let _ = SetFocus(Some(self.inner.parent));
            }
        }
        had
    }

    pub(crate) fn navigation(&self) -> u64 {
        self.inner.navigation.borrow().current()
    }

    pub(crate) fn load(&self, url: &str) {
        self.inner.navigation.borrow_mut().begin(None);
        self.inner.navigate(url);
    }

    pub(crate) fn back(&self) {
        self.inner.navigation.borrow_mut().begin(None);
        if let Some(webview) = self.inner.webview.borrow().as_ref() {
            unsafe {
                let _ = webview.GoBack();
            }
        }
    }

    pub(crate) fn forward(&self) {
        self.inner.navigation.borrow_mut().begin(None);
        if let Some(webview) = self.inner.webview.borrow().as_ref() {
            unsafe {
                let _ = webview.GoForward();
            }
        }
    }

    pub(crate) fn reload(&self) {
        self.inner.navigation.borrow_mut().begin(None);
        if let Some(webview) = self.inner.webview.borrow().as_ref() {
            unsafe {
                let _ = webview.Reload();
            }
        }
    }

    pub(crate) fn stop(&self) {
        if let Some(webview) = self.inner.webview.borrow().as_ref() {
            unsafe {
                let _ = webview.Stop();
            }
        }
    }

    /// Gives the page the keyboard (once WebView2 is up, if it isn't yet).
    pub(crate) fn focus(&self) {
        match self.inner.controller.borrow().as_ref() {
            Some(controller) => unsafe {
                let _ = controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
            },
            None => self.inner.focus_when_ready.set(true),
        }
    }

    pub(crate) fn can_go_back(&self) -> bool {
        self.inner.can_back.get()
    }

    pub(crate) fn can_go_forward(&self) -> bool {
        self.inner.can_forward.get()
    }

    /// WebView2 can't say whether its DevTools window is open.
    /// A docked inspector is open (WebView2 can't say whether its own
    /// DevTools window is).
    pub(crate) fn inspector_visible(&self) -> bool {
        self.inner.inspector_side.get().is_some()
    }

    /// The Developer tools button: closes a docked inspector, else opens
    /// one where Settings → General says (docked, or WebView2's window).
    pub(crate) fn toggle_inspector(&self) -> bool {
        if self.inspector_visible() {
            return self.inner.close_inspector();
        }
        match placement_side(crate::browser_pane::inspector_placement()) {
            Some(side) => self.inner.open_inspector(side),
            None => self.inner.inspector_window(),
        }
        false
    }

    /// Where the docked inspector is. It follows Settings → General while
    /// open (the pane polls this), so a new placement applies at once.
    pub(crate) fn inspector_place(&self) -> InspectorPlace {
        let Some(side) = self.inner.inspector_side.get() else {
            return InspectorPlace::Unsupported;
        };
        match placement_side(crate::browser_pane::inspector_placement()) {
            Some(want) if want != side => {
                self.inner.open_inspector(want);
                InspectorPlace::Docked(Some(want))
            }
            Some(_) => InspectorPlace::Docked(Some(side)),
            None => {
                self.inner.close_inspector();
                self.inner.inspector_window();
                InspectorPlace::OwnWindow
            }
        }
    }

    pub(crate) fn inspector_has_focus(&self) -> bool {
        self.inner.inspector_focus.get()
    }

    pub(crate) fn move_inspector(&self, to: InspectorMove) {
        match to {
            InspectorMove::AttachBottom => self.inner.open_inspector(DockSide::Bottom),
            InspectorMove::Dock(side) => self.inner.open_inspector(side),
            InspectorMove::Detach => {
                self.inner.close_inspector();
                self.inner.inspector_window();
            }
        }
    }

    /// Nothing strands the keyboard on Windows (see `release_focus`).
    pub(crate) fn recover_focus(&self) {}
}

/// The docked side for a placement; `None` for its own window.
fn placement_side(placement: crate::browser_pane::InspectorPlacement) -> Option<DockSide> {
    use crate::browser_pane::InspectorPlacement as P;
    match placement {
        P::Bottom => Some(DockSide::Bottom),
        P::Left => Some(DockSide::Left),
        P::Right => Some(DockSide::Right),
        P::Window => None,
    }
}

impl Drop for NativeWebView {
    fn drop(&mut self) {
        self.release_focus();
        // The docked inspector is this page's (as on macOS): it goes first.
        self.inner.close_inspector();
        unsafe {
            if let Some(controller) = self.inner.controller.borrow_mut().take() {
                let _ = controller.Close();
            }
            let _ = DestroyWindow(self.inner.host);
        }
        self.inner.webview.borrow_mut().take();
        let me = Rc::as_ptr(&self.inner);
        PAGES.with(|pages| pages.borrow_mut().retain(|p| p.strong_count() > 0 && p.as_ptr() != me));
    }
}
