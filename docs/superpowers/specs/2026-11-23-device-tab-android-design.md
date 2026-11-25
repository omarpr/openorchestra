# Device tab: Android emulators — design

Date: 2026-11-23. Status: decisions recorded 2026-11-23 (see "Decisions"); ready for planning (phase 2 of the Device
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
- **macOS only in this phase** (user, 2026-11-23). The Android path is public
  and cross-platform, so keep pure Android modules platform-neutral where
  it's free, but the Device tab stays macOS-only; Windows is a later phase.

## Non-goals

- Physical Android devices over adb (later, maybe).
- Creating, deleting or editing AVDs; installing system images.
- Multi-display, foldables' posture UI, sensors, GPS, phone/SMS, snapshots.
- Starting adb servers or managing adb ourselves beyond what the controls
  need.
- The Device tab on Windows (later phase).
- A Recents / app-switcher button (Back only).

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

Since the tab stays macOS-only this phase, Design §4's Windows `RenderImage`
path isn't needed now: draw through the existing 420f `surface()` route.

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
8. **Windows:** not in this phase. Pure Android parsing compiles everywhere;
   the rest is `#[cfg(target_os = "macos")]` like the iOS parts; the Windows
   build must still pass.

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

## Decisions (user, 2026-11-23)

1. Emulators an agent started are shown and controllable exactly like iOS
   simulators (same picker, input only on the user's click).
2. Windows: not in this phase; the Device tab stays macOS-only.
3. Android-only button: **Back** (no Recents).

## Spike findings (2026-11-24)

Run on `Medium_Phone_API_35` (emulator 35.2.10, arm64, API 35, Google Play
image), booted headless by the spike four times (`-no-window
-no-snapshot-save -no-audio`, adding `-gpu host` from boot 2 on), and
stopped each time. Spike sources (a tonic client and a generator) live in
`.superpowers/sdd/2026-11-23-device-tab-android/spike/` (git-ignored).
**Gate: passed.** Plain launches accept the discovery file's bearer token
for every method, and the stream keeps up at about 59 fps.

1. **Discovery.** Running emulators are listed in
   `~/Library/Caches/TemporaryItems/avd/running/pid_<pid>.ini`, plus a
   `<pid>/jwks/<uuid>/` dir. `<pid>` is the `qemu-system-aarch64-headless`
   pid; the `emulator` launcher execs into it. The fields are
   `emulator.build`, `port.serial` (5554), `port.adb` (5555), `avd.id`
   (`Medium_Phone_API_35`), `avd.name` (`Medium Phone API 35`), `cmdline`,
   `grpc.token` (88 chars), `grpc.allowlist`, `avd.dir`, `grpc.port`
   (8554), `grpc.jwk_active`, `grpc.jwks` and `emulator.version`.
   Use `avd.id` as the id and `avd.name` for display.
   - `setVmState(SHUTDOWN)` exits the process in about 2 s and removes
     the ini and `<pid>/` dir.
   - After `kill -9`, the ini and dir stay behind (stale). The next
     emulator boot sweeps them.
   - **Discovery must check each pid is alive.**
   - `emulator -list-avds` takes about 25 ms, so caching hardly matters.
2. **Auth.** The log says `auth: +token` and warns that "Basic token auth
   should only be used by android-studio", but it works.
   - **No token:** every method (`getStatus`, `getScreenshot`,
     `streamScreenshot`, `sendTouch`, `sendKey`, `injectWheel`,
     `setPhysicalModel`, `setVmState`) fails with `Unauthenticated`
     ("Missing the 'authorization' header").
   - **Wrong token:** fails with `Unauthenticated`.
   - **`authorization: Bearer <grpc.token>`:** all of them work.
   - **JWT:** also works for all of them, through a JWK set
     `{"keys":[…]}` written as `<grpc.jwks>/<name>.jwk`, which the emulator
     picks up within 2 s and merges into `active.jwk`. ES256 and RS256
     both work. The token used `iss: "android-studio"`, `iat` and `exp`
     (whether each claim is required wasn't tested). It must have **no
     `typ` header** (rejected: "token has type header set").
   - **Decision for Task 1:** use the bearer token from the ini; no
     JWK/JWT and no crypto crates. The JWK route stays as a fallback if a
     future emulator drops bearer.
   - Not tested: an emulator an agent starts with no flags (windowed).
     The spike only boots headless; the earlier note that such an
     emulator serves gRPC on 8554 still stands.
3. **Client build.**
   - **Crates:** `tonic = { version = "0.14", default-features = false,
     features = ["channel", "codegen"] }` (`codegen` is required; the
     generated client uses `tonic::codegen::*`), `tonic-prost = "0.14"`,
     and `prost = "0.14"`. All resolve to 0.14.9 / prost 0.14.4. No TLS,
     no server, plain `http://127.0.0.1:<grpc.port>`.
   - **Generation:** `tonic-prost-build` 0.14
     (`default-features = false`, `.build_server(false)
     .build_transport(false)`) generates code from
     `emulator_controller.proto` alone. Its only import is
     `google/protobuf/empty.proto`, which maps to `()`. The output is one
     file, `android.emulation.control.rs`, about 146 KB. It builds and is
     clippy-clean with no `protoc` on PATH. That was tested in the spike
     crate on harness-app's pinned 1.98.1 toolchain; inside harness-app,
     only the deps were checked, not the generated module.
   - **Lockfile delta** (from `cargo check` in `crates/harness-app` with
     the three deps added; it built in 28 s and was then reverted): four
     new crates, `tonic 0.14.9`, `tonic-prost 0.14.9`,
     `hyper-timeout 0.5.2` and `tokio-stream 0.1.19`. `hyper-util` gains
     `client-legacy`, whose deps are all already in the tree.
   - Set `max_decoding_message_size` to at least 16 MiB. A full RGBA frame
     is 10.4 MB, above tonic's 4 MiB default.
4. **Stream.**
   - **Pixel order:** `streamScreenshot` RGBA8888 rows arrive **top-down**,
     despite the proto comment saying "bottom up".
   - **Requested size is a bounding box.** The aspect ratio is kept:
     400×890 gives 400×888 in portrait and 400×180 in landscape. Take the
     real size from `Image.format.width/height`, never from the request.
     One stream continues across a rotation and simply changes shape, so
     re-request with a swapped box on rotation (or request a square box)
     to keep resolution.
   - **Frames arrive only on change.** A static screen sends one frame,
     then nothing; scrolling runs at the display rate. Frame count roughly
     equalled the last `seq`, so few or no frames were dropped.
   - **Measured while scrolling,** 10 s runs (CPU in cores):

     | GPU | size | fps | latency, median / p95 | our CPU | qemu, no stream → with stream |
     |---|---|---|---|---|---|
     | `swiftshader` (default headless) | 400×888 | 58.8 | 4.5–5 ms / 5.5 ms | 0.04 | 4.1 → 5.6–5.9 |
     | `swiftshader` | 1080×2400 | 58 | 11–12 ms / 14 ms | 0.25 | 4.1 → 7.3–7.7 |
     | `host` | 400×888 | 58.6 | 2.3 ms / 3.5 ms | 0.03 | 0.87 → 0.6–1.2 |
     | `host` | 1080×2400 | 58.4 | 6 ms / 7 ms | 0.22 | 0.87 → 1.46 |

     Latency is from the emulator's `timestampUs` to our receipt. RGB888
     costs the same as RGBA. qemu at idle uses 0.48 cores on
     SwiftShader and 0.12 on host GPU.
   - **Decisions for Tasks 2 and 6:**
     - Request RGBA8888 at the panel's pixel size.
     - **Boot with `-gpu host`.** It works headless on Apple silicon
       (renderer "Apple M5 Pro"), boots in 13–14 s instead of 27 s, and
       costs about a fifth of the CPU.
     - Agent-started headless emulators may still be on SwiftShader. That
       is their cost, not ours.
5. **Touch** (`sendTouch`).
   - **Coordinates are device pixels in the natural (portrait) frame,
     1080×2400.** Verified in portrait and Landscape only. In landscape
     we sent (300, 200) and the pointer_location overlay showed it at
     (200, 780) of the 2400×1080 landscape frame. So `view_to_device`
     must un-rotate. For Landscape (z = 90): `x = W − ly`, `y = lx`,
     where W is the portrait width. ReverseLandscape and ReversePortrait
     are presumed to work the same way but weren't verified.
   - Tap = a `pressure > 0` event, then the same point with `pressure 0`.
     Swipe = a series of `pressure > 0` moves, then a release. Both
     verified: the app drawer opened, the Settings list scrolled, and
     taps opened Settings and its search.
   - The guest's touch devices report a 0–1024 pressure range, and they
     advertise `BTN_STYLUS` and a STYLUS source whatever we send. After
     our first taps (`pressure 1`, no size), Gboard showed its "Try out
     your stylus" prompt. Its floating stylus toolbar stayed even after
     we switched to `pressure 1024` and `touch_major/minor` 10, so the
     cause is unverified. It didn't stop typing.
   - **Decision:** send `pressure 1024` (matching the device's range),
     and identifier = finger id.
6. **Keys** (`sendKey`).
   - **`key: "GoHome"`** goes Home and **`key: "GoBack"`** goes Back
     (verified). `"Home"` and `"Back"` are accepted but do nothing useful
     (`"Home"` is the keyboard Home key).
   - `eventType: keypress` and single-character key names (`"x"`) type
     text.
   - **USB codes must be Chromium-style `0x07 << 16 | usage`.**
     `0x070004` types "a"; the bare usage `4` does nothing. The iOS
     mapping's HID usages need `| 0x70000`.
   - **`text`** types ASCII including uppercase, punctuation and spaces.
     **Non-ASCII characters are dropped** ("a é b" gave "a  b").
   - **A `text` call over budget is silently dropped whole, with an OK
     reply.** The budget is 15 units, where an unshifted character costs
     1 and a shifted one costs 2. Examples: 15 lowercase letters work and
     16 fail; 7 uppercase letters or `!` work and 8 fail;
     `abcdefghijklmN` works and `abcdefghijklmnO` fails.
   - Back-to-back calls of 15 characters each arrive in order, intact.
   - **Decision:**
     - Typing sends one keystroke per call: a key name, or a USB
       keydown/keyup.
     - **⌘V paste uses `setClipboard(text)` and then Ctrl+V**, sent as
       USB keydown `0x0700E0`, keydown/keyup `0x070019`, keyup
       `0x0700E0`. Verified with a 30-character string that includes "é".
     - Any `text` fallback must chunk by the 15-unit budget (7 characters
       is always safe).
7. **Wheel** (`injectWheel`). The call returns OK but **does nothing on
   this phone AVD** (tested headless, on one AVD): no evdev events, no
   scroll, with or without a prior `sendMouse` to position the pointer,
   at dy = ±1 and ±120. The likely cause is that the guest has no mouse
   or rotary device, only `virtio_input_multi_touch_*`, `gpio-keys` and
   `qwerty2`.
   - **Decision:** wheel and trackpad scrolling must use synthesised
     swipes via `sendTouch`, like the iOS path; Design §5 is wrong on
     this.
   - Units and direction could not be measured.
8. **Rotation.**
   - **`setPhysicalModel(ROTATION, [0, 0, z])`** works without adb.
     z = 90 gives `Landscape`, −90 `ReverseLandscape`, 180 is reported as
     −180 `ReversePortrait`, and 0 `Portrait`.
   - `adb emu rotate` also works, stepping z by −90 each call.
   - **Decision:** use `setPhysicalModel`, stepping z by 90 within
     [−180, 180].
   - `Image.format.rotation.rotation` reports the current `SkinRotation`,
     and `getPhysicalModel` reports the angles.
   - **Frames come pre-rotated to the viewer's orientation.** Landscape is
     a 2400×1080 image. A rotating app re-lays out upright, but a
     portrait-locked app (the launcher) shows sideways or upside down,
     exactly as on a turned physical phone.
   - **Decision:** draw the image as-is and rotate only the device frame
     art by `format.rotation`.
   - The initial physical model is `[-4.75, 0, 0]` (tilt), still
     Portrait.
9. **adb.** `adb -s emulator-<port.serial>` (`emulator-5554`) at
   `<sdk>/platform-tools/adb`. Both verified by screenshot:
   - `cmd uimode night yes|no` turns dark mode on and off.
   - `settings put system font_scale 1.15` enlarges text.
   - Read state back with `cmd uimode night` (prints `Night mode: yes|no`)
     and `settings get system font_scale`.
   - These settings persist in the AVD's userdata (`-no-snapshot-save`
     doesn't undo them). The spike restored night=no, font_scale=1.0 and
     pointer_location.
10. **Frame art.**
    - `Medium_Phone_API_35` has `skin.name = 1080x2400` / `skin.dynamic =
      yes`, so **no frame art**. Task 5 must handle AVDs without a skin
      dir: no frame, plain rounded screen.
    - `Pixel_9_Pro_API_35` has `skin.path = <sdk>/skins/pixel_9_pro`.
    - **WebP:** `image` 0.25.10 in harness-app's lockfile has the `webp`
      feature on (`image-webp` 0.2.4), but only because gpui, gpui_macos
      and gpui_apple enable it. harness-app itself declares `image` with
      `default-features = false`. **Decision for Task 5:** add
      `features = ["webp"]` to harness-app's `image` dependency rather
      than relying on gpui.
11. Not verified: the Linux and Windows discovery dirs, and physical
    devices. Out of scope this phase.
