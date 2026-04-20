#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "${SCRIPT_DIR}/.." && pwd)}"

SCHEDULE="${1:-17 */6 * * *}"
LOG_FILE="${LOG_FILE:-$HOME/.hermes/logs/upstream-sync.log}"
MARKER="# hermes-upstream-sync"
CRON_SYNC_SCRIPT="${REPO_ROOT}/scripts/cron-upstream-sync.sh"

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

ENTRY="${SCHEDULE} REPO_ROOT='${REPO_ROOT}' /usr/bin/env bash '${CRON_SYNC_SCRIPT}' >> '${LOG_FILE}' 2>&1 ${MARKER}"

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
