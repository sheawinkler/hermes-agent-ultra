$ErrorActionPreference = "Stop"
$root = Join-Path $PSScriptRoot "..\src-tauri\src"
$srcPath = Join-Path $root "commands.rs"
if (-not (Test-Path $srcPath)) {
    throw "commands.rs not found at $srcPath"
}
$allLines = [System.IO.File]::ReadAllLines($srcPath)

function Get-Slice([int]$Start, [int]$End) {
    if ($Start -gt $End) { throw "Invalid range ${Start}..${End}" }
    $segment = $allLines[($Start - 1)..($End - 1)]
    return (($segment -join [Environment]::NewLine) + [Environment]::NewLine)
}

function LineRange([int]$s, [int]$e) { return @{ s = $s; e = $e } }

$cmdDir = Join-Path $root "commands"
if (Test-Path $cmdDir) { Remove-Item -Recurse -Force $cmdDir }
New-Item -ItemType Directory -Force -Path $cmdDir | Out-Null

$header = "use super::*;" + [Environment]::NewLine + [Environment]::NewLine

$commandModules = @{
    backend = @(
        (LineRange 633 2100), (LineRange 2118 6069), (LineRange 6204 6543), (LineRange 6731 6878)
    )
    clipboard = @((LineRange 2101 2112))
    settings = @((LineRange 6070 6123))
    images = @((LineRange 6125 6167))
    preview = @((LineRange 6170 6201))
    terminal = @((LineRange 6544 6730))
}

foreach ($entry in $commandModules.GetEnumerator()) {
    $name = $entry.Key
    $body = $header
    foreach ($range in $entry.Value) {
        $body += Get-Slice $range.s $range.e
    }
    [System.IO.File]::WriteAllText((Join-Path $cmdDir "$name.rs"), $body)
}

$modNames = @("backend", "clipboard", "settings", "images", "preview", "terminal")
$mod = Get-Slice 1 631
$mod += Get-Slice 6880 8148
$mod += [Environment]::NewLine
foreach ($name in $modNames) {
    $mod += "mod $name;" + [Environment]::NewLine
}
$mod += [Environment]::NewLine
foreach ($name in $modNames) {
    $mod += "pub use ${name}::*;" + [Environment]::NewLine
}
$mod += [Environment]::NewLine
$mod += Get-Slice 8149 $allLines.Count

[System.IO.File]::WriteAllText((Join-Path $cmdDir "mod.rs"), $mod)
Remove-Item $srcPath
Write-Host "Split complete"
