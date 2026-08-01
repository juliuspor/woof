#!/usr/bin/env -S -i WOOF_RELEASE_CLEAN_ENV=1 /bin/sh
set -eu

if [ "${WOOF_RELEASE_CLEAN_ENV:-}" != 1 ]; then
  echo "Run scripts/build-release.sh directly so its clean-environment launcher is enforced." >&2
  exit 1
fi
if ! /usr/bin/env |
  while IFS='=' read -r environment_name _; do
    case "$environment_name" in
      WOOF_RELEASE_CLEAN_ENV|PWD|SHLVL|_) ;;
      *) exit 1 ;;
    esac
  done
then
  echo "Refusing a release process that bypassed the clean-environment launcher." >&2
  exit 1
fi
unset WOOF_RELEASE_CLEAN_ENV

export PATH=/usr/bin:/bin:/usr/sbin:/sbin
export LC_ALL=C

repo_dir=$(CDPATH= cd -- "$(/usr/bin/dirname -- "$0")/.." && /bin/pwd)
rust_toolchain="1.88.0"
target="aarch64-apple-darwin"
candidate="$repo_dir/target/$target/release/bundle/macos/woof.app"
release_dir="$repo_dir/artifacts/release"
identity_request=
notary_profile="woof-production"
prerequisites_only=0
verify_output_paths_only=0
original_argument_count=$#

usage() {
  echo "usage: scripts/build-release.sh [--signing-identity IDENTITY] [--notary-profile PROFILE] [--check-prerequisites] | scripts/build-release.sh --verify-output-paths" >&2
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --signing-identity)
      [ "$#" -ge 2 ] || {
        usage
        exit 64
      }
      identity_request=$2
      shift 2
      ;;
    --notary-profile)
      [ "$#" -ge 2 ] || {
        usage
        exit 64
      }
      notary_profile=$2
      shift 2
      ;;
    --check-prerequisites)
      prerequisites_only=1
      shift
      ;;
    --verify-output-paths)
      if [ "$original_argument_count" -ne 1 ]; then
        usage
        exit 64
      fi
      verify_output_paths_only=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      usage
      exit 64
      ;;
  esac
done

case "$identity_request" in
  *'
'*)
    echo "The signing identity must be a single line." >&2
    exit 64
    ;;
esac
if [ -n "$identity_request" ]; then
  identity_request_bytes=$(printf '%s' "$identity_request" | /usr/bin/wc -c | /usr/bin/tr -d ' ')
  if [ "$identity_request_bytes" -gt 512 ] ||
    printf '%s' "$identity_request" | /usr/bin/grep -q '[[:cntrl:]]'; then
    echo "The signing identity selector is not a valid certificate name or SHA-1 fingerprint." >&2
    exit 64
  fi
  if printf '%s\n' "$identity_request" |
    /usr/bin/grep -Eq '^[0-9A-Fa-f]{40}$'; then
    identity_request=$(
      printf '%s' "$identity_request" | /usr/bin/tr '[:lower:]' '[:upper:]'
    )
  fi
fi
case "$notary_profile" in
  [A-Za-z0-9]*) ;;
  *)
    echo "The notary profile must start with a letter or number." >&2
    exit 64
    ;;
esac
case "$notary_profile" in
  *[!A-Za-z0-9._-]*)
    echo "The notary profile may contain only letters, numbers, dots, underscores, or hyphens." >&2
    exit 64
    ;;
esac
notary_profile_bytes=$(printf '%s' "$notary_profile" | /usr/bin/wc -c | /usr/bin/tr -d ' ')
if [ "$notary_profile_bytes" -gt 128 ]; then
  echo "The notary profile name is too long." >&2
  exit 64
fi

assert_safe_repo_directory() {
  guarded_path=$1
  guarded_phase=$2
  case "$guarded_path" in
    "$repo_dir"/*) ;;
    *)
      echo "Refusing release path outside the repository during $guarded_phase: $guarded_path" >&2
      return 1
      ;;
  esac

  guarded_relative=${guarded_path#"$repo_dir"/}
  guarded_current=$repo_dir
  while [ -n "$guarded_relative" ]; do
    case "$guarded_relative" in
      */*)
        guarded_component=${guarded_relative%%/*}
        guarded_relative=${guarded_relative#*/}
        ;;
      *)
        guarded_component=$guarded_relative
        guarded_relative=
        ;;
    esac
    case "$guarded_component" in
      '' | . | ..)
        echo "Refusing malformed release path during $guarded_phase: $guarded_path" >&2
        return 1
        ;;
    esac
    guarded_current="$guarded_current/$guarded_component"
    if [ -L "$guarded_current" ]; then
      echo "Refusing unsafe release output path during $guarded_phase: symlink component $guarded_current" >&2
      return 1
    fi
    if [ -e "$guarded_current" ] && [ ! -d "$guarded_current" ]; then
      echo "Refusing unsafe release output path during $guarded_phase: non-directory component $guarded_current" >&2
      return 1
    fi
  done
}

assert_safe_release_output_trees() {
  guarded_phase=$1
  for guarded_tree in \
    "$repo_dir/assets/generated" \
    "$repo_dir/apps/woof/static/mascot" \
    "$repo_dir/apps/woof/src-tauri/binaries" \
    "$repo_dir/apps/woof/src-tauri/icons"
  do
    assert_safe_repo_directory "$guarded_tree" "$guarded_phase" || return 1
    if [ -d "$guarded_tree" ]; then
      if ! unsafe_output_entry=$(
        /usr/bin/find "$guarded_tree" -mindepth 1 \
          ! -type d ! -type f -print -quit
      ); then
        echo "Could not inspect release output paths during $guarded_phase." >&2
        return 1
      fi
      if [ -n "$unsafe_output_entry" ]; then
        echo "Refusing unsafe release output path during $guarded_phase: unsafe output entry $unsafe_output_entry" >&2
        return 1
      fi
      if ! multiply_linked_output=$(
        /usr/bin/find "$guarded_tree" -mindepth 1 \
          -type f ! -links 1 -print -quit
      ); then
        echo "Could not inspect release output links during $guarded_phase." >&2
        return 1
      fi
      if [ -n "$multiply_linked_output" ]; then
        echo "Refusing unsafe release output path during $guarded_phase: multiply-linked output file $multiply_linked_output" >&2
        return 1
      fi
    fi
  done
}

assert_release_output_paths() {
  guarded_phase=$1
  for guarded_path in \
    "$release_dir" \
    "$candidate" \
    "$repo_dir/target" \
    "$repo_dir/node_modules" \
    "$repo_dir/artifacts" \
    "$repo_dir/assets/generated" \
    "$repo_dir/assets/generated/woof-app.iconset" \
    "$repo_dir/apps/woof/node_modules" \
    "$repo_dir/apps/woof/.svelte-kit" \
    "$repo_dir/apps/woof/build" \
    "$repo_dir/apps/woof/static/mascot" \
    "$repo_dir/apps/woof/src-tauri/binaries" \
    "$repo_dir/apps/woof/src-tauri/icons" \
    "$repo_dir/apps/woof/src-tauri/target"
  do
    assert_safe_repo_directory "$guarded_path" "$guarded_phase" || return 1
  done
  assert_safe_release_output_trees "$guarded_phase" || return 1
  if [ -n "${work_dir:-}" ]; then
    assert_safe_repo_directory "$work_dir" "$guarded_phase" || return 1
  fi
}

require_safe_release_output_paths() {
  assert_release_output_paths "$1" || exit 1
}

umask 077
require_safe_release_output_paths "initial release path verification"
if [ "$verify_output_paths_only" -eq 1 ]; then
  echo "Release output paths are safe."
  exit 0
fi

[ -x /opt/homebrew/bin/node ] || {
  echo "The pinned Homebrew Node.js launcher is unavailable." >&2
  exit 1
}
account_home=$(/opt/homebrew/bin/node -p 'require("node:os").userInfo().homedir')
case "$account_home" in
  /*) ;;
  *)
    echo "Could not resolve the current account home." >&2
    exit 1
    ;;
esac
if [ ! -d "$account_home" ]; then
  echo "Could not resolve the current account home." >&2
  exit 1
fi
export HOME="$account_home"
export CARGO_HOME="$account_home/.cargo"
rust_flag_separator=$(printf '\037')
unset RUSTFLAGS
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$account_home=rust-build-home${rust_flag_separator}--remap-path-prefix=$repo_dir=woof-source"
repo_account_suffix=
case "$repo_dir" in
  "$account_home"/*)
    repo_account_suffix=${repo_dir#"$account_home"/}
    ;;
esac

if [ -x "$HOME/.cargo/bin/rustup" ]; then
  rustup_path="$HOME/.cargo/bin/rustup"
elif [ -x /opt/homebrew/bin/rustup ]; then
  rustup_path=/opt/homebrew/bin/rustup
else
  echo "Could not locate the pinned rustup launcher." >&2
  exit 1
fi
rustc_path=$("$rustup_path" which --toolchain "$rust_toolchain" rustc)
cargo_path=$("$rustup_path" which --toolchain "$rust_toolchain" cargo)
rust_toolchain_dir=$(/usr/bin/dirname "$cargo_path")
export PATH="$rust_toolchain_dir:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"
export RUSTC="$rustc_path"
export CARGO_BUILD_TARGET="$target"
export CARGO_TARGET_DIR="$repo_dir/target"
export CARGO_INCREMENTAL=0
export GIT_TERMINAL_PROMPT=0
export LC_ALL=C
umask 077

require_safe_release_output_paths "before release workspace creation"
mkdir -p "$release_dir"
require_safe_release_output_paths "before release workspace allocation"
work_dir=$(mktemp -d "$release_dir/.woof-release.XXXXXX")
case "$work_dir" in
  "$release_dir"/.woof-release.*) ;;
  *)
    echo "Refusing unexpected release work directory: $work_dir" >&2
    exit 1
    ;;
esac
require_safe_release_output_paths "after release workspace allocation"
candidate_owned=0
release_succeeded=0
published_archive=
published_source=
published_checksum=
cleanup() {
  if [ "$release_succeeded" -ne 1 ]; then
    if [ -n "$published_archive" ] || [ -n "$published_source" ] || [ -n "$published_checksum" ]; then
      assert_safe_repo_directory "$release_dir" "release publication cleanup" || return 1
      [ -z "$published_archive" ] || /bin/rm -f -- "$published_archive"
      [ -z "$published_source" ] || /bin/rm -f -- "$published_source"
      [ -z "$published_checksum" ] || /bin/rm -f -- "$published_checksum"
    fi
    if [ "$candidate_owned" -eq 1 ]; then
      assert_safe_repo_directory "$candidate" "release candidate cleanup" || return 1
      /bin/rm -rf -- "$candidate"
    fi
  fi
  assert_safe_repo_directory "$work_dir" "release workspace cleanup" || return 1
  /bin/rm -rf -- "$work_dir"
}
cleanup_and_exit() {
  trap - EXIT HUP INT TERM
  cleanup
  exit 1
}
trap cleanup EXIT
trap cleanup_and_exit HUP INT TERM

developer_id_ledger="$work_dir/developer-id-identities"
identity_inventory="$work_dir/code-signing-identities"
if ! /usr/bin/security find-identity -v -p codesigning \
  >"$identity_inventory" 2>/dev/null; then
  echo "Could not inspect the macOS code-signing identities." >&2
  exit 1
fi
/usr/bin/sed -n \
  's/^[[:space:]]*[0-9][0-9]*) \([0-9A-Fa-f]\{40\}\) "\(Developer ID Application: [^"]*\)"$/\1\	\2/p' \
  "$identity_inventory" >"$developer_id_ledger"

if [ -n "$identity_request" ]; then
  /usr/bin/awk -F '\t' -v requested="$identity_request" \
    '$1 == requested || $2 == requested { print }' \
    "$developer_id_ledger" >"$work_dir/selected-identities"
else
  /bin/cp "$developer_id_ledger" "$work_dir/selected-identities"
fi
selected_count=$(/usr/bin/awk 'END { print NR + 0 }' "$work_dir/selected-identities")
if [ "$selected_count" -eq 0 ]; then
  echo "No matching Apple Developer ID Application signing identity with a usable private key was found." >&2
  echo "Install an Apple-issued Developer ID Application certificate, then rerun this command." >&2
  exit 1
elif [ "$selected_count" -ne 1 ]; then
  echo "More than one Developer ID Application identity is available; select one with --signing-identity." >&2
  exit 1
fi
identity_sha1=$(/usr/bin/awk -F '\t' 'NR == 1 { print $1 }' "$work_dir/selected-identities")
identity=$(/usr/bin/awk -F '\t' 'NR == 1 { print $2 }' "$work_dir/selected-identities")
identity_sha1=$(
  printf '%s' "$identity_sha1" | /usr/bin/tr '[:lower:]' '[:upper:]'
)

for required_tool in /usr/bin/codesign /usr/sbin/spctl /usr/bin/ditto /usr/bin/xcrun; do
  [ -x "$required_tool" ] || {
    echo "Required macOS release tool is unavailable: $required_tool" >&2
    exit 1
  }
done
/usr/bin/xcrun -f notarytool >/dev/null 2>&1 || {
  echo "notarytool is unavailable from the selected Xcode installation." >&2
  exit 1
}
/usr/bin/xcrun -f stapler >/dev/null 2>&1 || {
  echo "stapler is unavailable from the selected Xcode installation." >&2
  exit 1
}

signed_leaf_sha1() {
  signed_path=$1
  certificate_prefix="$work_dir/certificate-$signed_leaf_sequence-"
  signed_leaf_sequence=$((signed_leaf_sequence + 1))
  /usr/bin/codesign -d --extract-certificates "$certificate_prefix" \
    "$signed_path" >/dev/null 2>&1
  [ -f "${certificate_prefix}0" ] || {
    echo "Could not extract the leaf certificate from signed code." >&2
    exit 1
  }
  observed_leaf_sha1=$(
    /usr/bin/shasum -a 1 "${certificate_prefix}0" |
      /usr/bin/awk '{ print toupper($1) }'
  )
}

verify_distribution_signature() {
  signed_path=$1
  expected_identifier=${2:-}
  require_bound_info=${3:-0}
  /usr/bin/codesign --verify --strict --verbose=2 "$signed_path"
  details=$(/usr/bin/codesign -dv --verbose=4 "$signed_path" 2>&1)
  if ! printf '%s\n' "$details" |
    /usr/bin/grep -Eq '^Authority=Developer ID Application: '; then
    echo "Signed code does not have an Apple Developer ID Application authority." >&2
    exit 1
  fi
  if ! printf '%s\n' "$details" |
    /usr/bin/grep -Eq '^TeamIdentifier=[A-Z0-9]{10}$'; then
    echo "Signed code does not have a valid Apple Developer Team identifier." >&2
    exit 1
  fi
  team_identifier=$(
    printf '%s\n' "$details" |
      /usr/bin/sed -n 's/^TeamIdentifier=\([A-Z0-9]\{10\}\)$/\1/p'
  )
  if [ -n "$expected_identifier" ] &&
    ! printf '%s\n' "$details" |
      /usr/bin/grep -Fxq "Identifier=$expected_identifier"; then
    echo "Signed code has an unexpected identifier: $signed_path" >&2
    exit 1
  fi
  if [ "$require_bound_info" -eq 1 ] &&
    ! printf '%s\n' "$details" |
      /usr/bin/grep -Eq '^Info[.]plist entries=[1-9][0-9]*$'; then
    echo "Signed helper does not bind its embedded Info.plist metadata." >&2
    exit 1
  fi
  if ! printf '%s\n' "$details" | /usr/bin/grep -Eq '^Timestamp=.+' ||
    printf '%s\n' "$details" |
      /usr/bin/grep -Eq '^Timestamp=(none|-)[[:space:]]*$'; then
    echo "Signed code does not have a secure timestamp." >&2
    exit 1
  fi
  if ! printf '%s\n' "$details" |
    /usr/bin/grep -Fq 'flags=0x10000(runtime)'; then
    echo "Signed code does not enable the hardened runtime." >&2
    exit 1
  fi
  requirement=$(/usr/bin/codesign -dr - "$signed_path" 2>&1)
  if [ -n "$expected_identifier" ] &&
    ! printf '%s\n' "$requirement" |
      /usr/bin/grep -Fq "identifier \"$expected_identifier\""; then
    echo "Signed code has an unexpected designated-requirement identifier." >&2
    exit 1
  fi
  if ! printf '%s\n' "$requirement" |
    /usr/bin/grep -Fq 'anchor apple generic'; then
    echo "Signed code does not anchor its designated requirement to Apple." >&2
    exit 1
  fi
  if ! printf '%s\n' "$requirement" |
    /usr/bin/grep -Fq '1.2.840.113635.100.6.2.6'; then
    echo "Signed code does not require the Developer ID intermediate certificate." >&2
    exit 1
  fi
  if ! printf '%s\n' "$requirement" |
    /usr/bin/grep -Fq '1.2.840.113635.100.6.1.13'; then
    echo "Signed code does not require a Developer ID Application certificate." >&2
    exit 1
  fi
  if ! printf '%s\n' "$requirement" |
      /usr/bin/grep -Fq "certificate leaf[subject.OU] = \"$team_identifier\"" &&
    ! printf '%s\n' "$requirement" |
      /usr/bin/grep -Fq "certificate leaf[subject.OU] = $team_identifier"; then
    echo "Signed code's designated requirement does not bind its Apple team." >&2
    exit 1
  fi
  if [ -n "$expected_identifier" ]; then
    explicit_requirement="identifier \"$expected_identifier\" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"$team_identifier\""
    /usr/bin/codesign --verify --strict --verbose=2 \
      -R="$explicit_requirement" "$signed_path"
  fi
  signed_leaf_sha1 "$signed_path"
  if [ "$observed_leaf_sha1" != "$identity_sha1" ]; then
    echo "Signed code does not use the selected Developer ID Application certificate." >&2
    exit 1
  fi
}

signed_leaf_sequence=0
probe_path="$work_dir/signing-probe"
require_safe_release_output_paths "before signing prerequisite probe"
/bin/cp /usr/bin/true "$probe_path"
if ! /usr/bin/codesign --force --options runtime --timestamp \
  --sign "$identity_sha1" "$probe_path" >/dev/null 2>&1; then
  echo "The selected Developer ID Application identity cannot produce a secure-timestamped hardened-runtime signature." >&2
  exit 1
fi
verify_distribution_signature "$probe_path"

notary_profile_check="$work_dir/notary-profile-check.json"
notary_profile_errors="$work_dir/notary-profile-errors"
: >"$notary_profile_check"
: >"$notary_profile_errors"
/bin/chmod 600 "$notary_profile_check" "$notary_profile_errors"
if ! /usr/bin/xcrun notarytool history --keychain-profile "$notary_profile" \
  --output-format json >"$notary_profile_check" 2>"$notary_profile_errors"; then
  echo "The notarytool Keychain profile could not authenticate with Apple's notary service." >&2
  echo "Create or refresh it interactively with: xcrun notarytool store-credentials PROFILE" >&2
  echo "Then select that non-secret profile name with --notary-profile." >&2
  exit 1
fi

if [ "$prerequisites_only" -eq 1 ]; then
  echo "Production release prerequisites are available."
  echo "Signing identity: $identity"
  echo "Notary credentials: Keychain profile is present."
  exit 0
fi

# Tauri reads this non-secret selector only after inherited signing and
# notarization environment variables have been rejected above. Credentials
# remain exclusively in the notarytool Keychain profile.
export APPLE_SIGNING_IDENTITY="$identity_sha1"
umask 022

assert_no_local_build_overrides() {
  overrides="$work_dir/local-build-overrides"
  : >"$overrides"
  for directory in "$repo_dir" "$repo_dir/apps/woof"; do
    /usr/bin/find "$directory" -mindepth 1 -maxdepth 1 \
      \( -name '.env' -o -name '.env.*' -o -name '.npmrc' \) \
      -print >>"$overrides"
  done
  /usr/bin/find "$repo_dir" \
    \( -path "$repo_dir/.git" -o -path "$repo_dir/target" -o \
       -path "$repo_dir/node_modules" -o -path "$repo_dir/artifacts" -o \
       -path "$repo_dir/apps/woof/node_modules" \) -prune -o \
    -name '.cargo' -print >>"$overrides"
  if [ -s "$overrides" ]; then
    echo "Refusing local environment or package-manager build overrides." >&2
    exit 1
  fi
}

assert_no_local_build_overrides
: >"$work_dir/empty-user-npmrc"
: >"$work_dir/empty-global-npmrc"
/bin/chmod 600 "$work_dir/empty-user-npmrc" "$work_dir/empty-global-npmrc"
export NPM_CONFIG_USERCONFIG="$work_dir/empty-user-npmrc"
export NPM_CONFIG_GLOBALCONFIG="$work_dir/empty-global-npmrc"

snapshot_sources() {
  destination=$1
  paths_file="$work_dir/source-paths"
  : >"$paths_file"
  /usr/bin/git --no-replace-objects -C "$repo_dir" \
    ls-files --cached --others --exclude-standard >>"$paths_file"
  sort -u -o "$paths_file" "$paths_file"
  : >"$destination"
  while IFS= read -r relative; do
    [ -n "$relative" ] || continue
    path="$repo_dir/$relative"
    if [ -L "$path" ]; then
      link_target=$(readlink "$path")
      hash=$(printf '%s' "$link_target" | /usr/bin/shasum -a 256 | /usr/bin/awk '{print $1}')
      mode=$(/usr/bin/stat -f '%Lp' "$path")
      printf 'symlink %s %s  %s\n' "$mode" "$hash" "$relative" >>"$destination"
    elif [ -f "$path" ]; then
      hash=$(/usr/bin/shasum -a 256 "$path" | /usr/bin/awk '{print $1}')
      mode=$(/usr/bin/stat -f '%Lp' "$path")
      printf 'file %s %s  %s\n' "$mode" "$hash" "$relative" >>"$destination"
    else
      printf 'missing - -  %s\n' "$relative" >>"$destination"
    fi
  done <"$paths_file"
}

assert_sources_stable() {
  phase=$1
  observed="$work_dir/source-$phase"
  snapshot_sources "$observed"
  if ! /usr/bin/cmp -s "$work_dir/source-baseline" "$observed"; then
    echo "Source inputs changed during the release pipeline ($phase); refusing a mixed candidate." >&2
    exit 1
  fi
}

assert_staged_sidecars_stable() {
  daemon_sha=$(
    /usr/bin/shasum -a 256 \
      "$repo_dir/apps/woof/src-tauri/binaries/woof_d-aarch64-apple-darwin" |
      /usr/bin/awk '{print $1}'
  )
  mcp_sha=$(
    /usr/bin/shasum -a 256 \
      "$repo_dir/apps/woof/src-tauri/binaries/woof-mcp-aarch64-apple-darwin" |
      /usr/bin/awk '{print $1}'
  )
  if [ "$daemon_sha" != "$staged_daemon_sha" ] || [ "$mcp_sha" != "$staged_mcp_sha" ]; then
    echo "Staged release sidecars changed during the release pipeline." >&2
    exit 1
  fi
}

clean_derived_output() {
  for output in \
    "$candidate" \
    "$repo_dir/apps/woof/build" \
    "$repo_dir/apps/woof/.svelte-kit"
  do
    case "$output" in
      "$repo_dir/target/$target/release/bundle/macos/woof.app" | \
        "$repo_dir/apps/woof/build" | \
        "$repo_dir/apps/woof/.svelte-kit")
        ;;
      *)
        echo "Refusing unexpected derived output path: $output" >&2
        exit 1
        ;;
    esac
    assert_safe_repo_directory "$output" "derived output cleanup" || exit 1
    if [ -e "$output" ] || [ -L "$output" ]; then
      /bin/rm -rf -- "$output"
    fi
  done
}

expect_plist_value() {
  bundle=$1
  key=$2
  expected=$3
  actual=$(/usr/bin/plutil -extract "$key" raw -o - "$bundle/Contents/Info.plist")
  if [ "$actual" != "$expected" ]; then
    echo "Unexpected $key in release bundle: $actual" >&2
    exit 1
  fi
}

verify_entitlements() {
  binary=$1
  role=$2
  entitlements_file="$work_dir/entitlements-$(basename "$binary")"
  entitlements_errors="$work_dir/entitlements-errors-$(basename "$binary")"
  if ! /usr/bin/codesign -d --entitlements :- "$binary" \
    >"$entitlements_file" 2>"$entitlements_errors"; then
    echo "Could not inspect release entitlements: $binary" >&2
    exit 1
  fi

  allowed='{"com.apple.security.device.audio-input":true,"com.apple.security.network.client":true}'
  if [ "$role" = main ]; then
    [ -s "$entitlements_file" ] || {
      echo "The application executable has no release entitlements." >&2
      exit 1
    }
    source_entitlements=$(
      /usr/bin/plutil -convert json -o - \
        "$repo_dir/apps/woof/src-tauri/Entitlements.plist" |
        /usr/bin/jq -S -c .
    )
    actual_entitlements=$(
      /usr/bin/plutil -convert json -o - "$entitlements_file" |
        /usr/bin/jq -S -c .
    )
    if [ "$source_entitlements" != "$allowed" ] || [ "$actual_entitlements" != "$allowed" ]; then
      echo "Application entitlements differ from the release allowlist." >&2
      exit 1
    fi
  elif [ -s "$entitlements_file" ]; then
    sidecar_entitlements=$(
      /usr/bin/plutil -convert json -o - "$entitlements_file" |
        /usr/bin/jq -S -c .
    )
    if ! printf '%s\n' "$sidecar_entitlements" |
      /usr/bin/jq -e '
        type == "object" and
        ((keys - [
          "com.apple.security.device.audio-input",
          "com.apple.security.network.client"
        ]) | length == 0) and
        all(.[]; . == true)
      ' >/dev/null; then
      echo "A sidecar entitlement is outside the release allowlist." >&2
      exit 1
    fi
  fi
}

verify_dependencies() {
  binary=$1
  stem=$(basename "$binary")
  dependency_output="$work_dir/dependencies-$stem"
  dependency_list="$work_dir/dependency-list-$stem"
  load_commands="$work_dir/load-commands-$stem"
  compiled_strings="$work_dir/compiled-strings-$stem"
  /usr/bin/strings -a "$binary" >"$compiled_strings"
  if /usr/bin/grep -Fq "$repo_dir" "$compiled_strings" ||
    /usr/bin/grep -Fq "$account_home" "$compiled_strings"; then
    echo "Release binary contains a build-host path: $binary" >&2
    exit 1
  fi
  if [ -n "$repo_account_suffix" ] &&
    /usr/bin/grep -Fq "rust-build-home/$repo_account_suffix" "$compiled_strings"; then
    echo "Release binary used the broad path remap for a workspace source: $binary" >&2
    exit 1
  fi
  /usr/bin/otool -L "$binary" >"$dependency_output"
  /usr/bin/awk 'NR > 1 { print $1 }' "$dependency_output" >"$dependency_list"
  [ -s "$dependency_list" ] || {
    echo "Release binary has no inspectable dependency ledger: $binary" >&2
    exit 1
  }
  while IFS= read -r dependency; do
    case "$dependency" in
      /System/Library/* | /usr/lib/*) ;;
      *)
        echo "Release binary has a non-system dynamic dependency: $binary" >&2
        exit 1
        ;;
    esac
  done <"$dependency_list"
  if [ "$stem" = woof_d ]; then
    sqlite_dependencies=$(
      /usr/bin/awk '$0 ~ /\/libsqlite3([.][0-9]+)*[.]dylib$/ { print }' \
        "$dependency_list"
    )
    if [ "$sqlite_dependencies" != /usr/lib/libsqlite3.dylib ]; then
      echo "The daemon must link exactly the macOS system SQLite library." >&2
      exit 1
    fi
  fi
  /usr/bin/otool -l "$binary" >"$load_commands"
  if /usr/bin/grep -Eq '^[[:space:]]*cmd LC_RPATH$' "$load_commands"; then
    echo "Release binary contains a disallowed runtime search path: $binary" >&2
    exit 1
  fi
  if /usr/bin/grep -Eq '^[[:space:]]*(segname __DWARF|sectname __debug_)' "$load_commands"; then
    echo "Release binary contains embedded debug sections: $binary" >&2
    exit 1
  fi
}

verify_signed_arm64() {
  binary=$1
  role=$2
  expected_identifier=$3
  [ -x "$binary" ] || {
    echo "Missing executable release binary: $binary" >&2
    exit 1
  }
  description=$(/usr/bin/file -b "$binary")
  if [ "$description" != "Mach-O 64-bit executable arm64" ]; then
    echo "Release binary is not thin arm64: $binary ($description)" >&2
    exit 1
  fi
  architectures=$(/usr/bin/lipo -archs "$binary")
  if [ "$architectures" != arm64 ]; then
    echo "Release binary has an unexpected architecture ledger: $binary" >&2
    exit 1
  fi
  require_bound_info=0
  if [ "$role" = sidecar ]; then
    require_bound_info=1
  fi
  verify_distribution_signature "$binary" "$expected_identifier" "$require_bound_info"
  verify_entitlements "$binary" "$role"
  verify_dependencies "$binary"
}

assert_no_debug_payload() {
  root=$1
  debug_entries="$work_dir/debug-entries"
  /usr/bin/find "$root" \
    \( -type d \( -iname '*.dSYM' -o -iname '*.dSYM.*' \) -o \
       -type f \( -iname '*.map' -o -iname '*.pdb' -o -iname '*.debug' -o \
         -iname '*.profraw' -o -iname '*.profdata' -o -iname '*.gcda' -o \
         -iname '*.gcno' -o -iname '*.bc' -o -iname '*.ll' \) \) \
    -print >"$debug_entries"
  if [ -s "$debug_entries" ]; then
    echo "Release output contains a debug or source-map payload." >&2
    exit 1
  fi

  mapping_markers="$work_dir/source-map-markers"
  set +e
  /usr/bin/grep -Ilr -- 'sourceMappingURL=' "$root" >"$mapping_markers" 2>/dev/null
  marker_status=$?
  set -e
  case "$marker_status" in
    0) ;;
    1) : >"$mapping_markers" ;;
    *)
      echo "Could not complete the source-map marker scan." >&2
      exit 1
      ;;
  esac
  if [ -s "$mapping_markers" ]; then
    echo "Release output contains an embedded source-map reference." >&2
    exit 1
  fi
}

strict_bundle_layout() {
  bundle=$1
  staple_state=$2
  bundle_parent=$(dirname "$bundle")
  bundle_name=$(basename "$bundle")
  expected="$work_dir/expected-bundle-layout"
  actual="$work_dir/actual-bundle-layout"
  {
    printf '%s\n' "$bundle_name"
    printf '%s\n' "$bundle_name/Contents"
    if [ "$staple_state" = stapled ]; then
      printf '%s\n' "$bundle_name/Contents/CodeResources"
    fi
    printf '%s\n' "$bundle_name/Contents/Info.plist"
    printf '%s\n' "$bundle_name/Contents/MacOS"
    printf '%s\n' "$bundle_name/Contents/MacOS/woof"
    printf '%s\n' "$bundle_name/Contents/MacOS/woof-mcp"
    printf '%s\n' "$bundle_name/Contents/MacOS/woof_d"
    printf '%s\n' "$bundle_name/Contents/Resources"
    printf '%s\n' "$bundle_name/Contents/Resources/LICENSE"
    printf '%s\n' "$bundle_name/Contents/Resources/THIRD_PARTY_NOTICES"
    printf '%s\n' "$bundle_name/Contents/Resources/icon.icns"
    printf '%s\n' "$bundle_name/Contents/_CodeSignature"
    printf '%s\n' "$bundle_name/Contents/_CodeSignature/CodeResources"
  } >"$expected"
  (
    cd "$bundle_parent"
    /usr/bin/find "$bundle_name" -print | /usr/bin/sort
  ) >"$actual"
  if ! /usr/bin/cmp -s "$expected" "$actual"; then
    echo "Release bundle content is outside the exact layout allowlist." >&2
    exit 1
  fi

  while IFS= read -r relative; do
    path="$bundle_parent/$relative"
    if [ -L "$path" ]; then
      echo "Release bundle contains a symbolic link." >&2
      exit 1
    elif [ -d "$path" ]; then
      expected_mode=755
    elif [ -f "$path" ]; then
      case "$relative" in
        "$bundle_name"/Contents/MacOS/*) expected_mode=755 ;;
        *) expected_mode=644 ;;
      esac
      link_count=$(/usr/bin/stat -f '%l' "$path")
      if [ "$link_count" != 1 ]; then
        echo "Release bundle contains a hard-linked file." >&2
        exit 1
      fi
    else
      echo "Release bundle contains an unsupported filesystem object." >&2
      exit 1
    fi
    actual_mode=$(/usr/bin/stat -f '%Lp' "$path")
    if [ "$actual_mode" != "$expected_mode" ]; then
      echo "Release bundle has an unexpected file mode: $relative" >&2
      exit 1
    fi
    permission_text=$(/bin/ls -lde "$path" | /usr/bin/awk 'NR == 1 { print $1 }')
    case "$permission_text" in
      *+*)
        echo "Release bundle contains an access-control list." >&2
        exit 1
        ;;
    esac
  done <"$actual"

  attribute_ledger="$work_dir/bundle-attributes"
  unexpected_attributes="$work_dir/unexpected-bundle-attributes"
  /usr/bin/xattr -r "$bundle" >"$attribute_ledger"
  /usr/bin/awk -F ': ' '$NF != "com.apple.provenance" { print }' \
    "$attribute_ledger" >"$unexpected_attributes"
  if [ -s "$unexpected_attributes" ]; then
    echo "Release bundle contains an extended attribute outside the allowlist." >&2
    exit 1
  fi
}

verify_bundle() {
  bundle=$1
  staple_state=$2
  strict_bundle_layout "$bundle" "$staple_state"
  assert_no_debug_payload "$bundle"
  node "$repo_dir/scripts/audit-zero-remnants.mjs" tree "$bundle"
  /usr/bin/codesign --verify --deep --strict --verbose=2 "$bundle"
  verify_signed_arm64 "$bundle/Contents/MacOS/woof" main com.julius.woof
  verify_signed_arm64 \
    "$bundle/Contents/MacOS/woof_d" sidecar com.julius.woof.daemon
  verify_signed_arm64 \
    "$bundle/Contents/MacOS/woof-mcp" sidecar com.julius.woof.mcp

  expect_plist_value "$bundle" CFBundleIdentifier com.julius.woof
  expect_plist_value "$bundle" CFBundleName woof
  expect_plist_value "$bundle" CFBundleDisplayName woof
  expect_plist_value "$bundle" CFBundleExecutable woof
  expect_plist_value "$bundle" CFBundlePackageType APPL
  expect_plist_value "$bundle" CFBundleIconFile icon.icns
  expect_plist_value "$bundle" CFBundleShortVersionString 0.1.0
  expect_plist_value "$bundle" CFBundleVersion 0.1.0
  expect_plist_value "$bundle" LSMinimumSystemVersion 14.0
  expect_plist_value "$bundle" LSUIElement true
  expect_plist_value "$bundle" LSMultipleInstancesProhibited true
  expect_plist_value "$bundle" CFBundleURLTypes.0.CFBundleURLName com.julius.woof
  expect_plist_value "$bundle" CFBundleURLTypes.0.CFBundleURLSchemes.0 woof
  url_types=$(
    /usr/bin/plutil -extract CFBundleURLTypes json -o - "$bundle/Contents/Info.plist" |
      /usr/bin/jq -S -c .
  )
  if [ "$url_types" != '[{"CFBundleURLName":"com.julius.woof","CFBundleURLSchemes":["woof"]}]' ]; then
    echo "Release bundle URL metadata is outside the exact allowlist." >&2
    exit 1
  fi

  /usr/bin/cmp -s \
    "$repo_dir/LICENSE" \
    "$bundle/Contents/Resources/LICENSE" || {
    echo "Bundled MIT license does not match the source license file." >&2
    exit 1
  }

  /usr/bin/cmp -s \
    "$repo_dir/THIRD_PARTY_NOTICES" \
    "$bundle/Contents/Resources/THIRD_PARTY_NOTICES" || {
    echo "Bundled third-party notices do not match the source notice file." >&2
    exit 1
  }

  /usr/bin/cmp -s \
    "$repo_dir/apps/woof/src-tauri/icons/icon.icns" \
    "$bundle/Contents/Resources/icon.icns" || {
    echo "Bundled application icon does not match the generated Tauri input." >&2
    exit 1
  }

  if [ "$staple_state" = stapled ]; then
    /usr/bin/xcrun stapler validate -q "$bundle"
    /usr/sbin/spctl --assess --type execute --verbose=4 "$bundle"
  fi
}

bundle_manifest() {
  bundle=$1
  destination=$2
  bundle_parent=$(dirname "$bundle")
  bundle_name=$(basename "$bundle")
  entries="$work_dir/bundle-entries"
  (
    cd "$bundle_parent"
    find "$bundle_name" \( -type f -o -type l \) -print | sort
  ) >"$entries"
  : >"$destination"
  while IFS= read -r relative; do
    path="$bundle_parent/$relative"
    if [ -L "$path" ]; then
      link_target=$(readlink "$path")
      hash=$(printf '%s' "$link_target" | /usr/bin/shasum -a 256 | /usr/bin/awk '{print $1}')
      mode=$(/usr/bin/stat -f '%Lp' "$path")
      printf 'symlink %s %s  %s\n' "$mode" "$hash" "$relative" >>"$destination"
    else
      hash=$(/usr/bin/shasum -a 256 "$path" | /usr/bin/awk '{print $1}')
      mode=$(/usr/bin/stat -f '%Lp' "$path")
      printf 'file %s %s  %s\n' "$mode" "$hash" "$relative" >>"$destination"
    fi
  done <"$entries"
}

create_deterministic_archive() {
  archive_root=$1
  archive_path=$2
  entries="$work_dir/archive-entries"
  (
    cd "$archive_root"
    find woof.app -print | sort
  ) >"$entries"
  (
    cd "$archive_root"
    /usr/bin/zip -0 -X -q -y "$archive_path" -@ <"$entries"
  )
}

verify_archive_listing() {
  archive=$1
  staple_state=$2
  archive_listing="$work_dir/archive-listing"
  expected_listing="$work_dir/expected-archive-listing"
  duplicate_listing="$work_dir/duplicate-archive-listing"
  /usr/bin/zipinfo -1 "$archive" | /usr/bin/sort >"$archive_listing"
  {
    printf '%s\n' 'woof.app/'
    printf '%s\n' 'woof.app/Contents/'
    if [ "$staple_state" = stapled ]; then
      printf '%s\n' 'woof.app/Contents/CodeResources'
    fi
    printf '%s\n' 'woof.app/Contents/Info.plist'
    printf '%s\n' 'woof.app/Contents/MacOS/'
    printf '%s\n' 'woof.app/Contents/MacOS/woof'
    printf '%s\n' 'woof.app/Contents/MacOS/woof-mcp'
    printf '%s\n' 'woof.app/Contents/MacOS/woof_d'
    printf '%s\n' 'woof.app/Contents/Resources/'
    printf '%s\n' 'woof.app/Contents/Resources/LICENSE'
    printf '%s\n' 'woof.app/Contents/Resources/THIRD_PARTY_NOTICES'
    printf '%s\n' 'woof.app/Contents/Resources/icon.icns'
    printf '%s\n' 'woof.app/Contents/_CodeSignature/'
    printf '%s\n' 'woof.app/Contents/_CodeSignature/CodeResources'
  } >"$expected_listing"
  if ! /usr/bin/cmp -s "$expected_listing" "$archive_listing"; then
    echo "Release archive content is outside the exact layout allowlist." >&2
    exit 1
  fi
  /usr/bin/zipinfo -1 "$archive" | /usr/bin/sort | /usr/bin/uniq -d >"$duplicate_listing"
  if [ -s "$duplicate_listing" ]; then
    echo "Release archive contains duplicate path entries." >&2
    exit 1
  fi
  while IFS= read -r archived_path; do
    case "$archived_path" in
      /* | *\\* | ../* | */../* | */.. | ./* | */./*)
        echo "Release archive contains an unsafe path." >&2
        exit 1
        ;;
    esac
  done <"$archive_listing"
}

cd "$repo_dir"
require_safe_release_output_paths "before dependency installation"
npm ci
require_safe_release_output_paths "after dependency installation"
node scripts/build-icons.mjs
require_safe_release_output_paths "after icon build"
snapshot_sources "$work_dir/source-baseline"
source_manifest_sha=$(
  /usr/bin/shasum -a 256 "$work_dir/source-baseline" | /usr/bin/awk '{print $1}'
)
require_safe_release_output_paths "before sidecar build"
scripts/stage-sidecars.sh release
require_safe_release_output_paths "after sidecar build"
assert_sources_stable "after-sidecar-build"
staged_daemon_sha=$(
  /usr/bin/shasum -a 256 \
    "$repo_dir/apps/woof/src-tauri/binaries/woof_d-aarch64-apple-darwin" |
    /usr/bin/awk '{print $1}'
)
staged_mcp_sha=$(
  /usr/bin/shasum -a 256 \
    "$repo_dir/apps/woof/src-tauri/binaries/woof-mcp-aarch64-apple-darwin" |
    /usr/bin/awk '{print $1}'
)
node scripts/audit-zero-remnants.mjs source "$repo_dir"

require_safe_release_output_paths "before repository verification"
scripts/verify.sh --sidecars-pre-staged
require_safe_release_output_paths "after repository verification"
assert_sources_stable "after-verification"
assert_staged_sidecars_stable
clean_derived_output
# Remove the entire workspace target tree after verification. Besides forcing
# Tauri to regenerate its embedded frontend context, this prevents unrelated
# debug or prior release bundles from surviving beside the candidate as stale
# compiled artifacts.
require_safe_release_output_paths "before Cargo cleanup"
"$cargo_path" clean --locked --offline
require_safe_release_output_paths "before application build and signing"
candidate_owned=1
npm run tauri:build:pre-staged --workspace apps/woof
require_safe_release_output_paths "after application build and signing"
assert_sources_stable "after-bundle"
assert_staged_sidecars_stable
node scripts/audit-zero-remnants.mjs source "$repo_dir"
assert_no_debug_payload "$repo_dir/apps/woof/build"
node scripts/audit-zero-remnants.mjs tree "$repo_dir/apps/woof/build"

[ -d "$candidate" ] && [ ! -L "$candidate" ] || {
  echo "Tauri did not produce the expected release bundle: $candidate" >&2
  exit 1
}
verify_bundle "$candidate" unstapled

require_safe_release_output_paths "before notarization submission staging"
notary_submission="$work_dir/woof-notary-submission.zip"
/usr/bin/ditto -c -k --keepParent --norsrc --noextattr --noqtn --noacl \
  "$candidate" "$notary_submission"
/usr/bin/unzip -tq "$notary_submission"
node scripts/audit-zero-remnants.mjs tree "$notary_submission"
notary_response="$work_dir/notary-response.json"
notary_errors="$work_dir/notary-errors"
: >"$notary_response"
: >"$notary_errors"
/bin/chmod 600 "$notary_response" "$notary_errors"
if ! /usr/bin/xcrun notarytool submit "$notary_submission" \
  --keychain-profile "$notary_profile" --wait --timeout 30m \
  --output-format json >"$notary_response" 2>"$notary_errors"; then
  echo "Apple notarization did not complete successfully; no release artifact was created." >&2
  exit 1
fi
if ! /usr/bin/jq -e '
  .status == "Accepted" and
  (.id | type == "string") and
  (.id | test("^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$"))
' "$notary_response" >/dev/null; then
  echo "Apple did not return an accepted, valid notarization response; no release artifact was created." >&2
  exit 1
fi
notary_submission_id=$(/usr/bin/jq -r '.id' "$notary_response")

require_safe_release_output_paths "before ticket stapling"
/usr/bin/xcrun stapler staple -q "$candidate"
require_safe_release_output_paths "after ticket stapling"
verify_bundle "$candidate" stapled
assert_sources_stable "after-notarization"
assert_staged_sidecars_stable

main_sha=$(
  /usr/bin/shasum -a 256 "$candidate/Contents/MacOS/woof" |
    /usr/bin/awk '{print $1}'
)
daemon_sha=$(
  /usr/bin/shasum -a 256 "$candidate/Contents/MacOS/woof_d" |
    /usr/bin/awk '{print $1}'
)
mcp_sha=$(
  /usr/bin/shasum -a 256 "$candidate/Contents/MacOS/woof-mcp" |
    /usr/bin/awk '{print $1}'
)
icon_sha=$(
  /usr/bin/shasum -a 256 "$candidate/Contents/Resources/icon.icns" |
    /usr/bin/awk '{print $1}'
)

archive_root="$work_dir/archive-root"
assert_safe_repo_directory "$archive_root" "before archive staging" || exit 1
mkdir -p "$archive_root"
assert_safe_repo_directory "$archive_root" "after archive staging" || exit 1
/usr/bin/ditto --norsrc --noextattr --noqtn --noacl \
  "$candidate" "$archive_root/woof.app"
find "$archive_root/woof.app" -exec /usr/bin/touch -h -t 200001010000 {} +
verify_bundle "$archive_root/woof.app" stapled
bundle_manifest "$candidate" "$work_dir/bundle-before-staging"
bundle_manifest "$archive_root/woof.app" "$work_dir/bundle-after-staging"
if ! /usr/bin/cmp -s "$work_dir/bundle-before-staging" "$work_dir/bundle-after-staging"; then
  echo "Archive staging changed bundle bytes or file modes." >&2
  exit 1
fi

first_archive="$work_dir/woof-first.zip"
second_archive="$work_dir/woof-second.zip"
create_deterministic_archive "$archive_root" "$first_archive"
create_deterministic_archive "$archive_root" "$second_archive"
if ! /usr/bin/cmp -s "$first_archive" "$second_archive"; then
  echo "Archive creation is not deterministic for an identical signed bundle." >&2
  exit 1
fi
verify_archive_listing "$first_archive" stapled
verify_archive_listing "$second_archive" stapled
node scripts/audit-zero-remnants.mjs tree "$first_archive" "$second_archive"

/usr/bin/unzip -tq "$first_archive"
roundtrip_root="$work_dir/roundtrip"
assert_safe_repo_directory "$roundtrip_root" "before archive round-trip extraction" || exit 1
mkdir -p "$roundtrip_root"
assert_safe_repo_directory "$roundtrip_root" "after archive round-trip extraction" || exit 1
/usr/bin/unzip -q "$first_archive" -d "$roundtrip_root"
top_level_entries=$(
  /usr/bin/find "$roundtrip_root" -mindepth 1 -maxdepth 1 -print
)
if [ "$top_level_entries" != "$roundtrip_root/woof.app" ]; then
  echo "Release archive extracted an unexpected top-level entry." >&2
  exit 1
fi
verify_bundle "$roundtrip_root/woof.app" stapled
bundle_manifest "$archive_root/woof.app" "$work_dir/bundle-before-archive"
bundle_manifest "$roundtrip_root/woof.app" "$work_dir/bundle-after-archive"
if ! /usr/bin/cmp -s "$work_dir/bundle-before-archive" "$work_dir/bundle-after-archive"; then
  echo "Archive round trip changed bundle content or executable modes." >&2
  exit 1
fi
assert_sources_stable "after-archive-verification"
assert_staged_sidecars_stable

require_safe_release_output_paths "before release publication staging"
bundle_version=$(
  /usr/bin/plutil -extract CFBundleShortVersionString raw -o - \
    "$candidate/Contents/Info.plist"
)
printf '%s\n' "$bundle_version" | /usr/bin/grep -Eq '^[0-9][0-9A-Za-z.-]*$'
timestamp=$(/bin/date -u '+%Y%m%d-%H%M%SZ')
archive_name="woof-$bundle_version-macos-arm64-$timestamp.zip"
archive_path="$release_dir/$archive_name"
sha_path="$archive_path.sha256"
source_path="$archive_path.sources"
[ ! -e "$archive_path" ] && [ ! -L "$archive_path" ] && \
  [ ! -e "$sha_path" ] && [ ! -L "$sha_path" ] && \
  [ ! -e "$source_path" ] && [ ! -L "$source_path" ] || {
  echo "Refusing to overwrite an existing release artifact: $archive_path" >&2
  exit 1
}
staged_archive="$work_dir/$archive_name"
staged_source="$work_dir/$archive_name.sources"
staged_checksum="$work_dir/$archive_name.sha256"
/bin/mv "$first_archive" "$staged_archive"
/bin/mv "$work_dir/source-baseline" "$staged_source"
node scripts/audit-zero-remnants.mjs tree "$staged_source"
archive_sha=$(
  /usr/bin/shasum -a 256 "$staged_archive" | /usr/bin/awk '{print $1}'
)
{
  printf '# source-manifest-sha256: %s\n' "$source_manifest_sha"
  printf '# signing-certificate-sha1: %s\n' "$identity_sha1"
  printf '# notarization-submission-id: %s\n' "$notary_submission_id"
  printf '# main-executable-sha256: %s\n' "$main_sha"
  printf '# woof-d-sha256: %s\n' "$daemon_sha"
  printf '# woof-mcp-sha256: %s\n' "$mcp_sha"
  printf '# application-icon-sha256: %s\n' "$icon_sha"
  printf '%s  %s\n' "$archive_sha" "$archive_name"
} >"$staged_checksum"
node scripts/audit-zero-remnants.mjs tree \
  "$staged_archive" "$staged_source" "$staged_checksum"
(
  cd "$work_dir"
  /usr/bin/shasum -a 256 -c "$archive_name.sha256"
)

# Publish companions first and the already-verified archive last. On an
# ordinary failure, the EXIT trap removes any partial publication set.
# Each exact destination is created with link(2), which atomically fails when
# any destination leaf already exists (including a directory or symlink), so
# parallel runs cannot replace, redirect, or mix a publication set. Signals
# are ignored only while each link is recorded and its staged name is unlinked.
publish_exact_leaf() {
  /usr/bin/perl -e '
    link($ARGV[0], $ARGV[1]) or die "atomic publication failed: $!\n";
  ' -- "$1" "$2"
}

trap '' HUP INT TERM
require_safe_release_output_paths "before source-manifest publication"
publish_exact_leaf "$staged_source" "$source_path"
published_source=$source_path
/bin/rm -f -- "$staged_source"
require_safe_release_output_paths "before checksum publication"
publish_exact_leaf "$staged_checksum" "$sha_path"
published_checksum=$sha_path
/bin/rm -f -- "$staged_checksum"
require_safe_release_output_paths "before release-archive publication"
publish_exact_leaf "$staged_archive" "$archive_path"
published_archive=$archive_path
/bin/rm -f -- "$staged_archive"
release_succeeded=1
trap cleanup_and_exit HUP INT TERM

echo "Release candidate: $candidate"
echo "Release archive: $archive_path"
echo "Release source manifest: $source_path"
echo "Archive SHA-256: $archive_sha"
echo "Source manifest SHA-256: $source_manifest_sha"
echo "Main executable SHA-256: $main_sha"
echo "woof_d SHA-256: $daemon_sha"
echo "woof-mcp SHA-256: $mcp_sha"
echo "Distribution status: Developer ID signed, notarized, stapled, and Gatekeeper accepted."
