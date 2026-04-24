#!/usr/bin/env sh
set -eu

APP_HOME="${HERMES_HOME:-/data}"
mkdir -p "${APP_HOME}"

if [ "$(id -u)" = "0" ]; then
  TARGET_UID="${HERMES_UID:-10000}"
  TARGET_GID="${HERMES_GID:-${TARGET_UID}}"

  if ! getent group hermes >/dev/null 2>&1; then
    groupadd -o -g "${TARGET_GID}" hermes >/dev/null 2>&1 || true
  fi
  if ! id -u hermes >/dev/null 2>&1; then
    useradd -m -o -u "${TARGET_UID}" -g "${TARGET_GID}" -s /bin/sh hermes >/dev/null 2>&1 || true
  fi

  if [ "$(id -g hermes)" != "${TARGET_GID}" ]; then
    groupmod -o -g "${TARGET_GID}" hermes >/dev/null 2>&1 || true
  fi
  if [ "$(id -u hermes)" != "${TARGET_UID}" ]; then
    usermod -o -u "${TARGET_UID}" -g "${TARGET_GID}" hermes >/dev/null 2>&1 || true
  fi

  actual_uid="$(id -u hermes)"
  actual_gid="$(id -g hermes)"
  needs_chown=false
  if [ -n "${HERMES_UID:-}" ] && [ "${HERMES_UID}" != "10000" ]; then
    needs_chown=true
  elif [ "$(stat -c %u "${APP_HOME}" 2>/dev/null || echo 0)" != "${actual_uid}" ]; then
    needs_chown=true
  fi
  if [ "${needs_chown}" = true ]; then
    chown -R "${actual_uid}:${actual_gid}" "${APP_HOME}" >/dev/null 2>&1 || true
  fi

  exec gosu "${actual_uid}:${actual_gid}" "$@"
fi

exec "$@"
