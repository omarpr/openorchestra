# Linux and Windows support

Status: proposed on 2026-11-18, deferred. Nothing implemented yet. File and
line references are as of commit `1f5dd98`.

## Summary

- **Linux is a port.** No restructuring is needed: replace roughly ten
  macOS-only tool calls, rework the keybindings and window chrome, and add
  packaging. The estimate is 1–2 weeks, depending on the first build.
- **Windows hinges on tmux and the POSIX shell.** Neither exists natively, and
  both run through the app's core. See [Windows options](#windows-options).

## What is already portable

- **Headless crates (verified).** `worktrees`, `forge`, `agent-runtime`,
  `a2a-broker`, `remote` and the others pass clippy and tests on
  `ubuntu-latest` in the CI `headless` job.
- **UI stack (expected, not verified).** `gpui_platform` already enables the
  `wayland` and `x11` features (`crates/harness-app/Cargo.toml`). Zed's
  `terminal`, `editor` and `workspace` crates are cross-platform upstream.
  `harness-app` itself has never been compiled off macOS, because the CI `app`
  job runs only on `macos-15`.
- **macOS-only code is mostly gated already.**
  - Native notifications and the About panel sit behind
    `cfg(target_os = "macos")`.
  - `objc2` and `block2` are target-specific dependencies.
  - The notifier already falls back to `notify-send` on Linux.
  - File-mode hardening is `#[cfg(unix)]` with no-op fallbacks.
- **Already portable.**
  - Process accounting uses `sysinfo`, and interface enumeration uses
    `if_addrs`.
  - Agents launch through the user's `$SHELL`
    (`worktrees/src/login_shell.rs`).
  - Without tmux, agents are spawned directly with no persistence
    (`harness-app/src/terminal_pane.rs`, the `None` branch of `spawn`).

## Linux work items

| Item | Where | Size |
|---|---|---|
| Build `harness-app` on Ubuntu (system libs: xkbcommon, wayland, fontconfig, alsa, …) and add an `app-linux` CI job | `.github/workflows/ci.yml` | M (unknown until the first build) |
| Home-network detection shells out to `route -n get default`, `ipconfig getoption` and `scutil`. These fail soft, but with no router the network has no identity, which can break trusted-network pairing | `remote/src/network.rs`, `remote/src/certs.rs` | S–M: `/proc/net/route` or `ip route`; `gethostname` |
| Keep-awake runs `/usr/bin/caffeinate`, so enabling remote access errors | `remote/src/keepawake.rs` | S: `systemd-inhibit`, or no-op |
| Sounds use `afplay` and `/System/Library/Sounds` | `harness-app/src/sounds.rs` | S: `paplay`/`pw-play`, or the `rodio` crate |
| Agent-liveness `ps -axo pid=,ppid=,comm=` works on procps but is better replaced with `sysinfo` | `harness-app/src/workspace/state/alerts.rs` (`ps_snapshot`) | XS |
| Update flow assumes a Homebrew cask (`brew upgrade --cask`, Caskroom paths) | `harness-app/src/update_check.rs` | S: link to the release page off macOS |
| 65 `cmd-` keybindings (1 `secondary-`), the app menu bar, and title-bar padding for the macOS window buttons | `shortcuts.rs`, `terminal_pane.rs`, `workspace/actions/window.rs`, `main.rs` (`TitlebarOptions`, `set_menus`), `workspace/ui/chrome.rs` | M: move to `secondary-`; client-side decorations on Wayland |
| The remote shell-tab conversation hook is zsh-only, via `ZDOTDIR`. Inferred, not tested: bash and fish users get no shell conversation view. This already applies on macOS, but zsh is a less common default on Linux | `remote/src/shell.rs` | M if bash support is wanted |
| Packaging: today DMG + notarization + Homebrew cask | `Makefile`, `script/package-mac.sh`, `packaging/` | M: AppImage or tarball first; .deb or Flatpak later |
| Preflight install hints say `brew install …` | `harness-app/src/preflight.rs` | XS |

## Windows blockers

1. **Ungated Unix code that will not compile:**
   - `remote/src/store.rs`: `libc::flock`, `AsRawFd`, `DirBuilderExt`
   - `remote/src/shell.rs` and `remote/src/activity.rs`: top-level
     `std::os::unix::fs` imports
   - `harness-app/src/workspace/state/alerts.rs`: `pid_alive` uses
     `libc::kill(pid, 0)`

   These are mechanical fixes.
2. **tmux is load-bearing.** It has about 400 references across 33 files. It
   provides:
   - agent persistence across tab and app close
   - restart and orphan detection (`@oo_agent`, `list-panes`)
   - copy-mode scroll handling
   - the remote bridge (`remote/src/tmux_bridge.rs`)
   - `resource_monitor` attribution

   There is no native tmux on Windows.
3. **POSIX shell assumptions:**
   - `$SHELL -lic` and `exec "$SHELL" -li` launch lines
   - POSIX `shell_quote`
   - `.sh` worktree hooks run via `sh` (`worktrees/src/hooks.rs`)
   - `reject.sh` (`harness-app/src/workspace/a2a.rs`)
   - direct `HOME` reads (≥6 sites)
   - `login_env.rs` PATH repair

## Windows options

- **A. WSL2 only.** Ship the Linux build and run it under WSLg. Costs little
  beyond the Linux port, but it is second-class: the GUI runs inside WSL.
  Repos on the Windows drive (`/mnt/c/...`) do work, but keeping them on the
  Linux side is recommended: git is much faster there, and the diff and
  git-status watchers rely on file-change events, which aren't reliably
  delivered into WSL for files changed from the Windows side.
- **B. Native, no persistence.** Build on the existing no-tmux path.
  - Fix the compile breaks.
  - Use `USERPROFILE` / `dirs`.
  - Make shell quoting and hooks Windows-aware (PowerShell or Git Bash).
  - Disable remote access and the persistence features.

  Several weeks, and it gives up what tmux provides.
- **C. Native with a session backend.** Put tmux behind a "session host" trait
  and write a Windows backend, likely a small daemon owning ConPTY sessions.
  This is a re-architecture: 1–2+ months.

**Open questions before choosing B or C:** whether each supported agent CLI
(claude, codex, pi, opencode, kimi) runs natively on Windows, and whether any
tmux-equivalent is viable there. Neither has been researched yet.

## Recommended sequence

1. Add an `ubuntu-latest` job for `crates/harness-app` to CI, or do one Linux
   container build first. Its result sizes the Linux effort.
2. Land the Linux work items above, and ship Linux as a supported platform.
3. Offer Windows through WSL2 (option A) in the meantime.
4. Answer the Windows open questions, then choose between B and C.
