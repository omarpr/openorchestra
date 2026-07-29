//! Rendering for the chrome view, grouped by screen region. Each submodule is
//! an `impl HarnessWorkspace` block (see `workspace.rs` for the split rationale
//! and the shared types/struct they render).

mod center;
mod chrome;
mod diff;
mod diff_edit;
mod new_task;
mod preflight;
mod settings;
/// `pub(crate)` for `SidebarSection` alone — its variant labels are the keys
/// `Prefs::collapsed_sections` persists, so the prefs tests pin them.
pub(crate) mod sidebar;
