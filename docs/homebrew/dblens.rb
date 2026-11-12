# Homebrew cask for a tap repository (github.com/miguelplaza17/homebrew-tap, Casks/dblens.rb).
# After each release: set version and sha256 (tools/release.sh prints it). The app is
# the Tauri build (universal), which updates itself.
#   brew install --cask miguelplaza17/tap/dblens
cask "dblens" do
  version "1.4.0"
  sha256 "b55d5b4a131e95b7e5fbbaddfb91f1a3dcc7d1d8a7d6aed5c65eeadd8ca115d2"

  url "https://github.com/miguelplaza17/dbLens/releases/download/v#{version}/dbLens-#{version}.dmg"
  name "dbLens"
  desc "Fast, minimal database client for Postgres, MySQL, SQLite, Redis and MongoDB"
  homepage "https://github.com/miguelplaza17/dbLens"

  depends_on macos: ">= :ventura"

  auto_updates true

  app "dbLens.app"

  zap trash: [
    "~/Library/Application Support/app.dblens.dbLens",
    "~/Library/Application Support/dbLens",
    "~/Library/Caches/io.github.miguelplaza17.dblens",
    "~/Library/WebKit/io.github.miguelplaza17.dblens",
    "~/Library/Preferences/io.github.miguelplaza17.dblens.plist",
  ]
end
