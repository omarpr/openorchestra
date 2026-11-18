# Homebrew cask template for openOrchestra. script/update-homebrew-cask.sh fills
# in the version and sha256 placeholders for a published release and commits the
# result to the omarpr/homebrew-tap repository. Never installed directly.
#
# Release tags are `v<version>+<build>` (see script/package-mac.sh), so the cask
# version is "<version>,<build>" and the `+` is URL-encoded as %2B, which is how
# GitHub serves the tag and the asset name.

cask "openorchestra" do
  version "VERSION_CSV"
  sha256 "SHA256"

  url "https://github.com/omarpr/openorchestra/releases/download/v#{version.csv.first}%2B#{version.csv.second}/openOrchestra-#{version.csv.first}%2B#{version.csv.second}-universal.dmg",
      verified: "github.com/omarpr/openorchestra/"
  name "openOrchestra"
  desc "Native workspace for running coding agents in parallel git worktrees"
  homepage "https://github.com/omarpr/openorchestra"

  livecheck do
    url :url
    regex(/^v?(\d+(?:\.\d+)+)\+(\d+)$/i)
    strategy :github_latest do |json, regex|
      match = json["tag_name"]&.match(regex)
      next if match.blank?

      "#{match[1]},#{match[2]}"
    end
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
