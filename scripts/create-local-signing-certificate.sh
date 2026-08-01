#!/bin/sh
set -eu

# Development only. The production pipeline deliberately cannot use this
# self-issued certificate.

identity="woof local development signing"
keychain_path="$HOME/Library/Keychains/woof-local-development-signing.keychain-db"
work_dir=$(/usr/bin/mktemp -d "/private/tmp/woof-signing.XXXXXX")
archive_password=$(/usr/bin/uuidgen)
trap '/bin/rm -rf "$work_dir"' EXIT HUP INT TERM
script_dir=$(CDPATH= cd -- "$(/usr/bin/dirname -- "$0")" && /bin/pwd)

. "$script_dir/lib/signing-keychain.sh"

if [ ! -f "$keychain_path" ]; then
  # This owner-private keychain contains only a local development identity.
  # The imported key ACL is scoped to Apple signing/security tooling below.
  /usr/bin/security create-keychain -p "" "$keychain_path"
fi
/bin/chmod 0600 "$keychain_path"

/usr/bin/security unlock-keychain -p "" "$keychain_path"
/usr/bin/security set-keychain-settings -lut 21600 "$keychain_path"
woof_append_signing_keychain "$keychain_path" "$work_dir/search-list"

if /usr/bin/security find-certificate -c "$identity" "$keychain_path" >/dev/null 2>&1; then
  woof_probe_signing_identity "$identity" "$work_dir"
  echo "Development signing identity is ready: $identity"
  exit 0
fi

/usr/bin/openssl req -new -newkey rsa:3072 -x509 -days 3650 -nodes \
  -subj "/CN=$identity/O=woof local development/" \
  -addext "keyUsage=digitalSignature" \
  -addext "extendedKeyUsage=codeSigning" \
  -keyout "$work_dir/key.pem" \
  -out "$work_dir/cert.pem"

/usr/bin/openssl pkcs12 -export \
  -inkey "$work_dir/key.pem" \
  -in "$work_dir/cert.pem" \
  -passout "pass:$archive_password" \
  -out "$work_dir/identity.p12"

/usr/bin/security import "$work_dir/identity.p12" \
  -k "$keychain_path" \
  -P "$archive_password" \
  -T /usr/bin/codesign \
  -T /usr/bin/security
/usr/bin/security set-key-partition-list \
  -S "apple-tool:,apple:" -s -k "" "$keychain_path" >/dev/null

woof_probe_signing_identity "$identity" "$work_dir"

echo "Created development signing identity: $identity"
