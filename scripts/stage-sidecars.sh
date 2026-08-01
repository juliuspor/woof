#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
profile=${1:-debug}
target="aarch64-apple-darwin"
deployment_target="14.0"
binary_dir="$repo_dir/apps/woof/src-tauri/binaries"

build_account_home=$(node -p 'require("node:os").userInfo().homedir')
case "$build_account_home" in
  /*) ;;
  *)
    echo "Could not resolve the build account home for path remapping." >&2
    exit 1
    ;;
esac
[ -d "$build_account_home" ] || {
  echo "Could not resolve the build account home for path remapping." >&2
  exit 1
}
rust_flag_separator=$(printf '\037')
unset RUSTFLAGS
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$build_account_home=rust-build-home${rust_flag_separator}--remap-path-prefix=$repo_dir=woof-source"
repo_account_suffix=
case "$repo_dir" in
  "$build_account_home"/*)
    repo_account_suffix=${repo_dir#"$build_account_home"/}
    ;;
esac

rejected_environment=$(
  /usr/bin/perl -e '
    for $name (sort keys %ENV) {
      if ($name =~ /^(?:LIBSQLITE3_SYS_|SQLITE3_|MACOSX_DEPLOYMENT_TARGET$)/) {
        print "$name\n";
      }
    }
  '
)
if [ -n "$rejected_environment" ]; then
  echo "Refusing sidecar-affecting inherited environment variables:" >&2
  printf '%s\n' "$rejected_environment" >&2
  exit 1
fi
if { [ -n "${CARGO_HOME+x}" ] && [ "$CARGO_HOME" != "$build_account_home/.cargo" ]; } ||
  { [ -n "${CARGO_TARGET_DIR+x}" ] && [ "$CARGO_TARGET_DIR" != "$repo_dir/target" ]; } ||
  [ -n "${CARGO_BUILD_TARGET_DIR+x}" ]; then
  echo "Refusing an inherited Cargo home or target-directory override." >&2
  exit 1
fi
export CARGO_HOME="$build_account_home/.cargo"
export CARGO_TARGET_DIR="$repo_dir/target"
unset CARGO_BUILD_TARGET_DIR
export MACOSX_DEPLOYMENT_TARGET="$deployment_target"

case "$profile" in
  debug)
    cargo_args=""
    profile_dir="debug"
    ;;
  release)
    cargo_args="--release"
    profile_dir="release"
    ;;
  *)
    echo "usage: scripts/stage-sidecars.sh [debug|release]" >&2
    exit 64
    ;;
esac

assert_safe_stage_directories() {
  stage_phase=$1
  for stage_directory in \
    "$repo_dir/target" \
    "$repo_dir/target/$target" \
    "$repo_dir/target/$target/$profile_dir" \
    "$repo_dir/apps" \
    "$repo_dir/apps/woof" \
    "$repo_dir/apps/woof/src-tauri" \
    "$binary_dir"
  do
    if [ -L "$stage_directory" ]; then
      echo "Refusing unsafe sidecar output path during $stage_phase: symlink component $stage_directory" >&2
      return 1
    fi
    if [ -e "$stage_directory" ] && [ ! -d "$stage_directory" ]; then
      echo "Refusing unsafe sidecar output path during $stage_phase: non-directory component $stage_directory" >&2
      return 1
    fi
  done
  if [ -d "$binary_dir" ]; then
    if ! unsafe_stage_entry=$(
      /usr/bin/find "$binary_dir" -mindepth 1 \
        ! -type d ! -type f -print -quit
    ); then
      echo "Could not inspect sidecar output paths during $stage_phase." >&2
      return 1
    fi
    if [ -n "$unsafe_stage_entry" ]; then
      echo "Refusing unsafe sidecar output path during $stage_phase: unsafe output entry $unsafe_stage_entry" >&2
      return 1
    fi
    if ! multiply_linked_stage_file=$(
      /usr/bin/find "$binary_dir" -mindepth 1 \
        -type f ! -links 1 -print -quit
    ); then
      echo "Could not inspect sidecar output links during $stage_phase." >&2
      return 1
    fi
    if [ -n "$multiply_linked_stage_file" ]; then
      echo "Refusing unsafe sidecar output path during $stage_phase: multiply-linked output file $multiply_linked_stage_file" >&2
      return 1
    fi
  fi
}

assert_safe_built_sidecars() {
  stage_phase=$1
  for built_sidecar in \
    "$repo_dir/target/$target/$profile_dir/woof_d" \
    "$repo_dir/target/$target/$profile_dir/woof-mcp"
  do
    if [ -L "$built_sidecar" ]; then
      echo "Refusing unsafe sidecar build path during $stage_phase: symlink output file $built_sidecar" >&2
      return 1
    fi
    if [ -e "$built_sidecar" ]; then
      if [ ! -f "$built_sidecar" ]; then
        echo "Refusing unsafe sidecar build path during $stage_phase: non-regular output file $built_sidecar" >&2
        return 1
      fi
      built_sidecar_links=$(/usr/bin/stat -f '%l' "$built_sidecar")
      if [ "$built_sidecar_links" -ne 1 ]; then
        echo "Refusing unsafe sidecar build path during $stage_phase: multiply-linked output file $built_sidecar" >&2
        return 1
      fi
    fi
  done
}

reset_release_profile_output() {
  [ "$profile" = release ] || return 0
  release_profile_dir="$repo_dir/target/$target/$profile_dir"
  case "$release_profile_dir" in
    "$repo_dir/target/aarch64-apple-darwin/release") ;;
    *)
      echo "Refusing unexpected release profile path: $release_profile_dir" >&2
      return 1
      ;;
  esac
  if [ -e "$release_profile_dir" ] || [ -L "$release_profile_dir" ]; then
    if [ -L "$release_profile_dir" ] || [ ! -d "$release_profile_dir" ]; then
      echo "Refusing unsafe release profile output: $release_profile_dir" >&2
      return 1
    fi
    /bin/rm -rf -- "$release_profile_dir"
  fi
  if [ -e "$release_profile_dir" ] || [ -L "$release_profile_dir" ]; then
    echo "Could not recreate a clean release profile output." >&2
    return 1
  fi
}

assert_safe_stage_directories "before sidecar build"
assert_safe_built_sidecars "before sidecar build"
reset_release_profile_output
assert_safe_stage_directories "after release profile reset"
cd "$repo_dir"
# cargo_args is a fixed value selected above, not user-provided shell input.
# shellcheck disable=SC2086
cargo build --locked --target "$target" $cargo_args -p woof_d -p woof-mcp
assert_safe_stage_directories "after sidecar build"
assert_safe_built_sidecars "after sidecar build"

for built_binary in \
  "$repo_dir/target/$target/$profile_dir/woof_d" \
  "$repo_dir/target/$target/$profile_dir/woof-mcp"
do
  if /usr/bin/strings -a "$built_binary" | /usr/bin/grep -Fq "$repo_dir" ||
    /usr/bin/strings -a "$built_binary" | /usr/bin/grep -Fq "$build_account_home"; then
    echo "Built sidecar contains a build-host path." >&2
    exit 1
  fi
  if [ -n "$repo_account_suffix" ] &&
    /usr/bin/strings -a "$built_binary" |
      /usr/bin/grep -Fq "rust-build-home/$repo_account_suffix"; then
    echo "Built sidecar used the broad path remap for a workspace source." >&2
    exit 1
  fi
done

assert_safe_stage_directories "before sidecar staging"
mkdir -p "$binary_dir"
assert_safe_stage_directories "after sidecar staging"
cp "$repo_dir/target/$target/$profile_dir/woof_d" \
  "$binary_dir/woof_d-$target"
assert_safe_stage_directories "between sidecar copies"
cp "$repo_dir/target/$target/$profile_dir/woof-mcp" \
  "$binary_dir/woof-mcp-$target"
assert_safe_stage_directories "after sidecar copies"
chmod 0755 "$binary_dir/woof_d-$target" "$binary_dir/woof-mcp-$target"

scripts/verify-code-identities.sh \
  "$binary_dir/woof_d-$target" \
  "$binary_dir/woof-mcp-$target"

echo "Staged $profile woof sidecars for $target."
