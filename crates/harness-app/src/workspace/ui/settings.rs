//! Split out of `workspace.rs` (see its module docs). Pure relocation:
//! `impl HarnessWorkspace` methods for this feature area.

use super::super::*;
use super::{ModalFocus, modal_button, modal_control, toggled};

/// A Settings section heading. Shared by every pane (and `settings_remote`).
pub(super) fn section_title(label: &str, hx: &HarnessTheme) -> gpui::Div {
    div()
        .pt_3()
        .pb_1()
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(hx.tx2)
        .child(label.to_uppercase())
}

/// A selectable option chip. Shared by every pane (and `settings_remote`).
pub(super) fn chip(label: String, selected: bool, hx: &HarnessTheme) -> gpui::Div {
    div()
        .px_2()
        .py_0p5()
        .rounded_md()
        .text_xs()
        .cursor_pointer()
        .border_1()
        .border_color(if selected { hx.accent } else { hx.border })
        .bg(if selected {
            tint(hx.accent, 0.16)
        } else {
            hx.surface
        })
        .text_color(if selected { hx.accent } else { hx.tx1 })
        .hover(|d| d.bg(hx.surface_hover))
        .child(label)
}

/// One short line of help under a setting. Shared by every pane.
pub(super) fn hint(text: &'static str, hx: &HarnessTheme) -> gpui::Div {
    div().pb_1().text_xs().text_color(hx.tx2).child(text)
}

/// An on/off switch: a pill with a knob that sits right and fills with the
/// accent when on. Every on/off setting uses it, so on and off read the same
/// way everywhere.
pub(super) fn switch(on: bool, hx: &HarnessTheme) -> gpui::Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .when(on, |d| d.justify_end())
        .w(px(28.0))
        .h(px(16.0))
        .px(px(2.0))
        .rounded_full()
        .border_1()
        .border_color(if on { hx.accent } else { hx.border })
        .bg(if on { hx.accent } else { hx.surface_hover })
        .cursor_pointer()
        .child(div().size(px(10.0)).rounded_full().bg(if on {
            gpui::white()
        } else {
            hx.tx2.into()
        }))
}

/// An on/off setting: what it is, one short line on what it does, and the
/// switch on the right. The whole row toggles.
pub(super) fn toggle_row(
    focus: &ModalFocus,
    id: &'static str,
    title: &'static str,
    detail: &'static str,
    on: bool,
    hx: &HarnessTheme,
    cx: &mut App,
) -> gpui::Stateful<gpui::Div> {
    focus
        .scroll_control(id, title, gpui::Role::Switch, hx, cx)
        .aria_description(detail)
        .aria_toggled(toggled(on))
        .flex()
        .items_center()
        .gap_3()
        .py_1()
        .mb_1()
        .cursor_pointer()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_sm().text_color(hx.tx0).child(title))
                .child(div().text_xs().text_color(hx.tx2).child(detail)),
        )
        .child(switch(on, hx))
}

/// A small square glyph button with a tooltip, for per-row actions.
fn icon_button(
    focus: &ModalFocus,
    id: String,
    glyph: &'static str,
    label: String,
    hx: &HarnessTheme,
    hover: Rgba,
    cx: &mut App,
) -> gpui::Stateful<gpui::Div> {
    focus
        .scroll_button(id, label.clone(), hx, cx)
        .flex_none()
        .w(px(20.0))
        .py_0p5()
        .rounded_md()
        .text_center()
        .text_sm()
        .text_color(hx.tx2)
        .cursor_pointer()
        .hover(move |d| d.bg(hx.surface_hover).text_color(hover))
        .tooltip(tip(label))
        .child(glyph)
}

impl HarnessWorkspace {
    /// Put the keyboard on `section`'s tab in the Settings rail, a control
    /// that survives any change to the section's contents. Called before
    /// removing the focused control (a removed agent's ×, or a Reset button
    /// that goes away or inert once nothing is left to reset): gpui would
    /// otherwise route the next key from the window root, outside the
    /// Settings card, and Tab would leave Settings.
    fn focus_section_tab(
        &self,
        section: SettingsSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = self.settings_focus.control_focus(section.focus_id(), cx);
        window.focus(&tab, cx);
    }

    /// zed's modal layer (file finder, pickers) rendered as OUR window-root
    /// child — the third window-root duty this embed adopts (after action
    /// wiring and key context). At the root rather than inside the editor
    /// tab's wrapper: an overlay hosted in the wrapper centers over the tab
    /// content area, which sits visibly right of the window's center the
    /// title bar establishes. zed centers modals over the window; so do we.
    pub(crate) fn render_zed_modal(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ws = self.zed_modal_host()?;
        let view: gpui::AnyView = ws
            .read(cx)
            .active_modal::<file_finder::FileFinder>(cx)?
            .into();
        let ws_for_dismiss = ws.clone();
        Some(
            div()
                .absolute()
                .inset_0()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    // `hide_modal` emits no DismissEvent, so the finder's
                    // dismissal watcher never sees a click-outside; hand
                    // focus back here (zed's own restore can't fire — see
                    // `watch_finder_dismiss`).
                    cx.listener(move |this, _, window, cx| {
                        let hidden = ws_for_dismiss.update(cx, |ws, cx| ws.hide_modal(window, cx));
                        if hidden {
                            this.focus_active_pane(window, cx);
                        }
                    }),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .h(px(0.0))
                        .mt_20()
                        .items_center()
                        .child(
                            div()
                                .occlude()
                                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation()
                                })
                                .child(view),
                        ),
                )
                .into_any_element(),
        )
    }

    /// Whose modal layer `render_zed_modal` draws: the Editor tab's own
    /// workspace when that tab is showing, else the service workspace behind
    /// the tools panel. Since zed v1.2x a Workspace no longer draws its own
    /// modal layer, so ⌘P's finder only appears if we draw it.
    fn zed_modal_host(&self) -> Option<Entity<::workspace::Workspace>> {
        let r = self.resolve()?;
        match self.active_ide_workspace() {
            Some(ide) => Some(ide),
            None => self.service_workspaces.get(&r.key).cloned(),
        }
    }

    /// The ⌘P file finder is open (drawn by `render_zed_modal`).
    pub(crate) fn zed_modal_open(&self, cx: &App) -> bool {
        self.zed_modal_host().is_some_and(|ws| {
            ws.read(cx)
                .active_modal::<file_finder::FileFinder>(cx)
                .is_some()
        })
    }

    /// The settings screen: an overlay, not a tab — settings are a glance,
    /// not a workspace. v1 is the shortcut catalog; the section structure is
    /// where future settings (theme, agent defaults) slot in.
    ///
    /// Rendered from `shortcuts::entries()` — the same table `main.rs` binds
    /// from — so this screen structurally cannot claim a shortcut that is not
    /// actually bound.
    pub(crate) fn render_settings(
        &self,
        hx: &HarnessTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.remote_setup_refocus.take() && !self.remote.ui.setup_open {
            self.focus_remote_section(window, cx);
        }
        let viewport = window.viewport_size();
        let focus = &self.settings_focus;
        let mut card = focus
            .card(div().id("settings-card"), "Settings")
            .when(self.rebinding.is_some(), |d| {
                d.key_context("SettingsRebinding")
            })
            .on_action(cx.listener(|ws, _: &menu::Cancel, window, cx| {
                ws.close_settings(window, cx);
            }))
            .flex()
            .flex_col()
            .w(px(660.0).min(viewport.width - px(24.0)))
            .h(px(600.0).min(viewport.height - px(24.0)))
            .rounded_lg()
            .bg(hx.bg1)
            .border_1()
            .border_color(hx.border)
            .shadow_lg()
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(hx.border)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(hx.tx0)
                            .child("Settings"),
                    )
                    .child(
                        modal_button("settings-close", "Close Settings", hx)
                            .px_2()
                            .rounded_md()
                            .text_sm()
                            .text_color(hx.tx2)
                            .cursor_pointer()
                            .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
                            .child("✕")
                            .on_click(cx.listener(|ws, _, window, cx| {
                                ws.close_settings(window, cx);
                            })),
                    ),
            );

        use crate::workspace::SettingsSection as S;
        let sec = self.settings_section;

        // min_w_0: a flex child otherwise grows to its widest row and runs
        // past the card (long addresses, network keys) instead of letting
        // those rows truncate.
        let mut body = div()
            .id(gpui::SharedString::from(format!(
                "settings-body-{}",
                sec.label()
            )))
            .role(gpui::Role::TabPanel)
            .aria_label(sec.label())
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&focus.scroll)
            .px_4()
            .py_2();

        // Shared by every pane.
        let section_title = |label: &str| section_title(label, hx);
        if sec == S::Appearance {
            // Mode decides which theme family the list below offers. Switching
            // modes returns to the harness pair — a dark zed theme in light mode
            // would contradict the choice just made.
            let mut modes = div().flex().gap_1p5().pb_1();
            for (label, dark) in [("Dark", true), ("Light", false)] {
                let selected = self.prefs.dark == dark;
                modes = modes.child(
                    focus
                        .scroll_choice(format!("mode-{label}"), label, selected, hx, cx)
                        .child(chip(label.to_string(), selected, hx))
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            cx.stop_propagation();
                            ws.prefs.dark = dark;
                            ws.set_zed_theme(None, cx);
                        })),
                );
            }
            body = body.child(section_title("Mode")).child(modes);

            // Themes for the chosen mode: the harness palette plus every bundled
            // zed theme of the same appearance. The registry ALSO holds our own
            // generated theme under the harness name — excluded, or the list
            // shows a phantom duplicate that fights the real chip for clicks
            // (the "can't select Harness Dark" bug).
            let mut themes = div().flex().flex_wrap().gap_1p5().pb_1();
            let active_zed = self.prefs.zed_theme.clone();
            let want_dark = self.prefs.dark;
            let harness_label = if want_dark {
                "Harness Dark".to_string()
            } else {
                "Harness Light".to_string()
            };
            let mut options: Vec<(String, Option<String>)> = vec![(harness_label, None)];
            for meta in ::theme::ThemeRegistry::global(cx).list() {
                let name = meta.name.to_string();
                if crate::harness_zed_theme::is_harness_theme_name(&name) {
                    continue;
                }
                let is_dark = matches!(meta.appearance, ::theme::Appearance::Dark);
                if is_dark == want_dark {
                    options.push((name.clone(), Some(name)));
                }
            }
            for (label, value) in options {
                let selected = match (&active_zed, &value) {
                    (Some(a), Some(v)) => a == v,
                    (None, None) => true,
                    _ => false,
                };
                let value2 = value.clone();
                themes = themes.child(
                    focus
                        .scroll_choice(format!("theme-{label}"), label.clone(), selected, hx, cx)
                        .child(chip(label.clone(), selected, hx))
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            cx.stop_propagation();
                            ws.set_zed_theme(value2.clone(), cx);
                        })),
                );
            }
            body = body.child(section_title("Theme")).child(themes);
        }

        // ---- Fonts: curated candidates that are actually installed --------
        let installed = cx.text_system().all_font_names();
        let has = |name: &str| installed.iter().any(|f| f == name);
        for (title, mono) in [("Code font", true), ("Interface font", false)] {
            // The code face is an Editor setting; the interface face is part of
            // how the app looks. Same loop, two panes.
            if sec != if mono { S::Editor } else { S::Appearance } {
                continue;
            }
            let candidates: &[&str] = if mono {
                &[
                    "Menlo",
                    "Monaco",
                    "SF Mono",
                    "Courier New",
                    "JetBrains Mono",
                    "Fira Code",
                    "Source Code Pro",
                    "IBM Plex Mono",
                    "Zed Plex Mono",
                    "Hack",
                ]
            } else {
                &[
                    "Zed Plex Sans",
                    "Helvetica Neue",
                    "Arial",
                    "Inter",
                    "IBM Plex Sans",
                    "Avenir Next",
                    "Seravek",
                    "Verdana",
                ]
            };
            let current = if mono {
                self.prefs
                    .font_mono
                    .clone()
                    .unwrap_or_else(|| hx.font_mono.to_string())
            } else {
                self.prefs
                    .font_ui
                    .clone()
                    .unwrap_or_else(|| hx.font_ui.to_string())
            };
            let mut row = div().flex().flex_wrap().gap_1p5().pb_1();
            for name in candidates.iter().filter(|n| has(n)) {
                let name = name.to_string();
                let selected = name == current;
                let pick = name.clone();
                row = row.child(
                    focus
                        .scroll_choice(
                            format!("font-{title}-{name}"),
                            name.clone(),
                            selected,
                            hx,
                            cx,
                        )
                        .child(chip(name, selected, hx))
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            cx.stop_propagation();
                            ws.set_font(mono, pick.clone(), cx);
                        })),
                );
            }
            body = body.child(section_title(title)).child(row);
        }
        if sec == S::Editor {
            {
                let size = self.prefs.font_size.unwrap_or(13.0);
                let step = |delta: f32| {
                    move |ws: &mut Self,
                          _: &gpui::ClickEvent,
                          _: &mut Window,
                          cx: &mut Context<Self>| {
                        let cur = ws.prefs.font_size.unwrap_or(13.0);
                        let next = (cur + delta).clamp(9.0, 24.0);
                        ws.prefs.font_size = if next == 13.0 { None } else { Some(next) };
                        ws.apply_appearance(cx);
                        ws.save_prefs(cx);
                        cx.refresh_windows();
                        cx.notify();
                    }
                };
                let size_chip = |label: &str| {
                    div()
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .border_1()
                        .border_color(hx.border)
                        .bg(hx.surface)
                        .text_xs()
                        .font_family(hx.font_mono)
                        .text_color(hx.tx0)
                        .cursor_pointer()
                        .hover(|d| d.bg(hx.surface_hover))
                        .child(label.to_string())
                };
                body = body.child(section_title("Font size")).child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .pb_1()
                        .child(
                            focus
                                .scroll_button("fs-minus", "Decrease font size", hx, cx)
                                .tooltip(tip("Smaller"))
                                .child(size_chip("−"))
                                .on_click(cx.listener(step(-1.0))),
                        )
                        .child(
                            div()
                                .w(px(44.0))
                                .text_center()
                                .text_xs()
                                .font_family(hx.font_mono)
                                .text_color(hx.tx0)
                                .child(format!("{size:.0} px")),
                        )
                        .child(
                            focus
                                .scroll_button("fs-plus", "Increase font size", hx, cx)
                                .tooltip(tip("Larger"))
                                .child(size_chip("+"))
                                .on_click(cx.listener(step(1.0))),
                        ),
                );
            }
            // ---- Files: gitignored entries ---------------------------------
            let enabled = self.prefs.show_ignored_files;
            body = body.child(section_title("Files")).child(
                toggle_row(
                    focus,
                    "show-ignored-toggle",
                    "Show gitignored files",
                    {
                        static DETAIL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
                        DETAIL.get_or_init(|| {
                            format!(
                                "In the file tree and {}.",
                                crate::shortcuts::global_label("cmd-p")
                            )
                        })
                    },
                    enabled,
                    hx,
                    cx,
                )
                .on_click(cx.listener(|ws, _, _, cx| {
                    cx.stop_propagation();
                    ws.prefs.show_ignored_files = !ws.prefs.show_ignored_files;
                    save_prefs_to(&ws.prefs_path, &ws.prefs);
                    // Rebuilds the zed settings JSON, which is where the tree
                    // and the file finder read it from.
                    ws.apply_appearance(cx);
                    cx.notify();
                })),
            );
        }

        if sec == S::Notifications {
            let current = self.prefs.notify_sound.clone();
            let mut list = div().flex().flex_col().gap_0p5().pb_1();
            for sound in crate::sounds::CATALOG {
                let id = sound.id;
                let selected = current == id;
                let origin = match sound.source {
                    crate::sounds::Source::Bundled => "bundled",
                    crate::sounds::Source::System => crate::sounds::SYSTEM_LABEL,
                    crate::sounds::Source::SystemDefault => "the system alert sound",
                    crate::sounds::Source::Silent => "no sound at all",
                };
                list = list.child(
                    focus
                        .scroll_choice(format!("sound-{id}"), id, selected, hx, cx)
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .when(selected, |d| d.bg(hx.surface))
                        .cursor_pointer()
                        .hover(|d| d.bg(hx.surface_hover))
                        .child(
                            div()
                                .flex_none()
                                .w(px(12.0))
                                .text_xs()
                                .text_color(hx.accent)
                                .child(if selected { "●" } else { "○" }),
                        )
                        .child(
                            div()
                                .flex_none()
                                .w(px(110.0))
                                .text_sm()
                                .text_color(hx.tx0)
                                .child(id),
                        )
                        .child(div().flex_1().text_xs().text_color(hx.tx2).child(origin))
                        // Hearing it is the only way to choose one. Plays on
                        // click without selecting, so you can audition the list.
                        // No ▶ on "silent": a play button that is *correct* to
                        // do nothing is indistinguishable from a broken one,
                        // and it is the row most likely to be tried first when
                        // someone is checking whether preview works at all.
                        .when(sound.source != crate::sounds::Source::Silent, |row| {
                            row.child(
                                focus
                                    .scroll_button(
                                        format!("play-{id}"),
                                        format!("Preview {id}"),
                                        hx,
                                        cx,
                                    )
                                    .flex_none()
                                    .px_1p5()
                                    .rounded_md()
                                    .text_xs()
                                    .text_color(hx.tx2)
                                    .cursor_pointer()
                                    .hover(|d| d.bg(hx.surface).text_color(hx.accent))
                                    .child("▶")
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.stop_propagation();
                                        crate::sounds::preview(id);
                                    })),
                            )
                        })
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            cx.stop_propagation();
                            ws.prefs.notify_sound = id.to_string();
                            save_prefs_to(&ws.prefs_path, &ws.prefs);
                            // Selecting plays it too: picking a sound you cannot
                            // hear is picking a name.
                            crate::sounds::preview(id);
                            cx.notify();
                        })),
                );
            }
            body = body.child(section_title("Notification sound")).child(list);
        }
        if sec == S::Diff {
            // ---- Review / tools panel ------------------------------------------
            {
                let location = self.prefs.diff_location;
                let mut row = div().flex().gap_1p5().pb_1();
                for (label, value) in [
                    ("Tools panel", DiffLocation::Dock),
                    ("Center tab", DiffLocation::Tab),
                ] {
                    let selected = location == value;
                    row = row.child(
                        focus
                            .scroll_choice(format!("diff-loc-{label}"), label, selected, hx, cx)
                            .child(chip(label.to_string(), selected, hx))
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                cx.stop_propagation();
                                ws.set_diff_location(value, window, cx);
                            })),
                    );
                }
                body = body
                    .child(section_title("Review location"))
                    .child(row)
                    .child(hint(
                        "A center tab gives Review the whole window. Files and Git stay in the tools panel.",
                        hx,
                    ));

                let view = self.prefs.diff_view;
                let mut view_row = div().flex().gap_1p5().pb_1();
                for value in [DiffView::Unified, DiffView::Split] {
                    let selected = view == value;
                    view_row = view_row.child(
                        focus
                            .scroll_choice(
                                format!("diff-view-set-{}", value.label()),
                                value.label(),
                                selected,
                                hx,
                                cx,
                            )
                            .child(chip(value.label().to_string(), selected, hx))
                            .on_click(cx.listener(move |ws, _, _, cx| {
                                cx.stop_propagation();
                                ws.set_diff_view(value, cx);
                            })),
                    );
                }
                body = body
                    .child(section_title("Diff layout"))
                    .child(view_row)
                    .child(hint(
                        "Split falls back to unified when the panel is too narrow.",
                        hx,
                    ));
            }

            body = body
                .child(section_title("Review Overview"))
                .child(self.render_review_overview_settings(hx, window, cx));
        }

        if sec == S::General {
            // ---- Browser developer tools ----------------------------------------
            // Where a browser tab's inspector opens. macOS also remembers a
            // move made from the inspector's own dock buttons; on Windows
            // (WebView2's docked frontend has none) this is the way to pick.
            {
                use crate::browser_pane::InspectorPlacement as P;
                let current = self.prefs.inspector_placement;
                let mut row = div().flex().gap_1p5().pb_1();
                for (label, value) in
                    [("Bottom", P::Bottom), ("Left", P::Left), ("Right", P::Right), ("Window", P::Window)]
                {
                    row = row.child(
                        focus
                            .scroll_choice(format!("inspector-{label}"), label, current == value, hx, cx)
                            .child(chip(label.to_string(), current == value, hx))
                            .on_click(cx.listener(move |ws, _, _, cx| {
                                cx.stop_propagation();
                                ws.prefs.inspector_placement = value;
                                crate::browser_pane::set_inspector_placement(value);
                                ws.save_prefs(cx);
                                cx.notify();
                            })),
                    );
                }
                body = body
                    .child(section_title("Browser developer tools"))
                    .child(row)
                    .child(hint(
                        "Where a browser tab's inspector opens: docked beside the page, or in its own window.",
                        hx,
                    ));
            }

            // ---- Terminals on quit ----------------------------------------------
            // The same choice the quit prompt's "Remember" box saves. Without this
            // row, a remembered answer could only be undone by editing prefs.json.
            let current = self.prefs.close_terminals_on_exit;
            let mut row = div().flex().gap_1p5().pb_1();
            for (label, value) in [
                ("Ask", None),
                ("Keep running", Some(false)),
                ("Close", Some(true)),
            ] {
                row = row.child(
                    focus
                        .scroll_choice(
                            format!("quit-terminals-{label}"),
                            label,
                            current == value,
                            hx,
                            cx,
                        )
                        .child(chip(label.to_string(), current == value, hx))
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            cx.stop_propagation();
                            ws.prefs.close_terminals_on_exit = value;
                            save_prefs_to(&ws.prefs_path, &ws.prefs);
                            cx.notify();
                        })),
                );
            }
            body = body
                .child(section_title("Terminals on quit"))
                .child(row)
                .child(hint(
                    if self.has_tmux {
                        "Keep running lets agents pick up where they left off next launch."
                    } else {
                        crate::platform::CURRENT.sessions.missing_hint
                    },
                    hx,
                ));

            // ---- Links -----------------------------------------------------------
            let in_tab = self.prefs.links_in_browser_tab;
            body = body.child(section_title("Links")).child(
                toggle_row(
                    focus,
                    "links-in-browser-tab-toggle",
                    "Open links in a browser tab",
                    "Links to local addresses (localhost, your network, Tailscale) and ports you open from this project or task open in a browser tab. Other links always use your default browser. Turn off to send everything to your default browser.",
                    in_tab,
                    hx,
                    cx,
                )
                .on_click(cx.listener(|ws, _, _, cx| {
                    cx.stop_propagation();
                    ws.prefs.links_in_browser_tab = !ws.prefs.links_in_browser_tab;
                    crate::link_routing::set_routing_enabled(ws.prefs.links_in_browser_tab);
                    save_prefs_to(&ws.prefs_path, &ws.prefs);
                    cx.notify();
                })),
            );
            body = body
                .child(
                    div().flex().pt_1().child(
                        focus
                            .scroll_button("browser-history-clear", "Clear browsing history", hx, cx)
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(hx.border)
                            .bg(hx.surface)
                            .text_xs()
                            .text_color(hx.tx1)
                            .cursor_pointer()
                            .hover(|d| d.bg(hx.surface_hover).text_color(hx.tx0))
                            .child("Clear browsing history")
                            .on_click(cx.listener(|ws, _, _, cx| {
                                cx.stop_propagation();
                                // The files go on the history thread; report
                                // once they have.
                                let cleared = ws.browser_history.update(cx, |h, cx| h.clear(cx));
                                cx.spawn(async move |ws, cx| {
                                    let cleared = cleared.await;
                                    let _ = ws.update(cx, |ws, cx| {
                                        match cleared {
                                            Ok(()) => ws.set_note("Browsing history cleared".into()),
                                            Err(e) => ws.set_error_note(format!(
                                                "Couldn't clear browsing history: {e}"
                                            )),
                                        }
                                        cx.notify();
                                    });
                                })
                                .detach();
                            })),
                    ),
                )
                .child(hint(
                    "Pages you open in browser tabs are remembered per project, to suggest addresses.",
                    hx,
                ));

            // ---- Conductor -------------------------------------------------------
            let always = self.prefs.conductor_always_on;
            body = body.child(section_title("Conductor")).child(
                toggle_row(
                    focus,
                    "conductor-toggle",
                    "Always-on conductor",
                    "Stays on screen and narrates what agents are doing. Drag him anywhere.",
                    always,
                    hx,
                    cx,
                )
                .on_click(cx.listener(|ws, _, _, cx| {
                    cx.stop_propagation();
                    ws.prefs.conductor_always_on = !ws.prefs.conductor_always_on;
                    if !ws.prefs.conductor_always_on {
                        ws.conductor_view = None;
                    }
                    save_prefs_to(&ws.prefs_path, &ws.prefs);
                    cx.notify();
                })),
            );
        }

        if sec == S::Agents {
            body = body
                .child(section_title("Agents"))
                .child(self.render_agent_list(hx, cx));
            // The reset button is always shown for discoverability, but stays
            // greyed out and inert while every built-in agent is already at its
            // default — clicking it would undo nothing. Custom agents are the
            // user's own additions, not defaults, so reset never touches them.
            let agents_dirty = !self.prefs.agent_order.is_empty()
                || self.prefs.default_agent.is_some()
                || self
                    .prefs
                    .agents
                    .values()
                    .any(|a| !a.custom && (a.disabled || a.command.is_some()));
            let reset = if agents_dirty {
                focus.scroll_button("agents-reset", "Reset agents to defaults", hx, cx)
            } else {
                div().id("agents-reset")
            };
            let mut reset = reset
                .mt_2()
                .px_3()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(hx.border)
                .bg(hx.surface)
                .text_xs()
                .child("Reset agents to defaults");
            if agents_dirty {
                reset = reset
                    .text_color(hx.tx1)
                    .cursor_pointer()
                    .hover(|d| d.bg(hx.surface_hover).text_color(hx.tx0))
                    .on_click(cx.listener(|ws, _, window, cx| {
                        cx.stop_propagation();
                        // The button goes inert once nothing is left to
                        // reset; the keyboard moves first, or Tab would no
                        // longer reach Settings.
                        ws.focus_section_tab(SettingsSection::Agents, window, cx);
                        // Drop built-in overrides and the custom order; keep
                        // the user's custom agents, which have no "default"
                        // to reset to.
                        ws.prefs.agents.retain(|_, a| a.custom);
                        ws.prefs.agent_order.clear();
                        ws.prefs.default_agent = None;
                        save_prefs_to(&ws.prefs_path, &ws.prefs);
                        cx.notify();
                    }));
            } else {
                reset = reset.text_color(hx.tx2);
            }
            body = body.child(div().flex().child(reset));
        }

        if sec == S::Remote {
            body = body.child(self.render_remote_settings(hx, cx));
        }

        // Keyboard shortcuts: their own tab. They used to trail the Agents
        // section, where nobody looking for a key binding would think to go.
        if sec == S::Shortcuts {
            let entries = crate::shortcuts::entries_with(&self.prefs.shortcuts);
            let scopes = [
                crate::shortcuts::Scope::Global,
                crate::shortcuts::Scope::Workspace,
                crate::shortcuts::Scope::Ide,
                crate::shortcuts::Scope::Editor,
                crate::shortcuts::Scope::Picker,
            ];
            for scope in scopes {
                let mut section = div().flex().flex_col().pb_2().child(
                    div()
                        .pt_3()
                        .pb_1()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(hx.tx2)
                        .child(scope.label().to_uppercase()),
                );
                for e in entries.iter().filter(|e| {
                    e.scope == scope
                        || (scope == crate::shortcuts::Scope::Editor
                            && e.scope == crate::shortcuts::Scope::EditorFull)
                }) {
                    let id = crate::shortcuts::key_id(e.scope, e.what);
                    let arming = self.rebinding.as_deref() == Some(id.as_str());
                    let overridden = self.prefs.shortcuts.contains_key(&id);
                    let arm_id = id.clone();
                    let button_id = format!("sc-{id}");
                    let button_focus = focus.control_focus(button_id.clone().into(), cx);
                    let chip_label = if arming {
                        "press keys…".to_string()
                    } else {
                        crate::shortcuts::display_keys(&e.keys)
                    };
                    let chipd = focus
                        .scroll_button(button_id, e.what, hx, cx)
                        .aria_description(chip_label.clone())
                        .flex_none()
                        .w(px(120.0))
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .bg(if arming {
                            tint(hx.accent, 0.16)
                        } else {
                            hx.surface
                        })
                        .border_1()
                        .border_color(if arming {
                            hx.accent
                        } else if overridden {
                            hx.amber
                        } else {
                            hx.border
                        })
                        .text_xs()
                        .font_family(hx.font_mono)
                        .text_color(if arming { hx.accent } else { hx.tx0 })
                        .text_center()
                        .cursor_pointer()
                        .hover(|d| d.bg(hx.surface_hover))
                        .child(chip_label)
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            cx.stop_propagation();
                            ws.rebinding = Some(arm_id.clone());
                            window.focus(&button_focus, cx);
                            cx.notify();
                        }));
                    // While armed, the chip owns the keyboard until a chord
                    // lands or escape cancels: the workspace's keystroke
                    // interceptor takes the keys before the keymap can run
                    // them (`capture_rebind_keystroke`).
                    section = section.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .py_1()
                            .child(chipd)
                            .child(div().text_sm().text_color(hx.tx1).child(e.what)),
                    );
                }
                body = body.child(section);
            }
            // Reset appears only once something is rebound — a button that
            // undoes nothing is clutter.
            if !self.prefs.shortcuts.is_empty() {
                body = body.child(
                    focus
                        .scroll_button("sc-reset", "Reset shortcuts to defaults", hx, cx)
                        .mt_2()
                        .mb_3()
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(hx.border)
                        .bg(hx.surface)
                        .text_xs()
                        .text_color(hx.tx1)
                        .cursor_pointer()
                        .hover(|d| d.bg(hx.surface_hover).text_color(hx.tx0))
                        .child("Reset shortcuts to defaults")
                        .on_click(cx.listener(|ws, _, window, cx| {
                            cx.stop_propagation();
                            // The button goes once nothing is rebound; the
                            // keyboard moves first, or Tab would no longer
                            // reach Settings.
                            ws.focus_section_tab(SettingsSection::Shortcuts, window, cx);
                            ws.prefs.shortcuts.clear();
                            save_prefs_to(&ws.prefs_path, &ws.prefs);
                            ws.apply_shortcuts(cx);
                            cx.notify();
                        })),
                );
            }
        }
        let mut rail = div()
            .id("settings-nav")
            .role(gpui::Role::TabList)
            .aria_label("Settings sections")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(140.0))
            .min_h_0()
            .overflow_y_scroll()
            .gap_0p5()
            .p_2()
            .border_r_1()
            .border_color(hx.border);
        for s in S::ALL {
            let selected = s == sec;
            let nav_focus = focus.control_focus(s.focus_id(), cx);
            rail = rail.child(
                modal_control(s.focus_id(), s.label(), gpui::Role::Tab, hx)
                    .track_focus(&nav_focus)
                    .aria_selected(selected)
                    .flex_none()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_sm()
                    .when(selected, |d| d.bg(hx.surface))
                    .text_color(if selected { hx.tx0 } else { hx.tx2 })
                    .cursor_pointer()
                    .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
                    .child(s.label())
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        cx.stop_propagation();
                        if ws.settings_section != s {
                            ws.leave_settings_control();
                            ws.settings_focus.reset_scroll();
                        }
                        ws.settings_section = s;
                        if s == S::Diff {
                            ws.ensure_review_model_catalog(cx);
                        }
                        if s != S::Remote {
                            ws.remote_close_setup();
                        }
                        cx.notify();
                    })),
            );
        }
        card = card
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .when(crate::probe_paint(), |d| {
                        d.child(
                            gpui::canvas(
                                |b, _, _| log::info!("paint: settings content row = {b:?}"),
                                |_, _, _, _| {},
                            )
                            .absolute()
                            .size_full(),
                        )
                    })
                    .child(rail)
                    .child(body),
            )
            .child(self.settings_focus.end());

        // Full-window backdrop. Swallows mouse-down (the ＋-menu lesson: a
        // backdrop that closes on mouse-down eats the click before any child
        // on_click can fire on mouse-up) and closes on click outside the card.
        div()
            .id("settings-backdrop")
            // Occlude, not just catch clicks: without it the elements under
            // an overlay still hover, and their tooltips show through it.
            .occlude()
            .when(crate::probe_paint(), |d| {
                d.child(
                    gpui::canvas(
                        |b, _, _| log::info!("paint: settings overlay bounds = {b:?}"),
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
            })
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.45))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            // A modal owns the wheel. Scroll events bubble past the card's
            // own scroll region, and anything the backdrop lets through
            // reaches the editor underneath — both were scrolling at once.
            .on_scroll(|_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|ws, _, window, cx| {
                ws.close_settings(window, cx);
            }))
            .child(card.on_click(|_, _, cx| cx.stop_propagation()))
            .when(
                self.remote.ui.setup_open
                    && self.remote.serve == crate::workspace::remote::RemoteServe::Serving,
                |d| d.child(self.render_add_phone_dialog(hx, cx)),
            )
            .into_any_element()
    }

    /// Settings › Agents: one row per agent, in the user's order.
    ///
    /// Row: grip · icon · name · launch command · restore · switch · remove.
    /// Only the grip starts a drag — the rest of the row holds a switch, a
    /// click-to-edit command and, while editing, a live text input, none of
    /// which should turn into a drag. The whole row (with a custom agent's
    /// status controls) is the drop target, so the pair moves together.
    fn render_agent_list(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> gpui::Div {
        let focus = &self.settings_focus;
        let mut agents = div().flex().flex_col().gap_0p5().pb_1();
        for kind in agent_catalog(&self.prefs) {
            let key = kind.as_str().to_string();
            let pref = self.prefs.agents.get(&key).cloned().unwrap_or_default();
            let is_custom = matches!(kind, AgentKind::Other(_));
            // The command each adapter actually launches when no override is
            // set — defined once per agent in agent-runtime, not here.
            let default_cmd = agent_runtime::default_command_display(&kind);
            let editing = matches!(self.edit_kind(), Some(EditKind::AgentCmd(k)) if *k == key);
            // A built-in with its own command; customs have no default to
            // restore.
            let overridden = !is_custom && pref.command.is_some();
            // Prefill what the user SEES — the override if set, else the
            // default — so editing refines the current command instead of
            // starting from a blank line.
            let prefill = pref.command.clone().unwrap_or_else(|| default_cmd.clone());
            let cmd_display = prefill.clone();
            let on = !pref.disabled;
            let display_name = agent_display(&kind);

            let ghost_kind = kind.clone();
            let ghost_theme = hx.clone();
            let grip = div()
                .id(gpui::SharedString::from(format!("agent-grip-{key}")))
                .flex_none()
                .w(px(14.0))
                .h(px(20.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .cursor(gpui::CursorStyle::OpenHand)
                .hover(|d| d.bg(hx.surface_hover))
                .tooltip(tip("Drag to reorder"))
                .child(grip_dots(hx.tx2))
                .on_drag(DraggedAgentRow { key: key.clone() }, move |_, _, _, cx| {
                    let kind = ghost_kind.clone();
                    let hx = ghost_theme.clone();
                    cx.new(|_| AgentRowGhost { kind, hx })
                });

            let name = div()
                .id(gpui::SharedString::from(format!("agent-name-{key}")))
                .flex_none()
                .w(px(150.0))
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .size(px(18.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(!on, |d| d.opacity(0.5))
                        .child(agent_icon(&kind, 16.0)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .min_w_0()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .text_color(if on { hx.tx0 } else { hx.tx2 })
                                .child(display_name.clone()),
                        )
                )
                .when(is_custom, |d| d.tooltip(tip(display_name.clone())))
                // The one thing worth knowing about Codex status, where it is
                // asked about rather than in a paragraph over every agent.
                .when(matches!(kind, AgentKind::Codex), |d| {
                    d.tooltip(tip(
                        "Running and Idle come from Codex hooks. \
                         If they're missing, check /hooks in Codex.",
                    ))
                });

            let command: AnyElement =
                if let Some(editor) = editing.then(|| self.edit_editor()).flatten() {
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(self.render_input(hx, editor, cx))
                        .into_any_element()
                } else {
                    let edit_key = key.clone();
                    focus
                        .scroll_button(
                            format!("agent-cmd-{key}"),
                            format!("Edit {display_name} launch command"),
                            hx,
                            cx,
                        )
                        .flex_1()
                        // Without min_w_0 a flex child keeps its content width
                        // (min-width: auto), so a long default like codex's
                        // refuses to shrink and shoves the row out of
                        // alignment. min_w_0 lets it shrink; truncate clips the
                        // overflow with an ellipsis.
                        .min_w_0()
                        .truncate()
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .bg(hx.surface)
                        .border_1()
                        .border_color(hx.border)
                        .text_xs()
                        .font_family(hx.font_mono)
                        .text_color(if on && pref.command.is_some() {
                            hx.tx0
                        } else {
                            hx.tx2
                        })
                        .cursor_pointer()
                        .hover(|d| d.bg(hx.surface_hover))
                        .tooltip(tip(
                            "Launch command. Runs in your login shell, so aliases apply.",
                        ))
                        .child(cmd_display)
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            cx.stop_propagation();
                            ws.begin_edit(
                                EditKind::AgentCmd(edit_key.clone()),
                                &prefill,
                                "launch command…",
                                window,
                                cx,
                            );
                        }))
                        .into_any_element()
                };

            // The default-agent star, in a fixed slot so every row's lines up.
            // Filled = the effective default; outlined = click to make this
            // agent the default; a disabled agent can't be, so no star. A
            // filled star is inert once saved; one that is the default only
            // as the last-used or first agent saves it (`star_saves`).
            // Filled and outlined share one id, so the keyboard stays on the
            // star when activating it turns outlined into filled.
            let star = match agent_star(&self.prefs, &kind) {
                AgentStar::Hidden => div().flex_none().w(px(20.0)).into_any_element(),
                state => {
                    let filled = state == AgentStar::Filled;
                    let saves = star_saves(&self.prefs, &kind).is_some();
                    let star_key = key.clone();
                    let accent = hx.accent;
                    let hover_bg = hx.surface_hover;
                    // Built by hand rather than `icon_button`: its tooltip is
                    // the accessible label, which here names the agent.
                    focus
                        .scroll_choice(
                            format!("agent-star-{key}"),
                            if filled && saves {
                                format!("Keep {display_name} as the default for new tasks")
                            } else if filled {
                                format!("{display_name} is the default for new tasks")
                            } else {
                                format!("Make {display_name} the default for new tasks")
                            },
                            filled,
                            hx,
                            cx,
                        )
                        .flex_none()
                        .w(px(20.0))
                        .py_0p5()
                        .rounded_md()
                        .text_center()
                        .text_sm()
                        .text_color(if filled { accent } else { hx.tx2 })
                        .tooltip(tip("Default for new tasks"))
                        .child(if filled { "★" } else { "☆" })
                        .when(saves, |d| {
                            d.cursor_pointer()
                                .hover(move |d| d.bg(hover_bg).text_color(accent))
                        })
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            cx.stop_propagation();
                            // The saved default's star: nothing to change.
                            if let Some(key) =
                                star_saves(&ws.prefs, &AgentKind::from(star_key.as_str()))
                            {
                                ws.prefs.default_agent = Some(key);
                                save_prefs_to(&ws.prefs_path, &ws.prefs);
                                cx.notify();
                            }
                        }))
                        .into_any_element()
                }
            };

            // Fixed-width slots, filled or not, so every row's switch lines up.
            let restore = if overridden {
                let restore_key = key.clone();
                icon_button(
                    focus,
                    format!("agent-restore-{key}"),
                    "↺",
                    format!("Restore the default {display_name} command"),
                    hx,
                    hx.tx0,
                    cx,
                )
                .on_click(cx.listener(move |ws, _, _, cx| {
                    cx.stop_propagation();
                    if let Some(e) = ws.prefs.agents.get_mut(&restore_key) {
                        e.command = None;
                    }
                    save_prefs_to(&ws.prefs_path, &ws.prefs);
                    cx.notify();
                }))
                .into_any_element()
            } else {
                div().flex_none().w(px(20.0)).into_any_element()
            };

            let toggle_key = key.clone();
            let switch_el = focus
                .scroll_control(
                    format!("agent-en-{key}"),
                    format!("Enable {display_name}"),
                    gpui::Role::Switch,
                    hx,
                    cx,
                )
                .aria_toggled(toggled(on))
                .flex_none()
                .tooltip(tip(if on {
                    "On: offered in the + menu"
                } else {
                    "Off: hidden from the + menu"
                }))
                .child(switch(on, hx))
                .on_click(cx.listener(move |ws, _, _, cx| {
                    cx.stop_propagation();
                    let is_custom_key = AgentKind::known().iter().all(|k| k.as_str() != toggle_key);
                    let e = ws.prefs.agents.entry(toggle_key.clone()).or_default();
                    if is_custom_key {
                        e.custom = true;
                    }
                    e.disabled = !e.disabled;
                    save_prefs_to(&ws.prefs_path, &ws.prefs);
                    cx.notify();
                }));

            let remove = if is_custom {
                let remove_key = key.clone();
                icon_button(
                    focus,
                    format!("agent-rm-{key}"),
                    "×",
                    format!("Remove {display_name}"),
                    hx,
                    hx.red,
                    cx,
                )
                .on_click(cx.listener(move |ws, _, window, cx| {
                    cx.stop_propagation();
                    // This row, and its focused × with it, is about to go.
                    ws.focus_section_tab(SettingsSection::Agents, window, cx);
                    ws.prefs.agents.remove(&remove_key);
                    ws.prefs.agent_order.retain(|k| *k != remove_key);
                    if ws.prefs.default_agent.as_deref() == Some(remove_key.as_str()) {
                        ws.prefs.default_agent = None;
                    }
                    save_prefs_to(&ws.prefs_path, &ws.prefs);
                    cx.notify();
                }))
                .into_any_element()
            } else {
                div().flex_none().w(px(20.0)).into_any_element()
            };

            let row = div()
                .flex()
                .items_center()
                .gap_2()
                .py_1()
                .child(grip)
                .child(name)
                .child(command)
                .child(star)
                .child(restore)
                .child(switch_el)
                .child(remove);

            let drop_key = key.clone();
            let drop_bg = tint(hx.accent, 0.16);
            let mut item = div()
                .id(gpui::SharedString::from(format!("agent-item-{key}")))
                .flex()
                .flex_col()
                .px_1()
                .rounded_md()
                .drag_over::<DraggedAgentRow>(move |d, _, _, _| d.bg(drop_bg))
                .on_drop(cx.listener(move |ws, dragged: &DraggedAgentRow, _, cx| {
                    // Dropped where it started: nothing moved, so save nothing
                    // (a saved order would light up "Reset" for no change).
                    if dragged.key == drop_key {
                        return;
                    }
                    ws.prefs.agent_order =
                        agent_order_after_move(&ws.prefs, &dragged.key, &drop_key);
                    save_prefs_to(&ws.prefs_path, &ws.prefs);
                    cx.notify();
                }))
                .child(row);
            // Status controls — custom agents only. Built-ins keep their
            // dedicated adapters; these prefs drive ConfigurableRuntime.
            if is_custom {
                item = item.child(self.render_agent_signal_row(hx, &key, &pref, cx));
            }
            agents = agents.child(item);
        }

        // Add-a-custom-agent affordance: a slug field (prefilled empty) that
        // creates a new `AgentKind::Other` on commit.
        let adding = matches!(self.edit_kind(), Some(EditKind::NewAgent));
        if let Some(editor) = adding.then(|| self.edit_editor()).flatten() {
            agents = agents.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_1()
                    .py_1()
                    .child(div().flex_none().w(px(14.0)))
                    .child(
                        div()
                            .flex_none()
                            .w(px(18.0))
                            .text_center()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(hx.tx2)
                            .child("◇"),
                    )
                    .child(div().flex_1().child(self.render_input(hx, editor, cx))),
            );
        } else {
            agents = agents.child(
                div().flex().pt_1().child(
                    focus
                        .scroll_button("agent-add", "Add agent", hx, cx)
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(hx.border)
                        .bg(hx.surface)
                        .text_xs()
                        .text_color(hx.tx1)
                        .cursor_pointer()
                        .hover(|d| d.bg(hx.surface_hover).text_color(hx.tx0))
                        .child("+ Add agent")
                        .on_click(cx.listener(|ws, _, window, cx| {
                            cx.stop_propagation();
                            ws.begin_edit(EditKind::NewAgent, "", "agent name…", window, cx);
                        })),
                ),
            );
        }
        agents
    }

    /// Status controls under a custom agent, as visible choices rather than
    /// chips that cycle on click: how status is detected, and (when on) how
    /// its hooks get installed. Manual install shows the shim to wire up.
    fn render_agent_signal_row(
        &self,
        hx: &HarnessTheme,
        key: &str,
        pref: &AgentPref,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        use agent_runtime::{HookInstall, SignalProfile, WaitFidelity};

        let fidelity = pref
            .signal
            .as_ref()
            .map(|s| s.fidelity)
            .filter(|f| !matches!(f, WaitFidelity::TerminalOnly));
        let install = pref
            .signal
            .as_ref()
            .map(|s| s.install)
            .unwrap_or(HookInstall::None);
        let signal_on = fidelity.is_some();
        let show_shim = signal_on && install.effective(key) == HookInstall::None;
        let shim_cmd = agent_runtime::harness_status_shim_command(&self.state_dir);

        let label = |text: &'static str| {
            div()
                .flex_none()
                .w(px(44.0))
                .text_xs()
                .text_color(hx.tx2)
                .child(text)
        };

        let mut status = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1p5()
            .child(label("Status"));
        for (text, value, detail) in [
            (
                "Off",
                None,
                "No status hooks: the terminal is the only signal.",
            ),
            (
                "Best effort",
                Some(WaitFidelity::BestEffort),
                "Hooks report busy and idle. Approval prompts may not notify.",
            ),
            (
                "Exact",
                Some(WaitFidelity::Exact),
                "Hooks also report when the agent is waiting on you.",
            ),
        ] {
            let k = key.to_string();
            status = status.child(
                self.settings_focus
                    .scroll_choice(
                        format!("agent-sig-{key}-{text}"),
                        format!("{key} status: {text}"),
                        fidelity == value,
                        hx,
                        cx,
                    )
                    .tooltip(tip(detail))
                    .child(chip(text.to_string(), fidelity == value, hx))
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        cx.stop_propagation();
                        let e = ws.prefs.agents.entry(k.clone()).or_default();
                        e.custom = true;
                        let install = e
                            .signal
                            .as_ref()
                            .map(|s| s.install)
                            .unwrap_or(HookInstall::None);
                        e.signal = match value {
                            None => None,
                            Some(WaitFidelity::Exact) => Some(SignalProfile::exact(install)),
                            Some(_) => Some(SignalProfile::best_effort(install)),
                        };
                        save_prefs_to(&ws.prefs_path, &ws.prefs);
                        cx.notify();
                    })),
            );
        }

        // Indented past the grip and icon, so it reads as part of the agent.
        let mut col = div()
            .flex()
            .flex_col()
            .gap_1()
            .pl(px(46.0))
            .pb_1p5()
            .child(status);

        if signal_on {
            let mut hooks = div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_1p5()
                .child(label("Hooks"));
            for (text, value, detail) in [
                (
                    "Manual",
                    HookInstall::None,
                    "Wire the status shim into your own hook config.",
                ),
                (
                    "Auto",
                    HookInstall::Auto,
                    "Pick the hook format from the agent's name.",
                ),
                (
                    "Claude",
                    HookInstall::ClaudeLocalSettings,
                    "Install Claude Code hooks when the agent launches.",
                ),
                (
                    "Codex",
                    HookInstall::CodexUserHooks,
                    "Install Codex hooks when the agent launches. Review them with /hooks.",
                ),
                (
                    "Cursor",
                    HookInstall::CursorUserHooks,
                    "Install Cursor Agent hooks when the agent launches.",
                ),
            ] {
                let k = key.to_string();
                hooks = hooks.child(
                    self.settings_focus
                        .scroll_choice(
                            format!("agent-inst-{key}-{text}"),
                            format!("{key} hooks: {text}"),
                            install == value,
                            hx,
                            cx,
                        )
                        .tooltip(tip(detail))
                        .child(chip(text.to_string(), install == value, hx))
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            cx.stop_propagation();
                            let e = ws.prefs.agents.entry(k.clone()).or_default();
                            e.custom = true;
                            let fidelity = e
                                .signal
                                .as_ref()
                                .map(|s| s.fidelity)
                                .filter(|f| !matches!(f, WaitFidelity::TerminalOnly))
                                .unwrap_or(WaitFidelity::BestEffort);
                            e.signal = Some(SignalProfile {
                                fidelity,
                                install: value,
                            });
                            save_prefs_to(&ws.prefs_path, &ws.prefs);
                            cx.notify();
                        })),
                );
            }
            col = col.child(hooks);
        }
        if show_shim {
            col = col.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(label("Shim"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(hx.tx1)
                            .child(shim_cmd),
                    ),
            );
        }
        col
    }
}

/// The 2×2 dots a drag handle shows.
fn grip_dots(color: Rgba) -> gpui::Div {
    let dot = || div().size(px(3.0)).rounded_full().bg(color);
    div()
        .flex()
        .flex_col()
        .gap(px(3.0))
        .child(div().flex().gap(px(3.0)).child(dot()).child(dot()))
        .child(div().flex().gap(px(3.0)).child(dot()).child(dot()))
}

/// What follows the cursor while an agent row is dragged: the agent itself.
struct AgentRowGhost {
    kind: AgentKind,
    hx: HarnessTheme,
}

impl Render for AgentRowGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let hx = &self.hx;
        div()
            .w(px(220.0))
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.accent)
            .shadow_lg()
            .child(grip_dots(hx.tx1))
            .child(agent_icon(&self.kind, 16.0))
            .child(
                div()
                    .text_sm()
                    .text_color(hx.tx0)
                    .child(self.kind.to_string()),
            )
    }
}
