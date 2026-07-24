//! Split out of `workspace.rs` (see its module docs). Pure relocation:
//! `impl HarnessWorkspace` methods for this feature area.

use super::super::*;

impl HarnessWorkspace {

    /// zed's modal layer (file finder, pickers) rendered as OUR window-root
    /// child — the third window-root duty this embed adopts (after action
    /// wiring and key context). At the root rather than inside the editor
    /// tab's wrapper: an overlay hosted in the wrapper centers over the tab
    /// content area, which sits visibly right of the window's center the
    /// title bar establishes. zed centers modals over the window; so do we.
    pub(crate) fn render_zed_modal(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let r = self.resolve()?;
        let pane = self.panes.get(&r.key)?;
        let ws = match &pane.active_tab()?.content {
            TabContent::Editor(ws) => ws.clone(),
            _ => return None,
        };
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
                    cx.listener(move |_, _, window, cx| {
                        ws_for_dismiss.update(cx, |ws, cx| {
                            ws.hide_modal(window, cx);
                        });
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

    /// The settings screen: an overlay, not a tab — settings are a glance,
    /// not a workspace. v1 is the shortcut catalog; the section structure is
    /// where future settings (theme, agent defaults) slot in.
    ///
    /// Rendered from `shortcuts::entries()` — the same table `main.rs` binds
    /// from — so this screen structurally cannot claim a shortcut that is not
    /// actually bound.
    pub(crate) fn render_settings(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> AnyElement {
        let mut card = div()
            .id("settings-card")
            .flex()
            .flex_col()
            .w(px(520.0))
            .max_h(px(600.0))
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
                        div()
                            .id("settings-close")
                            .px_2()
                            .rounded_md()
                            .text_sm()
                            .text_color(hx.tx2)
                            .cursor_pointer()
                            .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
                            .child("✕")
                            .on_click(cx.listener(|ws, _, _, cx| {
                                ws.settings_open = false;
                                cx.notify();
                            })),
                    ),
            );

        let mut body = div().id("settings-body").flex_1().min_h_0().overflow_y_scroll().px_4().py_2();

        // ---- Appearance: mode first, then themes of that appearance -------
        let section_title = |label: &str| {
            div()
                .pt_3()
                .pb_1()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(hx.tx2)
                .child(label.to_uppercase())
        };
        let chip = |label: String, selected: bool, hx: &HarnessTheme| {
            div()
                .px_2()
                .py_0p5()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .border_1()
                .border_color(if selected { hx.accent } else { hx.border })
                .bg(if selected { tint(hx.accent, 0.16) } else { hx.surface })
                .text_color(if selected { hx.accent } else { hx.tx1 })
                .hover(|d| d.bg(hx.surface_hover))
                .child(label)
        };

        // Mode decides which theme family the list below offers. Switching
        // modes returns to the harness pair — a dark zed theme in light mode
        // would contradict the choice just made.
        let mut modes = div().flex().gap_1p5().pb_1();
        for (label, dark) in [("Dark", true), ("Light", false)] {
            let selected = self.prefs.dark == dark;
            modes = modes.child(
                div()
                    .id(gpui::SharedString::from(format!("mode-{label}")))
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
        let harness_label =
            if want_dark { "Harness Dark".to_string() } else { "Harness Light".to_string() };
        let mut options: Vec<(String, Option<String>)> = vec![(harness_label, None)];
        for meta in ::theme::ThemeRegistry::global(cx).list() {
            let name = meta.name.to_string();
            if name == crate::harness_zed_theme::HARNESS_THEME_NAME {
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
                div()
                    .id(gpui::SharedString::from(format!("theme-{label}")))
                    .child(chip(label.clone(), selected, hx))
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        cx.stop_propagation();
                        ws.set_zed_theme(value2.clone(), cx);
                    })),
            );
        }
        body = body.child(section_title("Theme")).child(themes);

        // ---- Fonts: curated candidates that are actually installed --------
        let installed = cx.text_system().all_font_names();
        let has = |name: &str| installed.iter().any(|f| f == name);
        for (title, mono) in [("Code font", true), ("Interface font", false)] {
            let candidates: &[&str] = if mono {
                &["Menlo", "Monaco", "SF Mono", "Courier New", "JetBrains Mono",
                  "Fira Code", "Source Code Pro", "IBM Plex Mono", "Zed Plex Mono", "Hack"]
            } else {
                &["Zed Plex Sans", "Helvetica Neue", "Arial", "Inter",
                  "IBM Plex Sans", "Avenir Next", "Seravek", "Verdana"]
            };
            let current = if mono {
                self.prefs.font_mono.clone().unwrap_or_else(|| hx.font_mono.to_string())
            } else {
                self.prefs.font_ui.clone().unwrap_or_else(|| hx.font_ui.to_string())
            };
            let mut row = div().flex().flex_wrap().gap_1p5().pb_1();
            for name in candidates.iter().filter(|n| has(n)) {
                let name = name.to_string();
                let selected = name == current;
                let pick = name.clone();
                row = row.child(
                    div()
                        .id(gpui::SharedString::from(format!("font-{title}-{name}")))
                        .child(chip(name, selected, hx))
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            cx.stop_propagation();
                            ws.set_font(mono, pick.clone(), cx);
                        })),
                );
            }
            body = body.child(section_title(title)).child(row);
        }
        {
            let size = self.prefs.font_size.unwrap_or(13.0);
            let step = |delta: f32| {
                move |ws: &mut Self, _: &gpui::ClickEvent, _: &mut Window, cx: &mut Context<Self>| {
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
                    .child(div().id("fs-minus").tooltip(tip("Smaller")).child(size_chip("−")).on_click(cx.listener(step(-1.0))))
                    .child(
                        div()
                            .w(px(44.0))
                            .text_center()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(hx.tx0)
                            .child(format!("{size:.0} px")),
                    )
                    .child(div().id("fs-plus").tooltip(tip("Larger")).child(size_chip("+")).on_click(cx.listener(step(1.0)))),
            );
        }

        // ---- Extras --------------------------------------------------------
        {
            let enabled = self.prefs.pet_enabled;
            body = body.child(section_title("Extras")).child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pb_1()
                    .child(
                        div()
                            .id("pet-toggle")
                            .child(chip(
                                if enabled { "on".into() } else { "off".into() },
                                enabled,
                                hx,
                            ))
                            .on_click(cx.listener(|ws, _, _, cx| {
                                cx.stop_propagation();
                                ws.prefs.pet_enabled = !ws.prefs.pet_enabled;
                                if !ws.prefs.pet_enabled {
                                    ws.pet = None;
                                }
                                save_prefs_to(&ws.prefs_path, &ws.prefs);
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(hx.tx1)
                            .child("Conductor pet on notifications"),
                    ),
            );
        }

        // ---- Agents: enable/disable + launch-command override -------------
        let mut agents = div().flex().flex_col().gap_1().pb_1();
        for kind in AgentKind::known() {
            let key = kind.as_str().to_string();
            let pref = self.prefs.agents.get(&key).cloned().unwrap_or_default();
            let (glyph, color) = agent_mark(&kind);
            // The command each adapter actually launches when no override is
            // set — defined once per agent in agent-runtime, not here.
            let default_cmd = agent_runtime::default_command_display(&kind);
            let editing = matches!(self.edit_kind(), Some(EditKind::AgentCmd(k)) if *k == key);

            let toggle_key = key.clone();
            let edit_key_ = key.clone();
            // Prefill what the user SEES — the override if set, else the
            // default — so editing refines the current command instead of
            // starting from a blank line.
            let prefill = pref.command.clone().unwrap_or_else(|| default_cmd.clone());
            let cmd_display = pref.command.clone().unwrap_or_else(|| default_cmd.clone());

            let mut row = div()
                .flex()
                .items_center()
                .gap_2()
                .py_1()
                .child(
                    div()
                        .w(px(18.0))
                        .text_center()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(color)
                        .child(glyph),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(110.0))
                        .text_sm()
                        .text_color(if pref.disabled { hx.tx2 } else { hx.tx0 })
                        .child(kind.to_string()),
                )
                .child(
                    // Fixed width + centered so "on"/"off" occupy the same
                    // box — otherwise the wider "off" pill shoves the command
                    // field right and the column stops aligning.
                    div()
                        .id(gpui::SharedString::from(format!("agent-en-{key}")))
                        .flex_none()
                        .w(px(38.0))
                        .py_0p5()
                        .rounded_md()
                        .border_1()
                        .text_xs()
                        .text_center()
                        .cursor_pointer()
                        .border_color(if pref.disabled { hx.border } else { hx.accent })
                        .bg(if pref.disabled {
                            hx.surface
                        } else {
                            tint(hx.accent, 0.16)
                        })
                        .text_color(if pref.disabled { hx.tx1 } else { hx.accent })
                        .hover(|d| d.bg(hx.surface_hover))
                        .child(if pref.disabled { "off" } else { "on" })
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            cx.stop_propagation();
                            let e = ws.prefs.agents.entry(toggle_key.clone()).or_default();
                            e.disabled = !e.disabled;
                            save_prefs_to(&ws.prefs_path, &ws.prefs);
                            cx.notify();
                        })),
                );
            if let Some(editor) = editing.then(|| self.edit_editor()).flatten() {
                row = row.child(div().flex_1().child(self.render_input(hx, editor, cx)));
            } else {
                row = row.child(
                    div()
                        .id(gpui::SharedString::from(format!("agent-cmd-{key}")))
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
                        .text_color(if pref.command.is_some() { hx.tx0 } else { hx.tx2 })
                        .cursor_pointer()
                        .hover(|d| d.bg(hx.surface_hover))
                        .child(cmd_display)
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            cx.stop_propagation();
                            ws.begin_edit(
                                EditKind::AgentCmd(edit_key_.clone()),
                                &prefill,
                                "launch command…",
                                window,
                                cx,
                            );
                        })),
                );
            }
            agents = agents.child(row);
        }
        body = body
            .child(section_title("Agents"))
            .child(
                div()
                    .text_xs()
                    .text_color(hx.tx2)
                    .pb_1()
                    .child("Click a command to edit it (runs via $SHELL -lc; empty restores the default). Enter saves, Esc cancels."),
            )
            .child(agents);
        // The reset button is always shown for discoverability, but stays
        // greyed out and inert while every agent is already at its default —
        // clicking it would undo nothing.
        let agents_dirty = self
            .prefs
            .agents
            .values()
            .any(|a| a.disabled || a.command.is_some());
        let mut reset = div()
            .id("agents-reset")
            .mt_1()
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
                .on_click(cx.listener(|ws, _, _, cx| {
                    cx.stop_propagation();
                    ws.prefs.agents.clear();
                    save_prefs_to(&ws.prefs_path, &ws.prefs);
                    cx.notify();
                }));
        } else {
            reset = reset.text_color(hx.tx2);
        }
        body = body.child(reset);

        // The shortcuts get their own heading now that the screen holds
        // appearance, fonts, and agents above them.
        body = body.child(
            div()
                .mt_2()
                .pt_2()
                .border_t_1()
                .border_color(hx.border)
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(hx.tx0)
                .child("Keyboard Shortcuts"),
        );
        let entries = crate::shortcuts::entries_with(&self.prefs.shortcuts);
        let scopes = [
            crate::shortcuts::Scope::Global,
            crate::shortcuts::Scope::Workspace,
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
                let chip_label = if arming {
                    "press keys…".to_string()
                } else {
                    crate::shortcuts::display_keys(&e.keys)
                };
                let mut chipd = div()
                    .id(gpui::SharedString::from(format!("sc-{id}")))
                    .flex_none()
                    .w(px(120.0))
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .bg(if arming { tint(hx.accent, 0.16) } else { hx.surface })
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
                        window.focus(&ws.rebind_focus, cx);
                        cx.notify();
                    }));
                if arming {
                    // The armed chip owns the keyboard until a chord lands
                    // or escape cancels.
                    chipd = chipd
                        .track_focus(&self.rebind_focus)
                        .on_key_down(cx.listener(Self::rebind_key));
                }
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
                div()
                    .id("sc-reset")
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
                    .on_click(cx.listener(|ws, _, _, cx| {
                        cx.stop_propagation();
                        ws.prefs.shortcuts.clear();
                        save_prefs_to(&ws.prefs_path, &ws.prefs);
                        ws.apply_shortcuts(cx);
                        cx.notify();
                    })),
            );
        }
        card = card.child(body);

        // Full-window backdrop. Swallows mouse-down (the ＋-menu lesson: a
        // backdrop that closes on mouse-down eats the click before any child
        // on_click can fire on mouse-up) and closes on click outside the card.
        div()
            .id("settings-backdrop")
            .when(std::env::var_os("HARNESS_PROBE_PAINT").is_some(), |d| {
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
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|ws, _, _, cx| {
                ws.settings_open = false;
                cx.notify();
            }))
            .child(card.on_click(|_, _, cx| cx.stop_propagation()))
            .into_any_element()
    }
}
