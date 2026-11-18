#!/usr/bin/env bash
# Publish a released openOrchestra build to the Homebrew tap.
#
#   ./script/update-homebrew-cask.sh --check            # preflight only
#   ./script/update-homebrew-cask.sh <tag> [<dmg>]      # e.g. v0.1.0+294
#
# Renders packaging/homebrew/openorchestra.rb for <tag>, writes it to
# Casks/openorchestra.rb in the local checkout of the tap (`brew tap
# omarpr/tap` first), runs brew style + audit, then commits and pushes the tap.
# `make release` runs this after the GitHub Release is created. Run it by hand
# to backfill a tag, or to retry after a failed tap push.
#
# The sha256 is taken from the asset GitHub actually serves (downloaded with
# gh), never from a local file alone: a cask whose checksum matches a DMG that
# was rebuilt after upload fails for every user. When <dmg> is given, it must
# match the downloaded asset too.
#
# Env:
#   OO_HOMEBREW_TAP   tap name (default: omarpr/tap)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TEMPLATE="$ROOT/packaging/homebrew/openorchestra.rb"
TAP="${OO_HOMEBREW_TAP:-omarpr/tap}"
CASK_NAME="openorchestra"
SOURCE_REPO="omarpr/openorchestra"

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

usage() {
  sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'
}

# Everything that can fail before touching the tap. Cheap, so package-mac.sh
# runs it before a long release build rather than after it.
preflight() {
  for cmd in brew gh git ruby shasum; do
    command -v "$cmd" >/dev/null 2>&1 || die "$cmd is required"
  done
  [ -f "$TEMPLATE" ] || die "cask template missing: $TEMPLATE"
  gh auth status >/dev/null 2>&1 || die "gh is not authenticated: run 'gh auth login'"

  # Homebrew downloads anonymously. A cask pointing at a private repo's
  # release passes every local check and then 404s for every user.
  local visibility
  visibility="$(gh repo view "$SOURCE_REPO" --json visibility --jq .visibility)"
  [ "$visibility" = "PUBLIC" ] \
    || die "$SOURCE_REPO is $visibility; Homebrew can't download its releases until it is public"

  TAP_DIR="$(brew --repository "$TAP" 2>/dev/null)" || true
  [ -n "${TAP_DIR:-}" ] && [ -d "$TAP_DIR/.git" ] \
    || die "tap $TAP is not checked out; run: brew tap $TAP"
  git -C "$TAP_DIR" remote get-url origin >/dev/null 2>&1 || die "tap $TAP has no origin remote"
  [ -z "$(git -C "$TAP_DIR" status --porcelain)" ] \
    || die "tap checkout $TAP_DIR has uncommitted changes; commit or discard them first"
}

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
  usage
  exit 0
fi

if [ "${1:-}" = "--check" ]; then
  preflight
  say "homebrew preflight passed ($TAP)"
  exit 0
fi

[ $# -ge 1 ] && [ $# -le 2 ] || { usage >&2; exit 2; }
TAG="$1"
LOCAL_DMG="${2:-}"

[[ "$TAG" =~ ^v([0-9]+(\.[0-9]+)+)\+([0-9]+)$ ]] \
  || die "tag must look like v<version>+<build>, e.g. v0.1.0+294 (got: $TAG)"
VERSION="${BASH_REMATCH[1]}"
BUILD="${BASH_REMATCH[3]}"
ASSET="openOrchestra-$VERSION+$BUILD-universal.dmg"

preflight

say "fetching $ASSET from release $TAG"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
gh release download "$TAG" --repo "$SOURCE_REPO" --pattern "$ASSET" --dir "$WORK" \
  || die "release $TAG has no asset named $ASSET"
SHA="$(shasum -a 256 "$WORK/$ASSET" | awk '{print $1}')"

if [ -n "$LOCAL_DMG" ]; then
  [ -f "$LOCAL_DMG" ] || die "no such file: $LOCAL_DMG"
  LOCAL_SHA="$(shasum -a 256 "$LOCAL_DMG" | awk '{print $1}')"
  [ "$LOCAL_SHA" = "$SHA" ] \
    || die "local $LOCAL_DMG does not match the uploaded asset (local $LOCAL_SHA, release $SHA)"
fi

say "updating $TAP to $VERSION,$BUILD"
git -C "$TAP_DIR" pull --ff-only --quiet
mkdir -p "$TAP_DIR/Casks"
CASK="$TAP_DIR/Casks/$CASK_NAME.rb"
sed -e "s/^  version \"VERSION_CSV\"$/  version \"$VERSION,$BUILD\"/" \
    -e "s/^  sha256 \"SHA256\"$/  sha256 \"$SHA\"/" "$TEMPLATE" > "$CASK"
grep -q -e VERSION_CSV -e '"SHA256"' "$CASK" && die "template placeholders were not all filled in"
ruby -c "$CASK" >/dev/null || die "rendered cask is not valid Ruby"

# On any failure below, put the tap back the way it was, so a retry starts
# from a clean checkout instead of tripping the dirty-tap preflight.
restore_tap() {
  git -C "$TAP_DIR" checkout --quiet -- "Casks/$CASK_NAME.rb" 2>/dev/null \
    || rm -f "$CASK"
}
trap 'restore_tap; rm -rf "$WORK"' ERR

brew style "$CASK"
brew audit --cask --online --strict "$TAP/$CASK_NAME"

git -C "$TAP_DIR" add -- "Casks/$CASK_NAME.rb"
trap - ERR
if git -C "$TAP_DIR" diff --cached --quiet; then
  say "tap already has $CASK_NAME $VERSION,$BUILD; nothing to commit"
else
  git -C "$TAP_DIR" commit --quiet -m "$CASK_NAME $VERSION,$BUILD"
fi
# Push even when there was nothing to commit: a previous run may have
# committed and then failed to push, and its commit is the one users need.
git -C "$TAP_DIR" push --quiet origin HEAD \
  || die "committed to $TAP_DIR but the push failed; retry with: git -C \"$TAP_DIR\" push origin HEAD"

say "published $CASK_NAME $VERSION,$BUILD to $TAP"
echo "    install with:  brew install --cask $TAP/$CASK_NAME"
