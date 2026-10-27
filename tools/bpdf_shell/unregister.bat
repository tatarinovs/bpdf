@echo off
setlocal EnableExtensions
cd /d "%~dp0"

set "DLL=%~dp0bpdf_shell.dll"
if not exist "%DLL%" (
  echo [ERROR] bpdf_shell.dll not found in "%~dp0"
  pause
  exit /b 1
)

echo Unregistering bpdf context menu shell extension...
regsvr32 /s /u "%DLL%"

echo.
echo ============================================================
echo   bpdf context menu extension unregistered successfully.
echo ============================================================
echo.
pause
exit /b 0
