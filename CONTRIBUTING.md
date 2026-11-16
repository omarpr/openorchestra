# Contributing to openOrchestra

Thanks for your interest in openOrchestra. This project is **maintainer-led**
with a deliberately narrow scope. Before opening a pull request, please read this
guide and the [Code of Conduct](CODE_OF_CONDUCT.md).

## Before you write code

- **Bugs:** open an issue, or send a small, obvious fix straight as a PR.
- **New features or non-trivial changes:** open an issue or discussion **before**
  writing code. PRs that skip this may be closed without review.
- **Out-of-scope proposals:** anything that pushes openOrchestra toward a web UI,
  a plugin API, or replacing the embedded zed editor gets closed. Nothing
  personal, just scope discipline.

## Project invariants (non-negotiable)

A change that breaks one of these will not merge:

- **Native UI, no web tech.** GPUI (zed's UI framework) for all UI. No Electron,
  no cross-platform abstraction layers, no web views. GPUI itself runs on macOS,
  Linux and Windows — this rule is about *how* the UI is built, not where it
  runs.
- **A real embedded zed, not a re-implementation.** The editor is zed's own
  `Workspace` hosted as a child view. When something is missing, wire up zed's
  existing surface rather than reinventing it — see the pane-toolbar notes in
  `crates/harness-app/src/workspace_host.rs` for what "hosting" actually entails.
- **Backend crates stay UI-free.** `worktrees`, `forge`, `status-watch`,
  `agent-runtime`, `agent-schema`, `training`, `pi-client` and `a2a-broker` are
  headless and fully tested without a window. The app crate renders; it does
  not own protocol or git logic.
- **Agent-agnostic.** pi, Claude Code, Codex, OpenCode and Kimi are peers. Don't
  privilege one in shared paths; agent-specific behavior belongs behind
  `AgentKind` or in that agent's own module.
- **Nothing leaves the machine.** No telemetry, no analytics, no uploading a
  user's code, diffs, prompts or credentials.
- **The UI must not lie.** A badge, a pill or a count that can go stale is worse
  than one that isn't shown — a stale number reads as a current one. If you add a
  surface that reflects external state, make it refresh, and say in a comment
  what refreshes it.

## Platform support

macOS is the only target built and tested. Linux is **not ruled out** — the
architecture does not stand in the way, and it is worth knowing why before
anyone assumes either direction:

- Every platform-specific line in the codebase lives in
  `crates/harness-app/src/notifier.rs`, which already carries a `notify-send`
  branch and a catch-all for everything else. The `objc2` dependencies are gated
  to macOS in `Cargo.toml`, so they would not even be compiled elsewhere.
- The gaps are known and none of them is architectural: `notify-send` has no
  action buttons or response callback, so the notification's **View** button
  needs D-Bus (`org.freedesktop.Notifications`) to reach parity; notification
  sound is macOS-only today; packaging, `setup-macos.sh` and the `.app` bundle
  are macOS-shaped; and `ps -axo pid=,ppid=,comm=` in the agent-liveness probe
  needs checking against GNU `ps`.
- There is no Linux build in CI, so nobody should claim support until there is.

Patches that keep the door open are welcome; a claim of "Linux support" without
a CI job to back it is not.

## Development setup

Requires macOS with a Rust toolchain. `make setup` installs and verifies the
rest; `make doctor` reports what is present.

```sh
make setup   # install/verify build + runtime tools (idempotent)
make run     # build and launch the debug app in a .app bundle
make build   # build the app
make test    # headless crate tests
make app     # release bundle into dist/openOrchestra.app
make dmg     # universal (arm64+x86_64) bundle + .dmg into dist/
make release # universal bundle + .dmg, published to GitHub Releases
make doctor  # report which build/runtime tools are present
```

`make dmg` and `make release` compile a second time for `x86_64-apple-darwin`.
Crate sources are shared, so nothing re-downloads, but the Intel slice is a cold
build of the whole zed cone the first time. `make release` refuses to run on a
dirty tree or an existing tag — a published build must be reproducible from its
tag. Version comes from `crates/harness-app/Cargo.toml`; the build number is the
commit count.

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
- When you discover that a design note or comment is wrong, correct it in the
  same change. "This turned out not to exist" is the intended pattern, not an
  embarrassment — the next person pays for a stale explanation.
- Prefer doing git and filesystem work off the UI thread. Synchronous work in a
  render path shows up as dropped frames, and this app renders while agents are
  writing files underneath it.

## Tests and evidence

- New behavior needs tests. Backend crates are tested headlessly; git behavior is
  tested against **real git** in temp repos (`crates/worktrees/tests/git_e2e.rs`),
  not mocks, because the questions worth asking are about what git actually does.
- Follow TDD where practical: write the failing test first.
- **Verify before claiming.** "It builds" is not "it works". For UI changes,
  state how you checked — the app has a probe hook for exactly this
  (`HARNESS_PROBE_PAINT=1` logs painted bounds of overlays and lists, and
  `RUST_LOG=harness_app=debug` traces the watchers).
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

## Licensing of contributions

openOrchestra is [GPL-3.0-or-later](LICENSE), because it links zed's GPL crates —
see the README. Contributions are accepted under that same license: what comes in
goes out under the terms the project already carries. There is no CLA and no
copyright assignment.

Two practical consequences worth knowing before you write code:

- Don't paste in code under a license that cannot combine into GPLv3 (anything
  proprietary, or GPL-incompatible terms). Apache-2.0 and MIT are fine.
- Adding a dependency means checking its license, not just its API. The weekly
  [`security-audit.yml`](.github/workflows/security-audit.yml) job checks
  advisories, not license compatibility — that part is on the reviewer.

## Reporting security issues

Please do **not** open public issues for security vulnerabilities. Use the
repository's private GitHub security advisory channel. Dependency advisories are
also checked on every `Cargo.toml`/`Cargo.lock` change and weekly by
[`.github/workflows/security-audit.yml`](.github/workflows/security-audit.yml).

## Code of conduct

By taking part you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).
