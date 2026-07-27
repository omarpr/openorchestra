# Universal DMG packaging, versioning, and GitHub Releases

**Date:** 2026-07-26
**Status:** approved

## Problem

`script/package-mac.sh` produces an arm64-only `dist/openOrchestra.app` and stops
there. Three gaps follow, and each blocks handing the app to anyone else:

1. **No Intel support.** The bundle carries a single `aarch64` slice, so it does
   not launch on an Intel Mac at all.
2. **No distributable artifact.** A `.app` directory is not something you send
   someone. There is no `.dmg` step anywhere in the repo, and `dist/` is
   gitignored (`.gitignore:6`), so nothing built has ever been published.
3. **No version.** `CFBundleShortVersionString` and `CFBundleVersion` are both
   hardcoded to `0.1.0` at `package-mac.sh:41-42`, duplicating
   `crates/harness-app/Cargo.toml:14`. Two builds a month apart are
   indistinguishable to macOS, to Finder, and to whoever downloaded them.

## Constraints discovered before designing

These measurements drove most of the decisions below and are recorded so the
reasoning survives.

**The artifact is large.** The arm64 release binary is 171.6 MB; `strip -x`
takes it to 141.1 MB, and it gzips to 51.1 MB. A universal binary is both slices
lipo'd together, so expect ~285 MB stripped and a **~105 MB UDZO DMG**.

**Therefore the DMG cannot be committed to git.** GitHub hard-rejects any single
file over 100 MB on push and warns at 50 MB, so a universal DMG would likely
fail `git push` outright — and each committed version would live in every future
clone permanently. This ruled out the original "commit the DMG" plan in favour
of GitHub Releases, which has a 2 GB per-asset limit and costs the repo nothing.
`dist/` stays gitignored.

**Cross-compiling reuses downloads but not compilation.** `~/.cargo/registry`
(1.9 GB) and `~/.cargo/git` (920 MB, the zed cone) are crate *sources* and are
architecture-independent, so the x86_64 build re-downloads nothing. But
`crates/harness-app/target/release` (7.0 GB, 1390 rlibs) is arm64 machine code
and cannot be reused. The Intel slice is a genuinely cold compile of the whole
zed cone: **20–40 minutes** the first time, incremental thereafter.

**Build locally, not in CI.** GitHub Actions starts with none of that 9.8 GB
warm cache, GitHub's per-repo cache ceiling is 10 GB (which the zed cone alone
would nearly exhaust), and macOS runner minutes bill at 10×. Local is
dramatically cheaper until someone other than the author needs to cut releases.

## Design

### 1. Version derivation

Both bundle version fields are computed at package time; no version string stays
hardcoded in the script.

```
VERSION = crates/harness-app/Cargo.toml  [package] version   -> 0.1.0
BUILD   = git rev-list --count HEAD                          -> 127
```

- `CFBundleShortVersionString = $VERSION` — the human/marketing version. This is
  the field Finder and the About box show.
- `CFBundleVersion = $BUILD` — the build number. macOS only requires this to
  increase monotonically, which is exactly what a commit count does, so two
  builds of the same `0.1.0` remain distinguishable.

This is already semver, at `0.1.0`. Adopting semver "properly" later is a
one-line edit to `Cargo.toml:14` — the script needs no change, and there is no
migration or version-number discontinuity to manage. That is the whole reason
for splitting the two fields rather than stamping the commit count into both.

Full version string is `$VERSION+$BUILD` (`0.1.0+127`), using semver's `+`
build-metadata syntax. Known cosmetic risk: GitHub has been historically
inconsistent about preserving `+` in *release-asset filenames*. The git tag is
unaffected, and a mangled asset filename is harmless, so this is accepted rather
than worked around.

### 2. Universal build

The native slice is built **without** `--target`, and only the Intel slice with
it:

```sh
cargo build --release                               # reuses target/release/ (7 GB, warm)
cargo build --release --target x86_64-apple-darwin  # the only cold build
lipo -create -output "$UNIVERSAL" \
  target/release/harness-app \
  target/x86_64-apple-darwin/release/harness-app
```

This asymmetry is deliberate and load-bearing. Cargo keys its output directory
on the `--target` flag, not on the resolved triple: passing
`--target aarch64-apple-darwin` would write to `target/aarch64-apple-darwin/release/`,
an empty directory, and rebuild all 1390 units from scratch **for the
architecture already fully built**. Omitting the flag keeps the existing warm
cache and turns two cold builds into one.

`rustup target add x86_64-apple-darwin` is run on demand when `--universal` is
passed, matching the idempotent install style of `script/setup-macos.sh`. It is
deliberately not added to `setup-macos.sh`, which every contributor runs — no
reason to make everyone download a target they will never build.

### 3. Bundle assembly order

Order is load-bearing; getting it wrong yields a bundle that will not launch.

1. build both slices
2. `lipo -create`
3. `strip -x` — saves ~60 MB across the two slices
4. assemble `.app`: binary, `openOrchestra.icns`, `sounds/*.wav`, `Info.plist`
5. `codesign --force --deep --sign -` — **last**; any mutation after signing
   invalidates the signature

Steps 4–5 are today's script unchanged; 1–3 slot in ahead of them. Note that
stripping must precede signing for the same reason.

### 4. DMG

Stage a directory containing the `.app` plus a symlink to `/Applications`, so
the mounted volume presents the conventional drag-to-install window, then:

```sh
hdiutil create -volname openOrchestra -srcfolder "$STAGE" -ov -format UDZO \
  "dist/openOrchestra-$VERSION+$BUILD-universal.dmg"
```

### 5. Publishing

```sh
gh release create "v$VERSION+$BUILD" "$DMG" --generate-notes
```

Preconditions, all checked before any work begins:

- `gh` present and `gh auth status` clean
- working tree clean — a published build must be reproducible from its tag
- the tag does not already exist

The bundle is **ad-hoc signed** (no Apple Developer ID available). Downloads
from GitHub Releases are therefore quarantined by Gatekeeper, so the release
notes lead with the workaround:

> First launch: right-click the app → Open, then confirm. macOS quarantines apps
> from unidentified developers.

The signing step is kept isolated so Developer ID + `notarytool submit` +
`stapler staple` can replace it later without restructuring the script.

### 6. Interface

`script/package-mac.sh [--universal] [--dmg] [--publish]`, driven by three make
targets:

| target | does | compiles |
| --- | --- | --- |
| `make app` | host-arch `.app` — **unchanged behaviour** | 1 |
| `make dmg` | universal `.app` + `.dmg`, local only | 2 |
| `make release` | universal + `.dmg` + GitHub Release | 2 |

Publishing is never a side effect of a build: it is outward-facing and awkward
to retract, so it requires the explicit `--publish`. Equally, the default path
stays a single fast compile so day-to-day `make app` does not get slower.

## Verification

- `lipo -info` on the assembled binary reports both `x86_64` and `arm64`.
- `arch -x86_64 dist/openOrchestra.app/Contents/MacOS/openOrchestra` launches the
  Intel slice under Rosetta 2. This is the step that matters: it proves the
  cross-build *runs*, not merely that it linked.
- `hdiutil verify` on the DMG; then mount it and `codesign --verify --deep` the
  contained `.app`.
- `shellcheck script/package-mac.sh`.
- `make app` still produces a working host-arch bundle (no regression).

## Out of scope

- Notarization and Developer ID signing — blocked on an Apple Developer account;
  the hook is left in place.
- A GitHub Actions release workflow — reconsider when releases need to be cut
  from somewhere other than the author's machine.
- Windows and Linux packaging.
