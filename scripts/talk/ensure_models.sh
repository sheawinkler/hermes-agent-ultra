#!/usr/bin/env bash
# Verify sherpa-onnx talk models under ${MODELS_ROOT}/models/; download if anything is missing.
#
# Usage:
#   bash scripts/talk/ensure_models.sh
#   MODELS_ROOT=/path/to/.models bash scripts/talk/ensure_models.sh
set -euo pipefail

ROOT="${HERMES_ULTRA_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}"
MODELS_ROOT="${MODELS_ROOT:-${ROOT}/.models}"
DEST="${MODELS_ROOT}/models"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

REQUIRED=(
  "sensevoice/model.int8.onnx"
  "sensevoice/tokens.txt"
  "kokoro/model.onnx"
  "kokoro/voices.bin"
  "kokoro/tokens.txt"
  "kws-zh-en/encoder.onnx"
  "kws-zh-en/decoder.onnx"
  "kws-zh-en/joiner.onnx"
  "kws-zh-en/tokens.txt"
  "vad/silero_vad.onnx"
  "denoise/dpdfnet_baseline.onnx"
  "speaker/3dspeaker.onnx"
)

missing=()
for rel in "${REQUIRED[@]}"; do
  if [[ ! -f "${DEST}/${rel}" ]]; then
    missing+=("${rel}")
  fi
done

if [[ ${#missing[@]} -eq 0 ]]; then
  echo "=== talk models OK (${DEST}) ==="
  exit 0
fi

echo "=== talk models missing under ${DEST} ==="
printf '  %s\n' "${missing[@]}"
if [[ "${CHECK_ONLY:-0}" == "1" ]]; then
  echo "Run: make download-talk-models" >&2
  exit 1
fi

echo "=== downloading (HTTPS_PROXY=${HTTPS_PROXY:-${https_proxy:-${HTTP_PROXY:-${http_proxy:-unset}}}}) ==="
HERMES_ULTRA_ROOT="${ROOT}" MODELS_ROOT="${MODELS_ROOT}" bash "${SCRIPT_DIR}/download_models.sh"
