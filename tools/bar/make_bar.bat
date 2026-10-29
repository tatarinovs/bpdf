@echo off
rem Generates bpdf.bar (English Total Commander button bar) beside this file.
rem The script lives in the bar folder beside bpdf.exe. If bpdf.exe is inside Total Commander,
rem the path is written using %COMMANDER_PATH%, otherwise an absolute path is used.
rem
rem   make_bar.bat            - detect path automatically
rem   make_bar.bat /default   - path %COMMANDER_PATH%\tools\bpdf (for distribution)
rem   make_bar.bat NAME.bar   - write NAME.bar instead of bpdf.bar
rem
setlocal EnableExtensions DisableDelayedExpansion

for /f "tokens=2 delims=:." %%C in ('chcp') do set "OLD_CP=%%C"
chcp 1252 >nul

rem The script lives in the bar folder beside bpdf.exe (or right beside it).
set "EXE_DIR=%~dp0.."
if exist "%~dp0bpdf.exe" set "EXE_DIR=%~dp0."
for %%I in ("%EXE_DIR%") do set "EXE_DIR=%%~fI"
set "OUT=%~dp0bpdf.bar"
set "FORCED_BASE="
set "Q="

:parse_args
if "%~1"=="" goto :args_done
if /i "%~1"=="/default" (
  set "FORCED_BASE=1"
  shift
  goto :parse_args
)
if /i "%~x1"==".bar" (
  set "OUT=%~dp0%~nx1"
  shift
  goto :parse_args
)
shift
goto :parse_args
:args_done

if defined FORCED_BASE (
  set "BASE=%%COMMANDER_PATH%%\tools\bpdf"
  goto :write
)

if not exist "%EXE_DIR%\bpdf.exe" (
  echo Warning: bpdf.exe not found in "%EXE_DIR%"
)

rem Total Commander folder: from %COMMANDER_PATH% (when launched from TC)
rem or by searching for TOTALCMD*.EXE upwards from the bpdf.exe folder.
set "TC_DIR="
if defined COMMANDER_PATH (
  for %%I in ("%COMMANDER_PATH%\.") do set "TC_DIR=%%~fI"
  goto :have_tc
)
set "D=%EXE_DIR%"
:find_tc
if exist "%D%\TOTALCMD64.EXE" set "TC_DIR=%D%" & goto :have_tc
if exist "%D%\TOTALCMD.EXE" set "TC_DIR=%D%" & goto :have_tc
for %%I in ("%D%\..") do set "P=%%~fI"
if /i "%P%"=="%D%" goto :have_tc
set "D=%P%"
goto :find_tc

:have_tc
set "BASE=%EXE_DIR%"
setlocal EnableDelayedExpansion
if defined TC_DIR (
  set "TAIL=!EXE_DIR:%TC_DIR%=!"
  if /i "!TC_DIR!!TAIL!"=="!EXE_DIR!" (
    if "!TAIL!"=="" (set "OK=1") else if "!TAIL:~0,1!"=="\" set "OK=1"
  )
)
if defined OK (
  endlocal & set "BASE=%%COMMANDER_PATH%%%TAIL%"
) else (
  endlocal
  if defined TC_DIR (
    echo bpdf.exe is outside Total Commander folder "%TC_DIR%", path will be absolute.
  ) else (
    echo Total Commander not found, path will be absolute.
  )
)
rem Put path in quotes if EXE_DIR contains spaces, otherwise TC splits arguments.
if not "%EXE_DIR: =%"=="%EXE_DIR%" set Q="

:write
setlocal EnableDelayedExpansion
set "EXE=!BASE!\bpdf.exe"
set "RUN=!Q!!EXE!!Q!"
> "!OUT!" (
echo [Buttonbar]
echo Buttoncount=21
echo button1=!EXE!,1
echo cmd1=!RUN! merge --optimize --auto-rotate -s A4
echo param1=@"%%UL"
echo menu1=Merge into PDF
echo button2=!EXE!,2
echo cmd2=!RUN! ocr
echo param2=@"%%UL"
echo iconic2=0
echo menu2=OCR text to Markdown ^(Groq^)
echo button3=!EXE!,17
echo cmd3=!RUN! ocr --engine windows -i
echo param3=@"%%UL"
echo iconic3=0
echo menu3=OCR text into PDF ^(Windows OCR^)
echo button4=!EXE!,17
echo cmd4=!RUN! ocr --engine windows
echo param4=@"%%UL"
echo iconic4=0
echo menu4=OCR text to Markdown ^(Windows OCR^)
echo button5=!EXE!,3
echo cmd5=!RUN! strip -i
echo param5=@"%%UL"
echo iconic5=0
echo menu5=Remove metadata
echo button6=!EXE!,4
echo cmd6=!RUN! split
echo param6=%%P%%N
echo iconic6=0
echo menu6=Split PDF into pages
echo button7=!EXE!,5
echo cmd7=!RUN! extract
echo param7=?%%P%%N 1-5 -o "%%P%%O_extract.pdf"
echo iconic7=0
echo menu7=Extract pages from PDF
echo button8=!EXE!,6
echo cmd8=!RUN! info
echo param8=%%P%%N
echo iconic8=0
echo menu8=PDF info ^(pages, properties^)
echo button9=!EXE!,7
echo cmd9=!RUN! rotate
echo param9=?@"%%UL" -a 90 -i
echo iconic9=0
echo menu9=Rotate pages ^(PDF, JPEG^)
echo button10=!EXE!,8
echo cmd10=!RUN! resize
echo param10=?@"%%UL" --size A4 -i
echo iconic10=0
echo menu10=Resize pages ^(PDF, JPEG^)
echo button11=!EXE!,9
echo cmd11=!RUN! text
echo param11=%%P%%N --out "%%P%%O.txt"
echo iconic11=0
echo menu11=Extract text from PDF to TXT
echo button12=!EXE!,12
echo cmd12=!RUN! optimize
echo param12=%%P%%N
echo iconic12=0
echo menu12=Optimize PDF ^(compression^)
echo button13=!EXE!,11
echo cmd13=!RUN! stamp
echo param13=%%P%%N "stamp.png" --pick --position br --blend multiply --scale 1 -i
echo path12="!BASE!\"
echo iconic13=0
echo menu13=Apply stamp to PDF ^(stamp.png beside bpdf.exe^)
echo button14=!EXE!,15
echo cmd14=!RUN! number
echo param14=?%%P%%N --format "Page {n} of {total}"
echo iconic14=0
echo menu14=Number PDF pages
echo button15=!EXE!,16
echo cmd15=!RUN! watermark
echo param15=?%%P%%N "COPY"
echo iconic15=0
echo menu15=Watermark PDF
echo button16=!EXE!,13
echo cmd16=!RUN! metadata
echo param16=?%%P%%N -i --title "" --author ""
echo iconic16=0
echo menu16=Set PDF properties ^(title, author^)
echo button17=!EXE!,14
echo cmd17=!RUN! convert
echo param17=@"%%UL"
echo iconic17=0
echo menu17=Convert to JPEG
echo button18=!EXE!,18
echo cmd18=!RUN! protect
echo param18=?%%P%%N --no-modify --password ""
echo iconic18=0
echo menu18=Protect PDF with password ^(AES-256^)
echo button19=!EXE!,19
echo cmd19=!RUN! unlock
echo param19=?%%P%%N --password ""
echo iconic19=0
echo menu19=Remove password from PDF
echo button20=
echo iconic20=0
echo button21=!EXE!,10
echo cmd21=!RUN! doctor
echo iconic21=0
echo menu21=Check bpdf setup ^(doctor^)
)
set "RC=!errorlevel!"
if "!RC!"=="0" (
  echo Created "!OUT!"
  echo bpdf.exe: !EXE!
)
endlocal & set "RC=%RC%"

if defined OLD_CP chcp %OLD_CP% >nul
exit /b %RC%
