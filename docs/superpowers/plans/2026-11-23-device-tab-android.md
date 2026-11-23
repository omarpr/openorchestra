# Device tab: Android emulators Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Android emulators in the Device tab, working like the iOS simulators do: in the same picker, with a live picture, the skin frame, click and type, the bottom-bar controls plus Back, and Boot/Shut down. macOS only.

**Architecture:** A tonic gRPC client per watched emulator talks to the emulator's own controller service, which is the public route Android Studio uses. Its screenshot stream feeds the existing frame path (FrameSlot → 420f `surface()`), and its touch, wheel and key calls back the existing input mapping. Discovery reads the emulator's `avd/running/*.ini` files. The skin's `layout` + WebP art feeds the existing overlay pipeline. `adb` is used only for appearance and font scale.

**Tech Stack:** Rust 2024, gpui (zed e8bc16a), `tonic` + `prost` 0.14 (generated code checked in), the Android emulator's `emulator_controller.proto`, VideoToolbox (existing converter), `emulator` / `adb` CLIs.

**Spec:** `docs/superpowers/specs/2026-11-23-device-tab-android-design.md` (and, for what it builds on, `2026-11-22-device-tab-ios-design.md` and `2026-11-23-device-tab-controls-design.md`)

## Global Constraints

- Build, test and clippy from `crates/harness-app` (`cargo test`, `cargo clippy --all-targets -- -D warnings`); the root workspace skips it. Never run `cargo fmt` there.
- Work on the current branch (master). No worktrees or branches. Push only when the user says so.
- Every user-visible commit adds a `CHANGELOG.md` `## Unreleased` line.
- The Device tab stays macOS-only. Windows must still build: use `#[cfg(target_os = "macos")]` attributes, never `cfg!(target_os…)` outside `platform.rs`, and leave no dead code under Windows clippy `-D warnings`.
- No build may need `protoc`, a JDK or Android Studio. The protos are vendored with their Apache-2.0 headers, and the generated Rust is checked in.
- Act only on a user click or keystroke, on the device shown. No stream and no polling while the tab is hidden, covered, or behind a Boot offer (the iOS rules).
- Emulators an agent started are listed and controllable like the ones openOrchestra boots.
- Live checks only on an AVD the run boots itself headless (`emulator -avd <id> -no-window`), stopped afterwards. Never stop or drive an emulator you didn't start. Never run app binaries directly: `target/debug/*` and `target/debug/deps/harness_app-*` can be the app itself. Use `cargo test`, or an isolated scratch bundle with HOME/TMUX isolation.
- Android's extra bar button is **Back** only.

## Review Focus

1. **Several emulators, and an agent's emulator shutting down mid-stream.** The stream ends cleanly, the tab falls back like iOS does, and input never reaches another device. (Task 3, plus the iOS keyboard-handback rules carried into Task 4.)
2. **A stale discovery file.** The emulator crashed and left its `pid_*.ini` behind, or the pid was reused. That device must not be listed, and connecting must not hang. (Task 2.)
3. **The panel resizing while streaming.** The stream is re-requested at the new size, debounced, with no buffer-size mismatch or stretched frame. (Task 3.)
4. **adb missing, or not on PATH.** Dark/Light and Text size are hidden, and nothing else breaks. (Task 4.)
5. **A skin missing or of another format.** There's no frame, but the screen still has rounded corners from the AVD's `hw.lcd` size and a default radius. (Task 5.)

---

### Task 0: Spike (throwaway, gate)

Per the spec's "Spike first". It produces findings only. Write them into the spec as "## Spike findings", commit only the spec, and keep the sources under `.superpowers/sdd/2026-11-23-device-tab-android/spike/`.

- [ ] **Step 1:** Boot `Medium_Phone_API_35` headless yourself (`emulator -avd Medium_Phone_API_35 -no-window -no-snapshot-save`). Read its `pid_<pid>.ini`.
- [ ] **Step 2:** Auth. With grpcurl (or a tiny tonic program), call `getStatus`, `getScreenshot`, `streamScreenshot`, `sendTouch`, `sendKey` and `setVmState`: once with `authorization: Bearer <grpc.token>`, once with no token, and once with a JWK placed in `grpc.jwks`. Record which works for each method.
- [ ] **Step 3:** `streamScreenshot` RGBA8888 at about 400×890 and at full size. Record fps, latency, our CPU and qemu's CPU.
- [ ] **Step 4:** Input. Tap, swipe, `injectWheel`, `text` typing, Home and Back via key names, rotation (`setPhysicalModel` versus the rotate key), and shutdown. Verify each by screenshot.
- [ ] **Step 5:** Build. Add `tonic` to a scratch copy of harness-app's Cargo.toml, generate the client from the vendored protos once, and confirm it builds with no `protoc` on PATH. Record the lockfile delta.
- [ ] **Step 6: Gate.** If no auth works without Android Studio, or the stream can't keep up, stop and report.
- [ ] **Step 7:** Write the findings into the spec and commit (`docs: record the Android Device tab spike findings`). Stop the AVD.

### Task 1: Vendored protos and the generated gRPC client

**Files:**
- Create: `crates/harness-app/proto/android/` (the needed `.proto` files, unmodified, plus a `NOTICE`)
- Create: `crates/harness-app/src/devices/android/grpc/` (the generated prost/tonic code, checked in, with a header naming the generator script)
- Create: `script/gen-android-grpc.sh` (regenerates that code with protoc and prost-build/tonic-build; never run by the build)
- Modify: `crates/harness-app/Cargo.toml` (`tonic` with only the client and transport features the spike needed; `prost` 0.14 to match the lockfile)

**Interfaces:** Produces the `devices::android::grpc` module (macOS-only) with an `EmulatorControllerClient`.

- [ ] **Step 1:** Write the failing test `the_generated_client_builds_a_request`: build an `ImageFormat { format: RGBA8888, width: 400, height: 890 }` and round-trip it through prost encode/decode.
- [ ] **Step 2:** `cargo test the_generated_client` → FAIL.
- [ ] **Step 3:** Vendor the protos, generate the code and wire up the module.
- [ ] **Step 4:** `cargo test` → PASS, and clippy is clean. `PATH` without protoc still builds.
- [ ] **Step 5:** Commit: `build(devices): vendored Android emulator gRPC client` (internal).

### Task 2: Discovery, the SDK, and Android in the picker

**Files:**
- Create: `crates/harness-app/src/devices/android/discovery.rs` (pure parse, all platforms; the directory scan and pid check are macOS-gated)
- Create: `crates/harness-app/src/devices/android/sdk.rs` (SDK location from `ANDROID_HOME`, then `ANDROID_SDK_ROOT`, then `~/Library/Android/sdk`; `emulator -list-avds`, cached; `emulator`/`adb` paths)
- Modify: `crates/harness-app/src/devices/mod.rs` (`DeviceInfo` gains `platform: Platform { Ios, Android }` and an Android handle), `simctl.rs` / `DeviceList` (merge the iOS and Android lists), `workspace/ui/device_panel.rs` (an "Android" picker group, plus Boot for stopped AVDs)

**Interfaces:**
- `pub(crate) struct AndroidRunning { pid: u32, avd_id: String, name: String, grpc_port: u16, token: Option<String>, serial_port: u16, avd_dir: PathBuf }`
- `pub(crate) fn parse_running_ini(text: &str) -> Option<AndroidRunning>`
- `pub(crate) fn running_emulators() -> Vec<AndroidRunning>` (skips dead pids)
- `pub(crate) fn list_avds() -> Result<Vec<String>, String>`

- [ ] **Step 1:** Write the failing tests:
  - `parses_a_running_emulator_ini`, using the real field names from this machine's file (port 8554, `Pixel_9_Pro_API_35`), with the token redacted to `"t"` in the fixture
  - `a_dead_pid_is_skipped` (an injected pid-alive function)
  - `sdk_is_found_from_android_home_then_default`
  - `android_devices_group_separately_in_the_picker`
  - `a_stopped_avd_offers_boot`
- [ ] **Step 2:** → FAIL. **Step 3:** Implement. **Step 4:** Full `cargo test` and clippy → PASS.
- [ ] **Step 5:** Commit: `feat(devices): list Android emulators in the Device tab` (internal until Task 3 shows something; no changelog line).

### Task 3: The live Android picture

**Files:**
- Create: `crates/harness-app/src/devices/android/stream.rs` (macOS: a tonic `streamScreenshot` on a tokio runtime owned by the worker thread)
- Modify: `devices/worker.rs` (start and stop the Android stream next to the iOS one, with the same generation tags), `devices/convert.rs` (an RGBA8888 bytes → 420f entry point that reuses the pool and transfer session), `workspace/ui/device_panel.rs` (request the panel's pixel size, re-request on resize, debounced)
- Modify: `CHANGELOG.md` (`### Added`: `- Android emulators in the Device tab: live picture, picked from the same list as iOS simulators.`)

**Interfaces:** `AndroidStream::start(dev: &AndroidRunning, size: (u32, u32), on_frame) -> Result<AndroidStream, StreamError>`; dropping it ends the call.

- [ ] **Step 1:** Write the failing tests:
  - `rgba_frames_convert_to_420f` (synthetic bytes; size and format)
  - `a_resize_is_debounced_into_one_request` (pure)
  - `the_stream_request_carries_the_token`, if the spike says it's needed (a pure metadata builder)
- [ ] **Step 2:** → FAIL. **Step 3:** Implement.
- [ ] **Step 4:** `cargo test` → PASS. An `#[ignore]` live test, `android_stream_delivers_frames`, on a spike-booted AVD (`OO_TEST_AVD`) → PASS.
- [ ] **Step 5:** Commit: `feat(app): live Android emulator picture in the Device tab`.

### Task 4: Input and controls

**Files:**
- Create: `crates/harness-app/src/devices/android/input.rs` (`sendTouch`, `injectWheel`, `sendKey`)
- Modify: `workspace/ui/device_input.rs` (a small input-sink trait that the iOS helper and Android both implement; Android scrolls with the real wheel, not a synthesised swipe), `devices/controls.rs` and `workspace/ui/device_controls.rs` (per-platform controls, plus a Back button for Android)
- Modify: `CHANGELOG.md` (`### Added`: `- Click, scroll, type, and the Device bar's controls (plus Back) on Android emulators.`)

**Interfaces:**
- `pub(crate) trait DeviceInput { fn touch(&self, phase: TouchPhase, x: f64, y: f64); fn wheel(&self, dx: f64, dy: f64); fn key(&self, usage: u16, down: bool); fn text(&self, s: &str); }`
- Android controls:
  - Home and Back: `sendKey` key names
  - Rotate: what the spike found
  - Screenshot: `getScreenshot` PNG
  - Shut down: `setVmState`, with the same confirm
  - Boot: `emulator -avd <id> -no-window`, detached
  - Dark/Light and Text size: `adb -s emulator-<serial_port> shell cmd uimode night yes|no` and `settings put system font_scale` (stepping 0.85/1.0/1.15/1.3), hidden without adb

- [ ] **Step 1:** Write the failing tests:
  - `android_touch_maps_to_device_pixels` (each turn)
  - `wheel_deltas_pass_through_without_a_swipe`
  - `back_and_home_use_key_names`
  - `font_scale_steps_and_stops_at_the_ends`
  - `controls_without_adb_hide_appearance_and_text_size`
  - `the_keyboard_handback_rules_hold_for_android` (the iOS decision functions, applied to an Android target)
- [ ] **Step 2:** → FAIL. **Step 3:** Implement.
- [ ] **Step 4:** `cargo test` → PASS. An `#[ignore]` live test on a spike-booted AVD runs tap, swipe, wheel, type, Home, Back, Rotate and screenshot → PASS.
- [ ] **Step 5:** Commit: `feat(app): use Android emulators from the Device tab`.

### Task 5: The skin frame

**Files:**
- Create: `crates/harness-app/src/devices/android/skin.rs` (a parser for the `layout` brace format, and WebP decode through the `image` crate; check its WebP feature and add it if missing)
- Modify: `devices/chrome.rs` (`FrameArt` gets an Android variant fed by the skin: display rect, corner radius, part offsets, `back.webp`, `mask.webp`)
- Modify: `CHANGELOG.md` (`### Added`: `- Android emulators get their device frame in the Device tab.`)

**Interfaces:** `pub(crate) fn parse_layout(text: &str) -> Option<SkinLayout>`, and `pub(crate) fn skin_for(avd_dir: &Path) -> Option<Arc<AndroidSkin>>` (reads `config.ini` → `skin.path`).

- [ ] **Step 1:** Write the failing tests:
  - `parses_pixel_9_pro_layout`: a trimmed copy of the real `layout`, numbers only and no artwork, giving display 1280×2856 at 60,61 in 1408×2974 with corner 109
  - `missing_skin_falls_back_to_rounded_screen`
  - `android_overlay_hole_matches_the_display_rect`
- [ ] **Step 2:** → FAIL. **Step 3:** Implement. **Step 4:** `cargo test` → PASS; render the composed frame to a PNG and look at it by eye.
- [ ] **Step 5:** Commit: `feat(app): Android device frame in the Device tab`.

### Task 6: Verify

- [ ] **Step 1:** Run the full `cargo test` and clippy. Push only on the user's say-so, then confirm CI is green, Windows included.
- [ ] **Step 2:** The user checks by eye in `make run`, against an AVD they boot from the picker:
  - the picture, smoothness and frame
  - tap, scroll and type
  - Home, Back and Rotate
  - Dark/Light, Text size and Screenshot
  - Shut down and Boot
  - the keyboard staying with the device that was clicked
- [ ] **Step 3:** Clean up any AVDs the run started. Report the results.
