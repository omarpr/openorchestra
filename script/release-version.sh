#!/usr/bin/env bash
# Ask which version to release, before `make release` starts its long build.
#
# Shows the version in crates/harness-app/Cargo.toml and the latest release
# tag, then offers a patch / minor / major bump or keeping the version. It then
# turns CHANGELOG.md's Unreleased section into the version's section (and
# refuses to go on while Unreleased is empty), bumps Cargo.toml and Cargo.lock
# if asked, commits that as `chore(release): X`, and pushes the branch:
# package-mac.sh refuses a dirty tree, and the GitHub release must point at a
# commit GitHub has.
#
# Non-interactive (no terminal): keeps the version, or set
# OO_RELEASE_BUMP=patch|minor|major|keep to choose without a prompt.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CRATE="$ROOT/crates/harness-app"
MANIFEST="$CRATE/Cargo.toml"
LOCKFILE="$CRATE/Cargo.lock"
CHANGELOG="$ROOT/CHANGELOG.md"

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# Same parse as package-mac.sh: the [package] version.
current="$(sed -n '/^\[package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p;}' "$MANIFEST" | head -1)"
[[ "$current" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]] \
  || die "version in $MANIFEST must be MAJOR.MINOR.PATCH (got: ${current:-nothing})"
major="${BASH_REMATCH[1]}" minor="${BASH_REMATCH[2]}" patch="${BASH_REMATCH[3]}"

git -C "$ROOT" fetch --tags --quiet 2>/dev/null || true
latest="$(git -C "$ROOT" tag --list 'v[0-9]*.[0-9]*.[0-9]*' --sort=-v:refname | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | head -1 || true)"
released=0
git -C "$ROOT" rev-parse -q --verify "refs/tags/v$current" >/dev/null && released=1

next_patch="$major.$minor.$((patch + 1))"
next_minor="$major.$((minor + 1)).0"
next_major="$((major + 1)).0.0"

echo
printf '  Current version:  \033[1m%s\033[0m' "$current"
((released)) && printf '  (already released as v%s)' "$current"
echo
printf '  Latest release:   %s\n\n' "${latest:-none}"

choice="${OO_RELEASE_BUMP:-}"
if [ -z "$choice" ]; then
  if [ ! -t 0 ]; then
    choice=keep
  else
    echo "  Bump the version for this release?"
    echo "    1) patch  → $next_patch   (fixes)"
    echo "    2) minor  → $next_minor   (features)"
    echo "    3) major  → $next_major   (a change older builds can't follow)"
    if ((released)); then
      echo "    4) keep $current   (not possible: v$current is already released)"
    else
      echo "    4) keep $current"
    fi
    echo "    q) cancel"
    echo
    default=4
    ((released)) && default=1
    read -r -p "  Choice [$default]: " answer
    case "${answer:-$default}" in
      1|p|patch) choice=patch ;;
      2|m|minor) choice=minor ;;
      3|M|major) choice=major ;;
      4|k|keep)  choice=keep ;;
      q|Q)       die "release cancelled" ;;
      *)         die "unknown choice: $answer" ;;
    esac
  fi
fi

case "$choice" in
  patch) new="$next_patch" ;;
  minor) new="$next_minor" ;;
  major) new="$next_major" ;;
  keep)
    ((released)) && die "v$current is already released: pick patch, minor or major"
    new="$current"
    ;;
  *) die "OO_RELEASE_BUMP must be patch, minor, major or keep (got: $choice)" ;;
esac

# The release notes start from the changelog, so a release with nothing under
# Unreleased would ship without saying what changed.
[ -f "$CHANGELOG" ] || die "$CHANGELOG is missing"
grep -q '^## Unreleased$' "$CHANGELOG" || die "$CHANGELOG has no '## Unreleased' section"
unreleased="$(awk '/^## Unreleased$/ {on = 1; next} on && /^## / {exit} on && !/^### / && NF' "$CHANGELOG")"
[ -n "$unreleased" ] \
  || die "nothing under '## Unreleased' in CHANGELOG.md: add what changed since ${latest:-the last release} first"

# The release commit carries only the version and the changelog.
[ -z "$(git -C "$ROOT" status --porcelain)" ] \
  || die "working tree is dirty: commit or stash first, so the release commit is a commit of its own"
branch="$(git -C "$ROOT" symbolic-ref --quiet --short HEAD)" \
  || die "not on a branch (detached HEAD)"

if [ "$new" != "$current" ]; then
  say "bumping $current → $new"
  # Only the [package] version line, never a dependency's.
  sed -i '' "/^\[package\]/,/^\[/s/^version *= *\"$current\"/version = \"$new\"/" "$MANIFEST"
  # The lockfile's own entry for this package (harness-app is outside the root
  # workspace, so only its own lockfile lists it).
  awk -v old="$current" -v new="$new" '
    /^name = "harness-app"$/ { pkg = 1; print; next }
    pkg && $0 == "version = \"" old "\"" { print "version = \"" new "\""; pkg = 0; next }
    { pkg = 0; print }
  ' "$LOCKFILE" > "$LOCKFILE.tmp" && mv "$LOCKFILE.tmp" "$LOCKFILE"

  grep -q "^version = \"$new\"" "$MANIFEST" || die "could not update $MANIFEST"
  grep -A1 '^name = "harness-app"$' "$LOCKFILE" | grep -q "^version = \"$new\"" \
    || die "could not update $LOCKFILE"
else
  say "releasing $current (no version change)"
fi

# Unreleased becomes this version's section; a fresh, empty Unreleased stays on
# top for the next commits.
awk -v heading="## $new - $(date +%F)" '
  !done && /^## Unreleased$/ { print; print ""; print heading; done = 1; next }
  { print }
' "$CHANGELOG" > "$CHANGELOG.tmp" && mv "$CHANGELOG.tmp" "$CHANGELOG"
say "CHANGELOG.md: Unreleased → $new"

git -C "$ROOT" add "$MANIFEST" "$LOCKFILE" "$CHANGELOG"
git -C "$ROOT" commit --quiet -m "chore(release): $new"
say "committed chore(release): $new"

# gh releases a commit GitHub has: push before the build, not after it.
say "pushing $branch"
git -C "$ROOT" push --quiet origin "$branch" \
  || die "push failed; the release commit is local only (undo with: git reset --hard HEAD~1)"
