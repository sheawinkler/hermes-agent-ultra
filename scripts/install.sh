#!/usr/bin/env bash
set -euo pipefail

REPO="nousresearch/hermes-agent-rust"
VERSION="${1:-latest}"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
BIN_NAME="hermes"

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
echo "Ensure ${INSTALL_DIR} is in your PATH."
