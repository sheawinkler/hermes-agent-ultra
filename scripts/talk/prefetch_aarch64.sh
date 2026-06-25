#!/usr/bin/env bash
set -euo pipefail

ROOT="${HERMES_ULTRA_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}"
CACHE="${CROSS_CACHE:-${ROOT}/.cross-cache}"

echo "=== prefetch (ROOT=${ROOT}, CACHE=${CACHE}) ==="

echo "=== Downloading LLVM 14 ==="
LLVM_VER=14.0.0
LLVM_DIR="${CACHE}/llvm-14"
LLVM_URL="https://github.com/llvm/llvm-project/releases/download/llvmorg-${LLVM_VER}/clang+llvm-${LLVM_VER}-x86_64-linux-gnu-ubuntu-18.04.tar.xz"

if [[ ! -d "${LLVM_DIR}/lib" ]]; then
    rm -rf "${LLVM_DIR}"
    mkdir -p "${LLVM_DIR}"
    TMP=$(mktemp -d)
    trap 'rm -rf "${TMP}"' EXIT
    echo "  GET ${LLVM_URL}"
    curl -fsSL "${LLVM_URL}" | tar xJ -C "${TMP}"
    mv "${TMP}/clang+llvm-${LLVM_VER}-x86_64-linux-gnu-ubuntu-18.04"/* "${LLVM_DIR}/"
    echo "  LLVM 14 installed to ${LLVM_DIR}"
else
    echo "  LLVM 14 already cached at ${LLVM_DIR}"
fi

echo "=== Downloading sherpa-onnx aarch64 ==="
SHERPA_VER=1.13.3
SHERPA_DIR="${CACHE}/sherpa-onnx"
SHERPA_ARCHIVE="sherpa-onnx-v${SHERPA_VER}-linux-aarch64-static-lib.tar.bz2"
SHERPA_URL="https://github.com/k2-fsa/sherpa-onnx/releases/download/v${SHERPA_VER}/${SHERPA_ARCHIVE}"

mkdir -p "${SHERPA_DIR}"
if [[ ! -f "${SHERPA_DIR}/${SHERPA_ARCHIVE}" ]]; then
    echo "  GET ${SHERPA_URL}"
    curl -fsSL -o "${SHERPA_DIR}/${SHERPA_ARCHIVE}" "${SHERPA_URL}"
    echo "  sherpa-onnx cached at ${SHERPA_DIR}/${SHERPA_ARCHIVE}"
else
    echo "  sherpa-onnx already cached at ${SHERPA_DIR}/${SHERPA_ARCHIVE}"
fi

echo "=== Downloading ripgrep aarch64 (hermes-bundled-rg cross) ==="
RG_VER=14.1.1
RG_DIR="${CACHE}/ripgrep"
RG_ARCHIVE="ripgrep-${RG_VER}-aarch64-unknown-linux-gnu.tar.gz"
RG_URL="https://github.com/BurntSushi/ripgrep/releases/download/${RG_VER}/${RG_ARCHIVE}"

mkdir -p "${RG_DIR}"
if [[ ! -f "${RG_DIR}/${RG_ARCHIVE}" ]]; then
    echo "  GET ${RG_URL}"
    curl -fsSL -o "${RG_DIR}/${RG_ARCHIVE}" "${RG_URL}"
    echo "  ripgrep cached at ${RG_DIR}/${RG_ARCHIVE}"
else
    echo "  ripgrep already cached at ${RG_DIR}/${RG_ARCHIVE}"
fi

echo "=== Downloading mold (fast linker for aarch64 cross release) ==="
MOLD_VER=2.36.0
MOLD_DIR="${CACHE}/mold"
MOLD_ARCHIVE="mold-${MOLD_VER}-x86_64-linux.tar.gz"
MOLD_URL="https://github.com/rui314/mold/releases/download/v${MOLD_VER}/${MOLD_ARCHIVE}"

mkdir -p "${MOLD_DIR}"
if [[ ! -x "${MOLD_DIR}/bin/mold" ]]; then
    if [[ ! -f "${MOLD_DIR}/${MOLD_ARCHIVE}" ]]; then
        echo "  GET ${MOLD_URL}"
        curl -fsSL -o "${MOLD_DIR}/${MOLD_ARCHIVE}" "${MOLD_URL}"
    fi
    TMP="${MOLD_DIR}/.extract-tmp"
    rm -rf "${TMP}"
    mkdir -p "${TMP}"
    tar xzf "${MOLD_DIR}/${MOLD_ARCHIVE}" -C "${TMP}"
    rm -rf "${MOLD_DIR}/bin" "${MOLD_DIR}/lib" "${MOLD_DIR}/share" 2>/dev/null || true
    mv "${TMP}/mold-${MOLD_VER}-x86_64-linux"/* "${MOLD_DIR}/"
    rm -rf "${TMP}"
    echo "  mold installed to ${MOLD_DIR}/bin/mold"
else
    echo "  mold already cached at ${MOLD_DIR}/bin/mold"
fi

echo "=== Done ==="
