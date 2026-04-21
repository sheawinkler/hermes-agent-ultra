#!/usr/bin/env bash
set -euo pipefail

ALLOWLIST_FILE=".ci/clippy-allowlist.txt"
MODE="check"

usage() {
  cat <<'EOF'
Usage: scripts/clippy-warning-gate.sh [options] [-- <extra cargo args>]

Runs cargo clippy and enforces a warning allowlist.

Options:
  --allowlist <path>   Path to allowlist file (default: .ci/clippy-allowlist.txt)
  --update             Refresh allowlist from current clippy warnings
  --check              Validate current warnings against allowlist (default)
  -h, --help           Show help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --allowlist)
      ALLOWLIST_FILE="${2:?missing value for --allowlist}"
      shift 2
      ;;
    --update)
      MODE="update"
      shift
      ;;
    --check)
      MODE="check"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --)
      shift
      break
      ;;
    *)
      break
      ;;
  esac
done

EXTRA_ARGS=("$@")

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

tmp_json="$(mktemp)"
tmp_seen="$(mktemp)"
tmp_allow="$(mktemp)"
tmp_new="$(mktemp)"
tmp_stale="$(mktemp)"
trap 'rm -f "${tmp_json}" "${tmp_seen}" "${tmp_allow}" "${tmp_new}" "${tmp_stale}"' EXIT

echo "[clippy-gate] collecting warnings..."
cargo clippy --workspace --all-targets --message-format=json "${EXTRA_ARGS[@]}" > "${tmp_json}"

jq -r '
  select(.reason == "compiler-message")
  | . as $root
  | .message as $m
  | select($m.level == "warning")
  | ($root.target.name // "unknown_target") as $target
  | (
      $m.spans
      | map(select(.is_primary == true))
      | .[0] // ($m.spans[0] // {})
    ) as $span
  | ($span.file_name // "unknown_file") as $file
  | ($m.code.code // ("rustc:" + ($m.message | split(":")[0]))) as $lint
  | "\($target)|\($lint)|\($file)"
' "${tmp_json}" | LC_ALL=C sort -u > "${tmp_seen}"

if [[ "${MODE}" == "update" ]]; then
  mkdir -p "$(dirname "${ALLOWLIST_FILE}")"
  cp "${tmp_seen}" "${ALLOWLIST_FILE}"
  count="$(wc -l < "${ALLOWLIST_FILE}" | tr -d ' ')"
  echo "[clippy-gate] updated ${ALLOWLIST_FILE} (${count} entries)"
  exit 0
fi

if [[ ! -f "${ALLOWLIST_FILE}" ]]; then
  echo "[clippy-gate] allowlist not found: ${ALLOWLIST_FILE}" >&2
  echo "[clippy-gate] run: scripts/clippy-warning-gate.sh --update" >&2
  exit 1
fi

grep -vE '^\s*(#|$)' "${ALLOWLIST_FILE}" | LC_ALL=C sort -u > "${tmp_allow}"

comm -23 "${tmp_seen}" "${tmp_allow}" > "${tmp_new}"
comm -13 "${tmp_seen}" "${tmp_allow}" > "${tmp_stale}"

seen_count="$(wc -l < "${tmp_seen}" | tr -d ' ')"
allow_count="$(wc -l < "${tmp_allow}" | tr -d ' ')"
new_count="$(wc -l < "${tmp_new}" | tr -d ' ')"
stale_count="$(wc -l < "${tmp_stale}" | tr -d ' ')"

echo "[clippy-gate] seen=${seen_count} allowlist=${allow_count} new=${new_count} stale=${stale_count}"

if [[ "${new_count}" -gt 0 ]]; then
  echo "[clippy-gate] new warnings detected:" >&2
  sed 's/^/  + /' "${tmp_new}" >&2
  exit 1
fi

if [[ "${stale_count}" -gt 0 ]]; then
  echo "[clippy-gate] stale allowlist entries (safe to prune via --update):"
  sed 's/^/  - /' "${tmp_stale}"
fi

echo "[clippy-gate] pass"
