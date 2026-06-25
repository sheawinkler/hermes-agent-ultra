#!/usr/bin/env bash
# Download sherpa-onnx CPU static runtime for hermes-talk.
#
# Usage:
#   ./scripts/talk/fetch_sherpa_runtime.sh [cpu|auto]
#
# Build with:
#   SHERPA_ONNX_PACK=cpu make release-talk

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
EP="${1:-auto}"
VERSION="1.13.3"
BASE="https://github.com/k2-fsa/sherpa-onnx/releases/download/v${VERSION}"
CACHE="${SHERPA_ONNX_CACHE:-$ROOT/.cross-cache/sherpa-onnx}"
OS="$(uname -s)"
ARCH="$(uname -m)"

EP="cpu"

if [[ "${1:-}" == "cuda" || "${1:-}" == "gpu" || "${1:-}" == "directml" || "${1:-}" == "dml" || "${1:-}" == "macos" || "${1:-}" == "coreml" ]]; then
  echo "SHERPA_ONNX_PACK=${1} is no longer supported; use cpu (default)" >&2
  exit 1
fi

archive_for() {
  case "$OS:$ARCH" in
    Linux:x86_64)  echo "sherpa-onnx-v${VERSION}-linux-x64-static-lib.tar.bz2" ;;
    Linux:aarch64) echo "sherpa-onnx-v${VERSION}-linux-aarch64-static-lib.tar.bz2" ;;
    Darwin:arm64)  echo "sherpa-onnx-v${VERSION}-osx-arm64-static-lib.tar.bz2" ;;
    Darwin:x86_64) echo "sherpa-onnx-v${VERSION}-osx-x64-static-lib.tar.bz2" ;;
    MINGW*|MSYS*|CYGWIN*) echo "sherpa-onnx-v${VERSION}-win-x64-static-MT-Release-lib.tar.bz2" ;;
    *) echo "unsupported cpu target: $OS $ARCH" >&2; return 1 ;;
  esac
}

ARCHIVE="$(archive_for)"
DEST="$CACHE/$EP"
STEM="${ARCHIVE%.tar.bz2}"
LIB_DIR="$DEST/$STEM/lib"

if [[ -d "$LIB_DIR" ]]; then
  echo "sherpa-onnx pack=$EP runtime already at $LIB_DIR"
  echo "export SHERPA_ONNX_LIB_DIR=$LIB_DIR"
  echo "export SHERPA_ONNX_PACK=$EP"
  exit 0
fi

mkdir -p "$DEST"
TMP="$DEST/$ARCHIVE"
if [[ ! -f "$TMP" ]]; then
  echo "Downloading $BASE/$ARCHIVE"
  curl -fL "$BASE/$ARCHIVE" -o "$TMP"
fi

tar -xjf "$TMP" -C "$DEST"
if [[ ! -d "$LIB_DIR" ]]; then
  echo "expected lib/ under $DEST/$STEM" >&2
  exit 1
fi

echo "sherpa-onnx pack=$EP runtime ready at $LIB_DIR"
echo "export SHERPA_ONNX_LIB_DIR=$LIB_DIR"
echo "export SHERPA_ONNX_PACK=$EP"
