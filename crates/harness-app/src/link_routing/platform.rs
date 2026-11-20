//! `LinkRoutingPlatform`: gpui's real platform with `open_url` intercepted.
//!
//! zed code the app hosts (the terminal's ⌘-click, editor hover links,
//! markdown preview …) opens links with `cx.open_url`, which goes straight to
//! the platform. Wrapping the platform is the one place that sees all of them.
//! A local or dev address (see [`super::should_route`]) is queued for the
//! workspace, which opens it in a browser tab; everything else goes to the
//! real platform, i.e. the default browser.
//!
//! Every other `Platform` method forwards to the real platform, including the
//! ones the trait gives a default body: relying on a default would silently
//! swap the real platform's behavior for a no-op. A required method added at a
//! zed bump fails the build here; a *defaulted* one doesn't, so diff this impl
//! against the trait's method list at each bump.

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::channel::oneshot;
use gpui::{
    Action, ActivationPolicy, ActivityGuard, AnyWindowHandle, AppLifecyclePhase,
    BackgroundExecutor, ClipboardItem, ClipboardReadError, CursorStyle, ForegroundExecutor,
    GraphicalEnvironment, Keymap, Menu, MenuItem, OwnedMenu, PathPromptOptions, Platform,
    PlatformDisplay, PlatformGestures, PlatformInfo, PlatformKeyboardLayout,
    PlatformKeyboardMapper, PlatformTextSystem, PlatformWindow, PowerMode, ScreenCaptureSource,
    SystemNotification, SystemNotificationResponse, Task, ThermalState, WindowAppearance,
    WindowButtonLayout, WindowParams, WindowingRequest,
};
use smallvec::SmallVec;

thread_local! {
    /// The real platform, for opens that must not be routed. gpui's platform
    /// is `Rc` and lives on the main thread, so a thread-local is its home.
    static INNER: RefCell<Option<Rc<dyn Platform>>> = const { RefCell::new(None) };
    /// The routed URLs' receiving end, until the workspace takes it.
    static ROUTED_RX: RefCell<Option<UnboundedReceiver<String>>> = const { RefCell::new(None) };
}

pub struct LinkRoutingPlatform {
    inner: Rc<dyn Platform>,
    routed: UnboundedSender<String>,
    /// The "Open links in a browser tab" setting, read at each open.
    enabled: fn() -> bool,
}

impl LinkRoutingPlatform {
    /// Wraps `inner` and makes it this thread's target for
    /// [`open_with_inner`]; a fresh routed channel replaces any earlier one.
    pub fn new(inner: Rc<dyn Platform>) -> Self {
        Self::with_setting(inner, super::routing_enabled)
    }

    /// [`Self::new`] reading the setting from `enabled` (tests pin it rather
    /// than share the process-wide flag).
    pub(crate) fn with_setting(inner: Rc<dyn Platform>, enabled: fn() -> bool) -> Self {
        let (routed, rx) = unbounded();
        INNER.with(|i| *i.borrow_mut() = Some(inner.clone()));
        ROUTED_RX.with(|r| *r.borrow_mut() = Some(rx));
        Self {
            inner,
            routed,
            enabled,
        }
    }
}

/// The routed URLs, in order. Taken once: the workspace that takes it opens
/// every routed link.
pub fn take_routed_receiver() -> Option<UnboundedReceiver<String>> {
    ROUTED_RX.with(|r| r.borrow_mut().take())
}

/// Hands `url` to the real platform, bypassing routing. False when no
/// [`LinkRoutingPlatform`] was created on this thread.
pub(super) fn open_with_inner(url: &str) -> bool {
    // Cloned out first: the platform's open_url must not run under the borrow.
    let Some(inner) = INNER.with(|i| i.borrow().clone()) else {
        return false;
    };
    inner.open_url(url);
    true
}

impl Platform for LinkRoutingPlatform {
    fn open_url(&self, url: &str) {
        if super::should_route(url, (self.enabled)()) {
            // A drain that's gone (its window closed) must not eat the click.
            match self.routed.unbounded_send(url.to_string()) {
                Ok(()) => return,
                Err(e) => log::info!("link routing: no browser tab to open in ({e})"),
            }
        }
        self.inner.open_url(url);
    }

    fn platform_info(&self) -> PlatformInfo {
        self.inner.platform_info()
    }
    fn background_executor(&self) -> BackgroundExecutor {
        self.inner.background_executor()
    }
    fn foreground_executor(&self) -> ForegroundExecutor {
        self.inner.foreground_executor()
    }
    fn text_system(&self) -> Arc<dyn PlatformTextSystem> {
        self.inner.text_system()
    }
    fn run(&self, on_finish_launching: Box<dyn 'static + FnOnce()>) {
        self.inner.run(on_finish_launching)
    }
    fn quit(&self) {
        self.inner.quit()
    }
    fn set_initial_windowing(&self, request: WindowingRequest) {
        self.inner.set_initial_windowing(request)
    }
    fn request_windowing(&self, request: WindowingRequest) -> Task<anyhow::Result<()>> {
        self.inner.request_windowing(request)
    }
    fn restart(&self, binary_path: Option<PathBuf>, arguments: Vec<OsString>) {
        self.inner.restart(binary_path, arguments)
    }
    fn activate(&self, ignoring_other_apps: bool) {
        self.inner.activate(ignoring_other_apps)
    }
    fn set_activation_policy(&self, policy: ActivationPolicy) {
        self.inner.set_activation_policy(policy)
    }
    fn hide(&self) {
        self.inner.hide()
    }
    fn hide_other_apps(&self) {
        self.inner.hide_other_apps()
    }
    fn unhide_other_apps(&self) {
        self.inner.unhide_other_apps()
    }
    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
        self.inner.displays()
    }
    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        self.inner.primary_display()
    }
    fn on_displays_changed(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_displays_changed(callback)
    }
    fn active_window(&self) -> Option<AnyWindowHandle> {
        self.inner.active_window()
    }
    fn window_stack(&self) -> Option<Vec<AnyWindowHandle>> {
        self.inner.window_stack()
    }
    fn is_screen_capture_supported(&self) -> bool {
        self.inner.is_screen_capture_supported()
    }
    fn screen_capture_sources(
        &self,
    ) -> oneshot::Receiver<anyhow::Result<Vec<Rc<dyn ScreenCaptureSource>>>> {
        self.inner.screen_capture_sources()
    }
    fn open_window(
        &self,
        handle: AnyWindowHandle,
        options: WindowParams,
    ) -> anyhow::Result<Box<dyn PlatformWindow>> {
        self.inner.open_window(handle, options)
    }
    fn window_appearance(&self) -> WindowAppearance {
        self.inner.window_appearance()
    }
    fn set_window_appearance(&self, appearance: Option<WindowAppearance>) {
        self.inner.set_window_appearance(appearance)
    }
    fn button_layout(&self) -> Option<WindowButtonLayout> {
        self.inner.button_layout()
    }
    fn on_open_urls(&self, callback: Box<dyn FnMut(Vec<String>)>) {
        self.inner.on_open_urls(callback)
    }
    fn register_url_scheme(&self, url: &str) -> Task<gpui::Result<()>> {
        self.inner.register_url_scheme(url)
    }
    fn prompt_for_paths(
        &self,
        options: PathPromptOptions,
    ) -> oneshot::Receiver<gpui::Result<Option<Vec<PathBuf>>>> {
        self.inner.prompt_for_paths(options)
    }
    fn prompt_for_new_path(
        &self,
        directory: &Path,
        suggested_name: Option<&str>,
    ) -> oneshot::Receiver<gpui::Result<Option<PathBuf>>> {
        self.inner.prompt_for_new_path(directory, suggested_name)
    }
    fn can_select_mixed_files_and_dirs(&self) -> bool {
        self.inner.can_select_mixed_files_and_dirs()
    }
    fn reveal_path(&self, path: &Path) {
        self.inner.reveal_path(path)
    }
    fn open_with_system(&self, path: &Path) {
        self.inner.open_with_system(path)
    }
    fn on_quit(&self, callback: Box<dyn FnMut() -> bool>) {
        self.inner.on_quit(callback)
    }
    fn on_reopen(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_reopen(callback)
    }
    fn on_system_sleep(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_system_sleep(callback)
    }
    fn on_system_wake(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_system_wake(callback)
    }
    fn on_app_lifecycle(&self, callback: Box<dyn FnMut(AppLifecyclePhase)>) {
        self.inner.on_app_lifecycle(callback)
    }
    fn on_memory_warning(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_memory_warning(callback)
    }
    fn gestures(&self) -> Option<Rc<dyn PlatformGestures>> {
        self.inner.gestures()
    }
    fn set_menus(&self, menus: Vec<Menu>, keymap: &Keymap) {
        self.inner.set_menus(menus, keymap)
    }
    fn get_menus(&self) -> Option<Vec<OwnedMenu>> {
        self.inner.get_menus()
    }
    fn set_dock_menu(&self, menu: Vec<MenuItem>, keymap: &Keymap) {
        self.inner.set_dock_menu(menu, keymap)
    }
    fn perform_dock_menu_action(&self, action: usize) {
        self.inner.perform_dock_menu_action(action)
    }
    fn add_recent_document(&self, path: &Path) {
        self.inner.add_recent_document(path)
    }
    fn update_jump_list(
        &self,
        menus: Vec<MenuItem>,
        entries: Vec<SmallVec<[PathBuf; 2]>>,
    ) -> Task<Vec<SmallVec<[PathBuf; 2]>>> {
        self.inner.update_jump_list(menus, entries)
    }
    fn on_app_menu_action(&self, callback: Box<dyn FnMut(&dyn Action)>) {
        self.inner.on_app_menu_action(callback)
    }
    fn on_will_open_app_menu(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_will_open_app_menu(callback)
    }
    fn on_validate_app_menu_command(&self, callback: Box<dyn FnMut(&dyn Action) -> bool>) {
        self.inner.on_validate_app_menu_command(callback)
    }
    fn thermal_state(&self) -> ThermalState {
        self.inner.thermal_state()
    }
    fn on_thermal_state_change(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_thermal_state_change(callback)
    }
    fn prevent_idle_sleep(&self, reason: &str) -> Task<gpui::Result<ActivityGuard>> {
        self.inner.prevent_idle_sleep(reason)
    }
    fn power_mode(&self) -> PowerMode {
        self.inner.power_mode()
    }
    fn on_power_mode_change(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_power_mode_change(callback)
    }
    fn set_app_identity(&self, identifier: &str, name: &str) {
        self.inner.set_app_identity(identifier, name)
    }
    fn show_system_notification(&self, notification: SystemNotification) {
        self.inner.show_system_notification(notification)
    }
    fn dismiss_system_notification(&self, tag: &str) {
        self.inner.dismiss_system_notification(tag)
    }
    fn on_system_notification_response(
        &self,
        callback: Box<dyn FnMut(SystemNotificationResponse)>,
    ) {
        self.inner.on_system_notification_response(callback)
    }
    fn compositor_name(&self) -> &'static str {
        self.inner.compositor_name()
    }
    fn graphical_environment(&self) -> Option<GraphicalEnvironment> {
        self.inner.graphical_environment()
    }
    fn app_path(&self) -> gpui::Result<PathBuf> {
        self.inner.app_path()
    }
    fn path_for_auxiliary_executable(&self, name: &str) -> gpui::Result<PathBuf> {
        self.inner.path_for_auxiliary_executable(name)
    }
    fn set_cursor_style(&self, style: CursorStyle) {
        self.inner.set_cursor_style(style)
    }
    fn hide_cursor_until_mouse_moves(&self) {
        self.inner.hide_cursor_until_mouse_moves()
    }
    fn is_cursor_visible(&self) -> bool {
        self.inner.is_cursor_visible()
    }
    fn should_auto_hide_scrollbars(&self) -> bool {
        self.inner.should_auto_hide_scrollbars()
    }
    fn read_from_clipboard(&self) -> Option<ClipboardItem> {
        self.inner.read_from_clipboard()
    }
    fn write_to_clipboard(&self, item: ClipboardItem) {
        self.inner.write_to_clipboard(item)
    }
    fn read_from_clipboard_async(
        &self,
    ) -> Task<gpui::Result<Option<ClipboardItem>, ClipboardReadError>> {
        self.inner.read_from_clipboard_async()
    }
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn read_from_primary(&self) -> Option<ClipboardItem> {
        self.inner.read_from_primary()
    }
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn write_to_primary(&self, item: ClipboardItem) {
        self.inner.write_to_primary(item)
    }
    #[cfg(target_os = "macos")]
    fn read_from_find_pasteboard(&self) -> Option<ClipboardItem> {
        self.inner.read_from_find_pasteboard()
    }
    #[cfg(target_os = "macos")]
    fn write_to_find_pasteboard(&self, item: ClipboardItem) {
        self.inner.write_to_find_pasteboard(item)
    }
    fn write_credentials(
        &self,
        url: &str,
        username: &str,
        password: &[u8],
    ) -> Task<gpui::Result<()>> {
        self.inner.write_credentials(url, username, password)
    }
    #[cfg(target_os = "macos")]
    fn write_credentials_if_not_exists(
        &self,
        url: &str,
        username: &str,
        password: &[u8],
    ) -> Task<gpui::Result<bool>> {
        self.inner
            .write_credentials_if_not_exists(url, username, password)
    }
    fn read_credentials(&self, url: &str) -> Task<gpui::Result<Option<(String, Vec<u8>)>>> {
        self.inner.read_credentials(url)
    }
    fn delete_credentials(&self, url: &str) -> Task<gpui::Result<()>> {
        self.inner.delete_credentials(url)
    }
    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
        self.inner.keyboard_layout()
    }
    fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper> {
        self.inner.keyboard_mapper()
    }
    fn on_keyboard_layout_change(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_keyboard_layout_change(callback)
    }
}
