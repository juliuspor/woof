#!/bin/sh
set -eu

usage() {
  echo "usage: scripts/verify-code-identities.sh DAEMON MCP [SECOND_DAEMON SECOND_MCP]" >&2
}

case "$#" in
  2 | 4) ;;
  *)
    usage
    exit 64
    ;;
esac

umask 077
work_dir=$(/usr/bin/mktemp -d "/private/tmp/woof-code-identities.XXXXXX")
case "$work_dir" in
  /private/tmp/woof-code-identities.*) ;;
  *)
    echo "Refusing unexpected code-identity work directory: $work_dir" >&2
    exit 1
    ;;
esac
cleanup() {
  /bin/rm -rf -- "$work_dir"
}
trap cleanup EXIT HUP INT TERM

verify_identity() {
  binary=$1
  expected_identifier=$2
  expected_name=$3
  sequence=$4

  [ -f "$binary" ] && [ ! -L "$binary" ] || {
    echo "Code-identity input must be a regular, non-symbolic-link file: $binary" >&2
    exit 1
  }
  description=$(/usr/bin/file -b "$binary")
  case "$description" in
    "Mach-O 64-bit executable arm64") ;;
    *)
      echo "Code-identity input is not a thin arm64 executable: $binary ($description)" >&2
      exit 1
      ;;
  esac

  metadata_output="$work_dir/metadata-$sequence"
  metadata_plist="$work_dir/Info-$sequence.plist"
  /usr/bin/otool -P "$binary" >"$metadata_output"
  if [ "$(/usr/bin/sed -n '2p' "$metadata_output")" != \
    '(__TEXT,__info_plist) section' ]; then
    echo "Executable has no embedded __TEXT,__info_plist metadata: $binary" >&2
    exit 1
  fi
  /usr/bin/sed -n '3,$p' "$metadata_output" >"$metadata_plist"
  [ -s "$metadata_plist" ] || {
    echo "Executable has no embedded __TEXT,__info_plist metadata: $binary" >&2
    exit 1
  }
  /usr/bin/plutil -lint "$metadata_plist" >/dev/null
  observed_identifier=$(
    /usr/bin/plutil -extract CFBundleIdentifier raw -o - "$metadata_plist"
  )
  observed_name=$(/usr/bin/plutil -extract CFBundleName raw -o - "$metadata_plist")
  observed_version=$(
    /usr/bin/plutil -extract CFBundleVersion raw -o - "$metadata_plist"
  )
  if [ "$observed_identifier" != "$expected_identifier" ]; then
    echo "Unexpected embedded code identifier: $observed_identifier" >&2
    exit 1
  fi
  if [ "$observed_name" != "$expected_name" ]; then
    echo "Unexpected embedded code name: $observed_name" >&2
    exit 1
  fi
  if [ "$observed_version" != 0.1.0 ]; then
    echo "Unexpected embedded helper version: $observed_version" >&2
    exit 1
  fi

  signed_copy="$work_dir/signed-$sequence"
  /bin/cp "$binary" "$signed_copy"
  /bin/chmod 0700 "$signed_copy"
  # This temporary ad-hoc signature deliberately omits --identifier. codesign
  # must derive the stable identity from the embedded metadata, as the
  # production Developer ID signing step does.
  /usr/bin/codesign --force --options runtime --timestamp=none \
    --sign - "$signed_copy" >/dev/null
  /usr/bin/codesign --verify --strict --verbose=2 "$signed_copy"
  signature_details=$(/usr/bin/codesign -dv --verbose=4 "$signed_copy" 2>&1)
  if ! printf '%s\n' "$signature_details" |
    /usr/bin/grep -Fxq "Identifier=$expected_identifier"; then
    echo "Ad-hoc signing did not preserve the stable code identifier." >&2
    exit 1
  fi
  if ! printf '%s\n' "$signature_details" |
    /usr/bin/grep -Eq '^Info[.]plist entries=[1-9][0-9]*$'; then
    echo "Ad-hoc signing did not bind the embedded Info.plist metadata." >&2
    exit 1
  fi
  if ! printf '%s\n' "$signature_details" |
    /usr/bin/grep -Fq 'flags=0x10002(adhoc,runtime)'; then
    echo "Ad-hoc identity probe did not enable the hardened runtime." >&2
    exit 1
  fi
}

verify_identity "$1" com.julius.woof.daemon "woof daemon" 1
verify_identity "$2" com.julius.woof.mcp "woof mcp" 2
if [ "$#" -eq 4 ]; then
  verify_identity "$3" com.julius.woof.daemon "woof daemon" 3
  verify_identity "$4" com.julius.woof.mcp "woof mcp" 4
fi

echo "woof helper code identities are stable and bound."
