//! Files → Search: harness project find-in-files (⌘⇧F).
//!
//! Compact results: one file header + each matching line. Clicking a line opens
//! the file in the center editor and scrolls that line into view.

use std::collections::{BTreeMap, HashSet};
use std::ops::Range;
use std::time::Duration;

use language::{Buffer, Point};
use project::search::{SearchQuery, SearchResult};
use util::paths::{PathMatcher, PathStyle};

use super::super::*;

/// Cap how many match rows we keep in the panel (project search can explode).
const MAX_HITS: usize = 2_000;
const SEARCH_DEBOUNCE_MS: u64 = 150;

impl HarnessWorkspace {
    /// Create the query editor if missing and wire BufferEdited → debounced search.
    pub(crate) fn ensure_project_search_ui(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.project_search.is_some() {
            return;
        }
        let query = cx.new(|cx| {
            let mut e = Editor::single_line(window, cx);
            e.set_placeholder_text("Search project…", window, cx);
            e
        });
        let _query_sub = cx.subscribe(&query, |ws, _editor, event, cx| {
            if matches!(event, editor::EditorEvent::BufferEdited) {
                ws.schedule_project_search_debounced(cx);
            }
        });
        self.project_search = Some(ProjectSearchUi {
            query,
            _query_sub,
            generation: 0,
            results: Vec::new(),
            searching: false,
            status: String::new(),
        });
    }

    fn schedule_project_search_debounced(&mut self, cx: &mut Context<Self>) {
        let search_id = {
            let Some(ui) = self.project_search.as_mut() else {
                return;
            };
            ui.generation = ui.generation.wrapping_add(1);
            ui.searching = true;
            ui.status = "Searching…".into();
            ui.generation
        };
        cx.notify();
        self.run_project_search_after_debounce(search_id, cx);
    }

    /// Public entry used when selection changes (already have a Window).
    pub(crate) fn schedule_project_search(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_project_search_ui(window, cx);
        let search_id = {
            let Some(ui) = self.project_search.as_mut() else {
                return;
            };
            ui.generation = ui.generation.wrapping_add(1);
            ui.searching = true;
            ui.status = "Searching…".into();
            ui.generation
        };
        cx.notify();
        self.start_project_search(search_id, cx);
    }

    fn run_project_search_after_debounce(&mut self, search_id: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(SEARCH_DEBOUNCE_MS))
                .await;
            this.update(cx, |ws, cx| {
                let Some(ui) = ws.project_search.as_ref() else {
                    return;
                };
                if ui.generation != search_id {
                    return;
                }
                ws.start_project_search(search_id, cx);
            })
            .ok();
        })
        .detach();
    }

    fn start_project_search(&mut self, search_id: u64, cx: &mut Context<Self>) {
        let Some(ui) = self.project_search.as_ref() else {
            return;
        };
        let query_text = ui.query.read(cx).text(cx);
        let query_text = query_text.trim().to_string();
        if query_text.is_empty() {
            if let Some(ui) = self.project_search.as_mut() {
                ui.results.clear();
                ui.searching = false;
                ui.status.clear();
            }
            cx.notify();
            return;
        }
        let Some(hosted) = self.editor_workspace() else {
            if let Some(ui) = self.project_search.as_mut() {
                ui.searching = false;
                ui.status = "Open a project to search".into();
            }
            cx.notify();
            return;
        };
        let Ok(matcher_empty) = PathMatcher::new(None::<&str>, PathStyle::local()) else {
            return;
        };
        let Ok(search_query) = SearchQuery::text(
            query_text,
            false,
            false,
            false,
            matcher_empty.clone(),
            matcher_empty,
            false,
            None,
        ) else {
            if let Some(ui) = self.project_search.as_mut() {
                ui.searching = false;
                ui.status = "Invalid search query".into();
            }
            cx.notify();
            return;
        };
        let results = hosted.update(cx, |workspace, cx| {
            workspace
                .project()
                .update(cx, |project, cx| project.search(search_query, cx))
        });
        self.collect_project_search_results(search_id, results, cx);
    }

    fn collect_project_search_results(
        &mut self,
        search_id: u64,
        results: project::SearchResults<SearchResult>,
        cx: &mut Context<Self>,
    ) {
        if let Some(ui) = self.project_search.as_mut()
            && ui.generation == search_id
        {
            ui.results.clear();
        }
        cx.spawn(async move |this, cx| {
            let project::SearchResults {
                task_handle,
                rx,
            } = results;
            // Keep the search task alive until the stream ends.
            let _search_task = task_handle;

            let mut by_path: BTreeMap<String, Vec<ProjectSearchHit>> = BTreeMap::new();
            let mut total = 0usize;
            let mut limit_reached = false;

            while let Ok(result) = rx.recv().await {
                match result {
                    SearchResult::Buffer { buffer, ranges } => {
                        if total >= MAX_HITS {
                            limit_reached = true;
                            break;
                        }
                        let remaining = MAX_HITS - total;
                        let capped = ranges.len().min(remaining);
                        let hits = extract_hits(&buffer, &ranges[..capped], cx);
                        for (path, hit) in hits {
                            by_path.entry(path).or_default().push(hit);
                            total += 1;
                        }
                        if capped < ranges.len() {
                            limit_reached = true;
                            break;
                        }
                    }
                    SearchResult::LimitReached => limit_reached = true,
                    SearchResult::WaitingForScan | SearchResult::Searching => {}
                }

                let snapshot: Vec<ProjectSearchFile> = by_path
                    .iter()
                    .map(|(path, hits)| ProjectSearchFile {
                        path: path.clone(),
                        hits: hits.clone(),
                    })
                    .collect();
                let status = if limit_reached {
                    format!("Showing first {MAX_HITS} matches…")
                } else {
                    format!(
                        "{} file{}, {} match{}",
                        snapshot.len(),
                        if snapshot.len() == 1 { "" } else { "s" },
                        total,
                        if total == 1 { "" } else { "es" },
                    )
                };
                let _ = this.update(cx, |ws, cx| {
                    let Some(ui) = ws.project_search.as_mut() else {
                        return;
                    };
                    if ui.generation != search_id {
                        return;
                    }
                    ui.results = snapshot;
                    ui.searching = true;
                    ui.status = status;
                    cx.notify();
                });
                if limit_reached {
                    break;
                }
            }

            let final_files: Vec<ProjectSearchFile> = by_path
                .into_iter()
                .map(|(path, hits)| ProjectSearchFile { path, hits })
                .collect();
            let total = final_files.iter().map(|f| f.hits.len()).sum::<usize>();
            let status = if total == 0 {
                "No matches".into()
            } else if limit_reached {
                format!("Showing first {MAX_HITS} matches")
            } else {
                format!(
                    "{} file{}, {} match{}",
                    final_files.len(),
                    if final_files.len() == 1 { "" } else { "s" },
                    total,
                    if total == 1 { "" } else { "es" },
                )
            };
            let _ = this.update(cx, |ws, cx| {
                let Some(ui) = ws.project_search.as_mut() else {
                    return;
                };
                if ui.generation != search_id {
                    return;
                }
                ui.results = final_files;
                ui.searching = false;
                ui.status = status;
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn open_project_search_hit(
        &mut self,
        path: &str,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(r) = self.resolve() else {
            return;
        };
        let root = r.editor_path.clone();
        let Some(hosted) = self.ensure_service_workspace(window, cx) else {
            return;
        };
        crate::workspace_host::open_file_at(&hosted, &root, path, Some(line), true, window, cx);
    }

    /// Files → Search body: query field + compact match list.
    pub(crate) fn render_project_search_panel(
        &self,
        hx: &HarnessTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(ui) = self.project_search.as_ref() else {
            return div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(hx.tx2)
                .child("Search will appear here")
                .into_any_element();
        };

        let query = ui.query.clone();
        let status = ui.status.clone();
        let searching = ui.searching;

        let mut list = div()
            .id("project-search-results")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_1()
            .py_1()
            .gap_1();

        for file in &ui.results {
            let path = file.path.clone();
            list = list.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .px_1p5()
                            .pt_1()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .font_family(hx.font_mono)
                            .text_color(hx.tx1)
                            .child(file.path.clone()),
                    )
                    .children(file.hits.iter().enumerate().map(|(i, hit)| {
                        let path = path.clone();
                        let line = hit.line;
                        let label = format!("{:>5}  {}", hit.line, hit.text);
                        div()
                            .id(gpui::SharedString::from(format!(
                                "search-hit-{}-{}-{}",
                                path, line, i
                            )))
                            .px_1p5()
                            .py_0p5()
                            .rounded_sm()
                            .cursor_pointer()
                            .hover(|d| d.bg(hx.surface_hover))
                            .font_family(hx.font_mono)
                            .text_xs()
                            .text_color(hx.tx0)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(label)
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                ws.open_project_search_hit(&path, line, window, cx);
                            }))
                    })),
            );
        }

        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .border_b_1()
                    .border_color(hx.border)
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .h(px(26.0))
                            .px_2()
                            .rounded_md()
                            .bg(hx.bg0)
                            .border_1()
                            .border_color(hx.border)
                            .key_context("HarnessInput")
                            .child(query),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(if searching { hx.accent } else { hx.tx2 })
                    .child(if status.is_empty() {
                        if searching {
                            "Searching…".into()
                        } else {
                            "Type to search the project".into()
                        }
                    } else {
                        status
                    }),
            )
            .child(list)
            .into_any_element()
    }
}

fn extract_hits(
    buffer: &Entity<Buffer>,
    ranges: &[Range<language::Anchor>],
    cx: &gpui::AsyncApp,
) -> Vec<(String, ProjectSearchHit)> {
    buffer.read_with(cx, |buf, _cx| {
        let Some(file) = buf.file() else {
            return Vec::new();
        };
        let path = file.path().display(PathStyle::local()).to_string();
        let mut out = Vec::with_capacity(ranges.len());
        let mut seen_lines = HashSet::new();
        for range in ranges {
            let start_offset: usize = buf.summary_for_anchor(&range.start);
            let point = buf.offset_to_point(start_offset);
            let line = point.row + 1;
            if !seen_lines.insert(line) {
                // One row per matched line in the panel.
                continue;
            }
            let line_start = Point::new(point.row, 0);
            let line_end = Point::new(point.row, buf.line_len(point.row));
            let text: String = buf
                .text_for_range(line_start..line_end)
                .collect::<String>()
                .trim_end_matches(['\r', '\n'])
                .to_string();
            out.push((path.clone(), ProjectSearchHit { line, text }));
        }
        out
    })
}
