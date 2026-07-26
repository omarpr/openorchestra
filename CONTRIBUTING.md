# Contributing to openOrchestra

Thanks for your interest in openOrchestra. This project is **maintainer-led**
with a deliberately narrow scope. Before opening a pull request, please read this
guide, the [Code of Conduct](CODE_OF_CONDUCT.md), and
[`docs/architecture.md`](docs/architecture.md) — the architecture document is the
source of truth for what this app is and why it is built the way it is.

## Before you write code

- **Bugs:** open an issue, or send a small, obvious fix straight as a PR.
- **New features or non-trivial changes:** open an issue or discussion **before**
  writing code. PRs that skip this may be closed without review.
- **Out-of-scope proposals:** anything that pushes openOrchestra toward
  cross-platform support, a web UI, a plugin API, or replacing the embedded zed
  editor gets closed with a pointer to the architecture doc. Nothing personal,
  just scope discipline.

## Project invariants (non-negotiable)

A change that breaks one of these will not merge:

- **macOS-native only.** GPUI (zed's UI framework) for all UI. No Electron, no
  cross-platform layers, no web views.
- **A real embedded zed, not a re-implementation.** The editor is zed's own
  `Workspace` hosted as a child view. When something is missing, wire up zed's
  existing surface rather than reinventing it — see the pane-toolbar notes in
  `crates/harness-app/src/workspace_host.rs` for what "hosting" actually entails.
- **Backend crates stay UI-free.** `worktrees`, `forge`, `status-watch`,
  `agent-runtime`, `agent-schema`, `training` and `pi-client` are headless and
  fully tested without a window. The app crate renders; it does not own protocol
  or git logic.
- **Agent-agnostic.** pi, Claude Code, Codex, OpenCode and Kimi are peers. Don't
  privilege one in shared paths; agent-specific behavior belongs behind
  `AgentKind` or in that agent's own module.
- **Nothing leaves the machine.** No telemetry, no analytics, no uploading a
  user's code, diffs, prompts or credentials.
- **The UI must not lie.** A badge, a pill or a count that can go stale is worse
  than one that isn't shown — a stale number reads as a current one. If you add a
  surface that reflects external state, make it refresh, and say in a comment
  what refreshes it.

## Development setup

Requires macOS with a Rust toolchain. `make setup` installs and verifies the
rest; `make doctor` reports what is present.

```sh
make setup   # install/verify build + runtime tools (idempotent)
make run     # build and launch the debug binary
make build   # build the app
make test    # headless crate tests
make app     # release bundle into dist/openOrchestra.app
make doctor  # report which build/runtime tools are present
```

### The one build gotcha

`crates/harness-app` is **deliberately excluded from the root Cargo workspace**
(see the comment in `Cargo.toml`): it drags in the zed git dependency cone, and
keeping it out lets the headless crates build fast with their own lockfile.

The consequence is that a plain `cargo build` or `cargo test` at the repo root
**silently skips the app** and still reports success. That is a false green. To
build or test the app, work from its own directory:

```sh
cd crates/harness-app
cargo build
cargo test --bin harness-app   # it is a binary crate, so --bin, not --lib
```

`make test` runs the headless crates only, by design.

## Coding conventions

- Rust 2024 edition. Keep changes focused, with no drive-by reformatting.
- **Comments explain *why*, not *what*.** This codebase's comments carry the
  reasoning, the measurement, or the failure that motivated the code — the
  surrounding files are the style guide. A comment that restates the line below
  it is noise; one that records "this looked correct and did nothing, because…"
  is why the next person doesn't repeat it.
- Match the density and idiom of the code you are editing.
- Update `docs/architecture.md` when you change architecture, and correct it when
  you discover it is wrong. Several sections carry explicit "this turned out not
  to exist" notes; that is the intended pattern, not an embarrassment.
- Prefer doing git and filesystem work off the UI thread. Synchronous work in a
  render path shows up as dropped frames, and this app renders while agents are
  writing files underneath it.

## Tests and evidence

- New behavior needs tests. Backend crates are tested headlessly; git behavior is
  tested against **real git** in temp repos (`crates/worktrees/tests/git_e2e.rs`),
  not mocks, because the questions worth asking are about what git actually does.
- Follow TDD where practical: write the failing test first.
- **Verify before claiming.** "It builds" is not "it works". For UI changes,
  state how you checked — the app has probe hooks for exactly this
  (`HARNESS_PROBE_PAINT` logs painted bounds, `HARNESS_PROBE_OPEN` opens a file
  and reports pane state, and `RUST_LOG=harness_app=debug` traces the watchers).
  A view that lays out to zero height reports perfect state and draws nothing;
  every state probe passes while the screen shows nothing.
- For performance work, include the before and after numbers and how you got
  them. A claim of "faster" without a measurement will be asked for one.
- Never commit real credentials, tokens, session transcripts, or anything from a
  private repository into tests, fixtures, docs, issues or PRs. Use synthetic
  examples and temp repos.

## Commits and pull requests

- Conventional commit subjects with a scope, matching the existing history:
  `feat(diff): …`, `fix(editor): …`, `perf(diff): …`, `docs(readme): …`.
- The body should explain **why** the change is right, not restate the diff. Say
  what was broken, what you measured, and what you decided against.
- Keep PRs small and single-purpose.
- Say how you verified the change, including anything you could **not** verify.
- If you used an AI assistant, credit it with a `Co-Authored-By:` trailer, as the
  existing history does. Authorship stays with the human who reviewed and stands
  behind the change.
- Expect review from the project lead, who has the final say on acceptance.

## Reporting security issues

Please do **not** open public issues for security vulnerabilities. Use the
repository's private GitHub security advisory channel. Dependency advisories are
also checked on every `Cargo.toml`/`Cargo.lock` change and weekly by
[`.github/workflows/security-audit.yml`](.github/workflows/security-audit.yml).

## Code of conduct

By taking part you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).
