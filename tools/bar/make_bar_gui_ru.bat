@echo off
rem Генератор bpdf_gui_ru.bar (русской панели кнопок Total Commander для bpdf-gui.exe) рядом с этим файлом.
rem Сами кнопки будут брать иконки из bpdf.exe / bpdf-gui.exe. Если bpdf установлен внутри папки
rem Total Commander, пути запишутся через %COMMANDER_PATH%, иначе - абсолютными.
rem
rem   make_bar_gui_ru.bat            - автопоиск пути относительно TC
rem   make_bar_gui_ru.bat /default   - путь %COMMANDER_PATH%\tools\bpdf (для дистрибутивов)
rem
rem Сам скрипт сохранён в cp1251 + CRLF: bpdf_gui_ru.bar должен быть в этой же кодировке.
setlocal EnableExtensions DisableDelayedExpansion

for /f "tokens=2 delims=:." %%C in ('chcp') do set "OLD_CP=%%C"
chcp 1251 >nul

rem Скрипт лежит в папке bar рядом с bpdf-gui.exe (или прямо рядом с ним).
set "EXE_DIR=%~dp0.."
if exist "%~dp0bpdf-gui.exe" set "EXE_DIR=%~dp0."
for %%I in ("%EXE_DIR%") do set "EXE_DIR=%%~fI"
set "OUT=%~dp0bpdf_gui_ru.bar"
rem Путь в cmd= всегда в кавычках: в нём могут быть пробелы ("Program Files").
set Q="

if /i "%~1"=="/default" (
  set "BASE=%%COMMANDER_PATH%%\tools\bpdf"
  goto :write
)

if not exist "%EXE_DIR%\bpdf-gui.exe" (
  if exist "%EXE_DIR%\..\dist\bpdf-gui.exe" (
    for %%I in ("%EXE_DIR%\..\dist") do set "EXE_DIR=%%~fI"
  ) else (
    echo Предупреждение: рядом нет bpdf-gui.exe: "%EXE_DIR%"
  )
)

rem Поиск Total Commander: по %COMMANDER_PATH% (запуск из TC) или перебором
rem TOTALCMD*.EXE вверх по дереву от папки bpdf.
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
    echo bpdf-gui.exe не лежит внутри Total Commander "%TC_DIR%", пути будут абсолютными.
  ) else (
    echo Total Commander не найден, пути будут абсолютными.
  )
)

:write
setlocal EnableDelayedExpansion
set "EXE=!BASE!\bpdf.exe"
set "GUI=!BASE!\bpdf-gui.exe"
set "RUN=!Q!!GUI!!Q!"
> "!OUT!" (
echo [Buttonbar]
echo Buttoncount=22
echo button1=!EXE!,0
echo cmd1=!RUN!
echo param1=%%P%%N
echo iconic1=0
echo menu1=bpdf: Палитра действий ^(GUI окно для файла^)
echo button2=!EXE!,1
echo cmd2=!RUN! --icon 1
echo param2=-- merge @"%%UL" --optimize --auto-rotate -s A4
echo iconic2=0
echo menu2=Объединить в PDF
echo button3=!EXE!,2
echo cmd3=!RUN! --icon 2
echo param3=-- ocr @"%%UL"
echo iconic3=0
echo menu3=Распознать текст в MD ^(Groq^)
echo button4=!EXE!,17
echo cmd4=!RUN! --icon 17
echo param4=-- ocr @"%%UL" --engine windows -i
echo iconic4=0
echo menu4=Распознать текст в PDF ^(Windows OCR^)
echo button5=!EXE!,17
echo cmd5=!RUN! --icon 17
echo param5=-- ocr @"%%UL" --engine windows
echo iconic5=0
echo menu5=Распознать текст в MD ^(Windows OCR^)
echo button6=!EXE!,3
echo cmd6=!RUN! --icon 3
echo param6=-- strip @"%%UL" -i
echo iconic6=0
echo menu6=Удалить метаданные
echo button7=!EXE!,4
echo cmd7=!RUN! --icon 4
echo param7=-- split %%P%%N
echo iconic7=0
echo menu7=Разбить PDF постранично
echo button8=!EXE!,5
echo cmd8=!RUN! --icon 5
echo param8=-- extract %%P%%N "{ask:Номера страниц (например 1-5, 8, even, odd, reverse):1-5}" -o "%%P%%O_extract.pdf"
echo iconic8=0
echo menu8=Извлечь страницы из PDF
echo button9=!EXE!,6
echo cmd9=!RUN! --icon 6 --gui info
echo param9=-- info %%P%%N
echo iconic9=0
echo menu9=Информация о PDF ^(страницы, размер^)
echo button10=!EXE!,7
echo cmd10=!RUN! --icon 7
echo param10=-- rotate @"%%UL" -a "{ask:Угол поворота (90, 180, 270, portrait):90}" -i
echo iconic10=0
echo menu10=Повернуть страницы ^(PDF, JPEG^)
echo button11=!EXE!,8
echo cmd11=!RUN! --icon 8
echo param11=-- resize @"%%UL" --size "{ask:Формат страницы (A4, A3, A5, Letter):A4}" -i
echo iconic11=0
echo menu11=Изменить размер страниц ^(PDF, JPEG^)
echo button12=!EXE!,9
echo cmd12=!RUN! --icon 9
echo param12=-- text %%P%%N --out "%%P%%O.txt"
echo iconic12=0
echo menu12=Извлечь текст из PDF в TXT
echo button13=!EXE!,12
echo cmd13=!RUN! --icon 12
echo param13=-- optimize %%P%%N
echo iconic13=0
echo menu13=Оптимизировать PDF ^(сжатие^)
echo button14=!EXE!,11
echo cmd14=!RUN! --icon 11
echo param14=-- stamp %%P%%N "{exe_dir}\stamp.png" --pick --position br --blend multiply --scale 1 -i
echo iconic14=0
echo menu14=Наложить штамп на PDF ^(stamp.png рядом с bpdf.exe^)
echo button15=!EXE!,15
echo cmd15=!RUN! --icon 15
echo param15=-- number %%P%%N --format "{ask:Формат нумерации:Стр. {n} из {total}}"
echo iconic15=0
echo menu15=Пронумеровать страницы PDF
echo button16=!EXE!,16
echo cmd16=!RUN! --icon 16
echo param16=-- watermark %%P%%N "{ask:Текст водяного знака:КОПИЯ}"
echo iconic16=0
echo menu16=Водяной знак на PDF
echo button17=!EXE!,13
echo cmd17=!RUN! --icon 13
echo param17=-- metadata %%P%%N -i --title "{ask:Заголовок документа:}" --author "{ask:Автор документа:}"
echo iconic17=0
echo menu17=Правка метаданных PDF ^(заголовок, автор^)
echo button18=!EXE!,14
echo cmd18=!RUN! --icon 14
echo param18=-- convert @"%%UL"
echo iconic18=0
echo menu18=Конвертировать в JPEG
echo button19=!EXE!,18
echo cmd19=!RUN! --icon 18
echo param19=-- protect %%P%%N --no-modify --password "{ask_pass:Введите пароль для защиты PDF:}"
echo iconic19=0
echo menu19=Защитить PDF паролем ^(AES-256^)
echo button20=!EXE!,19
echo cmd20=!RUN! --icon 19
echo param20=-- unlock %%P%%N --password "{ask_pass:Введите пароль (или оставьте пустым):}"
echo iconic20=0
echo menu20=Снять пароль с PDF
echo button21=
echo iconic21=0
echo button22=!EXE!,10
echo cmd22=!RUN! --icon 10 --gui keep
echo param22=-- doctor
echo iconic22=0
echo menu22=Проверка окружения bpdf ^(doctor^)
)
set "RC=!errorlevel!"
if "!RC!"=="0" (
  echo Создан "!OUT!"
  echo bpdf-gui.exe: !GUI!
)
endlocal & set "RC=%RC%"

if defined OLD_CP chcp %OLD_CP% >nul
exit /b %RC%