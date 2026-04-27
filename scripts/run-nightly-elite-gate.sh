#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/run-nightly-elite-gate.sh [options]

Run the nightly elite validation gate on local main.

Options:
  --repo-root <path>   Repository root (default: script parent)
  --log-dir <path>     Log directory (default: ~/.hermes-agent-ultra/logs/elite-gate)
  --skip-pull          Do not run `git pull --ff-only origin main`
  --allow-non-main     Allow execution outside `main` (for local testing)
  --allow-dirty        Allow execution with a dirty worktree (for local testing)
  -h, --help           Show help
USAGE
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
LOG_DIR="${HOME}/.hermes-agent-ultra/logs/elite-gate"
DO_PULL="1"
ALLOW_NON_MAIN="0"
ALLOW_DIRTY="0"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo-root)
      REPO_ROOT="${2:?missing value for --repo-root}"
      shift 2
      ;;
    --log-dir)
      LOG_DIR="${2:?missing value for --log-dir}"
      shift 2
      ;;
    --skip-pull)
      DO_PULL="0"
      shift
      ;;
    --allow-non-main)
      ALLOW_NON_MAIN="1"
      shift
      ;;
    --allow-dirty)
      ALLOW_DIRTY="1"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

mkdir -p "${LOG_DIR}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
LOG_PATH="${LOG_DIR}/nightly-elite-gate-${STAMP}.log"
SUMMARY_PATH="${LOG_DIR}/nightly-elite-gate-latest.json"

{
  echo "[elite-gate] started at $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "[elite-gate] repo=${REPO_ROOT}"
  cd "${REPO_ROOT}"

  branch="$(git rev-parse --abbrev-ref HEAD)"
  if [[ "${branch}" != "main" && "${ALLOW_NON_MAIN}" != "1" ]]; then
    echo "[elite-gate] error: expected main branch, got '${branch}'" >&2
    exit 10
  fi

  if [[ -n "$(git status --short)" && "${ALLOW_DIRTY}" != "1" ]]; then
    echo "[elite-gate] error: working tree is dirty; refusing nightly gate" >&2
    exit 11
  fi

  if [[ "${DO_PULL}" == "1" ]]; then
    git pull --ff-only origin main
  fi

  echo "[elite-gate] running cargo test -p hermes-cli"
  cargo test -p hermes-cli

  echo "[elite-gate] running deterministic replay suite"
  "${REPO_ROOT}/scripts/run-deterministic-replay-suite.sh"

  head_sha="$(git rev-parse HEAD)"
  cat > "${SUMMARY_PATH}" <<EOF
{
  "ok": true,
  "timestamp_utc": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "repo_root": "${REPO_ROOT}",
  "head_sha": "${head_sha}",
  "log_path": "${LOG_PATH}",
  "checks": [
    "cargo test -p hermes-cli",
    "scripts/run-deterministic-replay-suite.sh"
  ]
}
EOF
  echo "[elite-gate] complete"
  echo "[elite-gate] summary=${SUMMARY_PATH}"
} 2>&1 | tee "${LOG_PATH}"
