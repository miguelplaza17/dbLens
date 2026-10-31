#!/bin/zsh
# Turns each Localization/<lang>.json (key -> translation) into
# <resources>/<lang>.lproj/Localizable.strings. English is the source language:
# its keys are the strings in the code, so en.lproj only marks it as supported.
# Usage: tools/strings.sh path/to/App.app/Contents/Resources
set -euo pipefail
here=${0:A:h:h}
resources=${1:?usage: strings.sh path/to/Resources}
mkdir -p "$resources/en.lproj"
: > "$resources/en.lproj/Localizable.strings"
for table in "$here"/Localization/*.json(N); do
  lang=${table:t:r}
  mkdir -p "$resources/$lang.lproj"
  plutil -convert binary1 -o "$resources/$lang.lproj/Localizable.strings" "$table"
done
