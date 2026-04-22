#!/usr/bin/env bash
set -euo pipefail

REPO="${REPO:-sheawinkler/hermes-agent-ultra}"
VERSION="${1:-latest}"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
CANONICAL_BIN_NAME="${CANONICAL_BIN_NAME:-hermes-agent-ultra}"
LEGACY_BIN_NAME="${LEGACY_BIN_NAME:-hermes}"
RELEASE_BIN_BASENAME="${RELEASE_BIN_BASENAME:-hermes}"

if [[ "${VERSION}" == "--help" || "${VERSION}" == "-h" ]]; then
  cat <<'EOF'
Usage: scripts/install.sh [version]

Install hermes-agent-ultra from GitHub Releases.

Arguments:
  version                Release tag to install (default: latest)

Environment variables:
  REPO                   GitHub repo slug (default: sheawinkler/hermes-agent-ultra)
  INSTALL_DIR            Destination bin directory (default: $HOME/.local/bin)
  CANONICAL_BIN_NAME     Installed binary name (default: hermes-agent-ultra)
  LEGACY_BIN_NAME        Compatibility alias symlink name (default: hermes)
  RELEASE_BIN_BASENAME   Tarball executable basename (default: hermes)
  HERMES_HOME            Hermes config dir for SOUL.md bootstrap (default: $HOME/.hermes)
EOF
  exit 0
fi

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "Missing required command: $1" >&2
    exit 1
  }
}

release_url() {
  local asset="$1"
  if [[ "$VERSION" == "latest" ]]; then
    echo "https://github.com/${REPO}/releases/latest/download/${asset}"
  else
    echo "https://github.com/${REPO}/releases/download/${VERSION}/${asset}"
  fi
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
ASSET_CANDIDATES=("${RELEASE_BIN_BASENAME}-${TARGET}.tar.gz")
if [[ "$TARGET" == "macos-aarch64" ]]; then
  ASSET_CANDIDATES+=("${RELEASE_BIN_BASENAME}-macos-arm64.tar.gz")
elif [[ "$TARGET" == "linux-aarch64" ]]; then
  ASSET_CANDIDATES+=("${RELEASE_BIN_BASENAME}-linux-arm64.tar.gz")
fi

mkdir -p "${INSTALL_DIR}"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

SOURCE_BIN=""
DOWNLOADED_ASSET=""
for asset in "${ASSET_CANDIDATES[@]}"; do
  URL="$(release_url "${asset}")"
  echo "Installing ${CANONICAL_BIN_NAME} from ${URL}"
  if curl -fsSL "${URL}" -o "${TMP_DIR}/${RELEASE_BIN_BASENAME}.tar.gz"; then
    DOWNLOADED_ASSET="${asset}"
    break
  fi
done

if [[ -n "${DOWNLOADED_ASSET}" ]]; then
  tar -xzf "${TMP_DIR}/${RELEASE_BIN_BASENAME}.tar.gz" -C "${TMP_DIR}"
  for candidate in "${CANONICAL_BIN_NAME}" "${RELEASE_BIN_BASENAME}" "${LEGACY_BIN_NAME}"; do
    if [[ -f "${TMP_DIR}/${candidate}" ]]; then
      SOURCE_BIN="${TMP_DIR}/${candidate}"
      break
    fi
  done
fi

if [[ -z "${SOURCE_BIN}" ]]; then
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  SOURCE_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
  if [[ -f "${SOURCE_ROOT}/Cargo.toml" ]] && command -v cargo >/dev/null 2>&1; then
    echo "Release asset not available for target (${TARGET}); building from source at ${SOURCE_ROOT}"
    (
      cd "${SOURCE_ROOT}"
      cargo build --release -p hermes-cli --bin "${CANONICAL_BIN_NAME}"
    )
    SOURCE_BIN="${SOURCE_ROOT}/target/release/${CANONICAL_BIN_NAME}"
  else
    echo "No executable release asset found for target (${TARGET}) in repo ${REPO}." >&2
    echo "Tried assets: ${ASSET_CANDIDATES[*]}" >&2
    exit 1
  fi
fi

if [[ ! -f "${SOURCE_BIN}" ]]; then
  echo "Built binary not found at expected path: ${SOURCE_BIN}" >&2
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
