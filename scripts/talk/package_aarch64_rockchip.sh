#!/usr/bin/env bash
# Package hermes-agent-ultra (talk-rockchip) for aarch64 Rockchip boards.
#
# ONNX / bundled models: place under repo-root `.models/` (gitignored), e.g.
#   .models/models/vad/silero_vad.onnx
#   .models/models/denoise/dpdfnet_baseline.onnx
#   .models/models/speaker/3dspeaker.onnx
#   .models/models/kws-zh-en/{encoder,decoder,joiner}.onnx, tokens.txt, en.phone
#   .models/models/rk3588/          (optional; falls back to RK_TTS_SDK_DIR)
#   .models/data/                   (optional; falls back to RK_ASR_SDK_DIR)
#   .models/frontend_extras/        (optional; falls back to RK_TTS_SDK_DIR)
#   .models/auth/                   Rockchip license keys (key_asr.lic, key_tts.lic)
# Board default config: scripts/talk/config.example.rockchip.{toml,yaml}
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DIST="${DIST_DIR:-${ROOT}/target/dist}"
BIN="${BIN_PATH:-${ROOT}/target/aarch64-unknown-linux-gnu/release/hermes-agent-ultra}"
GCC="${ROOT}/.cross-cache/gcc-aarch64/aarch64-none-linux-gnu"
OUT="${DIST}/hermes-talk-rk3588"
RKAUDIO="${ROOT}/crates/hermes-talk/rkaudio"
MODELS_ROOT="${MODELS_ROOT:-${ROOT}/.models}"

RK_TTS_SDK_DIR="${RK_TTS_SDK_DIR:-/home/leeyang/Rockchip_RKTTS_SDK_Release}"
RK_ASR_SDK_DIR="${RK_ASR_SDK_DIR:-/home/leeyang/ASR_SDK/ROCKASR2_RK3588/rockasr2_android_linux_rk3588_20260312}"

if [[ ! -f "${BIN}" ]]; then
  echo "missing ${BIN}; run: make release-talk-rockchip-arm64" >&2
  exit 1
fi

rm -rf "${OUT}"
mkdir -p "${OUT}/bin" "${OUT}/lib" "${OUT}/models"

cp -f "${BIN}" "${OUT}/bin/hermes-agent-ultra"
chmod +x "${OUT}/bin/hermes-agent-ultra"

cp -a "${GCC}/libc/lib/ld-linux-aarch64.so.1" "${OUT}/lib/"

for lib in libc.so.6 libm.so.6 libpthread.so.0 libdl.so.2 librt.so.1 \
           libutil.so.1 libresolv.so.2 libnss_files.so.2 libnss_dns.so.2; do
  cp -a "${GCC}/libc/lib64/${lib}" "${OUT}/lib/"
done

cp -a "${GCC}/lib64/libstdc++.so.6.0.32" "${OUT}/lib/"
ln -sf libstdc++.so.6.0.32 "${OUT}/lib/libstdc++.so.6"
cp -a "${GCC}/lib64/libgcc_s.so.1" "${OUT}/lib/"

cp "${RKAUDIO}/lib/librktts.so" "${OUT}/lib/"
cp "${RKAUDIO}/lib/librknnrt.so" "${OUT}/lib/"

for lib in librockasr.so librockx2.so librockx_modules.so librkllmrt.so \
           librknn3_api.so libonnxruntime.so libgomp.so.1; do
  [[ -f "${RKAUDIO}/lib/${lib}" ]] && cp "${RKAUDIO}/lib/${lib}" "${OUT}/lib/"
done

# Rockchip TTS models + dictionaries (.models preferred, SDK fallback)
if [[ -d "${MODELS_ROOT}/models/rk3588" ]]; then
  cp -r "${MODELS_ROOT}/models/rk3588" "${OUT}/models/"
elif [[ -d "${RK_TTS_SDK_DIR}/models/rk3588" ]]; then
  cp -r "${RK_TTS_SDK_DIR}/models/rk3588" "${OUT}/models/"
else
  echo "warn: no TTS models in ${MODELS_ROOT}/models/rk3588 or RK_TTS_SDK_DIR" >&2
fi

if [[ -d "${MODELS_ROOT}/frontend_extras" ]]; then
  cp -r "${MODELS_ROOT}/frontend_extras" "${OUT}/"
elif [[ -d "${RK_TTS_SDK_DIR}/frontend_extras" ]]; then
  cp -r "${RK_TTS_SDK_DIR}/frontend_extras" "${OUT}/"
fi

# Sherpa ONNX models (wake / vad / denoise / speaker)
if [[ -d "${MODELS_ROOT}/models/kws-zh-en" ]]; then
  mkdir -p "${OUT}/models/kws-zh-en"
  cp -a "${MODELS_ROOT}/models/kws-zh-en/." "${OUT}/models/kws-zh-en/"
else
  echo "warn: missing ${MODELS_ROOT}/models/kws-zh-en (wake word)" >&2
fi

for sub in vad denoise speaker; do
  if [[ -d "${MODELS_ROOT}/models/${sub}" ]]; then
    mkdir -p "${OUT}/models/${sub}"
    cp -a "${MODELS_ROOT}/models/${sub}/." "${OUT}/models/${sub}/"
  fi
done

# Rockchip ASR model data (.models preferred, SDK fallback)
if [[ -d "${MODELS_ROOT}/data" ]]; then
  cp -r "${MODELS_ROOT}/data" "${OUT}/data"
elif [[ -d "${RK_ASR_SDK_DIR}/data" ]]; then
  cp -r "${RK_ASR_SDK_DIR}/data" "${OUT}/data"
else
  echo "warn: no ASR data in ${MODELS_ROOT}/data or RK_ASR_SDK_DIR" >&2
fi

# Rockchip license keys (.models/auth)
if [[ -d "${MODELS_ROOT}/auth" ]]; then
  cp -a "${MODELS_ROOT}/auth" "${OUT}/auth"
else
  echo "warn: missing ${MODELS_ROOT}/auth (Rockchip license keys)" >&2
fi

cp "${ROOT}/scripts/talk/config.example.rockchip.toml" "${OUT}/config.example.toml"
cp "${ROOT}/scripts/talk/config.example.rockchip.yaml" "${OUT}/config.example.yaml"
cp "${ROOT}/scripts/talk/start_board.sh" "${OUT}/start.sh"
chmod +x "${OUT}/start.sh"

echo "Bundled: ${OUT}"
echo "On board: cd ${OUT} && ./start.sh"
echo "  (first run: ~/.hermes-agent-ultra + hermes-talk/config.toml; models linked from bundle)"
