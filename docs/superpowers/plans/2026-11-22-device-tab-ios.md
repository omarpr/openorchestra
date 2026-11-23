# Device tab (iOS simulators) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A watch-only **Device** tab in the right-hand tools panel that shows a live view of any booted iOS simulator, picked from a list, smooth via Apple's private simulator display API, with a screenshot fallback.

**Architecture:** A pure `devices` module (simctl JSON parsing, device choice, a latest-frame slot, the tab's state) plus two macOS-only parts: `sim_display` (dlopen CoreSimulator + SimulatorKit, register screen callbacks, get `IOSurface`s) and `convert` (BGRA `IOSurface` → 420f `CVPixelBuffer` for gpui's `surface()`). The UI is a separate gpui entity (`DeviceScreen`) embedded with `.cached(..)` so per-frame redraws don't re-render the workspace. Device listing and the fallback both shell out to `xcrun simctl`, polled only while the tab is on screen.

**Tech Stack:** Rust 2024, gpui (zed rev e8bc16a), objc2 0.6 family (`objc2`, `objc2-foundation`, `block2`, `dispatch2`, `objc2-core-foundation`, `objc2-io-surface` 0.3.2, `objc2-core-video` 0.3.2, new: `objc2-video-toolbox` 0.3.2), `serde_json`, `xcrun simctl`.

**Spec:** `docs/superpowers/specs/2026-11-22-device-tab-ios-design.md`

## Global Constraints

- Build, test and clippy harness-app **from `crates/harness-app`** (`cd crates/harness-app && cargo test`); the root workspace silently skips it.
- **Never run `cargo fmt`** in harness-app (it rewrites ~65 unrelated files). Format only the lines you touch, by hand.
- Work on the current branch. No worktrees or new branches.
- Every user-visible commit adds a line under `CHANGELOG.md` `## Unreleased` (Added/Changed/Fixed), in that commit.
- Windows must still build: everything touching CoreSimulator, SimulatorKit, IOSurface, CoreVideo or VideoToolbox is `#[cfg(target_os = "macos")]`. On Windows the Device tab is not in the strip.
- Watch-only. Nothing in this plan may boot, shut down, resize, rotate, focus or send input to a simulator.
- No work while the tab is hidden: no frame callbacks registered, no `simctl` polling, unless `self.prefs.diff_open && self.prefs.tool_tab == ToolTab::Device` **and** the window isn't minimised or the app hidden. For the last part reuse `browser_pane::webview::window_is_minimized_or_hidden(window)` (`src/browser_pane/webview.rs:3233`, macOS). gpui's own visibility can't tell minimised from merely covered.
- **Test device:** every live check (spike, ignored tests, Task 7) runs on a simulator the run creates and boots itself, addressed by UDID, never `booted`, which may be an agent's device: `UDID=$(xcrun simctl create oo-device-test "iPhone 18 Pro") && xcrun simctl boot $UDID`, then `xcrun simctl delete $UDID` when done.
- **Motion source** (there's no input route): `for i in $(seq 10); do xcrun simctl ui $UDID appearance dark; sleep 0.5; xcrun simctl ui $UDID appearance light; sleep 0.5; done`. Each toggle redraws the whole screen. Gate on "frames follow screen changes", not on a frame rate.
- A missing private class/selector must never panic or raise an Objective-C exception: check with `NSClassFromString` / `respondsToSelector:` first and return an error.
- gpui's surface renderer only accepts `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange` (420f). Never hand it BGRA or 420v.
- Frameworks: CoreSimulator at `/Library/Developer/PrivateFrameworks/CoreSimulator.framework`; SimulatorKit at `$(xcode-select -p)/../SharedFrameworks/SimulatorKit.framework`.
- Default device set only; main display only.
- Visual checks use an isolated scratch `.app` with its own bundle id and a wrapper exe that sets `HOME`, a short `TMUX_TMPDIR` under `/tmp`, and unsets `TMUX`. Never touch the user's running `openOrchestra Dev`.

## Review Focus

1. **Settings written by this build, read by an older one.** `~/.openorchestra/prefs.json` is shared by dev and release builds. Today an unknown `tool_tab` value makes `load_prefs` drop *every* pref (including agent launch overrides). An older build reading a file saved with the Device tab open must keep its other prefs. Pinned in Task 2.
2. **The watched simulator shuts down or is erased mid-stream.** The tab should drop to "Simulator shut down", unregister callbacks and not crash on a dead `IOSurface`. Pinned in Task 4 (state machine) and checked by hand in Task 7.
3. **Rotation or display-size change while watching** (the agent rotates the device). The surfaces-changed callback hands over a new size; the view must re-fit, not stretch or keep the old buffer pool. Pinned in Task 5 (converter rebuilds its pool on a size change).
4. **Two Xcodes installed / `xcode-select` pointing at a CLT-only path.** No SimulatorKit there → fallback with the "Reduced frame rate" note or "Needs Xcode", never a crash. Pinned in Task 3 (path resolution tests).
5. **The selected simulator disappears and another is booted.** A stale UDID falls back to the first booted device and the stream restarts on it. Pinned in Task 1 (`choose_device`).

---

### Task 0: Spike — prove the private display route (throwaway, gate)

Not product code. Don't commit the spike; commit only the findings in the spec.

**Files:**
- Create (uncommitted): `crates/harness-app/examples/sim_spike.rs` (main is `#[cfg(target_os = "macos")]`, with an empty `main` otherwise)
- Modify: `docs/superpowers/specs/2026-11-22-device-tab-ios-design.md` (add a "Spike findings" section)

- [ ] **Step 1:** Create and boot the test device (Global Constraints) headless; leave Device Hub closed. Write `sim_spike.rs`: `dlopen` both frameworks, get `SimServiceContext` for the selected developer dir, get the default device set, find the booted `SimDevice` by UDID, walk its IO ports to the main display (the `com.apple.framebuffer.display` port matching the device's screen size), find the object that responds to `registerScreenCallbacksWithUUID:callbackQueue:frameCallback:surfacesChangedCallback:propertiesChangedCallback:`, register, and log surface width/height/pixel format and a timestamp per frame callback for 15 s while the motion-source loop runs. The surfaces callback hands over two `IOSurface`s: log both and work out which is the picture (and what the other is, e.g. a mask).
- [ ] **Step 2:** Run `cd crates/harness-app && cargo run --example sim_spike -- <udid>`. Expected: a surface arrives at once; frame callbacks arrive in bursts at each appearance toggle and stop between them; `BGRA` format; size matching the device's pixel size.
- [ ] **Step 3:** Repeat with Device Hub open on the same device. Expected: both work. The device's size, orientation and focus don't change.
- [ ] **Step 4:** Sign the example with the hardened runtime (`codesign --force --options runtime --entitlements resources/openOrchestra.entitlements --sign - target/debug/examples/sim_spike`) and run it again. Expected: frameworks load and frames arrive.
- [ ] **Step 5:** Measure both drawing routes in the spike, during the motion loop and while idle, with the spike's CPU and GPU (`top -pid`, Activity Monitor's GPU column): (a) BGRA→420f with `VTPixelTransferSession`; (b) a bare `NSWindow` whose content view's `CALayer.contents` is the `IOSurface`, with `setContentsChanged` per frame.
- [ ] **Step 6: Gate.** If Step 2 or Step 4 fails, **stop and report to the user**: the plan changes (helper process, or the screenshot route as the main route). If (a) costs clearly more than (b), more than ~10% of one core during motion, stop and report: Task 5 switches to the native-layer route (a `CALayer` whose `contents` is the `IOSurface`, positioned like `browser_pane/webview.rs`).
- [ ] **Step 7:** Write "Spike findings" into the spec: the exact class and selector chain from UDID to the screen object, the callback signatures as observed, which of the two surfaces is the picture, how the main display was told apart, the CPU/GPU numbers for both routes, and the display route chosen. Commit the spec only (`docs: record the Device tab spike findings`). Delete `examples/sim_spike.rs` and the test device.

---

### Task 1: Pure device model — simctl parsing, choice, frame slot

**Files:**
- Create: `crates/harness-app/src/devices/mod.rs`
- Create: `crates/harness-app/src/devices/simctl.rs`
- Create: `crates/harness-app/src/devices/testdata/booted_one.json` (the `simctl list devices booted -j` capture, below), `booted_none.json`, `booted_two.json`
- Modify: `crates/harness-app/src/main.rs` (add `mod devices;` in the module list)
- Modify: `crates/harness-app/src/ports.rs` (`run_bounded` → `pub(crate)`)

**Interfaces:**
- Produces:
  - `pub(crate) struct DeviceInfo { pub udid: String, pub name: String, pub os: String /* "iOS 27.2" */, pub device_type: String /* "iPhone-18-Pro" */ }` (`Clone, Debug, PartialEq, Eq`)
  - `pub(crate) fn parse_booted(json: &str) -> Result<Vec<DeviceInfo>, String>`: booted devices only, sorted by `name` then `os`
  - `pub(crate) fn choose_device<'a>(booted: &'a [DeviceInfo], selected: Option<&str>) -> Option<&'a DeviceInfo>`: the selected UDID if booted, else the first
  - `pub(crate) struct FrameSlot<T>`: `new()`, `put(&self, T) -> bool` (true when the slot was empty, meaning wake the UI), `take(&self) -> Option<T>`. `Send + Sync` for `T: Send` (a `Mutex<Option<T>>`).
  - `pub(crate) fn list_booted() -> Result<Vec<DeviceInfo>, ListError>` in `simctl.rs`: runs `xcrun simctl list devices booted -j` with a 5 s timeout
  - `pub(crate) enum ListError { NoXcode, Failed(String) }`. `NoXcode` when `xcrun` is missing or reports no `simctl` (exit status ≠ 0 with "unable to find utility").
  - `pub(crate) fn screenshot_jpeg(udid: &str, dir: &Path) -> Result<Vec<u8>, String>` in `simctl.rs`: `xcrun simctl io <udid> screenshot --type=jpeg <dir>/<udid>.jpg`, then reads and removes the file. simctl does not write to stdout (`-` gives nothing, measured). 5 s timeout.

Captured input for `booted_one.json` (trim `dataPath`/`logPath` if you like; keep the shape):

```json
{"devices":{"com.apple.CoreSimulator.SimRuntime.iOS-27-2":[{"udid":"0F291397-02EB-448E-BDFF-320883371EB1","isAvailable":true,"deviceTypeIdentifier":"com.apple.CoreSimulator.SimDeviceType.iPhone-18-Pro","state":"Booted","name":"iPhone 18 Pro"}],"com.apple.CoreSimulator.SimRuntime.iOS-27-0":[]}}
```

- [ ] **Step 1: Write the failing tests** (in `devices/mod.rs` `#[cfg(test)] mod tests`):
  - `parses_one_booted_iphone`: `parse_booted(booted_one)` == `[DeviceInfo { udid: "0F291397-02EB-448E-BDFF-320883371EB1", name: "iPhone 18 Pro", os: "iOS 27.2", device_type: "iPhone-18-Pro" }]`
  - `no_booted_devices_is_an_empty_list`: `booted_none` (`{"devices":{"com.apple.CoreSimulator.SimRuntime.iOS-27-2":[]}}`) → `Ok(vec![])`
  - `a_shutdown_device_in_the_list_is_skipped`: a device with `"state":"Shutdown"` isn't returned
  - `two_devices_sort_by_name`: `booted_two` (an "iPhone 18 Pro" on iOS 27.2 and an "iPad Air 13-inch (M4)" on iOS 27.0) → iPad first, `os: "iOS 27.0"`
  - `garbage_json_is_an_error`: `parse_booted("not json").is_err()`
  - `choose_keeps_a_booted_selection`, `choose_falls_back_to_the_first_when_the_selection_is_gone`, `choose_with_nothing_booted_is_none`
  - `frame_slot_wakes_once_then_coalesces`: `put(1) == true`, `put(2) == false`, `take() == Some(2)`, `take() == None`, `put(3) == true`
- [ ] **Step 2:** `cd crates/harness-app && cargo test devices::` → FAIL (module missing).
- [ ] **Step 3:** Implement. Runtime key → `os`: strip `com.apple.CoreSimulator.SimRuntime.`, split the rest on `-`: the first part is the platform, the rest joined by `.` is the version (`iOS-27-2` → `iOS 27.2`). `device_type`: the part after `com.apple.CoreSimulator.SimDeviceType.`. For the timeouts, reuse `ports::run_bounded` (`src/ports.rs:894`, make it `pub(crate)`). It already handles stopping the process tree on timeout on both platforms.
- [ ] **Step 4:** `cargo test devices::` → PASS.
- [ ] **Step 5:** Commit: `feat(devices): list booted iOS simulators` (internal, no changelog line).

---

### Task 2: `ToolTab::Device` and prefs that older builds still read

**Files:**
- Modify: `crates/harness-app/src/workspace/prefs.rs` (`ToolTab`, `Prefs`, `load_prefs`, `save_prefs_to`)
- Modify: `crates/harness-app/src/workspace/ui/tool_panel.rs` (`ToolTab::as_str` → `"device"`; strip loop; `render_tool_body` arm, a placeholder until Task 6)
- Modify: `crates/harness-app/src/workspace/state/hosted.rs` (`set_tool_tab` arm, a no-op until Task 6)
- Test: `crates/harness-app/src/workspace/diff_rows.rs` (the existing tool-tab prefs tests live here, ~line 1200)

**Interfaces:**
- Produces: `ToolTab::Device`. The variant exists on every platform so `match`es stay exhaustive, but it is **not added to the strip in this task**: Task 6 adds it (macOS only) with the changelog line, so no commit ships a placeholder tab. `Prefs.device_udid: Option<String>` (`#[serde(default)]`), the last watched simulator.
- On disk: when the active tab is Device, `tool_tab` is written as `"review"` and a new `"device_tab": true` is added. Older builds ignore unknown fields but reject unknown enum values, so this keeps their prefs intact. On load, `device_tab: true` maps back to `ToolTab::Device` on macOS and to `Review` elsewhere.
- Also: `tool_tab` deserializes leniently (any unknown string → `Review`) using the same pattern as `InspectorPlacement::deserialize_lenient`, so a future tab can't wipe prefs either.

- [ ] **Step 1: Write the failing tests** (beside `the_ports_tool_tab_survives_a_save_load_cycle`):
  - `the_device_tool_tab_survives_a_save_load_cycle`: save `tool_tab: Device` → file contains `"tool_tab": "review"` and `"device_tab": true`; `load_prefs` → `Device` (`#[cfg(target_os = "macos")]`)
  - `a_build_without_the_device_tab_still_reads_the_file`: save with `Device` and `sidebar_width: 333.0`, then deserialize the JSON into a copy of today's shape (a test-local struct with `tool_tab` as the *old* four-variant enum and `sidebar_width`) → `Ok`, `sidebar_width == 333.0`
  - `an_unknown_tool_tab_loads_as_review_and_keeps_other_prefs`: `{"tool_tab":"telescope","sidebar_width":301.0,...}` → `Review`, `sidebar_width == 301.0`
  - `the_device_udid_survives_a_save_load_cycle`
- [ ] **Step 2:** `cargo test tool_tab device_tab device_udid` → FAIL.
- [ ] **Step 3:** Implement. `render_tool_body` and `set_tool_tab` get `ToolTab::Device` arms (empty body / no-op for now). The strip is unchanged.
- [ ] **Step 4:** `cargo test` (whole crate) → PASS. `cargo clippy --all-targets` → clean.
- [ ] **Step 5:** Commit: `feat(app): prefs know the Device tool tab, readable by older builds`. Add a `### Fixed` changelog line: `- An unknown tools-panel tab in prefs.json no longer resets every setting.`

---

### Task 3: Load the private simulator display API safely (macOS)

**Files:**
- Create: `crates/harness-app/src/devices/sim_display.rs` (`#[cfg(target_os = "macos")]`, declared from `devices/mod.rs`)
- Modify: `crates/harness-app/Cargo.toml` macOS deps: add `objc2-io-surface = { version = "0.3", default-features = false, features = ["std", "IOSurfaceRef", "IOSurfaceTypes", "objc2"] }` (already in the lockfile at 0.3.2 via gpui; feature names checked against its Cargo.toml)

**Interfaces:**
- Consumes: the Task 0 findings (class and selector chain) from the spec's "Spike findings".
- Produces:
  - `pub(crate) fn framework_paths(developer_dir: &Path) -> FrameworkPaths` (pure): `{ core_simulator: "/Library/Developer/PrivateFrameworks/CoreSimulator.framework/CoreSimulator", simulator_kit: developer_dir.parent().join("SharedFrameworks/SimulatorKit.framework/SimulatorKit") }` (no literal `..` in the path)
  - `pub(crate) fn developer_dir() -> Result<PathBuf, StreamError>`: `xcode-select -p`
  - `pub(crate) enum StreamError { NoXcode, Unsupported(&'static str) /* the missing class or selector */, DeviceNotFound, Failed(String) }`
  - `pub(crate) struct ScreenStream`: `start(udid: &str, on_event: impl Fn(ScreenEvent) + Send + Sync + 'static) -> Result<ScreenStream, StreamError>`; `Drop` unregisters the callbacks.
  - `pub(crate) enum ScreenEvent { Surface(CFRetained<IOSurfaceRef>) /* a new or resized surface */, Frame /* new pixels in the current surface */, Gone /* device shut down or display disconnected */ }`
  - Callbacks run on a private serial dispatch queue created by `start`, never on the main thread.
- Rules: `dlopen` once per process (a `OnceLock` holding the result). Every class goes through `NSClassFromString` and every selector through `respondsToSelector:` before use, and a miss returns `Unsupported("<name>")`. Wrap each Objective-C call that can raise in `objc2::exception::catch` and map an exception to `Failed`.

- [ ] **Step 1: Write the failing tests** (pure parts):
  - `simulator_kit_is_found_beside_the_selected_xcode`: `framework_paths("/Applications/Xcode-beta.app/Contents/Developer").simulator_kit == "/Applications/Xcode-beta.app/Contents/SharedFrameworks/SimulatorKit.framework/SimulatorKit"`
  - `a_command_line_tools_developer_dir_has_no_simulator_kit`: for `/Library/Developer/CommandLineTools` the path doesn't exist and `ScreenStream::start` (run through the path check, not a live device) returns `NoXcode`. Put the existence check in a pure `fn check_paths(&FrameworkPaths, exists: impl Fn(&Path) -> bool) -> Result<(), StreamError>` and test that.
- [ ] **Step 2:** `cargo test sim_display` → FAIL.
- [ ] **Step 3:** Implement from the spike findings.
- [ ] **Step 4:** `cargo test sim_display` → PASS. Manual: a `#[test] #[ignore] live_stream_delivers_frames` reads `OO_TEST_SIM_UDID`, starts a stream on it, runs two appearance toggles (`xcrun simctl ui $UDID appearance dark|light`, 1 s apart), and asserts one `Surface` and at least one `Frame` after each toggle. Run it with `OO_TEST_SIM_UDID=$UDID cargo test live_stream -- --ignored` on the test device → PASS.
- [ ] **Step 5:** Commit: `feat(devices): stream a simulator's screen surfaces` (internal, no changelog line).

---

### Task 4: The Device tab's state machine (pure)

**Files:**
- Create: `crates/harness-app/src/devices/tab_state.rs`

**Interfaces:**
- Produces:
  - `pub(crate) enum DeviceView { NeedsXcode, NoneRunning, Live { udid: String }, Reduced { udid: String, why: String }, ShutDown { name: String } }`
  - `pub(crate) struct DeviceTab { pub booted: Vec<DeviceInfo>, pub selected: Option<String>, pub view: DeviceView }`
  - `pub(crate) enum Command { StartStream(String), StopStream, StartScreenshots(String), StopScreenshots }`
  - Methods returning the commands to run: `on_list(&mut self, Result<Vec<DeviceInfo>, ListError>) -> Vec<Command>`, `on_select(&mut self, udid: &str) -> Vec<Command>`, `on_stream_failed(&mut self, StreamError) -> Vec<Command>`, `on_gone(&mut self) -> Vec<Command>`, `on_hidden(&mut self) -> Vec<Command>`, `on_shown(&mut self) -> Vec<Command>`.

- [ ] **Step 1: Write the failing tests:**
  - `first_list_starts_a_stream_on_the_first_device`: `on_list(Ok([a]))` → `[StartStream(a)]`, view `Live{a}`
  - `no_xcode_shows_needs_xcode_and_starts_nothing`
  - `an_empty_list_shows_none_running`
  - `a_stream_failure_falls_back_to_screenshots`: `on_stream_failed(Unsupported("SimDeviceScreen"))` → `[StartScreenshots(a)]`, view `Reduced`
  - `the_device_going_away_shows_shut_down_and_stops`: `on_gone()` → `[StopStream]`, `ShutDown{name}`. A later `on_list(Ok([]))` → `NoneRunning`.
  - `a_vanished_selection_moves_to_the_next_booted_device`: selected `a`, `on_list(Ok([b]))` → `[StopStream, StartStream(b)]`
  - `the_same_list_again_issues_no_commands` (no restart on every poll)
  - `hiding_stops_everything_and_showing_resumes_the_selection`
  - `selecting_another_device_switches_the_stream`
- [ ] **Step 2:** `cargo test tab_state` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test tab_state` → PASS.
- [ ] **Step 5:** Commit: `feat(devices): Device tab state machine` (internal).

---

### Task 5: BGRA surface → 420f pixel buffer (macOS)

The spike may have chosen the native-layer route instead. If so, replace this task with: a `DeviceLayerView` (`NSView` + `CALayer.contents = IOSurface`, positioned from gpui bounds like `browser_pane/webview.rs`, `setContentsChanged` per `Frame`), and drop the VideoToolbox dependency.

**Files:**
- Create: `crates/harness-app/src/devices/convert.rs` (`#[cfg(target_os = "macos")]`)
- Modify: `crates/harness-app/Cargo.toml` macOS deps: `objc2-core-video = { version = "0.3", default-features = false, features = ["std", "CVPixelBuffer", "CVPixelBufferPool", "CVBuffer", "CVImageBuffer", "CVReturn", "objc2-io-surface"] }`, `objc2-video-toolbox = { version = "0.3", default-features = false, features = ["std", "VTPixelTransferSession", "objc2-core-video"] }` (pin to 0.3.2 to match the family). The core-video feature names were checked against its 0.3.2 Cargo.toml. **Verify the video-toolbox feature names** once it's downloaded: it isn't in the registry cache yet. Check `Cargo.lock` adds only `objc2-video-toolbox`.

**Interfaces:**
- Produces: `pub(crate) struct PixelConverter`: `new() -> Result<Self, String>`; `convert(&mut self, src: &IOSurfaceRef) -> Result<CFRetained<CVPixelBuffer>, String>`. The output is always `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange`, the same width/height as `src`, from a `CVPixelBufferPool` that is rebuilt when the source size changes.

- [ ] **Step 1: Write the failing tests** (they create IOSurfaces locally, so no simulator is needed):
  - `converts_bgra_to_full_range_420`: a 64×128 BGRA `IOSurface` filled with one colour → the output pixel format is `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange`, size 64×128
  - `a_resized_source_gets_a_resized_output`: convert 64×128, then 128×64 → the second output is 128×64 (the rotation case)
- [ ] **Step 2:** `cargo test convert` → FAIL.
- [ ] **Step 3:** Implement with one `VTPixelTransferSession` per converter.
- [ ] **Step 4:** `cargo test convert` → PASS.
- [ ] **Step 5:** Commit: `feat(devices): convert simulator frames for gpui` (internal).

---

### Task 6: Device tab UI and wiring

**Files:**
- Create: `crates/harness-app/src/workspace/ui/device_panel.rs` (declared in `workspace/ui.rs` as `mod device_panel;`)
- Modify: `crates/harness-app/src/workspace.rs` (fields on `HarnessWorkspace`: `device_tab: crate::devices::tab_state::DeviceTab`, `device_screen: Option<Entity<DeviceScreen>>`, and the macOS-only stream handle `device_stream: Option<crate::devices::sim_display::ScreenStream>`)
- Modify: `crates/harness-app/src/workspace/construct.rs` (a poll loop beside the Ports one at ~line 770: every 3 s, only while `device_visible()`, run `devices::simctl::list_booted` in `background_spawn` and feed `on_list`)
- Modify: `crates/harness-app/src/workspace/state/hosted.rs` (`set_tool_tab`: `ToolTab::Device` refreshes the list at once and calls `on_shown`. Switching away calls `on_hidden`. Closing the panel and minimising the window do too: hook wherever `diff_open` flips and the window's visibility changes)
- Modify: `crates/harness-app/src/workspace/ui/tool_panel.rs` (`render_tool_body` → `self.render_device_panel(hx, cx)`)
- Modify: `CHANGELOG.md` (`## Unreleased` → `### Added`)

**Interfaces:**
- Consumes: `DeviceInfo`, `choose_device`, `FrameSlot` (Task 1); `ToolTab::Device`, `Prefs.device_udid` (Task 2); `ScreenStream`, `ScreenEvent`, `StreamError` (Task 3); `DeviceTab`, `DeviceView`, `Command` (Task 4); `PixelConverter` (Task 5); `list_booted`, `screenshot_jpeg` (Task 1).
- Produces: `pub(crate) fn device_visible(&self, window: &Window) -> bool` (`diff_open && tool_tab == Device && !window_is_minimized_or_hidden(window)`); the poll loop reads it through `view.update(cx, |ws, cx| ...)` with the window handle, and a `cx.observe_window_activation` / appearance observer calls `on_hidden`/`on_shown` when minimising or ⌘H flips it; `pub(crate) struct DeviceScreen` (a gpui entity holding the latest `CFRetained<CVPixelBuffer>` or, in Reduced mode, an `Arc<gpui::Image>` from the JPEG).

Behaviour to build:
- **Frame path:** the `ScreenStream` callback (dispatch queue) converts on that queue, `put`s into a `FrameSlot` shared with the UI, and when `put` returns true sends one wake on a `futures::channel::mpsc::unbounded` channel. A UI task awaiting that channel `take`s the frame, stores it in `DeviceScreen` and calls `cx.notify()` on **that entity only**. The panel embeds it with `AnyView::from(screen).cached(StyleRefinement::default().size_full())` so the workspace doesn't re-render per frame.
- **Drawing:** `gpui::surface(buffer).object_fit(ObjectFit::Contain)`, centred, filling the space under the picker.
- **Reduced mode:** while visible, one `screenshot_jpeg` in flight at a time, the next started when the last finishes (≈2 fps). Temp dir: `std::env::temp_dir().join("openorchestra-device")`. A one-line note above the picture: `Reduced frame rate — this Xcode's simulator display API isn't supported`.
- **Picker:** a dropdown row at the top, `<name> · <os>` per booted device. Choosing one calls `on_select`, stores `prefs.device_udid` and saves prefs. Start-up uses `prefs.device_udid` as the initial selection. The selection is per window and **does not change when the task, worktree or center tab changes**: nothing in the task-switch path touches `device_tab`.
- **Empty states (exact copy):** NeedsXcode: `Needs Xcode` / `Install Xcode to watch iOS simulators here.` NoneRunning: `No simulator running` / `Simulators your agents boot show up here.` ShutDown: `<name> shut down`.
- **Strip:** add `ToolTab::Device` to the `for tab in [...]` list in `render_tool_tab_strip` only under `cfg!(target_os = "macos")`, label `"Device"`.
- **CHANGELOG line** under `### Added`: `- **Device tab** (macOS): watch a running iOS simulator live from the tools panel, picked from every booted simulator on the Mac. Watch-only; falls back to about 2 frames a second when this Xcode's simulator display API isn't available.`

- [ ] **Step 1: Write the failing tests** (gpui `TestAppContext`, following the existing workspace tests):
  - `switching_tasks_keeps_the_watched_device`: with `device_tab.selected = Some(a)`, run the task-switch action → still `Some(a)`, and no `StopStream` was issued (assert on a test hook that records commands)
  - `the_device_tab_polls_only_while_visible`: the prefs half of `device_visible` (split out as `fn device_tab_shown(&self) -> bool`) is false for `tool_tab == Ports`, false when `diff_open == false`, true for Device + open
  - `choosing_a_device_saves_it`: `on_select(b)` through the panel handler → `prefs.device_udid == Some(b)`
- [ ] **Step 2:** `cargo test device_` → FAIL.
- [ ] **Step 3:** Implement. Run `Command`s in one place, `fn run_device_commands(&mut self, Vec<Command>, cx)`, the only code that starts or stops a `ScreenStream` or the screenshot loop.
- [ ] **Step 4:** `cargo test` → PASS. `cargo clippy --all-targets -- -D warnings` → clean. Cross-check Windows from the Mac: `cargo check --target x86_64-pc-windows-msvc` if the target is installed; otherwise rely on CI and note it in the commit message.
- [ ] **Step 5:** Commit: `feat(app): watch iOS simulators live in the Device tab` (with the CHANGELOG line).

---

### Task 7: Verify in a real build

**Files:** none (findings go in the PR / report).

- [ ] **Step 1:** `cd crates/harness-app && cargo build`. Make a scratch `.app` (own bundle id, e.g. `ai.openorchestra.devicetest`) with a wrapper exe that sets `HOME=<scratch>/home`, `TMUX_TMPDIR=/tmp/oo-dt`, unsets `TMUX`, then execs the binary. Ad-hoc sign it. Launch only through that wrapper.
- [ ] **Step 2:** With only the test device booted: open the Device tab → it's live within 1 s. Run the motion-source loop → each toggle shows at once, with no lag or tearing.
- [ ] **Step 3:** Switch tasks and center tabs → the view stays and doesn't flicker or restart.
- [ ] **Step 4:** Switch to Ports → the app's CPU drops back to idle (Activity Monitor or `top -pid`) and no `simctl` processes appear. Back on Device, minimise the window, then ⌘H → same.
- [ ] **Step 5:** Create and boot a second test device (`oo-device-test-2`) → it appears in the picker within ~3 s. Select it → it switches. `xcrun simctl shutdown <it>` → the tab shows "… shut down", then moves to the remaining device on the next poll. Delete it afterwards.
- [ ] **Step 6:** Rotate the device from Device Hub (simctl has no rotate command) → the view re-fits to landscape, then back.
- [ ] **Step 7:** Force the fallback (a debug env var `OO_DEVICE_FORCE_SCREENSHOTS=1`, read once in `run_device_commands`) → the "Reduced frame rate" note shows and the picture updates about 2×/s.
- [ ] **Step 8:** `make app`, then `bash script/notarize-mac.sh --sign-only dist/openOrchestra.app`. Copy the signed binary into the scratch bundle behind the same isolating wrapper (never launch `dist/openOrchestra.app` directly, as it would use the real HOME) → the Device tab is still live under the hardened runtime.
- [ ] **Step 9:** Check the old-build case: with the scratch HOME's `prefs.json` saved on the Device tab, run the released 1.5.0 binary against that same scratch HOME → its prefs (sidebar width, agents) are intact.
- [ ] **Step 10:** Delete the test devices. Report results with numbers (CPU during motion, idle CPU) to the user. Don't push or open a PR unless asked.
