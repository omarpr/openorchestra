# harness-app

The native GPUI desktop app for openOrchestra. This package hosts the Zed
workspace and editor, worktree terminals, agent chat, and diff review.

## Build and run

From the repository root, use the Makefile targets documented in
[`CONTRIBUTING.md`](../../CONTRIBUTING.md):

```sh
make setup   # install and verify build/runtime tools
make run     # build and launch the development app
make app     # build dist/openOrchestra.app
```

To run Cargo commands directly, work from this directory:

```sh
cargo run
cargo check
cargo test
```

The app uses the current directory as its initial project. To open a specific
worktree at launch, pass its path (`cargo run -- /path/to/worktree`) or set
`HARNESS_WORKTREE_ROOT`.

## Standalone Cargo package

This package is intentionally excluded from the root Cargo workspace because
its Zed dependencies create a large build graph. It has its own
`Cargo.lock`; commands run from the repository root do not build or test it.
Use `make build` or run Cargo from this directory when changing the app.

All Zed git dependencies in `Cargo.toml` must stay on the same pinned revision.
When upgrading Zed, update them together and review Zed's
`[patch.crates-io]` entries, which do not propagate from its workspace.

## Module map

- `src/workspace/remote.rs` — bridge between the workspace and the `oo-remote`
  crate: owns the remote server's lifecycle (start/stop off the main thread),
  the marked-tab list in `~/.openorchestra/remote.json`, live agent snapshots
  for the phone, and the Settings → Remote state (port, pairing codes, Add a
  phone panel, paired devices, keep-awake).

## Historical implementation notes

[`SPIKE_NOTES.md`](SPIKE_NOTES.md) records the early feasibility spikes. It is
historical context; some prototype details and limitations described there
were replaced as the app developed. For current build guidance, use the root
README and [`CONTRIBUTING.md`](../../CONTRIBUTING.md).
