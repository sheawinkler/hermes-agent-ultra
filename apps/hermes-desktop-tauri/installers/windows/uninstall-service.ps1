param(
    [Parameter(Mandatory = $false)]
    [string]$ServiceName = "TerraHermesHttp"
)

$ErrorActionPreference = "Stop"

if (-not (Get-Command sc.exe -ErrorAction SilentlyContinue)) {
    throw "sc.exe not found"
}

$existing = sc.exe query $ServiceName 2>$null
if ($LASTEXITCODE -ne 0) {
    Write-Host "Service $ServiceName is not installed"
    exit 0
}

sc.exe stop $ServiceName | Out-Null
Start-Sleep -Seconds 2
sc.exe delete $ServiceName | Out-Null

$logDir = Join-Path $env:LOCALAPPDATA "Terra\logs\hermes-http"
if (Test-Path $logDir) {
    Remove-Item -Recurse -Force $logDir -ErrorAction SilentlyContinue
}

Write-Host "Removed $ServiceName"
