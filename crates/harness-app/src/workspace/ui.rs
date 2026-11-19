//! Rendering for the chrome view, grouped by screen region. Each submodule is
//! an `impl HarnessWorkspace` block (see `workspace.rs` for the split rationale
//! and the shared types/struct they render).

mod center;
mod chrome;
pub(crate) mod conductor;
mod dialogs;
mod diff;
mod diff_edit;
mod new_task;
pub(crate) mod ports_panel;
mod preflight;
mod project_search;
mod review_overview;
mod settings;
/// `pub(crate)` for `qr_cells` / `parse_port`, which `remote.rs` and the
/// port field's commit use.
pub(crate) mod settings_remote;
/// `pub(crate)` for `SidebarSection` alone — its variant labels are the keys
/// `Prefs::collapsed_sections` persists, so the prefs tests pin them.
pub(crate) mod sidebar;
mod start_specialists;
mod tool_panel;
