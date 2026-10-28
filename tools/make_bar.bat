@echo off
rem Генерирует bpdf.bar (панель кнопок Total Commander) рядом с этим файлом.
rem Файл должен лежать рядом с bpdf.exe. Если bpdf.exe находится внутри папки
rem Total Commander, путь пишется через %COMMANDER_PATH%, иначе - абсолютный.
rem
rem   make_bar.bat            - определить путь автоматически
rem   make_bar.bat /default   - путь %COMMANDER_PATH%\tools\bpdf (для дистрибутива)
rem
rem Сам батник хранится в cp1251 + CRLF: bpdf.bar должен быть в той же кодировке.
setlocal EnableExtensions DisableDelayedExpansion

for /f "tokens=2 delims=:." %%C in ('chcp') do set "OLD_CP=%%C"
chcp 1251 >nul

set "EXE_DIR=%~dp0"
set "EXE_DIR=%EXE_DIR:~0,-1%"
set "OUT=%~dp0bpdf.bar"
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
echo Buttoncount=20
echo button1=!EXE!,1
echo cmd1=!RUN! merge --optimize --auto-rotate -s A4
echo param1=@"%%UL"
echo menu1=Объединить в PDF
echo button2=!EXE!,2
echo cmd2=!RUN! ocr
echo param2=@"%%UL"
echo iconic2=0
echo menu2=Распознать текст в MD ^(Groq^)
echo button3=!EXE!,17
echo cmd3=!RUN! ocr --engine windows -i
echo param3=@"%%UL"
echo iconic3=0
echo menu3=Распознать текст в PDF ^(Windows OCR^)
echo button4=!EXE!,3
echo cmd4=!RUN! strip -i
echo param4=@"%%UL"
echo iconic4=0
echo menu4=Удалить метаданные
echo button5=!EXE!,4
echo cmd5=!RUN! split
echo param5=%%P%%N
echo iconic5=0
echo menu5=Разбить PDF постранично
echo button6=!EXE!,5
echo cmd6=!RUN! extract
echo param6=?%%P%%N 1-5 -o "%%P%%O_extract.pdf"
echo iconic6=0
echo menu6=Извлечь страницы из PDF
echo button7=!EXE!,6
echo cmd7=!RUN! info
echo param7=%%P%%N
echo iconic7=0
echo menu7=Информация о PDF ^(страницы, свойства^)
echo button8=!EXE!,7
echo cmd8=!RUN! rotate
echo param8=?@"%%UL" -a 90 -i
echo iconic8=0
echo menu8=Повернуть страницы ^(PDF, JPEG^)
echo button9=!EXE!,8
echo cmd9=!RUN! resize
echo param9=?@"%%UL" --size A4 -i
echo iconic9=0
echo menu9=Изменить размер страниц ^(PDF, JPEG^)
echo button10=!EXE!,9
echo cmd10=!RUN! text
echo param10=%%P%%N --out "%%P%%O.txt"
echo iconic10=0
echo menu10=Извлечь текст из PDF в TXT
echo button11=!EXE!,12
echo cmd11=!RUN! optimize
echo param11=%%P%%N
echo iconic11=0
echo menu11=Оптимизировать PDF ^(сжатие^)
echo button12=!EXE!,11
echo cmd12=!RUN! stamp
echo param12=%%P%%N "stamp.png" --pick --position br --blend multiply --scale 1 -i
echo path12="!BASE!\"
echo iconic12=0
echo menu12=Вставить штамп в PDF ^(stamp.png рядом с bpdf.exe^)
echo button13=!EXE!,15
echo cmd13=!RUN! number
echo param13=?%%P%%N --format "Стр. {n} из {total}"
echo iconic13=0
echo menu13=Пронумеровать страницы PDF
echo button14=!EXE!,16
echo cmd14=!RUN! watermark
echo param14=?%%P%%N "КОПИЯ"
echo iconic14=0
echo menu14=Водяной знак в PDF
echo button15=!EXE!,13
echo cmd15=!RUN! metadata
echo param15=?%%P%%N -i --title "" --author ""
echo iconic15=0
echo menu15=Задать свойства PDF ^(название, автор^)
echo button16=!EXE!,14
echo cmd16=!RUN! convert
echo param16=@"%%UL"
echo iconic16=0
echo menu16=Конвертировать в JPEG
echo button17=!EXE!,18
echo cmd17=!RUN! protect
echo param17=?%%P%%N --no-modify --password ""
echo iconic17=0
echo menu17=Защитить PDF паролем ^(AES-256^)
echo button18=!EXE!,19
echo cmd18=!RUN! unlock
echo param18=?%%P%%N --password ""
echo iconic18=0
echo menu18=Снять пароль с PDF
echo button19=
echo iconic19=0
echo button20=!EXE!,10
echo cmd20=!RUN! doctor
echo iconic20=0
echo menu20=Проверка настроек bpdf ^(doctor^)
)
set "RC=!errorlevel!"
if "!RC!"=="0" (
  echo Создан "!OUT!"
  echo bpdf.exe: !EXE!
)
endlocal & set "RC=%RC%"

if defined OLD_CP chcp %OLD_CP% >nul
exit /b %RC%
