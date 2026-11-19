use super::super::*;
use super::settings::chip;
use crate::markdown_preview::markdown_style;
use crate::review_overview::review_file_reference;
use ::ui::prelude::Disableable;
use ::ui::{CommonAnimationExt, Icon, IconName, IconSize};
use markdown::MarkdownElement;

impl HarnessWorkspace {
    pub(super) fn render_review_overview_settings(
        &self,
        hx: &HarnessTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let prefs = &self.prefs.review_overview;
        let cli = prefs.cli;
        let mut clis = div().flex().flex_wrap().gap_1p5();
        for option in ReviewCli::ALL {
            clis = clis.child(
                div()
                    .id(gpui::SharedString::from(format!(
                        "review-overview-cli-{}",
                        option.label()
                    )))
                    .child(chip(option.label().to_string(), cli == option, hx))
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        ws.prefs.review_overview.cli = option;
                        save_prefs_to(&ws.prefs_path, &ws.prefs);
                        ws.ensure_review_model_catalog(cx);
                        cx.notify();
                    })),
            );
        }

        let catalog = self.review_model_catalogs.get(&cli);
        let options = match catalog {
            Some(ModelCatalog::Ready(options)) => options.as_slice(),
            _ => &[],
        };
        let selected = prefs.model();
        let selected_option = options
            .iter()
            .find(|option| Some(option.id.as_str()) == selected);
        let label = selected_option
            .map(|option| option.label.as_str())
            .or(selected)
            .unwrap_or("Use CLI default");
        let loading = matches!(catalog, Some(ModelCatalog::Loading));
        let workspace = cx.weak_entity();
        let menu = ::ui::ContextMenu::build(window, cx, |menu, _, _| {
            use ::ui::IconPosition;

            let default_workspace = workspace.clone();
            let mut menu = menu.toggleable_entry(
                "Use CLI default",
                selected.is_none(),
                IconPosition::End,
                None,
                move |_, cx| {
                    let _ = default_workspace
                        .update(cx, |ws, cx| ws.choose_review_model(cli, None, cx));
                },
            );
            if let Some(id) = selected.filter(|_| selected_option.is_none()) {
                menu =
                    menu.toggleable_entry(id.to_string(), true, IconPosition::End, None, |_, _| {});
            }
            for option in options {
                let is_selected = selected == Some(option.id.as_str());
                let id = option.id.clone();
                let workspace = workspace.clone();
                menu = menu.toggleable_entry(
                    option.label.clone(),
                    is_selected,
                    IconPosition::End,
                    None,
                    move |_, cx| {
                        let _ = workspace
                            .update(cx, |ws, cx| ws.choose_review_model(cli, Some(&id), cx));
                    },
                );
            }
            menu.separator().entry("Reload models", None, move |_, cx| {
                let _ = workspace.update(cx, |ws, cx| ws.load_review_model_catalog(cli, cx));
            })
        });
        let dropdown = ::ui::DropdownMenu::new(
            "review-overview-model",
            if loading { "Loading models…" } else { label }.to_string(),
            menu,
        )
        .style(::ui::DropdownStyle::Outlined)
        .disabled(loading)
        .full_width(true)
        .aria_label("Review overview model");
        let notice = match catalog {
            Some(ModelCatalog::Failed(error)) => Some(
                div()
                    .id("review-model-catalog-error")
                    .text_xs()
                    .text_color(hx.tx2)
                    .tooltip(tip(error.clone()))
                    .child(format!(
                        "Could not load {} models. Use Reload models to try again.",
                        cli.label()
                    ))
                    .into_any_element(),
            ),
            Some(ModelCatalog::Ready(options)) if options.is_empty() => Some(
                div()
                    .text_xs()
                    .text_color(hx.tx2)
                    .child(format!("{} reported no selectable models.", cli.label()))
                    .into_any_element(),
            ),
            _ => None,
        };

        div()
            .flex()
            .flex_col()
            .gap_2()
            .pb_2()
            .child(clis)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_sm().text_color(hx.tx1).child("Model"))
                    .child(div().flex_1().min_w_0().child(dropdown)),
            )
            .children(notice)
            .child(
                div().text_xs().text_color(hx.tx2).child(
                    "An overview sends committed and working-tree changes to the selected CLI.",
                ),
            )
            .into_any_element()
    }

    fn choose_review_model(&mut self, cli: ReviewCli, id: Option<&str>, cx: &mut Context<Self>) {
        let models = &mut self.prefs.review_overview.models;
        if let Some(id) = id {
            models.insert(cli, id.to_string());
        } else {
            models.remove(&cli);
        }
        save_prefs_to(&self.prefs_path, &self.prefs);
        cx.notify();
    }

    pub(crate) fn review_overview_button(
        &self,
        hx: &HarnessTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        overview_button("open-review-overview", "Overview", hx)
            .tooltip(tip("Review the full branch and working-tree changes"))
            .on_click(cx.listener(|ws, _, window, cx| {
                ws.open_tab(TabKind::ReviewOverview, window, cx);
            }))
    }

    pub(super) fn render_review_overview(
        &self,
        overview: &OverviewTab,
        hx: &HarnessTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let running = matches!(&overview.status, OverviewStatus::Running(_));
        let can_chat = matches!(&overview.status, OverviewStatus::Ready { saved, .. } if saved.report.generated);
        let action = if running {
            overview_button("cancel-review-overview", "Cancel", hx)
                .on_click(cx.listener(|ws, _, _, cx| ws.cancel_review_overview(cx)))
        } else {
            let label = match &overview.status {
                OverviewStatus::Ready { .. } => "Refresh",
                OverviewStatus::Waiting => "Generate",
                _ => "Run again",
            };
            overview_button("refresh-review-overview", label, hx)
                .on_click(cx.listener(|ws, _, _, cx| ws.run_review_overview(cx)))
        };
        let toolbar = div()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(hx.border)
            .child(
                div()
                    .id("review-overview-settings")
                    .min_w_0()
                    .truncate()
                    .cursor_pointer()
                    .text_xs()
                    .text_color(hx.tx2)
                    .hover(|d| d.text_color(hx.tx0))
                    .tooltip(tip("Change the CLI or model in Settings"))
                    .child(overview.preferences.label())
                    .on_click(cx.listener(|ws, _, _, cx| {
                        ws.settings_section = SettingsSection::Diff;
                        ws.settings_open = true;
                        ws.ensure_review_model_catalog(cx);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(can_chat, |row| {
                        row.child(
                            overview_button("chat-about-review", "Chat about this", hx).on_click(
                                cx.listener(|ws, _, window, cx| ws.chat_about_review(window, cx)),
                            ),
                        )
                    })
                    .child(action),
            );
        let content = match &overview.status {
            OverviewStatus::Running(_) => div()
                .flex()
                .items_center()
                .gap_3()
                .py_5()
                .text_color(hx.tx1)
                .child(review_spinner(IconSize::Custom(gpui::rems(1.5))))
                .child("Generating review overview…"),
            OverviewStatus::Waiting => div()
                .text_sm()
                .text_color(hx.tx2)
                .child("Generate a review overview for the current branch."),
            OverviewStatus::Cancelled => div()
                .text_sm()
                .text_color(hx.tx2)
                .child("Overview cancelled."),
            OverviewStatus::Failed(error) => div()
                .flex()
                .flex_col()
                .gap_2()
                .text_sm()
                .text_color(hx.tx1)
                .child(
                    div()
                        .text_color(hx.red)
                        .child("Could not generate the overview"),
                )
                .child(error.clone()),
            OverviewStatus::Ready { markdown, saved } => div()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_xs()
                        .text_color(hx.tx2)
                        .children(
                            saved
                                .report
                                .scope
                                .iter()
                                .cloned()
                                .map(|repo| div().child(repo)),
                        )
                        .children(
                            saved
                                .report
                                .limitations
                                .iter()
                                .cloned()
                                .map(|note| div().text_color(hx.amber).child(note)),
                        ),
                )
                .child(render_review_markdown(markdown, saved, hx, cx)),
        };

        div()
            .id("review-overview")
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .min_w_0()
            .bg(hx.bg1)
            .child(toolbar)
            .child(
                div()
                    .id("review-overview-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_5()
                    .py_4()
                    .child(content.max_w(px(850.0))),
            )
            .into_any_element()
    }
}

fn render_review_markdown(
    markdown: &Entity<markdown::Markdown>,
    saved: &Arc<StoredReviewOverview>,
    hx: &HarnessTheme,
    cx: &Context<HarnessWorkspace>,
) -> MarkdownElement {
    let workspace = cx.weak_entity();
    let linked = Arc::clone(saved);
    let opened = Arc::clone(saved);
    let multi_repo = saved.report.scope.len() > 1;
    let mut style = markdown_style(hx, cx);
    style.inline_code.color = Some(hx.tx1.into());
    MarkdownElement::new(markdown.clone(), style)
        .on_code_span_link(move |code, _| {
            let (index, line) = review_file_reference(&linked.report.files, code, multi_repo)?;
            Some(format!("review-file:{index}:{}", line.unwrap_or(0)).into())
        })
        .on_url_click(move |url, window, cx| {
            let Some((index, line)) = url
                .strip_prefix("review-file:")
                .and_then(|url| url.split_once(':'))
            else {
                return;
            };
            let (Ok(index), Ok(line)) = (index.parse::<usize>(), line.parse::<u32>()) else {
                return;
            };
            let Some(file) = opened.report.files.get(index) else {
                return;
            };
            let _ = workspace.update(cx, |ws, cx| {
                if !file.root.join(&file.relative).is_file() {
                    ws.set_note(format!(
                        "{} is no longer in the working tree",
                        file.relative
                    ));
                    cx.notify();
                    return;
                }
                ws.exit_hooks_edit(window, cx);
                ws.open_workspace_file(
                    &file.root,
                    &file.relative,
                    (line > 0).then_some(line),
                    window,
                    cx,
                );
            });
        })
}

pub(super) fn review_spinner(size: IconSize) -> impl IntoElement {
    Icon::new(IconName::LoadCircle)
        .size(size)
        .color(::ui::Color::Accent)
        .with_rotate_animation(1)
}

fn overview_button(
    id: &'static str,
    label: &'static str,
    hx: &HarnessTheme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .px_2()
        .py_0p5()
        .rounded_md()
        .text_xs()
        .text_color(hx.tx0)
        .bg(hx.surface)
        .hover(|d| d.bg(hx.surface_hover))
        .cursor_pointer()
        .child(label)
}
