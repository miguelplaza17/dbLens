#!/bin/zsh
# Prints Sparkle's EdDSA signature (base64 Ed25519) of a file, with the update key
# kept in the login keychain (item "dblens-sparkle-ed25519"). Same output as
# Sparkle's sign_update, made with openssl so no extra tool is needed.
# The public half is SUPublicEDKey in Info.plist.in. Losing the key means users
# can't receive updates anymore: back it up (Keychain Access, search "dbLens Sparkle").
set -euo pipefail
file=$1
key=$(mktemp)
trap 'rm -f "$key"' EXIT
security find-generic-password -s dblens-sparkle-ed25519 -w | base64 -d > "$key"
openssl pkeyutl -sign -inkey "$key" -rawin -in "$file" | base64
