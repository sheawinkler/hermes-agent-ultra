# Download sherpa-onnx pretrained models for hermes-talk desktop (Windows).
# URLs: https://k2-fsa.github.io/sherpa/onnx/index.html
#
# Usage:
#   powershell -File scripts/talk/download_models.ps1
#   $env:MODELS_ROOT = "C:\path\.models"; powershell -File scripts/talk/download_models.ps1
#   $env:HTTPS_PROXY = "http://127.0.0.1:7890"; powershell -File scripts/talk/download_models.ps1
$ErrorActionPreference = "Stop"

function Get-DownloadProxy {
    if ($env:HTTPS_PROXY) { return $env:HTTPS_PROXY.Trim() }
    if ($env:https_proxy) { return $env:https_proxy.Trim() }
    if ($env:HTTP_PROXY) { return $env:HTTP_PROXY.Trim() }
    if ($env:http_proxy) { return $env:http_proxy.Trim() }
    return $null
}

$DownloadProxy = Get-DownloadProxy

$Root = if ($env:HERMES_ULTRA_ROOT) { $env:HERMES_ULTRA_ROOT } else {
    (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
}
$ModelsRoot = if ($env:MODELS_ROOT) { $env:MODELS_ROOT } else { Join-Path $Root ".models" }
$Dest = Join-Path $ModelsRoot "models"
$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("hermes-talk-models-" + [guid]::NewGuid().ToString("n"))
New-Item -ItemType Directory -Path $Tmp -Force | Out-Null

$SherpaBase = "https://github.com/k2-fsa/sherpa-onnx/releases/download"

function Ensure-Dir([string]$Path) {
    if (-not (Test-Path $Path)) {
        New-Item -ItemType Directory -Path $Path -Force | Out-Null
    }
}

function Fetch([string]$Url, [string]$OutPath) {
    if (Test-Path $OutPath) {
        Write-Host "  skip (cached): $(Split-Path -Leaf $OutPath)"
        return
    }
    Write-Host "  GET $Url"
    $params = @{
        Uri             = $Url
        OutFile         = $OutPath
        UseBasicParsing = $true
    }
    if ($DownloadProxy) {
        $params.Proxy = $DownloadProxy
    }
    Invoke-WebRequest @params
}

function Extract-TarBz2([string]$Archive, [string]$DestDir) {
    Ensure-Dir $DestDir
    $extractTo = Join-Path $Tmp ([System.IO.Path]::GetFileNameWithoutExtension([System.IO.Path]::GetFileNameWithoutExtension($Archive)))
    Ensure-Dir $extractTo
    tar xf $Archive -C $extractTo
    $inner = Get-ChildItem -Path $extractTo -Directory | Select-Object -First 1
    if (-not $inner) { throw "extract failed: no top-level dir in $Archive" }
    if (Test-Path $DestDir) { Remove-Item -Recurse -Force $DestDir }
    Ensure-Dir $DestDir
    Copy-Item -Path (Join-Path $inner.FullName "*") -Destination $DestDir -Recurse -Force
}

Write-Host "=== hermes-talk model download ==="
Write-Host "MODELS_ROOT=$ModelsRoot"
Write-Host "DEST=$Dest"
if ($DownloadProxy) {
    Write-Host "HTTPS_PROXY=$DownloadProxy"
}
Write-Host ""

Ensure-Dir $ModelsRoot
Ensure-Dir $Dest

# SenseVoice
$senseDest = Join-Path $Dest "sensevoice"
if ((Test-Path (Join-Path $senseDest "model.int8.onnx")) -and (Test-Path (Join-Path $senseDest "tokens.txt"))) {
    Write-Host "=== sensevoice: already present ==="
} else {
    Write-Host "=== sensevoice (SenseVoice int8) ==="
    $archive = Join-Path $Tmp "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2"
    Fetch "$SherpaBase/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2" $archive
    Extract-TarBz2 $archive $senseDest
    Write-Host "  -> $senseDest"
}

# Kokoro
$kokoroDest = Join-Path $Dest "kokoro"
if ((Test-Path (Join-Path $kokoroDest "model.onnx")) -and (Test-Path (Join-Path $kokoroDest "voices.bin"))) {
    Write-Host "=== kokoro: already present ==="
} else {
    Write-Host "=== kokoro (Kokoro multi-lang v1.0) ==="
    $archive = Join-Path $Tmp "kokoro-multi-lang-v1_0.tar.bz2"
    Fetch "$SherpaBase/tts-models/kokoro-multi-lang-v1_0.tar.bz2" $archive
    Extract-TarBz2 $archive $kokoroDest
    Write-Host "  -> $kokoroDest"
}

# KWS
$kwsDest = Join-Path $Dest "kws-zh-en"
if ((Test-Path (Join-Path $kwsDest "encoder.onnx")) -and (Test-Path (Join-Path $kwsDest "decoder.onnx"))) {
    Write-Host "=== kws-zh-en: already present ==="
} else {
    Write-Host "=== kws-zh-en (Zipformer zh+en KWS) ==="
    $archive = Join-Path $Tmp "sherpa-onnx-kws-zipformer-zh-en-3M-2025-12-20.tar.bz2"
    Fetch "$SherpaBase/kws-models/sherpa-onnx-kws-zipformer-zh-en-3M-2025-12-20.tar.bz2" $archive
    $extract = Join-Path $Tmp "kws-src"
    Ensure-Dir $extract
    tar xf $archive -C $extract
    $inner = Get-ChildItem -Path $extract -Directory | Select-Object -First 1
    if (Test-Path $kwsDest) { Remove-Item -Recurse -Force $kwsDest }
    Ensure-Dir $kwsDest
    Copy-Item (Join-Path $inner.FullName "tokens.txt") $kwsDest
    Copy-Item (Join-Path $inner.FullName "en.phone") $kwsDest
    Copy-Item (Join-Path $inner.FullName "encoder-epoch-13-avg-2-chunk-16-left-64.int8.onnx") (Join-Path $kwsDest "encoder.onnx")
    Copy-Item (Join-Path $inner.FullName "decoder-epoch-13-avg-2-chunk-16-left-64.onnx") (Join-Path $kwsDest "decoder.onnx")
    Copy-Item (Join-Path $inner.FullName "joiner-epoch-13-avg-2-chunk-16-left-64.int8.onnx") (Join-Path $kwsDest "joiner.onnx")
    Write-Host "  -> $kwsDest"
}

# VAD
$vadDest = Join-Path $Dest "vad"
$vadOut = Join-Path $vadDest "silero_vad.onnx"
if (Test-Path $vadOut) {
    Write-Host "=== vad: already present ==="
} else {
    Write-Host "=== vad (Silero VAD) ==="
    Ensure-Dir $vadDest
    Fetch "$SherpaBase/asr-models/silero_vad.onnx" $vadOut
    Write-Host "  -> $vadOut"
}

# Denoise
$denoiseDest = Join-Path $Dest "denoise"
$denoiseOut = Join-Path $denoiseDest "dpdfnet_baseline.onnx"
if (Test-Path $denoiseOut) {
    Write-Host "=== denoise: already present ==="
} else {
    Write-Host "=== denoise (DPDFNet baseline) ==="
    Ensure-Dir $denoiseDest
    Fetch "$SherpaBase/speech-enhancement-models/dpdfnet_baseline.onnx" $denoiseOut
    Write-Host "  -> $denoiseOut"
}

# Speaker
$speakerDest = Join-Path $Dest "speaker"
$speakerOut = Join-Path $speakerDest "3dspeaker.onnx"
if (Test-Path $speakerOut) {
    Write-Host "=== speaker: already present ==="
} else {
    Write-Host "=== speaker (3D-Speaker campplus zh+en) ==="
    Ensure-Dir $speakerDest
    $srcName = "3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx"
    $srcTmp = Join-Path $Tmp $srcName
    Fetch "$SherpaBase/speaker-recongition-models/$srcName" $srcTmp
    Copy-Item $srcTmp $speakerOut
    Write-Host "  -> $speakerOut"
}

function Install-ToTalkHome {
    if ($env:SKIP_TALK_INSTALL -eq "1") { return }
    $talkHome = if ($env:HERMES_HOME) {
        Join-Path $env:HERMES_HOME "hermes-talk\models"
    } elseif ($env:USERPROFILE) {
        Join-Path $env:USERPROFILE ".hermes-agent-ultra\hermes-talk\models"
    } else { $null }
    if (-not $talkHome) { return }
    Write-Host "=== install to talk home: $talkHome ==="
    foreach ($sub in @("sensevoice", "kokoro", "kws-zh-en", "vad", "denoise", "speaker")) {
        $src = Join-Path $Dest $sub
        if (-not (Test-Path $src)) { continue }
        $dst = Join-Path $talkHome $sub
        New-Item -ItemType Directory -Path $dst -Force | Out-Null
        Copy-Item -Path (Join-Path $src "*") -Destination $dst -Recurse -Force
    }
    Write-Host "  -> $talkHome"
}

Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue

Install-ToTalkHome

Write-Host ""
Write-Host "=== Done ==="
Write-Host "Models installed under $Dest"
Write-Host "Packaging: make package-talk-windows (MODELS_ROOT=$ModelsRoot)"
