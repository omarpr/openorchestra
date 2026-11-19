//! End-to-end demo of the Spike 2 status pipeline, without the UI:
//!
//! ```text
//! cargo run -p status-watch --example demo -- /path/to/worktree [more...]
//! ```
//!
//! Then, in the watched worktree, either run pi with the harness-status
//! extension (`pi -e pi-ext/harness-status.ts`) or fake a write:
//!
//! ```text
//! mkdir -p .harness
//! printf '{"state":"running","timestamp":"2026-07-19T12:00:00Z","sessionFile":null}' > .harness/s.tmp
//! mv .harness/s.tmp .harness/status.json
//! ```
//!
//! Output lines look like `[repo/wt-nav] agent: running`.

use status_watch::StatusWatcher;
use std::path::Path;

fn label(root: &Path) -> String {
    // Last two path components, e.g. ".../repo/wt-nav" -> "repo/wt-nav".
    let mut parts = root
        .components()
        .rev()
        .take(2)
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    parts.reverse();
    parts.join("/")
}

fn main() {
    let roots: Vec<String> = std::env::args().skip(1).collect();
    if roots.is_empty() {
        eprintln!("usage: demo <worktree-root> [<worktree-root>...]");
        std::process::exit(2);
    }

    let (mut watcher, rx) = StatusWatcher::new().expect("failed to create watcher");
    for root in &roots {
        match watcher.watch(root) {
            Ok(()) => eprintln!("watching {root}"),
            Err(e) => {
                eprintln!("cannot watch {root}: {e}");
                std::process::exit(1);
            }
        }
    }

    for event in rx {
        println!(
            "[{}] agent: {}",
            label(&event.worktree_root),
            event.status.state
        );
    }
}
