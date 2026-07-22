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
    /// Only FULL editors (buffers), not single-line inputs. zed's own keymap
    /// makes this split: enter must insert a newline in a buffer but must
    /// CONFIRM in a search box or picker — a Newline bound to all Editors
    /// swallows enter in every single-line input, which broke search submit.
    EditorFull,
    /// Inside a modal picker (file finder). zed's own context name.
    Picker,
}

impl Scope {
    fn context(self) -> Option<&'static str> {
        match self {
            // "Harness" is the app root's key context, present in every focus
            // path — but at the SHALLOWEST depth. Truly context-less bindings
            // get maximum depth in gpui and would beat every scoped binding
            // (that is how the global ⌘W swallowed the editor's close-file),
            // so "global" here means "the root handles it unless someone
            // deeper claims the chord".
            Scope::Global => Some("Harness"),
            Scope::Workspace => Some("Workspace"),
            Scope::Editor => Some("Editor"),
            Scope::EditorFull => Some("Editor && mode == full"),
            // "Picker > Editor", zed's own predicate: the picker's query box
            // IS an editor, so a plain "Picker" context matches at the picker
            // node — SHALLOWER than the query editor — and the Editor-scoped
            // enter→Newline wins. This predicate matches at the editor node
            // itself (deepest), and catalog order breaks the tie our way.
            Scope::Picker => Some("Picker > Editor"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Scope::Global => "Global",
            Scope::Workspace => "Editor tab",
            Scope::Editor => "Text editing",
            Scope::EditorFull => "Text editing (buffers)",
            Scope::Picker => "File finder",
        }
    }
}

pub struct Entry {
    /// gpui keystroke syntax (`cmd-shift-z`). Also parsed for display.
    pub keys: String,
    pub scope: Scope,
    pub what: &'static str,
    /// `None` for shortcuts that reach the user another way (documented-only
    /// entries would go here); everything below binds.
    pub binding: Option<KeyBinding>,
}

impl Entry {
    fn new(
        keys: String,
        scope: Scope,
        what: &'static str,
        action: impl gpui::Action,
    ) -> Self {
        Entry {
            binding: Some(KeyBinding::new(&keys, action, scope.context())),
            keys,
            scope,
            what,
        }
    }
}

/// The stable identity a rebinding is stored under. Scope + description,
/// because the same chord legitimately exists in several scopes (⌘C).
pub fn key_id(scope: Scope, what: &str) -> String {
    format!("{}/{}", scope.label(), what)
}

/// The catalog with the user's rebindings applied. An override replaces the
/// chord, never the action or scope — the settings screen can move a command
/// to a new key but cannot detach the key from what it does.
pub fn entries_with(overrides: &std::collections::BTreeMap<String, String>) -> Vec<Entry> {
    let mut all = entries();
    for e in &mut all {
        if let Some(keys) = overrides.get(&key_id(e.scope, e.what)) {
            // Re-parse under the new chord. An invalid stored chord falls
            // back to the default rather than panicking at startup.
            if let Some(binding) = e.binding.take() {
                let predicate = e.scope.context().map(|c| {
                    std::rc::Rc::new(
                        gpui::KeyBindingContextPredicate::parse(c).expect("catalog context parses"),
                    )
                });
                match KeyBinding::load(
                    keys,
                    binding.action().boxed_clone(),
                    predicate,
                    false,
                    None,
                    &gpui::DummyKeyboardMapper,
                ) {
                    Ok(b) => {
                        e.keys = keys.clone();
                        e.binding = Some(b);
                    }
                    // An invalid stored chord keeps the default binding.
                    Err(_) => e.binding = Some(binding),
                }
            }
        }
    }
    all
}

/// The default catalog. Order within a scope is display order.
pub fn entries() -> Vec<Entry> {
    use editor::actions as ed;

    fn s(v: &str) -> String {
        v.to_string()
    }

    vec![
        // ---- Global ------------------------------------------------------
        Entry::new(s("cmd-t"), Scope::Global, "New terminal tab", crate::NewTerminal),
        Entry::new(s("cmd-e"), Scope::Global, "New editor tab", crate::NewEditor),
        Entry::new(s("cmd-shift-c"), Scope::Global, "New agent chat tab", crate::NewChat),
        Entry::new(
            s("cmd-c"),
            Scope::Global,
            "Copy selected diff lines (in the diff panel)",
            crate::CopyDiffSelection,
        ),
        Entry::new(s("cmd-,"), Scope::Global, "Open settings", crate::OpenSettings),
        // ⌘W layers by context depth: this global binding closes the harness
        // tab (terminal, agent, chat); the Workspace-scoped one below closes
        // the active FILE when focus is inside an editor tab, which is what
        // ⌘W means in an editor.
        Entry::new(s("cmd-w"), Scope::Global, "Close tab", crate::CloseTab),
        Entry::new(s("cmd-q"), Scope::Global, "Quit", crate::Quit),
        // The root-level enter: submits search, confirms menus — anything
        // deeper (a full buffer's Newline, a picker's Confirm) outranks it.
        Entry::new(s("enter"), Scope::Global, "Confirm / submit search", menu::Confirm),

        // ---- Editor tab (hosted zed workspace) ---------------------------
        Entry::new(
            s("cmd-p"),
            Scope::Workspace,
            "Find file by name",
            ::workspace::ToggleFileFinder::default(),
        ),
        Entry::new(
            s("cmd-shift-f"),
            Scope::Workspace,
            "Find in files (project search)",
            ::workspace::pane::DeploySearch::default(),
        ),
        Entry::new(
            s("cmd-shift-e"),
            Scope::Workspace,
            "Focus the file tree",
            zed_actions::project_panel::ToggleFocus,
        ),
        Entry::new(
            s("cmd-s"),
            Scope::Workspace,
            "Save file",
            ::workspace::Save { save_intent: None },
        ),
        Entry::new(
            s("cmd-w"),
            Scope::Workspace,
            "Close file",
            ::workspace::pane::CloseActiveItem::default(),
        ),

        // ---- Text editing -------------------------------------------------
        // ⌘C here deliberately shadows the global diff-copy: context depth
        // wins, so inside a buffer ⌘C copies editor text.
        Entry::new(s("cmd-c"), Scope::Editor, "Copy", ed::Copy),
        Entry::new(s("cmd-x"), Scope::Editor, "Cut", ed::Cut),
        Entry::new(s("cmd-v"), Scope::Editor, "Paste", ed::Paste),
        Entry::new(s("cmd-z"), Scope::Editor, "Undo", ed::Undo),
        Entry::new(s("cmd-shift-z"), Scope::Editor, "Redo", ed::Redo),
        Entry::new(s("cmd-a"), Scope::Editor, "Select all", ed::SelectAll),
        Entry::new(s("escape"), Scope::Editor, "Clear selection", ed::Cancel),
        Entry::new(s("enter"), Scope::EditorFull, "New line", ed::Newline),
        Entry::new(s("backspace"), Scope::Editor, "Delete backwards", ed::Backspace),
        Entry::new(s("delete"), Scope::Editor, "Delete forwards", ed::Delete),
        Entry::new(s("tab"), Scope::Editor, "Indent / accept", ed::Tab),
        Entry::new(s("up"), Scope::Editor, "Move up", zed_actions::editor::MoveUp),
        Entry::new(s("down"), Scope::Editor, "Move down", zed_actions::editor::MoveDown),
        Entry::new(s("left"), Scope::Editor, "Move left", ed::MoveLeft),
        Entry::new(s("right"), Scope::Editor, "Move right", ed::MoveRight),
        Entry::new(s("shift-up"), Scope::Editor, "Select up", ed::SelectUp),
        Entry::new(s("shift-down"), Scope::Editor, "Select down", ed::SelectDown),
        Entry::new(s("shift-left"), Scope::Editor, "Select left", ed::SelectLeft),
        Entry::new(s("shift-right"), Scope::Editor, "Select right", ed::SelectRight),
        Entry::new(
            s("cmd-left"),
            Scope::Editor,
            "Start of line",
            ed::MoveToBeginningOfLine {
                stop_at_soft_wraps: true,
                stop_at_indent: true,
            },
        ),
        Entry::new(
            s("cmd-right"),
            Scope::Editor,
            "End of line",
            ed::MoveToEndOfLine {
                stop_at_soft_wraps: true,
            },
        ),
        Entry::new(s("cmd-up"), Scope::Editor, "Start of file", ed::MoveToBeginning),
        Entry::new(s("cmd-down"), Scope::Editor, "End of file", ed::MoveToEnd),
        // Inline blame is on by default (current line, after the code); these
        // control it when it gets loud.
        Entry::new(s("cmd-alt-b"), Scope::Editor, "Toggle inline git blame", ed::ToggleGitBlameInline),
        Entry::new(s("cmd-alt-g"), Scope::Editor, "Toggle git blame gutter", ::git::Blame),

        // ---- Pickers ------------------------------------------------------
        // Without this, ⌘P opens the file finder and only ⌘P closes it again —
        // escape inside the picker's text box would hit the Editor binding.
        Entry::new(s("escape"), Scope::Picker, "Close the file finder", menu::Cancel),
        Entry::new(s("enter"), Scope::Picker, "Open selected file", menu::Confirm),
        Entry::new(s("up"), Scope::Picker, "Previous match", menu::SelectPrevious),
        Entry::new(s("down"), Scope::Picker, "Next match", menu::SelectNext),
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
    fn overrides_rebind_and_bad_chords_fall_back() {
        let mut ov = std::collections::BTreeMap::new();
        ov.insert(key_id(Scope::Global, "New terminal tab"), "cmd-y".to_string());
        // Note: Keystroke::parse is lenient enough that almost any string
        // parses (words become key sequences), so the invalid-chord fallback
        // in entries_with is belt-and-braces, not a testable path.
        let all = entries_with(&ov);
        let term = all
            .iter()
            .find(|e| e.what == "New terminal tab")
            .unwrap();
        assert_eq!(term.keys, "cmd-y", "override must rebind");
        assert!(term.binding.is_some());
        let undo = all
            .iter()
            .find(|e| e.what == "Undo" && e.scope == Scope::Editor)
            .unwrap();
        assert_eq!(undo.keys, "cmd-z", "unrelated entries keep their defaults");
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
