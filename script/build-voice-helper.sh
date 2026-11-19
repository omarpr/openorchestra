#!/usr/bin/env bash
# Build the optional SpeechAnalyzer executable for a dev or release app.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
output="$1"
shift
if (($# == 0)); then
  set -- "$(uname -m)"
fi

sdk="$(xcrun --sdk macosx --show-sdk-version 2>/dev/null || true)"
if [[ ! "$sdk" =~ ^[0-9]+ || "${sdk%%.*}" -lt 26 ]]; then
  rm -f "$output"
  echo "Voice input requires the macOS 26 SDK or newer to build the speech helper."
  exit 0
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
for arch in "$@"; do
  xcrun --sdk macosx swiftc -target "${arch}-apple-macosx12.0" -parse-as-library -O \
    -module-cache-path "$tmp/modules" -o "$tmp/oo-voice-$arch" \
    "$ROOT/crates/remote/voice/VoiceTranscriber.swift"
done

mkdir -p "$(dirname "$output")"
if (($# == 1)); then
  cp "$tmp/oo-voice-$1" "$output"
else
  lipo -create -output "$output" "$tmp"/oo-voice-*
fi
