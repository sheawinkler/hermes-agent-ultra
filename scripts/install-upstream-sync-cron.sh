#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

SCHEDULE="${1:-17 */6 * * *}"
LOG_FILE="${LOG_FILE:-$HOME/.hermes/logs/upstream-sync.log}"
MARKER="# hermes-upstream-sync"

mkdir -p "$(dirname "${LOG_FILE}")"

ENTRY="${SCHEDULE} /usr/bin/env bash '${REPO_ROOT}/scripts/cron-upstream-sync.sh' >> '${LOG_FILE}' 2>&1 ${MARKER}"

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
