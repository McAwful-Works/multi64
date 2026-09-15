# Embeds wix-sendto-uninstall.ps1 as Base64 into wix-sendto-uninstall.wxs (run after editing the .ps1).
$ErrorActionPreference = 'Stop'
$dir = $PSScriptRoot
$ps1 = Join-Path $dir 'wix-sendto-uninstall.ps1'
$b64 = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes((Get-Content -Raw -LiteralPath $ps1)))
# MessageBox must not be suppressed during interactive uninstall.
$exe = "powershell.exe -NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -EncodedCommand $b64"
$wxs = @"
<?xml version="1.0" encoding="UTF-8"?>
<!-- Generated from wix-sendto-uninstall.ps1; do not edit by hand. -->
<Wix xmlns="http://schemas.microsoft.com/wix/2006/wi">
  <Fragment>
    <!--
      Anchor for the fragment. WiX links a Fragment only when something references it, and Tauri
      exposes no customActionRefs, so without a referencable element here light silently drops this
      whole file: the MSI uninstall then never ran Xfer64RemoveSendTo and left the Send to shortcut
      pointing at a deleted exe. Referenced from tauri.conf.json via
      bundle.windows.wix.componentGroupRefs, as Multi64's wix-xfer64-option.wxs does.
    -->
    <ComponentGroup Id="Xfer64InstallerActions" />
    <!--
      SystemFolder is a standard MSI directory but is not among the ones Tauri emits into main.wxs,
      so once this fragment is linked, a custom action naming it fails with LGHT0094. Declare it here.
    -->
    <DirectoryRef Id="TARGETDIR">
      <Directory Id="SystemFolder" Name="System" />
    </DirectoryRef>
    <CustomAction Id="Xfer64RemoveSendTo"
      Directory="SystemFolder"
      Execute="deferred"
      Impersonate="yes"
      ExeCommand="$exe"
      Return="ignore" />
    <InstallExecuteSequence>
      <Custom Action="Xfer64RemoveSendTo" Before="RemoveFiles"><![CDATA[REMOVE="ALL"]]></Custom>
    </InstallExecuteSequence>
  </Fragment>
</Wix>
"@
$out = Join-Path $dir 'wix-sendto-uninstall.wxs'
$utf8NoBom = New-Object System.Text.UTF8Encoding $false
[System.IO.File]::WriteAllText($out, $wxs, $utf8NoBom)
