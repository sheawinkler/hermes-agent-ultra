#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/uninstall-upstream-webhook-launchd.sh [options]

Unload and remove launchd user agents for upstream webhook sync.

Options:
  --label-prefix <prefix> launchd label prefix (default: com.hermes_agent_ultra)
  --agents-dir <path>     LaunchAgents directory (default: ~/Library/LaunchAgents)
  --env-file <path>       Env file to optionally remove
  --purge-env             Remove env file
  -h, --help              Show help
USAGE
}

LABEL_PREFIX="com.hermes_agent_ultra"
AGENTS_DIR="${HOME}/Library/LaunchAgents"
ENV_FILE="${HOME}/.hermes-agent-ultra/upstream-webhook-sync.env"
PURGE_ENV="0"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --label-prefix)
      LABEL_PREFIX="${2:?missing value for --label-prefix}"
      shift 2
      ;;
    --agents-dir)
      AGENTS_DIR="${2:?missing value for --agents-dir}"
      shift 2
      ;;
    --env-file)
      ENV_FILE="${2:?missing value for --env-file}"
      shift 2
      ;;
    --purge-env)
      PURGE_ENV="1"
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
LISTENER_PLIST="${AGENTS_DIR}/${LISTENER_LABEL}.plist"
WORKER_PLIST="${AGENTS_DIR}/${WORKER_LABEL}.plist"

launchctl bootout "gui/${UID}" "${LISTENER_PLIST}" >/dev/null 2>&1 || true
launchctl bootout "gui/${UID}" "${WORKER_PLIST}" >/dev/null 2>&1 || true

rm -f "${LISTENER_PLIST}" "${WORKER_PLIST}"

if [[ "${PURGE_ENV}" == "1" ]]; then
  rm -f "${ENV_FILE}"
fi

echo "Removed launchd agents:"
echo "  ${LISTENER_PLIST}"
echo "  ${WORKER_PLIST}"
if [[ "${PURGE_ENV}" == "1" ]]; then
  echo "Removed env file: ${ENV_FILE}"
else
  echo "Retained env file: ${ENV_FILE}"
fi
