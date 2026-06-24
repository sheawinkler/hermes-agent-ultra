#!/usr/bin/env bash
# Package hermes-agent-ultra (talk / sherpa desktop) for Windows, Linux, or macOS.
#
# Requires: make release-talk (native release binary with --features talk)
#
# ONNX models: place under repo-root `.models/` (gitignored), e.g.
#   .models/models/sensevoice/{model.int8.onnx,tokens.txt}
#   .models/models/kokoro/{model.onnx,voices.bin,tokens.txt,...}
#   .models/models/kws-zh-en/{encoder,decoder,joiner}.onnx, tokens.txt, en.phone
#   .models/models/vad/silero_vad.onnx
#   .models/models/denoise/dpdfnet_baseline.onnx
#   .models/models/speaker/3dspeaker.onnx
#
# Usage:
#   PLATFORM=linux|macos|windows ROOT=... BIN_PATH=... bash scripts/talk/package_desktop.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DIST="${DIST_DIR:-${ROOT}/target/dist}"
MODELS_ROOT="${MODELS_ROOT:-${ROOT}/.models}"
PLATFORM="${PLATFORM:?set PLATFORM to linux, macos, or windows}"

case "${PLATFORM}" in
  linux)
    OUT="${DIST}/hermes-talk-linux-x86_64"
    BIN="${BIN_PATH:-${ROOT}/target/release/hermes-agent-ultra}"
    ARCHIVE_EXT="tar.gz"
    ;;
  macos)
    MAC_ARCH="$(uname -m)"
    OUT="${DIST}/hermes-talk-macos-${MAC_ARCH}"
    BIN="${BIN_PATH:-${ROOT}/target/release/hermes-agent-ultra}"
    ARCHIVE_EXT="tar.gz"
    ;;
  windows)
    OUT="${DIST}/hermes-talk-windows-x86_64"
    BIN="${BIN_PATH:-${ROOT}/target/release/hermes-agent-ultra.exe}"
    ARCHIVE_EXT="zip"
    ;;
  *)
    echo "unsupported PLATFORM=${PLATFORM} (expected linux, macos, or windows)" >&2
    exit 1
    ;;
esac

if [[ ! -f "${BIN}" ]]; then
  echo "missing ${BIN}; run: make release-talk" >&2
  exit 1
fi

copy_models() {
  local name="$1"
  local src="${MODELS_ROOT}/models/${name}"
  if [[ -d "${src}" ]]; then
    mkdir -p "${OUT}/models/${name}"
    cp -a "${src}/." "${OUT}/models/${name}/"
  else
    echo "warn: missing ${src}" >&2
  fi
}

rm -rf "${OUT}"
mkdir -p "${OUT}/bin" "${OUT}/models"

cp -f "${BIN}" "${OUT}/bin/"
if [[ "${PLATFORM}" != "windows" ]]; then
  chmod +x "${OUT}/bin/$(basename "${BIN}")"
fi

for sub in sensevoice kokoro kws-zh-en vad denoise speaker; do
  copy_models "${sub}"
done

cp "${ROOT}/crates/hermes-talk/config.example.toml" "${OUT}/config.example.toml"
cp "${ROOT}/crates/hermes-config/config.example.yaml" "${OUT}/config.example.yaml"

if [[ "${PLATFORM}" == "windows" ]]; then
  cp "${ROOT}/scripts/talk/start_desktop.ps1" "${OUT}/start.ps1"
else
  cp "${ROOT}/scripts/talk/start_desktop.sh" "${OUT}/start.sh"
  chmod +x "${OUT}/start.sh"
fi

echo "Bundled: ${OUT}"

ARCHIVE="${OUT}.${ARCHIVE_EXT}"
rm -f "${ARCHIVE}"
if [[ "${ARCHIVE_EXT}" == "tar.gz" ]]; then
  tar -C "${DIST}" -czf "${ARCHIVE}" "$(basename "${OUT}")"
  echo "Archive: ${ARCHIVE}"
elif command -v tar >/dev/null 2>&1; then
  tar -a -C "${DIST}" -cf "${ARCHIVE}" "$(basename "${OUT}")"
  echo "Archive: ${ARCHIVE}"
elif command -v powershell.exe >/dev/null 2>&1; then
  powershell.exe -NoProfile -Command \
    "Compress-Archive -Path '${OUT}' -DestinationPath '${ARCHIVE}' -Force" \
    || echo "note: zip archive skipped (Compress-Archive failed; use tar or bundle folder)" >&2
  if [[ -f "${ARCHIVE}" ]]; then
    echo "Archive: ${ARCHIVE}"
  fi
else
  echo "note: tar/powershell not found; skipped ${ARCHIVE}" >&2
fi

case "${PLATFORM}" in
  windows) echo "Run: cd ${OUT} && powershell -File .\\start.ps1" ;;
  *)       echo "Run: cd ${OUT} && ./start.sh" ;;
esac
