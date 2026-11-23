# Device tab test controls Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A bottom bar in the Device tab with Rotate, Home, Dark/Light, Text size − / +, Screenshot and Shut down, Boot from the picker, an interactive picture (tap, swipe, scroll, type), and Apple's device frame with rounded screen corners. Everything acts only on the watched simulator, only on a user action.

**Architecture:** A pure-where-possible `devices::controls` module that runs public `simctl` commands (appearance, content size, screenshot) with timeouts. A macOS-only `devices::hid` module sends Rotate and Home through SimulatorKit's private HID path, found by a spike and loaded defensively like `sim_display`. A new `workspace/ui/device_controls.rs` draws the bar and dispatches clicks off the main thread, one control at a time. If the spike shows the surface stays portrait, the picture is rotated when drawn.

**Tech Stack:** Rust 2024, gpui (zed rev e8bc16a), objc2 0.6 (`exception` feature already on), `block2`, `dispatch2`, `xcrun simctl`.

**Spec:** `docs/superpowers/specs/2026-11-23-device-tab-controls-design.md` (and, for the existing tab, `docs/superpowers/specs/2026-11-22-device-tab-ios-design.md`)

## Global Constraints

- Build, test and clippy from `crates/harness-app` (`cargo test`, `cargo clippy --all-targets -- -D warnings`); the root workspace skips it.
- Never run `cargo fmt` in harness-app. Format touched lines by hand.
- Work on the current branch (master). No worktrees or branches. Don't push.
- Every user-visible commit adds a `CHANGELOG.md` `## Unreleased` line (Added/Changed/Fixed) in that commit.
- macOS-only code is behind `#[cfg(target_os = "macos")]` attributes. `cfg!(target_os = …)` is forbidden outside `src/platform.rs` (a test enforces it). The Windows build must still pass.
- A control acts only on the currently watched UDID, only on a user click. Nothing else changes a simulator.
- No control blocks the UI thread. Every `simctl` call and HID send runs off the main thread with a 5 s timeout (`ports::run_bounded_why`). One control is in flight at a time.
- A missing private class, selector or symbol never panics or throws: check with `NSClassFromString`, `respondsToSelector:` or `dlsym` first, and wrap sends in `objc2::exception::catch`. If the private path is unavailable, Rotate and Home are hidden.
- Live checks only on a simulator the run creates (`xcrun simctl create "OO device test" "iPhone 18 Pro"`; the name sorts before the user's devices) and deletes. Never target the user's existing simulators. Never quit or reconfigure the user's Device Hub or openOrchestra apps.
- Exact copy: tooltips `Rotate`, `Home`, `Dark appearance` / `Light appearance` (naming the appearance the click switches to), `Smaller text`, `Larger text`, `Copy screenshot`. Notes: `Copied screenshot` on success; on failure, the error's first line via `set_error_note`.

## Review Focus

1. **A click while the watched device changes** (the picker switches, or the device shuts down mid-command). The command must go to the device that was shown at click time and must not be retried on the new one. Pinned in Task 3 (the click captures the UDID; test).
2. **Rotating while live.** The picture re-fits and stays upright, with no stretched frame or stale buffer size. Pinned in Task 3 (the pure "draw rotation" function from orientation and surface size) and checked by eye in Task 4.
3. **Text size at its limits.** Repeated `increment` or `decrement` past the largest or smallest size must not error loudly. `simctl` may exit non-zero. Treat that as a quiet no-op note, not a red error. Pinned in Task 1.
4. **Reduced mode.** Dark/Light, Text size and Screenshot work. Rotate is offered only when `rotate_available()` (public `devicectl`). Home is always offered: the `oo-device-input` helper's button press, else bringing SpringBoard forward. The legacy HID path is gone (amended 2026-11-23). Pinned in Task 1 (`available_controls`).
5. **A large clipboard image.** A full-resolution PNG (1206×2622) is written once, with no UI hitch: encoding happens off the main thread. Pinned in Task 3.

---

### Task 0: Spike: how to rotate and press Home (throwaway, gate)

**Files:**
- Create (uncommitted): spike sources under `.superpowers/sdd/2026-11-23-device-tab-controls/spike/`
- Modify: `docs/superpowers/specs/2026-11-23-device-tab-controls-design.md` (add "Spike findings")

- [ ] **Step 1:** Create and boot the test device headless. Using the ObjC runtime and `dlsym`, find how to rotate the device and press Home from outside Device Hub. Candidates to try: `IndigoHIDMessageForButton` plus a `SimDeviceLegacyHIDClient` (`initWithDevice:error:`, `sendWithMessage:freeWhenDone:completionQueue:completion:` or whatever this Xcode has); a device-orientation HID or GSEvent; any `SimDevice` or SimulatorKit rotation selector (`deviceRotation`, `SimDisplayUIOrientationChangeDelegate`). Prior art for orientation: Meta's idb and Radon IDE.
- [ ] **Step 2:** Prove each one works. Rotate four times: `screenProperties.uiOrientation` cycles and the app's layout visibly changes in a `simctl io screenshot`. Home: launch Settings (`simctl launch $UDID com.apple.Preferences`), press Home, and a screenshot shows the home screen. Do this with Device Hub closed if it isn't running, and with it open if it is. Change nothing else.
- [ ] **Step 3:** Record whether the live screen surface (from `sim_display`'s register call) changes size on rotation, or stays portrait while `uiOrientation` changes. Note the orientation enum values.
- [ ] **Step 4:** Hardened-runtime check: sign the spike with `codesign --force --options runtime --entitlements crates/harness-app/resources/openOrchestra.entitlements --sign -` and repeat Step 2.
- [ ] **Step 5: Gate.** If rotation can't be sent without Device Hub, stop and report: the plan drops Rotate (Home alone, if it works, still ships).
- [ ] **Step 6:** Write "Spike findings" (exact symbols, signatures, block encodings, struct layouts if any, enum values, surface behaviour on rotation) into the spec. Commit only the spec: `docs: record the Device controls spike findings`. Delete the test device.

---

### Task 1: Public controls: appearance, text size, screenshot PNG

**Files:**
- Create: `crates/harness-app/src/devices/controls.rs` (all platforms; declared `pub(crate) mod controls;` in `devices/mod.rs`)
- Modify: `crates/harness-app/src/devices/simctl.rs` (factor the bounded `xcrun simctl …` call it already uses into `pub(crate) fn simctl(args: &[&str]) -> Result<String, String>` if that isn't there yet, and reuse it)

**Interfaces:**
- Produces:
  - `pub(crate) enum Control { Rotate, Home, ToggleAppearance, TextSmaller, TextBigger, Screenshot }` (`Clone, Copy, Debug, PartialEq, Eq`)
  - `pub(crate) enum Appearance { Light, Dark }`, plus `pub(crate) fn parse_appearance(stdout: &str) -> Option<Appearance>` (`"light\n"` → Light, `"dark"` → Dark, `"unsupported"`/`"unknown"`/other → None)
  - `pub(crate) fn appearance_args(udid: &str, set: Option<Appearance>) -> Vec<String>`: `["ui", udid, "appearance"]` plus `"light"`/`"dark"` when set
  - `pub(crate) fn content_size_args(udid: &str, bigger: bool) -> Vec<String>`: `["ui", udid, "content_size", "increment"|"decrement"]`
  - `pub(crate) fn available_controls(view: &DeviceView, hid: bool) -> Vec<Control>`: nothing for NeedsXcode/NoneRunning/ShutDown; for Live and Reduced, the three public controls plus Screenshot, and Rotate + Home when `hid` is true. Order: Rotate, Home, ToggleAppearance, TextSmaller, TextBigger, Screenshot.
  - `pub(crate) fn run_public(udid: &str, control: Control) -> Result<ControlOutcome, String>`, where `pub(crate) enum ControlOutcome { Done, Appearance(Appearance), AtLimit, Png(Vec<u8>) }`. ToggleAppearance reads the current value, sets the other one, and returns the new one. A text-size call that exits non-zero with stderr mentioning the size limit, or any non-zero exit after a valid increment/decrement, returns `AtLimit`: record the real stderr you see in the test fixture. Screenshot runs `simctl io <udid> screenshot --type=png <path>` to a unique `<udid>-<pid>-<n>.png` (add a PNG variant beside `simctl::shot_path`), reads the bytes, removes the file, and returns them.

- [ ] **Step 1: Write the failing tests:** `parses_light_and_dark`, `unsupported_appearance_is_none`, `appearance_args_get_and_set`, `content_size_args_increment_and_decrement`, `no_controls_when_nothing_is_running`, `public_controls_without_hid`, `all_controls_with_hid_in_order`, `reduced_mode_keeps_public_controls`, `text_size_at_the_limit_is_quiet` (a pure `fn content_size_outcome(code: i32, stderr: &str) -> ControlOutcome` fed with the stderr you captured on a real device at the limit).
- [ ] **Step 2:** `cargo test controls::` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test controls::` → PASS. Manual check on the test device: an `#[ignore]` test `public_controls_on_a_live_device` (reads `OO_TEST_SIM_UDID`) toggles the appearance twice and back, steps text bigger then smaller, and takes a PNG whose first 8 bytes are the PNG signature. Run it with `OO_TEST_SIM_UDID=$UDID cargo test public_controls_on -- --ignored` → PASS.
- [ ] **Step 5:** Commit: `feat(devices): public simulator test controls` (internal, no changelog line).

---

### Task 2: Rotate and Home through the private HID path (macOS)

**Files:**
- Create: `crates/harness-app/src/devices/hid.rs` (`#[cfg(target_os = "macos")]`)

**Interfaces:**
- Consumes: the spec's "Spike findings" (Task 0).
- Produces:
  - `pub(crate) fn hid_available() -> bool`: loads once (in a `OnceLock`) and caches whether every symbol and selector the findings need is present
  - `pub(crate) fn rotate(udid: &str) -> Result<(), StreamError>` (90° clockwise from the current orientation)
  - `pub(crate) fn press_home(udid: &str) -> Result<(), StreamError>`
  - These are blocking: callers run them off the main thread. Missing pieces return `StreamError::Unsupported("<symbol or selector>")`. A bad UDID returns `DeviceNotFound`.

- [ ] **Step 1: Write the failing tests:** pure parts only, e.g. `next_orientation_cycles_clockwise` (the orientation values from the findings) and any message-building helper the findings call for.
- [ ] **Step 2:** `cargo test hid::` → FAIL.
- [ ] **Step 3:** Implement from the findings, with the same defensive rules as `sim_display.rs`: proxies messaged via typed `objc_msgSend` after `respondsToSelector:`, everything inside `exception::catch`, blocks with signatures.
- [ ] **Step 4:** `cargo test hid::` → PASS. Manual check: an `#[ignore]` test `rotate_and_home_on_a_live_device` (with `OO_TEST_SIM_UDID`) rotates four times, asserting `uiOrientation` returns to the start (read via `sim_display` or the findings' getter), then launches Settings, presses Home, and checks that a screenshot differs from the Settings one. → PASS on the test device.
- [ ] **Step 5:** Commit: `feat(devices): rotate and press Home on a simulator` (internal).

---

### Task 3: The bottom bar, upright picture, clipboard

**Files:**
- Create: `crates/harness-app/src/workspace/ui/device_controls.rs` (declared in `workspace/ui.rs`)
- Modify: `crates/harness-app/src/workspace/ui/device_panel.rs` (call the bar under the picture, and apply the draw rotation in `render_device_screen` / `DeviceScreen`)
- Modify: `crates/harness-app/src/workspace.rs` (a `device_control_in_flight: bool` field)
- Modify: `CHANGELOG.md`

**Interfaces:**
- Consumes: `Control`, `available_controls`, `run_public`, `ControlOutcome` (Task 1); `hid_available`, `rotate`, `press_home` (Task 2); `DeviceTab.view` / `selected`; `set_note` and `set_error_note` (`workspace/render.rs:819-830`); `gpui::ClipboardItem` with `ClipboardEntry::Image(gpui::Image)` (`gpui/src/platform.rs:3143`) via `cx.write_to_clipboard`.
- Produces: `pub(crate) fn draw_rotation(surface_w: u32, surface_h: u32, ui_orientation: u32) -> Quarter` (a pure function, `pub(crate) enum Quarter { None, Cw90, Cw180, Cw270 }`) and `pub(crate) fn render_device_controls(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> AnyElement`.

Behaviour:
- The bar sits at the bottom of the Device tab, under the picture: a row of icon buttons, in `available_controls` order, with the tooltips from Global Constraints. It is hidden when `available_controls` is empty.
- A click captures the current `selected` UDID, sets `device_control_in_flight`, and runs the control in `background_spawn` (`hid::*` or `run_public`). On completion it clears the flag and shows the note. Clicks while a control is in flight are ignored, and the buttons look disabled.
- Screenshot: the PNG bytes go to `ClipboardItem` as `gpui::Image` (format PNG) on the UI thread. Encoding was already done by simctl. Note: `Copied screenshot`.
- ToggleAppearance: the button's tooltip and icon follow the last known appearance, read on device change (one `appearance` get, off the main thread) and updated from `ControlOutcome::Appearance`.
- `AtLimit`: a plain note, `Text size is at its limit`, not an error.
- Upright picture: if the spike found the surface stays portrait, `DeviceScreen` draws the surface rotated by `draw_rotation(..)` using the orientation reported on the stream's properties callback (wire `propertiesChanged` through `ScreenEvent` if the findings say that's where it comes from). Otherwise `draw_rotation` returns `None` and only the re-fit applies.
- CHANGELOG `### Added`: `- **Device tab controls** (macOS): rotate, Home, dark/light appearance, text size and copy-screenshot buttons under the live simulator view.`

- [ ] **Step 1: Write the failing tests:**
  - `draw_rotation_for_each_orientation`, with the values from the findings
  - `a_click_targets_the_device_shown_at_click_time`: a pure `fn control_target(selected: Option<&str>, view: &DeviceView) -> Option<String>`, None when nothing is live or reduced
  - `a_second_click_while_one_runs_is_ignored`: a pure `fn may_run_control(in_flight: bool) -> bool`, or fold it into `control_target`
  - `the_appearance_tooltip_names_the_switch_target`
- [ ] **Step 2:** `cargo test device_control` and `cargo test draw_rotation` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Full `cargo test` → PASS. `cargo clippy --all-targets -- -D warnings` → clean.
- [ ] **Step 5:** Commit: `feat(app): test controls under the Device tab's live view` (with the CHANGELOG line).

---

### Task 4: Boot and Shut down from the tab

Added by the spec's Amendments §3. It doesn't depend on Task 0.

**Files:**
- Modify: `crates/harness-app/src/devices/simctl.rs` (list every device in the default set, not just booted ones)
- Modify: `crates/harness-app/src/devices/controls.rs` (`boot`, `shutdown`)
- Modify: `crates/harness-app/src/workspace/ui/device_panel.rs` (picker groups) and `crates/harness-app/src/workspace/ui/device_controls.rs` (Shut down button and confirm), or create the latter if Task 3 hasn't yet
- Modify: `CHANGELOG.md`

**Interfaces:**
- Produces:
  - `pub(crate) struct DeviceList { pub booted: Vec<DeviceInfo>, pub shut_down: Vec<DeviceInfo> }`
  - `pub(crate) fn parse_devices(json: &str) -> Result<DeviceList, String>`: available devices only (`isAvailable: true`); `state == "Booted"` goes to booted, `"Shutdown"` to shut_down, and other states (Booting, Shutting Down, Creating) to neither. Each group sorted like `parse_booted`.
  - `pub(crate) fn list_devices() -> Result<DeviceList, ListError>`: `xcrun simctl list devices -j` (one call; replaces `list_booted` in the poll, and the state machine still gets `booted`)
  - `pub(crate) fn boot(udid: &str) -> Result<(), String>` and `pub(crate) fn shutdown(udid: &str) -> Result<(), String>`: bounded at 60 s, since boot can take a while; off the main thread
- Behaviour:
  - The picker shows "Running" then "Shut down" groups. Choosing a shut-down device shows `Boot <name>` in place of the picture.
  - Clicking Boot runs `boot`, makes that device the user's pick, and lets the normal poll take it live.
  - The bottom bar's **Shut down** (tooltip `Shut down`) opens a confirm reusing the app's existing prompt pattern (`workspace/ui/dialogs.rs`): title `Shut down <name>?`, body `An agent may be using it.`, buttons `Shut down` / `Cancel`.
  - While a boot or shutdown runs, the bar shows it's busy and other controls wait.
- CHANGELOG `### Added`: `- Boot and shut down simulators from the Device tab.`

- [ ] **Step 1: Write the failing tests:**
  - `parses_booted_and_shut_down_groups`
  - `unavailable_and_transitional_devices_are_left_out`
  - `boot_and_shutdown_args`
  - `the_picker_offers_boot_for_a_shut_down_pick`: pure, a `fn picker_action(list: &DeviceList, pick: &str) -> PickAction { Watch, Boot }`
- [ ] **Step 2:** `cargo test parse_devices picker_action` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Full `cargo test` → PASS, and clippy is clean. Manual: on your own "OO device test", shut down from the bar and boot from the picker. The live view comes back.
- [ ] **Step 5:** Commit: `feat(app): boot and shut down simulators from the Device tab` (with the CHANGELOG line).

---

### Task 5: Click, drag, scroll and type on the picture (CoreDevice input helper)

Rewritten 2026-11-23 after the input spike: legacy HID is ignored on most iOS 27.2 boots, but CoreDevice's remote HID worked on 8 of 8 boots. The authority is the spec section "## Input spike findings (CoreDevice)": its call chain, stub `.swiftinterface`, helper protocol, timing, lifetime and coordinate space. The spike's sources are in `.superpowers/sdd/2026-11-23-device-tab-controls/spike-input/`.

**Files:**
- Create: `crates/harness-app/helpers/device-input/` (Swift sources for `oo-device-input`, the stub `CoreDevice.swiftinterface`, and anything else the findings need)
- Create: `script/build-device-input-helper.sh`, modelled on `script/build-voice-helper.sh`: it takes the output path and an arch list, builds with `xcrun swiftc` against the private frameworks per the findings, and supports universal builds via `lipo`. It exits 0 and removes the output, with a one-line message, when the SDK or frameworks are missing, as on CI.
- Modify: `Makefile`, `script/run-dev.sh`, `script/package-mac.sh` and `script/notarize-mac.sh`: build, place and sign the helper at `Contents/Helpers/oo-device-input` exactly as each one does `oo-voice`. Release signing uses `--options runtime --timestamp` with the app's identity.
- Create: `crates/harness-app/src/devices/input.rs` (macOS-only process client)
- Modify: `crates/harness-app/src/workspace/ui/device_panel.rs` (`DeviceScreen` mouse, scroll and key handlers, focus), `crates/harness-app/src/devices/worker.rs` (forward the current `Quarter` to the UI with its generation, which the mapping needs)
- Modify: `crates/harness-app/src/devices/hid.rs` (delete it: legacy HID is retired) and `devices/mod.rs`
- Modify: `CHANGELOG.md`

**Interfaces:**
- Produces in `input.rs`:
  - `pub(crate) struct InputHelper`: `spawn(udid: &str) -> Result<InputHelper, String>`, found next to the app executable in `../Helpers/oo-device-input`, falling back to `target/debug/oo-device-input` for `cargo run`.
  - Methods: `touch(phase: TouchPhase, x: f64, y: f64)`, using the findings' 0…1 portrait ratios; `key(usage: u16, down: bool)`; `text(s: &str)`, if the protocol has it, otherwise per-key; `home()`.
  - Writes are line-oriented and non-blocking: a writer thread drains a channel, so the UI thread never blocks.
  - One helper per watched device. It is started lazily on the first input and killed on device change, tab hide, stream stop and drop.
  - Helper exit or stderr makes input unavailable for that device until the next start attempt, with a capped retry.
  - `pub(crate) fn helper_path() -> Option<PathBuf>`.
- Produces in the UI: `pub(crate) fn view_to_device(point: Point<Pixels>, screen: Bounds<Pixels>, turn: Quarter) -> Option<(f64, f64)>` (pure). It maps a point in the drawn screen rectangle, which Task 6's `screen_rect_in_frame` supplies when the frame is shown, to the portrait 0…1 ratios the helper wants. It undoes the drawn turn and returns None outside the screen.
- Behaviour:
  - Mouse down → touch down, drag → move, up → up.
  - Trackpad or wheel scroll → a short synthesised swipe; there is no scroll capability.
  - Clicking the picture focuses it. While it's focused, typed keys go to the device as USB HID usages: letters, digits, punctuation with Shift, Return, Delete, Tab, arrows, and Esc as Shift-Esc. ⌘-chords stay with the app, except ⌘V, which types the Mac clipboard as text.
  - Esc or clicking elsewhere unfocuses, and a focus ring shows while focused.
  - Home: if the helper is available, the bar's Home uses its button press; otherwise it uses the existing SpringBoard launch.
  - If the helper is missing or fails, the picture stays watch-only, with the tooltip `Simulator input isn't available with this Xcode`.
- CHANGELOG `### Added`: `- Click, drag, scroll and type on the Device tab's live view to use the simulator directly.`

- [ ] **Step 1: Write the failing tests:**
  - `view_to_device_maps_corners_and_centre`
  - `points_outside_the_screen_are_ignored`
  - `turned_points_map_back_to_portrait` (each Quarter)
  - `cmd_chords_stay_with_the_app_except_paste`
  - `keys_map_to_hid_usages` (a pure table from gpui keystrokes to usage codes, with Shift)
  - `helper_lines_are_well_formed` (the line encoding per the findings' protocol)
- [ ] **Step 2:** `cargo test view_to_device keys_map helper_lines` → FAIL.
- [ ] **Step 3:** Implement the helper (Swift, from the spike), the build script and packaging, then the Rust client and the UI.
- [ ] **Step 4:** Full `cargo test` → PASS, and clippy is clean. `bash script/build-device-input-helper.sh /tmp/oo-dt/oo-device-input` builds. An `#[ignore]` live test, `helper_taps_home_and_types` (with `OO_TEST_SIM_UDID` on your own device), spawns the helper, presses Home, taps Settings → General, and types in Safari's address bar, checked by `simctl io` screenshot. Run it on 3 fresh boots → PASS each.
- [ ] **Step 5:** Commit: `feat(app): use the simulator from the Device tab` (with the CHANGELOG line).

---

### Task 6: Device frame and rounded screen

Added by the spec's Amendments §2. It doesn't depend on Task 0.

**Files:**
- Create: `crates/harness-app/src/devices/chrome.rs` (parsing is all-platform; rendering the PDF to an image is macOS-only)
- Modify: `crates/harness-app/src/workspace/ui/device_panel.rs` (draw the frame around the picture, apply the mask) and `crates/harness-app/src/workspace/ui/device_controls.rs` (a `Show frame` toggle)
- Modify: `crates/harness-app/src/workspace/prefs.rs` (`device_frame: bool`, `#[serde(default = "yes")]`, on by default)
- Modify: `CHANGELOG.md`

**Interfaces:**
- Produces:
  - `pub(crate) fn chrome_for(device_type_id: &str) -> Option<ChromeSpec>`. It reads the device type's `profile.plist` (`/Library/Developer/CoreSimulator/Profiles/DeviceTypes/<name>.simdevicetype/Contents/Resources/profile.plist` — find the bundle via `simctl list devicetypes -j`'s `bundlePath`), takes `chromeIdentifier` and `framebufferMask`, then reads `/Library/Developer/DeviceKit/Chrome/<id-suffix>.devicechrome/Contents/Resources/chrome.json`.
  - `pub(crate) struct ChromeSpec` holds the nine-slice image names, the screen inset or offsets from `chrome.json`, and the mask PDF path. Parse only what drawing needs, and record the JSON keys you rely on in a comment, citing `phone11`.
  - macOS: `pub(crate) fn render_pdf(path: &Path, size_px: (u32, u32)) -> Option<Arc<RenderImage>>` via CoreGraphics (`CGPDFDocument`), cached per (path, size).
- Behaviour:
  - The frame is drawn around the picture when `device_frame` is on and a `ChromeSpec` exists. The picture is always clipped by the mask, so corners are rounded, whether the frame is on or off.
  - The frame scales with the picture, and `view_to_device` (Task 5) gets the frame inset.
  - Missing chrome or an unreadable PDF falls back to the rounded screen only, without an error.
  - The PDFs are read from the user's Xcode install at run time and never copied into the repo or the app bundle.
- CHANGELOG `### Added`: `- The Device tab draws the iPhone's frame and rounded screen (toggle with Show frame).`

- [ ] **Step 1: Write the failing tests:**
  - `parses_phone11_chrome_json`, using a trimmed copy of the real `chrome.json` structure, keys only with no Apple artwork, in `devices/testdata/`
  - `profile_names_chrome_and_mask`
  - `missing_chrome_falls_back_to_rounded_screen`
  - `device_frame_defaults_on_and_survives_a_save_load_cycle`
- [ ] **Step 2:** `cargo test chrome device_frame` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Full `cargo test` → PASS, and clippy is clean.
- [ ] **Step 5:** Commit: `feat(app): device frame and rounded screen in the Device tab` (with the CHANGELOG line).

---

### Task 7: Verify in a real build

Same isolation as the Device tab plan's Task 7: the `/tmp/oo-dt` scratch bundle with the HOME/TMUX wrapper, your own "OO device test" simulator, and the user's devices and apps untouched. Window captures and clicks need macOS Screen Recording and Accessibility for the host app. If they're missing, do the non-visual checks and list the rest for the user to check by eye.

- [ ] **Step 1:** Rebuild and refresh the scratch bundle. Open the Device tab on the test device.
- [ ] **Step 2:** Rotate ×4: the picture turns and stays upright, then returns to portrait. Home from Settings: the home screen shows.
- [ ] **Step 3:** Dark/Light: the picture changes, and the tooltip flips. Text size: Settings' text visibly grows and shrinks. At the limit, the quiet note shows.
- [ ] **Step 4:** Copy screenshot: the clipboard holds a PNG of the device (`osascript -e 'clipboard info'` lists `«class PNGf»`), and the note shows.
- [ ] **Step 5:** With `OO_DEVICE_FORCE_SCREENSHOTS=1`: the public controls still work, Rotate follows `rotate_available()`, and Home is always offered (the input helper's button press, else SpringBoard). There is no legacy HID any more.
- [ ] **Step 6:** Switch devices mid-click (start Screenshot, then pick the other device): the screenshot is of the first device, and nothing runs on the second.
- [ ] **Step 7:** Delete the test devices. Report the results.
