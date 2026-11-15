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
        // The Editor tab's own workspace when that tab is showing, else the
        // service workspace behind the tools panel. Since zed v1.2x a
        // Workspace no longer draws its own modal layer, so ⌘P's finder only
        // appears if we draw it.
        let ws = match self.active_ide_workspace() {
            Some(ide) => ide,
            None => self.service_workspaces.get(&r.key)?.clone(),
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
            // Widened with the rail, not for its own sake: the rail takes 140px
            // out of what used to be 520px of content, and the font chips and
            // agent command rows were laid out against the old width.
            .w(px(660.0))
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

        use crate::workspace::SettingsSection as S;
        let sec = self.settings_section;

        let mut body = div().id("settings-body").flex_1().min_h_0().overflow_y_scroll().px_4().py_2();

        // Shared by every pane.
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
        if sec == S::Appearance {

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
        if sec == S::Editor {
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
        }

        if sec == S::Notifications {
            let current = self.prefs.notify_sound.clone();
            let mut list = div().flex().flex_col().gap_0p5().pb_1();
            for sound in crate::sounds::CATALOG {
                let id = sound.id;
                let selected = current == id;
                let origin = match sound.source {
                    crate::sounds::Source::Bundled => "bundled",
                    crate::sounds::Source::System => "macOS",
                    crate::sounds::Source::SystemDefault => "the system alert sound",
                    crate::sounds::Source::Silent => "no sound at all",
                };
                list = list.child(
                    div()
                        .id(gpui::SharedString::from(format!("sound-{id}")))
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
                        .when(sound.source != crate::sounds::Source::Silent, |row| row
                        .child(
                            div()
                                .id(gpui::SharedString::from(format!("play-{id}")))
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
                        ))
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
        if sec == S::Notifications {
        // ---- Extras --------------------------------------------------------
        {
            body = body.child(section_title("Extras"));
            let always = self.prefs.conductor_always_on;
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pb_1()
                    .child(
                        div()
                            .id("conductor-toggle")
                            .child(chip(
                                if always { "on".into() } else { "off".into() },
                                always,
                                hx,
                            ))
                            .on_click(cx.listener(|ws, _, _, cx| {
                                cx.stop_propagation();
                                ws.prefs.conductor_always_on = !ws.prefs.conductor_always_on;
                                if !ws.prefs.conductor_always_on {
                                    ws.conductor_view = None;
                                }
                                save_prefs_to(&ws.prefs_path, &ws.prefs);
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(div().text_sm().text_color(hx.tx1).child("Always-on conductor"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(hx.tx2)
                                    .child("Stays on screen, conducts while agents work, and says what they're doing. Drag him anywhere."),
                            ),
                    ),
            );
        }
        }

        if sec == S::Diff {
        // ---- Files: gitignored entries ---------------------------------------
        {
            let enabled = self.prefs.show_ignored_files;
            body = body.child(section_title("Files")).child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pb_1()
                    .child(
                        div()
                            .id("show-ignored-toggle")
                            .child(chip(
                                if enabled { "on".into() } else { "off".into() },
                                enabled,
                                hx,
                            ))
                            .on_click(cx.listener(|ws, _, _, cx| {
                                cx.stop_propagation();
                                ws.prefs.show_ignored_files = !ws.prefs.show_ignored_files;
                                save_prefs_to(&ws.prefs_path, &ws.prefs);
                                // Rebuilds the zed settings JSON, which is where
                                // the tree and the file finder read it from.
                                ws.apply_appearance(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(hx.tx1)
                            .child("Show gitignored files in the file tree and ⌘P"),
                    ),
            );
        }
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
                    div()
                        .id(gpui::SharedString::from(format!("diff-loc-{label}")))
                        .child(chip(label.to_string(), selected, hx))
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            cx.stop_propagation();
                            ws.set_diff_location(value, window, cx);
                        })),
                );
            }
            body = body.child(section_title("Review location")).child(row).child(
                div()
                    .text_xs()
                    .text_color(hx.tx2)
                    .pb_1()
                    .child(
                        "Tools panel keeps Review beside Files and Git on the right. \
                         A center tab gives Review the whole window; Files and Git \
                         still use the tools panel when it is open.",
                    ),
            );

            let view = self.prefs.diff_view;
            let mut view_row = div().flex().gap_1p5().pb_1();
            for value in [DiffView::Unified, DiffView::Split] {
                let selected = view == value;
                view_row = view_row.child(
                    div()
                        .id(gpui::SharedString::from(format!("diff-view-set-{}", value.label())))
                        .child(chip(value.label().to_string(), selected, hx))
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            cx.stop_propagation();
                            ws.set_diff_view(value, cx);
                        })),
                );
            }
            body = body.child(section_title("Diff layout")).child(view_row).child(
                div()
                    .text_xs()
                    .text_color(hx.tx2)
                    .pb_1()
                    .child(
                        "Split shows old and new side by side. A panel too narrow for two \
                         readable columns draws unified until you widen it.",
                    ),
            );
        }

        if !self.has_tmux {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(hx.tx2)
                    .pb_1()
                    .child("Install tmux for terminal/agent sessions that survive app restarts."),
            );
        }
        }

        if sec == S::Agents {
        // ---- Agents: enable/disable + launch-command override -------------
        let mut agents = div().flex().flex_col().gap_1().pb_1();
        for kind in agent_catalog(&self.prefs) {
            let key = kind.as_str().to_string();
            let pref = self.prefs.agents.get(&key).cloned().unwrap_or_default();
            let is_custom = matches!(kind, AgentKind::Other(_));
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
                            let is_custom_key =
                                AgentKind::known().iter().all(|k| k.as_str() != toggle_key);
                            let e = ws.prefs.agents.entry(toggle_key.clone()).or_default();
                            if is_custom_key {
                                e.custom = true;
                            }
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
            if is_custom {
                let remove_key = key.clone();
                row = row.child(
                    div()
                        .id(gpui::SharedString::from(format!("agent-rm-{key}")))
                        .flex_none()
                        .w(px(20.0))
                        .py_0p5()
                        .rounded_md()
                        .text_center()
                        .text_sm()
                        .text_color(hx.tx2)
                        .cursor_pointer()
                        .hover(|d| d.bg(hx.surface_hover).text_color(hx.red))
                        .child("×")
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            cx.stop_propagation();
                            ws.prefs.agents.remove(&remove_key);
                            save_prefs_to(&ws.prefs_path, &ws.prefs);
                            cx.notify();
                        })),
                );
            }
            agents = agents.child(row);
            // Signal / hook controls — custom agents only. Built-ins keep
            // their dedicated adapters; these prefs drive ConfigurableRuntime.
            if is_custom {
                agents = agents.child(self.render_agent_signal_row(hx, &key, &pref, cx));
            }
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
                    .py_1()
                    .child(
                        div()
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
                div()
                    .id("agent-add")
                    .mt_1()
                    .px_2()
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
            );
        }
        body = body
            .child(section_title("Agents"))
            .child(
                div()
                    .text_xs()
                    .text_color(hx.tx2)
                    .pb_1()
                    .child("Click a command to edit it (runs via $SHELL -lic, so your aliases and ~/.zshrc apply; empty restores the default). Custom agents can enable status hooks for busy/idle notifications. Enter saves, Esc cancels."),
            )
            .child(agents);
        // The reset button is always shown for discoverability, but stays
        // greyed out and inert while every built-in agent is already at its
        // default — clicking it would undo nothing. Custom agents are the
        // user's own additions, not defaults, so reset never touches them.
        let agents_dirty = self
            .prefs
            .agents
            .values()
            .any(|a| !a.custom && (a.disabled || a.command.is_some()));
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
                    // Drop built-in overrides only; keep the user's custom
                    // agents, which have no "default" to reset to.
                    ws.prefs.agents.retain(|_, a| a.custom);
                    save_prefs_to(&ws.prefs_path, &ws.prefs);
                    cx.notify();
                }));
        } else {
            reset = reset.text_color(hx.tx2);
        }
        body = body.child(reset);

        // ---- Terminals on quit ----------------------------------------------
        // The same choice the quit prompt's "Remember" box saves. Without this
        // row, a remembered answer could only be undone by editing prefs.json.
        {
            let current = self.prefs.close_terminals_on_exit;
            let mut row = div().flex().gap_1p5().pb_1();
            for (label, value) in [
                ("Ask", None),
                ("Keep running", Some(false)),
                ("Close", Some(true)),
            ] {
                row = row.child(
                    div()
                        .id(gpui::SharedString::from(format!("quit-terminals-{label}")))
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
                .child(
                    div()
                        .pb_1()
                        .text_xs()
                        .text_color(hx.tx2)
                        .child("Keep running leaves agent tmux sessions alive so they reattach on the next launch."),
                );
        }
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
        }
        let mut rail = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(140.0))
            .gap_0p5()
            .p_2()
            .border_r_1()
            .border_color(hx.border);
        for s in S::ALL {
            let selected = s == sec;
            rail = rail.child(
                div()
                    .id(gpui::SharedString::from(format!("settings-nav-{}", s.label())))
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
                        ws.settings_section = s;
                        cx.notify();
                    })),
            );
        }
        card = card.child(
            div()
                .flex()
                .flex_1()
                .h_full()
                .min_h_0()
                .when(crate::probe_paint(), |d| {
                    d.child(gpui::canvas(|b, _, _| log::info!("paint: settings content row = {b:?}"), |_, _, _, _| {}).absolute().size_full())
                })
                .child(rail)
                .child(body),
        );

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
            .on_click(cx.listener(|ws, _, _, cx| {
                ws.settings_open = false;
                cx.notify();
            }))
            .child(card.on_click(|_, _, cx| cx.stop_propagation()))
            .into_any_element()
    }

    /// Second row under a custom agent: cycle signal fidelity and hook-install
    /// dialect; when Manual + signal on, show the shim invocation to wire.
    fn render_agent_signal_row(
        &self,
        hx: &HarnessTheme,
        key: &str,
        pref: &AgentPref,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        use agent_runtime::{HookInstall, SignalProfile, WaitFidelity};

        let fidelity_label = match pref.signal.as_ref().map(|s| s.fidelity) {
            None => "signal: off",
            Some(WaitFidelity::BestEffort) => "signal: best",
            Some(WaitFidelity::Exact) => "signal: exact",
            Some(WaitFidelity::TerminalOnly) => "signal: off",
        };
        let install_label = match pref.signal.as_ref().map(|s| s.install) {
            None | Some(HookInstall::None) => "hooks: manual",
            Some(HookInstall::ClaudeLocalSettings) => "hooks: claude",
            Some(HookInstall::CodexUserHooks) => "hooks: codex",
        };
        let signal_on = pref.signal.as_ref().is_some_and(|s| {
            !matches!(s.fidelity, WaitFidelity::TerminalOnly)
        });
        let show_shim = signal_on
            && matches!(
                pref.signal.as_ref().map(|s| s.install),
                None | Some(HookInstall::None)
            );
        let shim_cmd = agent_runtime::harness_status_shim_command(&self.state_dir);

        let fid_key = key.to_string();
        let inst_key = key.to_string();

        let mut row = div()
            .flex()
            .items_center()
            .gap_2()
            .pl(px(28.0))
            .pb_1()
            .child(
                div()
                    .id(gpui::SharedString::from(format!("agent-sig-{key}")))
                    .flex_none()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .border_1()
                    .border_color(hx.border)
                    .bg(hx.surface)
                    .text_xs()
                    .text_color(if signal_on { hx.accent } else { hx.tx2 })
                    .cursor_pointer()
                    .hover(|d| d.bg(hx.surface_hover))
                    .child(fidelity_label)
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        cx.stop_propagation();
                        let e = ws.prefs.agents.entry(fid_key.clone()).or_default();
                        e.custom = true;
                        let install = e
                            .signal
                            .as_ref()
                            .map(|s| s.install)
                            .unwrap_or(HookInstall::None);
                        e.signal = match e.signal.as_ref().map(|s| s.fidelity) {
                            None | Some(WaitFidelity::TerminalOnly) => {
                                Some(SignalProfile::best_effort(install))
                            }
                            Some(WaitFidelity::BestEffort) => {
                                Some(SignalProfile::exact(install))
                            }
                            Some(WaitFidelity::Exact) => None,
                        };
                        save_prefs_to(&ws.prefs_path, &ws.prefs);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id(gpui::SharedString::from(format!("agent-inst-{key}")))
                    .flex_none()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .border_1()
                    .border_color(hx.border)
                    .bg(hx.surface)
                    .text_xs()
                    .text_color(if signal_on { hx.tx0 } else { hx.tx2 })
                    .cursor_pointer()
                    .hover(|d| d.bg(hx.surface_hover))
                    .child(install_label)
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        cx.stop_propagation();
                        let e = ws.prefs.agents.entry(inst_key.clone()).or_default();
                        e.custom = true;
                        // Turning on hooks without a fidelity still enables
                        // best-effort so install is meaningful.
                        let fidelity = e
                            .signal
                            .as_ref()
                            .map(|s| s.fidelity)
                            .filter(|f| !matches!(f, WaitFidelity::TerminalOnly))
                            .unwrap_or(WaitFidelity::BestEffort);
                        let next_install = match e.signal.as_ref().map(|s| s.install) {
                            None | Some(HookInstall::None) => HookInstall::ClaudeLocalSettings,
                            Some(HookInstall::ClaudeLocalSettings) => HookInstall::CodexUserHooks,
                            Some(HookInstall::CodexUserHooks) => HookInstall::None,
                        };
                        e.signal = Some(SignalProfile {
                            fidelity,
                            install: next_install,
                        });
                        save_prefs_to(&ws.prefs_path, &ws.prefs);
                        cx.notify();
                    })),
            );
        if show_shim {
            row = row.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .font_family(hx.font_mono)
                    .text_color(hx.tx2)
                    .child(shim_cmd),
            );
        }
        row
    }
}
