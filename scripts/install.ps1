[CmdletBinding()]
param(
    [string]$Version = "",
    [string]$InstallDir = "",
    [string]$HermesHome = "",
    [string]$Repository = "",
    [string]$RunSetupMode = "",
    [switch]$Setup,
    [switch]$SkipSetup,
    [switch]$InstallLegacyAlias,
    [string]$CanonicalBinName = "hermes-agent-ultra.exe",
    [string]$PrimaryBinName = "hermes-ultra.exe",
    [string]$LegacyBinName = "hermes.exe",
    [string]$ReleaseBinName = "hermes.exe"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$DefaultSoulMd = "You are Hermes Agent, an intelligent AI assistant created by Nous Research. You are helpful, knowledgeable, and direct. You assist users with a wide range of tasks including answering questions, writing and editing code, analyzing information, creative work, and executing actions via tools. You communicate clearly, admit uncertainty when appropriate, and prioritize being genuinely useful over being verbose."

function Write-Info {
    param([Parameter(Mandatory = $true)][string]$Message)
    Write-Host $Message
}

function Write-Warn {
    param([Parameter(Mandatory = $true)][string]$Message)
    Write-Warning $Message
}

function Test-TruthyEnvironmentValue {
    param([string]$Value)
    if ([string]::IsNullOrWhiteSpace($Value)) {
        return $false
    }

    return @("1", "true", "yes", "on") -contains $Value.Trim().ToLowerInvariant()
}

function Resolve-InstallDir {
    param([string]$RequestedInstallDir)

    if (-not [string]::IsNullOrWhiteSpace($RequestedInstallDir)) {
        return $RequestedInstallDir
    }
    if (-not [string]::IsNullOrWhiteSpace($env:HERMES_INSTALL_DIR)) {
        return $env:HERMES_INSTALL_DIR
    }
    if (-not [string]::IsNullOrWhiteSpace($env:INSTALL_DIR)) {
        return $env:INSTALL_DIR
    }
    if (-not [string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        return (Join-Path $env:LOCALAPPDATA "Hermes\\bin")
    }

    return (Join-Path $HOME ".local\\bin")
}

function Resolve-HermesHome {
    param([string]$RequestedHermesHome)

    if (-not [string]::IsNullOrWhiteSpace($RequestedHermesHome)) {
        return $RequestedHermesHome
    }
    if (-not [string]::IsNullOrWhiteSpace($env:HERMES_HOME)) {
        return $env:HERMES_HOME
    }

    return (Join-Path $HOME ".hermes")
}

function Resolve-Repository {
    param([string]$RequestedRepository)

    if (-not [string]::IsNullOrWhiteSpace($RequestedRepository)) {
        return $RequestedRepository
    }
    if (-not [string]::IsNullOrWhiteSpace($env:REPO)) {
        return $env:REPO
    }

    return "sheawinkler/hermes-agent-ultra"
}

function Resolve-Version {
    param([string]$RequestedVersion)

    if (-not [string]::IsNullOrWhiteSpace($RequestedVersion)) {
        return $RequestedVersion
    }
    if (-not [string]::IsNullOrWhiteSpace($env:VERSION)) {
        return $env:VERSION
    }

    return "latest"
}

function Resolve-RunSetupMode {
    param(
        [string]$RequestedMode,
        [switch]$ForceSetup,
        [switch]$ForceSkipSetup
    )

    if ($ForceSetup) {
        return "always"
    }
    if ($ForceSkipSetup) {
        return "never"
    }

    $mode = $RequestedMode
    if ([string]::IsNullOrWhiteSpace($mode)) {
        $mode = $env:RUN_SETUP_MODE
    }
    if ([string]::IsNullOrWhiteSpace($mode)) {
        return "auto"
    }

    $normalized = $mode.Trim().ToLowerInvariant()
    if (@("auto", "always", "never") -notcontains $normalized) {
        throw "RUN_SETUP_MODE must be auto, always, or never; got '$mode'."
    }

    return $normalized
}

function Assert-BinaryFileName {
    param([Parameter(Mandatory = $true)][string]$Name)

    if ([string]::IsNullOrWhiteSpace($Name) -or $Name -eq "." -or $Name -eq ".." -or [System.IO.Path]::GetFileName($Name) -ne $Name) {
        throw "Binary names must be plain file names; got '$Name'."
    }
}

function Get-TargetAsset {
    $architecture = $env:PROCESSOR_ARCHITEW6432
    if ([string]::IsNullOrWhiteSpace($architecture)) {
        $architecture = $env:PROCESSOR_ARCHITECTURE
    }
    if ([string]::IsNullOrWhiteSpace($architecture)) {
        throw "Could not determine Windows processor architecture."
    }

    switch ($architecture.Trim().ToUpperInvariant()) {
        "AMD64" { return "hermes-windows-x86_64.zip" }
        "X86_64" { return "hermes-windows-x86_64.zip" }
        default { throw "Unsupported Windows architecture '$architecture'. Release artifacts currently support x86_64 Windows." }
    }
}

function Get-ReleaseUrl {
    param(
        [Parameter(Mandatory = $true)][string]$Repo,
        [Parameter(Mandatory = $true)][string]$ReleaseVersion,
        [Parameter(Mandatory = $true)][string]$Asset
    )

    if ($ReleaseVersion -eq "latest") {
        return "https://github.com/$Repo/releases/latest/download/$Asset"
    }

    return "https://github.com/$Repo/releases/download/$ReleaseVersion/$Asset"
}

function Enable-Tls12 {
    try {
        [System.Net.ServicePointManager]::SecurityProtocol = [System.Net.ServicePointManager]::SecurityProtocol -bor [System.Net.SecurityProtocolType]::Tls12
    } catch {
        Write-Warn "Could not enable TLS 1.2 explicitly; using the system default TLS policy."
    }
}

function Add-InstallDirToCurrentProcessPath {
    param([Parameter(Mandatory = $true)][string]$Directory)

    $currentPath = [string]$env:Path
    $normalizedDirectory = $Directory.TrimEnd("\\")
    $entries = @($currentPath -split ";" | ForEach-Object { $_.TrimEnd("\\") })
    if ($entries -notcontains $normalizedDirectory) {
        $env:Path = "$Directory;$currentPath"
        Write-Info "PATH guard applied for this install process: $Directory"
    }
}

function Get-InstallProbeTimeoutSeconds {
    $value = $env:HERMES_INSTALL_PROBE_TIMEOUT_SECONDS
    if ([string]::IsNullOrWhiteSpace($value)) {
        return 45
    }

    $seconds = 0
    if (-not [int]::TryParse($value, [ref]$seconds) -or $seconds -lt 0) {
        Write-Warn "Invalid HERMES_INSTALL_PROBE_TIMEOUT_SECONDS '$value'; using 45 seconds."
        return 45
    }

    return $seconds
}

function Invoke-BoundedInstalledCommand {
    param(
        [Parameter(Mandatory = $true)][string]$BinaryPath,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter(Mandatory = $true)][string]$Label
    )

    $timeoutSeconds = Get-InstallProbeTimeoutSeconds
    if ($timeoutSeconds -eq 0) {
        & $BinaryPath @Arguments
        if ($LASTEXITCODE -ne 0) {
            Write-Warn "$Label exited with status $LASTEXITCODE; continuing."
        }
        return
    }

    $startInfo = New-Object System.Diagnostics.ProcessStartInfo
    $startInfo.FileName = $BinaryPath
    $startInfo.Arguments = [string]::Join(" ", $Arguments)
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true

    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $startInfo
    try {
        if (-not $process.Start()) {
            Write-Warn "$Label did not start; continuing."
            return
        }
        if (-not $process.WaitForExit($timeoutSeconds * 1000)) {
            try { $process.Kill() } catch { }
            Write-Warn "$Label timed out after $timeoutSeconds seconds; continuing. Run manually: $BinaryPath $($Arguments -join ' ')"
            return
        }
        if ($process.ExitCode -ne 0) {
            Write-Warn "$Label exited with status $($process.ExitCode); continuing."
        }
    } catch {
        Write-Warn "$Label could not run: $($_.Exception.Message). Continuing."
    } finally {
        $process.Dispose()
    }
}

function Test-InteractiveConsole {
    try {
        return (-not [Console]::IsInputRedirected) -and (-not [Console]::IsOutputRedirected)
    } catch {
        return $false
    }
}

function Invoke-PostInstallFlow {
    param(
        [Parameter(Mandatory = $true)][string]$BinaryPath,
        [Parameter(Mandatory = $true)][string]$Mode
    )

    if ($Mode -eq "never") {
        Write-Info "Post-install setup skipped. Run later: $BinaryPath setup"
        return
    }
    if ($Mode -eq "auto" -and -not (Test-InteractiveConsole)) {
        Write-Info "Post-install setup skipped (non-interactive install). Run later: $BinaryPath setup"
        return
    }

    Write-Info "Running post-install verification..."
    Invoke-BoundedInstalledCommand -BinaryPath $BinaryPath -Arguments @("doctor") -Label "doctor"
    Write-Info "Current auth/platform status:"
    Invoke-BoundedInstalledCommand -BinaryPath $BinaryPath -Arguments @("auth", "status") -Label "auth status"

    $runSetup = $Mode -eq "always"
    if ($Mode -eq "auto") {
        $response = Read-Host "Run interactive setup now? [Y/n]"
        $runSetup = [string]::IsNullOrWhiteSpace($response) -or @("y", "yes") -contains $response.Trim().ToLowerInvariant()
    }
    if ($runSetup) {
        Invoke-BoundedInstalledCommand -BinaryPath $BinaryPath -Arguments @("setup") -Label "setup"
    } else {
        Write-Info "Skipped setup. Run later: $BinaryPath setup"
    }
}

function Install-HermesRelease {
    param(
        [Parameter(Mandatory = $true)][string]$Repo,
        [Parameter(Mandatory = $true)][string]$ReleaseVersion,
        [Parameter(Mandatory = $true)][string]$DestinationDirectory,
        [Parameter(Mandatory = $true)][string]$HomeDirectory,
        [Parameter(Mandatory = $true)][string]$CanonicalName,
        [Parameter(Mandatory = $true)][string]$PrimaryName,
        [Parameter(Mandatory = $true)][string]$LegacyName,
        [Parameter(Mandatory = $true)][string]$PackagedName,
        [Parameter(Mandatory = $true)][string]$SetupMode,
        [switch]$InstallLegacy
    )

    foreach ($name in @($CanonicalName, $PrimaryName, $LegacyName, $PackagedName)) {
        Assert-BinaryFileName -Name $name
    }

    $asset = Get-TargetAsset
    $url = Get-ReleaseUrl -Repo $Repo -ReleaseVersion $ReleaseVersion -Asset $asset
    $temporaryRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("hermes-release-install-" + [Guid]::NewGuid().ToString("N"))
    $archivePath = Join-Path $temporaryRoot $asset
    $extractDirectory = Join-Path $temporaryRoot "extract"

    try {
        New-Item -ItemType Directory -Force -Path $temporaryRoot | Out-Null
        Write-Info "Installing hermes-agent-ultra from $url"
        Enable-Tls12
        Invoke-WebRequest -Uri $url -OutFile $archivePath -UseBasicParsing -Headers @{ "User-Agent" = "hermes-agent-ultra-installer" }
        if (-not (Test-Path -LiteralPath $archivePath -PathType Leaf)) {
            throw "Release asset download produced no file: $url"
        }

        Expand-Archive -LiteralPath $archivePath -DestinationPath $extractDirectory -Force
        $sourceBinary = Get-ChildItem -LiteralPath $extractDirectory -File -Recurse -Filter $PackagedName | Select-Object -First 1
        if ($null -eq $sourceBinary) {
            throw "Release asset $asset did not contain $PackagedName."
        }

        New-Item -ItemType Directory -Force -Path $DestinationDirectory | Out-Null
        $canonicalPath = Join-Path $DestinationDirectory $CanonicalName
        Copy-Item -LiteralPath $sourceBinary.FullName -Destination $canonicalPath -Force
        if (-not (Test-Path -LiteralPath $canonicalPath -PathType Leaf)) {
            throw "Installed binary was not created at $canonicalPath."
        }
        try { Unblock-File -LiteralPath $canonicalPath -ErrorAction Stop } catch { }

        if ($PrimaryName -ne $CanonicalName) {
            Copy-Item -LiteralPath $canonicalPath -Destination (Join-Path $DestinationDirectory $PrimaryName) -Force
        }
        if ($InstallLegacy -and $LegacyName -ne $CanonicalName) {
            Copy-Item -LiteralPath $canonicalPath -Destination (Join-Path $DestinationDirectory $LegacyName) -Force
        } else {
            Write-Info "Legacy hermes alias not installed by default; existing upstream hermes commands are left untouched."
        }

        New-Item -ItemType Directory -Force -Path $HomeDirectory | Out-Null
        $soulPath = Join-Path $HomeDirectory "SOUL.md"
        if (-not (Test-Path -LiteralPath $soulPath -PathType Leaf)) {
            $utf8WithoutBom = New-Object System.Text.UTF8Encoding($false)
            [System.IO.File]::WriteAllText($soulPath, $DefaultSoulMd + [Environment]::NewLine, $utf8WithoutBom)
        }

        Add-InstallDirToCurrentProcessPath -Directory $DestinationDirectory
        Write-Info "Installed to $canonicalPath"
        Invoke-PostInstallFlow -BinaryPath $canonicalPath -Mode $SetupMode
    } finally {
        if (Test-Path -LiteralPath $temporaryRoot) {
            Remove-Item -LiteralPath $temporaryRoot -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

try {
    $resolvedRepository = Resolve-Repository -RequestedRepository $Repository
    $resolvedVersion = Resolve-Version -RequestedVersion $Version
    $resolvedInstallDir = Resolve-InstallDir -RequestedInstallDir $InstallDir
    $resolvedHermesHome = Resolve-HermesHome -RequestedHermesHome $HermesHome
    $resolvedSetupMode = Resolve-RunSetupMode -RequestedMode $RunSetupMode -ForceSetup:$Setup -ForceSkipSetup:$SkipSetup
    $shouldInstallLegacyAlias = $InstallLegacyAlias -or (Test-TruthyEnvironmentValue -Value $env:INSTALL_LEGACY_ALIAS)

    $installArguments = @{
        Repo = $resolvedRepository
        ReleaseVersion = $resolvedVersion
        DestinationDirectory = $resolvedInstallDir
        HomeDirectory = $resolvedHermesHome
        CanonicalName = $CanonicalBinName
        PrimaryName = $PrimaryBinName
        LegacyName = $LegacyBinName
        PackagedName = $ReleaseBinName
        SetupMode = $resolvedSetupMode
        InstallLegacy = $shouldInstallLegacyAlias
    }
    Install-HermesRelease @installArguments
} catch {
    [Console]::Error.WriteLine("Install failed: $($_.Exception.Message)")
    exit 1
}
