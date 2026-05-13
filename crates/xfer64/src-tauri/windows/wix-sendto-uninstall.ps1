# WiX MSI uninstall; path must match SHORTCUT_NAME in send_to_windows.rs (see installer-hooks.nsh for NSIS).
$sendTo = Join-Path $env:APPDATA 'Microsoft\Windows\SendTo\Xfer64 upload.lnk'
if (-not (Test-Path -LiteralPath $sendTo)) { exit 0 }
$quiet = $false
$id = $PID
for ($i = 0; $i -lt 8; $i++) {
  $p = Get-CimInstance Win32_Process -Filter "ProcessId=$id" -ErrorAction SilentlyContinue
  if (-not $p) { break }
  if ($p.Name -match '\bmsiexec\.exe$') {
    $cmd = $p.CommandLine
    if ($cmd -match '/(quiet|qn|passive)\b') { $quiet = $true }
    break
  }
  $id = $p.ParentProcessId
}
if ($quiet) { exit 0 }
Add-Type -AssemblyName System.Windows.Forms
$msg = "Remove the Xfer64 upload shortcut from Send to?`n`nChoose Yes to delete it, or No to leave it (you can remove it later from Xfer64 Settings)."
$r = [System.Windows.Forms.MessageBox]::Show($msg, 'Xfer64', 'YesNo', 'Question', 'Button2')
if ($r -eq 'Yes') { Remove-Item -LiteralPath $sendTo -Force }
