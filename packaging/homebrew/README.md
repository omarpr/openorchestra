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

Tags are `v<version>+<build>` (for example `v0.1.0+294`). The cask version is
`<version>,<build>`, and the URL rebuilds the tag with the `+` encoded as `%2B`,
which is how GitHub serves both the tag and the asset name. `livecheck` reads
the latest release's tag the same way.

## Install

```sh
brew install --cask omarpr/tap/openorchestra
```

The cask depends on the `tmux` formula, which the app requires.
