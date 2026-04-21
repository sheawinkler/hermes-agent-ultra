#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "${SCRIPT_DIR}/.." && pwd)}"
LOCK_FILE="${LOCK_FILE:-$HOME/.hermes/locks/upstream-sync.lock}"
LOG_TAG="[cron-upstream-sync]"
SYNC_STRATEGY="${SYNC_STRATEGY:-merge}"
REPORT_DIR="${REPORT_DIR:-${REPO_ROOT}/.sync-reports}"
CONFLICT_LABEL="${CONFLICT_LABEL:-upstream-sync-conflict}"
CREATE_CONFLICT_ISSUE="${CREATE_CONFLICT_ISSUE:-1}"

# Cron has a minimal PATH; include common tool locations explicitly.
export PATH="/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:${PATH:-}"

mkdir -p "$(dirname "${LOCK_FILE}")"
if command -v flock >/dev/null 2>&1; then
  exec 9>"${LOCK_FILE}"
  if ! flock -n 9; then
    echo "${LOG_TAG} lock busy; skipping overlapping run"
    exit 0
  fi
fi

if [[ ! -d "${REPO_ROOT}/.git" ]]; then
  echo "${LOG_TAG} repository not found at ${REPO_ROOT}"
  exit 1
fi

cd "${REPO_ROOT}"
ARGS=(
  --repo-root "${REPO_ROOT}"
  --mode branch-pr
  --strategy "${SYNC_STRATEGY}"
  --report-dir "${REPORT_DIR}"
  --conflict-label "${CONFLICT_LABEL}"
  --test-cmd "cargo test -p hermes-gateway"
)

if [[ "${CREATE_CONFLICT_ISSUE}" == "0" ]]; then
  ARGS+=(--no-conflict-issue)
fi

exec /usr/bin/env bash "${REPO_ROOT}/scripts/sync-upstream.sh" \
  "${ARGS[@]}"
