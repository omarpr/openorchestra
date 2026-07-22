#!/usr/bin/env bash
# Build openOrchestra.app — a double-clickable macOS bundle.
# Run on a Mac (after `make setup` has installed the toolchain).
#
#   ./script/package-mac.sh          # release build + bundle → dist/openOrchestra.app
#
# The bundle is ad-hoc signed for local use; Gatekeeper may still require a
# right-click → Open the first time (unsigned/undistributed app).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP_NAME="openOrchestra"
CRATE="$ROOT/crates/harness-app"
APP="$ROOT/dist/$APP_NAME.app"

echo "==> building release binary (this takes a few minutes cold)"
( cd "$CRATE" && cargo build --release )

BIN="$CRATE/target/release/harness-app"
[ -f "$BIN" ] || { echo "error: binary not found at $BIN"; exit 1; }

echo "==> assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/$APP_NAME"
chmod +x "$APP/Contents/MacOS/$APP_NAME"
cp "$ROOT/assets/openOrchestra.icns" "$APP/Contents/Resources/$APP_NAME.icns"
# Notification sound (UNNotificationSound resolves bundle resources by name).
cp "$ROOT/crates/harness-app/sounds/uh-oh.wav" "$APP/Contents/Resources/uh-oh.wav"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>               <string>${APP_NAME}</string>
  <key>CFBundleDisplayName</key>        <string>openOrchestra</string>
  <key>CFBundleIdentifier</key>         <string>ai.openorchestra.app</string>
  <key>CFBundleVersion</key>            <string>0.1.0</string>
  <key>CFBundleShortVersionString</key> <string>0.1.0</string>
  <key>CFBundlePackageType</key>        <string>APPL</string>
  <key>CFBundleExecutable</key>         <string>${APP_NAME}</string>
  <key>CFBundleIconFile</key>           <string>${APP_NAME}</string>
  <key>LSMinimumSystemVersion</key>     <string>12.0</string>
  <key>NSHighResolutionCapable</key>    <true/>
  <key>LSApplicationCategoryType</key>  <string>public.app-category.developer-tools</string>
</dict>
</plist>
PLIST

# Ad-hoc sign so it launches without "damaged app" complaints on local machines.
if command -v codesign >/dev/null 2>&1; then
  echo "==> ad-hoc signing"
  codesign --force --deep --sign - "$APP" || echo "(codesign failed; app still runnable via right-click → Open)"
fi

# Refresh the icon cache so Finder shows the new icon immediately.
touch "$APP"

echo "==> done: $APP"
echo "    open it with:  open \"$APP\""
