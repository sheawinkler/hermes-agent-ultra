#Requires -Version 5.1
<#
.SYNOPSIS
  Start (or stop) Hermes Agent Ultra gateway on Windows with stable paths and logging.

.DESCRIPTION
  - Uses %LOCALAPPDATA%\hermes-agent-ultra as HERMES_HOME (migrates from legacy \hermes if needed).
  - Writes logs to %LOCALAPPDATA%\hermes-agent-ultra\logs\hermes.log
  - Stops conflicting Python `hermes gateway` processes before start.
  - Does NOT change streaming or other config — only process/env setup.

.EXAMPLE
  .\scripts\start-gateway-ultra.ps1

.EXAMPLE
  .\scripts\start-gateway-ultra.ps1 -Stop

.EXAMPLE
  .\scripts\start-gateway-ultra.ps1 -VerboseLog
#>
[CmdletBinding()]
param(
    [switch] $Stop,
    [switch] $VerboseLog,
    [string] $HermesHome = $(Join-Path $env:LOCALAPPDATA 'hermes-agent-ultra'),
    [string] $Binary = ''
)

$ErrorActionPreference = 'Stop'

function Ensure-MigratedHermesHome {
    param([string] $TargetHome)
    $legacyHome = Join-Path $env:LOCALAPPDATA 'hermes'
    if (Test-Path -LiteralPath $TargetHome) {
        return $TargetHome
    }
    if (Test-Path -LiteralPath $legacyHome) {
        Write-Host "迁移 Hermes 数据: $legacyHome -> $TargetHome"
        Copy-Item -LiteralPath $legacyHome -Destination $TargetHome -Recurse -Force
        return $TargetHome
    }
    New-Item -ItemType Directory -Force -Path $TargetHome | Out-Null
    return $TargetHome
}

function Resolve-HermesUltraBinary {
    param([string] $Override)
    if ($Override -and (Test-Path -LiteralPath $Override)) {
        return (Resolve-Path -LiteralPath $Override).Path
    }
    $candidates = @(
        (Join-Path $PSScriptRoot '..\target\release\hermes-agent-ultra.exe'),
        (Join-Path $PSScriptRoot '..\target\debug\hermes-agent-ultra.exe')
    )
    foreach ($path in $candidates) {
        $full = [System.IO.Path]::GetFullPath($path)
        if (Test-Path -LiteralPath $full) {
            return $full
        }
    }
    $onPath = Get-Command 'hermes-agent-ultra.exe' -ErrorAction SilentlyContinue
    if ($onPath) {
        return $onPath.Source
    }
    throw "找不到 hermes-agent-ultra.exe。请先执行: cargo build -p hermes-cli --release"
}

function Get-GatewayLikeProcesses {
    $patterns = @(
        'hermes_cli\\main\.py gateway',
        'hermes_cli/main\.py gateway',
        'hermes-agent-ultra.* gateway',
        'hermes-ultra.* gateway',
        'hermes gateway run'
    )
    $regex = '(' + ($patterns -join '|') + ')'
    Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
        Where-Object { $_.CommandLine -and ($_.CommandLine -match $regex) }
}

function Stop-ConflictingGateways {
    param([string] $KeepPid)
    $stopped = 0
    foreach ($proc in Get-GatewayLikeProcesses) {
        if ($KeepPid -and $proc.ProcessId -eq [int]$KeepPid) {
            continue
        }
        Write-Host "停止冲突网关进程 PID $($proc.ProcessId)"
        Stop-Process -Id $proc.ProcessId -Force -ErrorAction SilentlyContinue
        $stopped++
    }
    $pidFile = Join-Path $HermesHome 'gateway.pid'
    if (Test-Path -LiteralPath $pidFile) {
        $raw = (Get-Content -LiteralPath $pidFile -Raw).Trim()
        if ($raw -match '^\d+$') {
            $oldPid = [int]$raw
            if ((-not $KeepPid) -or ($oldPid -ne [int]$KeepPid)) {
                Stop-Process -Id $oldPid -Force -ErrorAction SilentlyContinue
                $stopped++
            }
        }
        Remove-Item -LiteralPath $pidFile -Force -ErrorAction SilentlyContinue
    }
    return $stopped
}

$exe = Resolve-HermesUltraBinary -Override $Binary
$HermesHome = Ensure-MigratedHermesHome -TargetHome $HermesHome
$logDir = Join-Path $HermesHome 'logs'
$logFile = Join-Path $logDir 'hermes.log'
New-Item -ItemType Directory -Force -Path $logDir | Out-Null

if ($Stop) {
    $n = Stop-ConflictingGateways
    Write-Host "已停止 $n 个网关相关进程。日志: $logFile"
    exit 0
}

if (-not (Test-Path -LiteralPath (Join-Path $HermesHome 'config.yaml'))) {
    Write-Warning "未找到 $HermesHome\config.yaml — 请确认 Discord 配置目录是否正确。"
}

Stop-ConflictingGateways | Out-Null

$env:HERMES_HOME = $HermesHome
$env:HERMES_LOG_FILE = $logFile

# Discord 需访问 discord.com；国内/Clash 假 IP 时走本地代理（Clash 常见 mixed 7897）
if (-not $env:DISCORD_PROXY) {
    foreach ($port in @(7897, 7890, 10808)) {
        try {
            $open = Test-NetConnection -ComputerName 127.0.0.1 -Port $port -WarningAction SilentlyContinue -ErrorAction SilentlyContinue |
                Select-Object -ExpandProperty TcpTestSucceeded
            if ($open) {
                $env:DISCORD_PROXY = "http://127.0.0.1:$port"
                Write-Host "DISCORD_PROXY = $($env:DISCORD_PROXY) (检测到本地代理端口 $port)"
                break
            }
        } catch { }
    }
}
if ($VerboseLog) {
    $env:RUST_LOG = 'info,hermes_gateway::platforms::discord=debug,hermes_gateway=debug'
} else {
    $env:RUST_LOG = 'info,hermes_gateway::platforms::discord=info,hermes_cli=warn'
}

Write-Host "HERMES_HOME   = $HermesHome"
Write-Host "HERMES_LOG_FILE = $logFile"
Write-Host "二进制        = $exe"
Write-Host "启动: gateway run （Ctrl+C 停止）"
Write-Host ""

& $exe -C $HermesHome gateway run
