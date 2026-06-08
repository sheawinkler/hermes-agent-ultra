# Scan ~/.hermes-agent-ultra/skills for SkillGuard violations (CI / local audit).
param(
    [string]$SkillsDir = "$env:USERPROFILE\.hermes-agent-ultra\skills",
    [string]$Mode = "strict"
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)

if (-not (Test-Path $SkillsDir)) {
    Write-Host "No skills directory at $SkillsDir — nothing to scan."
    exit 0
}

$env:HERMES_SKILL_GUARD_MODE = $Mode
Write-Host "Scanning skills in $SkillsDir (mode=$Mode)..."

Push-Location $repoRoot
try {
    cargo run -q -p hermes-cli -- skills audit $SkillsDir 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Error "skills guard scan failed (exit $LASTEXITCODE)"
    }
} finally {
    Pop-Location
}

Write-Host "skills guard scan OK"
