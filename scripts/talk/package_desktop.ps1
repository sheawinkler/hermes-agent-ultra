# Package hermes-agent-ultra (talk / sherpa desktop) for Windows.
#
# Requires: make release-talk
#
# Usage:
#   powershell -File scripts/talk/package_desktop.ps1
#   $env:MODELS_ROOT = "C:\path\.models"; powershell -File scripts/talk/package_desktop.ps1
$ErrorActionPreference = "Stop"

$Root = if ($env:ROOT) { $env:ROOT } elseif ($env:HERMES_ULTRA_ROOT) { $env:HERMES_ULTRA_ROOT } else {
    (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
}
$Dist = if ($env:DIST_DIR) { $env:DIST_DIR } else { Join-Path $Root "target\dist" }
$ModelsRoot = if ($env:MODELS_ROOT) { $env:MODELS_ROOT } else { Join-Path $Root ".models" }
$Bin = if ($env:BIN_PATH) { $env:BIN_PATH } else { Join-Path $Root "target\release\hermes-agent-ultra.exe" }

function Resolve-UnderRoot([string]$Path) {
    if ([System.IO.Path]::IsPathRooted($Path)) { return $Path }
    return (Join-Path $Root ($Path -replace '/', '\'))
}

$Dist = Resolve-UnderRoot $Dist
$ModelsRoot = Resolve-UnderRoot $ModelsRoot
$Bin = Resolve-UnderRoot $Bin

$Out = Join-Path $Dist "hermes-talk-windows-x86_64"
$Archive = "$Out.zip"

function Ensure-Dir([string]$Path) {
    if (-not (Test-Path $Path)) {
        New-Item -ItemType Directory -Path $Path -Force | Out-Null
    }
}

function Copy-Models([string]$Name) {
    $src = Join-Path $ModelsRoot "models\$Name"
    $dst = Join-Path $Out "models\$Name"
    if (Test-Path $src) {
        Ensure-Dir $dst
        Copy-Item -Path (Join-Path $src "*") -Destination $dst -Recurse -Force
    } else {
        Write-Warning "missing $src"
    }
}

if (-not (Test-Path $Bin)) {
    throw "missing $Bin; run: make release-talk"
}

if (Test-Path $Out) {
    Remove-Item -Recurse -Force $Out
}
Ensure-Dir (Join-Path $Out "bin")
Ensure-Dir (Join-Path $Out "models")

Copy-Item $Bin (Join-Path $Out "bin\") -Force

foreach ($sub in @("sensevoice", "kokoro", "kws-zh-en", "vad", "denoise", "speaker")) {
    Copy-Models $sub
}

Copy-Item (Join-Path $Root "crates\hermes-talk\config.example.toml") (Join-Path $Out "config.example.toml") -Force
Copy-Item (Join-Path $Root "crates\hermes-config\config.example.yaml") (Join-Path $Out "config.example.yaml") -Force
Copy-Item (Join-Path $Root "scripts\talk\start_desktop.ps1") (Join-Path $Out "start.ps1") -Force

Write-Host "Bundled: $Out"

if (Test-Path $Archive) {
    Remove-Item -Force $Archive
}
$outName = Split-Path -Leaf $Out
$archiveName = "${outName}.zip"
Push-Location $Dist
try {
    & tar.exe -a -cf $archiveName $outName
    if ($LASTEXITCODE -ne 0) {
        Write-Warning "archive skipped (tar exit $LASTEXITCODE)"
    } else {
        Write-Host "Archive: $Archive"
    }
} catch {
    Write-Warning "archive skipped: $_"
} finally {
    Pop-Location
}
Write-Host "Run: cd $Out; powershell -File .\start.ps1"
