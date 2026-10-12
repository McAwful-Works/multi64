@echo off
rem Double-click to record a play session: the bridge runs here, fully logged, while you play
rem through AP64. Everything is saved in a results-<time> folder and zipped: send that zip back.
cd /d "%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0diagnose.ps1" -Play %*
echo.
pause
