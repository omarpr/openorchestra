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

## 1.2.0 - 2026-11-18

### Added

- Attach PNG, JPEG, or WebP screenshots up to 8 MiB from the phone agent
  composer, with an optional caption. Codex receives an image attachment and
  other agents receive the file path on the Mac.
- Dictate editable phone replies with on-device transcription on supported Macs
  running macOS 26 or newer. Development and release builds compile the helper
  with the macOS 26 SDK or later, and app bundles include and sign it.
- Shared OpenCode 2.x tabs show the active session in the phone web app’s
  Conversation view. The view becomes available when the first export arrives.
- Desktop project and worktree rows show clickable pull request numbers and CI
  chips. The right-click menu opens the pull request, and the CI chip opens its
  Checks tab.

### Changed

- Settings → Agents defaults to `codex --sandbox workspace-write --ask-for-approval on-request`
  for new Codex launches. Codex uses its normal hook trust prompts.
- Desktop project and worktree rows have equal padding and a consistent height
  with or without status badges and Git details. Chips show commits ahead or
  behind and the number of changed files.
- Desktop and phone checkout rows show the checked-out branch. The default
  branch appears only while it is checked out.
- The phone Conversation view shows one live Thinking indicator, hides empty
  reasoning history, and aligns tool rows across agents and shells. Codex command
  details show the executed command and its output.
- The phone agent list puts elapsed time next to the status badge, aligns agent
  and shell icons, and removes the duplicate shell prompt glyph.
- Desktop agent launch preparation runs in the background to keep the app
  responsive while hooks and terminals are set up.

### Fixed

- Reuse the same task or worktree name and Git branch across projects, including
  standalone projects. New slugs include the task, home project, and branch.
- Pull request and CI status follow the checked-out branch in each repository,
  even when multiple projects use the same branch name. Repositories awaiting
  their first commit display the current branch name.
- Starting multiple specialists keeps their panes open until their tmux sessions
  are ready and delivers queued messages after startup.
- Concurrent OpenCode launches can install the phone integration safely. The
  export recreates missing transcripts and preserves Unicode when truncating
  long entries.
- The phone web app keeps its controls available if a view fails to load, renders
  basic Markdown on older browsers, and retries temporary device lookup failures.
- Long phone drafts wrap and scroll. Dictation is available on supported
  languages, can retry transcription from the same recording, and keeps Stop and
  Cancel available during an active recording.
- Canceled or failed screenshot sends clean up files that never reached the
  agent. A file stays available once its path may have reached the agent.
- Address Rust lint warnings in headless remote builds without web assets.

### Security

- Screenshot uploads validate image structure and dimensions, limit concurrent
  uploads, and time out slow requests. Delivery rechecks device write access,
  session sharing, and the target agent before sending. Dictation also rechecks
  write access before starting the transcription helper.
- Disabling phone remote stops shell and OpenCode transcript exports, even if the
  agent list is refreshing. OpenCode exports are cleared when a tab is unshared.
- Phone setup diagnostics omit local listener details and the certificate
  directory from app logs.
- The CI and dependency audit workflows use read-only repository access.

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
