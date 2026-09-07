; Stop `multi64d.exe` before touching files. It is a bundled **resource**, not the main binary, so
; Tauri's own `CheckIfAppIsRunning` (inserted right after this hook) only ever looks at
; `multi64.exe` and never sees the daemon. A daemon still running holds
; `$INSTDIR\resources\multi64d.exe` open and the copy fails with a locked file.
;
; This is not a rare case. The GUI kills its child on exit (`kill_daemon`, src/lib.rs), but the
; installer terminates the GUI itself, so that cleanup never runs and the daemon is orphaned --
; parentless, windowless, and invisible in Task Manager's Processes tab. The installer's own
; behaviour creates the lock that then blocks it.
;
; Matching Tauri, this kills by image name, so a developer's separately built multi64d is killed
; too. That is the same trade Tauri already makes for the main binary, and the alternative is an
; install that fails. The daemon holds no state needing a flush -- the OS releases its serial
; handle on exit -- so a forced kill is safe.
!macro NSIS_HOOK_PREINSTALL
  nsExec::Exec 'taskkill /F /IM multi64d.exe'
  Pop $0
  Sleep 300
!macroend

; Same reasoning for uninstall: the file cannot be deleted while the daemon runs, which otherwise
; leaves `resources\multi64d.exe` and its directory behind.
!macro NSIS_HOOK_PREUNINSTALL
  nsExec::Exec 'taskkill /F /IM multi64d.exe'
  Pop $0
  Sleep 300
!macroend

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
