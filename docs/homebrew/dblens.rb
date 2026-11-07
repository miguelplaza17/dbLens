# Homebrew cask for a tap repository (github.com/miguelplaza17/homebrew-tap, Casks/dblens.rb).
# After each release: set version and sha256 (tools/release.sh prints it).
#   brew install --cask miguelplaza17/tap/dblens
cask "dblens" do
  version "1.0.7"
  sha256 "95fcb74153e7bcb6ef1f9459431c7ae942a4ad58bcb1ff73b95a9218ba5ac6d2"

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
