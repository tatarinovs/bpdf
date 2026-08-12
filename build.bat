@echo off
setlocal EnableExtensions

cd /d "%~dp0"
if errorlevel 1 exit /b 1

set "RESOURCE_DIR=%CD%\target\windows-res"
set "RESOURCE_FILE=%RESOURCE_DIR%\bpdf.res"

if not exist "%RESOURCE_DIR%" mkdir "%RESOURCE_DIR%"
if errorlevel 1 goto :error

echo [1/4] Preparing icon and version information...
powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass ^
  -File "%CD%\tools\prepare-windows-resources.ps1" ^
  -ProjectRoot "%CD%" ^
  -OutputDir "%RESOURCE_DIR%"
if errorlevel 1 goto :error

if defined RC_EXE if not exist "%RC_EXE%" (
  echo RC_EXE points to a missing file: "%RC_EXE%"
  goto :error
)

if not defined RC_EXE (
  for /f "delims=" %%R in ('where rc.exe 2^>nul') do (
    if not defined RC_EXE set "RC_EXE=%%R"
  )
)

if not defined RC_EXE if exist "%ProgramFiles(x86)%\Windows Kits\10\bin" (
  for /f "delims=" %%V in ('dir /b /ad /o-n "%ProgramFiles(x86)%\Windows Kits\10\bin" 2^>nul') do (
    if not defined RC_EXE if exist "%ProgramFiles(x86)%\Windows Kits\10\bin\%%V\x64\rc.exe" (
      set "RC_EXE=%ProgramFiles(x86)%\Windows Kits\10\bin\%%V\x64\rc.exe"
    )
  )
)

if not defined RC_EXE (
  echo Windows SDK resource compiler rc.exe was not found.
  echo Install Windows 10/11 SDK or set RC_EXE to its full path.
  goto :error
)

echo [2/4] Compiling Windows resources...
"%RC_EXE%" /nologo /c65001 /i "%RESOURCE_DIR%" ^
  /fo "%RESOURCE_FILE%" "%CD%\resources\bpdf.rc"
if errorlevel 1 goto :error

echo [3/4] Building optimized Windows executable...
set "BPDF_WINDOWS_RES=%RESOURCE_FILE%"
cargo build --release
if errorlevel 1 goto :error

echo [4/4] Preparing dist directory...
if not exist "%CD%\dist" mkdir "%CD%\dist"
if errorlevel 1 goto :error
copy /y "%CD%\target\release\bpdf.exe" "%CD%\dist\bpdf.exe" >nul || goto :error
copy /y "%CD%\config.example.jsonc" "%CD%\dist\config.example.jsonc" >nul || goto :error
copy /y "%CD%\README.md" "%CD%\dist\README.md" >nul || goto :error

echo.
echo Build complete:
echo   %CD%\dist\bpdf.exe
exit /b 0

:error
echo.
echo Build failed.
exit /b 1
