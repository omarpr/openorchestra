#!/usr/bin/env bash
# Build the optional Device-tab input helper (oo-device-input) for a dev or release app.
#
# usage: build-device-input-helper.sh <output> [arch...]   (default: this Mac's arch)
#
# It links against Xcode's private CoreDevice frameworks through a hand-written stub
# module (crates/harness-app/helpers/device-input). Where they are missing (as on CI),
# or this Xcode's don't match the stub, it removes <output>, says so in one line and
# exits 0: the app then keeps the Device tab watch-only. An arch the frameworks lack
# (x86_64 against arm64e-only frameworks) is left out of a universal build.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/crates/harness-app/helpers/device-input"
output="$1"
shift
if (($# == 0)); then
  set -- "$(uname -m)"
fi

skip() {
  rm -f "$output"
  echo "Device input helper not built ($1); the Device tab stays watch-only."
  exit 0
}

PRIVATE=/Library/Developer/PrivateFrameworks
CORE_DEVICE="$PRIVATE/CoreDevice.framework"
NESTED="$CORE_DEVICE/Versions/A/Frameworks"
for lib in "$CORE_DEVICE/CoreDevice" \
  "$PRIVATE/CoreDeviceUtilities.framework/CoreDeviceUtilities" \
  "$NESTED/UniversalHID.framework/UniversalHID"; do
  [ -e "$lib" ] || skip "Xcode's CoreDevice frameworks aren't installed"
done
xcrun --sdk macosx --show-sdk-path >/dev/null 2>&1 || skip "no macOS SDK"

have="$(lipo -archs "$CORE_DEVICE/CoreDevice" 2>/dev/null || true)"
archs=()
for arch in "$@"; do
  case " $have " in
    *" $arch "* | *" ${arch}e "*) archs+=("$arch") ;;
    *) echo "Device input helper: no $arch in this Xcode's CoreDevice ($have); leaving it out." ;;
  esac
done
((${#archs[@]})) || skip "CoreDevice has none of: $*"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
for arch in "${archs[@]}"; do
  stub="$tmp/stub-$arch/CoreDevice.swiftmodule"
  mkdir -p "$stub"
  sed "s/-target arm64-apple-macos/-target ${arch}-apple-macos/" \
    "$SRC/stub/CoreDevice.swiftmodule/arm64-apple-macos.swiftinterface" \
    > "$stub/${arch}-apple-macos.swiftinterface"
  if ! xcrun --sdk macosx swiftc -target "${arch}-apple-macos15.0" -parse-as-library -O \
    -module-cache-path "$tmp/modules" -I "$tmp/stub-$arch" \
    -F "$PRIVATE" -F "$NESTED" \
    -framework CoreDevice -framework CoreDeviceUtilities -framework UniversalHID \
    -o "$tmp/oo-device-input-$arch" "$SRC/main.swift" >"$tmp/log-$arch" 2>&1; then
    # A renamed private symbol shows up here, undefined at link: name the first.
    why="$(grep -A1 'Undefined symbols' "$tmp/log-$arch" | sed -n '2s/^ *//p')"
    why="${why:-$(grep -m1 'error' "$tmp/log-$arch" || true)}"
    skip "this Xcode's CoreDevice doesn't match the helper: ${why:-swiftc failed}"
  fi
done

mkdir -p "$(dirname "$output")"
if ((${#archs[@]} == 1)); then
  cp "$tmp/oo-device-input-${archs[0]}" "$output"
else
  lipo -create -output "$output" "$tmp"/oo-device-input-*
fi
