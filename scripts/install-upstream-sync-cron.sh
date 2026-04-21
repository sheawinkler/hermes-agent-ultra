#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "${SCRIPT_DIR}/.." && pwd)}"

SCHEDULE="${1:-17 */6 * * *}"
LOG_FILE="${LOG_FILE:-$HOME/.hermes/logs/upstream-sync.log}"
MARKER="# hermes-upstream-sync"
CRON_SYNC_SCRIPT="${REPO_ROOT}/scripts/cron-upstream-sync.sh"
SYNC_STRATEGY="${SYNC_STRATEGY:-merge}"
REPORT_DIR="${REPORT_DIR:-${REPO_ROOT}/.sync-reports}"
CONFLICT_LABEL="${CONFLICT_LABEL:-upstream-sync-conflict}"
CREATE_CONFLICT_ISSUE="${CREATE_CONFLICT_ISSUE:-1}"
STRICT_RISK_GATE="${STRICT_RISK_GATE:-1}"
ALLOW_RISK_PATHS="${ALLOW_RISK_PATHS:-0}"
RISK_PATHS_FILE="${RISK_PATHS_FILE:-${REPO_ROOT}/scripts/upstream-risk-paths.txt}"

if [[ ! -f "${CRON_SYNC_SCRIPT}" ]]; then
  echo "Missing script: ${CRON_SYNC_SCRIPT}" >&2
  exit 1
fi
if ! command -v crontab >/dev/null 2>&1; then
  echo "crontab command not found" >&2
  exit 1
fi

mkdir -p "$(dirname "${LOG_FILE}")"
chmod +x "${CRON_SYNC_SCRIPT}" || true

ENTRY="${SCHEDULE} REPO_ROOT='${REPO_ROOT}' SYNC_STRATEGY='${SYNC_STRATEGY}' REPORT_DIR='${REPORT_DIR}' CONFLICT_LABEL='${CONFLICT_LABEL}' CREATE_CONFLICT_ISSUE='${CREATE_CONFLICT_ISSUE}' STRICT_RISK_GATE='${STRICT_RISK_GATE}' ALLOW_RISK_PATHS='${ALLOW_RISK_PATHS}' RISK_PATHS_FILE='${RISK_PATHS_FILE}' /usr/bin/env bash '${CRON_SYNC_SCRIPT}' >> '${LOG_FILE}' 2>&1 ${MARKER}"

TMP_CURRENT="$(mktemp)"
TMP_NEW="$(mktemp)"
trap 'rm -f "${TMP_CURRENT}" "${TMP_NEW}"' EXIT

crontab -l 2>/dev/null | grep -v "${MARKER}" > "${TMP_CURRENT}" || true
cat "${TMP_CURRENT}" > "${TMP_NEW}"
printf '%s\n' "${ENTRY}" >> "${TMP_NEW}"
crontab "${TMP_NEW}"

echo "Installed upstream sync cron entry:"
echo "  ${ENTRY}"
echo
echo "Inspect with: crontab -l | grep hermes-upstream-sync"
echo "Logs: ${LOG_FILE}"
