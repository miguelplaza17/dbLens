#!/bin/zsh
# Lists UI strings with no translation, and translations no longer used.
# The compiler extracts the exact keys the app looks up (-emit-localized-strings).
# Usage: tools/check-strings.sh [language]      (default pt-BR)
set -euo pipefail
here=${0:A:h:h}
lang=${1:-pt-BR}
out=$(mktemp -d)
swift build -c release --package-path "$here" --scratch-path "$HOME/Library/Developer/dbLens/locbuild" \
  -Xswiftc -emit-localized-strings -Xswiftc -emit-localized-strings-path -Xswiftc "$out" >/dev/null
python3 - "$out" "$here/Localization/$lang.json" <<'PY'
import glob, json, os, sys
keys = {}
for path in glob.glob(sys.argv[1] + "/*.stringsdata"):
    for entries in json.load(open(path)).get("tables", {}).values():
        for entry in entries:
            keys[entry["key"]] = os.path.basename(path).removesuffix(".stringsdata")
table = json.load(open(sys.argv[2]))
# Words that read the same in every language need no entry.
same = {"dbLens", "SQL", "SSL", "URL", "NULL", "Host", "Log", "postgres", "22", "5432"}
missing = sorted(k for k in keys if k not in table and k not in same)
unused = sorted(k for k in table if k not in keys)
for k in missing: print(f"missing  {keys[k]}: {k!r}")
for k in unused: print(f"unused   {k!r}")
print(f"{len(keys)} keys, {len(missing)} missing, {len(unused)} unused")
sys.exit(1 if missing or unused else 0)
PY
rm -rf "$out"
