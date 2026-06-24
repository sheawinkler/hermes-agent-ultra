# Verify sherpa-onnx talk models under ${MODELS_ROOT}/models/; download if anything is missing.
#
# Usage:
#   powershell -File scripts/talk/ensure_models.ps1
#   $env:MODELS_ROOT = "C:\path\.models"; powershell -File scripts/talk/ensure_models.ps1
$ErrorActionPreference = "Stop"

$Root = if ($env:HERMES_ULTRA_ROOT) { $env:HERMES_ULTRA_ROOT } else {
    (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
}
$ModelsRoot = if ($env:MODELS_ROOT) { $env:MODELS_ROOT } else { Join-Path $Root ".models" }
$Dest = Join-Path $ModelsRoot "models"

$Required = @(
    "sensevoice/model.int8.onnx",
    "sensevoice/tokens.txt",
    "kokoro/model.onnx",
    "kokoro/voices.bin",
    "kokoro/tokens.txt",
    "kws-zh-en/encoder.onnx",
    "kws-zh-en/decoder.onnx",
    "kws-zh-en/joiner.onnx",
    "kws-zh-en/tokens.txt",
    "vad/silero_vad.onnx",
    "denoise/dpdfnet_baseline.onnx",
    "speaker/3dspeaker.onnx"
)

$Missing = @()
foreach ($rel in $Required) {
    $path = Join-Path $Dest $rel
    if (-not (Test-Path $path)) {
        $Missing += $rel
    }
}

if ($Missing.Count -eq 0) {
    Write-Host "=== talk models OK ($Dest) ==="
    exit 0
}

Write-Host "=== talk models missing under $Dest ==="
foreach ($rel in $Missing) {
    Write-Host "  $rel"
}
if ($env:CHECK_ONLY -eq "1") {
    Write-Host "Run: make download-talk-models" -ForegroundColor Red
    exit 1
}
$proxy = if ($env:HTTPS_PROXY) { $env:HTTPS_PROXY } elseif ($env:HTTP_PROXY) { $env:HTTP_PROXY } else { "unset" }
Write-Host "=== downloading (HTTPS_PROXY=$proxy) ==="
& (Join-Path $PSScriptRoot "download_models.ps1")
