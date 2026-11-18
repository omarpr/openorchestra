#!/usr/bin/env bash
# Build openOrchestra.app — a double-clickable macOS bundle — and optionally a
# universal DMG, signed and notarized, published to GitHub Releases.
# Run on a Mac (after `make setup` has installed the toolchain).
#
#   ./script/package-mac.sh                        # host-arch .app → dist/  (fast, default)
#   ./script/package-mac.sh --universal            # arm64 + x86_64 .app
#   ./script/package-mac.sh --universal --dmg      # ...and a .dmg beside it
#   ./script/package-mac.sh --universal --dmg --notarize  # ...Developer ID + notarized
#   ./script/package-mac.sh --universal --dmg --publish   # ...and a GitHub Release
#                                                         #    + the Homebrew tap
#
# Without --notarize the bundle is ad-hoc signed for local use and Gatekeeper
# requires a right-click → Open on any other Mac. --publish implies --notarize,
# so a published release is never ad-hoc signed. The signing, submission, and
# stapling themselves live in script/notarize-mac.sh. --publish also updates
# the Homebrew cask (script/update-homebrew-cask.sh); OO_SKIP_HOMEBREW=1 skips
# that, e.g. while the repository is still private.
#
# Design + the measurements behind it:
#   docs/superpowers/specs/2026-07-26-universal-dmg-packaging-design.md
#   docs/superpowers/specs/2026-08-04-signing-notarization-design.md
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP_NAME="openOrchestra"
CRATE="$ROOT/crates/harness-app"
APP="$ROOT/dist/$APP_NAME.app"

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

usage() {
  sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'
}

universal=0
make_dmg=0
publish=0
notarize=0

while (($#)); do
  case "$1" in
    --universal) universal=1 ;;
    --dmg)       make_dmg=1 ;;
    --notarize)  notarize=1 ;;
    --publish)   publish=1 ;;
    -h|--help)   usage; exit 0 ;;
    *)           die "unknown flag: $1 (try --help)" ;;
  esac
  shift
done

# Publishing without building the artifact would upload nothing. Flags are
# otherwise single-purpose so the Makefile spells out its intent.
if ((publish)) && ((!make_dmg)); then
  die "--publish requires --dmg"
fi

# The one deliberate implication. An ad-hoc signed public release is the exact
# failure this pipeline exists to prevent, and "I forgot --notarize" is too easy
# a way to ship one — the mistake is only visible to whoever downloads it.
if ((publish)); then
  notarize=1
fi

# Notarization signs the .app before the image is built, so it needs --dmg to
# have an image to sign second. Signing a bundle nobody can install is not
# useful on its own; --sign-only on notarize-mac.sh covers that case directly.
if ((notarize)) && ((!make_dmg)); then
  die "--notarize requires --dmg (or use: ./script/notarize-mac.sh --sign-only <app>)"
fi

# ---------------------------------------------------------------------------
# Version. Single source of truth is the crate manifest, bumped by hand for
# every release (semver: patch = fixes, minor = features, major = a change
# older builds can't follow, such as unreadable ~/.openorchestra state). The tag
# and DMG carry only that version. The commit count is the
# monotonically-increasing build number macOS wants in CFBundleVersion, and
# stays inside the bundle: a `+<build>` in the tag had to be URL-encoded
# everywhere and split apart again for Homebrew.
# ---------------------------------------------------------------------------
VERSION="$(sed -n '/^\[package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p;}' "$CRATE/Cargo.toml" | head -1)"
[ -n "$VERSION" ] || die "could not parse version from $CRATE/Cargo.toml"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || die "version in $CRATE/Cargo.toml must be MAJOR.MINOR.PATCH (got: $VERSION)"
BUILD="$(git -C "$ROOT" rev-list --count HEAD)"
# The commit this run builds. Publishing tags it and cuts the notes from it,
# not whatever HEAD is by then: the build takes long enough for that to move.
BUILD_SHA="$(git -C "$ROOT" rev-parse HEAD)"
TAG="v$VERSION"

# ---------------------------------------------------------------------------
# Preflight. Check everything that can fail *before* starting a 40-minute
# build, so a missing `gh` login doesn't surface after the compile.
# ---------------------------------------------------------------------------
if ((publish)); then
  command -v gh >/dev/null 2>&1 || die "gh is required to publish: brew install gh"
  gh auth status >/dev/null 2>&1 || die "gh is not authenticated: run 'gh auth login'"
  [ -z "$(git -C "$ROOT" status --porcelain)" ] \
    || die "working tree is dirty — a published build must be reproducible from its tag"
  # The changelog is cut against the previous release tag, so a stale tag cache
  # would silently widen the range.
  git -C "$ROOT" fetch --tags --quiet || true
  if git -C "$ROOT" rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    die "tag $TAG already exists — bump version in $CRATE/Cargo.toml"
  fi
  REPO="$(gh repo view --json nameWithOwner --jq .nameWithOwner)"
  [ -n "$REPO" ] || die "could not determine the GitHub repository"
  if [ "${OO_SKIP_HOMEBREW:-0}" != 1 ]; then
    say "preflight: homebrew tap"
    "$ROOT/script/update-homebrew-cask.sh" --check \
      || die "homebrew preflight failed (set OO_SKIP_HOMEBREW=1 to release without updating the tap)"
  fi
fi

# Same rule, applied to Apple: a missing certificate or an unauthenticated
# notarytool profile should cost two seconds, not a completed build. Resolving
# the identity here is the real check — it exercises the same code path, and the
# team assertion, that will sign the bundle an hour from now.
if ((notarize)); then
  [ -x "$ROOT/script/notarize-mac.sh" ] || die "script/notarize-mac.sh is missing or not executable"
  say "preflight: signing identity"
  "$ROOT/script/notarize-mac.sh" --check
fi

say "version $VERSION (build $BUILD)"

# The remote web client (web/remote/dist) is embedded into the binary at compile
# time by crates/remote/build.rs; without it the app ships a 503 placeholder.
say "building the remote web client"
make -C "$ROOT" web
[ -f "$ROOT/web/remote/dist/index.html" ] || die "web/remote/dist missing — make web failed"

# ---------------------------------------------------------------------------
# Build. The native slice is built WITHOUT --target on purpose: cargo keys its
# output directory on the flag rather than the resolved triple, so passing
# --target aarch64-apple-darwin would ignore the several GB already compiled in
# target/release and rebuild the arch we already have. Only the Intel slice is
# genuinely cold.
# ---------------------------------------------------------------------------
BIN="$CRATE/target/release/harness-app"

if ((universal)); then
  if ! rustup target list --installed | grep -qx x86_64-apple-darwin; then
    say "adding rust target x86_64-apple-darwin"
    rustup target add x86_64-apple-darwin
  fi

  say "building native slice (reuses target/release)"
  ( cd "$CRATE" && cargo build --release )

  say "building x86_64 slice (cold the first time — ~3-5 min, then incremental)"
  ( cd "$CRATE" && cargo build --release --target x86_64-apple-darwin )

  X86_BIN="$CRATE/target/x86_64-apple-darwin/release/harness-app"
  [ -f "$BIN" ]     || die "native binary not found at $BIN"
  [ -f "$X86_BIN" ] || die "x86_64 binary not found at $X86_BIN"

  say "lipo → universal binary"
  UNIVERSAL_BIN="$CRATE/target/harness-app-universal"
  lipo -create -output "$UNIVERSAL_BIN" "$BIN" "$X86_BIN"
  BIN="$UNIVERSAL_BIN"
  ARCH_TAG="universal"
else
  say "building release binary (this takes a few minutes cold)"
  ( cd "$CRATE" && cargo build --release )
  [ -f "$BIN" ] || die "binary not found at $BIN"
  ARCH_TAG="$(uname -m)"
fi

# ---------------------------------------------------------------------------
# Assemble the bundle. Order matters: strip and lipo must both precede
# codesign, since any mutation of the bundle invalidates the signature.
# ---------------------------------------------------------------------------
say "assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/$APP_NAME"
chmod +x "$APP/Contents/MacOS/$APP_NAME"

# Strip the bundled copy, never the cargo artifact — stripping in target/ would
# make the next incremental build think it needs to relink.
strip -x "$APP/Contents/MacOS/$APP_NAME"

cp "$ROOT/assets/openOrchestra.icns" "$APP/Contents/Resources/$APP_NAME.icns"
# Notification sounds (UNNotificationSound resolves bundle resources by name).
# All of them, not a named one: the settings picker offers every bundled sound,
# and one left out of the bundle is a choice that silently plays nothing.
cp "$ROOT"/crates/harness-app/sounds/*.wav "$APP/Contents/Resources/"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>               <string>${APP_NAME}</string>
  <key>CFBundleDisplayName</key>        <string>openOrchestra</string>
  <key>CFBundleIdentifier</key>         <string>ai.openorchestra.app</string>
  <key>CFBundleVersion</key>            <string>${BUILD}</string>
  <key>CFBundleShortVersionString</key> <string>${VERSION}</string>
  <key>CFBundlePackageType</key>        <string>APPL</string>
  <key>CFBundleExecutable</key>         <string>${APP_NAME}</string>
  <key>CFBundleIconFile</key>           <string>${APP_NAME}</string>
  <key>LSMinimumSystemVersion</key>     <string>12.0</string>
  <key>NSHighResolutionCapable</key>    <true/>
  <key>LSApplicationCategoryType</key>  <string>public.app-category.developer-tools</string>
</dict>
</plist>
PLIST

# Refresh the icon cache so Finder shows the new icon immediately. Done *before*
# signing: it is the last thing that mutates the bundle, and a mutation after
# signing is what invalidates a signature.
touch "$APP"

# ---------------------------------------------------------------------------
# Sign. Two mutually exclusive paths:
#
#   --notarize   Developer ID + hardened runtime, submitted to Apple and
#                stapled. What a download needs to open on a plain double-click.
#   otherwise    ad-hoc, which carries no identity at all: fine on the machine
#                that built it, quarantined everywhere else.
#
# No --deep in either path. Apple deprecated it, and it pushes entitlements onto
# nested code; this bundle holds exactly one Mach-O, so signing it directly is
# both correct and sufficient.
# ---------------------------------------------------------------------------
if ((notarize)); then
  "$ROOT/script/notarize-mac.sh" "$APP"
elif command -v codesign >/dev/null 2>&1; then
  say "ad-hoc signing"
  codesign --force --sign - "$APP" || echo "(codesign failed; app still runnable via right-click → Open)"
fi

say "built: $APP"
lipo -info "$APP/Contents/MacOS/$APP_NAME" 2>/dev/null || true

# ---------------------------------------------------------------------------
# DMG. Stage the bundle next to an /Applications symlink so the mounted volume
# is the conventional drag-to-install window, then dress that window: large
# icons on either side of an arrow (assets/dmg-background*.png, drawn by
# script/gen-dmg-background.py, which shares this layout).
# ---------------------------------------------------------------------------
DMG="$ROOT/dist/$APP_NAME-$VERSION-$ARCH_TAG.dmg"

# Window layout in points. Must match W/H/ICON/*_X/ICON_Y in
# script/gen-dmg-background.py, or the arrow won't sit between the icons.
DMG_W=720 DMG_H=420 DMG_ICON=160
DMG_APP_X=200 DMG_APPS_X=520 DMG_ICON_Y=190

# Lay out the mounted read-write volume's Finder window. Finder saves the result
# to the volume's .DS_Store, which is what every later mount reads.
style_dmg_window() {
  local disk="$1"
  osascript <<OSA
tell application "Finder"
  tell disk "$disk"
    open
    delay 1
    set w to container window
    set current view of w to icon view
    set toolbar visible of w to false
    set statusbar visible of w to false
    set the bounds of w to {200, 120, $((200 + DMG_W)), $((120 + DMG_H))}
    set opts to the icon view options of w
    set arrangement of opts to not arranged
    set icon size of opts to $DMG_ICON
    set text size of opts to 13
    set background picture of opts to file ".background:background.tiff"
    set position of item "$APP_NAME.app" of w to {$DMG_APP_X, $DMG_ICON_Y}
    set position of item "Applications" of w to {$DMG_APPS_X, $DMG_ICON_Y}
    update without registering applications
    delay 1
    close
  end tell
end tell
OSA
}

if ((make_dmg)); then
  say "creating $(basename "$DMG")"
  STAGE="$ROOT/dist/.dmg-stage"
  RW_DMG="$ROOT/dist/.dmg-rw.dmg"
  rm -rf "$STAGE" "$RW_DMG"
  mkdir -p "$STAGE/.background"
  # ditto, not cp -R: the app carries a stapled notarization ticket and a code
  # signature, and ditto is the copy that preserves bundle metadata verbatim.
  ditto "$APP" "$STAGE/$APP_NAME.app"
  ln -s /Applications "$STAGE/Applications"
  # One TIFF holding both resolutions, so Finder picks the sharp one on Retina.
  tiffutil -cathidpicheck "$ROOT/assets/dmg-background.png" "$ROOT/assets/dmg-background@2x.png" \
    -out "$STAGE/.background/background.tiff" >/dev/null 2>&1 \
    || die "could not build the DMG background (run: make dmg-background)"

  # Built writable first: Finder can only save the window layout to a volume it
  # can write. Compressed to the read-only image afterwards.
  hdiutil create -volname "$APP_NAME" -srcfolder "$STAGE" -fs HFS+ -format UDRW -ov "$RW_DMG" >/dev/null
  rm -rf "$STAGE"
  # Not -nobrowse: Finder has to see the volume to lay it out. The mount point,
  # not the volume name, names it to Finder, since a second copy already
  # mounted would make this one "openOrchestra 1".
  MOUNT="$(hdiutil attach -readwrite -noverify -noautoopen "$RW_DMG" | awk -F'\t' '/\/Volumes\//{print $NF; exit}')"
  [ -d "$MOUNT" ] || die "could not mount $RW_DMG"
  say "laying out the installer window"
  # Driving Finder needs Automation permission for the terminal running this.
  # Without it the image still installs, just as a plain window, so that's a
  # warning for a local build and a failure for anything going to users.
  if ! style_dmg_window "$(basename "$MOUNT")"; then
    if ((notarize)); then
      hdiutil detach "$MOUNT" -quiet || true
      die "could not lay out the DMG window: allow this terminal to control Finder (System Settings → Privacy & Security → Automation)"
    fi
    echo "(could not lay out the DMG window — it will open as a plain Finder window)"
  fi
  chmod -Rf go-w "$MOUNT" || true
  sync
  # Finder can hold the volume for a moment after closing its window.
  for _ in 1 2 3 4 5; do
    hdiutil detach "$MOUNT" -quiet && break
    sleep 2
  done
  [ ! -d "$MOUNT" ] || hdiutil detach "$MOUNT" -force -quiet

  rm -f "$DMG"
  hdiutil convert "$RW_DMG" -format UDZO -imagekey zlib-level=9 -o "$DMG" >/dev/null
  rm -f "$RW_DMG"
  say "verifying image"
  hdiutil verify "$DMG" >/dev/null || die "hdiutil verify failed for $DMG"

  # The image gets its own signature and its own ticket. Notarizing only the app
  # would leave the .dmg unsigned, and notarizing only the .dmg would leave an
  # app dragged to /Applications without a ticket of its own — which fails to
  # launch offline, or behind a firewall blocking Apple's OCSP responder.
  if ((notarize)); then
    "$ROOT/script/notarize-mac.sh" "$DMG"
  fi

  say "built: $DMG ($(du -h "$DMG" | cut -f1))"
fi

# ---------------------------------------------------------------------------
# Publish. Outward-facing and awkward to retract, so it never happens as a side
# effect of a build — only under an explicit --publish.
# ---------------------------------------------------------------------------
# The install note is a function of how the artifact was signed. A notarized
# build needs no instructions at all, and shipping the right-click ritual
# alongside one would train users to bypass Gatekeeper for no reason.
if ((notarize)); then
  INSTALL_NOTE="**Install:** open the DMG and drag openOrchestra to Applications.

Signed with Developer ID and notarized by Apple — it opens on a double-click.

Universal binary: Apple Silicon and Intel."
else
  INSTALL_NOTE="**First launch:** right-click the app → **Open**, then confirm. macOS
quarantines apps from unidentified developers, so a plain double-click will say
the app is damaged. This is expected — the build is ad-hoc signed, not notarized.

Universal binary: Apple Silicon and Intel."
fi

# ---------------------------------------------------------------------------
# Release notes. GitHub's --generate-notes lists *pull requests* only, so work
# pushed straight to master disappears from the changelog no matter who wrote
# it. Build the notes from the commits instead, resolving each author to their
# GitHub handle so everyone who shipped in the range is credited.
# ---------------------------------------------------------------------------
release_notes() {
  local prev range sha subject author handle
  local -a handles=()

  prev="$(git -C "$ROOT" describe --tags --abbrev=0 "$BUILD_SHA" 2>/dev/null || true)"
  range="${prev:+$prev..}$BUILD_SHA"

  printf '%s\n\n' "$INSTALL_NOTE"
  # This version's CHANGELOG.md section (cut by release-version.sh) leads:
  # it says what changed for users. The commit list below stays for detail.
  local notes
  notes="$(awk -v v="$VERSION" '
    index($0, "## " v " - ") == 1 { on = 1; next }
    on && /^## / { exit }
    on { print }
  ' "$ROOT/CHANGELOG.md" 2>/dev/null | sed -e '/./,$!d')"
  if [ -n "$notes" ]; then
    printf '%s\n\n' "$notes"
  fi
  printf "## What's Changed\n"
  while IFS=$'\x1f' read -r sha subject author; do
    handle="$(gh api "repos/$REPO/commits/$sha" --jq '.author.login // empty' 2>/dev/null || true)"
    handle="${handle:+@$handle}"
    handle="${handle:-$author}"
    handles+=("$handle")
    printf '* %s by %s in https://github.com/%s/commit/%s\n' \
      "$subject" "$handle" "$REPO" "${sha:0:7}"
  done < <(git -C "$ROOT" log --no-merges --reverse --format=$'%H\x1f%s\x1f%an' "$range")

  printf '\n## Contributors\n'
  printf '%s\n' "${handles[@]}" | sort | uniq -c | sort -rn | while read -r count handle; do
    printf '* %s — %s commit%s\n' "$handle" "$count" "$([ "$count" -eq 1 ] || echo s)"
  done

  if [ -n "$prev" ]; then
    printf '\n**Full Changelog**: https://github.com/%s/compare/%s...%s\n' "$REPO" "$prev" "$TAG"
  else
    printf '\n**Full Changelog**: https://github.com/%s/commits/%s\n' "$REPO" "$TAG"
  fi
}

if ((publish)); then
  # The DMG must be exactly BUILD_SHA. Stop if the checkout moved or changed
  # during the build rather than publish a tag that names other code.
  [ "$(git -C "$ROOT" rev-parse HEAD)" = "$BUILD_SHA" ] \
    || die "HEAD moved during the build (built ${BUILD_SHA:0:7}, now $(git -C "$ROOT" rev-parse --short HEAD)); not publishing"
  [ -z "$(git -C "$ROOT" status --porcelain --untracked-files=no)" ] \
    || die "tracked files changed during the build; not publishing"
  say "publishing GitHub Release $TAG"
  # --target: tag the commit that was built. Without it gh tags the default
  # branch's tip on GitHub, which need not be this commit.
  gh release create "$TAG" "$DMG" \
    --target "$BUILD_SHA" \
    --title "$APP_NAME $VERSION ($BUILD)" \
    --notes "$(release_notes)"
  say "published: $(gh release view "$TAG" --json url --jq .url)"
  if [ "${OO_SKIP_HOMEBREW:-0}" != 1 ]; then
    # The release is already public at this point, so a tap failure must not
    # read as a failed release: say exactly how to finish the job instead.
    "$ROOT/script/update-homebrew-cask.sh" "$TAG" "$DMG" \
      || die "release $TAG is published, but the Homebrew tap was not updated; retry with: ./script/update-homebrew-cask.sh $TAG"
  fi
fi

say "done"
if ((!publish)); then
  echo "    open it with:  open \"$APP\""
fi
