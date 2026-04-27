#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/install-elite-gate-launchd.sh [options]

Install/update a launchd user agent that runs the nightly elite gate.

Options:
  --repo-root <path>      Repository root (default: script parent)
  --label-prefix <prefix> launchd label prefix (default: com.hermes_agent_ultra)
  --agents-dir <path>     LaunchAgents directory (default: ~/Library/LaunchAgents)
  --log-dir <path>        Log directory (default: ~/.hermes-agent-ultra/logs)
  --hour <0-23>           Run hour (default: 3)
  --minute <0-59>         Run minute (default: 30)
  --no-load               Only write plist, do not load
  -h, --help              Show help
USAGE
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
LABEL_PREFIX="com.hermes_agent_ultra"
AGENTS_DIR="${HOME}/Library/LaunchAgents"
LOG_DIR="${HOME}/.hermes-agent-ultra/logs"
HOUR="3"
MINUTE="30"
LOAD_SERVICE="1"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo-root)
      REPO_ROOT="${2:?missing value for --repo-root}"
      shift 2
      ;;
    --label-prefix)
      LABEL_PREFIX="${2:?missing value for --label-prefix}"
      shift 2
      ;;
    --agents-dir)
      AGENTS_DIR="${2:?missing value for --agents-dir}"
      shift 2
      ;;
    --log-dir)
      LOG_DIR="${2:?missing value for --log-dir}"
      shift 2
      ;;
    --hour)
      HOUR="${2:?missing value for --hour}"
      shift 2
      ;;
    --minute)
      MINUTE="${2:?missing value for --minute}"
      shift 2
      ;;
    --no-load)
      LOAD_SERVICE="0"
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

if ! [[ "${HOUR}" =~ ^[0-9]+$ ]] || (( HOUR < 0 || HOUR > 23 )); then
  echo "Invalid --hour: ${HOUR}" >&2
  exit 2
fi
if ! [[ "${MINUTE}" =~ ^[0-9]+$ ]] || (( MINUTE < 0 || MINUTE > 59 )); then
  echo "Invalid --minute: ${MINUTE}" >&2
  exit 2
fi

LABEL="${LABEL_PREFIX}.nightly_elite_gate"
PLIST_PATH="${AGENTS_DIR}/${LABEL}.plist"
RUNNER="${REPO_ROOT}/scripts/run-nightly-elite-gate.sh"

mkdir -p "${AGENTS_DIR}" "${LOG_DIR}" "${LOG_DIR}/elite-gate"
chmod +x "${RUNNER}" "${REPO_ROOT}/scripts/run-deterministic-replay-suite.sh"

cat > "${PLIST_PATH}" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>${LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/bin/env</string>
    <string>bash</string>
    <string>${RUNNER}</string>
    <string>--repo-root</string>
    <string>${REPO_ROOT}</string>
    <string>--log-dir</string>
    <string>${LOG_DIR}/elite-gate</string>
  </array>
  <key>WorkingDirectory</key>
  <string>${REPO_ROOT}</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
  </dict>
  <key>StartCalendarInterval</key>
  <dict>
    <key>Hour</key>
    <integer>${HOUR}</integer>
    <key>Minute</key>
    <integer>${MINUTE}</integer>
  </dict>
  <key>StandardOutPath</key>
  <string>${LOG_DIR}/elite-gate-launchd.out.log</string>
  <key>StandardErrorPath</key>
  <string>${LOG_DIR}/elite-gate-launchd.err.log</string>
</dict>
</plist>
EOF

if [[ "${LOAD_SERVICE}" == "1" ]]; then
  launchctl bootout "gui/${UID}" "${PLIST_PATH}" >/dev/null 2>&1 || true
  launchctl bootstrap "gui/${UID}" "${PLIST_PATH}"
fi

echo "Installed: ${PLIST_PATH}"
echo "Label: ${LABEL}"
echo "Schedule: daily at $(printf "%02d:%02d" "${HOUR}" "${MINUTE}") local time"
if [[ "${LOAD_SERVICE}" == "1" ]]; then
  echo "Service loaded."
else
  echo "Service not loaded (--no-load)."
fi

