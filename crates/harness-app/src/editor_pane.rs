//! Spike 3b — a real zed `editor::Editor` pane, out-of-tree, with tree-sitter
//! syntax highlighting driven by the harness theme (Spike 3c).
//!
//! This is the §8 R1 go/no-go payload: it proves zed's editor crate is usable
//! as an embedded pane in our GPUI app without the `workspace` crate. We build a
//! Rust `Language` directly (grammars crate's embedded config + queries +
//! `tree-sitter-rust`), so highlighting needs neither the heavy `languages`
//! crate nor a `Project`.
//!
//! What is intentionally NOT here (mirrors Spike 1's terminal scope): no LSP /
//! rust-analyzer (that needs a `Project` + worktree — see SPIKE_NOTES Phase D),
//! and no editing keymap yet (typing flows through the IME input handler; arrow
//! keys / backspace need a keymap, deferred to the MVP chrome pass).

use std::path::Path;
use std::sync::Arc;

use editor::Editor;
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    Styled, Window, div, px,
};
use language::{Buffer, Language};
use theme::{ActiveTheme, Theme};

use crate::theme::{HarnessTheme, theme};

pub struct EditorPane {
    editor: Entity<Editor>,
    title: String,
}

impl EditorPane {
    pub fn new(worktree_root: &Path, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // The active theme (installed in main.rs) carries the SyntaxTheme that
        // maps highlight captures → colors.
        let active = cx.theme().clone();
        let language = build_rust_language(&active);

        let (title, source) = load_source(worktree_root);
        let buffer = cx.new(|cx| {
            let mut buffer = Buffer::local(source, cx);
            buffer.set_language(Some(language.clone()), cx);
            buffer
        });

        let editor = cx.new(|cx| Editor::for_buffer(buffer, None, window, cx));
        Self { editor, title }
    }

}

impl Focusable for EditorPane {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl Render for EditorPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hx: HarnessTheme = theme(cx).clone();
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(hx.bg0)
            .child(
                // Slim file header so it reads as an editor tab, not raw text.
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .h(px(24.0))
                    .px_3()
                    .bg(hx.bg1)
                    .border_b_1()
                    .border_color(hx.border)
                    .child(
                        div()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(hx.tx1)
                            .child(self.title.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(hx.tx2)
                            .child("rust · tree-sitter"),
                    ),
            )
            .child(div().flex_1().min_h_0().child(self.editor.clone()))
    }
}

/// Build a Rust `Language` with highlighting, using zed's embedded rust config
/// and query set (from the light `grammars` crate) plus `tree-sitter-rust`.
/// `set_theme` binds the highlight captures to the active theme's SyntaxTheme.
fn build_rust_language(active: &Theme) -> Arc<Language> {
    let config = grammars::load_config("rust");
    let queries = grammars::load_queries("rust");
    let language = Language::new(config, Some(tree_sitter_rust::LANGUAGE.into()))
        .with_queries(queries)
        .expect("rust queries parse");
    language.set_theme(active.syntax());
    Arc::new(language)
}

/// Load a real Rust file from the worktree so the pane shows live code; fall
/// back to a bundled sample so the spike always renders something highlighted.
fn load_source(root: &Path) -> (String, String) {
    let candidates = [
        root.join("src/main.rs"),
        root.join("crates/harness-app/src/main.rs"),
        root.join("src/lib.rs"),
    ];
    for path in candidates {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "source.rs".to_string());
            return (name, text);
        }
    }
    ("sample.rs".to_string(), SAMPLE_SOURCE.to_string())
}

const SAMPLE_SOURCE: &str = r#"//! Fallback sample shown when no worktree source file is found.
use std::collections::HashMap;

/// A worktree card in the harness sidebar.
#[derive(Debug, Clone)]
pub struct Worktree<'a> {
    pub name: &'a str,
    pub dirty_files: usize,
    pub status: Status,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Status {
    Running,
    Waiting,
    Idle,
}

impl<'a> Worktree<'a> {
    pub fn label(&self) -> String {
        match self.status {
            Status::Running => format!("{} is running", self.name),
            Status::Waiting => format!("{} needs input", self.name),
            Status::Idle => format!("{} is idle", self.name),
        }
    }
}

fn main() {
    let counts: HashMap<&str, usize> = HashMap::new();
    let total = 42_usize;
    println!("harness up — {} worktrees, {total} tasks", counts.len());
}
"#;
