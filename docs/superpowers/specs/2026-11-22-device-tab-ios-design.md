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

## Spike findings (2026-11-22)

Measured on Xcode 27.2 (27B52), macOS 27 (Darwin 27.3), Apple Silicon, with
a throwaway Objective-C spike (the controller allowed ObjC instead of
`examples/sim_spike.rs`). The test device was created for the purpose
(`simctl create oo-device-test "iPhone 18 Pro"`, iOS 27.2, booted headless
with `simctl boot`) and deleted afterwards. **Result: the private route
works**, under the hardened runtime too, and needs only CoreSimulator.

### Loading

- `dlopen("/Library/Developer/PrivateFrameworks/CoreSimulator.framework/CoreSimulator", RTLD_NOW)`
  is enough. **SimulatorKit is not needed for the display**: the run that
  skipped the SimulatorKit `dlopen` got the same surfaces and frames. The
  Xcode path is used only as the developer dir string passed to the
  service context (`xcode-select -p`).
- After the `dlopen`, `objc_getProtocol("SimScreen")` is non-nil. Neither
  CoreSimulator nor SimulatorKit has a class that implements the
  screen-callback selector as an instance method. The objects you get are
  **ROCK remote proxies whose class names are generated at run time**, for
  example
  `ROCKRemoteProxy-0F58CBFD-…-SimScreen-SimDeviceIOPortDescriptorInterface-ROCKImpersonateable-SimDisplayIOSurfaceRenderable-SimDisplayResizeableRenderable-SimDisplayRenderable`.
  The UUID part changes between runs. Never look these classes up by name:
  test each instance with `respondsToSelector:` (and, if wanted,
  `conformsToProtocol:`). Both returned YES on every display descriptor.

### Call chain, UDID to screen (all instance methods unless marked +)

| Step | Receiver | Selector | Types (64-bit) | Returns |
|---|---|---|---|---|
| 1 | class `SimServiceContext` | + `sharedServiceContextForDeveloperDir:error:` | `@32@0:8@16^@24` (NSString*, NSError**) | `SimServiceContext` |
| 2 | context | `defaultDeviceSetWithError:` | `@24@0:8^@16` | `SimDeviceSet` |
| 3 | device set | `devicesByUDID` (also `devices`) | `@16@0:8` | `NSDictionary<NSUUID*, SimDevice*>` |
| 4 | `SimDevice` | `state` `Q`, 3 = Booted; `stateString`; `name`; `UDID` | | |
| 5 | `SimDevice` | `io` | `@16@0:8` | `SimDeviceIOClient` (a `SimDeviceIO`, `<SimDeviceIOProtocol>`) |
| 6 | io | `ioPorts` | `@16@0:8` | `NSArray` of 15 port proxies (`<SimDeviceIOPortInterface>`) |
| 7 | port | `portIdentifier` (NSString), `ioPortClass` (`S`, 1 = display), `uuid` (NSUUID), `descriptor` | | descriptor proxy |
| 8 | display descriptor | `state` | | `ROCKImmutableProxy` `<SimDisplayDescriptorState>`: `displayClass` `S`, `defaultWidthForDisplay`/`defaultHeightForDisplay` `I`, `defaultPixelFormat` `I`, `mask` (NSURL) |
| 9 | display descriptor | `screenProperties` | | `<SimScreenProperties>`: `pixelSize` `{CGSize=dd}`, `screenID` `I`, `screenType` `Q`, `uiOrientation` `I`, `powerState` `i`, `name`, `currentMode` (`<SimScreenMode>`: `pixelFormat` `I`, `refreshRate` `I`, 6000 = 60 Hz) |
| 10 | **display descriptor = the screen** | `registerScreenCallbacksWithUUID:callbackQueue:frameCallback:surfacesChangedCallback:propertiesChangedCallback:` | `v56@0:8@16@24@?32@?40@?48` | void |
| 11 | same | `unregisterScreenCallbacksWithUUID:` | `v24@0:8@16` (NSUUID*) | void |

So the object that implements the selector is **the display port's
`descriptor`** (protocol `SimScreen`, from `CoreSimDeviceIO.framework`). The
same proxy also answers `SimDisplayIOSurfaceRenderable`
(`framebufferSurface`, `maskedFramebufferSurface`, and idb's older
`registerCallbackWithUUID:ioSurfacesChangeCallback:`) and
`SimDisplayRenderable` (`displaySize` = 1206×2622 on the main display).

**Choosing the main display.** The phone has four
`com.apple.framebuffer.display` ports: main LCD, TVOut 720×480, Wireless
720×480 and Resizable 7680×4320. The rule that works:
**`descriptor.state.displayClass == 0`**. Exactly one port matched. Every
other discriminator agreed: `screenProperties.screenType == 0`
("Integrated"), `screenID == 1`, `name == "LCD"`, `powerState == 1` (the
external ones read 0), a non-nil `state.mask` (the PDF corner mask), and a
non-zero `displaySize`. Port order is not stable enough to rely on (main
was 5th). The `com.apple.framebuffer.server` port's descriptor
(`SimScreenAdapter`) has `enumerateScreensWithCompletionQueue:completionHandler:`.
It returns the same four proxies, identical objects, so it's an alternative
path to the same screen but adds nothing.

### Callback signatures (from `_protocol_getMethodTypeEncoding`, confirmed by calls)

- `frameCallback`: `void (^)(void)`. Block signature `v8@?0`.
- `surfacesChangedCallback`: `void (^)(IOSurface *framebuffer, IOSurface *masked)`.
  Signature `v24@?0@"IOSurface"8@"IOSurface"16`. The variant without class
  names, `v24@?0@8@16`, also works.
- `propertiesChangedCallback`: `void (^)(id<SimScreenProperties>)`. Signature
  `v16@?0@8`.
- `callbackQueue`: a `dispatch_queue_t`. **The frame and surfaces
  callbacks arrive on the queue you pass.** That was confirmed with a
  labelled serial queue (label read inside each callback, never on the main
  thread) and with the main queue (label `com.apple.main-thread`,
  `pthread_main_np() == 1`). `propertiesChanged` never fired, so its queue
  is unconfirmed; assume it's the same one.
- **The blocks must carry a signature** (`BLOCK_HAS_SIGNATURE`, flag 1<<30).
  A block without one makes the register call throw
  `NSInternalInconsistencyException: Block is missing signature field`,
  synchronously, as an Objective-C exception. From Rust: build the blocks
  with `block2::RcBlock::with_encoding` and a `ManualBlockEncoding` that
  carries the strings above (block2 0.6.2 is in
  `crates/harness-app/Cargo.lock`). Wrap the `msg_send` in
  `objc2::exception::catch`, or an exception aborts the app. That needs
  objc2's `exception` feature, which harness-app doesn't enable today
  (`objc2 = "0.6"`, objc2 0.6.3, no `objc2-exception-helper` in the
  lockfile), so turning it on adds that one small crate. The object
  arguments must be encoded as `@`: with `^v` (raw pointer) in the
  signature, registering succeeded and frames arrived, but **the surfaces
  callback was silently never called**.

### Observed behaviour

- **Surfaces arrive at once.** `surfacesChanged` ran on the callback queue
  less than 1 ms after register returned (asynchronously, never inside the
  call). Both surfaces were non-nil, 1206×2622, `'BGRA'`, bytes per row 4864
  (padded; 1206×4 = 4824). That matches `screenProperties.pixelSize` and
  `simctl io enumerate`. The objects are of class `IOSurface` (toll-free
  bridged to `IOSurfaceRef`); retain them to keep them past the callback.
- **Which surface is the picture.** Arg 1 has the same `IOSurfaceGetID` as
  `framebufferSurface`: the full rectangular picture, opaque (alpha 255
  in the corners). Arg 2 matches `maskedFramebufferSurface`: **the same
  picture with the device's rounded-corner mask applied** (alpha 0 outside
  the screen shape, identical pixels inside). Both were dumped to PNG and
  checked by eye. Both are updated in place on every frame: same IDs all
  session, `IOSurfaceGetSeed` moves on both in step. **Use arg 1** for the
  420f route, because 420f has no alpha (the masked corners would
  presumably convert to black; not run). Arg 2 suits a native layer that
  wants rounded corners.
- **Frames.** With nothing changing on screen: 0 frame callbacks, 0.0% CPU.
  During the `simctl ui appearance` loop: bursts of 3–6 callbacks about
  every 0.3 s, about 16 per second, stopping between toggles. With a
  continuous CSS animation in Safari on the device: a steady **60 per
  second**, the display's 6000 = 60 Hz. About 6% of callbacks arrive with
  no new seed (up to four within 0.3 ms), so coalescing to the latest frame
  costs nothing.
- **No surface replacement** happened during toggles or animation, and
  `propertiesChanged` never fired: there was no size or orientation change,
  and it doesn't fire on register.
- **Unregister** stops the callbacks: 0 or 1 more in the following 3 s
  while the device kept animating, across runs. **A callback already queued
  can still run just after `unregisterScreenCallbacksWithUUID:` returns.**
  Keep the blocks' captured state alive (e.g. an `Arc` with a "stopped"
  flag) and ignore late calls; don't free what they touch on unregister.
- **Device shutdown while registered**: `surfacesChanged(nil, nil)` fires
  on the queue, frames stop, nothing throws. Afterwards `screenProperties`
  reads zeros, and `unregisterScreenCallbacksWithUUID:` on the dead device
  is harmless. Treat nil surfaces as "device gone".
- **Doesn't alter the device.** Before and after every run (register,
  frames, unregister): pixel size 1206×2622, orientation 1 (portrait) and
  power state 1, all unchanged. `simctl io enumerate` diffed before and
  after shows only `Seed:` changing. That counter rises by 2 on every
  client connection, including `simctl io enumerate` itself, so it isn't a
  device change. No mode, power or resize selector was ever called.

### Hardened runtime

A copy of the spike was signed with
`codesign --force --options runtime --entitlements crates/harness-app/resources/openOrchestra.entitlements --sign -`
(`flags=0x10002(adhoc,runtime)`). It loads CoreSimulator and SimulatorKit,
gets surfaces and runs at 60 frames per second, and routes (a) and (b) both
work. Library validation is really enforced in this configuration: an
ad-hoc-signed test dylib was refused by the same kind of binary ("mapping
process and mapped file (non-platform)"). The Apple-signed frameworks
(Software Signing, team 59GAB85EFG) pass. Not yet re-checked on a real
Developer ID `notarize-mac.sh --sign-only` build, which should behave the
same, since Apple-signed code is accepted whatever the team.

### Cost of the two drawing routes (the spike process's CPU from `getrusage`; GPU from ioreg `AppUsage.accumulatedGPUTime` on the AGX user clients the process created, used here in place of Activity Monitor's GPU column)

| Route | Idle | Toggle loop (~16 fps) | Sustained 60 fps | CPU per frame | Process GPU |
|---|---|---|---|---|---|
| Registration only (no drawing) | 0.0% | 0.2% | 0.8% | 0.13 ms | 0 |
| (a) `VTPixelTransferSession` BGRA→420f, full size, pooled `CVPixelBuffer` (IOSurface + Metal compatible) | 0.0% | 1.2% | 6.7–7.7% (3 runs) | 1.1–1.3 ms | 0 |
| (a′) same, downscaled to 402×874 | 0.0% | 1.2% | 7.1% | 1.2 ms | 0 |
| (b) bare `NSWindow`, layer `contents` = IOSurface, `setContentsChanged` per frame | 0.04% | 0.4% | 2.7–3.1% (3 runs) | 0.44–0.51 ms | 0 |

Percentages are of one core. Hardened-runtime copies gave the same figures
(a 7.0%, b 2.8%).

- Measured fact: the spike's AGX (GPU) client recorded **zero GPU time**
  in route (a). During (a) the process also holds
  `IOSurfaceAcceleratorClient` connections, which is consistent with
  VideoToolbox doing the conversion on the hardware scaler rather than the
  GPU. No no-VT control was run for that client. Downscaling in the
  transfer saves no CPU.
- `setContentsChanged` is CALayer SPI (not in the headers; call it with
  `msg_send`). The public alternative is to set `contents` again each frame.
- **Not measured:** WindowServer's GPU time was pure noise (58–269 ms/s
  with nothing changed, because other apps are on screen), so nothing could
  be attributed to route (b)'s compositing. Worse, the Mac's screen was
  locked during the spike (`CGSSessionScreenIsLocked = 1`), the test
  window's `occlusionState` was not visible, and WindowServer may not have
  composited it at all. (b)'s real cost is its 3% plus compositing that
  wasn't measured. (a)'s figure is a lower bound too: it doesn't include
  gpui repainting the window on every frame.

### Device Hub

Device Hub (process `DeviceHub`, `com.apple.dt.Devices`) **was already
running**, the user's own, with its main window open since before the
spike. So every run above coexisted with it, and the "Device Hub closed"
case was not tested: quitting the user's app was off-limits. Nothing in
any run changed the test device's size, orientation or power, and frames
kept flowing at 60 per second. Whether Device Hub was showing *this* device
at the time is **unverified**: the screen was locked, so its window
couldn't be inspected. Focus (does watching change which window or
device has focus) was not checked either. An `open -g` with
`-CurrentDeviceUDID` went to the
running instance and opened no new window (window numbers confirm it). The
"Device Hub shows this same device" case is still open for the manual
test.

### Route chosen

**(a) Convert to 420f with `VTPixelTransferSession` and draw with gpui's
`surface()`.** The plan's gate, verbatim: "If (a) costs clearly more than
(b), more than ~10% of one core during motion, stop and report: Task 5
switches to the native-layer route". At the worst case measured, a
sustained 60 fps, (a) passes on either reading of that sentence: about 7%
of a core in absolute terms, and about 4 points more than (b) (≈3%). Both
are 0 while idle. (a) also avoids the z-order and focus problems of native
views. **Condition for
Task 5:** measure the whole app (conversion plus gpui repainting the window
at 60 fps) with the Device tab live. If that exceeds the budget, switch to
(b), a `CALayer` whose `contents` is the framebuffer IOSurface (arg 2 if
rounded corners are wanted), positioned like `browser_pane/webview.rs`.

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
