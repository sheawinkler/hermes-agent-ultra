# Desktop sherpa launcher (Windows): init Hermes home + talk config/models, then run.
$ErrorActionPreference = "Stop"

$BundleDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$UserHome = if ($env:USERPROFILE) { $env:USERPROFILE } else { $env:HOME }
if (-not $UserHome) { $UserHome = $env:TEMP }

$env:HERMES_HOME = if ($env:HERMES_HOME) { $env:HERMES_HOME } else { Join-Path $UserHome ".hermes-agent-ultra" }
$env:HERMES_TALK_BUNDLE_DIR = $BundleDir

$TalkHome = Join-Path $env:HERMES_HOME "hermes-talk"
$HermesConfig = Join-Path $env:HERMES_HOME "config.yaml"
$TalkConfig = Join-Path $TalkHome "config.toml"
$Bin = Join-Path $BundleDir "bin\hermes-agent-ultra.exe"

function Ensure-Dir([string]$Path) {
    if (-not (Test-Path $Path)) {
        New-Item -ItemType Directory -Path $Path -Force | Out-Null
    }
}

function Copy-TreeIfMissing([string]$Src, [string]$Dst) {
    if (-not (Test-Path $Src)) { return }
    if (Test-Path $Dst) { return }
    Copy-Item -Path $Src -Destination $Dst -Recurse -Force
}

Ensure-Dir $env:HERMES_HOME
foreach ($sub in @("profiles", "sessions", "logs", "skills", "cron", "cache")) {
    Ensure-Dir (Join-Path $env:HERMES_HOME $sub)
}
Ensure-Dir $TalkHome

Copy-TreeIfMissing (Join-Path $BundleDir "models") (Join-Path $TalkHome "models")

if (-not (Test-Path $HermesConfig)) {
    Copy-Item (Join-Path $BundleDir "config.example.yaml") $HermesConfig -Force
    Write-Host "Initialized $HermesConfig"
}
if (-not (Test-Path $TalkConfig)) {
    Copy-Item (Join-Path $BundleDir "config.example.toml") $TalkConfig -Force
    Write-Host "Initialized $TalkConfig"
}

foreach ($d in @(
    "models\sensevoice",
    "models\kokoro",
    "models\kws-zh-en",
    "models\vad"
)) {
    if (-not (Test-Path (Join-Path $BundleDir $d))) {
        Write-Warning "missing bundle path: $(Join-Path $BundleDir $d)"
    }
}

if (-not (Test-Path $Bin)) {
    throw "missing binary: $Bin"
}

Write-Host "HERMES_HOME=$($env:HERMES_HOME)"
& $Bin talk run @args
