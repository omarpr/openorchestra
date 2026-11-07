#!/usr/bin/env bash
# `make run`: build the debug app and run it from a dev .app bundle.
#
# Why a bundle at all: macOS only gives UNUserNotificationCenter to a process
# that is a registered app bundle. A bare `cargo run` binary falls back to
# `osascript` notifications, which belong to Script Editor — clicking one opens
# Script Editor and cannot navigate anywhere, so a dev build could never test
# (or use) the "click the alert → jump to that worktree's agent" flow.
#
# The bundle is cheap to rebuild every run: the executable is a *hard link* to
# cargo's output (no 400 MB copy), and only the plist, icon and sounds are
# written. A separate bundle id keeps the dev app's notification permission and
# state apart from an installed openOrchestra.app.
#
# Extra args are passed through to the app. Logs go to this terminal, as with
# `cargo run`.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CRATE="$ROOT/crates/harness-app"
APP="$CRATE/target/debug/openOrchestra Dev.app"
EXE_NAME="openOrchestra-dev"

(cd "$CRATE" && cargo build)

mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
# Re-link every run: cargo writes a new inode on each build, so a link made
# last time points at the previous binary.
ln -f "$CRATE/target/debug/harness-app" "$APP/Contents/MacOS/$EXE_NAME"
cp "$ROOT/assets/openOrchestra.icns" "$APP/Contents/Resources/openOrchestra.icns"
cp "$CRATE"/sounds/*.wav "$APP/Contents/Resources/"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>               <string>openOrchestra Dev</string>
  <key>CFBundleDisplayName</key>        <string>openOrchestra Dev</string>
  <key>CFBundleIdentifier</key>         <string>ai.openorchestra.dev</string>
  <key>CFBundleVersion</key>            <string>1</string>
  <key>CFBundleShortVersionString</key> <string>0.0.0-dev</string>
  <key>CFBundlePackageType</key>        <string>APPL</string>
  <key>CFBundleExecutable</key>         <string>${EXE_NAME}</string>
  <key>CFBundleIconFile</key>           <string>openOrchestra</string>
  <key>LSMinimumSystemVersion</key>     <string>12.0</string>
  <key>NSHighResolutionCapable</key>    <true/>
</dict>
</plist>
PLIST

# Ad-hoc sign the bundle. Unsigned, macOS refuses to even ask for notification
# permission (UNErrorDomain error 1, "notifications not allowed"). Signing
# rewrites the linked binary in place, which is harmless: cargo relinks on the
# next change regardless.
codesign --force --sign - "$APP" >/dev/null 2>&1 || echo "(codesign failed; notifications may fall back)"

# Launch through LaunchServices (`open`), not exec: run as a child of the
# terminal, the terminal is the "responsible process" and notification
# permission is judged against it rather than this bundle. `open` detaches
# stdout, so the app logs to a file this script follows — `make run` still
# shows the logs, and Ctrl-C stops the app.
LOG="$CRATE/target/debug/openOrchestra-dev.log"
: > "$LOG"
ENV_ARGS=()
[ -n "${RUST_LOG:-}" ] && ENV_ARGS+=(--env "RUST_LOG=$RUST_LOG")
tail -n +1 -F "$LOG" &
TAIL=$!
trap 'kill $TAIL 2>/dev/null; pkill -f "$APP/Contents/MacOS/$EXE_NAME" 2>/dev/null || true' EXIT INT TERM
open -W -n ${ENV_ARGS[@]+"${ENV_ARGS[@]}"} --stdout "$LOG" --stderr "$LOG" "$APP" --args "$@"
