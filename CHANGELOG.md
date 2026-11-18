# Changelog

Notable changes to openOrchestra, newest first. Versions follow
[semver](https://semver.org): patch for fixes, minor for features, major for a
change older builds can't follow.

**How this file is kept:** every commit that changes what users get adds a line
under **Unreleased**, in the group it belongs to (Added, Changed, Fixed,
Security). `make release` turns Unreleased into the new version's section and
puts it at the top of the GitHub release notes. It refuses to release while
Unreleased is empty.

## Unreleased

### Added

- **About openOrchestra** in the app menu: the standard macOS About panel with
  the logo, the version and a link to the repository.
- **Check for updates…** in the app menu. It asks GitHub for the latest release
  only when you click it (there is no background check). Homebrew installs get
  the `brew upgrade` command to copy; other installs get a download link.

### Security

- The phone remote server sends `nosniff`, `no-referrer` and
  `X-Frame-Options: DENY` on every response, including the API and the setup
  page (the web pages already had a Content-Security-Policy).
- Pairing results are logged: each successful pairing, each wrong code, and
  each address locked out after repeated wrong codes.
- Updated the phone client's build tools (vite 6.4.6, vitest 4.1.12), clearing
  21 advisories that affected only development, not the shipped app.
- CI audits the phone client's shipped dependencies on every lockfile change and
  weekly, alongside the existing Rust dependency audit.

## 1.0.0 - 2026-11-17

First public release: a native macOS app to run Claude Code, Codex, OpenCode,
pi and Kimi in parallel git worktrees, with Zed's editor built in, agents that
survive restarts in tmux, multi-repo tasks with a coordinator agent, diff review
made for agents, and phone remote control on your home network.
