# Homebrew cask for a tap repository (github.com/miguelplaza17/homebrew-tap, Casks/dblens.rb).
# After each release: set version and sha256 (tools/release.sh prints it).
#   brew install --cask miguelplaza17/tap/dblens
cask "dblens" do
  version "1.0.7"
  sha256 "06ddd892f688ba8820cb2f67bbbf3c94243f9eff6c902a20fa1910eda0387f93"

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
