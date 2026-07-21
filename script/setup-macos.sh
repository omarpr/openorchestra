#!/usr/bin/env bash
# One-time macOS setup for building harness-app (the zed/gpui dependency cone).
# Idempotent: safe to re-run any time; only installs what's missing.
#
# What it handles (each discovered the hard way on a cold build):
#   - full Xcode selected (Metal shader compiler ships with Xcode.app, not the CLT)
#   - Metal toolchain downloaded + activated (separate component since Xcode 26)
#   - cmake     (wasmtime-c-api-impl build script shells out to it)
#   - protobuf  (zed dep cone / prost build scripts)
set -euo pipefail

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

[[ "$(uname -s)" == "Darwin" ]] || die "this setup script is macOS-only"

# 1. Full Xcode must be the selected developer directory.
dev_dir="$(xcode-select -p 2>/dev/null || true)"
if [[ "$dev_dir" != *"Xcode"*".app"* ]]; then
  if [[ -d /Applications/Xcode.app ]]; then
    say "Pointing xcode-select at Xcode.app (needs sudo)"
    sudo xcode-select --switch /Applications/Xcode.app/Contents/Developer
  else
    die "Full Xcode is required (Metal shader compiler). Install it from the App Store, then re-run: make setup"
  fi
else
  say "Xcode selected: $dev_dir"
fi

# 2. Metal toolchain (separate multi-GB download since Xcode 26).
#    Downloading the asset is NOT enough on its own: xcodebuild must register it
#    (-runFirstLaunch), xcrun caches "tool not found" results (--kill-cache), and
#    on some machines the toolchain image (cryptex) fails to auto-mount due to a
#    permissions bug and has to be attached manually.
metal_ok() {
  xcrun --kill-cache >/dev/null 2>&1 || true  # xcrun caches negative lookups
  xcrun -sdk macosx metal --version >/dev/null 2>&1
}

if metal_ok; then
  say "Metal toolchain OK"
else
  say "Metal toolchain not active — downloading if needed (a few GB, needs sudo, one-time)"
  sudo xcodebuild -downloadComponent MetalToolchain
  if ! metal_ok; then
    say "Activating Xcode components (xcodebuild -runFirstLaunch)"
    sudo xcodebuild -runFirstLaunch
  fi
  if ! metal_ok; then
    # Known Xcode 26 bug: the downloaded toolchain image never gets mounted.
    # Mount it ourselves; xcodebuild picks it up at /Volumes/MetalToolchainCryptex.
    say "Toolchain still not visible — mounting its disk image manually"
    shopt -s nullglob
    dmgs=(/System/Library/AssetsV2/com_apple_MobileAsset_MetalToolchain/*.asset/AssetData/Restore/*.dmg)
    shopt -u nullglob
    if ((${#dmgs[@]})); then
      hdiutil attach "${dmgs[${#dmgs[@]}-1]}" -nobrowse || true
    else
      say "No downloaded toolchain image found under /System/Library/AssetsV2"
    fi
  fi
  if ! metal_ok; then
    say "Diagnostics (does the toolchain build match 'xcodebuild -version'?):"
    xcodebuild -version || true
    xcodebuild -showComponent metalToolchain || true
    die "Metal toolchain still not active. If the builds above mismatch (e.g. after an Xcode update), re-run: sudo xcodebuild -downloadComponent MetalToolchain. Otherwise try opening Xcode.app once, or reboot, then re-run: make setup"
  fi
  say "Metal toolchain OK"
fi

# 3. Homebrew packages.
command -v brew >/dev/null 2>&1 || die "Homebrew is required: https://brew.sh"

need=()
command -v cmake  >/dev/null 2>&1 || need+=(cmake)
command -v protoc >/dev/null 2>&1 || need+=(protobuf)
if ((${#need[@]})); then
  say "Installing via Homebrew: ${need[*]}"
  brew install "${need[@]}"
else
  say "cmake + protobuf OK"
fi

# 4. Rust toolchain.
command -v cargo >/dev/null 2>&1 || die "Rust toolchain missing — install via https://rustup.rs"

# 5. Runtime tools (recommended). The app runs without these — the terminal
#    falls back to a plain shell and PR/CI badges hide — but they're what make it
#    useful: `pi` is the coding agent; `gh` powers the PR/CI badges.
run_need=()
command -v node >/dev/null 2>&1 || run_need+=(node)
command -v gh   >/dev/null 2>&1 || run_need+=(gh)
if ((${#run_need[@]})); then
  say "Installing runtime tools via Homebrew: ${run_need[*]}"
  brew install "${run_need[@]}"
else
  say "node + gh OK"
fi

# pi (the coding agent) via npm.
if command -v pi >/dev/null 2>&1; then
  say "pi OK ($(command -v pi))"
elif command -v npm >/dev/null 2>&1; then
  # Pinned to the version the harness-status extension is tested against
  # (docs/spike2-notes.md); bump deliberately alongside the extension.
  say "Installing pi (npm i -g @mariozechner/pi-coding-agent@0.73.1)"
  npm install -g @mariozechner/pi-coding-agent@0.73.1 \
    || say "pi install failed — install manually: npm i -g @mariozechner/pi-coding-agent@0.73.1"
else
  say "npm not found even after installing node — open a new shell, then: npm i -g @mariozechner/pi-coding-agent@0.73.1"
fi

# gh auth is interactive — just nudge (don't block setup).
if command -v gh >/dev/null 2>&1 && ! gh auth status >/dev/null 2>&1; then
  say "gh is installed but not authenticated — run 'gh auth login' to enable PR/CI badges"
fi

say "All prerequisites present. Build with: make run   (check tools anytime: make doctor)"
