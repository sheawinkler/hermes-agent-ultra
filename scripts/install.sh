#!/usr/bin/env bash
set -euo pipefail

REPO="${REPO:-sheawinkler/hermes-agent-ultra}"
VERSION="${1:-latest}"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
CANONICAL_BIN_NAME="${CANONICAL_BIN_NAME:-hermes-agent-ultra}"
LEGACY_BIN_NAME="${LEGACY_BIN_NAME:-hermes}"
RELEASE_BIN_BASENAME="${RELEASE_BIN_BASENAME:-hermes}"

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "Missing required command: $1" >&2
    exit 1
  }
}

detect_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Linux) os="linux" ;;
    Darwin) os="macos" ;;
    *) echo "Unsupported OS: $os" >&2; exit 1 ;;
  esac

  case "$arch" in
    x86_64|amd64) arch="x86_64" ;;
    arm64|aarch64) arch="aarch64" ;;
    *) echo "Unsupported arch: $arch" >&2; exit 1 ;;
  esac

  echo "${os}-${arch}"
}

need_cmd curl
need_cmd tar
need_cmd install

TARGET="$(detect_target)"
ASSET="${RELEASE_BIN_BASENAME}-${TARGET}.tar.gz"

if [[ "$VERSION" == "latest" ]]; then
  URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"
else
  URL="https://github.com/${REPO}/releases/download/${VERSION}/${ASSET}"
fi

echo "Installing ${CANONICAL_BIN_NAME} from ${URL}"
mkdir -p "${INSTALL_DIR}"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

curl -fsSL "${URL}" -o "${TMP_DIR}/${RELEASE_BIN_BASENAME}.tar.gz"
tar -xzf "${TMP_DIR}/${RELEASE_BIN_BASENAME}.tar.gz" -C "${TMP_DIR}"

SOURCE_BIN=""
for candidate in "${CANONICAL_BIN_NAME}" "${RELEASE_BIN_BASENAME}" "${LEGACY_BIN_NAME}"; do
  if [[ -f "${TMP_DIR}/${candidate}" ]]; then
    SOURCE_BIN="${TMP_DIR}/${candidate}"
    break
  fi
done
if [[ -z "${SOURCE_BIN}" ]]; then
  echo "No executable binary found in release archive (${ASSET})." >&2
  exit 1
fi

install -m 0755 "${SOURCE_BIN}" "${INSTALL_DIR}/${CANONICAL_BIN_NAME}"
if [[ "${LEGACY_BIN_NAME}" != "${CANONICAL_BIN_NAME}" ]]; then
  ln -sfn "${CANONICAL_BIN_NAME}" "${INSTALL_DIR}/${LEGACY_BIN_NAME}"
fi

echo "Installed to ${INSTALL_DIR}/${CANONICAL_BIN_NAME}"
if [[ ! -x "${INSTALL_DIR}/${CANONICAL_BIN_NAME}" ]]; then
  echo "Install appears incomplete: ${INSTALL_DIR}/${CANONICAL_BIN_NAME} is not executable." >&2
  exit 1
fi

HERMES_HOME="${HERMES_HOME:-$HOME/.hermes}"
mkdir -p "${HERMES_HOME}"
if [[ ! -f "${HERMES_HOME}/SOUL.md" ]]; then
  cat > "${HERMES_HOME}/SOUL.md" <<'SOUL_EOF'
# Hermes Agent Persona

<!--
Customize this file to control how Hermes communicates.
This file is loaded every message; no restart needed.
Delete this file (or leave it empty) to use the default personality.
-->
SOUL_EOF
  echo "Created ${HERMES_HOME}/SOUL.md (edit to customize personality)."
fi

if command -v "${CANONICAL_BIN_NAME}" >/dev/null 2>&1; then
  echo "Detected on PATH: $(command -v "${CANONICAL_BIN_NAME}")"
  if command -v "${LEGACY_BIN_NAME}" >/dev/null 2>&1; then
    echo "Legacy alias available: $(command -v "${LEGACY_BIN_NAME}")"
  fi
else
  echo "Binary is not currently on PATH."
  echo "Add this line to your shell config, then restart your shell:"
  echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
  echo
  echo "zsh quick apply:"
  echo "  echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ~/.zshrc && source ~/.zshrc"
  echo "  exec zsh -l"
fi
