# Homebrew cask

`openorchestra.rb` is the template for the cask in the
[`omarpr/homebrew-tap`](https://github.com/omarpr/homebrew-tap) repository
(`brew tap omarpr/tap`). It holds placeholders, not a real version; never point
Homebrew at this copy.

## How a release reaches Homebrew

`make release` (`script/package-mac.sh --universal --dmg --publish`) does it:

1. Preflight, before the build: `script/update-homebrew-cask.sh --check`
   confirms `brew`, `gh`, a clean local checkout of the tap, and that this
   repository is **public**. Homebrew downloads anonymously, so a cask for a
   private repository's release would 404 for every user.
2. After `gh release create`, `script/update-homebrew-cask.sh <tag> <dmg>`
   downloads the DMG GitHub actually serves, checks it matches the local one,
   fills in the version and sha256, runs `brew style` and
   `brew audit --cask --online --strict`, and commits and pushes the tap.

If the tap step fails, the GitHub Release is already published. The error says
to rerun `./script/update-homebrew-cask.sh <tag>` by hand, which is also how you
backfill an existing release. Set `OO_SKIP_HOMEBREW=1` to release without
touching the tap.

## Version format

Tags are `v<major>.<minor>.<patch>` (for example `v1.0.0`), the cask version is
the same number, and the asset is `openOrchestra-<version>-universal.dmg`.
`livecheck` reads the latest GitHub release. Releases before 1.0.0 used
`v0.1.0+<build>` tags; the script doesn't handle those, and no cask was ever
published for them.

## Install

```sh
brew install --cask omarpr/tap/openorchestra
```

The cask depends on the `tmux` formula, which the app requires.
