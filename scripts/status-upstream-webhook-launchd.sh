#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/status-upstream-webhook-launchd.sh [options]

Show launchd status for upstream webhook listener + worker.

Options:
  --label-prefix <prefix> launchd label prefix (default: com.hermes_agent_ultra)
  --log-dir <path>        Log directory (default: ~/.hermes-agent-ultra/logs)
  --tail <n>              Tail last n lines from each log (default: 20)
  --verbose               Print full launchctl service records
  -h, --help              Show help
USAGE
}

LABEL_PREFIX="com.hermes_agent_ultra"
LOG_DIR="${HOME}/.hermes-agent-ultra/logs"
TAIL_N=20
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

LISTENER_LABEL="${LABEL_PREFIX}.upstream_webhook_listener"
WORKER_LABEL="${LABEL_PREFIX}.upstream_webhook_worker"

print_service_status() {
  local label="$1"
  local ref="gui/${UID}/${label}"
  if launchctl print "${ref}" >/tmp/.launchd_status.$$ 2>/dev/null; then
    echo "[loaded] ${label}"
    local pid
    pid="$(grep -E '^\s*pid = ' /tmp/.launchd_status.$$ | head -n 1 | sed -E 's/.*pid = ([0-9]+).*/\1/' || true)"
    if [[ -n "${pid}" ]]; then
      echo "  pid: ${pid}"
    fi
    if [[ "${VERBOSE}" == "1" ]]; then
      sed -n '1,120p' /tmp/.launchd_status.$$
    fi
  else
    echo "[not loaded] ${label}"
  fi
  rm -f /tmp/.launchd_status.$$ >/dev/null 2>&1 || true
}

echo "launchd service status:"
print_service_status "${LISTENER_LABEL}"
print_service_status "${WORKER_LABEL}"
echo

for log_file in \
  "${LOG_DIR}/upstream-webhook-listener.out.log" \
  "${LOG_DIR}/upstream-webhook-listener.err.log" \
  "${LOG_DIR}/upstream-webhook-worker.out.log" \
  "${LOG_DIR}/upstream-webhook-worker.err.log"; do
  echo "=== ${log_file} ==="
  if [[ -f "${log_file}" ]]; then
    tail -n "${TAIL_N}" "${log_file}"
  else
    echo "(missing)"
  fi
done
