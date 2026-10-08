Multi64 cart diagnostics
========================

Thanks for testing. This runs two short tests on your flash cart and saves everything we need into
one zip file for you to send back. It takes about 15 minutes, and you'll need your phone's camera
for a few photos of the TV. Nothing writes to your SD card except the two ROMs you copy onto it.


Before you start
----------------

1. Copy multi64_test.z64 and multi64_bringup.z64 to your cart's SD card.
2. Connect the cart's USB port to this PC and turn the console on.
3. Close Multi64, AP64 and Xfer64 if they are running (Quit from the tray icon). The diagnostics
   run their own copy of the bridge, with full logging, and only one program can use the cart.


Run it
------

Double-click diagnose.bat. It walks you through each step and waits for you to press Enter:

  1. Control test: boot multi64_test.z64 from the cart menu. This ROM is known to work, so it
     checks your cable, driver and port.
  2. Bring-up test: boot multi64_bringup.z64. This runs the code that AP64 puts into games, on
     its own, and measures what works on your cart. On an EverDrive X7 it may ask you to press R
     on the controller and run it a second time.
  3. Photos: a folder opens. Copy your photos of the TV into it.

At the end it tells you where the zip is. Send back that one file.

Photos: take one whenever it asks, with all the text on the TV readable. If the screen stays
black, or stops partway and doesn't move, photograph that too: it shows where it got stuck.


Optional: Multi64_..._setup.exe
-------------------------------

The latest Multi64, the same build as the bridge the diagnostics use. You don't need it for the
tests. Install it if you want the current version for AP64 afterward.


If something goes wrong
-----------------------

- Windows SmartScreen warns about the files: they aren't code-signed. Choose "More info", then
  "Run anyway".
- It says the bridge exited at once: another program has the cart's port open. Close it and run
  diagnose.bat again.
- Anything else: send the results folder or zip anyway, with your photos. A failed run is still
  useful data.


For the maintainer
------------------

diagnose.bat passes its arguments to diagnose.ps1: -Cart ed64|ed64pro|sc64 (default: the only
X7 or SummerCart64 plugged in, by USB IDs, else it asks), -Port COMn (default: the only port with
that cart's USB IDs, else it asks), -SkipControl. The bridge it starts is tied to its window, so
closing the window mid-run stops it too.
VERSION.txt records the commit this bundle was built from and every file's SHA-256.
