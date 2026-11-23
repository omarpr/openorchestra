# Device tab: Android emulators — design

Date: 2026-11-23. Status: draft, prepared for review (phase 2 of the Device
tab; phase 1 is `2026-11-22-device-tab-ios-design.md` plus
`2026-11-23-device-tab-controls-design.md`).

## Why

The user asked, from the start, for the Device tab to cover "the iOS
simulator … and on Android using their emulator", headless, with the same
experience the iOS side now has: a live picture in the right-hand tools
panel, a picker, the device frame, the bottom-bar controls, and clicking and
typing straight into the device. Android is phase 2, after iOS shipped.

## Goals

- Android emulators appear in the **same picker** as iOS simulators, in their
  own group ("Android"), running and stopped.
- **Live picture** of a running emulator, smooth (target: the emulator's own
  frame rate), including emulators started headless (`-no-window`) and ones
  an agent started with no flags.
- **Interactive** like iOS: click = tap, drag = swipe, scroll wheel and
  trackpad = scroll, typing when the picture has focus (same focus rules,
  same thin focus ring, same ⌘-chord routing and ⌘V paste).
- **Controls** in the same bottom bar: Rotate, Home, Dark/Light, Text size
  − / +, Copy screenshot, Shut down; **Boot** a stopped AVD from the picker
  (headless). Back is added for Android (it has no iOS equivalent).
- **Device frame** from the AVD's skin (`<sdk>/skins/<name>/`: `layout`,
  `back.webp`, `mask.webp`), with the screen's rounded corners from the
  skin's `corner_radius` / mask, and the same "Show frame" toggle.
- **Windows too.** The Android path is public and cross-platform; on Windows
  the Device tab appears with Android devices only. (iOS stays macOS-only.)

## Non-goals

- Physical Android devices over adb (later, maybe).
- Creating, deleting or editing AVDs; installing system images.
- Multi-display, foldables' posture UI, sensors, GPS, phone/SMS, snapshots.
- Starting adb servers or managing adb ourselves beyond what the controls
  need.

## Anti-goals

- Anything acts on an emulator without a user click/keystroke on the device
  shown (same rule as iOS).
- Cost while nobody is looking: no stream, no polling while the tab is
  hidden, covered, or behind a Boot offer (same rule as iOS).
- Breaking the iOS side or the Windows build.
- Requiring `protoc`, a JDK, or Android Studio to build openOrchestra.

## What's on the machine (checked 2026-11-23)

- SDK at `~/Library/Android/sdk`; emulator 35.2.10; AVDs `Pixel_9_Pro_API_35`
  and `Medium_Phone_API_35` (arm64-v8a).
- **Discovery:** each running emulator writes
  `~/Library/Caches/TemporaryItems/avd/running/pid_<pid>.ini` with
  `grpc.port`, `grpc.token`, `avd.name`, `avd.id`, `avd.dir`, `port.serial`,
  `port.adb`, `grpc.jwks`. An agent-started emulator (no flags) served gRPC
  on 8554. Linux and Windows have equivalent directories (Linux
  `$XDG_RUNTIME_DIR/avd/running`, Windows `%LOCALAPPDATA%\Temp\avd\running`;
  to confirm).
- **gRPC API** (`<sdk>/emulator/lib/emulator_controller.proto`, plus
  `ui_controller_service.proto`): `streamScreenshot(ImageFormat)` (format
  PNG/RGBA8888/RGB888, requested width/height, rotation), `getScreenshot`,
  `sendTouch(TouchEvent)` (multi-touch with identifiers/pressure),
  `sendMouse`, `injectWheel(stream WheelEvent{dx,dy})`,
  `sendKey(KeyboardEvent)` (USB/evdev codes, `key` names like "Home", or
  `text`), `setVmState` (shutdown), `setPhysicalModel` (rotation),
  `getStatus`, `streamNotification`, clipboard get/set.
- **Auth:** `emulator_access.json` lists methods that need a token. Plain
  launches write `grpc.token` (bearer) into the discovery file; Android Studio
  instead drops a JWK into `grpc.jwks`. Which one a plain launch accepts for
  the protected methods is **to verify in the spike**.
- **Skin:** `skins/pixel_9_pro/layout` gives the display rect inside the
  frame (`display width 1280 height 2856 corner_radius 109`, placed at
  `x 60 y 61` in a 1408×2974 layout), `back.webp` (frame) and `mask.webp`
  (screen shape). The AVD's `config.ini` names `skin.name` / `skin.path`.
- **Rust:** `prost` 0.14 is already in harness-app's lockfile (and `h2`,
  `hyper`, `tower`); `tonic` is not. `protoc` is installed here but must not
  be a build requirement.

## Approaches

- **A. gRPC directly from Rust with `tonic` (recommended).** One client per
  watched emulator: a screenshot stream into the existing frame path, and
  touch/key/wheel calls for input. It's the official route Android Studio
  uses, public, cross-platform, and needs no helper process. The cost is one
  new crate family (`tonic`) and generated code from the protos.
- **B. `adb` only** (`adb exec-out screencap`, `adb shell input`). It's
  simple, but slow (~2 fps), laggy input, and has no streaming. It's a
  fallback at most.
- **C. A small helper process** (as `oo-device-input` for iOS). There's no
  reason to: the API is public and reachable from Rust directly.

Pick A, with `adb` used only for the few controls gRPC lacks (appearance and
font scale: `adb -s emulator-<port> shell cmd uimode night yes|no`,
`settings put system font_scale`).

## Design

1. **Generated client, no protoc at build time.** Vendor the needed protos
   (`emulator_controller.proto`, and its imports) under
   `crates/harness-app/proto/android/` (Apache-2.0, keep their headers and add
   a NOTICE line), and check in the prost/tonic-generated Rust once
   (regenerated by a script, `script/gen-android-grpc.sh`). Builds and CI
   never need `protoc`.
2. **Discovery** (`devices/android/discovery.rs`, all platforms, pure parse +
   a directory scan): read the `running/*.ini` files, drop ones whose pid is
   gone, and produce `AndroidDevice { avd_id, name, grpc_port, token, serial,
   skin }`. AVDs that aren't running come from `emulator -list-avds` (cached;
   SDK located via `ANDROID_HOME` / `ANDROID_SDK_ROOT` / the platform's default
   path) so Boot can offer them.
3. **One device model for both platforms.** Generalise `DeviceInfo` with a
   `platform: Platform { Ios, Android }` and a platform-specific handle; the
   picker groups by platform; `tab_state` stays platform-agnostic (it already
   only sees ids, names and Live/Reduced/ShutDown).
4. **Frame source** (`devices/android/stream.rs`): a tonic client on a tokio
   runtime owned by the existing worker thread (the worker already owns the
   iOS stream; add a sibling for Android). `streamScreenshot` with
   `RGBA8888` at the panel's pixel size (re-requested on resize, debounced),
   frames into the same `FrameSlot` + wake path. macOS draws through the
   existing 420f `surface()` route (convert RGBA → 420f with the existing
   VideoToolbox converter); Windows draws an RGBA `RenderImage` and caps the
   redraw at 30 fps. Rotation: the stream reports it in `Image.format`; draw
   upright like iOS.
5. **Input** (`devices/android/input.rs`): reuse the UI's input mapping
   (`view_to_device`, swipe synthesis rules, focus and key routing) behind a
   small trait the iOS helper already fits; Android implements it with
   `sendTouch` (one touch id per finger, pressure 0 on up), `injectWheel` for
   wheel/trackpad (Android scrolls natively, so no synthesised swipe), and
   `sendKey` (USB HID codes the iOS mapping already produces, or `text` for
   ⌘V paste).
6. **Controls:** Home and Back via `sendKey` key names; Rotate via
   `setPhysicalModel` rotation (or the emulator's rotate key, whichever the
   spike finds reliable); Screenshot via `getScreenshot(PNG)`; Shut down via
   `setVmState(SHUTDOWN)` (with the same confirm); Boot via
   `emulator -avd <id> -no-window` (detached, logs to a temp file);
   Dark/Light and Text size via `adb shell` as above (hidden if `adb` is
   missing).
7. **Frame:** parse the skin's `layout` (a small brace format) for the
   display rect, corner radius and part offsets; decode `back.webp` /
   `mask.webp` (the `image` crate already in the tree has WebP decode —
   verify the feature); feed the existing overlay pipeline (`chrome.rs`'s
   placement + overlay cache) through a `FrameArt` that has an Android
   variant.
8. **Windows:** the Device tab shows on Windows when an Android SDK is found;
   the iOS parts stay `#[cfg(target_os = "macos")]`.

## Spike first (throwaway, gate)

On an AVD the spike boots headless itself (and stops afterwards; never
touches an emulator it didn't start):
1. Which auth a plain `emulator -avd X -no-window` accepts for
   `streamScreenshot`, `sendTouch`, `sendKey`, `setVmState`: the discovery
   file's `grpc.token` as `authorization: Bearer …`, or a JWK we drop into
   `grpc.jwks` (what Android Studio does). Also an emulator an agent started
   with no flags.
2. `streamScreenshot` RGBA8888 at ~400×890 and at full size: fps, latency,
   CPU (ours and qemu's).
3. Tap, swipe, wheel, typed text, Home/Back keys, rotation, shutdown — each
   verified by screenshot.
4. tonic + generated code builds in harness-app without new system deps; the
   lockfile delta.
5. Discovery directory on Windows (if a Windows box is available; otherwise
   from the emulator source) — record the path.

If auth can't be satisfied without Android Studio, stop and come back.

## Testing

Pure units: discovery ini parsing, skin `layout` parsing, the Android key
mapping, wheel conversion, platform grouping in the picker. Live
`#[ignore]` tests on a spike-booted AVD (`OO_TEST_AVD`), mirroring the iOS
ones. Visual check by the user in a Dev build (this host can't screen-
capture), plus Windows CI.

## Left to the builder

Module layout under `devices/android/`, tokio runtime placement, resize
debounce, exact picker grouping copy, which emulator key names to use, how
`emulator -list-avds` is cached.

## Open questions for the user (before planning)

1. Should Android emulators an **agent** started be shown and controllable
   exactly like iOS ones (yes by default, same as iOS)?
2. Windows: ship Android-only Device tab there in this phase, or keep Windows
   for later?
3. Add **Back** (and Recents?) to the bar for Android — Back only is my
   proposal.
