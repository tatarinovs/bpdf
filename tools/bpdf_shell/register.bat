@echo off
setlocal EnableExtensions
cd /d "%~dp0"

set "DLL=%~dp0bpdf_shell.dll"
if not exist "%DLL%" (
  echo [ERROR] bpdf_shell.dll not found in "%~dp0"
  pause
  exit /b 1
)

rem The extension finds bpdf.exe itself: beside the DLL, in its parent
rem folder or in PATH. It is looked up here only to show the layout.
set "EXE="
if exist "%~dp0bpdf.exe" set "EXE=%~dp0bpdf.exe"
if not defined EXE if exist "%~dp0..\bpdf.exe" (
  for %%F in ("%~dp0..\bpdf.exe") do set "EXE=%%~fF"
)

echo Registering bpdf context menu shell extension...
regsvr32 /s "%DLL%"
if errorlevel 1 (
  echo [ERROR] regsvr32 failed. Try running as Administrator.
  pause
  exit /b 1
)

echo.
echo ============================================================
echo   bpdf Windows context menu extension registered successfully!
if defined EXE (
echo   bpdf.exe: %EXE%
)
echo   - Right-click PDF, images, documents or folders
echo   - Multi-file selection supported for batch actions
echo ============================================================
echo.
pause
