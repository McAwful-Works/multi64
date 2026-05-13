# Invoked by Multi64 NSIS/MSI installers after the main app is installed.
# Prompts to run the bundled Xfer64 installer (NSIS .exe or WiX .msi) when non-placeholder.
param(
    [Parameter(Mandatory = $true)]
    [string] $InstallDir,
    # MSI UILevel: 2 = no UI (silent); skip prompting. Omitted for NSIS (silent handled in installer-hooks.nsh).
    [string] $UiLevel = '',
    # NSIS: set when the user already chose Install Xfer64 on the wizard page (no second dialog).
    [switch] $SkipPrompt
)

if (-not $SkipPrompt -and $UiLevel -eq '2') {
    exit 0
}

$InstallDir = $InstallDir.TrimEnd([char[]]@('\', '/'))
$exe = Join-Path $InstallDir 'resources\xfer64-setup.exe'
$msi = Join-Path $InstallDir 'resources\xfer64-setup.msi'

function Test-NonPlaceholder {
    param([string] $Path)
    if (-not (Test-Path -LiteralPath $Path)) { return $false }
    try { return (Get-Item -LiteralPath $Path).Length -gt 1024 } catch { return $false }
}

$target = $null
$useMsi = $false

if (Test-NonPlaceholder $exe) {
    $target = $exe
} elseif (Test-NonPlaceholder $msi) {
    $target = $msi
    $useMsi = $true
} else {
    exit 0
}

if (-not $SkipPrompt) {
    Add-Type -AssemblyName System.Windows.Forms | Out-Null
    $text = "Install Xfer64 (SD file manager for your flash cart) now?`n`nYou can install it later from Multi64."
    $answer = [System.Windows.Forms.MessageBox]::Show($text, 'Multi64', 'YesNo', 'Question')
    if ($answer -ne [System.Windows.Forms.DialogResult]::Yes) {
        exit 0
    }
}

if ($useMsi) {
    Start-Process -FilePath 'msiexec.exe' -ArgumentList @('/i', $target) -Wait
} else {
    Start-Process -FilePath $target -Wait
}
