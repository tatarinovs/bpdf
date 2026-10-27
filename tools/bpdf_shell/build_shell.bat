@echo off
setlocal EnableExtensions

cd /d "%~dp0"

set "OUT_DLL=%~dp0bpdf_shell.dll"
set "SRC_DLL=%~dp0bpdf_shell.cpp"
set "DEF_DLL=%~dp0bpdf_shell.def"
set "OUT_GUI=%~dp0bpdf-gui.exe"
set "SRC_GUI=%~dp0bpdf_gui.cpp"

echo [1/3] Looking for C++ compiler (g++ or cl)...

set "CXX="
for /f "delims=" %%C in ('where g++ 2^>nul') do if not defined CXX set "CXX=%%C"
if not defined CXX if exist "e:\_SOFT\w64devkit\bin\g++.exe" set "CXX=e:\_SOFT\w64devkit\bin\g++.exe"
if not defined CXX for /f "delims=" %%C in ('where cl 2^>nul') do if not defined CXX set "CXX=cl"

if not defined CXX (
  echo Error: neither g++ nor cl.exe was found in PATH.
  exit /b 1
)

echo Using compiler: %CXX%
echo [2/3] Compiling bpdf_shell.dll (Shell Context Menu Extension)...

if "%CXX%"=="cl" (
  cl.exe /nologo /O2 /LD /EHsc "%SRC_DLL%" /link /DEF:"%DEF_DLL%" /OUT:"%OUT_DLL%" ole32.lib shell32.lib user32.lib gdi32.lib advapi32.lib shlwapi.lib
) else (
  "%CXX%" -Os -shared -s -municode -fno-exceptions -fno-rtti -ffunction-sections -fdata-sections -Wl,--gc-sections -o "%OUT_DLL%" "%SRC_DLL%" "%DEF_DLL%" -lole32 -lshell32 -luser32 -lgdi32 -ladvapi32 -lshlwapi -luuid -static-libgcc
)

if errorlevel 1 (
  echo Build of bpdf_shell.dll failed!
  exit /b 1
)

echo [3/3] Compiling bpdf-gui.exe (Standalone Action Palette and Progress GUI)...

if "%CXX%"=="cl" (
  cl.exe /nologo /O2 /EHsc "%SRC_GUI%" /link /SUBSYSTEM:WINDOWS /OUT:"%OUT_GUI%" user32.lib gdi32.lib comctl32.lib comdlg32.lib shell32.lib shlwapi.lib
) else (
  "%CXX%" -Os -mwindows -municode -s -fno-exceptions -fno-rtti -ffunction-sections -fdata-sections -Wl,--gc-sections -o "%OUT_GUI%" "%SRC_GUI%" -luser32 -lgdi32 -lcomctl32 -lcomdlg32 -lshell32 -lshlwapi -static-libgcc
)

if errorlevel 1 (
  echo Build of bpdf-gui.exe failed!
  exit /b 1
)

echo.
echo Successfully built:
echo   %OUT_DLL%
echo   %OUT_GUI%

set "DIST_SHELL=%~dp0..\..\dist\shell"
if exist "%DIST_SHELL%" (
  if exist "%DIST_SHELL%\bpdf_shell.dll" (
    del /f /q "%DIST_SHELL%\bpdf_shell.dll.old*" 2>nul
    move /y "%DIST_SHELL%\bpdf_shell.dll" "%DIST_SHELL%\bpdf_shell.dll.old" >nul 2>&1
  )
  copy /y "%OUT_DLL%" "%DIST_SHELL%\bpdf_shell.dll" >nul
  copy /y "%OUT_GUI%" "%DIST_SHELL%\bpdf-gui.exe" >nul
  copy /y "%~dp0bpdf_shell.ini" "%DIST_SHELL%\bpdf_shell.ini" >nul
  echo Updated %DIST_SHELL%\bpdf_shell.dll, bpdf-gui.exe and bpdf_shell.ini
)

exit /b 0
