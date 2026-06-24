#!/usr/bin/env sh
# Desktop sherpa launcher (Linux / macOS): init Hermes home + talk config/models, then run.
set -eu

DIR="$(cd "$(dirname "$0")" && pwd)"
USER_HOME="${HOME:-$(cd ~ 2>/dev/null && pwd || echo "/tmp")}"
export HERMES_HOME="${HERMES_HOME:-${USER_HOME}/.hermes-agent-ultra}"
export HERMES_TALK_BUNDLE_DIR="${DIR}"
TALK_HOME="${HERMES_HOME}/hermes-talk"
CONFIG_EXAMPLE="${DIR}/config.example.toml"
HERMES_CONFIG_EXAMPLE="${DIR}/config.example.yaml"
HERMES_CONFIG="${HERMES_HOME}/config.yaml"
BIN="${DIR}/bin/hermes-agent-ultra"

init_hermes_home() {
  mkdir -p \
    "${HERMES_HOME}" \
    "${HERMES_HOME}/profiles" \
    "${HERMES_HOME}/sessions" \
    "${HERMES_HOME}/logs" \
    "${HERMES_HOME}/skills" \
    "${HERMES_HOME}/cron" \
    "${HERMES_HOME}/cache" \
    "${TALK_HOME}"
}

install_bundle_tree() {
  item="$1"
  if [ ! -e "${DIR}/${item}" ]; then
    return 0
  fi
  dst="${TALK_HOME}/${item}"
  if [ -d "${dst}" ] && [ ! -L "${dst}" ]; then
    rm -rf "${dst}"
  fi
  ln -sfn "${DIR}/${item}" "${dst}"
}

init_talk_assets() {
  for item in models; do
    install_bundle_tree "${item}"
  done
}

needs_default_config() {
  [ ! -f "${TALK_HOME}/config.toml" ]
}

needs_hermes_config() {
  [ ! -f "${HERMES_CONFIG}" ]
}

write_hermes_config() {
  cp -f "${HERMES_CONFIG_EXAMPLE}" "${HERMES_CONFIG}"
  echo "Initialized ${HERMES_CONFIG}"
}

write_talk_config() {
  cp -f "${CONFIG_EXAMPLE}" "${TALK_HOME}/config.toml"
  echo "Initialized ${TALK_HOME}/config.toml"
}

preflight() {
  missing=0
  for d in \
    "${DIR}/models/sensevoice" \
    "${DIR}/models/kokoro" \
    "${DIR}/models/kws-zh-en" \
    "${DIR}/models/vad"; do
    if [ ! -d "${d}" ]; then
      echo "warn: missing ${d}" >&2
      missing=1
    fi
  done
  if [ "${missing}" -eq 1 ]; then
    echo "warn: bundle incomplete; populate .models/ and re-run make package-talk-*" >&2
  fi
}

if [ ! -x "${BIN}" ]; then
  echo "error: missing ${BIN}" >&2
  exit 1
fi

echo "HERMES_HOME=${HERMES_HOME}"
init_hermes_home
init_talk_assets
if needs_hermes_config; then
  write_hermes_config
fi
if needs_default_config; then
  write_talk_config
fi
preflight
exec "${BIN}" talk run "$@"
