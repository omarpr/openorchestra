# Windows port test checklist

The issues below came from reviewing `windows-port` at `611b265` on macOS. Use
this checklist to confirm them on Windows 10 or 11 from PowerShell.

## Setup

From the repository root, follow the [Windows setup instructions](../README.md#windows-experimental).
Record the commit and Rust version, then run the setup check.

```powershell
git rev-parse --short HEAD
rustc --version
.\make setup
```

For the desktop checks, use a disposable Git repository with a commit and launch
the debug build with `.\make run "C:\path\to\test-repo"`. The phone check also
needs Node 20 or newer and pnpm (or Corepack) so `make run` builds the phone
web client.

## 1. Phone control of a live Windows terminal

1. In PowerShell, run `Get-Command tmux.exe -ErrorAction SilentlyContinue` and
   record whether it finds anything. The standard Windows setup has no `tmux.exe`.
2. Open a **Shell** tab in the test repo and run `echo WINDOWS_LOCAL_OK`. Leave
   the shell running at its prompt.
3. Open **Settings → Remote**, turn on **Remote control**, and use **Add a
   device** to pair a phone on the same network. Follow the
   [pairing instructions](remote-control.md#pair-a-device) and leave **Watch only** off.
4. Right-click the shell tab on the PC and choose **Allow remote control**. On
   the phone, open that tab's **Terminal** view and try to run
   `echo WINDOWS_PHONE_OK`.

- [ ] **Expected.** The phone shows the live shell, and the new marker appears
  on both the phone and the PC.
- [ ] **Suspected failure.** Pairing succeeds, but the live tab appears stopped,
  the terminal fails to attach, or input is disabled. If pairing itself fails,
  record the setup error and mark the terminal check blocked.

The phone server invokes `tmux` for session discovery and `tmux -C` for terminal
attachment. On Windows the app passes it `oo-session.exe`, which implements the
part of control mode the bridge uses; `cargo test -p oo-session --test
remote_bridge` exercises discovery, attach, typing, fitting and exit against it.
If the phone still fails, record whether `oo-session -L <label> -C
attach-session -t =<session>` prints `%output` lines for that tab's session.
[Remote `tmux` command](../crates/remote/src/tmux_bridge.rs) · [Windows control mode](../crates/oo-session/src/win/control.rs)

## 2. `make test` on the documented Windows toolchain

Check which utilities are available, then run the full suite from the repo root.

```powershell
Get-Command sleep.exe, cat.exe -ErrorAction SilentlyContinue
.\make test
```

If the full run fails, run the two suspect tests individually.

```powershell
cargo test --locked -p oo-remote --lib stalled_control_write_retires_the_client_and_reports_unknown_delivery -- --nocapture
cargo test --locked -p oo-remote --lib screenshot_cleanup_tracks_whether_the_path_may_have_been_written -- --nocapture
```

- [ ] **Expected.** The two tests pass with the toolchain from the Windows
  setup instructions.
- [ ] **Suspected failure.** One or both tests panic when spawning `sleep` or
  `cat` because their executables are missing from `PATH`. Record which utility
  PowerShell resolved and the first error from each failing test.

The test runner calls `cargo test` directly. Both tests spawn Unix utilities
and unwrap the result.
[Windows test runner](../script/windows/make.ps1#L269) · [`sleep` test](../crates/remote/src/tmux_bridge.rs#L1204) · [`cat` test](../crates/remote/src/tmux_bridge.rs#L1252)

## 3. Session reattachment contradicts the README

1. With the debug app built, confirm the bundled session helper prints its version.

   ```powershell
   .\crates\harness-app\target\debug\oo-session.exe -V
   ```

2. Open a **Shell** tab and enter `export OO_WINDOWS_SMOKE=kept` in Git Bash.
   Leave the tab open and choose **Quit openOrchestra** from the app menu.
3. Relaunch with the same `.\make run` command. Select the restored tab and run
   `echo "$OO_WINDOWS_SMOKE"`.

- [ ] **Expected from the implementation.** The tab reattaches and prints
  `kept`, confirming that the original shell survived. The README currently
  says sessions never reattach on Windows.
- [ ] **If the value is blank or the tab is missing**, record whether
  `oo-session.exe -V` worked and whether startup reported a missing session
  helper. Explicitly closing the tab before quitting ends its session.

[README](../README.md#L95) · [Windows session design](../crates/oo-session/src/lib.rs#L1) · [Explicit tab close](../crates/harness-app/src/workspace/state/tabs.rs#L438)

## macOS baseline

On Apple Silicon, the app opened a disposable Git repo, displayed the first-run
requirements screen, and ran a command in the embedded shell. With Rust 1.98.1,
the workspace and desktop suites passed 958 tests, with 30 ignored. The local
default Rust 1.96 failed to compile the pinned Zed dependency. Native Windows
results are pending.
