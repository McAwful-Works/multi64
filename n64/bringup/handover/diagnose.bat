@echo off
rem Double-click to run the cart diagnostics. Everything is saved in a results-<time> folder here,
rem and zipped at the end: send that zip back.
cd /d "%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0diagnose.ps1" %*
echo.
pause
