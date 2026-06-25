# Download sherpa-onnx CPU static runtime for hermes-talk.
param(
    [ValidateSet("cpu", "auto")]
    [string]$Ep = "auto"
)

$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "proxy_env.ps1")
Initialize-TalkProxy | Out-Null

$Root = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$Version = "1.13.3"
$Base = "https://github.com/k2-fsa/sherpa-onnx/releases/download/v$Version"
$Cache = if ($env:SHERPA_ONNX_CACHE) { $env:SHERPA_ONNX_CACHE } else { Join-Path $Root ".cross-cache\sherpa-onnx" }

$Ep = "cpu"
$Archive = "sherpa-onnx-v$Version-win-x64-static-MT-Release-lib.tar.bz2"
$Dest = Join-Path $Cache $Ep
$Stem = $Archive -replace '\.tar\.bz2$',''
$LibDir = Join-Path (Join-Path $Dest $Stem) "lib"

if (Test-Path $LibDir) {
    Write-Host "sherpa-onnx pack=$Ep runtime already at $LibDir"
    Write-Host "`$env:SHERPA_ONNX_LIB_DIR = '$LibDir'"
    Write-Host "`$env:SHERPA_ONNX_PACK = '$Ep'"
    exit 0
}

New-Item -ItemType Directory -Force -Path $Dest | Out-Null
$Tmp = Join-Path $Dest $Archive
if (-not (Test-Path $Tmp)) {
    Write-Host "Downloading $Base/$Archive"
    Invoke-TalkWebRequest -Uri "$Base/$Archive" -OutFile $Tmp
}

tar -xjf $Tmp -C $Dest
if (-not (Test-Path $LibDir)) {
    throw "expected lib/ under $(Join-Path $Dest $Stem)"
}

Write-Host "sherpa-onnx pack=$Ep runtime ready at $LibDir"
Write-Host "`$env:SHERPA_ONNX_LIB_DIR = '$LibDir'"
Write-Host "`$env:SHERPA_ONNX_PACK = '$Ep'"
