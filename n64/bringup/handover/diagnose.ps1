# Cart diagnostics for a remote tester: one run, one zip.
#
# Runs multi64d itself with full logging (debug, plus every byte read from the cart), so nothing
# depends on Multi64's in-memory Developer log. Two phases, each against a fresh daemon (phase 1
# sometimes two, below):
#
#   1. Control: multi64_test.z64, which moves USB through libdragon, and `multi64-test-connector
#      suite`. On an X7 this path passed on 2026-09-18 and on the morning of 2026-10-09, so a
#      failure here points at the tester's setup (cable, driver, port), not the agent's driver.
#      Before the suite, its first request goes out on its own, up to three times, through a
#      daemon started without --clear-serial. If none is answered, that daemon is stopped and a
#      second one started with --clear-serial, which purges the port when it opens it; the request
#      goes out again and the suite runs there. On 2026-10-09 an X7 sent nothing back through the
#      bridge until the suite's own direct-serial step had purged the port, and this shows whether
#      purging at bridge start is what fixes it. Doing it before the suite matters: the suite's
#      direct-serial step purges the port too, which would hide the answer.
#   2. Bring-up: multi64_bringup.z64 and `multi64-test-connector bringup --baseline`. On an X7,
#      when the CPU-word build gets no HELLO_ACK, the tester switches to the DMA build and it runs
#      again.
#
# Everything printed, every daemon's log, each daemon's status when it started and stopped, the
# phase 1 tries (phase1-probe.txt, and phase1-purged-probe.txt when a second daemon ran), and the
# machine's serial ports and drivers go into results-<time>\, which is zipped at the end.
# Written for Windows PowerShell 5.1, which every Windows 10/11 has.

param(
    # multi64d's --cart: ed64 (X7), ed64pro or sc64. Empty: the only X7 or SC64 plugged in, else ask.
    [string]$Cart = '',
    # Serial port. Empty: the only port with the cart's USB IDs, else ask.
    [string]$Port = '',
    # Skip phase 1, for a rerun of the bring-up alone.
    [switch]$SkipControl
)

$ErrorActionPreference = 'Stop'
# The tools print UTF-8. Windows PowerShell decodes a native program's output with the console's
# code page unless told otherwise, which turned an em dash into three junk characters in the log.
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$here = $PSScriptRoot
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$out = Join-Path $here "results-$stamp"
New-Item -ItemType Directory -Path $out | Out-Null
$session = Join-Path $out 'session.txt'
$base = 'http://127.0.0.1:38765'
$daemon = Join-Path $here 'multi64d.exe'
$tool = Join-Path $here 'multi64-test-connector.exe'
$baseline = Join-Path $here 'sc64-2026-10-08.json'

# Every multi64d this script starts goes into a Windows job that kills it when the job's last handle
# closes. The script holds that handle, so closing its window mid-run, or Ctrl+C, cannot leave a
# daemon holding the cart's port.
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class Multi64Job {
    [StructLayout(LayoutKind.Sequential)]
    struct BasicLimits {
        public long PerProcessUserTimeLimit, PerJobUserTimeLimit;
        public uint LimitFlags;
        public UIntPtr MinimumWorkingSetSize, MaximumWorkingSetSize;
        public uint ActiveProcessLimit;
        public UIntPtr Affinity;
        public uint PriorityClass, SchedulingClass;
    }
    [StructLayout(LayoutKind.Sequential)]
    struct IoCounters { public ulong a, b, c, d, e, f; }
    [StructLayout(LayoutKind.Sequential)]
    struct ExtendedLimits {
        public BasicLimits Basic;
        public IoCounters Io;
        public UIntPtr ProcessMemoryLimit, JobMemoryLimit, PeakProcessMemoryUsed, PeakJobMemoryUsed;
    }
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern IntPtr CreateJobObject(IntPtr attributes, string name);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool SetInformationJobObject(IntPtr job, int infoClass, ref ExtendedLimits info, uint length);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);
    static IntPtr job = IntPtr.Zero;
    public static bool Adopt(IntPtr process) {
        if (job == IntPtr.Zero) {
            job = CreateJobObject(IntPtr.Zero, null);
            ExtendedLimits info = new ExtendedLimits();
            info.Basic.LimitFlags = 0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            SetInformationJobObject(job, 9, ref info, (uint)Marshal.SizeOf(typeof(ExtendedLimits)));
        }
        return AssignProcessToJobObject(job, process);
    }
}
'@

function Say([string]$text) {
    Write-Host $text
    Add-Content -Encoding UTF8 -Path $session -Value $text
}

function Section([string]$title) {
    Say ''
    Say ('==== ' + $title + ' ' + ('=' * [Math]::Max(0, 60 - $title.Length)))
}

function Ask([string]$prompt) {
    Write-Host ''
    $answer = Read-Host $prompt
    Add-Content -Encoding UTF8 -Path $session -Value ("> " + $prompt + " [" + $answer + "]")
    return $answer
}

# Runs a native program and keeps what it printed, stderr included, in the session log and in its
# own file. Returns the exit code.
function Run-Logged([string]$file, [string]$exe, [string[]]$arguments) {
    $log = Join-Path $out $file
    Say ("$ " + (Split-Path $exe -Leaf) + " " + ($arguments -join ' '))
    $ErrorActionPreference = 'Continue'
    & $exe @arguments 2>&1 | ForEach-Object {
        $line = "$_"
        Write-Host $line
        Add-Content -Encoding UTF8 -Path $log -Value $line
        Add-Content -Encoding UTF8 -Path $session -Value $line
    }
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    Say "(exit code $code)"
    return $code
}

function Save-Status([string]$file) {
    try {
        $r = Invoke-WebRequest -UseBasicParsing -TimeoutSec 5 -Uri "$base/"
        Set-Content -Encoding UTF8 -Path (Join-Path $out $file) -Value $r.Content
        Say ("daemon status: " + $r.Content)
    } catch {
        Say ("daemon status: no answer (" + $_.Exception.Message + ")")
    }
}

# 1 when the daemon says it holds the cart's port, from GET / (serialActive).
function Port-Open {
    try {
        $r = Invoke-WebRequest -UseBasicParsing -TimeoutSec 5 -Uri "$base/"
        return ($r.Content -match '"serialActive":true')
    } catch { return $false }
}

# multi64d logs to stdout ($phase-multi64d.log); .err.log catches anything else. When the cart
# never comes up, its last warnings say why better than a guess here can: a port another program
# holds, a port that does not exist, or (on a PRO) a cart that did not answer its identity check.
function Show-DaemonErrors([string]$phase) {
    $lines = @()
    foreach ($f in @("$phase-multi64d.log", "$phase-multi64d.err.log")) {
        $path = Join-Path $out $f
        if (Test-Path $path) { $lines += @(Get-Content $path | Where-Object { $_ -match ' (WARN|ERROR) |error' }) }
    }
    if ($lines.Count -eq 0) { Say '(multi64d logged no warning or error)'; return }
    # The daemon retries once a second, so one cause repeats; each distinct message once, without
    # its timestamp, newest last.
    $seen = @{}
    $distinct = @()
    for ($k = $lines.Count - 1; $k -ge 0; $k--) {
        $msg = ($lines[$k] -replace '^\S+\s+', '').Trim()
        if (-not $seen.ContainsKey($msg)) { $seen[$msg] = $true; $distinct = @($msg) + $distinct }
    }
    Say "multi64d's last warnings:"
    $distinct | Select-Object -Last 6 | ForEach-Object { Say ("  " + $_) }
}

# $clearSerial is passed either way, so a MULTI64D_CLEAR_SERIAL in the tester's environment cannot
# decide it.
function Start-Daemon([string]$phase, [bool]$clearSerial = $false) {
    $env:RUST_LOG = 'debug'
    $env:MULTI64D_SERIAL_TRACE = '1'
    $dargs = @('--serial', $script:Port, '--cart', $Cart, ('--clear-serial=' + $clearSerial.ToString().ToLower()))
    Say ("starting multi64d " + ($dargs -join ' ') + " (debug log, serial trace)")
    $p = Start-Process -FilePath $daemon -ArgumentList $dargs -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput (Join-Path $out "$phase-multi64d.log") `
        -RedirectStandardError (Join-Path $out "$phase-multi64d.err.log")
    if (-not [Multi64Job]::Adopt($p.Handle)) {
        Say 'note: could not tie multi64d to this window; if you close it mid-run, end multi64d in Task Manager'
    }
    for ($i = 0; $i -lt 40; $i++) {
        Start-Sleep -Milliseconds 250
        if ($p.HasExited) {
            Say "multi64d exited at once (code $($p.ExitCode)). Is another program using port 38765, such as Multi64's own bridge?"
            Show-DaemonErrors $phase
            return $null
        }
        try {
            Invoke-WebRequest -UseBasicParsing -TimeoutSec 2 -Uri "$base/" | Out-Null
            # The link can take a moment: multi64d retries the port once a second, and a PRO's link
            # comes up only after the cart answers an identity check.
            $up = $false
            for ($j = 0; $j -lt 8 -and -not $up; $j++) {
                Start-Sleep -Seconds 1
                $up = Port-Open
            }
            Save-Status "$phase-status-start.json"
            if (-not $up) {
                Say "multi64d is running, but its link to the cart on $($script:Port) did not come up, so this test is skipped."
                Show-DaemonErrors $phase
                Stop-Daemon $p $phase
                return $null
            }
            return $p
        } catch { }
    }
    Say 'multi64d did not answer within 10 s; skipping this test'
    Stop-Daemon $p $phase
    return $null
}

function Stop-Daemon($p, [string]$phase) {
    if ($p -eq $null) { return }
    Save-Status "$phase-status-end.json"
    if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force }
    Start-Sleep -Milliseconds 500
}

# The suite's first request on its own: put the test ROM in M64T_PROTO, which it accepts from any
# mode, through the daemon. Up to three tries of 5 s, all logged to $file. $true once one is answered.
function Probe-Cart([string]$file) {
    for ($try = 1; $try -le 3; $try++) {
        if ((Run-Logged $file $tool @('set-mode', '--mode', '1')) -eq 0) {
            Say "the cart answered through the bridge (try $try of 3)"
            return $true
        }
    }
    Say 'the cart did not answer through the bridge (3 tries)'
    return $false
}

Section 'Multi64 cart diagnostics'
if (Test-Path (Join-Path $here 'VERSION.txt')) {
    Get-Content (Join-Path $here 'VERSION.txt') | ForEach-Object { Say $_ }
}
Say ("started " + (Get-Date -Format 'yyyy-MM-dd HH:mm:ss zzz'))
$os = Get-CimInstance Win32_OperatingSystem
Say ("Windows: " + $os.Caption + " " + $os.Version + " (build " + $os.BuildNumber + "), PowerShell " + $PSVersionTable.PSVersion)

foreach ($f in @($daemon, $tool, $baseline)) {
    if (-not (Test-Path $f)) { Say "missing $f; unzip the whole bundle and run this from inside it"; exit 1 }
}

Section 'Other programs using the cart'
# A daemon from this bundle is one an earlier run left behind (before the job above existed, or
# from a crash): not the tester's to close, so stop it here.
Get-Process multi64d -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $daemon } | ForEach-Object {
    Say "stopping multi64d $($_.Id), left over from an earlier run of this script"
    Stop-Process -Id $_.Id -Force
    Start-Sleep -Milliseconds 500
}
while ($true) {
    $busy = Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.ProcessName -in @('multi64d', 'multi64', 'ap64', 'xfer64') }
    if (-not $busy) { Say 'none running'; break }
    Say ("running: " + (($busy | ForEach-Object { $_.ProcessName }) -join ', '))
    Ask 'Close Multi64 (Quit from its tray icon), AP64 and Xfer64, then press Enter' | Out-Null
}

Section 'Serial ports and drivers'
$ports = @(Get-CimInstance Win32_PnPEntity | Where-Object { $_.Name -match '\(COM\d+\)' })
foreach ($d in $ports) { Say ($d.Name + "  " + $d.DeviceID + "  status " + $d.Status) }
Get-CimInstance Win32_PnPSignedDriver | Where-Object { $_.DeviceClass -eq 'PORTS' -or $_.DeviceName -match 'USB Serial|FTDI' } |
    ForEach-Object { Say ("driver: " + $_.DeviceName + "  " + $_.Manufacturer + "  " + $_.DriverVersion + "  " + $_.DriverDate) }
Run-Logged 'ports.txt' $daemon @('--list-ports') | Out-Null

# USB IDs from crates/cart-probe: the X7's FT245R is 0403:6001, the SummerCart64 0403:6014. Both
# are stock FTDI parts, so a match only picks a default; the PRO's is not known, so it is asked for.
if (-not $Cart) {
    # With -Port, only that port can say which cart it is.
    $candidates = $ports
    if ($Port) { $candidates = @($ports | Where-Object { $_.Name -match ('\(' + [regex]::Escape($Port) + '\)') }) }
    $x7 = @($candidates | Where-Object { $_.DeviceID -match 'VID_0403.PID_6001' })
    $sc = @($candidates | Where-Object { $_.DeviceID -match 'VID_0403.PID_6014' })
    if ($x7.Count + $sc.Count -eq 1) {
        $Cart = if ($x7.Count -eq 1) { 'ed64' } else { 'sc64' }
        Say "cart: $Cart, from the only X7 or SummerCart64 USB port plugged in"
    } else {
        $pick = (Ask 'Which cart is this: x7, pro or sc64?').Trim().ToLower()
        $Cart = switch ($pick) { 'x7' { 'ed64' } 'pro' { 'ed64pro' } default { $pick } }
    }
} else {
    Say "cart: $Cart (given)"
}
if ($Cart -notin @('ed64', 'ed64pro', 'sc64')) { Say "unknown cart '$Cart'; use x7, pro or sc64"; exit 1 }

if (-not $Port) {
    $id = switch ($Cart) { 'ed64' { 'VID_0403.PID_6001' } 'sc64' { 'VID_0403.PID_6014' } default { 'no match' } }
    $match = @($ports | Where-Object { $_.DeviceID -match $id })
    if ($match.Count -eq 1) {
        $Port = [regex]::Match($match[0].Name, 'COM\d+').Value
        Say "using $Port, the only matching port"
    } else {
        $Port = (Ask 'Which COM port is the cart (for example COM5)?').Trim().ToUpper()
    }
}
$script:Port = $Port

if (-not $SkipControl) {
    if ($Cart -eq 'ed64pro') {
        # libdragon has no PRO support, so on a PRO the test ROM's USB goes through this repo's own
        # ed64pro.c: the same unproven mapping as the agent, which makes this no control.
        Section 'Phase 1 of 2: test ROM run with multi64_test.z64'
        Say 'On a PRO this ROM uses the same untested link design as the bring-up ROM, so it is a second first-time check, not a known-good control.'
    } else {
        Section 'Phase 1 of 2: control run with multi64_test.z64'
        Say 'This ROM moves USB through libdragon, which has worked on an X7 and on a SummerCart64. It checks your cable, driver and port.'
    }
    Ask 'Boot multi64_test.z64 from the cart menu. When its text is on the TV, press Enter' | Out-Null
    # The purge experiment in this file's header: a daemon that does not purge first, then, only if
    # the cart says nothing through it, one that does.
    $phase = 'phase1'
    $p = Start-Daemon $phase
    if ($p -ne $null -and -not (Probe-Cart 'phase1-probe.txt')) {
        Stop-Daemon $p $phase
        Say 'Starting multi64d again, this time purging the port as it opens it, to see whether that brings the cart back.'
        $phase = 'phase1-purged'
        $p = Start-Daemon $phase $true
        if ($p -ne $null) { Probe-Cart 'phase1-purged-probe.txt' | Out-Null }
    }
    if ($p -ne $null) {
        Run-Logged "$phase-suite.txt" $tool @('suite', '--port', $Port) | Out-Null
        Stop-Daemon $p $phase
    }
}

Section 'Phase 2 of 2: bring-up run with multi64_bringup.z64'
Ask 'Power the console off and on, boot multi64_bringup.z64, wait until its text on the TV stops changing, then press Enter' | Out-Null
$p = Start-Daemon 'phase2'
if ($p -ne $null) {
    $words = Join-Path $out 'phase2-bringup.json'
    Run-Logged 'phase2-bringup.txt' $tool @('bringup', '--baseline', $baseline, '--out', $words) | Out-Null
    $noHello = Select-String -Path (Join-Path $out 'phase2-bringup.txt') -Pattern 'FAIL\s+link\.hello' -Quiet
    if ($noHello -and $Cart -eq 'ed64') {
        Section 'Phase 2b: the same with the DMA build'
        Say 'The ROM never answered through the CPU-word build. Now the DMA build.'
        Ask 'Press R on the controller once. When the top line on the TV reads "link X7 DMA", press Enter' | Out-Null
        $dma = Join-Path $out 'phase2b-bringup-dma.json'
        Run-Logged 'phase2b-bringup-dma.txt' $tool @('bringup', '--baseline', $baseline, '--out', $dma) | Out-Null
    }
    Stop-Daemon $p 'phase2'
}

Section 'Cart'
$cartOs = Ask 'Your cart menu/OS version, from the cart menu (or leave empty)'
Say ("cart OS: " + $cartOs)
$screen = Ask 'Did either ROM stay on a black screen, or stop partway? Describe it (or leave empty)'
Say ("screen: " + $screen)

Section 'Done'
Say ("finished " + (Get-Date -Format 'yyyy-MM-dd HH:mm:ss zzz'))
$zip = "$out.zip"
# Not Compress-Archive: Windows PowerShell 5.1's stores paths with backslashes, which unzip tools
# outside Windows refuse, and so does ZipFile.CreateFromDirectory under powershell.exe. Each entry
# is named here, with forward slashes.
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
if (Test-Path $zip) { Remove-Item $zip -Force }
$archive = [System.IO.Compression.ZipFile]::Open($zip, [System.IO.Compression.ZipArchiveMode]::Create)
try {
    $root = Split-Path $out -Parent
    Get-ChildItem -Path $out -Recurse -File | ForEach-Object {
        $name = $_.FullName.Substring($root.Length + 1).Replace('\', '/')
        [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $_.FullName, $name) | Out-Null
    }
} finally {
    $archive.Dispose()
}
Write-Host ''
Write-Host "Send this one file back: $zip" -ForegroundColor Green
Start-Process explorer.exe "/select,`"$zip`""
