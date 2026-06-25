# Download sherpa-onnx native runtime for hermes-talk GPU execution providers.
param(
    [ValidateSet("cpu", "cuda", "directml", "coreml")]
    [string]$Ep = "cpu"
)

$ErrorActionPreference = "Stop"
$Root = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$Version = "1.13.3"
$Base = "https://github.com/k2-fsa/sherpa-onnx/releases/download/v$Version"
$Cache = if ($env:SHERPA_ONNX_CACHE) { $env:SHERPA_ONNX_CACHE } else { Join-Path $Root ".cross-cache\sherpa-onnx" }

function Get-ArchiveName {
    param([string]$Provider)
    switch ($Provider) {
        "cpu" {
            return "sherpa-onnx-v$Version-win-x64-static-MT-Release-lib.tar.bz2"
        }
        "cuda" {
            return "sherpa-onnx-v$Version-cuda-12.x-cudnn-9.x-win-x64-cuda.tar.bz2"
        }
        "coreml" {
            throw "CoreML requires macOS"
        }
        "directml" {
            throw @"
DirectML has no official sherpa-onnx prebuilt archive.
Build sherpa-onnx with -DSHERPA_ONNX_ENABLE_DIRECTML=ON, then:
  `$env:SHERPA_ONNX_LIB_DIR = 'C:\path\to\lib'
"@
        }
    }
}

$Archive = Get-ArchiveName -Provider $Ep
$Dest = Join-Path $Cache $Ep
$Stem = $Archive -replace '\.tar\.bz2$',''
$LibDir = Join-Path (Join-Path $Dest $Stem) "lib"

if (Test-Path $LibDir) {
    Write-Host "sherpa-onnx $Ep runtime already at $LibDir"
    Write-Host "`$env:SHERPA_ONNX_LIB_DIR = '$LibDir'"
    exit 0
}

New-Item -ItemType Directory -Force -Path $Dest | Out-Null
$Tmp = Join-Path $Dest $Archive
if (-not (Test-Path $Tmp)) {
    Write-Host "Downloading $Base/$Archive"
    Invoke-WebRequest -Uri "$Base/$Archive" -OutFile $Tmp
}

tar -xjf $Tmp -C $Dest
if (-not (Test-Path $LibDir)) {
    throw "expected lib/ under $(Join-Path $Dest $Stem)"
}

Write-Host "sherpa-onnx $Ep runtime ready at $LibDir"
Write-Host "`$env:SHERPA_ONNX_LIB_DIR = '$LibDir'"
