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

# Git Bash is looked for by path FIRST, before anything called "bash" on PATH. On most Windows
# machines that name is C:\Windows\System32\bash.exe, the WSL launcher, which is the wrong bash
# here twice over: it cannot open D:\... at all, and it would run Linux cargo and curl with no
# COM port behind them.
$bash = $null
foreach ($candidate in @(
        "$env:ProgramFiles\Git\bin\bash.exe",
        "${env:ProgramFiles(x86)}\Git\bin\bash.exe",
        "$env:LOCALAPPDATA\Programs\Git\bin\bash.exe")) {
    if (Test-Path $candidate) { $bash = $candidate; break }
}
if (-not $bash) {
    $onPath = (Get-Command bash -ErrorAction SilentlyContinue).Source
    if ($onPath -and $onPath -notlike "$env:WINDIR\*") { $bash = $onPath }
}
if (-not $bash) {
    Write-Error "Git Bash not found. Install Git for Windows, or run ./scripts/l3_e2e.sh from Git Bash directly. (WSL's bash will not work: it cannot reach the COM port.)"
    exit 2
}

# A relative path, because bash reads backslashes as escapes: handing it the absolute Windows path
# turned D:\Users\...\l3_e2e.sh into DUsers...l3_e2e.sh and it could not find the file. The
# Push-Location below is what makes this resolve.
$bashArgs = @("scripts/l3_e2e.sh")
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
