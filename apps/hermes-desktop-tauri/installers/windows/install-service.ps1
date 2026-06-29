param(
    [Parameter(Mandatory = $false)]
    [string]$ServiceName = "TerraHermesHttp",
    [Parameter(Mandatory = $false)]
    [string]$BinaryPath = ""
)

$ErrorActionPreference = "Stop"

if (-not (Get-Command sc.exe -ErrorAction SilentlyContinue)) {
    throw "sc.exe not found"
}

if ([string]::IsNullOrWhiteSpace($BinaryPath)) {
    $installRoot = Split-Path -Parent $PSScriptRoot
    $repoRoot = Resolve-Path (Join-Path $installRoot "..\..\..")
    $BinaryPath = Join-Path $repoRoot "target\release\hermes-http.exe"
    if (-not (Test-Path $BinaryPath)) {
        $BinaryPath = Join-Path $repoRoot "target\debug\hermes-http.exe"
    }
}

if (-not (Test-Path $BinaryPath)) {
    throw "hermes-http binary not found at $BinaryPath"
}

$resolved = (Resolve-Path $BinaryPath).Path
$existing = sc.exe query $ServiceName 2>$null
if ($LASTEXITCODE -eq 0) {
    sc.exe stop $ServiceName | Out-Null
    Start-Sleep -Seconds 1
    sc.exe delete $ServiceName | Out-Null
    Start-Sleep -Seconds 1
}

sc.exe create $ServiceName binPath= "`"$resolved`"" type= user start= auto
if ($LASTEXITCODE -ne 0) { throw "sc.exe create failed" }

sc.exe description $ServiceName "Terra Hermes HTTP backend service"
sc.exe start $ServiceName
if ($LASTEXITCODE -ne 0) { throw "sc.exe start failed" }

Write-Host "Installed $ServiceName -> $resolved"
