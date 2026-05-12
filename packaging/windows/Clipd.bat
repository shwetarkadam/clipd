@echo off
REM Double-click launcher for Clipd on Windows.
REM Starts the tray UI which spawns the daemon + GUI.
cd /d "%~dp0"
start "" "%~dp0clipd-ui.exe"
if errorlevel 1 (
    echo.
    echo clipd-ui.exe failed to start. Error code: %errorlevel%
    pause
)
