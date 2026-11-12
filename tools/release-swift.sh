#!/bin/zsh
# Builds a release: Developer ID signature with the hardened runtime, Apple
# notarization, stapled tickets, dist/dbLens-<version>.dmg (the app and an
# Applications link to drag it to) for GitHub Releases and the Homebrew cask,
# and dist/appcast.xml, signed with the update key, for Sparkle.
#
# Once per Mac:
#   1. Xcode > Settings > Accounts > Manage Certificates > + > Developer ID Application
#   2. xcrun notarytool store-credentials dblens-notary --apple-id <you@example.com> --team-id <TEAMID>
#      (asks for an app-specific password from account.apple.com > Sign-In and Security)
#
# Usage: tools/release.sh               build, sign, notarize, staple, make the DMG
#        NOTARIZE=0 tools/release.sh    sign and make the DMG only (to try the signed build)
set -euo pipefail
here=${0:A:h:h}
name=dbLens
version=$(cat "$here/VERSION")
dist="$here/dist"
profile=${NOTARY_PROFILE:-dblens-notary}

identity=${SIGN_IDENTITY:-$(security find-identity -v -p codesigning | grep -m1 -oE '"Developer ID Application: [^"]*"' | tr -d '"' || true)}
[[ -n $identity ]] || { echo "No Developer ID Application certificate. See the steps at the top of this script."; exit 1; }

rm -rf "$dist"
mkdir -p "$dist"
SIGN_IDENTITY="$identity" HARDENED=1 APP_DIR="$dist" "$here/build.sh"
app="$dist/$name.app"

codesign --verify --deep --strict --verbose=1 "$app"
otool -L "$app/Contents/MacOS/$name" | grep -q /opt/homebrew && { echo "still links Homebrew"; exit 1; }

# The app is notarized first (through a zip), so it is stapled inside the DMG too.
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
# The appcast Sparkle reads (SUFeedURL points at the latest release's copy).
# Upload it with the DMG to the same release.
build=$(/usr/libexec/PlistBuddy -c "Print :CFBundleVersion" "$app/Contents/Info.plist")
minimum=$(/usr/libexec/PlistBuddy -c "Print :LSMinimumSystemVersion" "$app/Contents/Info.plist")
signature=$("$here/tools/sign_update.sh" "$dmg")
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
      <enclosure url="https://github.com/miguelplaza17/dbLens/releases/download/v$version/$name-$version.dmg"
                 length="$length" type="application/octet-stream" sparkle:edSignature="$signature"/>
    </item>
  </channel>
</rss>
XML
shasum -a 256 "$dmg"
echo "release ready: $dmg and $dist/appcast.xml (upload both to the v$version release)"
