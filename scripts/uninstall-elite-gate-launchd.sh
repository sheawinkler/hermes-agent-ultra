#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/uninstall-elite-gate-launchd.sh [options]

Unload and remove nightly elite gate launchd agent.

Options:
  --label-prefix <prefix> launchd label prefix (default: com.hermes_agent_ultra)
  --agents-dir <path>     LaunchAgents directory (default: ~/Library/LaunchAgents)
  -h, --help              Show help
USAGE
}

LABEL_PREFIX="com.hermes_agent_ultra"
AGENTS_DIR="${HOME}/Library/LaunchAgents"

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
PLIST_PATH="${AGENTS_DIR}/${LABEL}.plist"

launchctl bootout "gui/${UID}" "${PLIST_PATH}" >/dev/null 2>&1 || true
rm -f "${PLIST_PATH}"

echo "Removed: ${PLIST_PATH}"

