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
