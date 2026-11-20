//! Persisted preferences (`prefs.json`) and the repo lists, with the enums
//! they store.

use super::*;

/// User-interface preferences persisted across launches (§12 cross-cutting).
///
/// Deliberately separate from `registry.json`: that file is *project state* the
/// training loop reads (§6.4), and mixing window chrome into it would make a
/// theme toggle look like a change to harvestable data.
/// Where the review surface is drawn. `Dock` is the right-hand tools panel
/// (Files / Git / Review tabs); `Tab` moves Review into the center tab strip,
/// where it gets the whole window and the tools dock is not rendered at all.
///
/// One location, not two: a docked panel and a tab showing the same review
/// would each need their own scroll and selection, and the two would disagree
/// the moment either was touched.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DiffLocation {
    #[default]
    Dock,
    Tab,
}

/// Which surface the right-hand tools panel is showing.
///
/// Files and Git host Zed's `ProjectPanel` / `GitPanel` outside the editor
/// docks; Review is the harness diff surface. Persisted so relaunch restores
/// the last tool rather than always bouncing back to Review.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ToolTab {
    Files,
    Git,
    #[default]
    Review,
    Ports,
}

/// Sub-tab inside the Files tools surface: tree vs. project search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum FilesSubTab {
    #[default]
    Explorer,
    Search,
}

/// How a file's changes are laid out: one column with +/− markers, or old and
/// new side by side.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DiffView {
    #[default]
    Unified,
    Split,
}

impl DiffView {
    pub(crate) fn label(self) -> &'static str {
        match self {
            DiffView::Unified => "Unified",
            DiffView::Split => "Split",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct Prefs {
    pub(crate) dark: bool,
    /// Logical name of the notification sound (see `sounds`), never a
    /// filename — the pref outlives the platform that resolves it.
    #[serde(default = "default_notify_sound")]
    pub(crate) notify_sound: String,
    /// The requirements screen has been shown once. First launch shows it even
    /// on a fully-equipped machine, so the tools the app leans on are named
    /// before their absence has to be inferred from an empty screen.
    #[serde(default)]
    pub(crate) seen_preflight: bool,
    /// A zed theme name overriding the harness dark/light pair. `None` means
    /// the built-in pair selected by `dark`.
    #[serde(default)]
    pub(crate) zed_theme: Option<String>,
    /// Font overrides; `None` means the resolved defaults.
    #[serde(default)]
    pub(crate) font_ui: Option<String>,
    #[serde(default)]
    pub(crate) font_mono: Option<String>,
    /// Per-agent settings, keyed by `AgentKind::as_str()`. Missing entry =
    /// enabled, default command.
    #[serde(default)]
    pub(crate) agents: std::collections::BTreeMap<String, AgentPref>,
    /// The user's agent order from Settings › Agents (drag to reorder), as
    /// `AgentKind::as_str()` keys. Empty = the built-in order. Keys missing
    /// from it (a new built-in, a new custom agent) follow the saved ones.
    #[serde(default)]
    pub(crate) agent_order: Vec<String>,
    /// The agent the user starred in Settings › Agents (an
    /// `AgentKind::as_str()` key). Wins over the last-used agent while it
    /// names an agent that exists and is enabled; `None` = no star. Older
    /// prefs files load it as `None`, and older builds ignore the key.
    #[serde(default)]
    pub(crate) default_agent: Option<String>,
    /// Last agent chosen when starting agents for a new multi-member task.
    /// Used as the default in the start-agents picker when still enabled.
    #[serde(default)]
    pub(crate) last_task_agent: Option<String>,
    /// Editor + terminal font size in px. `None` = 13.
    #[serde(default)]
    pub(crate) font_size: Option<f32>,
    /// Keep the conductor on screen all the time, conducting while agents
    /// work and narrating what they're doing. On by default — including for
    /// prefs files written before it existed; Settings › General turns it off.
    #[serde(default = "default_true")]
    pub(crate) conductor_always_on: bool,
    /// Where he stands: distance from the window's right and bottom edges.
    /// `None` = the default corner.
    #[serde(default)]
    pub(crate) conductor_offset: Option<[f32; 2]>,
    /// Show gitignored files in the Files tree and in the ⌘P file finder.
    /// On by default: agents routinely write build output, generated code and
    /// local env files that are ignored, and a tree that hides them makes the
    /// files an agent just mentioned look missing.
    #[serde(default = "default_true")]
    pub(crate) show_ignored_files: bool,
    /// Shortcut rebindings, keyed by `shortcuts::key_id`. Value is the gpui
    /// chord string; missing = the catalog default.
    #[serde(default)]
    pub(crate) shortcuts: std::collections::BTreeMap<String, String>,
    /// Projects whose sidebar group is collapsed, keyed by project name.
    ///
    /// A **set**, not a map of name→bool: only collapsed projects are recorded,
    /// so expanding one removes its entry rather than persisting `false`.
    /// Otherwise the file accumulates an entry for every project ever touched,
    /// and "expanded" — the default — would be stored explicitly for all of
    /// them. Absent means expanded, which is also what a fresh install gets.
    #[serde(default)]
    pub(crate) collapsed: std::collections::BTreeSet<String>,
    /// Sidebar sections whose body is collapsed, keyed by header label
    /// ("STATS", "TASKS", "PROJECTS"). Set-not-map for the same reason as
    /// [`Prefs::collapsed`]: absent means expanded, which is the default.
    #[serde(default)]
    pub(crate) collapsed_sections: std::collections::BTreeSet<String>,
    /// What the diff panel compares against. Absent in older prefs files, which
    /// is why it defaults rather than migrating: they land on `Uncommitted`,
    /// the new default, exactly like a fresh install.
    #[serde(default)]
    pub(crate) diff_base: DiffBase,
    pub(crate) review_overview: crate::review_overview::ReviewPreferences,
    pub(crate) sidebar_width: f32,
    /// Width of the right-hand tools panel (Files / Git / Review).
    /// Kept as `diff_width` in prefs.json so existing installs don't reset.
    pub(crate) diff_width: f32,
    /// Where the review surface lives (tools dock vs. center tab).
    #[serde(default)]
    pub(crate) diff_location: DiffLocation,
    /// Active tab inside the tools panel. Absent in older prefs → Review.
    #[serde(default)]
    pub(crate) tool_tab: ToolTab,
    /// Row order in the Ports tool tab. Absent in older prefs → port ascending.
    #[serde(default)]
    pub(crate) ports_sort: crate::ports::PortSort,
    /// Unified or side-by-side bodies. Absent in older prefs files, which land
    /// on `Unified` — what they were already showing.
    #[serde(default)]
    pub(crate) diff_view: DiffView,
    /// Width of the file-list column in the two-pane diff layout.
    #[serde(default = "default_diff_list_width")]
    pub(crate) diff_list_width: f32,
    pub(crate) sidebar_open: bool,
    /// Whether the right-hand tools panel is visible (`diff_open` in prefs.json).
    pub(crate) diff_open: bool,
    /// Remembered answer to the "keep or close terminals on quit?" prompt.
    /// `None` = ask every time; `Some(true)` = always kill the tmux sessions;
    /// `Some(false)` = always leave them running (detached).
    #[serde(default)]
    pub(crate) close_terminals_on_exit: Option<bool>,
    /// Open local/dev links in an in-app browser tab instead of the system browser.
    #[serde(default = "default_true")]
    pub(crate) links_in_browser_tab: bool,
    /// Where browser tabs open their Web Inspector: docked at the bottom,
    /// left or right of the page, or in its own window; the user's last
    /// move from the inspector's toolbar. Absent in older prefs → bottom;
    /// older builds ignore it.
    #[serde(default, deserialize_with = "crate::browser_pane::InspectorPlacement::deserialize_lenient")]
    pub(crate) inspector_placement: crate::browser_pane::InspectorPlacement,
}

pub(crate) fn default_true() -> bool {
    true
}

pub(crate) fn default_notify_sound() -> String {
    crate::sounds::DEFAULT.to_string()
}

pub(crate) fn default_diff_list_width() -> f32 {
    280.0
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            dark: true,
            notify_sound: default_notify_sound(),
            seen_preflight: false,
            zed_theme: None,
            font_ui: None,
            font_mono: None,
            agents: Default::default(),
            agent_order: Vec::new(),
            default_agent: None,
            last_task_agent: None,
            font_size: None,
            conductor_always_on: true,
            conductor_offset: None,
            show_ignored_files: true,
            shortcuts: Default::default(),
            collapsed: Default::default(),
            collapsed_sections: Default::default(),
            diff_base: DiffBase::default(),
            review_overview: Default::default(),
            sidebar_width: 280.0,
            diff_width: 380.0,
            diff_location: DiffLocation::Dock,
            tool_tab: ToolTab::default(),
            ports_sort: Default::default(),
            diff_view: DiffView::default(),
            diff_list_width: 280.0,
            sidebar_open: true,
            diff_open: true,
            close_terminals_on_exit: None,
            links_in_browser_tab: true,
            inspector_placement: Default::default(),
        }
    }
}

impl Prefs {
    /// Clamp on load, not just on write: a hand-edited or truncated prefs file
    /// must not be able to render a panel unusable or invisible.
    pub(crate) fn sanitized(mut self) -> Self {
        self.sidebar_width = self.sidebar_width.clamp(180.0, 640.0);
        // No small fixed ceiling here (it used to be 800): on a large display
        // that is exactly the cap issue #8 hits, and a diff you cannot widen
        // past 800 px is unreadable side-by-side. The ceiling that matters —
        // "leave the center pane usable" — depends on the window, so it lives
        // in `drag_move` where the viewport is known. What is left here is
        // sanity for a hand-edited or cross-monitor prefs file.
        self.diff_width = self.diff_width.clamp(220.0, MAX_PANEL_W);
        self.diff_list_width = self.diff_list_width.clamp(160.0, 560.0);
        self
    }
}

/// Whether the last session used the dark palette.
///
/// Read before the window exists, so `main` can install the right zed theme up
/// front rather than flashing dark and correcting.
/// The persisted shortcut rebindings, for `main` to bind before any window
/// exists. Same file the settings screen writes.
pub fn persisted_shortcut_overrides() -> std::collections::BTreeMap<String, String> {
    let path = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".openorchestra")
        .join("prefs.json");
    load_prefs(&path).shortcuts
}

pub fn persisted_dark_theme() -> bool {
    let path = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".openorchestra")
        .join("prefs.json");
    load_prefs(&path).dark
}

pub(crate) fn load_prefs(path: &Path) -> Prefs {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Prefs>(&s).ok())
        .unwrap_or_default()
        .sanitized()
}

pub(crate) fn save_prefs_to(path: &Path, prefs: &Prefs) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(prefs) {
        // Private and atomic: prefs carry the agent launch overrides.
        if let Err(e) = crate::state_files::write_private(path, json.as_bytes()) {
            log::warn!("could not save prefs to {}: {e}", path.display());
        }
    }
}

pub(crate) fn load_repos(path: &Path) -> Vec<PathBuf> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub(crate) fn save_repos(path: &Path, repos: &[PathBuf]) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(repos)
        && let Err(e) = crate::state_files::write_private(path, json.as_bytes())
    {
        log::warn!("could not save {}: {e}", path.display());
    }
}

/// Move `root` out of `from` and into `to` (append), returning whether it was
/// present. Used for archive (active → archived) and restore (archived →
/// active); idempotent on `to` so a double-restore can't duplicate an entry.
pub(crate) fn relocate_root(from: &mut Vec<PathBuf>, to: &mut Vec<PathBuf>, root: &Path) -> bool {
    let Some(ix) = from.iter().position(|p| p == root) else {
        return false;
    };
    let moved = from.remove(ix);
    if !to.contains(&moved) {
        to.push(moved);
    }
    true
}

#[cfg(test)]
mod collapsed_prefs_tests {
    use super::{DiffBase, Prefs, load_prefs, save_prefs_to};

    #[test]
    fn a_collapsed_project_survives_a_disk_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");

        let mut prefs = Prefs::default();
        prefs.collapsed.insert("acme".to_string());
        save_prefs_to(&path, &prefs);

        let back = load_prefs(&path);
        assert!(
            back.collapsed.contains("acme"),
            "collapsed project persists"
        );
        assert!(
            !back.collapsed.contains("other"),
            "an untouched project stays expanded"
        );
    }

    #[test]
    fn expanding_removes_the_entry_rather_than_storing_false() {
        // The whole point of the set: expanding must SHRINK the file, so it
        // does not accumulate an entry per project ever touched.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");

        let mut prefs = Prefs::default();
        prefs.collapsed.insert("acme".to_string());
        save_prefs_to(&path, &prefs);

        prefs.collapsed.remove("acme");
        save_prefs_to(&path, &prefs);

        assert!(load_prefs(&path).collapsed.is_empty());
    }

    #[test]
    fn a_prefs_file_written_before_this_feature_still_loads() {
        // `#[serde(default)]` is what makes the upgrade non-breaking; without
        // it every existing install would fail to parse and silently reset to
        // defaults, losing theme, fonts, and shortcut rebindings.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        std::fs::write(&path, r#"{"dark":false,"sidebar_width":300.0,"diff_width":340.0,"sidebar_open":true,"diff_open":true}"#).unwrap();

        let back = load_prefs(&path);
        assert!(back.collapsed.is_empty(), "absent = nothing collapsed");
        assert!(
            back.collapsed_sections.is_empty(),
            "absent = every sidebar section expanded"
        );
        assert!(!back.dark, "the rest of the file still parses");
    }

    #[test]
    fn the_diff_base_maps_to_the_ref_the_panel_measures_from() {
        use crate::workspace::state::diff_base_ref;
        // `HEAD` is what collapses the merge-base to a working-tree diff — the
        // path the main entry has always taken.
        assert_eq!(
            diff_base_ref(DiffBase::Uncommitted, "origin/master"),
            "HEAD"
        );
        assert_eq!(
            diff_base_ref(DiffBase::DefaultBranch, "origin/master"),
            "origin/master"
        );
        // A compare ref that is already HEAD (the main entry) is unchanged by
        // either setting, so main keeps behaving as it always has.
        assert_eq!(diff_base_ref(DiffBase::Uncommitted, "HEAD"), "HEAD");
        assert_eq!(diff_base_ref(DiffBase::DefaultBranch, "HEAD"), "HEAD");
    }

    #[test]
    fn the_diff_base_label_names_the_real_ref() {
        // Not a hard-coded "main": a repo with no remote must not be told it is
        // comparing against origin.
        assert_eq!(DiffBase::Uncommitted.label("origin/master"), "Uncommitted");
        assert_eq!(
            DiffBase::DefaultBranch.label("origin/master"),
            "vs origin/master"
        );
        assert_eq!(DiffBase::DefaultBranch.label("master"), "vs master");
    }

    #[test]
    fn a_prefs_file_without_a_diff_base_lands_on_uncommitted() {
        // The new default reaches existing installs the same way a fresh one
        // gets it, with no migration step.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        std::fs::write(&path, r#"{"dark":true}"#).unwrap();
        assert_eq!(load_prefs(&path).diff_base, DiffBase::Uncommitted);

        let prefs = Prefs {
            diff_base: DiffBase::DefaultBranch,
            ..Default::default()
        };
        save_prefs_to(&path, &prefs);
        assert_eq!(
            load_prefs(&path).diff_base,
            DiffBase::DefaultBranch,
            "and a chosen base survives a restart"
        );
    }

    #[test]
    fn a_collapsed_sidebar_section_survives_a_disk_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");

        let mut prefs = Prefs::default();
        prefs.collapsed_sections.insert("STATS".to_string());
        save_prefs_to(&path, &prefs);

        let back = load_prefs(&path);
        assert!(back.collapsed_sections.contains("STATS"));
        assert!(
            !back.collapsed_sections.contains("TASKS"),
            "an untouched section stays expanded"
        );

        // Same set-not-map discipline as projects: expanding shrinks the file.
        prefs.collapsed_sections.remove("STATS");
        save_prefs_to(&path, &prefs);
        assert!(load_prefs(&path).collapsed_sections.is_empty());
    }

    #[test]
    fn section_labels_are_the_persistence_keys() {
        // section_header writes SidebarSection::label() into the prefs set, so
        // renaming a header silently orphans everyone's saved collapse state.
        // This pins the three keys the file format depends on.
        use crate::workspace::ui::sidebar::SidebarSection::*;
        assert_eq!(
            [
                Stats.label(),
                Tasks.label(),
                Projects.label(),
                Archived.label()
            ],
            ["STATS", "TASKS", "PROJECTS", "ARCHIVED"]
        );
        // Only ARCHIVED starts collapsed; for the rest a stored key still means
        // "collapsed", so existing prefs files keep their meaning.
        assert!(Archived.collapsed_by_default());
        assert!(
            ![Stats, Tasks, Projects]
                .iter()
                .any(|s| s.collapsed_by_default())
        );
    }

    #[test]
    fn ports_sections_share_the_sidebar_header_keys() {
        // The Ports tab's two lists use the same collapsible header as the
        // sidebar, so their collapse state persists the same way.
        use crate::workspace::ui::sidebar::SidebarSection::*;
        assert_eq!(
            [PortsTask.label(), PortsOther.label()],
            ["THIS TASK", "OTHER ON THIS MAC"]
        );
        assert!(
            PortsOther.collapsed_by_default(),
            "the rest of the Mac starts folded away"
        );
        assert!(!PortsTask.collapsed_by_default());
    }
}

#[cfg(test)]
mod links_pref_tests {
    use super::Prefs;
    use crate::browser_pane::InspectorPlacement;

    #[test]
    fn the_inspector_placement_is_remembered_and_old_files_get_bottom() {
        assert_eq!(Prefs::default().inspector_placement, InspectorPlacement::Bottom);
        // A prefs.json written before the setting existed.
        let mut old = serde_json::to_value(Prefs::default()).unwrap();
        old.as_object_mut().unwrap().remove("inspector_placement");
        let p: Prefs = serde_json::from_value(old).unwrap();
        assert_eq!(p.inspector_placement, InspectorPlacement::Bottom);
        // Each placement round-trips under its lowercase name.
        for (placement, name) in [
            (InspectorPlacement::Bottom, "bottom"),
            (InspectorPlacement::Left, "left"),
            (InspectorPlacement::Right, "right"),
            (InspectorPlacement::Window, "window"),
        ] {
            let prefs = Prefs {
                inspector_placement: placement,
                ..Prefs::default()
            };
            let json = serde_json::to_value(&prefs).unwrap();
            assert_eq!(json["inspector_placement"], name);
            let back: Prefs = serde_json::from_value(json).unwrap();
            assert_eq!(back.inspector_placement, placement);
        }
        // One this build doesn't know (a newer build's) is bottom, and the
        // rest of the file still loads.
        let mut newer = serde_json::to_value(Prefs::default()).unwrap();
        newer["inspector_placement"] = serde_json::json!("floating");
        newer["sidebar_width"] = serde_json::json!(300.0);
        let p: Prefs = serde_json::from_value(newer).unwrap();
        assert_eq!(p.inspector_placement, InspectorPlacement::Bottom);
        assert_eq!(p.sidebar_width, 300.0);
    }

    #[test]
    fn prefs_default_routes_links_and_old_files_get_the_default() {
        assert!(Prefs::default().links_in_browser_tab);
        // A prefs.json written before the setting existed.
        let mut old = serde_json::to_value(Prefs::default()).unwrap();
        old.as_object_mut().unwrap().remove("links_in_browser_tab");
        let p: Prefs = serde_json::from_value(old).unwrap();
        assert!(p.links_in_browser_tab);
    }
}
