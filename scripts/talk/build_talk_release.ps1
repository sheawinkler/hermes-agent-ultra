# Release-build hermes-cli with talk feature and sherpa-onnx pack env (Windows).
param(
    [Parameter(Mandatory = $true)]
    [string]$Pack,
    [string]$LibDir = "",
    [string]$Root = ""
)

$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "proxy_env.ps1")
Initialize-TalkProxy | Out-Null

if (-not $Root) {
    $Root = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
}
Set-Location $Root

$env:SHERPA_ONNX_PACK = $Pack
if ($LibDir -and (Test-Path $LibDir)) {
    $env:SHERPA_ONNX_LIB_DIR = (Resolve-Path $LibDir).Path
    Write-Host "SHERPA_ONNX_LIB_DIR=$($env:SHERPA_ONNX_LIB_DIR)"
}

Write-Host "SHERPA_ONNX_PACK=$Pack"
cargo build --release -p hermes-cli --features talk --bin hermes-agent-ultra
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}
