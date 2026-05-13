; Optional Xfer64: run resources/xfer64-installer-prompt.ps1 after Multi64 files are installed.
; Interactive NSIS builds use the wizard page in windows/installer.nsi; this hook runs the bundled
; installer without a second Yes/No when the user chose Install on that page.
; Skip for silent (/S), passive (/P), or when the user skipped Xfer64 on the wizard page.
!macro NSIS_HOOK_POSTINSTALL
  IfSilent xfer64_installer_prompt_done
  StrCmp $PassiveMode 1 xfer64_installer_prompt_done
  StrCmp $Xfer64InstallChoice 0 xfer64_installer_prompt_done
  IfFileExists "$INSTDIR\resources\xfer64-installer-prompt.ps1" 0 xfer64_installer_prompt_done
  ExecWait '"powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\resources\xfer64-installer-prompt.ps1" -InstallDir "$INSTDIR" -SkipPrompt'
xfer64_installer_prompt_done:
!macroend
