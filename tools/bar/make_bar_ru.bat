@echo off
rem Генерирует bpdf_ru.bar (русскую панель кнопок Total Commander) рядом с этим файлом.
rem Скрипт лежит в папке bar рядом с bpdf.exe. Если bpdf.exe находится внутри папки
rem Total Commander, путь пишется через %COMMANDER_PATH%, иначе - абсолютный.
rem
rem   make_bar_ru.bat            - определить путь автоматически
rem   make_bar_ru.bat /default   - путь %COMMANDER_PATH%\tools\bpdf (для дистрибутива)
rem
rem Сам батник хранится в cp1251 + CRLF: bpdf_ru.bar должен быть в той же кодировке.
setlocal EnableExtensions DisableDelayedExpansion

for /f "tokens=2 delims=:." %%C in ('chcp') do set "OLD_CP=%%C"
chcp 1251 >nul

rem Скрипт лежит в папке bar рядом с bpdf.exe (или прямо рядом с ним).
set "EXE_DIR=%~dp0.."
if exist "%~dp0bpdf.exe" set "EXE_DIR=%~dp0."
for %%I in ("%EXE_DIR%") do set "EXE_DIR=%%~fI"
set "OUT=%~dp0bpdf_ru.bar"
set "Q="

if /i "%~1"=="/default" (
  set "BASE=%%COMMANDER_PATH%%\tools\bpdf"
  goto :write
)

if not exist "%EXE_DIR%\bpdf.exe" (
  echo Предупреждение: рядом нет bpdf.exe: "%EXE_DIR%"
)

rem Папка Total Commander: из %COMMANDER_PATH% (запуск из TC) или поиском
rem TOTALCMD*.EXE вверх по дереву от папки bpdf.exe.
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
    echo bpdf.exe не внутри папки Total Commander "%TC_DIR%", путь будет абсолютным.
  ) else (
    echo Total Commander не найден, путь будет абсолютным.
  )
)
rem Путь с пробелами в cmd= надо брать в кавычки, иначе TC разрежет его на аргументы.
if not "%EXE_DIR: =%"=="%EXE_DIR%" set Q="

:write
setlocal EnableDelayedExpansion
set "EXE=!BASE!\bpdf.exe"
set "RUN=!Q!!EXE!!Q!"
> "!OUT!" (
echo [Buttonbar]
echo Buttoncount=26
echo button1=!EXE!,1
echo cmd1=!RUN! merge --optimize --auto-rotate -s A4
echo param1=@"%%UL"
echo menu1=Объединить в PDF
echo button2=!EXE!,4
echo cmd2=!RUN! split
echo param2=%%P%%N
echo iconic2=0
echo menu2=Разбить PDF постранично
echo button3=!EXE!,5
echo cmd3=!RUN! extract
echo param3=?%%P%%N 1-5 -o "%%P%%O_extract.pdf"
echo iconic3=0
echo menu3=Извлечь страницы из PDF
echo button4=!EXE!,7
echo cmd4=!RUN! rotate
echo param4=?@"%%UL" -a 90 -i
echo iconic4=0
echo menu4=Повернуть страницы ^(PDF, JPEG^)
echo button5=!EXE!,8
echo cmd5=!RUN! resize
echo param5=?@"%%UL" --size A4 -i
echo iconic5=0
echo menu5=Изменить размер страниц ^(PDF, JPEG^)
echo button6=
echo iconic6=0
echo button7=!EXE!,11
echo cmd7=!RUN! stamp
echo param7=%%P%%N "stamp.png" --pick --position br --blend multiply --scale 1 -i
echo path7="!BASE!\"
echo iconic7=0
echo menu7=Вставить штамп в PDF ^(stamp.png рядом с bpdf.exe^)
echo button8=!EXE!,16
echo cmd8=!RUN! watermark
echo param8=?%%P%%N "КОПИЯ"
echo iconic8=0
echo menu8=Водяной знак в PDF
echo button9=!EXE!,15
echo cmd9=!RUN! number
echo param9=?%%P%%N --format "Стр. {n} из {total}"
echo iconic9=0
echo menu9=Пронумеровать страницы PDF
echo button10=
echo iconic10=0
echo button11=!EXE!,2
echo cmd11=!RUN! ocr
echo param11=@"%%UL"
echo iconic11=0
echo menu11=Распознать текст в MD ^(Groq^)
echo button12=!EXE!,17
echo cmd12=!RUN! ocr --engine windows -i
echo param12=@"%%UL"
echo iconic12=0
echo menu12=Распознать текст в PDF ^(Windows OCR^)
echo button13=!EXE!,17
echo cmd13=!RUN! ocr --engine windows
echo param13=@"%%UL"
echo iconic13=0
echo menu13=Распознать текст в MD ^(Windows OCR^)
echo button14=!EXE!,9
echo cmd14=!RUN! text
echo param14=%%P%%N --out "%%P%%O.txt"
echo iconic14=0
echo menu14=Извлечь текст из PDF в TXT
echo button15=
echo iconic15=0
echo button16=!EXE!,14
echo cmd16=!RUN! convert
echo param16=@"%%UL"
echo iconic16=0
echo menu16=Конвертировать в JPEG
echo button17=
echo iconic17=0
echo button18=!EXE!,12
echo cmd18=!RUN! optimize
echo param18=%%P%%N
echo iconic18=0
echo menu18=Оптимизировать PDF ^(сжатие^)
echo button19=!EXE!,3
echo cmd19=!RUN! strip -i
echo param19=@"%%UL"
echo iconic19=0
echo menu19=Удалить метаданные
echo button20=!EXE!,13
echo cmd20=!RUN! metadata
echo param20=?%%P%%N -i --title "" --author ""
echo iconic20=0
echo menu20=Задать свойства PDF ^(название, автор^)
echo button21=
echo iconic21=0
echo button22=!EXE!,18
echo cmd22=!RUN! protect
echo param22=?%%P%%N --no-modify --password ""
echo iconic22=0
echo menu22=Защитить PDF паролем ^(AES-256^)
echo button23=!EXE!,19
echo cmd23=!RUN! unlock
echo param23=?%%P%%N --password ""
echo iconic23=0
echo menu23=Снять пароль с PDF
echo button24=
echo iconic24=0
echo button25=!EXE!,6
echo cmd25=!RUN! info
echo param25=%%P%%N
echo iconic25=0
echo menu25=Информация о PDF ^(страницы, свойства^)
echo button26=!EXE!,10
echo cmd26=!RUN! doctor
echo iconic26=0
echo menu26=Проверка настроек bpdf ^(doctor^)
)
set "RC=!errorlevel!"
if "!RC!"=="0" (
  echo Создан "!OUT!"
  echo bpdf.exe: !EXE!
)
endlocal & set "RC=%RC%"

if defined OLD_CP chcp %OLD_CP% >nul
exit /b %RC%
