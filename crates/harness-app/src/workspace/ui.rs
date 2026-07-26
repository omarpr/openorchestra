//! Rendering for the chrome view, grouped by screen region. Each submodule is
//! an `impl HarnessWorkspace` block (see `workspace.rs` for the split rationale
//! and the shared types/struct they render).

mod center;
mod chrome;
mod diff;
mod diff_edit;
mod preflight;
mod settings;
mod sidebar;
