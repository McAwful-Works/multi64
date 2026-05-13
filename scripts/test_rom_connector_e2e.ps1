# Hardware E2E for multi64-test-connector (requires multi64d + USB cart + test ROM in M64T_PROTO or BENCH).
# Reference transport: multi64d with SummerCart64 L2. See docs/connectors/test-rom.md.
# Usage (from repo root): .\scripts\test_rom_connector_e2e.ps1
# Env: MULTI64_WS_URL, MULTI64_RECV_TIMEOUT_SECS (optional)

$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $RepoRoot

$url = if ($env:MULTI64_WS_URL) { $env:MULTI64_WS_URL } else { "ws://127.0.0.1:38765/ws" }

$base = @("run", "-p", "multi64-test-connector", "--release", "--", "--url", $url)
if ($env:MULTI64_RECV_TIMEOUT_SECS) {
    $base += @("--recv-timeout-secs", $env:MULTI64_RECV_TIMEOUT_SECS)
}

function Invoke-ConnectorStep {
    param([Parameter(Mandatory = $true)][string[]]$StepArgs)
    Write-Host "==> multi64-test-connector $($StepArgs -join ' ')"
    & cargo @base @StepArgs
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
}

Invoke-ConnectorStep @("ping")
Invoke-ConnectorStep @("version")
Invoke-ConnectorStep @("echo", "--text", "Multi64 e2e")
Invoke-ConnectorStep @("req-controller")
Invoke-ConnectorStep @("rumble", "--port", "0", "--frames", "60")
Invoke-ConnectorStep @("display-text", "--text", "Multi64 e2e")
Invoke-ConnectorStep @("session-open")
Invoke-ConnectorStep @("eeprom-info")
Invoke-ConnectorStep @("eeprom-read", "--offset", "0", "--len", "16")
Invoke-ConnectorStep @("eeprom-write", "--offset", "0", "--hex", "0102030405060708")
Invoke-ConnectorStep @("sram-info")
Invoke-ConnectorStep @("session-close")
Invoke-ConnectorStep @("listen", "--duration-secs", "2")

Write-Host "OK: test ROM connector E2E script finished."
