#!/bin/sh
set -eu

# Development only. Production artifacts are created exclusively by
# scripts/build-release.sh after Apple notarization.

if [ "$#" -ne 1 ]; then
  echo "usage: scripts/sign-app.sh /absolute/path/to/woof.app" >&2
  echo "development-only: this helper never creates a production release" >&2
  exit 64
fi

app_path=$1
identity="woof local development signing"

case "$app_path" in
  */target/*/release/* | */artifacts/* | /Applications/*)
    echo "refusing to development-sign a release, artifact, or installed application" >&2
    exit 64
    ;;
  /*/woof.app) ;;
  *)
    echo "refusing to sign unexpected target: $app_path" >&2
    exit 64
    ;;
esac

test -d "$app_path"
[ ! -L "$app_path" ] || {
  echo "refusing to development-sign a symbolic link" >&2
  exit 64
}
app_path=$(/bin/realpath "$app_path")
case "$app_path" in
  */target/*/release/* | */artifacts/* | /Applications/*)
    echo "refusing to development-sign a release, artifact, or installed application" >&2
    exit 64
    ;;
  /*/woof.app) ;;
  *)
    echo "refusing to sign unexpected canonical target: $app_path" >&2
    exit 64
    ;;
esac
echo "Development-only signing; this output is not eligible for distribution." >&2
"$(/usr/bin/dirname "$0")/prepare-signing-keychain.sh"
/usr/bin/codesign --force --deep --options runtime --timestamp=none \
  --sign "$identity" "$app_path"
if ! /usr/bin/codesign --verify --deep --strict --verbose=2 "$app_path"; then
  echo "woof was development-signed, but macOS strict verification did not accept the certificate chain." >&2
  echo "A new self-signed identity is intentionally not trusted automatically." >&2
  echo "Grant trust only as an explicit, administrator-approved local-machine action." >&2
  exit 1
fi
