# bpdf

Лёгкий Rust-прототип GoPDF. Исходный проект в `D:\PROJECT\GoPDF` не изменяется.
Основная обработка PDF выполняется в памяти через `lopdf`; внешние программы
используются только там, где это действительно оправдано:

- `ffmpeg` — HEIC/HEIF и редкие кодировки изображений;
- установленный Microsoft Word/Excel — преобразование Office-файлов в PDF.

## Возможности

- объединение PDF, изображений, Word/Excel и текстовых файлов;
- диапазоны страниц во входе: `document.pdf:1-5,8,last`;
- HEIC/HEIF через внешний `ffmpeg`;
- рендеринг `.txt`/`.md` в PDF с кириллицей;
- Groq Vision OCR, HTTP/SOCKS5-прокси, повтор после `429`;
- ограниченный параллельный OCR и общий rate-limit gate;
- content-addressed OCR-кеш, учитывающий изображение, модель и промпт;
- использование готового текстового слоя PDF без обращения к API;
- приведение страниц к A4/Letter, центрирование и автоповорот;
- прозрачные PNG-штампы: `over`, `under`, `auto`;
- очистка метаданных JPEG/PNG без перекодирования и очистка PDF;
- разбиение, извлечение, поворот и изменение размера страниц;
- извлечение текста и структурная диагностика PDF;
- конвертация изображений и извлечение картинок из PDF в JPEG;
- `doctor`, самостоятельные `stamp`, `optimize`, `metadata`, `images` и `convert`;
- тихий режим и поток NDJSON для автоматизации;
- glob, каталоги, списки файлов через `@list.txt`, естественная сортировка и атомарная запись.

Если все входы `.txt`/`.md`, `merge` склеивает их как текст. В смешанном наборе
текст рендерится в страницы PDF.

## Сборка

```powershell
cargo build --release
cargo test --all-targets
```

Сборка не требует C/C++ компилятора. Для HEIC нужен `ffmpeg` в `PATH` либо
параметр `--ffmpeg D:\path\to\ffmpeg.exe`.

### Готовая Windows-сборка

```powershell
.\build.bat
```

Скрипт:

1. генерирует многослойную иконку 16–256 px;
2. берёт версию из `Cargo.toml`;
3. компилирует ICON, VERSIONINFO и application manifest через Windows SDK;
4. выполняет `cargo build --release --locked`;
5. складывает EXE, пример конфига и README в `dist`.

Требуются Rust MSVC toolchain и Windows 10/11 SDK с `rc.exe`. Скрипт ищет
`rc.exe` в `PATH` и установленных Windows Kits. Нестандартный путь можно задать:

```powershell
$env:RC_EXE = "D:\Windows Kits\10\bin\x64\rc.exe"
.\build.bat
```

В EXE встраиваются иконка, `FileVersion`, `ProductVersion`, описание продукта,
исходное имя файла, DPI-awareness и поддержка длинных Windows-путей.

## Примеры

```powershell
bpdf merge scan.jpg invoice.pdf:1-3 notes.txt -o result.pdf
bpdf merge @list.txt -o result.pdf
bpdf merge *.jpg -s A4 --auto-rotate --optimize -o scans.pdf
bpdf merge invoice.pdf --stamp seal.png --stamp-pos br --stamp-op 0.6
bpdf strip photo.jpg
bpdf split document.pdf pages
bpdf extract document.pdf 1-5,8 selected.pdf
bpdf rotate document.pdf -90 --pages even
bpdf resize document.pdf --size Letter
bpdf text document.pdf --out document.txt
bpdf inspect document.pdf --text
bpdf ocr scan.heic --out scan.md
bpdf doctor
bpdf stamp document.pdf seal.png --opacity 0.5
bpdf optimize document.pdf
bpdf metadata show document.pdf
bpdf metadata set document.pdf --author "Author" --title "Title"
bpdf images document.pdf extracted
bpdf convert photo.heic photo.png document.pdf -o converted_jpegs
bpdf ocr scan1.jpg scan2.jpg --jobs 2
bpdf --json doctor
bpdf --quiet merge *.jpg -o scans.pdf
```

`doctor` скрывает ключ и адрес прокси. Для Groq выполняется только безопасный
запрос списка моделей: изображение не отправляется и OCR-квота не расходуется.

Булевы параметры можно переопределить в обе стороны:
`--optimize`, `--optimize=false`, `--auto-rotate=false`.

`--json` выводит по одному JSON-объекту на строку (NDJSON), включая ошибки и
пути созданных файлов. `--quiet` скрывает прогресс, но сохраняет содержимое
команд `inspect`, `text` и `metadata show`, если оно отправляется в stdout.

OCR-кеш включён по умолчанию и хранится в
`%LOCALAPPDATA%\bpdf\ocr-cache`. Управление:

```powershell
bpdf ocr scan.jpg --no-cache
bpdf ocr scan.jpg --cache-dir D:\cache\bpdf
```

Одинаковые задания, запущенные параллельно, синхронизируются по ключу кеша и
не должны оплачивать один запрос дважды.

## Конфигурация

Скопируйте `config.example.jsonc` в `config.jsonc`. Сначала проверяется текущая
папка, затем папка рядом с `bpdf.exe`. Другой путь задаётся через
`--config file.jsonc`. Поддерживаются комментарии `//`.

Ключ API не передаётся внешнему процессу: HTTPS-запрос выполняется внутри
программы. Переменные окружения прокси намеренно не подхватываются; прокси
должен быть указан явно.

## Архитектура

- `input` — адаптеры PDF, изображений, Office и текста;
- `pdf` — сборка дерева страниц и базовые операции;
- `pdf::transform` — независимые стадии поворота, resize, штампа и оптимизации;
- `ocr`, `office`, `imageconv` — внешние сервисы и конвертеры;
- `app` — сценарии команд, `cli` — только описание интерфейса.

Новый входной формат добавляется в `input`, новое преобразование — отдельной
стадией в `pdf::transform`, поэтому они не сцеплены с реализацией `merge`.

## Ограничения прототипа

- Office-конвертация работает только в Windows и требует установленный,
  активированный Microsoft Office без открытых модальных окон.
- При объединении документные структуры (закладки, формы, Names) наследуются
  из первого PDF. Страницы и их ресурсы всех файлов сохраняются, но каталоги
  последующих PDF пока не объединяются.
- `optimize` выполняет безопасную структурную чистку, перенумерацию и сжатие
  потоков; это не полный набор эвристик `pdfcpu`.
- Облачный OCR нельзя полноценно проверить без действующего Groq API key.
