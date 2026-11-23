# Device tab test controls — design

Date: 2026-11-23. Status: approved 2026-11-23.

Follows `2026-11-22-device-tab-ios-design.md` (the watch-only Device tab).

## Why

The user watches agents' apps in the Device tab and wants the handful of
controls that make sense **to test the app** without leaving
openOrchestra: rotate it, go home, flip dark mode, change text size, grab a
screenshot. Not taking over the app (no tapping or typing), and not a
remote control for every simulator feature.

## Goals

- A **bottom bar** under the picture in the Device tab with five controls,
  acting only on the device being watched, only on a click:
  - **Rotate**: turns the device 90° (clockwise; repeated clicks cycle
    through the four orientations).
  - **Home**: the Home button press (on Face ID iPhones the system treats
    it as the go-home gesture, as Device Hub's Home does).
  - **Dark / Light**: toggles the appearance; the button shows the current
    one.
  - **Text size − / +**: steps the system Dynamic Type size.
  - **Screenshot**: copies a PNG of the device screen to the clipboard and
    shows a short "Copied screenshot" note.
- The live picture re-fits after a rotation and is drawn upright.
- Buttons are disabled when nothing is running or the device shut down.
  Dark/Light, Text size and Screenshot work in Reduced (screenshot) mode
  too.

## Non-goals

- The "More" menu from the brainstorm (lock, shake, contrast, location, open
  URL, status bar override), push, permissions, media, clipboard sync,
  launching apps.
- Tapping, typing, gestures on the picture (a later, separate project).
- Agents using these controls.
- Windows (the Device tab is macOS-only for now).

## Anti-goals

- A control acts on any device other than the one shown, or without a
  click.
- A missing private API crashes, hangs or throws: Rotate/Home hide instead.
- A control blocks the UI thread (every `simctl` call and HID send runs off
  the main thread, bounded by a timeout).

## Constraints

- The Device tab's rule changes from "watching never changes the device" to
  "nothing changes the device except a control the user clicks".
- Public commands (verified on Xcode 27.2): `xcrun simctl ui <udid>
  appearance [light|dark]` (no argument prints the current one),
  `xcrun simctl ui <udid> content_size [increment|decrement]`,
  `xcrun simctl io <udid> screenshot --type=png <file>` (simctl won't write
  to stdout).
- Rotate and Home have no `simctl` command. SimulatorKit exports
  `IndigoHIDMessageForButton(IndigoHIDButtonKeyCode, IndigoHIDButtonOp,
  IndigoHIDTarget)` and `SimDeviceLegacyHIDClient`, and has
  `deviceRotation` / `SimDisplayUIOrientationChangeDelegate` strings. How
  rotation is actually sent is **unverified**; a spike settles it.
- Reuse the Device tab's machinery: the `devices` module, the
  stream-owning worker thread, `ports::run_bounded_why` for timeouts, the
  `#[cfg(target_os = "macos")]` and platform-check rules.

## Approaches

- **A. Public commands only**: no Rotate or Home. Robust, but drops the
  most useful testing control.
- **B. Public commands + private HID for Rotate and Home (chosen,
  2026-11-23)**: a spike first; Rotate/Home hide if the private API is
  missing on this Xcode.
- **C. Fully interactive**: tap/type on the picture too. More than testing
  controls need; later.

## Design

1. **Spike (throwaway, gate).** On a test simulator the spike creates and
   deletes: find how to rotate (HID message, device-orientation event, or
   another SimDevice/SimulatorKit call) and how to press Home; confirm both
   from outside Device Hub with Device Hub closed and open; record whether
   the screen surface changes size on rotation or stays portrait with
   `screenProperties.uiOrientation` changing (decides whether the tab must
   rotate the drawn picture). Findings go into this doc. If rotation can't
   be done without Device Hub, stop and come back.
2. **Controls module** (`devices/controls.rs`): `Control { Rotate, Home,
   ToggleAppearance, TextBigger, TextSmaller, Screenshot }`;
   public ones run `simctl` with a 5 s timeout; private ones go through a
   macOS-only HID sender loaded like `sim_display` (dlopen, respondsToSelector,
   exception catch). Pure parts (argument building, appearance parsing,
   which controls are available in which `DeviceView`) are unit tested.
3. **Bottom bar UI** in `device_panel.rs` (or a split-out file): icon
   buttons with tooltips, disabled per state, a transient note for
   Screenshot and for a failed control (the error's first line). Clicks
   dispatch to a background task; one control in flight at a time.
4. **Upright picture**: if the spike shows the surface stays portrait, the
   tab rotates the drawn picture from the reported orientation; if the
   surface itself rotates, nothing to do beyond re-fit.
5. **Clipboard**: write the PNG to the macOS pasteboard (gpui's clipboard
   API if it takes images at this rev, else `NSPasteboard`).

## Left to the builder

Icons, exact bar height and spacing, the transient-note mechanism (reuse the
app's existing toast if there is one), rotate direction naming, module
split, and whether the Dark/Light button reads the current appearance on
device change or on first click.

A CHANGELOG `Unreleased` → `Added` line ships with the user-visible commit.

## Amendments (2026-11-23, from the user)

These widen the scope above. Where they conflict with earlier sections, they win.

1. **Interactive picture** (user: "you should allow clicking and controlling
   simulator"). Option C is in: click = tap, drag = swipe, trackpad/wheel =
   scroll, and typing goes to the device while the picture has focus (click
   into it first, like the browser tab; Esc or clicking elsewhere gives focus
   back). Same private HID path as Rotate/Home, so the same rule: if it's
   missing on this Xcode, the picture stays watch-only. The spike is extended
   to taps, swipes, scroll and keyboard.
2. **Device frame** (user: "the iPhone border around it … same with
   Android"). Draw Apple's device chrome around the picture, read at run
   time from `/Library/Developer/DeviceKit/Chrome/<chromeIdentifier>.devicechrome`
   (`chrome.json` plus PDF pieces); the device type's `profile.plist` names
   `chromeIdentifier` and `framebufferMask`. The mask gives the screen its
   rounded corners and is always applied. My call: the frame is on by
   default, with a "Show frame" toggle in the bottom bar. If a model has no
   chrome, show the rounded screen alone. Android skins
   (`<sdk>/skins/<name>/`, PNG + `layout`) come with the Android phase.
3. **Boot and Shut down from the tab** (user: "it would be nice to shutdown
   it on the device tab"). The picker lists shut-down simulators as well as
   running ones (from `simctl list devices -j`), in two groups. Picking a
   shut-down one offers **Boot** (`simctl boot <udid>`, headless). The bottom
   bar gets **Shut down** for the watched device. My call: it asks first
   ("Shut down iPhone 18 Pro? An agent may be using it."), because agents
   share these devices. This retires the original non-goal "booting, shutting
   down or creating simulators" for boot and shutdown only; creating and
   deleting devices stay out.

Consequence for the "no work while hidden" rule: unchanged. Input, boot and
shutdown only happen on a click while the tab is on screen.

## Spike findings (2026-11-23)

Measured on Xcode 27.2 (27B52), macOS 27 (Darwin 27.3), Apple Silicon, iOS
27.2 runtime, with a throwaway Objective-C spike on a simulator created for
the purpose (`simctl create "OO device test" "iPhone 18 Pro"`, booted
headless with `simctl boot`, deleted afterwards). Results were judged with
`simctl io screenshot` PNGs, raw framebuffer dumps and
`screenProperties.uiOrientation`. **Result: everything works from outside
Device Hub**: Rotate through a public CLI, and Home, tap, swipe and keyboard
through SimulatorKit's legacy HID client. Scroll has no working direct
message, so a swipe is the fallback. All of it also works under the hardened
runtime.

**Deviation from the Constraints section:** a public CLI for rotation
*does* exist. It is `devicectl` (CoreDevice), not `simctl`. So Rotate is a
subprocess like the `simctl` controls, not a private HID send.

### Rotate: `xcrun devicectl device orientation …` (public)

- `xcrun devicectl device orientation rotate --device <UDID> right`. `right`
  turns the device **clockwise** (portrait → landscapeRight →
  portraitUpsideDown → landscapeLeft → portrait); `left` is
  counter-clockwise. Also `l` / `r`.
- `xcrun devicectl device orientation set --device <UDID> <o>`. Values for
  `<o>`: `portrait`, `portraitUpsideDown`, `landscapeLeft`, `landscapeRight`,
  `faceUp`, `faceDown` (or `p`, `pu`, `ll`, `lr`, `fu`, `fd`).
- `xcrun devicectl device orientation get --device <UDID> -j -` prints JSON
  with `result.deviceOrientation` (e.g. `"landscapeRight"`, or `"unknown"`
  just after boot), `result.deviceOrientationNonFlat` and
  `result.deviceIsOrientationLocked`.
- **Always address the device by UDID.** `--device` also accepts names, and
  "iPhone 18 Pro" is ambiguous on this Mac (`devicectl list devices` shows
  the simulators next to physical devices).
- **Speed:** a call takes 70–140 ms wall time. It ran four times in a row with
  no settling time needed. `uiOrientation` updated within about 0.15 s.
- **Errors:**
  - Unknown device: exit 1, stderr `ERROR: The specified device was not
    found. (Name: …) (com.apple.dt.CoreDeviceError error 1000 (0x3E8))`.
  - Bad argument: exit 64.
  - An Xcode without the subcommand would give `Error: Unknown option …`,
    exit 64 (seen with a misspelled subcommand).
  - **Feature check (hide Rotate if it fails):** look for the line
    `USAGE: devicectl device orientation rotate` in the output of
    `xcrun devicectl device orientation rotate --help` (1 match here).
    **Exit codes don't work for this:** `devicectl help <anything>` and
    `devicectl device <bogus> --help` both exit 0 and print the parent's help
    (checked with `orientationx`).
  - An alternative check uses a dummy UDID:
    `… rotate --device 00000000-0000-0000-0000-000000000000 right`. It exits
    1 (device not found) when the subcommand exists and 64 when it doesn't.
- **Who serves it:** `devicectl` talks to `CoreDeviceService`
  (`/Library/Developer/PrivateFrameworks/CoreDevice.framework/…/XPCServices/CoreDeviceService.xpc`),
  a launchd-owned per-user process (ppid 1). It started at 18:42, before the
  running Device Hub (21:21), so it does not belong to Device Hub. Device Hub
  drives rotation through the same CoreDevice stack: DeviceKit's
  `RotateControlProvider` uses CoreDevice `OrientationControl`, and the guest
  runs `com.apple.coredevice.*` daemons. SimulatorKit has no rotation call,
  only `SimDisplayView.deviceRotation`, which is a view property.
- **The 180° step:** Safari on this iPhone refuses portraitUpsideDown. The
  device orientation becomes `portraitUpsideDown`, but `uiOrientation` stays
  at its previous value (3), and on a `set portraitUpsideDown` from portrait
  it stays at 1. So two of the four clicks change the picture and the other
  two don't. That is iOS behaviour, not a failure. `faceUp` also leaves
  `uiOrientation` at 1.

### What rotation does to the screen and surface (decides "upright picture")

| Device orientation (devicectl) | `screenProperties.uiOrientation` | Content in the raw (portrait) surface | Draw the surface rotated by |
|---|---|---|---|
| portrait | 1 | upright | 0 |
| landscapeRight (after one `right`) | 3 | UI top is at the surface's **left** edge | 90° **clockwise** |
| landscapeLeft | 4 | UI top is at the surface's **right** edge | 90° counter-clockwise |
| portraitUpsideDown | 2, **never observed** (Safari, Settings and the home screen are all portrait-only on iPhone) | presumably upside down | presumably 180°, unverified |

- **The surface stays portrait.** It was 1206×2622, same IOSurface ID 19,
  through every rotation. `surfacesChanged` did **not** fire, and
  `screenProperties.pixelSize` stayed 1206×2622. So the tab **must rotate
  the drawn picture itself** from `uiOrientation`. The rotation sense was
  checked by dumping the framebuffer in both landscapes and by eye.
- `simctl io screenshot` is already upright: 2622×1206 in landscape. Copy
  the screenshot PNG as it comes; don't rotate it.
- **`propertiesChanged` fires on rotation**, once per `uiOrientation` change,
  about 0.15 s after the `devicectl` call. It runs **on the callback queue
  passed to register** (a labelled serial queue, not the main thread, which
  settles the earlier spike's open question). Its argument is a
  `ROCKImmutableProxy` whose `uiOrientation` (`I`) is already the new value.
  Nothing fires when `uiOrientation` doesn't change (e.g. the refused 180°
  step). Use it to re-fit, or read `screenProperties.uiOrientation`.

### Home and the other HID input: `SimDeviceLegacyHIDClient` (SimulatorKit, private)

**Loading.** `dlopen` CoreSimulator as `sim_display` does, plus
`/Applications/Xcode.app/Contents/SharedFrameworks/SimulatorKit.framework/SimulatorKit`
(`RTLD_NOW`). Derive the Xcode path from `xcode-select -p`.

- **Class:** `NSClassFromString("SimulatorKit.SimDeviceLegacyHIDClient")`.
  The mangled `_TtC12SimulatorKit24SimDeviceLegacyHIDClient` gives the same
  class. Its superclass is NSObject, and it's a real class, not a ROCK proxy.
- **C functions** come from `dlsym(RTLD_DEFAULT, …)` after the `dlopen`.
  Every builder `calloc`s and returns an `IndigoHIDMessage *`.

**Calls.**

| What | Selector or symbol | Types |
|---|---|---|
| make a client | `alloc` then `initWithDevice:error:` (SimDevice from the `sim_display` chain) | `@32@0:8@16^@24`. Took 2–24 ms |
| send | `sendWithMessage:freeWhenDone:completionQueue:completion:` | `v44@0:8^{IndigoHIDMessageStruct=…}16B24@28@?36` (msg ptr, BOOL, `dispatch_queue_t` or nil, block or nil) |
| completion block | `void (^)(NSError *)` | signature `v16@?0@"NSError"8` |
| alt init | `initWithDevice:sessionResetQueue:error:sessionResetHandler:` | `@48@0:8@16@24^@32@?40` (not used) |

**Send semantics.**

- The send is effectively synchronous. It calls `mach_msg_send` on the
  caller's thread and returns in under 0.1 ms. It doesn't block and doesn't
  throw.
- `freeWhenDone: YES` hands ownership of the message to the client, which
  `free`s it. **Never pass a NULL message.**
- `completionQueue: nil, completion: nil` works (fire and forget; the event
  was delivered).
- `completionQueue: nil` *with* a completion → the completion is **never
  called** (the event is still delivered). Pass both or neither.
- With both, the completion runs on that queue with `nil` on success, or an
  `NSError` whose description reads
  `SimulatorKit.SimDeviceLegacyHIDClient.HIDError.machPortInvalid` /
  `.machPortNotConnected` when the guest's HID port died (see backboardd
  below).
- After such an error that client stays dead. A **new** client works once
  the guest restarts. Create a client per action, or recreate it on error;
  init is cheap.

**Message layout** (`IndigoHIDMessageStruct`; the builders fill all of it,
and the client fills the Mach header):

| Offset | Field |
|---|---|
| 0x00 | `mach_msg_header_t` (0x18 bytes) |
| 0x18 | `u32` event stride = 0xA0 |
| 0x1C | `u8` event count |
| 0x20 + n·0xA0 | the events |

Each event starts with:

| Offset (within the event) | Field |
|---|---|
| +0 | `u32` type: 2 button/key, 0xB digitizer, 6 wheel |
| +4 | `u64` timestamp |
| +0x10 | payload (from 0x30 for event 0) |

The total size is 0x20 + count·0xA0.

**Target: use `0x32` for everything.** It was found by a sweep:

- `0x32` reached the Home button, Lock, touch and keyboard.
- `0x33`, the value SimulatorKit's own `SimDeviceScreen.buttonTarget`
  returns (0x15 for productFamily 3) and idb's "hardware" target, was
  **accepted and ignored** for buttons.
- **Wheel events at target 0x35 crashed the guest's `backboardd`.**
  `SimulatorHID -[SimHIDVirtualServiceManager serviceForIndigoHIDData:]`
  threw an uncaught NSException; two crash reports
  (`~/Library/Logs/DiagnosticReports/backboardd-*.ips`) name this device's
  UDID. Afterwards the open HID client got `machPortInvalid` /
  `machPortNotConnected`, and a new client worked once the guest recovered.
  0x34 was sent after the port was already dead, so it is unattributed.
  Button events at 0x15, 0x0, 0x2 and 0x34 left no crash report, but the
  sweep was muddied by Lock. **Only ever send 0x32.**
- **Unknown:** whether a backboardd restart replaces the framebuffer
  IOSurface. The ID read afterwards (689, not 19) is confounded by a device
  reboot done earlier, before it was last read.

**Home** (verified: Settings in front, luminance 245 → home screen 139, and
the screenshot shows the home screen):

```c
typedef struct IndigoHIDMessage IndigoHIDMessage;
IndigoHIDMessage *IndigoHIDMessageForButton(uint32_t keyCode, uint32_t op, uint32_t target);
// keyCode: 0 = Home, 1 = Lock/power, 0xBB8 = side button
// op: 1 = down, 2 = up
// Home: send ForButton(0, 1, 0x32), then ForButton(0, 2, 0x32)
```

- Down and up can go back-to-back (0 ms apart worked). No session warm-up
  is needed.
- The layout is 192 bytes, count 1: `+0x30` keyCode, `+0x34` op,
  `+0x38` target.
- `keyCode 1` (Lock) at 0x32 locked the device: luminance fell to 72. Only
  the luminance was checked, no screenshot. Not needed.
- SimulatorKit's own chrome mapping, from the disassembly: consumer page
  0x0C usage 0x30 → keyCode 1, 0x40 → keyCode 0 (0x191 on a TV-type
  client), 0x95 → 0xBB8.

**Tap and drag** (verified: tapping the Settings icon opened Settings; a
drag from (0.5, 0.75) to (0.5, 0.3) over 300 ms scrolled the Settings
list; tapping Safari's address bar focused it):

```c
IndigoHIDMessage *IndigoHIDMessageForMouseNSEvent(CGPoint *p, CGPoint *p2 /* NULL = one finger */,
    uint32_t target /* 0x32 */, NSEventType type, CGSize size, uint32_t edge /* 0 */);
// type: 1 = LeftMouseDown (touch down), 6 = LeftMouseDragged (move), 2 = LeftMouseUp (touch up)
```

- **Coordinates:** the function stores `*p / size`, a 0…1 ratio.
  SimulatorKit itself passes the ratio with `size = {1, 1}`. Only that form
  was run. From the disassembly, device pixels with `size = {1206, 2622}`
  (or points with `{402, 874}`) should be equivalent; that wasn't run.
- **Coordinates are in the portrait surface's space at every
  orientation.** Verified in landscapeRight: a drag along the surface's x
  axis scrolled Safari vertically. To map a click at (u, v) in the
  *upright drawn picture* (0…1) back to the surface ratio (sx, sy):
  - `uiOrientation` 1: (u, v).
  - 3: (v, 1 − u).
  - 4: (1 − v, u).
  - For 3, the drag verified the axis mapping. The axis directions come from
    the surface dump. 4 is the mirror of 3; not run.
- **A one-finger message is 384 bytes, count 2**, and starts with
  `u32 0xB` at +0x20.
- **Throttle:** a *move* (type 6) built within 16 ms (0xF423FF ns) of the
  previous one returns **NULL** (the builder frees its buffer). Down and up
  always build. Measured: 15 moves 4 ms apart → 12 NULL. Coalesce moves to
  at most one per 16 ms (60 Hz), skip NULLs, and always send the up at the
  last position.
- Timing used: tap down→up 100 ms apart; drag moves 20 ms apart.

**Keyboard** (verified: typed `Hello OO 42` into Safari's address bar,
three Deletes left `Hello OO`, then `example.com` + Return loaded the page):

```c
IndigoHIDMessage *IndigoHIDMessageForKeyboardArbitrary(uint32_t hidUsage, uint32_t op /* 1 down, 2 up */);
```

- The arguments are USB HID keyboard usages:
  - `a`–`z` = 0x04–0x1D, `1`–`9` = 0x1E–0x26, `0` = 0x27.
  - Return 0x28, Delete (backspace) 0x2A, Tab 0x2B, Space 0x2C.
  - `.` 0x37, `/` 0x38, `;` 0x33.
  - Left Shift 0xE1. Send Shift down, the key down and up, then Shift up.
- **The builder hard-codes target 0x64 at +0x38, which the guest ignores.
  Overwrite the `u32` at offset 0x38 with 0x32 before sending**, otherwise
  nothing is typed.
- Layout: `+0x30` 0x2710 (keyboard source), `+0x34` op, `+0x38` target,
  `+0x3C` usage, `+0x40` keyboard type (204 here, from
  `IndigoHIDGetKeyboardType`).
- **Side effect:** the first key makes iOS treat a hardware keyboard as
  attached, so the on-screen keyboard hides while focus stays in the field.
  That is expected for typing from the Mac.

**Scroll: no working direct message; use a swipe.**

```c
IndigoHIDMessage *IndigoHIDMessageForScrollEvent(uint32_t, double dx, double dy, double dz, uint32_t target);
```

- It builds a wheel event (type 6, 192 bytes). At 0x32 it was accepted and
  had no effect on Settings (screenshot unchanged); 0x33 behaved the same.
  At 0x35 it crashed backboardd (see above).
- In the disassembly, SimulatorKit builds wheel events only on its
  pointer/trackpad capture path (`IndigoHIDMessageForScrollEventFromHIDEventRef`
  with a pointer target). That path presumably needs
  `IndigoHIDMessageToCreatePointerService` first. Not tried.
- **Map trackpad/wheel scroll to a synthesised drag** (down, moves at
  ≥16 ms intervals, up) in the opposite direction of the content motion.

### Public Home fallback

`xcrun simctl launch <UDID> com.apple.springboard` brought the home screen
forward from Settings. It was seen twice: once by screenshot, and once by
luminance (245 → 139). It **foregrounds SpringBoard rather than pressing a
button**: no button semantics, and no effect on lock or Siri. Use it when
SimulatorKit or the 0x32 HID path is unavailable.

### Hardened runtime

A copy signed with `codesign --force --options runtime --entitlements
crates/harness-app/resources/openOrchestra.entitlements --sign -`
(`flags=0x10002(adhoc,runtime)`) repeated every check:

- loading SimulatorKit;
- Home (245 → 139);
- tap;
- drag;
- typing `Hardened`;
- four `devicectl … rotate right` spawned from the hardened process with
  `NSTask` (exit 0, 130–140 ms each; `uiOrientation` went 3, 3, 4, 1).

No new entitlement is needed.

### Device Hub

Device Hub (`com.apple.dt.Devices`, pid 95107) **was running the whole
time**, the user's own, so the "Device Hub closed" case **was not tested**.
The rules forbid quitting it. Device Hub was not driven, and whether it was
showing this device is unknown. Everything above works with it running.
`CoreDeviceService` predates Device Hub, so Rotate should not need Device
Hub. The legacy HID port is owned by the simulator (`SimDevice.io` port
`com.apple.CoreSimulator.HID.LegacyHID`), so the HID input should not need
it either. Both are expected, not verified.

### Consequences for the plan

- **Rotate:** `devicectl` subprocess (5 s timeout, `--device <UDID>`,
  `right` = clockwise). Hide it when the `--help` output lacks the
  `USAGE: devicectl device orientation rotate` line.
- **Home:** private HID (`ForButton(0, 1/2, 0x32)`). Fallback:
  `simctl launch <UDID> com.apple.springboard`.
- **Tap, swipe, keyboard:** private HID as above.
- **Scroll:** synthesised swipe.
- **Load check for the private part:** the class, `initWithDevice:error:`,
  the send selector and the four symbols (`IndigoHIDMessageForButton`,
  `…ForMouseNSEvent`, `…ForKeyboardArbitrary`, plus `…ForScrollEvent` if
  ever used). If any is missing, hide Home and keep the picture watch-only.
- **Upright picture:** rotate the drawn surface from `uiOrientation`
  (3 → 90° CW, 4 → 90° CCW, 2 → 180° unverified) and map input back with
  the formulas above.
- **Survive a backboardd restart:** recreate the HID client on error.
  Whether the surface is replaced too is not established, so keep the
  existing `surfacesChanged` handling.
