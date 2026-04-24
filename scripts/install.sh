#!/usr/bin/env bash
set -euo pipefail

is_termux() {
  [[ -n "${TERMUX_VERSION:-}" ]] || [[ "${PREFIX:-}" == *"com.termux/files/usr"* ]]
}

default_install_dir() {
  if is_termux && [[ -n "${PREFIX:-}" ]]; then
    echo "${PREFIX}/bin"
  else
    echo "${HOME}/.local/bin"
  fi
}

REPO="${REPO:-sheawinkler/hermes-agent-ultra}"
VERSION="${VERSION:-latest}"
INSTALL_DIR="${HERMES_INSTALL_DIR:-${INSTALL_DIR:-$(default_install_dir)}}"
HERMES_HOME="${HERMES_HOME:-$HOME/.hermes}"
CANONICAL_BIN_NAME="${CANONICAL_BIN_NAME:-hermes-agent-ultra}"
PRIMARY_BIN_NAME="${PRIMARY_BIN_NAME:-hermes-ultra}"
LEGACY_BIN_NAME="${LEGACY_BIN_NAME:-hermes}"
RELEASE_BIN_BASENAME="${RELEASE_BIN_BASENAME:-hermes}"
RUN_SETUP_MODE="${RUN_SETUP_MODE:-auto}" # auto|always|never
POSITIONAL_VERSION=""
if [[ -t 0 ]]; then
  IS_INTERACTIVE=true
else
  IS_INTERACTIVE=false
fi

show_help() {
  cat <<'EOF'
Usage: scripts/install.sh [version] [options]

Install hermes-agent-ultra from GitHub Releases.

Arguments:
  version                Release tag to install (default: latest)

Options:
  --version TAG          Release tag to install (same as positional version)
  --setup                Run post-install setup flow without prompting
  --skip-setup           Skip post-install setup flow
  --dir PATH             Install directory for binaries/symlink
  --hermes-home PATH     Hermes home directory for SOUL.md bootstrap
  -h, --help             Show this help

Environment variables:
  REPO                   GitHub repo slug (default: sheawinkler/hermes-agent-ultra)
  HERMES_INSTALL_DIR     Destination bin directory (default: ~/.local/bin or $PREFIX/bin on Termux)
  INSTALL_DIR            Destination bin directory (legacy alias, overridden by HERMES_INSTALL_DIR)
  HERMES_HOME            Hermes config dir for SOUL.md bootstrap (default: $HOME/.hermes)
  CANONICAL_BIN_NAME     Installed binary name (default: hermes-agent-ultra)
  PRIMARY_BIN_NAME       Primary user-facing command symlink (default: hermes-ultra)
  LEGACY_BIN_NAME        Compatibility alias symlink name (default: hermes)
  RELEASE_BIN_BASENAME   Tarball executable basename (default: hermes)
  RUN_SETUP_MODE         auto|always|never for setup flow (default: auto)
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help)
      show_help
      exit 0
      ;;
    --version)
      if [[ $# -lt 2 ]]; then
        echo "--version requires a value" >&2
        exit 1
      fi
      VERSION="$2"
      shift 2
      ;;
    --setup)
      RUN_SETUP_MODE="always"
      shift
      ;;
    --skip-setup)
      RUN_SETUP_MODE="never"
      shift
      ;;
    --dir)
      if [[ $# -lt 2 ]]; then
        echo "--dir requires a value" >&2
        exit 1
      fi
      INSTALL_DIR="$2"
      shift 2
      ;;
    --hermes-home)
      if [[ $# -lt 2 ]]; then
        echo "--hermes-home requires a value" >&2
        exit 1
      fi
      HERMES_HOME="$2"
      shift 2
      ;;
    --*)
      echo "Unknown option: $1" >&2
      show_help
      exit 1
      ;;
    *)
      if [[ -n "${POSITIONAL_VERSION}" ]]; then
        echo "Unexpected extra positional argument: $1" >&2
        show_help
        exit 1
      fi
      POSITIONAL_VERSION="$1"
      shift
      ;;
  esac
done

if [[ -n "${POSITIONAL_VERSION}" ]]; then
  VERSION="${POSITIONAL_VERSION}"
fi

case "${RUN_SETUP_MODE}" in
  auto|always|never) ;;
  *)
    echo "Invalid RUN_SETUP_MODE: ${RUN_SETUP_MODE} (expected auto|always|never)" >&2
    exit 1
    ;;
esac

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

prompt_yes_no() {
  local question="$1"
  local default_yes="${2:-yes}"
  local answer=""
  local suffix
  case "${default_yes}" in
    [yY]|[yY][eE][sS]|[tT][rR][uU][eE]|1)
      suffix="[Y/n]"
      ;;
    *)
      suffix="[y/N]"
      ;;
  esac

  if [[ "${IS_INTERACTIVE}" == "true" ]]; then
    read -r -p "${question} ${suffix} " answer || answer=""
  elif [[ -r /dev/tty && -w /dev/tty ]]; then
    printf "%s %s " "${question}" "${suffix}" > /dev/tty
    IFS= read -r answer < /dev/tty || answer=""
  else
    answer=""
  fi

  answer="${answer#"${answer%%[![:space:]]*}"}"
  answer="${answer%"${answer##*[![:space:]]}"}"
  if [[ -z "${answer}" ]]; then
    case "${default_yes}" in
      [yY]|[yY][eE][sS]|[tT][rR][uU][eE]|1) return 0 ;;
      *) return 1 ;;
    esac
  fi
  case "${answer}" in
    y|Y|yes|YES|Yes) return 0 ;;
    *) return 1 ;;
  esac
}

run_post_install_flow() {
  local bin_path="$1"

  echo
  echo "Running post-install verification..."
  "${bin_path}" doctor || true

  echo
  echo "Current auth/platform status:"
  "${bin_path}" auth status || true

  if [[ -t 0 ]]; then
    echo
    if prompt_yes_no "Run interactive setup now?" "yes"; then
      "${bin_path}" setup || true
    else
      echo "Skipped setup. Run this anytime:"
      echo "  ${bin_path} setup"
    fi
  else
    echo
    echo "Interactive setup skipped (non-interactive shell)."
    echo "Run later:"
    echo "  ${bin_path} setup"
  fi
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
  for candidate in "${CANONICAL_BIN_NAME}" "${PRIMARY_BIN_NAME}" "${RELEASE_BIN_BASENAME}" "${LEGACY_BIN_NAME}"; do
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
if [[ "${PRIMARY_BIN_NAME}" != "${CANONICAL_BIN_NAME}" ]]; then
  ln -sfn "${CANONICAL_BIN_NAME}" "${INSTALL_DIR}/${PRIMARY_BIN_NAME}"
fi
if [[ "${LEGACY_BIN_NAME}" != "${CANONICAL_BIN_NAME}" ]]; then
  ln -sfn "${CANONICAL_BIN_NAME}" "${INSTALL_DIR}/${LEGACY_BIN_NAME}"
fi

echo "Installed to ${INSTALL_DIR}/${CANONICAL_BIN_NAME}"
if [[ ! -x "${INSTALL_DIR}/${CANONICAL_BIN_NAME}" ]]; then
  echo "Install appears incomplete: ${INSTALL_DIR}/${CANONICAL_BIN_NAME} is not executable." >&2
  exit 1
fi

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
  if command -v "${PRIMARY_BIN_NAME}" >/dev/null 2>&1; then
    echo "Primary command available: $(command -v "${PRIMARY_BIN_NAME}")"
  fi
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

BIN_PATH="${INSTALL_DIR}/${CANONICAL_BIN_NAME}"
if [[ -x "${BIN_PATH}" ]]; then
  case "${RUN_SETUP_MODE}" in
    always)
      run_post_install_flow "${BIN_PATH}"
      ;;
    auto)
      if prompt_yes_no "Run post-install setup flow (doctor + auth status + setup)?" "yes"; then
        run_post_install_flow "${BIN_PATH}"
      else
        echo "Post-install setup skipped. Run later:"
        echo "  ${BIN_PATH} setup"
      fi
      ;;
    never)
      echo "Post-install setup skipped (--skip-setup)."
      ;;
  esac
fi
