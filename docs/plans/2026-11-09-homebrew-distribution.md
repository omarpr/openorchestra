# Homebrew distribution

Status: implemented as option 1 on 2026-11-17: a cask in the public
`omarpr/homebrew-tap`, updated by `make release`. See
[`packaging/homebrew/README.md`](../../packaging/homebrew/README.md).

## Requirements

A Homebrew cask can install a prebuilt macOS app, but its release artifact must
be anonymously downloadable. A cask in a separately maintained tap is the
smallest path; it needs a stable URL and checksum for each release.

The app is GPL-3.0-or-later. Any binary distribution plan must include a
corresponding source offer that meets the license terms. Keep release artifacts,
source tags, and checksums tied to the same version.

## Options

1. Publish source and signed app releases from this repository, then add a cask
   to a public tap.
2. Publish app artifacts from a separate public release repository or file
   host, while keeping source and corresponding source access available.
3. Defer Homebrew and keep distributing the DMG directly.

## Before implementation

- Choose the public artifact host and stable naming scheme.
- Decide how the cask will be versioned and updated.
- Verify that every release artifact has a checksum and accessible source for
  the exact tagged version.
- Reuse the signing and notarization pipeline in `script/package-mac.sh`.
