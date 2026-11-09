# Distributing via Homebrew

Status: decision pending (noted 2026-11-09). Nothing is set up yet.

## Short answer

Shipping through Homebrew does **not** require open-sourcing the code. A cask in
our own tap (`omarpr/tap`) can install a closed-source, prebuilt app. The one
hard requirement is that the **download URL is publicly reachable** without
authentication.

## How LithePG does it (reference)

- Shipped as a **cask** (`Casks/lithepg.rb`), not a formula, in the public tap
  repo `omarpr/homebrew-tap`. Install: `brew install omarpr/tap/lithepg`.
- The cask downloads a prebuilt, Developer ID signed and notarized
  `LithePG-<version>.zip` from GitHub Releases on `omarpr/lithepg`:
  `https://github.com/omarpr/lithepg/releases/download/v<version>/LithePG-<version>.zip`
- That works only because `omarpr/lithepg` is a **public, MIT-licensed** repo.
- Template and release steps: `~/projects/lithepg/packaging/homebrew/`
  (`lithepg.rb`, `README.md`) and `~/projects/lithepg/docs/RELEASING.md`.

## What's different for this repo

- `omarpr/openorchestra` is **private**. Release assets on a private repo can't
  be downloaded anonymously, so copying the LithePG setup verbatim would fail
  for every user.
- The repo carries a **GPL-3.0 `LICENSE`** (GitHub reports it as GPL). Shipping
  binaries under GPL obliges us to provide source to recipients. As copyright
  holder we can relicense, but that should be a deliberate choice.

## Options

1. **Closed source, own tap (closest to LithePG)**
   - Keep the source repo private.
   - Publish only the signed + notarized `.zip` (and `.sha256` sidecar) to a
     public location: a releases-only public repo (e.g.
     `omarpr/openorchestra-releases`), a website, or an R2/S3 bucket.
   - Add `Casks/<app>.rb` to `omarpr/tap` pointing at that URL.
   - Replace the GPL `LICENSE` with a proprietary/EULA license.
2. **Open source**
   - Make the repo public and follow the LithePG flow exactly.
3. **Official Homebrew repos** (not recommended)
   - `homebrew-core` (formulae built from source) requires an open-source license.
   - The official `homebrew-cask` accepts closed-source apps but has notability
     requirements. Our own tap avoids both.

## Open questions before choosing option 1

- [ ] **Blocker: harness-app links GPL zed crates.** Of the 126 zed crates in
      `crates/harness-app`'s dependency graph, 93 are `GPL-3.0-or-later` —
      including the ones the app is built on: `editor`, `workspace`,
      `project_panel`, `git_ui`, `terminal`, `language`, `lsp`. Only gpui and
      a few base crates are Apache-2.0. That is why
      `crates/harness-app/Cargo.toml` declares `license = "GPL-3.0-or-later"`
      (accepted in `docs/architecture.md` §3.2). We don't hold zed's
      copyright, so relicensing our own code doesn't change this: a
      closed-source binary would first need every GPL zed crate replaced,
      i.e. the editor, file tree, Git panel and terminal rebuilt on gpui
      alone. Short of that, option 1 is off the table and option 2 (or
      shipping the source under GPL) is the path. Re-check with
      `cargo metadata` from `crates/harness-app` — the repo-root workspace
      excludes harness-app.
- [ ] Audit the remaining dependency licenses (Cargo crates, bundled JS/assets). Any GPL/AGPL
      dependency linked into the binary conflicts with closed-source
      distribution. `cargo license` / `cargo deny check licenses` are good starts.
- [ ] Pick the public hosting location for release artifacts.
- [ ] Decide the replacement license.
- [ ] Reuse LithePG's signing/notarization and cask release helpers.
