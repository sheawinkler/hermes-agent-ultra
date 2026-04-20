#!/usr/bin/env bash
set -euo pipefail

REPO="Lumio-Research/hermes-agent-rs"
VERSION="${1:-latest}"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
BIN_NAME="hermes"

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
ASSET="${BIN_NAME}-${TARGET}.tar.gz"

if [[ "$VERSION" == "latest" ]]; then
  URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"
else
  URL="https://github.com/${REPO}/releases/download/${VERSION}/${ASSET}"
fi

echo "Installing ${BIN_NAME} from ${URL}"
mkdir -p "${INSTALL_DIR}"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

curl -fsSL "${URL}" -o "${TMP_DIR}/hermes.tar.gz"
tar -xzf "${TMP_DIR}/hermes.tar.gz" -C "${TMP_DIR}"
install -m 0755 "${TMP_DIR}/hermes" "${INSTALL_DIR}/${BIN_NAME}"

echo "Installed to ${INSTALL_DIR}/${BIN_NAME}"
if [[ ! -x "${INSTALL_DIR}/${BIN_NAME}" ]]; then
  echo "Install appears incomplete: ${INSTALL_DIR}/${BIN_NAME} is not executable." >&2
  exit 1
fi

if command -v "${BIN_NAME}" >/dev/null 2>&1; then
  echo "Detected on PATH: $(command -v "${BIN_NAME}")"
else
  echo "Binary is not currently on PATH."
  echo "Add this line to your shell config, then restart your shell:"
  echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
  echo
  echo "zsh quick apply:"
  echo "  echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ~/.zshrc && source ~/.zshrc"
  echo "  exec zsh -l"
fi
