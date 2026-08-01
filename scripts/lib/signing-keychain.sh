#!/bin/sh

# Append woof's development-only signing keychain without discarding custom keychains
# already present in the user's search list.
woof_append_signing_keychain() {
  woof_signing_keychain=$1
  woof_search_list_file=$2

  /usr/bin/security list-keychains -d user |
    /usr/bin/sed \
      -e 's/^[[:space:]]*"//' \
      -e 's/"[[:space:]]*$//' >"$woof_search_list_file"

  set --
  woof_keychain_present=0
  while IFS= read -r woof_existing_keychain; do
    [ -n "$woof_existing_keychain" ] || continue
    if [ "$woof_existing_keychain" = "$woof_signing_keychain" ]; then
      woof_keychain_present=1
    fi
    set -- "$@" "$woof_existing_keychain"
  done <"$woof_search_list_file"

  if [ "$woof_keychain_present" -eq 0 ]; then
    set -- "$@" "$woof_signing_keychain"
  fi

  /usr/bin/security list-keychains -d user -s "$@"
}

# Prove that the certificate has a usable private key and that its ACL permits
# Apple's signing tool before creating a local development signature.
woof_probe_signing_identity() {
  woof_signing_identity=$1
  woof_probe_directory=$2
  woof_probe_path="$woof_probe_directory/signing-probe"

  /bin/cp /usr/bin/true "$woof_probe_path"
  /usr/bin/codesign --force --options runtime --timestamp=none \
    --sign "$woof_signing_identity" "$woof_probe_path"
}
