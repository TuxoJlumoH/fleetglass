cask "fleetglass" do
  arch arm: "aarch64", intel: "x64"

  version "@VERSION@"
  sha256 arm:   "@SHA256_ARM64@",
         intel: "@SHA256_INTEL@"

  url "https://github.com/TuxoJlumoH/fleetglass/releases/download/v#{version}/Fleetglass_#{version}_#{arch}.dmg"
  name "Fleetglass"
  desc "Fast, keyboard-first, multi-cluster Kubernetes desktop client"
  homepage "https://github.com/TuxoJlumoH/fleetglass"

  livecheck do
    url :url
    strategy :github_latest
  end

  auto_updates false
  depends_on macos: ">= :catalina"

  app "Fleetglass.app"

  zap trash: [
    "~/Library/Application Support/dev.fleetglass.Fleetglass",
    "~/Library/Caches/dev.fleetglass.Fleetglass",
    "~/Library/Preferences/dev.fleetglass.Fleetglass.plist",
    "~/Library/Saved Application State/dev.fleetglass.Fleetglass.savedState",
  ]
end
