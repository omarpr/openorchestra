//! The Markdown preview "eye" button — a pane toolbar item.
//!
//! Zed shows an eye icon at the top-right of an editor whose buffer is
//! Markdown; clicking it opens the rendered preview, alt-clicking opens it in a
//! split. We already had the *action* side (`::markdown_preview::init` runs in
//! `main.rs`, and ⌘⇧M / ⌘⌥M are bound in `shortcuts.rs`) — this adds the
//! affordance, so the feature is discoverable rather than folklore.
//!
//! ## Why this is reimplemented rather than imported
//!
//! Unlike `git_ui` and `markdown_preview`, this button is **not in a library
//! crate**. Zed keeps it at `crates/zed/src/zed/quick_action_bar/preview.rs` —
//! a module inside its *binary* crate — and at rev `952d712` `quick_action_bar`
//! is not a standalone crate either. There is nothing to depend on.
//!
//! Everything it actually needs is public, so this is a small rebuild against
//! stable API rather than a fork: `MarkdownPreviewView::
//! resolve_active_item_as_markdown_editor` supplies the "is this Markdown?"
//! test, and `ToolbarItemView` supplies the placement. A future rev bump that
//! moves either one breaks this at *compile* time, not silently.
//!
//! Zed's version also covers SVG and CSV (the latter behind a feature flag).
//! Deliberately Markdown-only here — that is what was asked for, and the
//! `PreviewType` branching is not worth carrying for surfaces we do not ship.
//!
//! ## The `::` on every `markdown_preview` path
//!
//! This crate has its **own** `markdown_preview` module (the diff-panel
//! renderer), which shadows zed's crate of the same name. Every reference below
//! uses the leading-`::` form for exactly the reason `main.rs` does at its
//! `::markdown_preview::init(cx)` call: without it the path resolves to the
//! local module and the code silently means something else.

use gpui::{Action, Entity, EventEmitter, Modifiers, WeakEntity};
use ui::{Tooltip, prelude::*, text_for_keystroke};
use workspace::{ItemHandle, ToolbarItemEvent, ToolbarItemLocation, ToolbarItemView, Workspace};

/// Renders the eye icon when — and only when — the active pane item is a
/// Markdown editor.
pub struct MarkdownPreviewButton {
    workspace: WeakEntity<Workspace>,
    /// Whether the active pane item is Markdown, as decided by the last
    /// `set_active_pane_item`. Cached rather than recomputed in `render`
    /// because the two must agree: `render` drawing nothing while
    /// `set_active_pane_item` reported a visible location is what puts an
    /// empty toolbar strip above the editor (see the impl below).
    show: bool,
    /// The Markdown editor this button sits above. The click previews *this*
    /// editor, not whatever the window's focus happens to name (see `render`).
    editor: Option<WeakEntity<editor::Editor>>,
}

impl MarkdownPreviewButton {
    pub fn new(workspace: &Workspace) -> Self {
        Self {
            workspace: workspace.weak_handle(),
            show: false,
            editor: None,
        }
    }

    /// Whether `item` is an editor over a Markdown buffer.
    ///
    /// Asks zed's own predicate rather than sniffing the file extension here:
    /// `is_markdown_file` is what `OpenPreview` itself consults, so sharing it
    /// keeps the button from appearing on files the action would refuse — or
    /// hiding on ones it would accept.
    fn markdown_editor(
        item: Option<&dyn ItemHandle>,
        cx: &mut Context<Self>,
    ) -> Option<Entity<editor::Editor>> {
        use ::markdown_preview::markdown_preview_view::MarkdownPreviewView;
        let item = item?;
        // A preview answers `act_as::<Editor>` with the editor it renders (zed
        // `markdown_preview_view.rs`, `act_as_type`), so without this the eye
        // showed on the preview itself, where a click only re-selects it.
        // Zed's own quick-action button has the same blind spot.
        if item.act_as::<MarkdownPreviewView>(cx).is_some() {
            return None;
        }
        // Resolve the editor first so the `&App` borrow ends before
        // `is_markdown_file` takes `cx` mutably.
        let editor = item.act_as::<editor::Editor>(cx)?;
        ::markdown_preview::markdown_preview_view::MarkdownPreviewView::is_markdown_file(
            &editor, cx,
        )
        .then_some(editor)
    }
}

impl EventEmitter<ToolbarItemEvent> for MarkdownPreviewButton {}

impl ToolbarItemView for MarkdownPreviewButton {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ToolbarItemLocation {
        let editor = Self::markdown_editor(active_pane_item, cx);
        self.show = editor.is_some();
        self.editor = editor.map(|e| e.downgrade());
        cx.notify();
        // `Hidden` — not "PrimaryRight rendering nothing" — is load-bearing.
        // `Toolbar::render` collapses to an empty div only when EVERY item is
        // Hidden (zed `toolbar.rs:111`), so a button that claims a location on
        // non-Markdown files would draw a bordered, padded toolbar strip above
        // every editor pane in the app.
        if self.show {
            ToolbarItemLocation::PrimaryRight
        } else {
            ToolbarItemLocation::Hidden
        }
    }
}

impl Render for MarkdownPreviewButton {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if !self.show {
            return div().into_any_element();
        }

        let workspace = self.workspace.clone();
        let editor = self.editor.clone();

        IconButton::new("toggle-markdown-preview", IconName::Eye)
            .icon_size(IconSize::Small)
            .style(ButtonStyle::Subtle)
            .tooltip(move |_window, cx| {
                Tooltip::with_meta(
                    "Preview Markdown",
                    // Renders the live ⌘⇧M binding into the tooltip. Because it
                    // is looked up from the action, a rebind in the settings
                    // screen keeps the tooltip honest for free.
                    Some(&::markdown_preview::OpenPreview as &dyn Action),
                    // The split gesture is otherwise entirely invisible.
                    format!(
                        "{} to open in a split",
                        text_for_keystroke(&Modifiers::alt(), "click", cx)
                    ),
                    cx,
                )
            })
            .on_click(move |_, window, cx| {
                let (Some(workspace), Some(editor)) =
                    (workspace.upgrade(), editor.as_ref().and_then(|e| e.upgrade()))
                else {
                    return;
                };
                // Call zed's preview entry points on this button's editor and
                // pane directly. It used to dispatch `OpenPreview`, but
                // `Window::dispatch_action` routes from the *focused* element:
                // in a harness file tab, focus is often outside the hosted
                // Workspace (the tab strip, the tools panel's Git panel), so
                // the action had no handler and the click did nothing; or it
                // sat on another item (the commit view a file was opened
                // from), which zed then judged not Markdown. ⌘⇧M keeps the
                // action route, where focus being in the editor is a given.
                use ::markdown_preview::markdown_preview_view::MarkdownPreviewView;
                let to_the_side = window.modifiers().alt;
                workspace.update(cx, |workspace, cx| {
                    let Some(pane) = workspace.pane_for(&editor) else {
                        return;
                    };
                    if to_the_side {
                        MarkdownPreviewView::open_preview_to_the_side_of_pane(
                            workspace, editor, pane, window, cx,
                        );
                    } else {
                        MarkdownPreviewView::open_preview_in_pane(workspace, editor, pane, window, cx);
                    }
                });
            })
            .into_any_element()
    }
}
