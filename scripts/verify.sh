#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_dir"

sidecars_pre_staged=0
case "$#" in
  0) ;;
  1)
    if [ "$1" != "--sidecars-pre-staged" ]; then
      echo "usage: scripts/verify.sh [--sidecars-pre-staged]" >&2
      exit 64
    fi
    sidecars_pre_staged=1
    ;;
  *)
    echo "usage: scripts/verify.sh [--sidecars-pre-staged]" >&2
    exit 64
    ;;
esac

verify_release_output_paths() (
  release_path_fixture=$(/usr/bin/mktemp -d /private/tmp/woof-release-paths.XXXXXX)
  case "$release_path_fixture" in
    /private/tmp/woof-release-paths.*) ;;
    *)
      echo "Refusing unexpected release-path fixture: $release_path_fixture" >&2
      exit 1
      ;;
  esac
  [ -d "$release_path_fixture" ] && [ ! -L "$release_path_fixture" ] || {
    echo "Release-path fixture is not a private directory." >&2
    exit 1
  }
  cleanup_release_path_fixture() {
    if [ ! -e "$release_path_fixture" ] && [ ! -L "$release_path_fixture" ]; then
      return
    fi
    case "$release_path_fixture" in
      /private/tmp/woof-release-paths.*) ;;
      *)
        echo "Refusing unsafe release-path fixture cleanup: $release_path_fixture" >&2
        return 1
        ;;
    esac
    [ -d "$release_path_fixture" ] && [ ! -L "$release_path_fixture" ] || {
      echo "Refusing unsafe release-path fixture cleanup: $release_path_fixture" >&2
      return 1
    }
    /bin/rm -rf -- "$release_path_fixture"
  }
  trap cleanup_release_path_fixture EXIT
  trap 'cleanup_release_path_fixture; exit 1' HUP INT TERM

  safe_repo="$release_path_fixture/safe/repo"
  target_link_repo="$release_path_fixture/target-link/repo"
  artifacts_link_repo="$release_path_fixture/artifacts-link/repo"
  source_link_repo="$release_path_fixture/source-link/repo"
  hardlink_repo="$release_path_fixture/hardlink/repo"
  non_directory_repo="$release_path_fixture/non-directory/repo"
  for fixture_repo in \
    "$safe_repo" \
    "$target_link_repo" \
    "$artifacts_link_repo" \
    "$source_link_repo" \
    "$hardlink_repo" \
    "$non_directory_repo"
  do
    /bin/mkdir -p "$fixture_repo/scripts"
    /bin/cp "$repo_dir/scripts/build-release.sh" "$fixture_repo/scripts/build-release.sh"
    /bin/chmod 700 "$fixture_repo/scripts/build-release.sh"
  done

  /bin/mkdir -p \
    "$safe_repo/artifacts/release" \
    "$safe_repo/target/aarch64-apple-darwin/release/bundle/macos/woof.app" \
    "$safe_repo/node_modules" \
    "$safe_repo/assets/generated/woof-app.iconset" \
    "$safe_repo/apps/woof/node_modules" \
    "$safe_repo/apps/woof/.svelte-kit" \
    "$safe_repo/apps/woof/build" \
    "$safe_repo/apps/woof/static/mascot" \
    "$safe_repo/apps/woof/src-tauri/binaries" \
    "$safe_repo/apps/woof/src-tauri/icons" \
    "$safe_repo/apps/woof/src-tauri/target"
  "$safe_repo/scripts/build-release.sh" --verify-output-paths >/dev/null

  outside_target="$release_path_fixture/target-link/outside"
  /bin/mkdir -p \
    "$target_link_repo/target/aarch64-apple-darwin/release/bundle" \
    "$outside_target/woof.app"
  /usr/bin/touch "$outside_target/woof.app/sentinel"
  /bin/ln -s "$outside_target" \
    "$target_link_repo/target/aarch64-apple-darwin/release/bundle/macos"
  if "$target_link_repo/scripts/build-release.sh" --verify-output-paths \
    >"$release_path_fixture/target-link.stdout" \
    2>"$release_path_fixture/target-link.stderr"; then
    echo "Release path guard accepted an intermediate target symlink." >&2
    exit 1
  fi
  /usr/bin/grep -Fq \
    "symlink component $target_link_repo/target/aarch64-apple-darwin/release/bundle/macos" \
    "$release_path_fixture/target-link.stderr"
  [ -f "$outside_target/woof.app/sentinel" ] || {
    echo "Release path rejection modified the symlink target." >&2
    exit 1
  }

  outside_artifacts="$release_path_fixture/artifacts-link/outside"
  /bin/mkdir -p "$outside_artifacts"
  /bin/ln -s "$outside_artifacts" "$artifacts_link_repo/artifacts"
  if "$artifacts_link_repo/scripts/build-release.sh" --verify-output-paths \
    >"$release_path_fixture/artifacts-link.stdout" \
    2>"$release_path_fixture/artifacts-link.stderr"; then
    echo "Release path guard accepted an artifacts symlink." >&2
    exit 1
  fi
  /usr/bin/grep -Fq \
    "symlink component $artifacts_link_repo/artifacts" \
    "$release_path_fixture/artifacts-link.stderr"
  [ ! -e "$outside_artifacts/release" ] || {
    echo "Release path rejection created a workspace through the artifacts symlink." >&2
    exit 1
  }

  outside_source="$release_path_fixture/source-link/outside"
  /bin/mkdir -p \
    "$source_link_repo/assets/generated/woof-app.iconset" \
    "$outside_source"
  printf '%s\n' 'outside sentinel' >"$outside_source/sentinel"
  /bin/ln -s "$outside_source/sentinel" \
    "$source_link_repo/assets/generated/woof-app.iconset/icon_16x16.png"
  if "$source_link_repo/scripts/build-release.sh" --verify-output-paths \
    >"$release_path_fixture/source-link.stdout" \
    2>"$release_path_fixture/source-link.stderr"; then
    echo "Release path guard accepted a generated-icon symlink." >&2
    exit 1
  fi
  /usr/bin/grep -Fq \
    "unsafe output entry $source_link_repo/assets/generated/woof-app.iconset/icon_16x16.png" \
    "$release_path_fixture/source-link.stderr"
  [ "$(/bin/cat "$outside_source/sentinel")" = 'outside sentinel' ] || {
    echo "Release path rejection modified the generated-icon symlink target." >&2
    exit 1
  }

  outside_hardlink="$release_path_fixture/hardlink/outside"
  /bin/mkdir -p \
    "$hardlink_repo/assets/generated/woof-app.iconset" \
    "$outside_hardlink"
  printf '%s\n' 'outside hardlink sentinel' >"$outside_hardlink/sentinel"
  /bin/ln "$outside_hardlink/sentinel" \
    "$hardlink_repo/assets/generated/woof-app.iconset/icon_32x32.png"
  if "$hardlink_repo/scripts/build-release.sh" --verify-output-paths \
    >"$release_path_fixture/hardlink.stdout" \
    2>"$release_path_fixture/hardlink.stderr"; then
    echo "Release path guard accepted a multiply-linked output file." >&2
    exit 1
  fi
  /usr/bin/grep -Fq \
    "multiply-linked output file $hardlink_repo/assets/generated/woof-app.iconset/icon_32x32.png" \
    "$release_path_fixture/hardlink.stderr"
  [ "$(/bin/cat "$outside_hardlink/sentinel")" = 'outside hardlink sentinel' ] || {
    echo "Release path rejection modified the hard-linked outside file." >&2
    exit 1
  }

  /bin/mkdir -p \
    "$non_directory_repo/target/aarch64-apple-darwin/release"
  /usr/bin/touch \
    "$non_directory_repo/target/aarch64-apple-darwin/release/bundle"
  if "$non_directory_repo/scripts/build-release.sh" --verify-output-paths \
    >"$release_path_fixture/non-directory.stdout" \
    2>"$release_path_fixture/non-directory.stderr"; then
    echo "Release path guard accepted a non-directory ancestor." >&2
    exit 1
  fi
  /usr/bin/grep -Fq \
    "non-directory component $non_directory_repo/target/aarch64-apple-darwin/release/bundle" \
    "$release_path_fixture/non-directory.stderr"
)

verify_release_output_paths
node scripts/audit-zero-remnants.mjs self-test
node scripts/audit-zero-remnants.mjs source "$repo_dir"
cargo fmt --all --check
cargo metadata --locked --no-deps --format-version 1 >/dev/null
if [ "$sidecars_pre_staged" -eq 0 ]; then
  scripts/stage-sidecars.sh debug
fi
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
scripts/verify-code-identities.sh \
  apps/woof/src-tauri/binaries/woof_d-aarch64-apple-darwin \
  apps/woof/src-tauri/binaries/woof-mcp-aarch64-apple-darwin
npm run check
npm test
npm run build --workspace apps/woof
node scripts/audit-zero-remnants.mjs tree \
  apps/woof/build \
  apps/woof/.svelte-kit/output
find docs/contracts -name '*.json' -type f -exec jq empty {} +
node scripts/audit-runtime-boundary.mjs

echo "woof source verification passed."
