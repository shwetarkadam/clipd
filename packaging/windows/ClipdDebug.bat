@echo off
cd /d "%~dp0"
echo Starting clipd-ui...
"%~dp0clipd-ui.exe"
if errorlevel 1 (
    echo.
    echo clipd-ui exited with error code: %errorlevel%
) else (
    echo clipd-ui exited normally.
)
echo.
pause
