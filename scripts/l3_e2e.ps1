# Windows entry point for scripts/l3_e2e.sh.
#
# A wrapper rather than a PowerShell port: the harness is ~300 lines of sequencing, JSON picking and
# an EXIT trap that puts the daemon's serial port back, and two copies of that would drift. The one
# that drifted would be the one nobody ran, and it would drift silently, because nothing in CI runs
# either (CI has no hardware).
#
# Git Bash ships with Git, which this repo already requires.
#
# Usage: .\scripts\l3_e2e.ps1 [-Port COM4] [-Url ws://...] [-Base http://...] [-SkipSerial]

[CmdletBinding()]
param(
    [string]$Port,
    [string]$Url,
    [string]$Base,
    [switch]$SkipSerial
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$script = Join-Path $PSScriptRoot "l3_e2e.sh"

$bash = (Get-Command bash -ErrorAction SilentlyContinue).Source
if (-not $bash) {
    foreach ($candidate in @(
            "$env:ProgramFiles\Git\bin\bash.exe",
            "${env:ProgramFiles(x86)}\Git\bin\bash.exe",
            "$env:LOCALAPPDATA\Programs\Git\bin\bash.exe")) {
        if (Test-Path $candidate) { $bash = $candidate; break }
    }
}
if (-not $bash) {
    Write-Error "bash not found. Install Git for Windows, or run scripts/l3_e2e.sh from Git Bash / WSL."
    exit 2
}

$bashArgs = @($script)
if ($Port) { $bashArgs += @("--port", $Port) }
if ($Url) { $bashArgs += @("--url", $Url) }
if ($Base) { $bashArgs += @("--base", $Base) }
if ($SkipSerial) { $bashArgs += "--skip-serial" }

Push-Location $repoRoot
try {
    & $bash @bashArgs
    # The harness exits 1 when a check failed and 2 when it could not start; pass that through
    # unchanged, so a caller can tell "the cart is broken" from "the run never happened".
    exit $LASTEXITCODE
}
finally {
    Pop-Location
}
