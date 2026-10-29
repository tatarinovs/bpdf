@echo off
setlocal EnableExtensions

cd /d "%~dp0"
if errorlevel 1 exit /b 1

set "RESOURCE_DIR=%CD%\target\windows-res"
set "RESOURCE_FILE=%RESOURCE_DIR%\bpdf.res"

if not exist "%RESOURCE_DIR%" mkdir "%RESOURCE_DIR%"
if errorlevel 1 goto :error

echo [1/5] Preparing icon and version information...
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

echo [2/5] Compiling Windows resources...
"%RC_EXE%" /nologo /c65001 /i "%RESOURCE_DIR%" /i "%CD%\resources" ^
  /fo "%RESOURCE_FILE%" "%CD%\resources\bpdf.rc"
if errorlevel 1 goto :error

echo [3/5] Building optimized Windows executable...
set "BPDF_WINDOWS_RES=%RESOURCE_FILE%"
cargo build --release --locked
if errorlevel 1 goto :error

echo [3.5/5] Building Windows shell extension (bpdf_shell.dll)...
if exist "%CD%\tools\bpdf_shell\build_shell.bat" (
  call "%CD%\tools\bpdf_shell\build_shell.bat"
  if errorlevel 1 goto :error
)

echo [4/5] Preparing dist directory...
if not exist "%CD%\dist" mkdir "%CD%\dist"
if errorlevel 1 goto :error
copy /y "%CD%\target\release\bpdf.exe" "%CD%\dist\bpdf.exe" >nul || goto :error
copy /y "%CD%\config.example.toml" "%CD%\dist\config.example.toml" >nul || goto :error
rem Documentation and the license go to dist\docs\.
if not exist "%CD%\dist\docs" mkdir "%CD%\dist\docs"
if errorlevel 1 goto :error
del /q "%CD%\dist\README*.md" "%CD%\dist\QUICKSTART*.txt" "%CD%\dist\LICENSE" >nul 2>&1
for %%F in (README.md README.en.md QUICKSTART.txt QUICKSTART.en.txt LICENSE) do (
  copy /y "%CD%\%%F" "%CD%\dist\docs\%%F" >nul || goto :error
)
rem Total Commander bar generators live in dist\bar\; the .bar files
rem themselves are generated on the user's machine.
if not exist "%CD%\dist\bar" mkdir "%CD%\dist\bar"
if errorlevel 1 goto :error
del /q "%CD%\dist\make_bar*.bat" "%CD%\dist\*.bar" "%CD%\dist\bar\*.bar" >nul 2>&1
copy /y "%CD%\tools\bar\make_bar*.bat" "%CD%\dist\bar\" >nul || goto :error

rem Prepare Windows Explorer context menu extension in dist\shell\
if not exist "%CD%\dist\shell" mkdir "%CD%\dist\shell"
if errorlevel 1 goto :error
if exist "%CD%\tools\bpdf_shell\bpdf_shell.dll" (
  del /f /q "%CD%\dist\shell\bpdf_shell.dll.old*" >nul 2>&1
  copy /y "%CD%\tools\bpdf_shell\bpdf_shell.dll" "%CD%\dist\shell\bpdf_shell.dll" >nul 2>&1
  if errorlevel 1 (
    rem Explorer may still hold an earlier .old, so every rename gets a new name.
    move /y "%CD%\dist\shell\bpdf_shell.dll" "%CD%\dist\shell\bpdf_shell.dll.old%RANDOM%" >nul 2>&1
    copy /y "%CD%\tools\bpdf_shell\bpdf_shell.dll" "%CD%\dist\shell\bpdf_shell.dll" >nul || goto :error
  )
  copy /y "%CD%\tools\bpdf_shell\bpdf_shell.ini" "%CD%\dist\shell\bpdf_shell.ini" >nul || goto :error
  copy /y "%CD%\tools\bpdf_shell\bpdf_shell_ru.ini" "%CD%\dist\shell\bpdf_shell_ru.ini" >nul || goto :error
  if exist "%CD%\tools\bpdf_shell\bpdf-gui.exe" (
    copy /y "%CD%\tools\bpdf_shell\bpdf-gui.exe" "%CD%\dist\bpdf-gui.exe" >nul || goto :error
  )
  copy /y "%CD%\tools\bpdf_shell\register.bat" "%CD%\dist\shell\register.bat" >nul || goto :error
  copy /y "%CD%\tools\bpdf_shell\unregister.bat" "%CD%\dist\shell\unregister.bat" >nul || goto :error
)

echo [5/5] Packing release archive...
set "VERSION="
for /f "tokens=2 delims== " %%V in ('findstr /b /c:"version" "%CD%\Cargo.toml"') do (
  if not defined VERSION set "VERSION=%%~V"
)
if not defined VERSION (
  echo Package version was not found in Cargo.toml.
  goto :error
)
set "ZIP_FILE=%CD%\dist\bpdf-%VERSION%-win-x64.zip"
powershell.exe -NoLogo -NoProfile -NonInteractive -Command ^
  "$ErrorActionPreference = 'Stop'; $d = '%CD%\dist';" ^
  "$stage = Join-Path $env:TEMP ('bpdf_pkg_' + [Guid]::NewGuid().ToString());" ^
  "New-Item -ItemType Directory -Path (Join-Path $stage 'shell'), (Join-Path $stage 'bar'), (Join-Path $stage 'docs') -Force | Out-Null;" ^
  "@('bpdf.exe', 'bpdf-gui.exe', 'config.example.toml') | ForEach-Object { Copy-Item (Join-Path $d $_) $stage };" ^
  "@('README.md', 'README.en.md', 'QUICKSTART.txt', 'QUICKSTART.en.txt', 'LICENSE') | ForEach-Object { Copy-Item (Join-Path $d ('docs\' + $_)) (Join-Path $stage 'docs') };" ^
  "@('make_bar.bat', 'make_bar_en.bat', 'make_bar_gui.bat', 'make_bar_gui_en.bat') | ForEach-Object { Copy-Item (Join-Path $d ('bar\' + $_)) (Join-Path $stage 'bar') };" ^
  "@('bpdf_shell.dll', 'bpdf_shell.ini', 'bpdf_shell_ru.ini', 'register.bat', 'unregister.bat') | ForEach-Object { Copy-Item (Join-Path $d ('shell\' + $_)) (Join-Path $stage 'shell') };" ^
  "Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem;" ^
  "if (Test-Path '%ZIP_FILE%') { Remove-Item -Force '%ZIP_FILE%' };" ^
  "$zip = [IO.Compression.ZipFile]::Open('%ZIP_FILE%', 'Create');" ^
  "Get-ChildItem -Recurse -File $stage | ForEach-Object { $name = $_.FullName.Substring($stage.Length + 1).Replace('\', '/'); [void][IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $_.FullName, $name, 'Optimal') };" ^
  "$zip.Dispose();" ^
  "Remove-Item -Recurse -Force $stage;"
if errorlevel 1 goto :error

echo.
echo Build complete:
echo   %CD%\dist\bpdf.exe
echo   %ZIP_FILE%
exit /b 0

:error
echo.
echo Build failed.
exit /b 1
