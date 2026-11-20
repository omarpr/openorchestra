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
    /// Inside the full editor (IDE) tab only.
    Ide,
    /// When focus is in a text editor buffer.
    Editor,
    /// One of the chrome's inline single-line inputs (rename, new worktree,
    /// avatar initials, agent command, review comment). The wrapper carries a
    /// `HarnessInput` key context; this predicate matches AT the editor node
    /// (like `Picker > Editor`), so it can bind escape→Cancel over the
    /// Editor-scoped clear-selection escape.
    Input,
    /// Only FULL editors (buffers), not single-line inputs. zed's own keymap
    /// makes this split: enter must insert a newline in a buffer but must
    /// CONFIRM in a search box or picker — a Newline bound to all Editors
    /// swallows enter in every single-line input, which broke search submit.
    EditorFull,
    /// Inside a modal picker (file finder). zed's own context name.
    Picker,
    /// The in-buffer find bar (⌘F), at the bar itself. zed's own context name.
    SearchBar,
    /// The find bar's query box. Matches AT the editor node (like
    /// `Picker > Editor`) so escape/tab can beat the Editor-scoped bindings
    /// that would otherwise clear a selection or indent instead.
    SearchBarInput,
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
            Scope::Ide => Some("IdeWorkspace"),
            Scope::Editor => Some("Editor"),
            Scope::EditorFull => Some("Editor && mode == full"),
            // Matches at the editor node itself (deepest), same shape as
            // `Picker > Editor`; catalog order breaks the tie against the
            // plain `Editor` escape so Cancel wins in our inline inputs.
            Scope::Input => Some("HarnessInput > Editor"),
            // "Picker > Editor", zed's own predicate: the picker's query box
            // IS an editor, so a plain "Picker" context matches at the picker
            // node — SHALLOWER than the query editor — and the Editor-scoped
            // enter→Newline wins. This predicate matches at the editor node
            // itself (deepest), and catalog order breaks the tie our way.
            Scope::Picker => Some("Picker > Editor"),
            Scope::SearchBar => Some("BufferSearchBar"),
            // Same shape and same reason as `Picker > Editor`: the find bar's
            // query box IS an editor, so a bare "BufferSearchBar" matches at
            // the bar node — SHALLOWER than the query editor — and the
            // Editor-scoped escape/tab win. This matches at the editor node
            // itself, and catalog order breaks the tie our way.
            Scope::SearchBarInput => Some("BufferSearchBar > Editor"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Scope::Global => "Global",
            Scope::Workspace => "Files & search",
            Scope::Ide => "Editor tab (IDE)",
            Scope::Editor => "Text editing",
            Scope::EditorFull => "Text editing (buffers)",
            Scope::Input => "Inline input",
            Scope::Picker => "File finder",
            Scope::SearchBar | Scope::SearchBarInput => "Find in file",
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
        keys: &'static str,
        scope: Scope,
        what: &'static str,
        action: impl gpui::Action,
    ) -> Self {
        Entry {
            binding: Some(KeyBinding::new(keys, action, scope.context())),
            keys: keys.to_string(),
            scope,
            what,
        }
    }
}

/// The OS keystrokes are parsed for. `KeyBinding::new` only takes static
/// chords now; runtime strings (user rebindings) go through the OS-aware
/// parser, which is also what resolves zed's `secondary-` alias.
pub fn host_os() -> gpui::OperatingSystem {
    if cfg!(target_os = "macos") {
        gpui::OperatingSystem::Mac
    } else if cfg!(target_os = "windows") {
        gpui::OperatingSystem::Windows
    } else {
        gpui::OperatingSystem::Linux
    }
}

/// Whether every keystroke in a (space-separated) chord parses.
fn chord_parses(keys: &str) -> bool {
    !keys.trim().is_empty()
        && keys
            .split_whitespace()
            .all(|k| gpui::Keystroke::parse_with_os(k, host_os()).is_ok())
}

/// The stable identity a rebinding is stored under. Scope + description,
/// because the same chord legitimately exists in several scopes (⌘C).
pub fn key_id(scope: Scope, what: &str) -> String {
    format!("{}/{}", scope.label(), what)
}

/// The catalog with the user's rebindings applied to what it *shows*: an
/// override replaces the chord, never the action or scope — the settings
/// screen can move a command to a new key but cannot detach the key from what
/// it does. A stored chord that does not parse is ignored, so the default
/// stays both shown and bound. Bindings themselves come from
/// [`bindings_with`], which needs the app (zed resolves chords against the
/// live keyboard layout).
pub fn entries_with(overrides: &std::collections::BTreeMap<String, String>) -> Vec<Entry> {
    let mut all = entries();
    for e in &mut all {
        if let Some(keys) = overrides.get(&key_id(e.scope, e.what))
            && chord_parses(keys)
        {
            e.keys = keys.clone();
        }
    }
    all
}

/// The key bindings to install: each entry's default binding, re-made under
/// the user's chord where one is set and valid.
pub fn bindings_with(
    overrides: &std::collections::BTreeMap<String, String>,
    cx: &gpui::App,
) -> Vec<KeyBinding> {
    let mut out = Vec::new();
    for e in entries() {
        let Some(binding) = e.binding else { continue };
        let rebound = overrides
            .get(&key_id(e.scope, e.what))
            .filter(|k| chord_parses(k))
            .and_then(|keys| {
                let predicate = e.scope.context().map(|c| {
                    std::rc::Rc::new(
                        gpui::KeyBindingContextPredicate::parse(c).expect("catalog context parses"),
                    )
                });
                KeyBinding::load(
                    keys,
                    binding.action().boxed_clone(),
                    predicate,
                    false,
                    None,
                    cx,
                )
                .ok()
            });
        // An invalid stored chord keeps the default binding.
        out.push(rebound.unwrap_or(binding));
    }
    out
}

/// One keystroke as the page/app split compares them: the key and the four
/// modifiers a chord is written with (fn is ignored; macOS sets it on arrows).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chord {
    pub key: String,
    pub cmd: bool,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl Chord {
    pub fn new(key: &str, cmd: bool, shift: bool, alt: bool, ctrl: bool) -> Self {
        Chord {
            key: key.to_string(),
            cmd,
            shift,
            alt,
            ctrl,
        }
    }

    /// A catalog chord (`cmd-shift-]`). For a sequence (`cmd-k cmd-t`) only
    /// its first keystroke, which is what a page would otherwise receive.
    pub fn parse(keys: &str) -> Option<Self> {
        let first = keys.split_whitespace().next()?;
        let k = gpui::Keystroke::parse_with_os(first, host_os()).ok()?;
        Some(Chord::new(
            &k.key,
            k.modifiers.platform,
            k.modifiers.shift,
            k.modifiers.alt,
            k.modifiers.control,
        ))
    }
}

/// The editing keys a focused web page keeps, and the AppKit action each
/// becomes when the page's own script leaves it alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageEdit {
    Copy,
    Paste,
    Cut,
    SelectAll,
    Undo,
    Redo,
}

/// ⌘C/⌘V/⌘X/⌘A/⌘Z/⌘⇧Z: the page's, whatever the catalog binds them to
/// (the global ⌘C copies diff lines; over a page it must copy page text).
pub fn page_edit(chord: &Chord) -> Option<PageEdit> {
    if !chord.cmd || chord.alt || chord.ctrl {
        return None;
    }
    match (chord.key.as_str(), chord.shift) {
        ("c", false) => Some(PageEdit::Copy),
        ("v", false) => Some(PageEdit::Paste),
        ("x", false) => Some(PageEdit::Cut),
        ("a", false) => Some(PageEdit::SelectAll),
        ("z", false) => Some(PageEdit::Undo),
        ("z", true) => Some(PageEdit::Redo),
        _ => None,
    }
}

/// The chords the app keeps while a web page has keyboard focus: every
/// Global-scope ⌘-chord in `entries` (pass [`entries_with`], so rebindings
/// count), minus the page's editing keys. Other scopes (editor, IDE, find
/// bar, inputs) are never in a browser tab's focus path, and a chord with
/// no ⌘ (the global `enter`) is typing, which the page always gets.
pub fn app_chords(entries: &[Entry]) -> Vec<Chord> {
    entries
        .iter()
        .filter(|e| e.scope == Scope::Global)
        .filter_map(|e| Chord::parse(&e.keys))
        .filter(|c| c.cmd && page_edit(c).is_none())
        .collect()
}

/// Whether a ⌘-chord pressed over a focused page goes to the app (`true`)
/// or stays with the page. `app` comes from [`app_chords`].
pub fn is_app_shortcut(chord: &Chord, app: &[Chord]) -> bool {
    page_edit(chord).is_none() && app.contains(chord)
}

/// The default catalog. Order within a scope is display order.
pub fn entries() -> Vec<Entry> {
    use editor::actions as ed;

    fn s(v: &'static str) -> &'static str {
        v
    }

    vec![
        // ---- Global ------------------------------------------------------
        Entry::new(
            s("cmd-t"),
            Scope::Global,
            "New terminal tab",
            crate::NewTerminal,
        ),
        Entry::new(
            s("cmd-e"),
            Scope::Global,
            "Open the editor tab (IDE)",
            crate::NewEditor,
        ),
        Entry::new(
            s("cmd-shift-c"),
            Scope::Global,
            "New agent chat tab",
            crate::NewChat,
        ),
        Entry::new(
            s("cmd-c"),
            Scope::Global,
            "Copy selected diff lines (in the diff panel)",
            crate::CopyDiffSelection,
        ),
        Entry::new(
            s("cmd-,"),
            Scope::Global,
            "Open settings",
            crate::OpenSettings,
        ),
        // ⌘W layers by context depth: this global binding closes the harness
        // tab (terminal, agent, chat); the Workspace-scoped one below closes
        // the active FILE when focus is inside an editor tab, which is what
        // ⌘W means in an editor.
        Entry::new(s("cmd-w"), Scope::Global, "Close tab", crate::CloseTab),
        Entry::new(s("cmd-q"), Scope::Global, "Quit", crate::Quit),
        Entry::new(s("cmd-n"), Scope::Global, "New task", crate::NewTask),
        Entry::new(
            s("cmd-alt-down"),
            Scope::Global,
            "Next worktree",
            crate::NextWorktree,
        ),
        Entry::new(
            s("cmd-alt-up"),
            Scope::Global,
            "Previous worktree",
            crate::PreviousWorktree,
        ),
        Entry::new(
            s("cmd-shift-a"),
            Scope::Global,
            "Jump to the next agent waiting on you",
            crate::NextWaitingAgent,
        ),
        Entry::new(s("cmd-shift-]"), Scope::Global, "Next tab", crate::NextTab),
        Entry::new(
            s("cmd-shift-["),
            Scope::Global,
            "Previous tab",
            crate::PreviousTab,
        ),
        Entry::new(s("cmd-1"), Scope::Global, "Tab 1", crate::ActivateTab1),
        Entry::new(s("cmd-2"), Scope::Global, "Tab 2", crate::ActivateTab2),
        Entry::new(s("cmd-3"), Scope::Global, "Tab 3", crate::ActivateTab3),
        Entry::new(s("cmd-4"), Scope::Global, "Tab 4", crate::ActivateTab4),
        Entry::new(s("cmd-5"), Scope::Global, "Tab 5", crate::ActivateTab5),
        Entry::new(s("cmd-6"), Scope::Global, "Tab 6", crate::ActivateTab6),
        Entry::new(s("cmd-7"), Scope::Global, "Tab 7", crate::ActivateTab7),
        Entry::new(s("cmd-8"), Scope::Global, "Tab 8", crate::ActivateTab8),
        Entry::new(
            s("cmd-9"),
            Scope::Global,
            "Last tab",
            crate::ActivateLastTab,
        ),
        // ⌘B is the left panel everywhere: the sidebar here, and zed's own
        // file-tree dock inside the Editor tab (its Ide-scoped binding below
        // is deeper, so it wins there).
        Entry::new(
            s("cmd-b"),
            Scope::Global,
            "Toggle the sidebar",
            crate::ToggleSidebar,
        ),
        Entry::new(
            s("cmd-shift-b"),
            Scope::Global,
            "Toggle the tools panel",
            crate::ToggleTools,
        ),
        // The root-level enter: submits search, confirms menus — anything
        // deeper (a full buffer's Newline, a picker's Confirm) outranks it.
        Entry::new(
            s("enter"),
            Scope::Global,
            "Confirm / submit search",
            menu::Confirm,
        ),
        // ---- Hosted workspace (file tabs / ⌘P / search) ------------------
        // Global, not Workspace: ⌘P has to work from a terminal or Review
        // too, where no zed "Workspace" key context is in the focus path. The
        // root attaches the hosted workspace's actions, so this still reaches
        // zed's own file finder.
        Entry::new(
            s("cmd-p"),
            Scope::Global,
            "Find file by name",
            ::workspace::ToggleFileFinder::default(),
        ),
        // Global so ⌘⇧F works from Review/Terminal/etc., not only when a
        // File tab holds Workspace focus. Opens Files → Search in the tools
        // panel (harness project search), not Zed's pane DeploySearch.
        Entry::new(
            s("cmd-shift-f"),
            Scope::Global,
            "Find in files (project search)",
            crate::FocusProjectSearch,
        ),
        Entry::new(
            s("cmd-shift-e"),
            Scope::Global,
            "Focus the file tree (tools panel)",
            crate::FocusFilesTool,
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
        // Native git/diff (git_ui): the project-diff view of all uncommitted
        // changes, opened as a pane item beside the editor.
        Entry::new(
            s("cmd-shift-d"),
            Scope::Workspace,
            "Show project diff (git)",
            git_ui::project_diff::Diff,
        ),
        // Tools panel Git tab (harness chrome), not Zed's dock ToggleFocus —
        // docks stay closed so the editor stays full-bleed.
        Entry::new(
            s("cmd-shift-g"),
            Scope::Global,
            "Toggle the git tools panel",
            crate::FocusGitTool,
        ),
        // ---- Editor tab (IDE): zed's own docks ---------------------------
        // Scoped to the IdeWorkspace context, which is deeper than the
        // harness root, so inside the Editor tab these chords drive *its*
        // docks while everywhere else they keep their tools-panel meaning.
        Entry::new(
            s("cmd-b"),
            Scope::Ide,
            "Toggle the file tree dock",
            ::workspace::ToggleLeftDock,
        ),
        Entry::new(
            s("cmd-shift-e"),
            Scope::Ide,
            "Focus the file tree",
            zed_actions::project_panel::ToggleFocus,
        ),
        Entry::new(
            s("cmd-shift-g"),
            Scope::Ide,
            "Toggle the git panel",
            zed_actions::git_panel::ToggleFocus,
        ),
        Entry::new(
            s("cmd-j"),
            Scope::Ide,
            "Toggle the bottom dock",
            ::workspace::ToggleBottomDock,
        ),
        Entry::new(
            s("cmd-shift-r"),
            Scope::Global,
            "Focus the review tools panel",
            crate::FocusReviewTool,
        ),
        // Native Markdown preview (markdown_preview): live, code-highlighted,
        // as an editor pane. Distinct from the diff panel's ◧ toggle.
        Entry::new(
            s("cmd-shift-m"),
            Scope::Workspace,
            "Preview markdown",
            zed_actions::preview::markdown::OpenPreview,
        ),
        Entry::new(
            s("cmd-alt-m"),
            Scope::Workspace,
            "Preview markdown to the side",
            zed_actions::preview::markdown::OpenPreviewToTheSide,
        ),
        // ---- Find in file (⌘F) ---------------------------------------------
        // The BufferSearchBar is already installed on every pane toolbar
        // (workspace_host.rs) and `search::init` runs in main.rs — until now
        // nothing was BOUND to it, which is why ⌘F did nothing with a file
        // open. Scoped to full buffers, not every Editor: our inline
        // single-line inputs (rename, agent command, review comment) are
        // editors too, and ⌘F must not deploy a find bar inside one.
        Entry::new(
            s("cmd-f"),
            Scope::EditorFull,
            "Find in this file",
            ::search::buffer_search::Deploy::find(),
        ),
        // Next-match from the buffer, so you can page through hits without
        // going back to the query box. zed puts previous-match on ⇧⌘G at
        // `Pane` scope; we do NOT, because `Pane` is deeper than `Workspace`
        // and would silently shadow our git-panel ⇧⌘G. ⇧enter in the find bar
        // is previous-match instead.
        Entry::new(
            s("cmd-g"),
            Scope::EditorFull,
            "Next match",
            ::search::SelectNextMatch,
        ),
        Entry::new(
            s("enter"),
            Scope::SearchBar,
            "Next match (find bar)",
            ::search::SelectNextMatch,
        ),
        Entry::new(
            s("shift-enter"),
            Scope::SearchBar,
            "Previous match (find bar)",
            ::search::SelectPreviousMatch,
        ),
        Entry::new(
            s("cmd-f"),
            Scope::SearchBar,
            "Back to the query box",
            ::search::FocusSearch,
        ),
        // escape/tab for the find bar live at the BOTTOM of the catalog, with
        // the other `> Editor` predicates — they tie with the Editor-scoped
        // escape and tab on depth, so only catalog order separates them.

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
        Entry::new(
            s("backspace"),
            Scope::Editor,
            "Delete backwards",
            ed::Backspace,
        ),
        Entry::new(s("delete"), Scope::Editor, "Delete forwards", ed::Delete),
        Entry::new(s("tab"), Scope::Editor, "Indent / accept", ed::Tab),
        Entry::new(
            s("up"),
            Scope::Editor,
            "Move up",
            zed_actions::editor::MoveUp,
        ),
        Entry::new(
            s("down"),
            Scope::Editor,
            "Move down",
            zed_actions::editor::MoveDown,
        ),
        Entry::new(s("left"), Scope::Editor, "Move left", ed::MoveLeft),
        Entry::new(s("right"), Scope::Editor, "Move right", ed::MoveRight),
        Entry::new(s("shift-up"), Scope::Editor, "Select up", ed::SelectUp),
        Entry::new(
            s("shift-down"),
            Scope::Editor,
            "Select down",
            ed::SelectDown,
        ),
        Entry::new(
            s("shift-left"),
            Scope::Editor,
            "Select left",
            ed::SelectLeft,
        ),
        Entry::new(
            s("shift-right"),
            Scope::Editor,
            "Select right",
            ed::SelectRight,
        ),
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
        Entry::new(
            s("cmd-up"),
            Scope::Editor,
            "Start of file",
            ed::MoveToBeginning,
        ),
        Entry::new(s("cmd-down"), Scope::Editor, "End of file", ed::MoveToEnd),
        // Inline blame is on by default (current line, after the code); these
        // control it when it gets loud.
        Entry::new(
            s("cmd-alt-b"),
            Scope::Editor,
            "Toggle inline git blame",
            ed::ToggleGitBlameInline,
        ),
        Entry::new(
            s("cmd-alt-g"),
            Scope::Editor,
            "Toggle git blame gutter",
            ::git::Blame,
        ),
        // ---- Inline inputs ------------------------------------------------
        // Rename / avatar / agent-command / review-comment / new-task dialog
        // fields are single-line editors. Enter reaches the global
        // menu::Confirm (a single-line editor has no Newline binding), but
        // escape would otherwise hit the Editor-scoped clear-selection; this
        // deeper predicate rebinds it to Cancel so escape abandons the edit.
        // Placed AFTER the Editor escape so catalog order gives it priority.
        Entry::new(s("escape"), Scope::Input, "Cancel the edit", menu::Cancel),
        // ---- Find bar (the `> Editor` half) ---------------------------------
        // The query box IS an editor, so these tie with the Editor-scoped
        // escape (clear selection) and tab (indent) on context depth. Without
        // them escape would clear a selection instead of closing the find bar,
        // and tab would indent the query instead of returning to the buffer.
        // Their position here — after the Editor entries — is what wins the tie.
        Entry::new(
            s("escape"),
            Scope::SearchBarInput,
            "Close the find bar",
            ::search::buffer_search::Dismiss,
        ),
        Entry::new(
            s("tab"),
            Scope::SearchBarInput,
            "Back to the buffer",
            ::search::buffer_search::FocusEditor,
        ),
        // ---- Pickers ------------------------------------------------------
        // Without this, ⌘P opens the file finder and only ⌘P closes it again —
        // escape inside the picker's text box would hit the Editor binding.
        Entry::new(
            s("escape"),
            Scope::Picker,
            "Close the file finder",
            menu::Cancel,
        ),
        Entry::new(
            s("enter"),
            Scope::Picker,
            "Open selected file",
            menu::Confirm,
        ),
        Entry::new(
            s("up"),
            Scope::Picker,
            "Previous match",
            menu::SelectPrevious,
        ),
        Entry::new(s("down"), Scope::Picker, "Next match", menu::SelectNext),
    ]
}

/// Render a binding's keys the way macOS users read them: `cmd-shift-z`
/// becomes `⌘⇧Z`. Display-only — the gpui syntax stays the source of truth.
pub fn display_keys(keys: &str) -> String {
    let mut out = String::new();
    let mut key = keys;
    // Modifiers first, in the conventional macOS order.
    for (name, sym) in [
        ("ctrl-", "⌃"),
        ("alt-", "⌥"),
        ("shift-", "⇧"),
        ("cmd-", "⌘"),
    ] {
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
        ov.insert(
            key_id(Scope::Global, "New terminal tab"),
            "cmd-y".to_string(),
        );
        // Note: Keystroke::parse is lenient enough that almost any string
        // parses (words become key sequences), so the invalid-chord fallback
        // in entries_with is belt-and-braces, not a testable path.
        let all = entries_with(&ov);
        let term = all.iter().find(|e| e.what == "New terminal tab").unwrap();
        assert_eq!(term.keys, "cmd-y", "override must rebind");
        assert!(term.binding.is_some());
        // A stored chord that does not parse is shown as the default.
        let mut bad = std::collections::BTreeMap::new();
        bad.insert(key_id(Scope::Global, "New terminal tab"), "   ".to_string());
        let term = entries_with(&bad)
            .into_iter()
            .find(|e| e.what == "New terminal tab")
            .unwrap();
        assert_eq!(term.keys, "cmd-t");
        let undo = all
            .iter()
            .find(|e| e.what == "Undo" && e.scope == Scope::Editor)
            .unwrap();
        assert_eq!(undo.keys, "cmd-z", "unrelated entries keep their defaults");
    }

    fn chord(keys: &str) -> Chord {
        Chord::parse(keys).unwrap_or_else(|| panic!("{keys} parses"))
    }

    #[test]
    fn a_focused_page_keeps_editing_keys_and_the_app_keeps_its_chords() {
        let app = app_chords(&entries());
        for keys in [
            "cmd-t", "cmd-e", "cmd-w", "cmd-shift-c", "cmd-,", "cmd-q", "cmd-n",
            "cmd-alt-down", "cmd-alt-up", "cmd-shift-a", "cmd-shift-]", "cmd-shift-[",
            "cmd-1", "cmd-5", "cmd-8", "cmd-9", "cmd-b", "cmd-shift-b", "cmd-p",
            "cmd-shift-f", "cmd-shift-e", "cmd-shift-g", "cmd-shift-r",
        ] {
            assert!(is_app_shortcut(&chord(keys), &app), "{keys} must reach the app");
        }
        for keys in ["cmd-c", "cmd-v", "cmd-x", "cmd-a", "cmd-z", "cmd-shift-z"] {
            assert!(!is_app_shortcut(&chord(keys), &app), "{keys} must stay with the page");
            assert!(page_edit(&chord(keys)).is_some(), "{keys} is a page edit");
        }
        // Not global (editor, IDE or find-bar scoped), or no ⌘ at all.
        for keys in ["cmd-s", "cmd-f", "cmd-g", "cmd-j", "cmd-left", "cmd-k", "enter", "escape"] {
            assert!(!is_app_shortcut(&chord(keys), &app), "{keys} must stay with the page");
        }
        assert_eq!(page_edit(&chord("cmd-c")), Some(PageEdit::Copy));
        assert_eq!(page_edit(&chord("cmd-shift-z")), Some(PageEdit::Redo));
        assert_eq!(page_edit(&chord("cmd-t")), None);
    }

    /// Drift guard: every global ⌘-chord in the catalog reaches the app over
    /// a page, except the editing keys, which the page owns.
    #[test]
    fn every_global_command_chord_is_an_app_shortcut() {
        let all = entries();
        let app = app_chords(&all);
        // Written out, not derived from the code under test.
        let page_owned = ["cmd-c", "cmd-v", "cmd-x", "cmd-a", "cmd-z", "cmd-shift-z"];
        let mut app_count = 0;
        for e in all.iter().filter(|e| e.scope == Scope::Global) {
            let expected = e.keys.starts_with("cmd-") && !page_owned.contains(&e.keys.as_str());
            app_count += usize::from(expected);
            assert_eq!(is_app_shortcut(&chord(&e.keys), &app), expected, "{} ({})", e.keys, e.what);
        }
        // Global entries are all ⌘-chords but `enter` and the diff ⌘C.
        assert_eq!(app_count, all.iter().filter(|e| e.scope == Scope::Global).count() - 2);
    }

    #[test]
    fn app_shortcuts_follow_rebindings() {
        let mut ov = std::collections::BTreeMap::new();
        ov.insert(key_id(Scope::Global, "New terminal tab"), "cmd-y".to_string());
        // A chord rebound onto an editing key still leaves it to the page.
        ov.insert(key_id(Scope::Global, "New task"), "cmd-v".to_string());
        let app = app_chords(&entries_with(&ov));
        assert!(is_app_shortcut(&chord("cmd-y"), &app));
        assert!(!is_app_shortcut(&chord("cmd-t"), &app));
        assert!(!is_app_shortcut(&chord("cmd-v"), &app));
        assert!(!is_app_shortcut(&chord("cmd-n"), &app));
    }

    #[test]
    fn chords_compare_modifiers_and_key_only() {
        assert_eq!(chord("cmd-shift-]"), Chord::new("]", true, true, false, false));
        assert_eq!(chord("cmd-alt-down"), Chord::new("down", true, false, true, false));
        assert_ne!(chord("cmd-t"), chord("cmd-shift-t"));
        // Only the first keystroke of a sequence counts.
        assert_eq!(chord("cmd-k cmd-t"), chord("cmd-k"));
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
