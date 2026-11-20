# Browser Tab, Link Routing and New-Tab Menu Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a WebKit browser tab to openOrchestra, route local/dev links into it, reorganize the ＋ menu, and give every tab kind a colored icon and a title-case label.

**Architecture:** The web page is a native WebKit view placed over the tab body, positioned each frame from a gpui element's bounds. A thin `Platform` wrapper intercepts gpui's `open_url`, and routes local/dev `http(s)` URLs through a channel to the workspace, which opens or focuses a browser tab. Browser tabs persist in a new sidecar file beside `panes.json`, so older builds never see them.

**Tech Stack:** Rust, gpui/zed (pinned rev `e8bc16a`), WebKit via `wry` or `objc2-web-kit` (decided in Task 5), `url` 2.5 for host parsing.

**Spec:** `docs/superpowers/specs/2026-11-19-browser-tab-design.md`

## Global Constraints

- Work in a **git worktree branched from `master` after the other session's agent-reorder work is committed** (`Prefs::agent_order`, `agent_catalog` honoring it). Don't start while `master` has those files uncommitted.
- Build and test from `crates/harness-app` (`cd crates/harness-app && cargo test`). Root `cargo` skips this crate and reports a false green.
- **Never run `cargo fmt`** in `harness-app`: it rewrites ~65 unrelated files.
- Each user-visible commit adds a line under `## Unreleased` in `CHANGELOG.md`, in Added / Changed / Fixed.
- Visual checks run in an isolated scratch `.app` with its own `HOME`, and with `TMUX` cleared plus its own `TMUX_TMPDIR`. Never touch the user's running Dev or installed app; both share `~/.openorchestra` and one tmux server.
- Browser tab records must never be written to `panes.json`, and never to `review-tabs.json` (the installed 1.3.0 build reads both).
- The DMG must not grow: WebKit is the system framework, no bundled engine.
- Routed hosts (exact): `localhost`, `*.localhost`, `127.0.0.0/8`, `::1`, `*.local`, `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`, `*.ts.net`, `100.64.0.0/10`; scheme `http` or `https` only.
- Kind colors: Shell `#e5c07b`, Editor/file `#56b6c2`, Diff `#e06c9f`, Browser `#61afef`.
- Tab labels: Shell, Editor, Diff, `Browser · <title>`; agents Claude, Codex, OpenCode, Kimi, Pi, Grok; custom agents keep their slug. The ＋ menu keeps full names ("Claude Code").
- Settings copy, verbatim. Title: **Open links in a browser tab**. Hint: "Links to local addresses (localhost, your network, Tailscale) and ports you open from this project or task open in a browser tab. Other links always use your default browser. Turn off to send everything to your default browser."

## Review Focus

1. **Look-alike and tricky hosts.** `http://localhost.evil.com`, `http://10.0.0.1.evil.com`, `http://evil.com@localhost/` (routed: the host is localhost), `http://localhost@evil.com/` (not routed), `HTTP://LOCALHOST:3000` (routed), `http://[::1]:8080` (routed). They must parse with `url::Url`, never string prefixes. The tests go in Task 3.
2. **A dev server that isn't up yet.** Opening `localhost:3000` before the server listens shows an in-tab error with Reload, never a blank tab or an external fallback. The check is in Task 6.
3. **Switching worktree or tab, or opening an overlay, while a page is visible.** The native view must hide in the same frame, with no page left floating over another worktree or covering the ＋ menu. A pure visibility function is tested in Task 7.
4. **The same URL opened twice** (a double ⌘-click, or a port opened again) focuses the existing tab instead of stacking duplicates. The test is in Task 8.
5. **The setting flipped while the app runs.** It takes effect on the next click, without a restart, because the router's flag updates when prefs save. The test is in Task 3.

---

### Task 0: Worktree and spec

- [ ] **Step 1:** Confirm the agent-reorder work is committed: `git -C /Users/omar/projects/omar-harness log --oneline -5` shows it, and `git status` no longer lists `agents.rs`, `center.rs` or `settings.rs`. If it isn't committed, stop and report back.
- [ ] **Step 2:** Use superpowers:using-git-worktrees to create branch `feat/browser-tab` from `master`.
- [ ] **Step 3:** Copy the spec and this plan into the worktree if they aren't tracked yet, then commit:
  `git add docs/superpowers/specs/2026-11-19-browser-tab-design.md docs/superpowers/plans/2026-11-19-browser-tab.md && git commit -m "docs: browser tab design and plan"`
- [ ] **Step 4:** Baseline: `cd crates/harness-app && cargo test 2>&1 | tail -3` passes. Note the count.

---

### Task 1: Kind icons, colors and title-case tab labels

**Files:**
- Create: `crates/harness-app/src/workspace/tab_look.rs` (register it as `mod tab_look;` in `workspace.rs`)
- Modify: `crates/harness-app/src/workspace/ui/center.rs` (tab strip, `render_tab` area around the `agent_icon` / `{ }` children)
- Modify: `crates/harness-app/src/workspace/ui/settings_remote.rs:684` (`display_title`) and its callers, so the phone UI shows the same labels

**Interfaces:**
- Produces:
  - `pub(crate) enum LookKind { Shell, Editor, Diff, Browser }`
  - `pub(crate) fn kind_look(kind: LookKind) -> (::ui::IconName, Rgba)`
  - `pub(crate) fn kind_icon(kind: LookKind, size: f32) -> AnyElement`, which draws the colored svg like `agent_icon`
  - `pub(crate) fn agent_short_name(kind: &AgentKind) -> String`
  - `pub(crate) fn tab_label(tab: &Tab) -> String`. It covers File and ReviewOverview, which use the stored `tab.title`; Browser, which uses `browser_label(..)`; and agent tabs.
  - `pub(crate) fn browser_label(page_title: Option<&str>, host: Option<&str>) -> String`

- [ ] **Step 1: Write failing tests** in `tab_look.rs`:

```rust
#[test] fn agent_tabs_use_short_title_case_names() {
    assert_eq!(agent_short_name(&AgentKind::ClaudeCode), "Claude");
    assert_eq!(agent_short_name(&AgentKind::Pi), "Pi");
    assert_eq!(agent_short_name(&AgentKind::OpenCode), "OpenCode");
    assert_eq!(agent_short_name(&AgentKind::Other("cursor-agent".into())), "cursor-agent");
}
#[test] fn browser_label_prefers_title_then_host() {
    assert_eq!(browser_label(Some("Vite App"), Some("localhost")), "Browser · Vite App");
    assert_eq!(browser_label(Some("  "), Some("localhost")), "Browser · localhost");
    assert_eq!(browser_label(None, None), "Browser");
}
#[test] fn kind_colors_match_spec() {
    assert_eq!(kind_look(LookKind::Shell).1, rgb(0xe5c07b));
    assert_eq!(kind_look(LookKind::Editor).1, rgb(0x56b6c2));
    assert_eq!(kind_look(LookKind::Diff).1, rgb(0xe06c9f));
    assert_eq!(kind_look(LookKind::Browser).1, rgb(0x61afef));
}
```

- [ ] **Step 2:** `cargo test tab_look` fails to compile because the functions don't exist yet.
- [ ] **Step 3: Implement.** Icons are the closest zed `IconName`s at this rev (check `ui::IconName`): Terminal, Code, a git/diff icon, Globe. `tab_label` ignores stored glyph-prefixed titles (`❯_ shell`, `{ } editor`, `⑂ diff`) for Shell, Editor and Diff. In the strip, every Shell, Editor, File, Diff and Browser tab gets a 13 px `kind_icon` before the label in the same slot agent tabs use (`.mr_1().size(px(14.0))`). File tabs lose the `{ }` text. Agent tabs keep `agent_icon`. Review Overview keeps its icon and spinner. The running spinner stays. Replace `.child(tab.title.clone())` with `.child(tab_label(tab))`, and make the remote `display_title` call sites use `tab_label` too.
- [ ] **Step 4:** `cargo test` passes, including the existing remote tests. Update any expectation in `settings_remote.rs`/`remote.rs` tests that asserted lowercase titles, and say so in the commit message.
- [ ] **Step 5:** Add a CHANGELOG line under Changed: "Tabs show a colored icon for every kind and title-case names (Shell, Editor, Claude, Codex, …)." Then commit: `feat(tabs): colored kind icons and title-case labels`.

---

### Task 2: ＋ menu reorganization

**Files:**
- Modify: `crates/harness-app/src/workspace/ui/center.rs` (`render_tab_menu`, `tab_menu_item`, `tab_menu_item_agent`)
- Modify: `crates/harness-app/src/workspace/tab_look.rs` (pure entry list)

**Interfaces:**
- Consumes: `kind_icon`, `LookKind` (Task 1); `agent_catalog`, `agent_icon`, `agent_display` (existing)
- Produces:
  - `pub(crate) enum MenuEntry { Kind(TabKind), Separator(&'static str), Agent(AgentKind), PiChat }`
  - `pub(crate) fn tab_menu_entries(prefs: &Prefs, diff_in_tab: bool, browser: bool, pi_chat: bool) -> Vec<MenuEntry>`

- [ ] **Step 1: Write the failing test.**

```rust
#[test] fn menu_puts_kinds_then_agents_in_settings_order_then_pi_chat() {
    let mut prefs = Prefs::default();
    prefs.agent_order = vec!["claude-code".into(), "codex".into()];
    for k in ["pi","opencode","kimi"] { prefs.agents.entry(k.into()).or_default().disabled = true; }
    let e = tab_menu_entries(&prefs, true, true, true);
    assert_eq!(e, vec![
        MenuEntry::Kind(TabKind::Shell), MenuEntry::Kind(TabKind::Editor),
        MenuEntry::Kind(TabKind::Diff), MenuEntry::Kind(TabKind::Browser),
        MenuEntry::Separator("Agents"),
        MenuEntry::Agent(AgentKind::ClaudeCode), MenuEntry::Agent(AgentKind::Codex),
        MenuEntry::Agent(AgentKind::Grok), MenuEntry::PiChat,
    ]);
    let e = tab_menu_entries(&prefs, false, true, false);
    assert!(!e.contains(&MenuEntry::Kind(TabKind::Diff)) && !e.contains(&MenuEntry::PiChat));
}
```

`TabKind::Browser` doesn't exist until Task 6. Add the variant now, with `open_tab` ignoring it, and pass `browser: false` from the renderer until Task 6 flips it. The assertion above uses `true`. Derive `PartialEq, Debug` on `MenuEntry`. `TabKind` (`workspace.rs:172`) derives only `Clone, PartialEq`, so add `Debug` to it.

- [ ] **Step 2:** Run it and see it fail.
- [ ] **Step 3: Implement** `tab_menu_entries`, then render from it with **one row builder**: a fixed 16 px icon box, `gap_2`, the label, and a right-aligned shortcut. Labels and shortcuts are: Shell `⌘T`, Editor `⌘E`, Diff and Browser with none, agents `agent_display(..)`, and pi chat `⌘⇧C` with the Pi logo. The separator is a muted `text_xs` label "Agents" over a 1 px `hx.border` rule, and isn't clickable. Keep the mouse-down swallow on the container. pi chat stays gated exactly as today (a worktree is selected and pi is enabled).
- [ ] **Step 4:** `cargo test` passes. In the scratch app, open the ＋ menu and confirm every label starts at the same x (compare with the user's screenshot, where they don't).
- [ ] **Step 5:** Add a CHANGELOG line under Changed: "The ＋ menu lists Shell and Editor first, then your agents in Settings order, with aligned rows; Terminal is now Shell." Then commit: `feat(tabs): reorganize the new-tab menu`.

---

### Task 3: Link decision and the setting

**Files:**
- Create: `crates/harness-app/src/link_routing.rs` (add `mod link_routing;` in `main.rs`)
- Modify: `crates/harness-app/Cargo.toml`: add `url = "2.5"` (already in the lockfile at 2.5.7)
- Modify: `crates/harness-app/src/workspace/prefs.rs`: add `#[serde(default = "default_true")] pub(crate) links_in_browser_tab: bool`, set to `true` in `Default`
- Modify: `crates/harness-app/src/workspace/ui/settings.rs`: a General-section `toggle_row`, placed after "Terminals on quit"

**Interfaces:**
- Produces:
  - `pub fn is_local_url(url: &str) -> bool`
  - `pub fn should_route(url: &str, enabled: bool) -> bool` (`enabled && is_local_url(url)`)
  - `pub fn set_routing_enabled(on: bool)` and `pub fn routing_enabled() -> bool`, backed by a `static AtomicBool`
  - Call `set_routing_enabled(prefs.links_in_browser_tab)` at prefs load and wherever the toggle saves.

- [ ] **Step 1: Write the failing tests.**

```rust
#[test] fn routes_local_and_dev_hosts() {
    for u in ["http://localhost:3000","HTTP://LOCALHOST:3000/x","https://app.localhost",
              "http://127.0.0.1:8080","http://127.4.5.6","http://[::1]:5173","http://printer.local",
              "http://10.1.2.3","http://172.16.0.1","http://172.31.255.255","http://192.168.1.10:3000",
              "https://mac.tail1234.ts.net","http://100.64.0.1","http://100.127.255.255",
              "http://evil.com@localhost/"] { assert!(is_local_url(u), "{u}"); }
}
#[test] fn leaves_public_and_look_alike_hosts_external() {
    for u in ["https://github.com/omarpr/openorchestra","http://localhost.evil.com","http://10.0.0.1.evil.com",
              "http://localhost@evil.com/","http://172.32.0.1","http://100.128.0.1","https://ts.net.evil.com",
              "file:///etc/hosts","mailto:a@localhost","zed://x","not a url"] { assert!(!is_local_url(u), "{u}"); }
}
#[test] fn routing_follows_the_setting_at_call_time() {
    set_routing_enabled(false); assert!(!should_route("http://localhost:3000", routing_enabled()));
    set_routing_enabled(true);  assert!(should_route("http://localhost:3000", routing_enabled()));
}
#[test] fn prefs_default_routes_links_and_old_files_get_the_default() {
    assert!(Prefs::default().links_in_browser_tab);
    let p: Prefs = serde_json::from_str("{}").unwrap();
    assert!(p.links_in_browser_tab);
}
```

(The prefs test goes in `prefs.rs`'s test module, or in a new one there.)

- [ ] **Step 2:** Run them and see them fail.
- [ ] **Step 3: Implement.** Parse with `url::Url::parse`. Use `host()`, matching `Host::Domain` (lowercased by `url`), `Host::Ipv4` and `Host::Ipv6`. Match the domain suffix on a label boundary (`== "localhost" || ends_with(".localhost")`). Wire the toggle (Global Constraints has the copy), save with `save_prefs_to`, and update the atomic.
- [ ] **Step 4:** `cargo test link_routing prefs` passes.
- [ ] **Step 5:** Commit: `feat(links): local-address routing decision and setting`. Hold the CHANGELOG line for Task 8, when routing becomes user-visible.

---

### Task 4: Browser tab persistence (sidecar)

**Files:**
- Modify: `crates/harness-app/src/panes_state.rs`

**Interfaces:**
- Produces: `TabRecordKind::Browser { url: String, #[serde(default)] title: Option<String> }`, persisted only in `browser-tabs.json` (`const BROWSER_TABS_FILE`).
- Generalize `legacy_and_reviews` / `splice_review_tabs` / `ReviewTabsFile` into one sidecar mechanism taking a predicate and a file name. The review file stays byte-compatible: same name, same JSON shape. Browser records go only to the browser file. Neither file ever holds the other's kind.

- [ ] **Step 1: Write the failing tests**, modeled on `review_overview_stays_out_of_the_file_an_older_build_reads` (`panes_state.rs:364`):
  - `browser_tabs_stay_out_of_panes_and_review_files`: save a state holding a shell, a review overview and a browser tab (active). Then `panes.json` contains neither `"Browser"` nor `"ReviewOverview"`, `review-tabs.json` doesn't contain `"Browser"`, and `browser-tabs.json` contains the URL.
  - `browser_tabs_restore_in_place_and_keep_active`: after `load`, the tab order is identical, `active` is the browser tab's id, and the title survives.
  - `an_older_build_save_keeps_browser_tabs`: write a `panes.json` the way an older build would (shell only, `next_tab_id` bumped). `load` then still splices the browser tab back from the sidecar.
  - `no_browser_tabs_removes_the_sidecar`.
- [ ] **Step 2:** Run them and see them fail.
- [ ] **Step 3:** Implement the generalization. Add `TabContent::Browser` mapping in `Tab::kind_tag` (`workspace.rs:336`) once Task 6 adds the variant. Until then the record kind exists only in `panes_state`.
- [ ] **Step 4:** All `panes_state` tests pass, old and new.
- [ ] **Step 5:** Commit: `feat(browser): persist browser tabs beside panes.json`.

---

### Task 5: Spike, then the native web view wrapper

**Files:**
- Create: `crates/harness-app/src/browser_pane/webview.rs` (create `browser_pane/mod.rs` with `mod webview;`, and add `mod browser_pane;` in `main.rs`)
- Modify: `crates/harness-app/Cargo.toml`

**Interfaces:**
- Produces a macOS-only `pub(crate) struct NativeWebView` with:
  - `fn new(window: &Window, url: &str, events: NativeEvents) -> anyhow::Result<Self>`, which attaches to the window's view through `HasWindowHandle` and starts hidden
  - `fn set_frame(&self, bounds: Bounds<Pixels>, window: &Window)`, taking bounds in gpui window coordinates (top-left origin) and flipping them if the parent view isn't flipped
  - `fn set_visible(&self, visible: bool)`, `fn load(&self, url: &str)`, `fn back(&self)`, `fn forward(&self)`, `fn reload(&self)`, `fn stop(&self)`, `fn focus(&self)`
  - `fn url(&self) -> Option<String>`, `fn can_go_back(&self) -> bool`, `fn can_go_forward(&self) -> bool`
  - `Drop` removes it from the superview.
  - `pub(crate) struct NativeEvents`, with boxed callbacks posted to the main thread: `on_title(String)`, `on_url(String)`, `on_loading(bool)`, `on_failed(String)`, and `on_new_window(String)` for `target=_blank` and `window.open`. Its last member is a navigation policy: `http`/`https` load in the page, and downloads and every other scheme are handed to the default browser through `link_routing::open_external(url)`. Add that tiny helper to `link_routing.rs`; it calls `/usr/bin/open` or the real platform, which Task 8 provides. Until Task 8, use `open`.
- Set `isInspectable = true` where the OS supports it (macOS 13.3+). Use one shared persistent default data store.

- [ ] **Step 1: Spike (throwaway, on a scratch branch).** Try `wry` (`WebViewBuilder::build_as_child(&window)` with gpui's `Window: HasWindowHandle`). Check `cargo tree -d -i objc2`: if `wry` pulls in a second `objc2` 0.6.x copy or fails to build universal, use `objc2-web-kit` (a `0.3` version matching `objc2` 0.6) instead. Show `http://localhost:<some port>` and follow a resize. Record the choice and why in a comment at the top of `webview.rs`.
- [ ] **Step 2:** Implement `NativeWebView` with the chosen library. Its only test is a debug-build `#[test]` that builds a `NativeEvents` and exercises the pure frame-flip helper `fn flip_y(bounds_y: f32, height: f32, parent_height: f32) -> f32` with three cases. AppKit itself is checked manually.
- [ ] **Step 3: Manual check** in the scratch app, via a temporary debug action you remove before committing. The view appears inside a given rect, tracks a window resize, hides and shows, and leaves no view behind after drop.
- [ ] **Step 4:** `cargo test` passes and `cargo build --release` passes for the universal targets the Makefile uses.
- [ ] **Step 5:** Commit: `feat(browser): native WebKit view wrapper`.

---

### Task 6: Browser tab

**Files:**
- Create: `crates/harness-app/src/browser_pane/mod.rs`: the `BrowserPane` entity and its address bar
- Modify: `crates/harness-app/src/workspace.rs`:
  - `TabKind::Browser` already exists (Task 2); make `open_tab` handle it
  - add `TabContent::Browser(Entity<BrowserPane>)`
  - map `TabContent::Browser(p)` to `K::Browser { url, title }` in `kind_tag`
- Modify: `crates/harness-app/src/workspace/state/tabs.rs`:
  - `open_tab` handles Browser
  - `restore_tabs` (`:629`) restores Browser records
  - add a new `open_browser_tab`
- Modify: `crates/harness-app/src/workspace/ui/center.rs`: the tab body render, the strip label via `tab_label`, and passing `browser: true` to `tab_menu_entries`

**Interfaces:**
- Consumes: `NativeWebView`, `NativeEvents` (Task 5); `browser_label`, `kind_icon(LookKind::Browser, ..)` (Task 1); `TabRecordKind::Browser` (Task 4); `link_routing::open_external` (Task 5)
- Produces:
  - `pub(crate) fn normalize_address(input: &str) -> Option<String>`
  - `impl BrowserPane`, with these methods:
    - `fn new(url: Option<String>, title: Option<String>, window, cx) -> Self`
    - `fn url(&self) -> Option<&str>` and `fn title(&self) -> Option<&str>`
    - `fn navigate(&mut self, url: String, cx)`
    - `fn set_shown(&mut self, shown: bool, window, cx)`: `true` creates the native view, `false` drops it and keeps url and title
  - `HarnessWorkspace::open_browser_tab(&mut self, url: Option<String>, window, cx) -> u64`. It focuses an existing browser tab in the active pane whose URL equals `url`, otherwise inserts a new tab after the active tab. It returns the tab id.

- [ ] **Step 1: Write the failing tests.**

```rust
#[test] fn address_bar_normalizes_input() {
    assert_eq!(normalize_address("localhost:3000").as_deref(), Some("http://localhost:3000/"));
    assert_eq!(normalize_address("192.168.1.5:8080/x").as_deref(), Some("http://192.168.1.5:8080/x"));
    assert_eq!(normalize_address("myhost:4000").as_deref(), Some("http://myhost:4000/"));
    assert_eq!(normalize_address("example.com").as_deref(), Some("https://example.com/"));
    assert_eq!(normalize_address("https://a.b/c?d").as_deref(), Some("https://a.b/c?d"));
    assert_eq!(normalize_address("  ").as_deref(), None);
    assert_eq!(normalize_address("javascript:alert(1)").as_deref(), None);
}
```

There's also a workspace-level test, in whatever style `state/tabs.rs` tests use today (follow the nearest existing test that opens tabs without a window; if none exists, test the pure lookup instead). It is `fn find_browser_tab(tabs: &[TabRecord or Tab], url: &str) -> Option<u64>`: it returns the existing id for an equal URL and `None` for a different path.

- [ ] **Step 2:** Run them and see them fail.
- [ ] **Step 3: Implement.**
  - **Address bar** (gpui, ~32 px tall): back, forward, reload/stop, the URL field (a single-line zed `Editor`, like the comment popover's), and an **Open in default browser** button that calls `link_routing::open_external(current_url)`. Enter calls `normalize_address`, then `navigate`.
  - **New from the ＋ menu:** a blank page with the URL field focused.
  - **Tab body:** a gpui element filling the space below the bar. In prepaint it calls `set_frame(bounds)`.
  - **Tab label:** `browser_label(title, host)`.
  - **Events:**
    - `on_title`/`on_url` update the tab and call `save_panes()`.
    - `on_new_window` calls `open_browser_tab(Some(url))` in the same pane.
    - `on_failed` shows an in-tab message, "Couldn't load <url>: <reason>", with a Reload button, and hides the native view while it shows.
  - **Showing and hiding:** `set_shown(true)` only for the active tab of the visible pane. Every other path calls `set_shown(false)`: tab switch, worktree switch, tab close, window hidden or minimized.
- [ ] **Step 4:** `cargo test` passes. Then check manually in the scratch app:
  - ＋ → Browser opens blank.
  - Typing `localhost:<port>` loads the page, and the label becomes `Browser · <title>`.
  - Back, forward and reload work.
  - With nothing listening on the port, the error and Reload show.
  - A `target=_blank` link opens a second browser tab.
  - Restarting the app restores the tab.
  - Switching tab or worktree leaves no view behind.
- [ ] **Step 5:** Add a CHANGELOG line under Added: "**Browser** tabs (＋ → Browser): a WebKit page next to your shells and agents, with back, forward, reload and Open in default browser." Then commit: `feat(browser): browser tab`.

---

### Task 7: Overlays and shortcuts over a page

**Files:**
- Modify: `crates/harness-app/src/browser_pane/mod.rs`, `crates/harness-app/src/workspace/ui/center.rs` (or wherever overlays are tracked on `HarnessWorkspace`), `crates/harness-app/src/browser_pane/webview.rs`

**Interfaces:**
- Produces `pub(crate) fn web_view_visible(tab_active: bool, pane_visible: bool, overlay_open: bool, load_failed: bool) -> bool`. It's pure, and `BrowserPane` uses it to drive `set_visible`.
- `HarnessWorkspace::overlay_open(&self) -> bool` is true whenever any of these is open: `tab_menu`, `tab_context_menu`, the comment popover, Settings, a prompt/modal, the command palette. List every overlay field you find, and add new ones to it.

- [ ] **Step 1: Write the failing test** `web_view_hidden_unless_active_visible_and_unobstructed`. It is a table over all 16 combinations, where only `(true, true, false, false)` is visible.
- [ ] **Step 2:** Run it and see it fail.
- [ ] **Step 3: Implement.**
  - **Overlays:** wire `overlay_open()` into the pane on every render.
  - **Shortcuts:** while the page has focus, the app's shortcuts reach gpui: ⌘T, ⌘E, ⌘W, ⌘⇧C, tab and worktree switching, and every global binding in `shortcuts.rs`. ⌘C/⌘V/⌘X/⌘A/⌘Z stay with the page. The mechanism is yours: override `performKeyEquivalent:` on the web view's container, or use a local `NSEvent` monitor that hands non-editing ⌘-chords to gpui. Document the choice in `webview.rs`.
  - **Esc** in the URL field focuses the page.
- [ ] **Step 4:** `cargo test` passes. Then check manually, with a page focused:
  - Open the ＋ menu, the tab right-click menu, Settings, and a quit or close prompt over it. Each one is fully visible and the page reappears after.
  - ⌘T opens a Shell, ⌘W closes the browser tab, and ⌘C/⌘V work in a page text field.
- [ ] **Step 5:** Commit: `feat(browser): keep menus and shortcuts working over a page`.

---

### Task 8: Route links and ports into browser tabs

**Files:**
- Create: `crates/harness-app/src/link_routing/platform.rs` (convert `link_routing.rs` into `link_routing/mod.rs`)
- Modify: `crates/harness-app/src/main.rs:155`: start with `gpui::Application::with_platform(Rc::new(LinkRoutingPlatform::new(gpui_platform::current_platform(false))))`, then `.with_assets(..)` as today
- Modify: `crates/harness-app/src/workspace/construct.rs` (or wherever the workspace spawns its long-lived tasks): drain routed URLs into `open_browser_tab(Some(url))`
- Modify: `crates/harness-app/src/workspace/ui/ports_panel.rs:495-501`: the Open button
- Modify: `crates/harness-app/src/link_routing/mod.rs`: `open_external` now calls the inner platform

**Interfaces:**
- Consumes: `should_route`, `routing_enabled` (Task 3); `open_browser_tab` (Task 6)
- Produces:
  - `pub struct LinkRoutingPlatform { inner: Rc<dyn Platform> }`, which implements `gpui::Platform` by forwarding every method. `open_url(url)`: if `should_route(url, routing_enabled())`, call `routed_sender().send(url)`, otherwise `inner.open_url(url)`.
  - `pub fn take_routed_receiver() -> Option<futures::channel::mpsc::UnboundedReceiver<String>>`, which can be taken once
  - `pub fn open_external(url: &str)`, which always goes to the inner platform. It's stored in a thread-local `Rc<dyn Platform>` set by `LinkRoutingPlatform::new`.

- [ ] **Step 1: Write the failing tests** with a fake inner platform. Reuse the `StubPlatform` in `main.rs` tests (`main.rs:677`), extending it to record `open_url` calls:
  - `routed_urls_go_to_the_channel_not_the_os`: with the setting on, `open_url("http://localhost:3000")` lands on the receiver, and the stub records nothing.
  - `public_urls_and_disabled_routing_go_to_the_os`: `https://github.com/...` is recorded by the stub. With the setting off, `localhost` is recorded by the stub too.
  - `open_external_bypasses_routing`.
- [ ] **Step 2:** Run them and see them fail.
- [ ] **Step 3: Implement.**
  - **The wrapper:** every `Platform` method forwards. A trait change at a future zed bump must fail the build, so write no blanket defaults that would hide it.
  - **The drain:** spawned once on the main thread, it opens in the active worktree. With no worktree selected, it falls back to `open_external`.
  - **Ports panel, "task" section:** if `routing_enabled()`, call `open_browser_tab(Some(url))` directly. Otherwise use `cx.open_url`.
  - **Ports panel, "other" section:** `open_external(&url)`.
  - **Repoint the Task 5 and Task 6 callers** of `open_external` to this version.
- [ ] **Step 4:** `cargo test` passes. Then check manually in the scratch app:
  - In a Shell, `echo http://localhost:3000` and ⌘-click: a browser tab opens. ⌘-click again: the same tab is focused.
  - `echo https://github.com` and ⌘-click: Safari or Chrome opens.
  - Ports panel Open in the task section opens a tab, and in the Other section opens the default browser.
  - With the setting off, everything opens externally.
  - About › View on GitHub opens externally.
- [ ] **Step 5:** Add a CHANGELOG line under Added: "Links to local addresses (localhost, your network, Tailscale) and ports opened from the current project or task open in a browser tab. Other links still use your default browser. Change it in Settings › General." Then commit: `feat(links): open local links and ports in browser tabs`.

---

### Task 9: Whole-feature verification

- [ ] **Step 1:** Run `cd crates/harness-app && cargo test 2>&1 | tail -3`. It passes, and the count is the baseline plus the new tests.
- [ ] **Step 2: Two builds sharing state.** Run the installed 1.3.0 build and the scratch build against the same isolated HOME. Open browser tabs and review tabs in the new build, then quit it. Open, close and quit the old build. Relaunch the new build: every browser and review tab is restored.
- [ ] **Step 3: Run the spec's manual list once more, end to end.** It's §7 "Manual" in the design doc.
- [ ] **Step 4:** Read `git diff master --stat` for stray changes: debug actions, formatting churn, or `cargo fmt` fallout.
