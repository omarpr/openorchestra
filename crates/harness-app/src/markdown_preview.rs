//! Rendered-markdown preview for the diff panel.
//!
//! Zed lets you preview a `.md` file instead of reading its source; the diff
//! panel now does the same. A unified diff of prose is genuinely hard to read —
//! wrapped paragraphs turn into long `+` lines with no structure — so for
//! markdown the useful question is usually "what does the document look like
//! now?", not "which bytes moved".
//!
//! Rendering goes through zed's own `markdown` crate rather than a hand-rolled
//! parser, so headings, lists, tables, links, and syntax-highlighted code
//! blocks all match what the editor shows, and the styling is driven from the
//! same `HarnessTheme` tokens as the rest of the chrome (§3.5).
//!
//! The preview shows the file's **current on-disk content**, which is what the
//! diff is producing. A deleted file has nothing to preview and says so.

use std::path::{Path, PathBuf};

use gpui::{
    App, AppContext, Context, Entity, FontWeight, IntoElement, ParentElement, Render, Styled,
    Window, div, prelude::*, px,
};
use markdown::{Markdown, MarkdownElement, MarkdownStyle};

use crate::theme::{HarnessTheme, theme};

/// Whether a path is markdown, and therefore previewable.
pub fn is_markdown(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
}

pub struct MarkdownPreview {
    markdown: Option<Entity<Markdown>>,
    /// Why there is nothing to show, when there isn't.
    note: Option<String>,
    path: PathBuf,
}

impl MarkdownPreview {
    /// Build a preview of `abs_path`'s current contents.
    pub fn new(abs_path: PathBuf, cx: &mut Context<Self>) -> Self {
        match std::fs::read_to_string(&abs_path) {
            Ok(source) => {
                // No language registry: the syntax highlighting inside fenced
                // code blocks is a nice-to-have, and wiring the hosted
                // workspace's registry in here would couple the diff panel to
                // the editor tab's lazily-built Project.
                let markdown = cx.new(|cx| Markdown::new(source.into(), None, None, cx));
                MarkdownPreview {
                    markdown: Some(markdown),
                    note: None,
                    path: abs_path,
                }
            }
            Err(e) => MarkdownPreview {
                markdown: None,
                // A deleted file is the common case here, not an error worth
                // shouting about.
                note: Some(if e.kind() == std::io::ErrorKind::NotFound {
                    "file no longer exists — nothing to preview".to_string()
                } else {
                    format!("could not read file: {e}")
                }),
                path: abs_path,
            },
        }
    }

    /// Filename for the preview's own header — the panel header shows the
    /// branch, not which file is being previewed.
    fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

/// Map the harness tokens onto zed's markdown styling, so a preview looks like
/// part of the app rather than a foreign document.
pub(crate) fn markdown_style(hx: &HarnessTheme, cx: &App) -> MarkdownStyle {
    let mut style = MarkdownStyle {
        base_text_style: gpui::TextStyle {
            color: hx.tx1.into(),
            font_family: hx.font_ui.into(),
            font_size: px(13.0).into(),
            ..Default::default()
        },
        // Fenced code blocks read as code, in the mono face the rest of the
        // app uses for code.
        code_block_overflow_x_scroll: true,
        rule_color: hx.border.into(),
        block_quote_border_color: hx.border2.into(),
        selection_background_color: hx.accent.into(),
        ..Default::default()
    };
    style.inline_code.font_family = Some(hx.font_mono.into());
    style.inline_code.color = Some(hx.accent.into());
    style.block_quote.color = Some(hx.tx2.into());
    style.link.color = Some(hx.accent.into());
    style.link.underline = Some(gpui::UnderlineStyle {
        thickness: px(1.0),
        color: Some(hx.accent.into()),
        wavy: false,
    });
    style.heading.text = gpui::TextStyleRefinement {
        color: Some(hx.tx0.into()),
        font_weight: Some(FontWeight::SEMIBOLD),
        ..Default::default()
    };
    // Code blocks pick up the same syntax theme the editor uses — the one
    // generated from these tokens in §3.5.
    style.syntax = theme::ActiveTheme::theme(cx).syntax().clone();
    style
}

impl Render for MarkdownPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hx = theme(cx).clone();
        let body = match (&self.markdown, &self.note) {
            (Some(markdown), _) => {
                MarkdownElement::new(markdown.clone(), markdown_style(&hx, cx)).into_any_element()
            }
            (None, Some(note)) => div()
                .p_2()
                .text_xs()
                .text_color(hx.tx2)
                .child(note.clone())
                .into_any_element(),
            (None, None) => div().into_any_element(),
        };

        div()
            .id("markdown-preview")
            .size_full()
            .overflow_y_scroll()
            .child(
                // Sticky-ish file header (§3.4): without it a rendered document
                // gives no clue which file you are looking at.
                div()
                    .flex_none()
                    .px_3()
                    .py_1()
                    .bg(hx.bg1)
                    .border_b_1()
                    .border_color(hx.border)
                    .text_xs()
                    .font_family(hx.font_mono)
                    .text_color(hx.tx2)
                    .child(self.file_name()),
            )
            .child(div().p_3().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::is_markdown;

    #[test]
    fn markdown_extensions_are_recognized_case_insensitively() {
        for path in [
            "README.md",
            "docs/architecture.MD",
            "notes.markdown",
            "a/b/c.Md",
        ] {
            assert!(is_markdown(path), "{path} should be previewable");
        }
    }

    #[test]
    fn non_markdown_files_are_not_offered_a_preview() {
        // `.mdx` and `.mdown` are deliberately excluded: zed's renderer does
        // not handle MDX's embedded JSX, and offering a preview that silently
        // mangles the file is worse than not offering one.
        for path in [
            "src/main.rs",
            "Cargo.toml",
            "page.mdx",
            "notes.mdown",
            "README",
            "a.md.bak",
        ] {
            assert!(!is_markdown(path), "{path} should not be previewable");
        }
    }
}
