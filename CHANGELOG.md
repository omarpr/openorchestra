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

### Changed

- The phone web app shows one live Thinking indicator, hides empty reasoning history, shows what Codex exec calls ran, and aligns tool rows across agents and shells.
- The phone agent list puts elapsed time next to the status badge and aligns agent and shell icons.

### Added

- Shared OpenCode 2.x tabs can show the active session in the phone web app’s Conversation view.
- Attach and send screenshots from the phone web app’s agent composer.
- Dictate editable phone replies with on-device transcription on supported Macs running macOS 26 or newer.

### Fixed

- The phone web app keeps its controls visible if a view fails to load, and renders basic Markdown on browsers without regex lookbehind.
- Long web app drafts wrap and scroll in the composer.
- The phone offers dictation when the Mac supports its current language and can retry a failed transcription from the same recording.

### Security

- Phone setup diagnostics omit the local listener details and certificate directory from app logs.

## 1.1.0 - 2026-11-18

### Added

- **Ports tab** in the tools panel: the servers a task is running, with Open in
  browser and Copy URL, plus the rest of the Mac's listening ports under
  "Other on this Mac".
- Ports published by Docker containers show the container (Compose service and
  project, or the container name) with a Docker icon and the port inside it.
  Compose containers started from a task's folder appear under that task, as
  do containers with a folder from the task mounted in. A plain `docker run`
  container whose name starts with the project's (`comicchat-ergo` for
  `comic-chat`) is listed there too, marked "matched by name".
- A server that's no longer under the task's terminals (started in the
  background, its shell gone) is still listed under the task when it runs from
  inside the task's folder. Hovering a row shows its folder.
- Ports tab rows can be sorted by port or name, ascending or descending, from
  two dropdowns; the choice is remembered. Its "This task" and "Other on this
  Mac" lists collapse like the sidebar's sections and remember their state.
- File tabs show the editor's `{ }` mark, so it's clear they are the zed
  editor.
- Syntax highlighting for the rest of zed's built-in languages: Markdown, C,
  C++, JSONC, `.env`, diffs, git commit messages, go.mod/go.work, JSDoc and
  regular expressions.

### Fixed

- The Review base menu on a project's main checkout read "vs HEAD" and showed
  the same files as Uncommitted. It now reads "vs origin/<default branch>" and
  shows everything the checked-out branch changes.
- The Markdown preview (eye) button showed on the preview itself, where it did
  nothing. It now shows only on the Markdown file.
- The Markdown preview (eye) button did nothing in a file's own tab when focus
  was elsewhere, for example after opening the file from the Git panel or a
  commit. It now always previews the file it sits above.

## 1.0.1 - 2026-11-17

### Added

- **About openOrchestra** in the app menu: the standard macOS About panel with
  the logo, the version and a link to the repository.
- **Check for updates…** in the app menu. It asks GitHub for the latest release
  only when you click it (there is no background check). Homebrew installs get
  the `brew upgrade` command to copy; other installs get a download link.
- `make release` asks whether to bump the version (patch, minor or major) and
  shows the current and latest released versions first.

### Changed

- Release tags now point at the exact commit that was built.

### Security

- The phone remote server sends `nosniff`, `no-referrer` and
  `X-Frame-Options: DENY` on every response, including the API and the setup
  page (the web pages already had a Content-Security-Policy).
- Pairing results are logged: each successful pairing, each wrong code, and
  each address locked out after repeated wrong codes.
- Updated the phone client's build tools (vite 6.4.6, vitest 4.1.12, and glob
  14 under workbox), clearing 20 of 21 advisories that affected only
  development, not the shipped app. The last one (picomatch) has no fix yet.
- CI audits the phone client's shipped dependencies on every lockfile change and
  weekly, alongside the existing Rust dependency audit.

## 1.0.0 - 2026-11-17

First public release: a native macOS app to run Claude Code, Codex, OpenCode,
pi and Kimi in parallel git worktrees, with Zed's editor built in, agents that
survive restarts in tmux, multi-repo tasks with a coordinator agent, diff review
made for agents, and phone remote control on your home network.
