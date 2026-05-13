Quick upload from Windows Explorer (no main Xfer64 window)
============================================================

Command line
------------
  xfer64 upload [options] <files...>

  --com COM          Serial port (optional; otherwise settings file, env, or auto)
  --to CART_FOLDER   Folder on the SD card relative to root (optional; default is the
                     folder you last opened in the SD card pane, or the card root)
  --overwrite, -y    Replace files that already exist on the cart

Environment (optional)
----------------------
  MULTI64_XFER64_COM   Default COM port when not in xfer64-settings.json
  MULTI64_DAEMON_LISTEN   multi64d HTTP base (default http://127.0.0.1:38765)
  MULTI64_XFER64_UPLOAD_NOTIFY=1   Same as --notify (success dialog)

Settings file
-------------
  %APPDATA%\\multi64\\xfer64-settings.json

  preferred_com            — saved when you pick a COM port in the app
  quick_upload_cart_path   — updated when you navigate the SD card pane

Send to (from Xfer64 Settings)
------------------------------
  On Windows, open Settings → Windows integration → Add to Send to. This creates
  %APPDATA%\\Microsoft\\Windows\\SendTo\\Xfer64 upload.lnk pointing at the running
  executable with arguments: upload --picker

  A minimal window opens to choose the destination folder on the SD card, then the
  process exits (no background task).

  The button switches to Remove from Send to when that shortcut already exists.

Registry (context menu)
-----------------------
  Edit windows\\upload-to-sd.reg so the path points to xfer64.exe, then merge the file
  (double-click or: reg import upload-to-sd.reg).

  The shell runs: xfer64.exe upload "%1"  (one file per invocation when using %1).
