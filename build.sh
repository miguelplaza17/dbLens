#!/bin/zsh
# Builds dbLens and installs it to ~/Applications/dbLens.app.
# Build products live in ~/Library/Developer/dbLens, outside the repo
# (and outside Dropbox or iCloud, which would try to sync them).
#
# Usage: ./build.sh            release build, install
#        ./build.sh --debug    debug build, install
#        ./build.sh --open     install, then quit the running copy and open the new one
set -euo pipefail
here=${0:A:h}
name="dbLens"
scratch="$HOME/Library/Developer/$name/build"
config=release
[[ " $* " == *" --debug "* ]] && config=debug

# Signing: SIGN_IDENTITY, else a Developer ID Application certificate (the one
# releases need), else any Apple certificate in the keychain, else ad hoc.
# A stable certificate keeps permissions across rebuilds; ad hoc is fine for your own Mac.
identity="${SIGN_IDENTITY:-}"
if [[ -z "$identity" ]]; then
  certs=$(security find-identity -v -p codesigning 2>/dev/null || true)
  identity=$(print -r -- "$certs" | grep -m1 -oE '"Developer ID Application: [^"]*"' | tr -d '"' || true)
  [[ -n "$identity" ]] || identity=$(print -r -- "$certs" | grep -m1 -oE '"(Apple Development|Apple Distribution|iPhone Developer|iPhone Distribution): [^"]*"' | tr -d '"' || true)
fi
[[ -n "$identity" ]] || identity=-
# HARDENED=1 (tools/release.sh) adds the hardened runtime and a secure timestamp, which notarization requires.
sign_flags=(--force --sign "$identity")
[[ "${HARDENED:-}" == 1 ]] && sign_flags+=(--options runtime --timestamp)

output=$(swift build -c $config --package-path "$here" --scratch-path "$scratch" 2>&1 || true)
print -r -- "$output" | grep -E "error|warning|Build complete" | grep -v "^\[" || true
bin="$scratch/$config/$name"
# Judge by the compiler's own errors: a change that only touches comments leaves
# the binary as it was, so its date says nothing.
if print -r -- "$output" | grep -q "error:" || [[ ! -x "$bin" ]]; then
  echo "build failed"
  exit 1
fi

# APP_DIR puts the app somewhere else (tools/release.sh uses dist/).
app_dir="${APP_DIR:-$HOME/Applications}"
app="$app_dir/$name.app"
mkdir -p "$app_dir"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/$name"
sed -e "s/__BUILD__/$(date +%Y%m%d%H%M)/" -e "s/__VERSION__/$(cat "$here/VERSION")/" "$here/Info.plist.in" > "$app/Contents/Info.plist"
# libpq and its libraries go inside the app, so it runs without Homebrew.
"$here/tools/bundle_libs.sh" "$app" >/dev/null
# Logos for Settings > Agents.
ditto "$here/Resources/AgentLogos" "$app/Contents/Resources/AgentLogos"
# Sparkle, for updates. The app isn't sandboxed, so its XPC services aren't needed.
sparkle="$app/Contents/Frameworks/Sparkle.framework"
ditto "$scratch/$config/Sparkle.framework" "$sparkle"
rm -rf "$sparkle/Versions/B/XPCServices" "$sparkle/XPCServices"
# Translations: Localization/*.json -> Resources/<lang>.lproj/Localizable.strings.
"$here/tools/strings.sh" "$app/Contents/Resources"
# The icon: Icon/AppIcon.icon -> Resources/Assets.car + AppIcon.icns, rebuilt when it changed.
if [[ -d "$here/Icon/AppIcon.icon" ]]; then
  car="$here/Resources/Assets.car"
  if [[ ! -f "$car" || -n "$(find "$here/Icon/AppIcon.icon" -newer "$car" | head -1)" ]]; then
    "$here/tools/make_icon.sh"
  fi
  cp -X "$car" "$here/Resources/AppIcon.icns" "$app/Contents/Resources/"
fi
xattr -cr "$app"
# Inside out: the bundled libraries first, then the app.
for lib in "$app/Contents/Frameworks"/*.dylib(N) "$app/Contents/Frameworks"/mariadb-plugins/*.so(N); do
  codesign "${sign_flags[@]}" "$lib" 2>&1 | grep -v "replacing existing signature" || true
done
# Sparkle's helpers before the framework, the framework before the app; never --deep.
for part in "$sparkle/Versions/B/Autoupdate" "$sparkle/Versions/B/Updater.app" "$sparkle"; do
  codesign "${sign_flags[@]}" "$part" 2>&1 | grep -v "replacing existing signature" || true
done
codesign "${sign_flags[@]}" "$app" 2>&1 | grep -v "replacing existing signature" || true
echo "installed $app (signed: ${identity})"

if [[ " $* " == *" --open "* ]]; then
  osascript -e "quit app \"$name\"" 2>/dev/null || true
  sleep 0.5
  open "$app"
fi
