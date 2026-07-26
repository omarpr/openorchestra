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

## Collaborators

<table>
  <tr>
    <td align="center">
      <a href="https://github.com/omarpr">
        <img src="https://avatars.githubusercontent.com/u/4412916?v=4" width="88" height="88" alt="omarpr"><br>
        <sub><b>omarpr</b></sub>
      </a>
    </td>
    <td align="center">
      <a href="https://github.com/joeswiggin">
        <img src="https://avatars.githubusercontent.com/u/4819705?v=4" width="88" height="88" alt="joeswiggin"><br>
        <sub><b>joeswiggin</b></sub>
      </a>
    </td>
  </tr>
</table>

## Contributing

[`CONTRIBUTING.md`](CONTRIBUTING.md) covers the setup, the project invariants,
and the one build gotcha that bites everyone: `crates/harness-app` is outside the
root workspace, so a root `cargo build` skips it and still reports success.
By taking part you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).

## More

Architecture, decisions, and the embedding gotchas live in
[docs/architecture.md](docs/architecture.md).
