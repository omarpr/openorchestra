# Homebrew cask template for openOrchestra. script/update-homebrew-cask.sh fills
# in the version and sha256 placeholders for a published release and commits the
# result to the omarpr/homebrew-tap repository. Never installed directly.
# Release tags are `v<version>` (see script/package-mac.sh).

cask "openorchestra" do
  version "VERSION"
  sha256 "SHA256"

  url "https://github.com/omarpr/openorchestra/releases/download/v#{version}/openOrchestra-#{version}-universal.dmg"
  name "openOrchestra"
  desc "Native workspace for running coding agents in parallel git worktrees"
  homepage "https://github.com/omarpr/openorchestra"

  livecheck do
    url :url
    strategy :github_latest
  end

  # Agents run inside tmux sessions; the app refuses to start without it.
  depends_on formula: "tmux"
  depends_on macos: :monterey

  app "openOrchestra.app"

  uninstall quit: "ai.openorchestra.app"

  zap trash: [
    "~/.openorchestra",
    "~/Library/Preferences/ai.openorchestra.app.plist",
    "~/Library/Saved Application State/ai.openorchestra.app.savedState",
  ]
end
