# Homebrew cask for a tap repository (github.com/miguelplaza17/homebrew-tap, Casks/dblens.rb).
# After each release: set version and sha256 (tools/release.sh prints it).
#   brew install --cask miguelplaza17/tap/dblens
cask "dblens" do
  version "1.0.5"
  sha256 "f72dc863ae04135048a0abe2a24c81be0fffc8e6c27ee62d6f4cebfa903dd48c"

  url "https://github.com/miguelplaza17/dbLens/releases/download/v#{version}/dbLens-#{version}.dmg"
  name "dbLens"
  desc "Fast, minimal database client for Postgres, MySQL, SQLite, Redis and MongoDB"
  homepage "https://github.com/miguelplaza17/dbLens"

  depends_on macos: ">= :tahoe"
  depends_on arch: :arm64

  app "dbLens.app"

  zap trash: [
    "~/Library/Application Support/dbLens",
    "~/Library/Preferences/io.github.miguelplaza17.dblens.plist",
  ]
end
