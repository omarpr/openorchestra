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

- **Windows** (beta): openOrchestra runs natively on Windows 10 and 11,
  built from source. `.\make setup`, `.\make run` and `.\make install` (or `make.cmd`) build
  it with the GNU toolchain, without Visual Studio or admin rights, and
  `.\make dist` and `.\make uninstall` package and remove it. Agents and
  shells run in the bundled `oo-session.exe`, so they keep running after you
  quit, their tabs reattach when you reopen the app, and the phone remote
  attaches to them. Browser tabs use Edge WebView2, notifications are
  Windows toasts, and ⌘ shortcuts are Alt (Ctrl in the editor), shown with
  Windows labels and remappable in **Settings → Shortcuts**.
- **First-time Checks** has an **Agents** tab: every agent CLI the app
  drives, with its version or its vendor's installer. Each fix command has
  **Copy** and **Run in shell**, which types it into a shell tab in the
  selected worktree. An agent that is installed but won't start says why
  (and asks for a newer Node.js when that is the cause) instead of offering
  to install it again.

### Changed

- The Claude Code and Grok status hooks report only sessions the app
  launched, so an agent you start yourself in the same worktree no longer
  drives the task's status.

### Fixed

- First-time Checks shows the version of a tool that prints it on stderr
  (pi).
- The app menu's tooltip no longer covers About, and **Add a device** fits
  smaller windows, so Done stays visible.
- A task's setup says so when it couldn't be typed into its shell tab,
  instead of reporting "setup finished", and waits at most 4 seconds for
  that shell to appear, however slowly tmux answers.
- An agent tab's status and notifications come only from that agent: a
  test suite or another agent CLI run inside a Codex tab no longer has it
  announced as "pi finished" or shown as Grok.

### Security

- `url` is updated to 2.5.9 (RUSTSEC-2026-0592: a stack overflow in
  `Url::origin()` on nested `blob:` URLs).

## 1.4.0 - 2026-11-20

### Added

- **Browser** tabs (＋ → Browser): a WebKit page next to your shells and
  agents, with back, forward, reload and Open in default browser. A blank
  browser tab lists the servers running in the task and the project's recent
  pages, a click away. Each
  project remembers the pages its browser tabs open, and the address bar
  suggests them as you type (↑/↓ and Enter to pick one). **Settings ›
  General › Clear browsing history** forgets them. The conductor stays
  visible over a browser page, with the page showing right up to his
  outline, and can be dragged there. Browser tabs have
  Developer tools (Web Inspector) from the address bar and from right-click
  → Inspect Element. Developer tools open docked in the tab (bottom by
  default; dock left/right or in a window from the inspector's toolbar,
  remembered).
- Links to local addresses (localhost, your network, Tailscale) and ports
  opened from the current project or task open in a browser tab. Other links
  still use your default browser. Change it in **Settings › General**.
  A server printed as `0.0.0.0` or `[::]` opens through loopback, which
  WebKit can reach.
- Drag agents by their grip in **Settings › Agents** to reorder them. The
  order is used in the **+** menu and the new-task picker. New tasks start
  with your starred agent, else your last-used agent, else the top of the
  list; the filled ★ on a row marks which one that is.
- Star an agent in **Settings › Agents** to make it the default for new tasks.
- Agent tabs show a spinner while that agent is running. With several agents
  in one task or project, each tab shows its own.
- In the Review panel, ↑ and ↓ move between changed files once you've
  clicked one.
- In a changed line, the words that changed get a stronger highlight, in
  both unified and split view.

### Changed

- Sidebar rows leave a little more room between the status pill and the
  badges (PR, ahead/behind, CI) below it.
- Review Overview tabs show their icon first and the same running spinner as
  agent tabs.
- Settings are tidier. The long explainer above Agents is gone; its useful
  parts are now tooltips on the command field and the Codex row. A built-in
  agent with an edited command has a ↺ button that restores the default.
  On/off settings use switches. A custom agent's status and hook options are
  now visible choices instead of chips you click to cycle.
- A new **General** settings pane, where Settings now opens, holds Terminals
  on quit and the always-on conductor. "Show gitignored files" moved to
  **Editor**, and the Diff pane is now called **Review**.
- With no saved order, agents are listed alphabetically in Settings, the **+**
  menu and the new-task picker, and the top one (Claude Code out of the box)
  is the default for new tasks.
- The **+** menu lists Shell and Editor first, then your agents under their
  own **Agents** heading in Settings order, and every item's label now lines
  up. Terminal is now Shell.
- Tabs show a colored icon for every kind and title-case names (Shell,
  Editor, Diff, Claude, Codex, …) instead of the stored lowercase titles, and
  the phone UI uses the same names. An agent tab's running spinner sits after
  its icon. Editor and file tabs show Zed's logo in Zed blue, Shell's `❯_` is
  cyan and Diff's `⑂` is pink.

### Fixed

- OpenCode tabs show when the agent is running or waiting again (OpenCode 2
  moved its server; the app now reads status from inside each OpenCode
  session).
- OpenCode tabs can be used from your phone when your OpenCode `cli.json`
  has comments or trailing commas. The app used to skip its plugin for those
  files.
- Tab and Shift-Tab cycle through controls in Settings, New Task, and New
  Worktree with a visible focus indicator. Settings scrolls the focused
  control into view. macOS accessibility exposes Settings navigation, agent
  switches, and the task form.
- New Task and New Worktree show a missing-name error beside Name, focus
  the field, and clear the error as soon as you enter a name.
- Settings keeps its navigation and close button in place when changing panes.
  Agent names match the tab menu and the default marker leaves room for the
  full name.
- The tools panel labels Git's count as changed files. Review shows the comment
  count with its comment actions and in the tab tooltip.
- The selected file in the Review panel's file list is now clearly
  highlighted.
- Added and removed lines in the diff are tinted across the full width
  instead of stopping where the text ends.
- The update prompt for Homebrew installs now gives the full cask name,
  `brew upgrade --cask omarpr/tap/openorchestra`, so the copied command
  targets the openOrchestra tap.
- Status messages are shown in red only when something failed. A success
  message that happens to name something like "Error Phone" stays in the
  normal color.
- When you change a keyboard shortcut in **Settings › Shortcuts**, pressing a
  key combination that already does something, like ⌘W, now records it
  instead of running it.

## 1.3.0 - 2026-11-19

### Added

- Open **Overview** from the Review panel to generate a review with Codex,
  Claude Code, pi, or Grok. It reviews branch commits and local changes,
  including staged, unstaged, and untracked files. For a multi-repository task,
  it uses each repository's configured base. You can cancel a run or refresh
  the report.
- Review Overview shows a recommended reading order when the report provides
  one. Each step says what to look for, marks your place as you follow it, and
  links to changed files in the editor. The writeup also links recognized
  file references to the editor.
- Choose the review CLI and model in Settings → Diff. **Chat about this**
  opens the report in a fresh agent tab with the same CLI and model for
  Codex, Claude Code, and pi.
- Grok is a built-in agent in Settings → Agents and appears in the agent
  picker when enabled. It uses the locally installed `grok` CLI and opens a
  plain shell if the CLI is missing from `PATH`. Grok starts in auto permission
  mode, receives task context, and can resume the latest session in the
  current directory. Grok's status and notifications require trusting the
  folder in Grok. Shared Grok tabs show the Grok name and icon on the phone.

### Fixed

- An agent started from inside an agent tab but working in another worktree
  now reports status for that worktree instead of the parent tab.
- The Review Overview tab shows the CLI and model chosen in Settings as soon
  as they change.
- With Review Overview focused, ⌘W closes its tab and ⌘Q quits openOrchestra.
- The sidebar shows pull request and CI badges for open pull requests,
  including drafts, opened by the signed-in GitHub user.
- Renamed worktree rows in the Projects sidebar leave the second line for Git
  and pull request badges. Hover over the row to see its branch.
- Review Overview restores the last completed report and its tab after a
  restart, even when an older build shares the same pane settings.
- Review Overview reports a missing CLI when generating a report or opening
  **Chat about this**. If the CLI was removed after generation, the completed
  report stays available.
- An agent tab recovered at startup shows the status that tab last saved.
- Review model lists open from the last saved catalog. **Reload models** asks
  the CLI for an updated list.
- Agent status, alerts, and waiting-tab navigation follow the tab that produced
  them when several agents share a worktree. The collapsed Conductor badge
  counts running agent tabs.
- Starting to watch several worktrees no longer blocks status updates on Linux.
- Notifications can arrive while another agent tab is selected, the app window
  is inactive, or a maximized tools panel covers the center pane.
- A closed or exited agent clears its own activity. Running agents keep their
  status when a tmux pane listing fails.
- Closing a file preview keeps the neighboring tab selected. Clicking an
  already open file in the Explorer brings that file's tab forward.
- An untracked file named `-` is included in a Review Overview diff, and a
  failed untracked diff with no patch counts as an omission. The report shows
  patch omissions and truncation, and explains when the snapshot uses an empty
  tree or falls back to uncommitted changes because it cannot find a merge base.
- Built-in Codex launches use `--no-daemon` when supported so the Conductor
  and notifications receive Codex's status updates.
- Closing the active tab selects the next tab to the right, or the tab to the
  left when closing the last one.

### Security

- Update the desktop app's Wasmtime and heapless dependencies to patch
  advisories reported by RustSec.
- Review Overview configures each CLI to restrict tool access during its
  one-shot snapshot review.

### Known Issues

- Opening an agent tab while a file tab is open can return focus to the editor.

## 1.2.1 - 2026-11-19

### Fixed

- Keep native macOS menu items (`About openOrchestra`, `Check for updates…`,
  `Settings…`, `First-time Checks…`, `Go`, and `View`) enabled and responsive
  even when no pane or editor is focused, and restore workspace focus after
  closing dialogs.
- Run startup tool-requirement checks off the UI thread so the desktop window
  opens and responds immediately without waiting on CLI version probes.

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
