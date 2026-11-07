# Homebrew cask for a tap repository (github.com/miguelplaza17/homebrew-tap, Casks/dblens.rb).
# After each release: set version and sha256 (tools/release.sh prints it).
#   brew install --cask miguelplaza17/tap/dblens
cask "dblens" do
  version "1.0.6"
  sha256 "7e5fee4800a5fdb2fb84ef1ba9e4ee22d0e74c2befc5c42097d12172c5925d55"

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
