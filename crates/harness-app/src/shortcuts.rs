//! The keyboard-shortcut catalog — one table, two consumers.
//!
//! `main.rs` binds from it and the settings screen renders from it, so the
//! settings screen structurally cannot drift from what is actually bound.
//! A shortcut documented here but bound elsewhere (or vice versa) is the bug
//! this module exists to make impossible.
//!
//! Precedence note (verified against gpui's `bindings_for_input`): bindings
//! are ranked by *context depth*, so an "Editor"-scoped ⌘C beats the global
//! ⌘C when focus is inside an editor pane. That is what lets the diff panel
//! and the editor share ⌘C without shadowing each other.

use gpui::KeyBinding;

/// Where a shortcut applies — used both as gpui context predicate and as the
/// section header on the settings screen.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Fires anywhere in the app.
    Global,
    /// Inside an editor tab's hosted zed workspace (file tree, panes, …).
    Workspace,
    /// When focus is in a text editor buffer.
    Editor,
    /// Inside a modal picker (file finder). zed's own context name.
    Picker,
}

impl Scope {
    fn context(self) -> Option<&'static str> {
        match self {
            Scope::Global => None,
            Scope::Workspace => Some("Workspace"),
            Scope::Editor => Some("Editor"),
            Scope::Picker => Some("Picker"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Scope::Global => "Global",
            Scope::Workspace => "Editor tab",
            Scope::Editor => "Text editing",
            Scope::Picker => "File finder",
        }
    }
}

pub struct Entry {
    /// gpui keystroke syntax (`cmd-shift-z`). Also parsed for display.
    pub keys: &'static str,
    pub scope: Scope,
    pub what: &'static str,
    /// `None` for shortcuts that reach the user another way (documented-only
    /// entries would go here); everything below binds.
    pub binding: Option<KeyBinding>,
}

impl Entry {
    fn new(
        keys: &'static str,
        scope: Scope,
        what: &'static str,
        action: impl gpui::Action,
    ) -> Self {
        Entry {
            keys,
            scope,
            what,
            binding: Some(KeyBinding::new(keys, action, scope.context())),
        }
    }
}

/// The catalog. Order within a scope is display order.
pub fn entries() -> Vec<Entry> {
    use editor::actions as ed;

    vec![
        // ---- Global ------------------------------------------------------
        Entry::new("cmd-t", Scope::Global, "New terminal tab", crate::NewTerminal),
        Entry::new("cmd-e", Scope::Global, "New editor tab", crate::NewEditor),
        Entry::new("cmd-shift-c", Scope::Global, "New agent chat tab", crate::NewChat),
        Entry::new(
            "cmd-c",
            Scope::Global,
            "Copy selected diff lines (in the diff panel)",
            crate::CopyDiffSelection,
        ),
        Entry::new("cmd-,", Scope::Global, "Open settings", crate::OpenSettings),
        Entry::new("cmd-q", Scope::Global, "Quit", crate::Quit),

        // ---- Editor tab (hosted zed workspace) ---------------------------
        Entry::new(
            "cmd-p",
            Scope::Workspace,
            "Find file by name",
            ::workspace::ToggleFileFinder::default(),
        ),
        Entry::new(
            "cmd-shift-f",
            Scope::Workspace,
            "Find in files (project search)",
            ::workspace::pane::DeploySearch::default(),
        ),
        Entry::new(
            "cmd-shift-e",
            Scope::Workspace,
            "Focus the file tree",
            zed_actions::project_panel::ToggleFocus,
        ),
        Entry::new(
            "cmd-s",
            Scope::Workspace,
            "Save file",
            ::workspace::Save { save_intent: None },
        ),

        // ---- Text editing -------------------------------------------------
        // ⌘C here deliberately shadows the global diff-copy: context depth
        // wins, so inside a buffer ⌘C copies editor text.
        Entry::new("cmd-c", Scope::Editor, "Copy", ed::Copy),
        Entry::new("cmd-x", Scope::Editor, "Cut", ed::Cut),
        Entry::new("cmd-v", Scope::Editor, "Paste", ed::Paste),
        Entry::new("cmd-z", Scope::Editor, "Undo", ed::Undo),
        Entry::new("cmd-shift-z", Scope::Editor, "Redo", ed::Redo),
        Entry::new("cmd-a", Scope::Editor, "Select all", ed::SelectAll),
        Entry::new("escape", Scope::Editor, "Clear selection", ed::Cancel),
        Entry::new("enter", Scope::Editor, "New line", ed::Newline),
        Entry::new("backspace", Scope::Editor, "Delete backwards", ed::Backspace),
        Entry::new("delete", Scope::Editor, "Delete forwards", ed::Delete),
        Entry::new("tab", Scope::Editor, "Indent / accept", ed::Tab),
        Entry::new("up", Scope::Editor, "Move up", zed_actions::editor::MoveUp),
        Entry::new("down", Scope::Editor, "Move down", zed_actions::editor::MoveDown),
        Entry::new("left", Scope::Editor, "Move left", ed::MoveLeft),
        Entry::new("right", Scope::Editor, "Move right", ed::MoveRight),
        Entry::new("shift-up", Scope::Editor, "Select up", ed::SelectUp),
        Entry::new("shift-down", Scope::Editor, "Select down", ed::SelectDown),
        Entry::new("shift-left", Scope::Editor, "Select left", ed::SelectLeft),
        Entry::new("shift-right", Scope::Editor, "Select right", ed::SelectRight),
        Entry::new(
            "cmd-left",
            Scope::Editor,
            "Start of line",
            ed::MoveToBeginningOfLine {
                stop_at_soft_wraps: true,
                stop_at_indent: true,
            },
        ),
        Entry::new(
            "cmd-right",
            Scope::Editor,
            "End of line",
            ed::MoveToEndOfLine {
                stop_at_soft_wraps: true,
            },
        ),
        Entry::new("cmd-up", Scope::Editor, "Start of file", ed::MoveToBeginning),
        Entry::new("cmd-down", Scope::Editor, "End of file", ed::MoveToEnd),
        // Inline blame is on by default (current line, after the code); these
        // control it when it gets loud.
        Entry::new("cmd-alt-b", Scope::Editor, "Toggle inline git blame", ed::ToggleGitBlameInline),
        Entry::new("cmd-alt-g", Scope::Editor, "Toggle git blame gutter", ::git::Blame),

        // ---- Pickers ------------------------------------------------------
        // Without this, ⌘P opens the file finder and only ⌘P closes it again —
        // escape inside the picker's text box would hit the Editor binding.
        Entry::new("escape", Scope::Picker, "Close the file finder", menu::Cancel),
        Entry::new("enter", Scope::Picker, "Open selected file", menu::Confirm),
        Entry::new("up", Scope::Picker, "Previous match", menu::SelectPrevious),
        Entry::new("down", Scope::Picker, "Next match", menu::SelectNext),
    ]
}

/// Render a binding's keys the way macOS users read them: `cmd-shift-z`
/// becomes `⌘⇧Z`. Display-only — the gpui syntax stays the source of truth.
pub fn display_keys(keys: &str) -> String {
    let mut out = String::new();
    let mut key = keys;
    // Modifiers first, in the conventional macOS order.
    for (name, sym) in [("ctrl-", "⌃"), ("alt-", "⌥"), ("shift-", "⇧"), ("cmd-", "⌘")] {
        if keys.contains(name) {
            out.push_str(sym);
        }
    }
    while let Some(rest) = key
        .strip_prefix("cmd-")
        .or_else(|| key.strip_prefix("shift-"))
        .or_else(|| key.strip_prefix("alt-"))
        .or_else(|| key.strip_prefix("ctrl-"))
    {
        key = rest;
    }
    out.push_str(&match key {
        "escape" => "Esc".to_string(),
        "enter" => "↩".to_string(),
        "backspace" => "⌫".to_string(),
        "delete" => "⌦".to_string(),
        "tab" => "⇥".to_string(),
        "up" => "↑".to_string(),
        "down" => "↓".to_string(),
        "left" => "←".to_string(),
        "right" => "→".to_string(),
        k => k.to_uppercase(),
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // `entries()` constructs every KeyBinding, and KeyBinding::new parses the
    // keystroke string — so simply building the table validates every binding
    // headlessly. A typo like "cmd-shft-z" fails here, not at first keypress.
    #[test]
    fn every_entry_binds_and_is_described() {
        let all = entries();
        assert!(all.len() > 20);
        for e in &all {
            assert!(!e.what.is_empty(), "{} has no description", e.keys);
            assert!(e.binding.is_some(), "{} lost its binding", e.keys);
        }
    }

    #[test]
    fn keys_display_as_mac_conventions() {
        assert_eq!(display_keys("cmd-shift-z"), "⇧⌘Z");
        assert_eq!(display_keys("cmd-,"), "⌘,");
        assert_eq!(display_keys("escape"), "Esc");
        assert_eq!(display_keys("cmd-left"), "⌘←");
    }

    #[test]
    fn no_duplicate_keys_within_a_scope() {
        let all = entries();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert!(
                    !(a.keys == b.keys && a.scope == b.scope),
                    "duplicate binding: {} in the same scope",
                    a.keys
                );
            }
        }
    }
}
