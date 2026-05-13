; Send to shortcut path must match `SHORTCUT_NAME` in `send_to_windows.rs` (%APPDATA% = Roaming).
; WiX MSI: same shortcut; `wix-sendto-uninstall.ps1` + `generate-wix-sendto-wxs.ps1`.
!macro NSIS_HOOK_PREUNINSTALL
  IfFileExists "$APPDATA\Microsoft\Windows\SendTo\Xfer64 upload.lnk" 0 xfer64_sendto_done
  MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "Remove the Xfer64 upload shortcut from your Send to menu?$\r$\n$\r$\nChoose Yes to delete it, or No to leave it (you can remove it later from Xfer64 Settings)." IDYES xfer64_sendto_remove IDNO xfer64_sendto_done
xfer64_sendto_remove:
  Delete "$APPDATA\Microsoft\Windows\SendTo\Xfer64 upload.lnk"
xfer64_sendto_done:
!macroend
