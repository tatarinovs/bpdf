@echo off
setlocal EnableExtensions

cd /d "%~dp0"

set "OUT_DLL=%~dp0bpdf_shell.dll"
set "SRC=%~dp0bpdf_shell.cpp"
set "DEF=%~dp0bpdf_shell.def"

echo [1/2] Looking for C++ compiler (g++ or cl)...

set "CXX="
for /f "delims=" %%C in ('where g++ 2^>nul') do if not defined CXX set "CXX=%%C"
if not defined CXX if exist "e:\_SOFT\w64devkit\bin\g++.exe" set "CXX=e:\_SOFT\w64devkit\bin\g++.exe"
if not defined CXX for /f "delims=" %%C in ('where cl 2^>nul') do if not defined CXX set "CXX=cl"

if not defined CXX (
  echo Error: neither g++ nor cl.exe was found in PATH.
  exit /b 1
)

echo Using compiler: %CXX%
echo [2/2] Compiling bpdf_shell.dll...

if "%CXX%"=="cl" (
  cl.exe /nologo /O2 /LD /EHsc "%SRC%" /link /DEF:"%DEF%" /OUT:"%OUT_DLL%" ole32.lib shell32.lib user32.lib gdi32.lib advapi32.lib shlwapi.lib comctl32.lib
) else (
  "%CXX%" -Os -shared -s -fno-exceptions -fno-rtti -ffunction-sections -fdata-sections -Wl,--gc-sections -o "%OUT_DLL%" "%SRC%" "%DEF%" -lole32 -lshell32 -luser32 -lgdi32 -ladvapi32 -lshlwapi -luuid -lcomctl32 -static-libgcc
)

if errorlevel 1 (
  echo Build failed!
  exit /b 1
)

echo.
echo Successfully built:
echo   %OUT_DLL%

set "DIST_SHELL=%~dp0..\..\dist\shell"
if exist "%DIST_SHELL%" (
  if exist "%DIST_SHELL%\bpdf_shell.dll" (
    del /f /q "%DIST_SHELL%\bpdf_shell.dll.old*" 2>nul
    move /y "%DIST_SHELL%\bpdf_shell.dll" "%DIST_SHELL%\bpdf_shell.dll.old" >nul 2>&1
  )
  copy /y "%OUT_DLL%" "%DIST_SHELL%\bpdf_shell.dll" >nul
  copy /y "%~dp0bpdf_shell.ini" "%DIST_SHELL%\bpdf_shell.ini" >nul
  echo Updated %DIST_SHELL%\bpdf_shell.dll and bpdf_shell.ini
)

exit /b 0
