#!/bin/zsh
# Copies libpq and everything it links from Homebrew (OpenSSL, Kerberos, gettext)
# into <App>.app/Contents/Frameworks and points the binary and each library at
# the copies through @rpath, so the app runs on Macs without Homebrew.
# Run before signing: install_name_tool breaks existing signatures.
# Usage: tools/bundle_libs.sh path/to/dbLens.app
set -euo pipefail
app=${1:?usage: bundle_libs.sh path/to/App.app}
name=${app:t:r}
bin="$app/Contents/MacOS/$name"
frameworks="$app/Contents/Frameworks"
mkdir -p "$frameworks"

# Homebrew paths a Mach-O file links against.
homebrew_deps() { otool -L "$1" | tail -n +2 | awk '{print $1}' | grep -E '^(/opt/homebrew|/usr/local)/' || true; }

typeset -A copied
queue=($(homebrew_deps "$bin"))
while (( ${#queue} )); do
  dep=$queue[1]; queue=(${queue[2,-1]})
  base=${dep:t}
  [[ -n ${copied[$base]:-} ]] && continue
  copied[$base]=1
  cp -f "$(realpath "$dep")" "$frameworks/$base"
  chmod u+w "$frameworks/$base"
  install_name_tool -id "@rpath/$base" "$frameworks/$base" 2>/dev/null
  queue+=($(homebrew_deps "$frameworks/$base"))
done

# MariaDB Connector/C loads login methods (MySQL 8's caching_sha2_password,
# ed25519, ...) as plugins at run time; the app points it at this folder.
plugin_src=/opt/homebrew/opt/mariadb-connector-c/lib/mariadb/plugin
if otool -L "$bin" | grep -q libmariadb && [[ -d $plugin_src ]]; then
  mkdir -p "$frameworks/mariadb-plugins"
  for plugin in caching_sha2_password sha256_password client_ed25519 mysql_clear_password dialog parsec; do
    [[ -f "$plugin_src/$plugin.so" ]] || continue
    cp -f "$plugin_src/$plugin.so" "$frameworks/mariadb-plugins/"
    chmod u+w "$frameworks/mariadb-plugins/$plugin.so"
    for dep in $(homebrew_deps "$frameworks/mariadb-plugins/$plugin.so"); do
      base=${dep:t}
      if [[ -z ${copied[$base]:-} ]]; then
        copied[$base]=1
        cp -f "$(realpath "$dep")" "$frameworks/$base"; chmod u+w "$frameworks/$base"
        install_name_tool -id "@rpath/$base" "$frameworks/$base" 2>/dev/null
      fi
    done
    otool -l "$frameworks/mariadb-plugins/$plugin.so" | grep -q '@loader_path/..' \
      || install_name_tool -add_rpath "@loader_path/.." "$frameworks/mariadb-plugins/$plugin.so" 2>/dev/null
  done
fi

# Point every reference, in the binary and in the copies, at @rpath.
for file in "$bin" "$frameworks"/*.dylib "$frameworks"/mariadb-plugins/*.so(N); do
  for dep in $(homebrew_deps "$file"); do
    install_name_tool -change "$dep" "@rpath/${dep:t}" "$file" 2>/dev/null
  done
done
otool -l "$bin" | grep -q '@executable_path/../Frameworks' \
  || install_name_tool -add_rpath "@executable_path/../Frameworks" "$bin" 2>/dev/null
# The copies find each other in the same folder.
for lib in "$frameworks"/*.dylib; do
  otool -l "$lib" | grep -q '@loader_path' || install_name_tool -add_rpath "@loader_path" "$lib" 2>/dev/null
done

left=$(for file in "$bin" "$frameworks"/*.dylib "$frameworks"/mariadb-plugins/*.so(N); do homebrew_deps "$file"; done)
if [[ -n $left ]]; then
  echo "bundle_libs: still linked to Homebrew:"; echo "$left"; exit 1
fi
echo "bundled ${#copied} libraries into ${frameworks:t}"
