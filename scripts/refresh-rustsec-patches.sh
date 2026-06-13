#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  scripts/refresh-rustsec-patches.sh check [all|crate-name]
  scripts/refresh-rustsec-patches.sh refresh <crate-name> [version]
  scripts/refresh-rustsec-patches.sh list

Downloads the configured crates.io crate tarball, reapplies the local RustSec
patch file, and either checks patched files against the repository copy or
refreshes third_party/rustsec-patches/<crate>-<version>.

Resolution workflow:
  1. Try a newer official crate version with refresh <crate> <version>.
  2. Update Cargo.toml [patch.crates-io] to the refreshed directory if still needed.
  3. If the official crate no longer needs the local patch, remove the patch entry
     instead and rerun cargo audit/check/test/build gates.
USAGE
}

repo_root() {
  git rev-parse --show-toplevel
}

patch_root() {
  printf '%s/third_party/rustsec-patches' "$(repo_root)"
}

patch_spec() {
  case "$1" in
    matrix-pickle-derive)
      printf '%s\t%s\t%s\n' \
        "0.2.2" \
        "matrix-pickle-derive-0.2.2" \
        "patches/matrix-pickle-derive-0.2.2-remove-proc-macro-error2.patch"
      ;;
    matrix-sdk-crypto)
      printf '%s\t%s\t%s\n' \
        "0.18.0" \
        "matrix-sdk-crypto-0.18.0" \
        "patches/matrix-sdk-crypto-0.18.0-remove-doc-aquamarine.patch"
      ;;
    tungstenite)
      printf '%s\t%s\t%s\n' \
        "0.29.0" \
        "tungstenite-0.29.0" \
        "patches/tungstenite-0.29.0-use-getrandom.patch"
      ;;
    vodozemac)
      printf '%s\t%s\t%s\n' \
        "0.10.0" \
        "vodozemac-0.10.0" \
        "patches/vodozemac-0.10.0-use-rand-core-osrng.patch"
      ;;
    *)
      return 1
      ;;
  esac
}

known_crates() {
  printf '%s\n' matrix-pickle-derive matrix-sdk-crypto tungstenite vodozemac
}

download_and_patch() {
  local crate=$1
  local version=$2
  local patch_file=$3
  local workdir=$4
  local archive="$workdir/$crate-$version.crate"

  curl \
    -fsSL \
    -A 'hermes-agent-ultra-rustsec-patch-refresh/1.0' \
    -H 'Accept: application/octet-stream' \
    "https://crates.io/api/v1/crates/$crate/$version/download" \
    -o "$archive"
  tar -xzf "$archive" -C "$workdir"
  patch -p1 -d "$workdir/$crate-$version" < "$patch_file"
}

patched_files() {
  awk '/^\+\+\+ b\// { sub(/^\+\+\+ b\//, ""); print }' "$1" | sort -u
}

check_one() {
  local crate=$1
  local configured_version configured_dir patch_rel
  IFS=$'\t' read -r configured_version configured_dir patch_rel < <(patch_spec "$crate")

  local patch_dir patch_file tmp
  patch_dir=$(patch_root)
  patch_file="$patch_dir/$patch_rel"
  tmp=$(mktemp -d)

  (
    trap 'rm -rf "$tmp"' EXIT
    download_and_patch "$crate" "$configured_version" "$patch_file" "$tmp"

    local file
    while IFS= read -r file; do
      diff -u "$tmp/$crate-$configured_version/$file" "$patch_dir/$configured_dir/$file" >/dev/null
    done < <(patched_files "$patch_file")
  )

  printf 'ok %s %s\n' "$crate" "$configured_version"
}

prune_non_runtime_artifacts() {
  local dir=$1
  rm -rf \
    "$dir/.cargo-ok" \
    "$dir/.cargo_vcs_info.json" \
    "$dir/Cargo.lock" \
    "$dir/Cargo.toml.orig" \
    "$dir/.cargo" \
    "$dir/.config" \
    "$dir/.github" \
    "$dir/.vscode" \
    "$dir/afl" \
    "$dir/benches" \
    "$dir/examples" \
    "$dir/contrib"
  find "$dir" -type d -name snapshots -prune -exec rm -rf {} +
}

refresh_one() {
  local crate=$1
  local requested_version=${2:-}
  local configured_version configured_dir patch_rel
  IFS=$'\t' read -r configured_version configured_dir patch_rel < <(patch_spec "$crate")

  local version=${requested_version:-$configured_version}
  local patch_dir patch_file tmp src dest
  patch_dir=$(patch_root)
  patch_file="$patch_dir/$patch_rel"
  tmp=$(mktemp -d)
  src="$tmp/$crate-$version"
  dest="$patch_dir/$crate-$version"

  (
    trap 'rm -rf "$tmp"' EXIT
    download_and_patch "$crate" "$version" "$patch_file" "$tmp"

    prune_non_runtime_artifacts "$src"
    rm -rf "$dest"
    mkdir -p "$dest"
    cp -R "$src/." "$dest/"
  )

  printf 'refreshed %s %s -> %s\n' "$crate" "$version" "$dest"
  if [[ "$version" != "$configured_version" ]]; then
    printf 'note: update Cargo.toml [patch.crates-io] from %s to %s before verifying the workspace\n' \
      "$configured_dir" "$crate-$version"
  fi
}

main() {
  local command=${1:-}
  case "$command" in
    list)
      known_crates
      ;;
    check)
      local target=${2:-all}
      if [[ "$target" == all ]]; then
        local crate
        while IFS= read -r crate; do
          check_one "$crate"
        done < <(known_crates)
      else
        patch_spec "$target" >/dev/null || {
          printf 'unknown crate: %s\n' "$target" >&2
          exit 2
        }
        check_one "$target"
      fi
      ;;
    refresh)
      local crate=${2:-}
      [[ -n "$crate" ]] || {
        usage >&2
        exit 2
      }
      patch_spec "$crate" >/dev/null || {
        printf 'unknown crate: %s\n' "$crate" >&2
        exit 2
      }
      refresh_one "$crate" "${3:-}"
      ;;
    -h|--help|help|"")
      usage
      ;;
    *)
      printf 'unknown command: %s\n' "$command" >&2
      usage >&2
      exit 2
      ;;
  esac
}

main "$@"
