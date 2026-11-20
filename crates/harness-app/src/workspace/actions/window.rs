//! Window-level actions: splitters and panel drags, saving prefs, the quit
//! prompt, shortcuts, and appearance (theme and fonts).

use super::*;

impl HarnessWorkspace {
    /// A draggable splitter. `target` says which dimension it drives.
    ///
    /// 6px wide with a 1px visible line: narrower than this and it is fiddly to
    /// grab, wider and it reads as a gap in the chrome.
    pub(crate) fn splitter(
        &self,
        hx: &HarnessTheme,
        target: Drag,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Both remaining splitters are vertical dividers between columns.
        let vertical = true;
        let active = self.drag.map(|d| d.target) == Some(target);
        let start_value = match target {
            Drag::Sidebar => self.prefs.sidebar_width,
            Drag::DiffPanel => self.prefs.diff_width,
            Drag::DiffList => self.prefs.diff_list_width,
        };

        let base = div()
            .id(match target {
                Drag::Sidebar => "split-sidebar",
                Drag::DiffPanel => "split-diff",
                Drag::DiffList => "split-diff-list",
            })
            .flex_none()
            .cursor(if vertical {
                gpui::CursorStyle::ResizeLeftRight
            } else {
                gpui::CursorStyle::ResizeUpDown
            })
            .when(active, |d| d.bg(hx.accent))
            .when(!active, |d| d.hover(|h| h.bg(hx.accent)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |ws, ev: &MouseDownEvent, _, cx| {
                    // Double-clicking the tools panel's handle maximizes it —
                    // the conventional "make this pane as big as it goes".
                    if target == Drag::DiffPanel && ev.click_count >= 2 {
                        ws.drag = None;
                        ws.toggle_tools_maximized(cx);
                        return;
                    }
                    ws.drag = Some(DragState {
                        target,
                        start: if vertical {
                            ev.position.x
                        } else {
                            ev.position.y
                        },
                        start_value,
                    });
                    cx.notify();
                }),
            );

        if vertical {
            base.w(px(6.0)).h_full()
        } else {
            base.h(px(6.0)).w_full()
        }
    }

    /// Apply an in-progress drag. Called from the root element so the pointer
    /// can leave the 6px handle without dropping the drag.
    pub(crate) fn drag_move(
        &mut self,
        ev: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.conductor_drag_move(ev, window, cx) {
            return;
        }
        let Some(drag) = self.drag else { return };
        // The button can be released outside the window, in which case no
        // mouse-up ever reaches us; treat "moving with no button" as the end.
        if ev.pressed_button != Some(MouseButton::Left) {
            self.end_drag(cx);
            return;
        }
        let (pos, sign) = match drag.target {
            Drag::Sidebar => (ev.position.x, 1.0),
            // The diff panel is right-anchored, so dragging left widens it.
            Drag::DiffPanel => (ev.position.x, -1.0),
            // The file list is left-anchored inside the surface, so dragging
            // right widens it — the opposite of the right-anchored dock.
            Drag::DiffList => (ev.position.x, 1.0),
        };
        let delta = f32::from(pos - drag.start) * sign;
        let next = drag.start_value + delta;
        match drag.target {
            Drag::Sidebar => self.prefs.sidebar_width = next,
            // The dock's real ceiling is the window, not a constant: it may be
            // dragged as wide as this display allows while still leaving the
            // center pane usable. Applied before `sanitized` so the persisted
            // width is the one actually shown.
            Drag::DiffPanel => {
                let ceiling = max_diff_width(
                    f32::from(window.viewport_size().width),
                    self.prefs.sidebar_width,
                    self.prefs.sidebar_open,
                );
                self.prefs.diff_width = next.min(ceiling);
            }
            Drag::DiffList => self.prefs.diff_list_width = next,
        }
        // Clamp every frame, not on release: a panel must never be draggable
        // to zero width and become unrecoverable.
        self.prefs = self.prefs.clone().sanitized();
        cx.notify();
    }

    pub(crate) fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            save_prefs_to(&self.prefs_path, &self.prefs);
            cx.notify();
        }
    }

    pub(crate) fn save_prefs(&mut self, cx: &App) {
        self.prefs.dark = theme(cx).is_dark();
        save_prefs_to(&self.prefs_path, &self.prefs);
    }

    /// Live openOrchestra tmux sessions (our `harness_` prefix). Also catches
    /// orphaned sessions whose tab is gone, so "close terminals" really leaves
    /// nothing of ours running.
    pub(crate) fn live_harness_sessions(&self) -> Vec<String> {
        if !self.has_tmux {
            return Vec::new();
        }
        crate::tmux::live_sessions()
            .into_iter()
            .filter(|n| n.starts_with("harness_"))
            .collect()
    }

    /// Entry point for both quit paths (⌘Q / menu, and the window close button).
    /// Returns whether the app may close *now*: `true` when we've started the
    /// quit, `false` when we've instead opened the keep-or-close prompt and are
    /// waiting on the user.
    pub(crate) fn request_quit(&mut self, cx: &mut Context<Self>) -> bool {
        self.save_panes(cx);
        let sessions = self.live_harness_sessions();
        // Nothing of ours is running — no reason to ask.
        if sessions.is_empty() {
            self.remote.shutdown_detached();
            cx.quit();
            return true;
        }
        match self.prefs.close_terminals_on_exit {
            // A remembered choice skips the prompt entirely.
            Some(kill) => {
                if kill {
                    self.kill_sessions(&sessions);
                }
                self.remote.shutdown_detached();
                cx.quit();
                true
            }
            None => {
                self.close_prompt = Some(ClosePrompt {
                    remember: false,
                    session_count: sessions.len(),
                });
                cx.notify();
                false
            }
        }
    }

    pub(crate) fn open_first_time_checks(&mut self, cx: &mut Context<Self>) {
        // Always re-probe on open: the point of reaching for it is
        // usually that something changed since startup.
        self.preflight_open = true;
        self.rerun_preflight(cx);
        cx.notify();
    }

    pub(crate) fn toggle_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_open {
            self.close_settings(window, cx);
        } else {
            self.open_settings(self.settings_section, window, cx);
        }
    }

    pub(crate) fn open_settings(
        &mut self,
        section: SettingsSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Refuse while a required tool is missing: Settings sits behind
        // the blocking screen, so opening it would only mean a menu
        // item that visibly does nothing.
        if crate::preflight::blocked(&self.preflight) {
            return;
        }
        self.settings_open = true;
        self.settings_section = section;
        self.settings_focus.reset_scroll();
        let focus = self.settings_focus.control_focus(section.focus_id(), cx);
        window.focus(&focus, cx);
        if section == SettingsSection::Diff {
            self.ensure_review_model_catalog(cx);
        }
        cx.notify();
    }

    pub(crate) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_open = false;
        self.leave_settings_control();
        self.remote_close_setup();
        if let Some(dialog) = &self.new_task {
            window.focus(&dialog.name.focus_handle(cx), cx);
        } else {
            self.focus_active_pane(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn leave_settings_control(&mut self) {
        self.rebinding = None;
        if matches!(
            self.edit_kind(),
            Some(EditKind::AgentCmd(_) | EditKind::NewAgent | EditKind::RemotePort)
        ) {
            self.edit = None;
        }
    }

    /// Resolve the quit prompt. `keep` leaves the tmux sessions running
    /// (detached); otherwise they're killed. A ticked "remember" persists the
    /// choice so the prompt won't show again.
    pub(crate) fn resolve_quit(&mut self, keep: bool, cx: &mut Context<Self>) {
        let remember = self.close_prompt.as_ref().is_some_and(|p| p.remember);
        self.close_prompt = None;
        if remember {
            self.prefs.close_terminals_on_exit = Some(!keep);
            save_prefs_to(&self.prefs_path, &self.prefs);
        }
        if !keep {
            self.kill_sessions(&self.live_harness_sessions());
        }
        // Stop the phone server off the main thread before quitting.
        self.remote.shutdown_detached();
        cx.quit();
    }

    /// Dismiss the quit prompt without quitting.
    pub(crate) fn cancel_quit(&mut self, cx: &mut Context<Self>) {
        self.close_prompt = None;
        cx.notify();
    }

    pub(crate) fn kill_sessions(&self, names: &[String]) {
        for name in names {
            crate::tmux::kill_session(name);
        }
    }

    /// Re-install the whole keymap and menus from the catalog + overrides.
    /// Clear-then-bind, not bind-on-top: gpui breaks precedence ties by
    /// recency, so stale chords would keep firing their old actions.
    pub(crate) fn apply_shortcuts(&self, cx: &mut Context<Self>) {
        cx.clear_key_bindings();
        crate::install_keymap(&self.prefs.shortcuts, cx);
        crate::install_menus(cx);
    }

    /// One captured chord while a shortcut row is armed.
    pub(crate) fn rebind_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.rebinding.clone() else {
            return;
        };
        let ks = &event.keystroke;
        if ks.key == "escape" && !ks.modifiers.modified() {
            self.rebinding = None;
            cx.notify();
            cx.stop_propagation();
            return;
        }
        // A bare modifier press is the chord being formed, not the chord.
        if ["shift", "ctrl", "alt", "cmd", "platform", "function"].contains(&ks.key.as_str()) {
            cx.stop_propagation();
            return;
        }
        let mut chord = String::new();
        if ks.modifiers.control {
            chord.push_str("ctrl-");
        }
        if ks.modifiers.alt {
            chord.push_str("alt-");
        }
        if ks.modifiers.shift {
            chord.push_str("shift-");
        }
        if ks.modifiers.platform {
            chord.push_str("cmd-");
        }
        chord.push_str(&ks.key);
        self.prefs.shortcuts.insert(id, chord);
        save_prefs_to(&self.prefs_path, &self.prefs);
        self.rebinding = None;
        self.apply_shortcuts(cx);
        cx.notify();
        cx.stop_propagation();
    }

    /// Apply a zed theme by registry name (`None` = the harness pair).
    pub(crate) fn set_zed_theme(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        self.prefs.zed_theme = name;
        self.apply_appearance(cx);
        self.save_prefs(cx);
        cx.refresh_windows();
        cx.notify();
    }

    /// Set a font override. `mono` covers the buffer, the terminal, and the
    /// chrome's monospace; `!mono` is the interface font.
    pub(crate) fn set_font(&mut self, mono: bool, name: String, cx: &mut Context<Self>) {
        if mono {
            self.prefs.font_mono = Some(name);
        } else {
            self.prefs.font_ui = Some(name);
        }
        self.apply_appearance(cx);
        self.save_prefs(cx);
        cx.refresh_windows();
        cx.notify();
    }

    /// Re-derive every appearance global from prefs: theme (harness pair or a
    /// zed registry theme), chrome tokens, fonts, and the zed settings JSON.
    /// One function on purpose — called at startup and after every settings
    /// change, so "restored" and "just picked" cannot diverge.
    pub(crate) fn apply_appearance(&mut self, cx: &mut Context<Self>) {
        // Fonts first: leaked to &'static str, matching HarnessTheme's fields
        // and theme.rs's own resolve_font. Bounded by the number of font
        // changes in a session.
        let cur = theme(cx).clone();
        let font_ui: &'static str = match &self.prefs.font_ui {
            Some(name) => Box::leak(name.clone().into_boxed_str()),
            None => cur.font_ui,
        };
        let font_mono: &'static str = match &self.prefs.font_mono {
            Some(name) => Box::leak(name.clone().into_boxed_str()),
            None => cur.font_mono,
        };

        let zed_theme = self
            .prefs
            .zed_theme
            .as_ref()
            .and_then(|name| ::theme::ThemeRegistry::global(cx).get(name).ok());
        let theme_name = match zed_theme {
            Some(zed) => {
                let name = zed.name.to_string();
                ::theme::GlobalTheme::update_theme(cx, zed.clone());
                // Parsing is not coloring (see harness_zed_theme::install):
                // the registry's HighlightMaps rebuild from the new theme.
                ::workspace::AppState::global(cx)
                    .languages
                    .set_theme(zed.clone());
                let hx = crate::harness_zed_theme::harness_from_zed(&zed, font_ui, font_mono);
                cx.set_global(hx);
                name
            }
            None => {
                let mut hx = if self.prefs.dark {
                    HarnessTheme::dark(cx)
                } else {
                    HarnessTheme::light(cx)
                };
                hx.font_ui = font_ui;
                hx.font_mono = font_mono;
                let name = crate::harness_zed_theme::theme_name(&hx).to_string();
                crate::harness_zed_theme::install(&hx, cx);
                cx.set_global(hx);
                name
            }
        };
        // Theme name goes into ThemeSettings too — not only GlobalTheme — so a
        // later reload_theme (Workspace window-appearance observer) keeps the
        // editor on the same palette the chrome is already using.
        crate::apply_zed_settings(
            cx,
            font_ui,
            font_mono,
            self.prefs.font_size.unwrap_or(13.0),
            &theme_name,
            self.prefs.show_ignored_files,
        );
    }
}
