# openOrchestra

A macOS harness for orchestrating coding agents — pi, Claude Code, Codex,
OpenCode, and Kimi — across git worktrees, with a real embedded zed editor,
per-worktree terminals, diff review, and native notifications when an agent
needs you.

## Run it (macOS)

```sh
make setup && make app && open dist/openOrchestra.app
```

- `make setup` installs/verifies the build and runtime tools (idempotent).
- `make app` builds the release bundle into `dist/openOrchestra.app`.
- First launch will ask for notification permission — that's the
  "agent needs your attention" banners with their View button.

For development iterations, `make run` builds and launches the debug binary
directly (native notifications fall back to plain banners there — the
notification framework requires a real app bundle).

## More

Architecture, decisions, and the embedding gotchas live in
[docs/architecture.md](docs/architecture.md).
