# Device tab (iOS simulators) — design

Date: 2026-11-22. Status: approved 2026-11-22; being implemented (plan: docs/superpowers/plans/2026-11-22-device-tab-ios.md).

Phase 1 of 2. Phase 2 (Android emulators) gets its own design after this
ships; the notes it needs are at the end.

## Why

Agents in openOrchestra build and run iOS apps: they boot a simulator with
`simctl` or `xcodebuild`, install the app and drive it. Since Xcode 27 the
only window onto a simulator is Device Hub (Simulator.app is gone), a
separate app that shows every device in one window and has nothing to do
with the task you're looking at. The user wants to **see what the agents are
doing in the simulator** without leaving openOrchestra: a live picture in the
right-hand tools panel, next to Files, Git, Review and Ports.

## Goals

- A **Device** tab in the tools panel (`ToolTab::Device`) showing a live,
  **watch-only** view of a booted iOS simulator.
- A **picker** listing every booted simulator on the Mac (name, OS version),
  so the user chooses which one to watch. No per-task attribution: the user
  finds the task that is using the device themselves.
- **The view stays put while the user moves around.** In the user's words,
  it lives in the right-hand tools panel "so they can navigate to the tab
  that is working with the emulator": switching tasks, worktrees or center
  tabs must not change the selected device or restart the stream. Selection
  is **per window, not per task** (unlike Ports, which is task-scoped).
- Smooth: frames arrive as the simulator draws them (target: the simulator's
  own refresh rate), from Apple's private simulator display API, the same
  path Device Hub, `idb` and Radon IDE use.
- Works on simulators an agent booted headless (no Device Hub window open).

## Non-goals (v1)

- **Input.** No taps, typing, gestures, rotation, hardware buttons. (The
  private HID path, `SimDeviceLegacyHIDClient` in SimulatorKit, exists and is
  the obvious next step, but not now.)
- **Booting, shutting down or creating simulators.** The tab watches what is
  already running; with nothing booted it says so ("No simulator running").
- **Which task owns which device.** iOS simulators run under `launchd_sim`
  (parent pid 1), not under the agent that booted them, so the process-tree
  attribution the Ports tab uses can't work. See TODO.
- Simulators outside the default device set (custom `--set`, the clones
  `xcodebuild test` makes under `XCTestDevices`).
- External/secondary simulator displays, CarPlay, watchOS/tvOS/visionOS
  specifics. iPhone and iPad main display only; other device families may
  work but aren't a goal.
- Device bezels/chrome, recording, screenshots-to-file.
- Remote control (phone) sharing of the device view.
- Windows: the tab doesn't appear there in phase 1 (iOS is macOS-only).

## Anti-goals (a failure even if it works)

- **The app crashes or hangs because Xcode changed.** The private API moves
  between Xcode releases. A missing class or selector must degrade the tab,
  never take the app down. (See "Fallback".)
- **CPU or GPU cost while nobody is looking.** With the Device tab not
  visible (other tool tab, panel closed, window minimised), no frames are
  received or drawn and the device list isn't polled. Same rule as the Ports
  tab.
- **It changes the simulator.** Watching must not boot, resize, rotate,
  focus or otherwise alter the device the agent is driving.

## Constraints found in the code and on the machine

- Tools tabs: `ToolTab` in `crates/harness-app/src/workspace/prefs.rs`
  (persisted, `serde(rename_all = "lowercase")`); strip and body in
  `crates/harness-app/src/workspace/ui/tool_panel.rs`
  (`render_tool_tab_strip`, `render_tool_body`). Ports
  (`ui/ports_panel.rs`, spec `2026-11-18-ports-panel-design.md`) is the
  model for a tab that samples only while visible.
- Measured on Xcode 27.2 (27B52), macOS 27:
  - `CoreSimulator.framework` is at `/Library/Developer/PrivateFrameworks/`.
  - `SimulatorKit.framework` is at
    `$(xcode-select -p)/../SharedFrameworks/SimulatorKit.framework`
    (no longer under `Developer/Library/PrivateFrameworks`). Two Xcodes are
    installed (`Xcode.app`, `Xcode-beta.app`): use the selected one.
  - The **candidate** frame API is the selector
    `registerScreenCallbacksWithUUID:callbackQueue:frameCallback:surfacesChangedCallback:propertiesChangedCallback:`,
    whose surfaces callback hands over two `IOSurface`s. Its string appears
    in exactly two binaries: `CoreSimDeviceIO.framework` (inside
    `CoreSimulator.framework/Versions/A/Frameworks/`, which also names the
    protocols `SimScreen`, `SimDisplayIOSurfaceRenderable`,
    `SimDisplayRenderable` and the class `SimDeviceIOPortDescriptor`) and
    SimulatorKit (which exports `SimDeviceScreen`,
    `SimDisplayRenderableView`, `SimDeviceLegacyHIDClient`). **Which object
    implements it, and how to reach it from a UDID, is unverified**: the
    likely path is a display port's descriptor conforming to
    `SimScreen`/`SimDisplayIOSurfaceRenderable`, but the spike must confirm
    it.
  - No public alternative: `xcrun devicectl` (Xcode 27.2) has no screen,
    display or stream command, and `simctl io` offers only screenshots and
    recording to a file.
  - `simctl io <udid> enumerate` lists several `com.apple.framebuffer.display`
    ports per device (main plus external/virtual displays); BGRA pixel format.
  - `simctl io screenshot --type=jpeg` takes ~0.4 s per shot (≈2.5 fps).
- gpui (pinned rev e8bc16a) has a `surface(CVPixelBuffer)` element, but its
  Metal renderer only accepts `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange`
  ("420f", full range; **not** "420v", video range). Anything else fails an
  `anyhow::ensure!` in the surface draw (`gpui_apple/src/metal_renderer.rs`
  ~1411, "unsupported surface pixel format"), i.e. the frame's draw errors
  out. The simulator hands out **BGRA**, so its surface can't go to
  `surface()` as is.
- The browser tab already places a native `NSView` (WKWebView) over gpui
  layout bounds and keeps it in step (`crates/harness-app/src/browser_pane/`),
  so a native view is an available, proven pattern too.
- objc2 0.6 (+ app-kit, foundation, block2) is already in the lockfile.
- Hardened runtime, no App Sandbox
  (`crates/harness-app/resources/openOrchestra.entitlements`). Loading
  Apple-signed frameworks from Xcode at run time should pass library
  validation; this must be verified on a `script/notarize-mac.sh --sign-only`
  build, not assumed.
- Xcode is optional for openOrchestra users. Without it (or without
  CoreSimulator) the tab still appears and explains what it needs.

## Approaches considered

- **A. Public tools only.** Poll `simctl io screenshot` (~2–3 fps). Robust
  across Xcode updates, little code; choppy.
- **B. Private simulator display API (chosen).** Register for the device's
  screen `IOSurface`s through SimulatorKit/CoreSimulator; draw frames as they
  come. Smooth, zero-copy, and the groundwork input will need later. Breaks
  when Apple changes the private API, so it needs a guarded load and a
  fallback.
- **C. `simctl io recordVideo` to a stream, decoded live.** Public and
  smoother than A, but streaming isn't a documented use, latency unknown;
  most of B's complexity without B's quality. Dropped.

The user picked **B** (2026-11-22).

## Design

### Pieces

1. **Device list** (`devices::ios`, pure where possible). Booted simulators
   in the default set: UDID, name, runtime/OS version, device type. Source is
   the builder's call: `xcrun simctl list devices booted -j` polled every
   few seconds while the tab is visible, or CoreSimulator's device-set
   notifications once the framework is loaded. Parsing is pure and unit
   tested from captured JSON.
2. **Frame source** (macOS only, behind a small trait so Android can slot in
   later):
   - Loads CoreSimulator and SimulatorKit with `dlopen` at first use, from
     the selected Xcode; looks classes and selectors up with
     `respondsToSelector:` / `NSClassFromString` before calling anything.
     Any missing piece → a typed "unsupported on this Xcode" error, never
     an unrecognised-selector exception.
   - Resolves the device by UDID through the service context / default
     device set, picks its **main** display port, and registers screen
     callbacks. The surfaces-changed callback delivers the current
     `IOSurface`; the frame callback says a new frame is ready.
   - Hands frames to the UI on the main thread and coalesces: at most one
     redraw per display refresh, latest frame wins.
   - Unregisters on stop. Stops when the tab is hidden, the device is
     deselected, or the device shuts down.
3. **Device tab UI** (`ui/device_panel.rs`). Tab strip label "Device", no
   count (a count would need polling while the tab is hidden, which the
   anti-goals forbid). Body:
   picker at the top, the screen below scaled to fit the panel preserving
   aspect ratio; maximising the tools panel gives a bigger view. States:
   no Xcode / no simulator running / device shut down / unsupported Xcode
   (fallback active) / live.

### Getting the BGRA surface on screen — builder's call, decided by the spike

- **Convert to 420f and use gpui's `surface()`**: a
  `VTPixelTransferSession` (GPU) converts each BGRA `IOSurface` into a
  pooled `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange` `CVPixelBuffer`
  (exactly that constant; video-range 420v fails gpui's check). Stays inside gpui (no z-order issues with
  menus and dialogs over the panel); costs one GPU conversion per frame and
  a slight colour-conversion loss.
- **Native layer view**: an `NSView` whose `CALayer.contents` is the
  `IOSurface`, positioned from gpui bounds like the browser tab's
  WKWebView. Zero-copy and exact colours; inherits the browser tab's
  native-view issues (overlap with gpui-drawn popovers, focus).

Pick after the spike measures both; record the choice and the numbers in
this doc.

### Fallback (my call, keeps B's fragility survivable)

If the private route fails to load or register on this Xcode, the tab falls
back to approach A: poll `simctl io <udid> screenshot --type=jpeg` while
visible (one in flight at a time, ~2 fps) and show a small "Reduced frame
rate — this Xcode's simulator display API isn't supported" note. The
picture keeps working after an Xcode update; only smoothness drops.

### Spike first (throwaway, before the plan's real tasks)

A standalone binary in `crates/harness-app/examples/` (or a scratch
crate) that:

1. dlopens both frameworks from the selected Xcode, finds the booted
   device, **names the class that implements the screen-callback selector
   and the chain from UDID to it** (service context → device set → device
   → IO port → descriptor, or whatever it really is), registers callbacks,
   and logs surface size, pixel format and frames per second for 10 s
   while something animates on the device;
2. shows it does not alter the device (no boot, no resize, no focus
   change), with and without Device Hub open;
3. runs under a hardened-runtime signature (`notarize-mac.sh --sign-only`
   flow) to confirm library validation lets the frameworks load;
4. tries both display routes above and records CPU/GPU cost per route.

If 1 or 3 fails, stop and come back: the plan changes (e.g. a separate
helper process, or A as the main route).

### Testing

- Unit: device-list parsing, display-port choice, frame coalescing, the
  state machine (visible/hidden, selected/shut down, fallback) with a fake
  frame source.
- Manual (isolated scratch `.app` + HOME, per the project's visual-check
  rule; never the user's running Dev app): live view of a headless
  simulator; Device Hub open at the same time; device shut down mid-view;
  tab hidden → CPU back to idle; fallback forced by a debug env var.
- CI: the frame source is macOS-only and needs a booted simulator, so it
  isn't exercised in CI; everything else is. Windows build must still pass
  with the tab compiled out.

## What's left to the builder

- Device-list source (polling `simctl` vs CoreSimulator notifications).
- The display route (decided by the spike, above).
- Module layout, naming, how the picker looks, whether the selected UDID
  survives a relaunch (it must survive task/tab switches, per Goals; a
  stale UDID falls back to the first booted device).
- A CHANGELOG `Unreleased` line with the user-visible commit.

## Phase 2: Android (separate design, after this ships)

Notes so far: every running emulator writes
`~/Library/Caches/TemporaryItems/avd/running/pid_<pid>.ini` (Windows has an
equivalent) with `grpc.port` and `grpc.token`; an agent-started emulator
(35.2.10, no flags) already serves gRPC on 8554. `streamScreenshot` in
`emulator_controller.proto` (shipped in the SDK) gives a live stream; ask
for frames at panel size, since full-size raw frames are ~11 MB. Works on
Windows too. Same Device tab and picker, second frame source.

## TODO recorded

`docs/TODO.md`: give each task its own simulator/emulator (openOrchestra
tells agents which device to use), which makes the Device tab able to show
"this task's device" exactly.
