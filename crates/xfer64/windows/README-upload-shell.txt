Quick upload from Windows Explorer (no main Xfer64 window)
============================================================

Command line
------------
  xfer64 upload [options] <files...>

  --com COM          Serial port (optional; otherwise MULTI64_XFER64_COM, then the
                     settings file, then auto-detect)
  --to CART_FOLDER   Folder on the SD card relative to root (optional; default is the
                     Quick upload folder: the one last opened in the Cart pane or in
                     Quick upload, or the card root)
  --overwrite, -y    Replace files that already exist on the cart
  --notify           Show a message when the upload has finished
  --picker           Open the Quick upload window to choose the cart folder instead of
                     uploading headless (what Send to and the context menu run)
  --experimental-ed64pro-writes
                     Allow writing to an EverDrive-64 PRO (experimental; never tested on
                     a cart). Headless only: the --picker window asks instead
  --help, -h         Show the options

  With --picker only the files are used: the window starts in the Quick upload folder,
  its own "Overwrite existing" box (remembered between runs) decides overwrites, and the
  serial port comes from MULTI64_XFER64_COM, the settings file, or auto-detect. Once the
  cart has listed the folder, the window uploads by itself after 5 seconds unless you
  open another folder or cancel the countdown.

Environment (optional)
----------------------
  MULTI64_XFER64_COM   Serial port when --com is not given; takes precedence over the
                       settings file
  MULTI64_DAEMON_LISTEN   multi64d HTTP base (default http://127.0.0.1:38765). Headless
                          upload only: the --picker window uses the main window's address
  MULTI64_XFER64_UPLOAD_NOTIFY=1   Same as --notify; unlike the option, it applies to the
                                   --picker window's upload too

Settings file
-------------
  %APPDATA%\multi64\xfer64-settings.json

  preferredCom          — saved when you pick a serial port in the app
  quickUploadCartPath   — updated when you open a folder in the Cart pane or in Quick upload
  quickUploadOverwrite  — Quick upload's "Overwrite existing" box

Send to (from Xfer64 Settings)
------------------------------
  On Windows, open Settings → Windows integration → Add to Send to. This creates
  %APPDATA%\Microsoft\Windows\SendTo\Xfer64 upload.lnk pointing at the running
  executable with arguments: upload --picker

  A minimal window opens to choose the destination folder on the SD card, then the
  process exits (no background task).

  The button switches to Remove from Send to when that shortcut already exists.

Registry (context menu)
-----------------------
  Edit windows\upload-to-sd.reg so the path points to xfer64.exe, then merge the file
  (double-click or: reg import upload-to-sd.reg).

  The shell runs: xfer64.exe upload --picker "%1"  (one file per invocation when using %1).
