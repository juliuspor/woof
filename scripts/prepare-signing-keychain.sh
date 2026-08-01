#!/bin/sh
set -eu

# Development only. Production signing uses an Apple-issued identity from the
# user's normal Keychain search list.

identity="woof local development signing"
keychain_path="$HOME/Library/Keychains/woof-local-development-signing.keychain-db"
work_dir=$(/usr/bin/mktemp -d "/private/tmp/woof-signing-prepare.XXXXXX")
trap '/bin/rm -rf "$work_dir"' EXIT HUP INT TERM
script_dir=$(CDPATH= cd -- "$(/usr/bin/dirname -- "$0")" && /bin/pwd)

. "$script_dir/lib/signing-keychain.sh"

if [ ! -f "$keychain_path" ]; then
  echo "woof development signing keychain is missing; run scripts/create-local-signing-certificate.sh" >&2
  exit 1
fi

/bin/chmod 0600 "$keychain_path"
/usr/bin/security unlock-keychain -p "" "$keychain_path"
/usr/bin/security set-keychain-settings -lut 21600 "$keychain_path"
woof_append_signing_keychain "$keychain_path" "$work_dir/search-list"
/usr/bin/security find-certificate -c "$identity" "$keychain_path" >/dev/null
woof_probe_signing_identity "$identity" "$work_dir"

echo "Prepared isolated woof development signing keychain."
