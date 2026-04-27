#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/status-elite-gate-launchd.sh [options]

Show launchd status + logs for nightly elite gate.

Options:
  --label-prefix <prefix> launchd label prefix (default: com.hermes_agent_ultra)
  --log-dir <path>        Log directory (default: ~/.hermes-agent-ultra/logs)
  --tail <n>              Tail count for logs (default: 30)
  --verbose               Print full launchctl record
  -h, --help              Show help
USAGE
}

LABEL_PREFIX="com.hermes_agent_ultra"
LOG_DIR="${HOME}/.hermes-agent-ultra/logs"
TAIL_N=30
VERBOSE="0"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --label-prefix)
      LABEL_PREFIX="${2:?missing value for --label-prefix}"
      shift 2
      ;;
    --log-dir)
      LOG_DIR="${2:?missing value for --log-dir}"
      shift 2
      ;;
    --tail)
      TAIL_N="${2:?missing value for --tail}"
      shift 2
      ;;
    --verbose)
      VERBOSE="1"
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

LABEL="${LABEL_PREFIX}.nightly_elite_gate"
REF="gui/${UID}/${LABEL}"
TMP="/tmp/.elite_gate_launchd_status.$$"

if launchctl print "${REF}" >"${TMP}" 2>/dev/null; then
  echo "[loaded] ${LABEL}"
  pid="$(grep -E '^\s*pid = ' "${TMP}" | head -n 1 | sed -E 's/.*pid = ([0-9]+).*/\1/' || true)"
  if [[ -n "${pid}" ]]; then
    echo "  pid: ${pid}"
  fi
  if [[ "${VERBOSE}" == "1" ]]; then
    sed -n '1,140p' "${TMP}"
  fi
else
  echo "[not loaded] ${LABEL}"
fi
rm -f "${TMP}" >/dev/null 2>&1 || true

for f in \
  "${LOG_DIR}/elite-gate-launchd.out.log" \
  "${LOG_DIR}/elite-gate-launchd.err.log" \
  "${LOG_DIR}/elite-gate/nightly-elite-gate-latest.json"; do
  echo
  echo "=== ${f} ==="
  if [[ -f "${f}" ]]; then
    if [[ "${f}" == *.json ]]; then
      cat "${f}"
    else
      tail -n "${TAIL_N}" "${f}"
    fi
  else
    echo "(missing)"
  fi
done

