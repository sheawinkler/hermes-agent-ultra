#!/bin/sh
set -eu

LABEL="app.terra.http"
SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
PLIST_SRC="${SCRIPT_DIR}/app.terra.http.plist"
PLIST_DEST="${HOME}/Library/LaunchAgents/${LABEL}.plist"
LOG_DIR="${HOME}/Library/Logs/Terra"

mkdir -p "${HOME}/Library/LaunchAgents" "${LOG_DIR}"

if command -v hermes-http >/dev/null 2>&1; then
  HTTP_BIN="$(command -v hermes-http)"
elif [ -x "/Applications/Terra Desktop Community.app/Contents/MacOS/hermes-http" ]; then
  HTTP_BIN="/Applications/Terra Desktop Community.app/Contents/MacOS/hermes-http"
else
  echo "hermes-http binary not found; skipping LaunchAgent install" >&2
  exit 0
fi

sed "s|/usr/local/bin/hermes-http|${HTTP_BIN}|g" "${PLIST_SRC}" > "${PLIST_DEST}"
chmod 644 "${PLIST_DEST}"

UID_NUM="$(id -u)"
launchctl bootout "gui/${UID_NUM}/${LABEL}" 2>/dev/null || true
launchctl bootstrap "gui/${UID_NUM}" "${PLIST_DEST}"
launchctl enable "gui/${UID_NUM}/${LABEL}" 2>/dev/null || true
launchctl kickstart -k "gui/${UID_NUM}/${LABEL}" 2>/dev/null || true

echo "Installed ${LABEL} -> ${HTTP_BIN}"
