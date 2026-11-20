# Browser tab, link routing, and the new-tab menu — design

Date: 2026-11-19. Status: approved 2026-11-19; implemented with the post-review decisions below. Link scope (local addresses only) decided 2026-11-19.

## Why

Agents and dev servers produce URLs all day: a `localhost:3000` from `npm run
dev`, a link an agent prints, a port in the Ports panel. Today every one of
them leaves the app for the default browser, away from the worktree it
belongs to. A browser tab keeps the page next to the agent and shell that
produced it, per worktree.

The new-tab (＋) menu grows a Browser entry, so it gets reorganized at the
same time: kinds on top, agents below in the user's order, aligned rows,
and colored icons that carry over to the tab strip.

## Goals

1. **Browser tab.** A tab kind showing a web page with WebKit
   (`WKWebView`), with back, forward, reload and an address bar.
2. **Local links open in a browser tab by default.** A link to a local or
   dev address (localhost, private network, Tailscale), ⌘-clicked in a
   Shell or agent tab or opened anywhere else in the app, opens in a browser
   tab of the active worktree. So does **Open** on a port in the Ports
   panel's current project/task section. Public links (GitHub, docs,
   sign-ins) keep going to the default browser. A setting turns routing off.
3. **＋ menu reorganized.** Shell, Editor (Diff and Browser) on top, an
   "Agents" separator, then the enabled agents in the Settings › Agents
   order, then pi chat. Every row's label starts at the same x.
4. **Colored kind icons** for Shell, Editor, Diff and Browser, in the menu
   and on tabs, the way agents already wear their brand colors.
5. **Title-case tab labels:** Shell, Editor, Claude, Codex, OpenCode, Kimi,
   Pi, Grok. Browser tabs read `Browser · <page title>`.

## Non-goals

- Chromium/CEF, or any bundled engine. WebKit ships with macOS; the DMG
  stays the same size.
- A general-purpose browser: no search from the address bar, no bookmarks,
  history UI, downloads manager, extensions, or private windows.
- Agents driving the browser (CDP/automation). WKWebView has no CDP; that
  is a separate project if wanted.
- Linux/Windows. The browser tab is macOS-only, like the rest of the app
  today.

## Anti-goals (would be a failure even if it works)

- **A gpui menu, popover, or dialog hidden behind the page.** The web view
  is a native view above gpui's drawing; anything gpui draws over the
  center area must not end up underneath it.
- **App shortcuts dead while a page has focus.** ⌘T, ⌘E, ⌘W, tab and
  worktree switching must keep working. (⌘C/⌘V/⌘X/⌘A/⌘Z go to the page.)
- **An older build wiping tabs.** The installed app and a dev build share
  `~/.openorchestra/panes.json`. An older build that meets an unknown tab
  kind loads an empty state and its next save drops everyone's tabs. Browser
  tabs must never appear in `panes.json` in a form older builds can't read.
- **A web view left behind**: visible on the wrong worktree, after its tab
  closed, or in the wrong window.

## Constraints

- gpui's `Window` implements `HasWindowHandle` (`gpui/src/window.rs:7864`;
  `raw-window-handle` 0.6.2 is in `crates/harness-app/Cargo.lock`), and on
  macOS it hands out the window's real AppKit view. So the web view can be
  attached with either `wry` (`build_as_child`, which brings new-window,
  download and navigation handlers plus devtools) or `objc2-web-kit` (same
  `objc2` 0.6 family already used, e.g. in `update_check.rs`). **Builder's
  call**, made in the first spike. Prefer `wry` if it builds against the
  `objc2` already in the lockfile without a second copy; otherwise
  `objc2-web-kit`. Either way it is WebKit, and the DMG size is unchanged.
- Universal build (arm64 + x86_64). No new bundled frameworks.
- `crates/harness-app` builds from its own directory (root `cargo` skips it).
  Don't run `cargo fmt` on it (the crate isn't fmt-clean).
- Every user-visible commit adds a `CHANGELOG.md` Unreleased line.
- **Work in flight:** another session has uncommitted changes on `master`
  that add Settings › Agents drag-to-reorder (`Prefs::agent_order`,
  `agent_catalog` now honors it) and touch `ui/center.rs`, `ui/settings.rs`,
  `agents.rs`, `workspace.rs`. This work builds on top of that once it is
  committed, in its own git worktree, rather than editing the same files
  concurrently.

## Approaches

### Embedding the page (decided earlier)

| | Size added | Notes |
|---|---|---|
| **WebKit (`WKWebView`) — chosen** | ~0 (system framework) | Native view over the tab; ~65 MB RAM for a blank page, ~265 MB for a heavy one (measured) |
| Chromium via CEF | ~150–250 MB compressed, universal | Helper apps, signing, monthly security updates; gives DevTools and CDP |
| Servo/Blitz | — | Not ready for real sites |

### Catching terminal links — **Option A chosen**

zed's `TerminalView` handles ⌘-click itself: `Event::Open(Url) =>
cx.open_url(url)` (`terminal_view.rs:1333`). Nothing hands us the URL first.

- **A (chosen): wrap gpui's platform.** Start gpui with a thin
  `Platform` wrapper that forwards every method to the real platform and
  intercepts `open_url`. Compile-time checked; catches links from anywhere
  (terminals, zed's markdown preview). Costs ~250 lines of delegation that
  a zed bump can break *at compile time*.
- B: intercept ⌘-click on our terminal wrapper and read
  `last_content.last_hovered_word`. Small, but depends on zed internals and
  can silently stop working after a zed bump; terminals only.
- C: patch zed's `terminal_view`. Rejected: a fork to carry on every bump.

## Design

### 1. Link routing (platform wrapper)

- `main.rs:155` currently runs `gpui_platform::application()`. Instead,
  build the real platform, wrap it in `LinkRoutingPlatform`, and start with
  `gpui::Application::with_platform(...)` (already used by the tests at
  `main.rs:851`). Every `Platform` method forwards; only `open_url` is
  intercepted.
- **Only local and dev addresses are routed** (decided). The wrapper sees
  every `open_url` in the process, including zed code the app hosts
  (editor hover links, git blame/permalinks, markdown preview, workspace
  notifications), and can't tell where a call came from. So it routes by
  **address**, not by source:
  - routed to a browser tab: `http`/`https` URLs whose host is
    `localhost`, `*.localhost`, `127.0.0.0/8`, `[::1]`, `*.local`, a
    private IPv4 address (10/8, 172.16/12, 192.168/16), or a Tailscale
    name (`*.ts.net`) or address (100.64/10);
  - everything else (GitHub, docs, sign-in pages, any public host, every
    non-http(s) scheme like `mailto:`, `file:`, `zed:`) goes to the real
    platform, i.e. the default browser. Sign-ins keep the user's real
    sessions and password manager.
- `open_url` has no app context, so it decides and **enqueues**: a routed
  URL is pushed onto a channel the workspace drains on the main thread
  (opening the tab); anything else is forwarded. The decision is a pure
  function (scheme × host × setting) with unit tests.
- **Explicitly external callers** use a helper that bypasses routing even
  for local addresses: Ports panel **Open** in the *Other* section (ports
  not in this project/task). About/update-check and sidebar GitHub links
  are public hosts and go external on their own; no bypass needed.
- **Where the tab opens:** in the active worktree (any click that opens a link
  happens in the visible, active worktree). If a browser tab in that
  worktree already shows exactly that URL, focus it; otherwise open a new
  browser tab after the active tab. With no worktree selected, open
  externally.
- **Ports panel, current project/task section:** **Open** opens a browser
  tab directly (our own code, no routing needed), subject to the same
  setting. *Other* section: external, as today.
  - In a task with several members, the "this task" list mixes ports from
    every member worktree and the coordinator (`ports_scopes`,
    `ports_panel.rs:25`). **Open** puts the tab in the pane the user is
    looking at (the active one), not the member that owns the port, so the
    click always visibly does something. The open-or-focus lookup is scoped
    to that pane. (My call.)

### 2. Setting

- Settings › General: **"Open links in a browser tab"**, default **on**.
  Help text: "Links to local addresses (localhost, your network, Tailscale)
  and ports you open from this project or task open in a browser tab. Other
  links always use your default browser. Turn off to send everything to
  your default browser."
- `Prefs::links_in_browser_tab: bool`, `#[serde(default = "true")]`-style
  default so existing `prefs.json` files get **on**. The wrapper reads it
  through a cheap shared flag (e.g. an atomic updated when prefs change),
  since it can't borrow prefs from inside `open_url`.
- Known limitation, accepted: an older build re-saving `prefs.json` drops
  the unknown key, which reads back as the default (on).

### 3. Browser tab

- **Kind and content:** `TabKind::Browser`, `TabContent::Browser(Entity<BrowserPane>)`.
- **Persistence:** `TabRecordKind::Browser { url }` is saved **beside**
  `panes.json`, the way Review Overview tabs are (`legacy_and_reviews` /
  `ReviewTabsFile` / `PlacedReviewTab` in `panes_state.rs`), so older builds
  never see it. Generalize that sidecar rather than copy it. Same treatment
  of the active-tab id (an older build must not be left pointing at a tab id
  it doesn't have).
- **Title:** `Browser · <page title>`; `Browser · <host>` until a title
  arrives; `Browser` for a blank tab. The strip already truncates.
- **New from the ＋ menu:** a blank page with the address bar focused.
- **Address bar** (top of the tab, gpui-drawn): back, forward,
  reload/stop, a URL field, and an **Open in default browser** button
  (sends the current URL out, bypassing routing). Enter navigates. No scheme →
  `http://` for `localhost`, `127.0.0.1`, `[::1]`, `*.local`, or any
  `host:port`; `https://` otherwise. Not a search box.
- **Web view lifetime:** created when the tab is shown, **destroyed when
  hidden** (tab switch, worktree switch, window hidden); the URL and title
  are kept and the page reloads when shown again. Closing the tab destroys
  it. This keeps background tabs at ~0 memory.
- **Placement:** a gpui element in the tab body reports its bounds each
  frame (prepaint) and the pane sets the `WKWebView` frame to match
  (convert gpui's top-left points to the content view's coordinates). The
  web view is a subview of the gpui window's content view; it must attach
  to the window that owns the tab, also with several windows open.
- **Overlays:** whenever gpui draws something over the center area (＋
  menu, tab context menu, comment popover, prompts, Settings, any modal),
  the web view is hidden (or replaced by a static snapshot) until the
  overlay closes. The builder picks the mechanism; the anti-goal above is
  the bar.
- **Keyboard:** page gets text editing and clipboard keys; the app's own
  shortcuts (from `shortcuts.rs`) are delivered to gpui even while the page
  has focus. Esc in the address bar returns focus to the page.
- **Page behavior:**
  - `target=_blank` / `window.open` → a new browser tab in the same worktree.
  - Downloads, non-http(s) navigations → handed to the default browser/OS.
  - `isInspectable = true` (macOS 13.3+), so Safari's Web Inspector works.
  - One persistent default `WKWebsiteDataStore` (cookies survive restarts,
    shared across browser tabs).
  - Load failures (connection refused is common for a dev server not
    started yet) show WebKit's/our error text in the page area with a
    Reload button; no crash, no external fallback.

### 4. ＋ menu

```
 [icon] Shell          ⌘T
 [icon] Editor         ⌘E
 [icon] Diff                 (only when Review opens as a tab)
 [icon] Browser
 ── Agents ───────────
 [logo] <enabled agents, in Settings › Agents order>
 [logo] pi chat        ⌘⇧C   (as today: worktrees, pi enabled)
```

- "Terminal" is renamed **Shell**.
- One row component for every entry: fixed 16 px icon column, gap, label,
  right-aligned shortcut. Kind entries stop embedding glyphs in the label
  (`"❯_  Terminal"` today), which is what misaligns them now.
- "Agents" is a small, muted, non-clickable section label with a rule.
- Agent order comes from `agent_catalog` (which the in-flight work makes
  follow Settings › Agents); disabled agents stay hidden.
- No shortcut for Browser in this round.

### 5. Kind icons and colors

- One function, like `agent_mark`/`agent_icon`, returning icon + color per
  kind, used by both the menu and the tab strip:

  | Kind | Icon (zed `IconName`, closest available) | Color |
  |---|---|---|
  | Shell | Terminal | yellow `#e5c07b` |
  | Editor (and file tabs) | Code | cyan `#56b6c2` |
  | Diff | a diff/git icon | pink `#e06c9f` |
  | Browser | Globe | sky `#61afef` |

  Chosen to avoid the agent colors (pi purple, Claude orange, Codex and
  OpenCode greens, Kimi blue, Grok white). The builder may nudge values for
  contrast in light themes.
- **Tabs:** a 13 px colored icon before the label for Shell, Editor, file
  tabs (replacing today's `{ }` text), Diff and Browser. Agent tabs keep
  their logo; Review Overview keeps its icon/spinner; the other session's
  running spinner stays.

### 6. Tab labels

- Computed when the tab is drawn from its kind, **not stored**, so saved
  titles and older builds are unaffected: Shell, Editor, Diff,
  `Browser · …`, and agents Claude, Codex, OpenCode, Kimi, Pi, Grok; a
  custom agent shows its own name; file tabs keep the file name; Review
  Overview unchanged.
- Old stored titles with glyph prefixes (`❯_ shell`, `{ } editor`,
  `⑂ diff`) are ignored for display.
- The ＋ menu and notifications keep full names ("Claude Code").
- The phone remote UI uses the same display function, so tab names match
  on both.

### 7. Testing

- Unit: routing decision (scheme × host × setting: each local/private/
  Tailscale form routed, public hosts and look-alikes such as
  `localhost.example.com` or `10.0.0.1.evil.com` not); address-bar URL normalization;
  label function per kind; menu ordering (kinds, separator, agents in
  `agent_catalog` order, pi chat last); sidecar round-trip proving
  `panes.json` never contains a Browser record and an older-format load
  keeps the other tabs.
- Manual (scratch `.app` + isolated HOME, never the running Dev app):
  ⌘-click a `localhost` URL in a shell; Ports panel Open in both sections;
  setting off → external; switch tabs/worktrees/windows and close the tab
  (no stray view); open ＋ menu and a modal over a page (nothing hidden);
  ⌘T/⌘W/tab switch with the page focused; restart restores browser tabs;
  installed build and dev build both running keep each other's tabs.

## Left to the builder

- `wry` vs `objc2-web-kit` (see Constraints), and how to keep the web
  view's frame in sync with the tab body. Spike this first: it decides
  everything in §3 placement. The app opens a single window (`main.rs:319`).
- The sidecar file's name. It must be new: the installed 1.3.0 build
  already reads `review-tabs.json`, so browser tabs can't go in it.
- The overlay-hiding and shortcut-forwarding mechanisms, against the
  anti-goals.
- Exact zed `IconName`s and color nudges.
- The channel/drain mechanics between `open_url` and the workspace.
- Sequencing into commits; CHANGELOG wording.

## Decisions after review

What was built differs from the sections above as follows. One line each:
what changed against this spec, and why.

### Rulings during the build (R1–R11)

The rulings as logged during the build (ruling text verbatim), with a
note where what shipped differs.

- **R1:** Names follow the spec — "Shell" in menu and tab; tabs use short
  agent names (Claude, Codex, OpenCode, Kimi, Pi, Grok); menu keeps full
  names. — user asked for this explicitly in the design session — cost if
  wrong: rename strings.
- **R2:** Editor and file tabs keep the base branch's Zed mark
  (IconName::AiZed) in EDITOR_MARK 0x3d8bfd instead of spec's cyan Code
  icon. — newer, deliberate brand-mark choice already committed by the
  user's other session; matches "like agent colors" — cost if wrong: one
  icon + constant.
- **R3:** Shell keeps base's ❯_ glyph in TERMINAL_MARK 0x3ec7d6 (cyan)
  instead of spec yellow; spec's yellow existed only to avoid Editor-cyan,
  which R2 removes. — cost if wrong: one constant.
- **R4:** New kinds use spec colors: Diff ⑂ glyph in 0xe06c9f (pink, was
  grey), Browser Globe in 0x61afef. pi chat row uses agent_icon(Pi). —
  spec values — cost: constants. *As built:* this zed rev has no Globe, so
  the Browser icon is `IconName::ToolWeb`; after the final review the pi
  chat tab uses `agent_icon(Pi)` too.
- **R5:** Keep base's "AGENTS" heading style (uppercase small label over a
  rule); the entry model calls it Separator("Agents"). — satisfies "an
  agents separator" — cost: style tweak.
- **R6:** Subagents don't do GUI/manual checks or release/universal builds
  (no release cache; hours to build; scratch-app needs isolation + Screen
  Recording). Implementers verify with cargo test + debug cargo
  build/check; the controller runs one consolidated scratch-app visual
  check + one `make dmg`-style universal build at Task 9. — cost if wrong:
  UI bugs found later, in one batch.
- **R7:** Controller runs a visual smoke check (page draws above gpui +
  takes clicks) right after Task 6, before Task 7 — fallback per T5
  report: sibling view in contentView. — cost if wrong: T6 rework. *As
  run:* a partial check from the user's screenshot confirmed the page
  draws above gpui, aligned, with its label; clicks weren't confirmed then
  and are in the Task 9 manual checks.
- **R8 (security, fail-safe):** file:/javascript:/unknown → Ignore; custom
  app schemes → External only on LinkActivated from main frame, else
  Ignore+log; downloads main-frame only (+LinkActivated on action path);
  open_external rate-limited 1/s and passes normalized URL; blob/data
  downloads emit on_failed. — spec said "non-http(s) → OS"; narrowed for
  safety — cost if wrong: OAuth/redirect-to-app flows don't launch from a
  browser tab (user has Open-in-default-browser button). *As built:*
  blob/data downloads report through `on_notice` (R9); a link counts as
  clicked only right after a real gesture, and download responses are
  gated too (both below).
- **R9 (carry to T6):** add an unthrottled user-action entry (e.g.
  link_routing::open_external_now) for the "Open in default browser"
  button and typed non-web address-bar input; rate limiter stays for
  page-originated opens. DOWNLOAD_UNSUPPORTED and similar non-load
  failures render as a transient notice, not the full-page error. — cost
  if wrong: button silently dead while a page spams.
- **R10** (Task 6 review: an uncoalesced fsync'd 3-file save on every
  title/URL change, on the UI thread): coalesce to one trailing save ~1s
  after last browser change; immediate saves stay for open/close/move/quit
  — overrides brief's save-per-event — cost: up to 1s of title/url lost on
  crash. *As built:* after the final review only URL changes schedule
  that save (below).
- **R11 (T7 Important 2):** failure/error toasts count as an overlay while
  visible (rare, must be seen); info toasts, tooltips, drag previews and
  the conductor stay under the page (conductor can be dragged off the
  page; hiding the page whenever he speaks would blank it constantly) —
  cost if wrong: conductor bubble unseen while over a page; mention to
  user.

### CodeRabbit review (PR #28)

- **Page-internal URLs:** `about:`, `data:` and `blob:` are never handed to
  the system, so "Open in default browser" can't launch a page's own
  content (8a65cae).
- **Real gestures:** a link counts as clicked only within 1 s of a real
  mouse or Return/Space gesture on that page, since WebKit reports a
  script's `a.click()` as a link activation too (62fbab5).
- **Stale failures:** a load failure carries its navigation number, so one
  from an earlier navigation can't replace the new page with an error
  (10a85fb).
- **Address bar:** leaving it puts the page's current URL back, after a
  redirect or `pushState` it missed while focused (0944354).
- **New windows:** a page's `target=_blank`/`window.open` always opens a
  new tab, even onto a URL already open; §1's focus-the-existing-tab rule
  is for app opens only (b06a9ab). The request survives its view being
  dropped by a tab switch (2c745cb).
- **Eviction:** a pending debounced save runs before a pane is evicted, so
  its latest URL and title aren't lost (a941e88).
- **Routed links:** when the workspace is gone, every queued routed link
  opens in the default browser, not just the current one (d23d77c).
- **Maximized tools panel:** an open into the selected worktree (Ports'
  Open, a routed link) restores the center so the page shows (a6c7976).
- **Toasts:** failure toasts are marked by their producer, not by matching
  words, so an info toast can't hide the page (d962639).

### Final whole-branch review

- **Popups:** `javaScriptCanOpenWindowsAutomatically` is off (macOS
  defaults it on), and a page opens at most one tab per second; §3's
  `window.open` → new tab now needs a user gesture.
- **zed context menus (open):** the Files/Git panels' menus can flip over
  the center column and draw under the page. Counting live
  `ui::ContextMenu` entities as an overlay was tried and reverted: at zed
  e8bc16a a parent dismissed with a submenu open (the Git panel's "Copy
  Tag") leaks with it, which would hide every page until restart.
- **Routed hosts:** also `0.0.0.0` and `[::]` (what dev servers print when
  bound to every interface), Tailscale's IPv6 range `fd7a:115c:a1e0::/48`,
  names ending in the root dot (`localhost.`) and IPv4-mapped IPv6
  addresses; the address bar's http default follows.
- **Keyboard on activation:** activating a tab with a page (a click,
  ⌘1..9, an open, the restored active tab) gives the page the keyboard
  with no click; a blank tab still focuses its address bar.
- **Download responses:** a server-marked download leaves only if the app
  started the navigation that fetched it (read as no source frame, an
  assumption not yet observed), or it followed a real gesture on the page
  within 1 s; a script's own navigation to an attachment is ignored.
- **Saves:** a title-only change relabels the tab but saves nothing (an
  animated title would save forever); the title rides along with the
  next save.
