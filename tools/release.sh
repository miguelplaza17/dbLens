#!/bin/zsh
# Builds the Mac release of dbLens (the Tauri app in desktop/): a universal app
# (Apple silicon and Intel), Developer ID signature with the hardened runtime,
# Apple notarization, stapled tickets, and in dist/:
#   dbLens-<version>.dmg            for GitHub Releases and the Homebrew cask
#   dbLens.app.tar.gz (+ .sig)      what the Tauri updater installs
#   latest.json                     the Tauri update feed (merged with the
#                                   Windows and Linux entries CI published)
#   appcast.xml                     Sparkle feed: moves people still on the old
#                                   Swift app (1.4 and earlier) to this one
#
# Once per Mac:
#   1. Xcode > Settings > Accounts > Manage Certificates > + > Developer ID Application
#   2. xcrun notarytool store-credentials dblens-notary --apple-id <you@example.com> --team-id <TEAMID>
#   3. The updater key in ~/Library/Developer/dbLens/tauri-updater-key/dblens.key
#      (`npx tauri signer generate`; the public half is in tauri.conf.json). Back it up:
#      losing it means installed apps can't take updates anymore.
#
# Usage: tools/release.sh               build, sign, notarize, staple, make the files
#        NOTARIZE=0 tools/release.sh    sign only (to try the signed build)
#        PUBLISH=1 tools/release.sh     also upload everything to the v<version> release
set -euo pipefail
here=${0:A:h:h}
name=dbLens
version=$(cat "$here/VERSION")
dist="$here/dist"
profile=${NOTARY_PROFILE:-dblens-notary}
updater_key=${TAURI_UPDATER_KEY:-$HOME/Library/Developer/dbLens/tauri-updater-key/dblens.key}
target=${CARGO_TARGET_DIR:-"/Volumes/Mac External/Docker/builds/dblens/target"}
repo=miguelplaza17/dbLens
base="https://github.com/$repo/releases/download/v$version"

conf=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$here/desktop/src-tauri/tauri.conf.json")
[[ $conf == $version ]] || { echo "VERSION is $version but tauri.conf.json says $conf: bump both (CLAUDE.md)."; exit 1; }
[[ -r $updater_key ]] || { echo "No updater key at $updater_key. See the steps at the top of this script."; exit 1; }
identity=${SIGN_IDENTITY:-$(security find-identity -v -p codesigning | grep -m1 -oE '"Developer ID Application: [^"]*"' | tr -d '"' || true)}
[[ -n $identity ]] || { echo "No Developer ID Application certificate. See the steps at the top of this script."; exit 1; }
[[ -d ${target:h} ]] || { echo "The build folder ${target:h} is missing (external disk not mounted?)."; exit 1; }

rm -rf "$dist"
mkdir -p "$dist"

# Same build numbers as the Swift app (yyyyMMddHHmm), so Sparkle sees this as newer.
build=$(date +%Y%m%d%H%M)
(
  cd "$here/desktop"
  npm ci --silent
  CARGO_TARGET_DIR="$target" APPLE_SIGNING_IDENTITY="$identity" \
    npm run tauri build -- --target universal-apple-darwin --bundles app \
      --config "{\"bundle\":{\"macOS\":{\"bundleVersion\":\"$build\"}}}"
)
ditto "$target/universal-apple-darwin/release/bundle/macos/$name.app" "$dist/$name.app"
app="$dist/$name.app"

# The Liquid Glass icon (desktop/src-tauri/icons/mac/AppIcon.icon, compiled to
# Assets.car and AppIcon.icns next to it; see desktop/README.md), then the
# signature again over it.
icons="$here/desktop/src-tauri/icons/mac"
cp "$icons/Assets.car" "$app/Contents/Resources/Assets.car"
cp "$icons/AppIcon.icns" "$app/Contents/Resources/icon.icns"
/usr/libexec/PlistBuddy -c "Delete :CFBundleIconName" "$app/Contents/Info.plist" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Add :CFBundleIconName string AppIcon" "$app/Contents/Info.plist"
codesign --force --options runtime --timestamp --sign "$identity" "$app"

codesign --verify --deep --strict --verbose=1 "$app"
for arch in arm64 x86_64; do lipo "$app/Contents/MacOS/dblens" -verify_arch $arch; done
otool -L "$app/Contents/MacOS/"* | grep -q /opt/homebrew && { echo "still links Homebrew"; exit 1; }

# The app is notarized first (through a zip), so it is stapled inside the DMG and the update too.
if [[ "${NOTARIZE:-1}" == 1 ]]; then
  zip="$dist/$name-$version.zip"
  ditto -c -k --keepParent "$app" "$zip"
  xcrun notarytool submit "$zip" --keychain-profile "$profile" --wait
  xcrun stapler staple "$app"
  rm "$zip"
  spctl --assess --type execute --verbose=2 "$app"
fi

dmg="$dist/$name-$version.dmg"
stage="$dist/dmg"
mkdir -p "$stage"
ditto "$app" "$stage/$name.app"
ln -s /Applications "$stage/Applications"
hdiutil create -volname "$name $version" -srcfolder "$stage" -fs APFS -format ULFO -ov "$dmg" >/dev/null
rm -rf "$stage"
codesign --sign "$identity" --timestamp "$dmg"
if [[ "${NOTARIZE:-1}" == 1 ]]; then
  xcrun notarytool submit "$dmg" --keychain-profile "$profile" --wait
  xcrun stapler staple "$dmg"
  spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"
fi

# The Tauri update: the stapled app as a tarball, signed with the updater key.
tarball="$dist/$name.app.tar.gz"
COPYFILE_DISABLE=1 tar -czf "$tarball" -C "$dist" "$name.app"
(
  cd "$here/desktop"
  TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" npx tauri signer sign -f "$updater_key" -p "" "$tarball" >/dev/null
)
signature=$(cat "$tarball.sig")

# latest.json: keep what CI published for Windows and Linux, add the Mac.
existing="$dist/latest.ci.json"
gh release download "v$version" --repo "$repo" --pattern latest.json --output "$existing" 2>/dev/null || echo '{}' > "$existing"
python3 - "$existing" "$dist/latest.json" "$version" "$base/$name.app.tar.gz" "$signature" <<'PY'
import json, sys, datetime
src, out, version, url, sig = sys.argv[1:]
feed = json.load(open(src)) or {}
feed["version"] = version
feed.setdefault("notes", f"dbLens {version}")
feed["pub_date"] = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
platforms = feed.setdefault("platforms", {})
for arch in ("darwin-aarch64", "darwin-x86_64", "darwin-aarch64-app", "darwin-x86_64-app"):
    platforms[arch] = {"signature": sig, "url": url}
json.dump(feed, open(out, "w"), indent=2)
PY
rm -f "$existing"

# Sparkle, for the old Swift app: offers this DMG; same bundle id and Developer ID,
# so it installs over the Swift app, and the first launch brings its connections.
minimum=$(/usr/libexec/PlistBuddy -c "Print :LSMinimumSystemVersion" "$app/Contents/Info.plist")
edsig=$("$here/tools/sign_update.sh" "$dmg")
length=$(stat -f %z "$dmg")
cat > "$dist/appcast.xml" <<XML
<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>dbLens</title>
    <item>
      <title>dbLens $version</title>
      <pubDate>$(LC_ALL=C date -u "+%a, %d %b %Y %H:%M:%S +0000")</pubDate>
      <sparkle:version>$build</sparkle:version>
      <sparkle:shortVersionString>$version</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>$minimum</sparkle:minimumSystemVersion>
      <enclosure url="$base/$name-$version.dmg"
                 length="$length" type="application/octet-stream" sparkle:edSignature="$edsig"/>
    </item>
  </channel>
</rss>
XML

shasum -a 256 "$dmg"
files=("$dmg" "$tarball" "$tarball.sig" "$dist/latest.json" "$dist/appcast.xml")
if [[ "${PUBLISH:-0}" == 1 ]]; then
  gh release view "v$version" --repo "$repo" >/dev/null 2>&1 || gh release create "v$version" --repo "$repo" --title "dbLens $version" --notes "dbLens $version"
  gh release upload "v$version" --repo "$repo" --clobber "${files[@]}"
  echo "uploaded to v$version"
else
  echo "release ready in $dist; upload with: PUBLISH=1 tools/release.sh, or gh release upload v$version ${files[@]:t}"
fi
