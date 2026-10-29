@echo off
rem Generates bpdf_gui.bar (English Total Commander button bar for bpdf-gui.exe) beside this file.
rem The buttons take their icons from bpdf.exe / bpdf-gui.exe. If bpdf is installed inside
rem the Total Commander folder, paths are written using %COMMANDER_PATH%, otherwise absolute.
rem
rem   make_bar_gui.bat            - detect path automatically
rem   make_bar_gui.bat /default   - path %COMMANDER_PATH%\tools\bpdf (for distribution)
rem
setlocal EnableExtensions DisableDelayedExpansion

for /f "tokens=2 delims=:." %%C in ('chcp') do set "OLD_CP=%%C"
chcp 1252 >nul

rem The script lives in the bar folder beside bpdf-gui.exe (or right beside it).
set "EXE_DIR=%~dp0.."
if exist "%~dp0bpdf-gui.exe" set "EXE_DIR=%~dp0."
for %%I in ("%EXE_DIR%") do set "EXE_DIR=%%~fI"
set "OUT=%~dp0bpdf_gui.bar"
rem The path in cmd= is always quoted: it may contain spaces ("Program Files").
set Q="

if /i "%~1"=="/default" (
  set "BASE=%%COMMANDER_PATH%%\tools\bpdf"
  goto :write
)

if not exist "%EXE_DIR%\bpdf-gui.exe" (
  if exist "%EXE_DIR%\..\dist\bpdf-gui.exe" (
    for %%I in ("%EXE_DIR%\..\dist") do set "EXE_DIR=%%~fI"
  ) else (
    echo Warning: bpdf-gui.exe not found in "%EXE_DIR%"
  )
)

rem Total Commander folder: from %COMMANDER_PATH% (when launched from TC)
rem or by searching for TOTALCMD*.EXE upwards from the bpdf folder.
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
    echo bpdf-gui.exe is outside Total Commander folder "%TC_DIR%", paths will be absolute.
  ) else (
    echo Total Commander not found, paths will be absolute.
  )
)

:write
setlocal EnableDelayedExpansion
set "EXE=!BASE!\bpdf.exe"
set "GUI=!BASE!\bpdf-gui.exe"
set "RUN=!Q!!GUI!!Q!"
> "!OUT!" (
echo [Buttonbar]
echo Buttoncount=28
echo button1=!EXE!,0
echo cmd1=!RUN!
echo param1=%%P%%N
echo iconic1=0
echo menu1=bpdf: Action palette ^(GUI window for the file^)
echo button2=
echo iconic2=0
echo button3=!EXE!,1
echo cmd3=!RUN! --icon 1
echo param3=-- merge @"%%UL" --optimize --auto-rotate -s A4
echo iconic3=0
echo menu3=Merge into PDF
echo button4=!EXE!,4
echo cmd4=!RUN! --icon 4
echo param4=-- split %%P%%N
echo iconic4=0
echo menu4=Split PDF into pages
echo button5=!EXE!,5
echo cmd5=!RUN! --icon 5
echo param5=-- extract %%P%%N "{ask:Page numbers (e.g. 1-5, 8, even, odd, reverse):1-5}" -o "%%P%%O_extract.pdf"
echo iconic5=0
echo menu5=Extract pages from PDF
echo button6=!EXE!,7
echo cmd6=!RUN! --icon 7
echo param6=-- rotate @"%%UL" -a "{ask:Rotation angle (90, 180, 270, portrait):90}" -i
echo iconic6=0
echo menu6=Rotate pages ^(PDF, JPEG^)
echo button7=!EXE!,8
echo cmd7=!RUN! --icon 8
echo param7=-- resize @"%%UL" --size "{ask:Page size (A4, A3, A5, Letter):A4}" -i
echo iconic7=0
echo menu7=Resize pages ^(PDF, JPEG^)
echo button8=
echo iconic8=0
echo button9=!EXE!,11
echo cmd9=!RUN! --icon 11
echo param9=-- stamp %%P%%N "{exe_dir}\stamp.png" --pick --position br --blend multiply --scale 1 -i
echo iconic9=0
echo menu9=Stamp PDF ^(stamp.png beside bpdf.exe^)
echo button10=!EXE!,16
echo cmd10=!RUN! --icon 16
echo param10=-- watermark %%P%%N "{ask:Watermark text:COPY}"
echo iconic10=0
echo menu10=Watermark PDF
echo button11=!EXE!,15
echo cmd11=!RUN! --icon 15
echo param11=-- number %%P%%N --format "{ask:Page number format:Page {n} of {total}}"
echo iconic11=0
echo menu11=Number PDF pages
echo button12=
echo iconic12=0
echo button13=!EXE!,2
echo cmd13=!RUN! --icon 2
echo param13=-- ocr @"%%UL"
echo iconic13=0
echo menu13=OCR text to Markdown ^(Groq^)
echo button14=!EXE!,17
echo cmd14=!RUN! --icon 17
echo param14=-- ocr @"%%UL" --engine windows -i
echo iconic14=0
echo menu14=OCR text into PDF ^(Windows OCR^)
echo button15=!EXE!,17
echo cmd15=!RUN! --icon 17
echo param15=-- ocr @"%%UL" --engine windows
echo iconic15=0
echo menu15=OCR text to Markdown ^(Windows OCR^)
echo button16=!EXE!,9
echo cmd16=!RUN! --icon 9
echo param16=-- text %%P%%N --out "%%P%%O.txt"
echo iconic16=0
echo menu16=Extract PDF text to TXT
echo button17=
echo iconic17=0
echo button18=!EXE!,14
echo cmd18=!RUN! --icon 14
echo param18=-- convert @"%%UL"
echo iconic18=0
echo menu18=Convert to JPEG
echo button19=
echo iconic19=0
echo button20=!EXE!,12
echo cmd20=!RUN! --icon 12
echo param20=-- optimize %%P%%N
echo iconic20=0
echo menu20=Optimize PDF ^(compress^)
echo button21=!EXE!,3
echo cmd21=!RUN! --icon 3
echo param21=-- strip @"%%UL" -i
echo iconic21=0
echo menu21=Remove metadata
echo button22=!EXE!,13
echo cmd22=!RUN! --icon 13
echo param22=-- metadata %%P%%N -i --title "{ask:Document title:}" --author "{ask:Document author:}"
echo iconic22=0
echo menu22=Edit PDF metadata ^(title, author^)
echo button23=
echo iconic23=0
echo button24=!EXE!,18
echo cmd24=!RUN! --icon 18
echo param24=-- protect %%P%%N --no-modify --password "{ask_pass:Password to protect the PDF:}"
echo iconic24=0
echo menu24=Protect PDF with password ^(AES-256^)
echo button25=!EXE!,19
echo cmd25=!RUN! --icon 19
echo param25=-- unlock %%P%%N --password "{ask_pass:Password (or leave empty):}"
echo iconic25=0
echo menu25=Remove password from PDF
echo button26=
echo iconic26=0
echo button27=!EXE!,6
echo cmd27=!RUN! --icon 6 --gui info
echo param27=-- info %%P%%N
echo iconic27=0
echo menu27=PDF info ^(pages, size^)
echo button28=!EXE!,10
echo cmd28=!RUN! --icon 10 --gui keep
echo param28=-- doctor
echo iconic28=0
echo menu28=Check bpdf setup ^(doctor^)
)
set "RC=!errorlevel!"
if "!RC!"=="0" (
  echo Created "!OUT!"
  echo bpdf-gui.exe: !GUI!
)
endlocal & set "RC=%RC%"

if defined OLD_CP chcp %OLD_CP% >nul
exit /b %RC%
