# bpdf — Fast, Lightweight PDF & Document Toolkit in Rust

[![Version](https://img.shields.io/badge/Version-0.5.2-blue.svg)](Cargo.toml)
[![Rust](https://img.shields.io/badge/Rust-1.97%2B-orange.svg?logo=rust)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/Platform-Windows%20%7C%20Linux%20%7C%20macOS-lightgrey.svg)]()
[![Language](https://img.shields.io/badge/Язык-Русский-blue.svg)](README.ru.md)

**bpdf** is a high-performance, single-binary CLI tool written in Rust for comprehensive processing of PDF files, images, text documents, e-books, and Microsoft Office files (Word, Excel, PowerPoint).

Designed for speed, low memory footprint, safe atomic file operations, and flexible scripting automation (NDJSON streaming, natural file sorting, input manifests, and environment variable expansion).

---

## Key Features

- **Universal Merge:** Seamlessly combine PDFs, standard images, comic archives (CBZ), JPEG 2000 / JPEG-LS / JPEG XR, HEIC/AVIF/PSD, camera RAW photos, e-books (EPUB, FB2, FB2.ZIP), Office documents (Word, Excel, PowerPoint, OpenDocument), and text/source-code files into a single PDF or merged text document.
- **Multipage Images & Animation Frames:** All TIFF pages, comic book archives (CBZ with natural sort), and all frames of GIF, APNG, and animated WebP become distinct sequential PDF pages.
- **OCR & Searchable (Sandwich) PDF:** Extract text to Markdown or create a PDF with an invisible searchable text layer (`-o searchable.pdf` / `--in-place`) using cloud **Groq Vision** (Qwen / Llama) or native local **Windows Media OCR** (offline, zero external API keys).
- **Table of Contents & Bookmarks:** Automatically build hierarchical PDF Outlines when merging documents (`merge --bookmarks`).
- **Content-Addressed OCR Cache:** SHA-256 hash-based disk caching (image hash + engine/model + prompt). Repetitive tasks and concurrent workers resolve instantly with zero redundant network requests.
- **Network Proxy Support:** Full HTTP and SOCKS5 proxy compatibility for bypassing network restrictions or corporate firewalls.
- **Watermarks & Stamps:** Overlay transparent PNG stamps with precise anchor positioning (`br`, `center`, or millimeter X,Y offsets), scale factors, opacity, and blend modes (`multiply` for authentic wet-ink appearance, `over`, `under`, `auto`).
- **Page Normalization & Resizing:** Fit and center pages to standard A4 or Letter, unify orientation by majority vote, or preserve native dimensions (`none`/`original`/`keep`).
- **Lossless Metadata Stripping (Strip):** Remove EXIF / metadata from JPEG and PNG without re-encoding pixels; sanitize PDF Info / XMP metadata.
- **PDF Manipulation Suite:** Page splitting (`split`), range extraction (`extract`), rotation (`rotate`), resizing (`resize`), and DPI-aware image optimization (`optimize`).
- **Safe In-Place Edits:** Safe in-place file replacement when `-o` is explicitly provided, with atomic temporary write-and-rename semantics to prevent data corruption.
- **PDF Info Metadata:** View (`metadata show`) and update (`metadata set`) Title, Author, Subject, Keywords, and Creator properties.
- **System Diagnostics (`doctor`):** Inspect runtime environment, configuration validity, helper binaries (`ffmpeg`, `powershell`, MS Office), and verify Groq API connectivity without consuming OCR quotas.
- **Automation & Total Commander:** Optimized for **Total Commander** workflows with a ready-to-use button bar (`bpdf.bar`), embedded action icons, and `@list.txt` / `@"%UL"` list file support. Features silent mode (`--quiet`), NDJSON streaming (`--json`), glob expansion (`*.jpg`), and natural alphanumeric sorting (`scan_1.jpg`, `scan_2.jpg`, `scan_10.jpg`).

---

## Requirements & External Dependencies

| Feature / Format | Windows | Linux / macOS | Notes |
| :--- | :--- | :--- | :--- |
| **Core (PDF, JPEG, PNG, BMP, GIF, TIFF, WebP, APNG)** | Native Pure Rust | Native Pure Rust | No external dependencies required. |
| **Camera RAW (`.cr2`, `.nef`, `.arw`, `.dng`, `.raf`, etc.)** | Native Pure Rust | Native Pure Rust | Instant extraction of full-size hardware JPEG preview. Optional sensor development on Windows via WIC (`raw_develop = true`). |
| **Windows Formats (JPEG XR `.jxr`, `.wdp`, `.hdp`, `.ico`)** | Native WIC | Via FFmpeg | Uses Windows Imaging Component. |
| **Extended Formats (HEIC/AVIF/PSD/JPEG 2000/HDR/EXR/etc.)** | FFmpeg | FFmpeg | Requires `ffmpeg` in `PATH` or `--ffmpeg path/to/ffmpeg`. |
| **Office Docs (DOCX, XLSX, PPTX, RTF, ODT, ODS, ODP)** | MS Office / Pure Rust | Pure Rust Fallback | Accurate layout via COM automation if MS Office is installed; otherwise fast built-in XML text extractor. |
| **E-books (EPUB, FB2, FB2.ZIP)** | Native Pure Rust | Native Pure Rust | Built-in ZIP/XML parsing and chapter structure extraction. |
| **OCR (Windows Media OCR)** | Native Windows 10/11 | — | Offline, built-in, no API keys needed. |
| **OCR (Groq Cloud Vision)** | Supported | Supported | Requires `groq_api_key` in `config.toml` or environment variable. |

---

## Installation & Building

### From Source via Cargo

```powershell
cargo build --release
cargo test --all-targets
```

Build is completely clean and does not require third-party C/C++ compilers.

### Windows Optimized Build Script

To build an optimized Windows binary with embedded version information, application icon, and long path manifest:

```powershell
.\build.bat
```

The script will:
1. Validate and compile the multi-resolution icon (16x16 – 256x256 px).
2. Read the package version from `Cargo.toml`.
3. Compile Windows resource manifests using `rc.exe` (Windows SDK).
4. Run `cargo build --release --locked`.
5. Package the executable, sample config, and documentation into the `dist/` directory.

---

## Global Flags

Global flags can be passed before any subcommand:

- `--config <PATH>` — Path to custom configuration file (defaults to `config.toml` in current directory or beside `bpdf.exe`).
- `--quiet` — Silent mode; suppresses progress meters and informative messages.
- `--json` — Output results and progress events as streamable NDJSON objects (one JSON object per line).
- `--fail-fast` — Abort batch operations immediately upon the first error. Without this flag, remaining items continue processing and errors are summarized at the end.
- `-h, --help` — Print help information.
- `-V, --version` — Print version.

---

## Commands & Usage

### 1. `bpdf merge`
Combines PDF files, images, e-books, Office documents, and text files into a single document.

```powershell
bpdf merge <INPUTS>... [OPTIONS]
```

**Options:**
- `<INPUTS>...` — Input files, folders, glob patterns (`*.jpg`), page ranges (`doc.pdf:1-5`), or file lists (`@list.txt`). *(Required)*.
- `-o, --out <PATH>` — Output path. If omitted, an automatic name is generated (e.g. `doc_merged.pdf`).
- `-s, --size <SIZE>` — Target page size: `A4`, `Letter`, or `none`/`original`/`keep` to preserve input PDF page dimensions.
- `--auto-rotate[=true|false]` — Rotate pages to match the dominant orientation of the document.
- `--no-rotate` — Disable auto-rotation, preserving each page's native orientation.
- `--stamp <PATH>` — Path to PNG image to overlay as a stamp or watermark.
- `--stamp-pos <POS>` — Stamp anchor: `br` (bottom-right), `bl`, `tr`, `tl`, `c` (center), `tc`, `bc`, `l`, `r` or millimeter offset `X,Y` from bottom-right (e.g. `--stamp-pos="-130,30"`).
- `--stamp-dpi <DPI>` — Stamp resolution in DPI (auto-detected from PNG metadata if present; default `96.0`).
- `--stamp-scale <SCALE>` — Stamp scale multiplier (default `1.0` for calibrated DPI; `0.0` for auto-fit up to 25% of page).
- `--stamp-op <OPACITY>` — Stamp opacity from `0.0` (transparent) to `1.0` (opaque).
- `--stamp-pages <PAGES>` — Pages to stamp (`all`, `first`, `last`, `1-5`, `even`, `odd`).
- `--stamp-mode <MODE>` — Layering mode: `auto` (under text if fonts exist, over images), `over`, `under`.
- `--stamp-blend <MODE>` — Blend mode (`normal`, `multiply`, `screen`, `overlay`, etc.; `multiply` provides authentic ink appearance).
- `--keep-icc[=true|false]` — Preserve ICC color profiles in embedded images (default `false` to save space).
- `--optimize[=true|false]` — Optimize PDF structure and downsample oversized images to `image_dpi`.
- `--strip-meta[=true|false]` — Strip metadata from generated PDF.
- `--bookmarks[=true|false]` — Generate PDF Outlines (table of contents) for each merged file.
- `--author <STRING>` — Set Author metadata property.
- `--creator <STRING>` — Set Creator metadata property.
- `--ffmpeg <PATH>` — Custom path to `ffmpeg.exe`.

---

### 2. `bpdf ocr`
Extracts text from documents and images using cloud Groq Vision OCR or local Windows Media OCR. When outputting to `.pdf`, creates a **Searchable (Sandwich) PDF** with an invisible text layer aligned over the scans.

```powershell
bpdf ocr <INPUTS>... [OPTIONS]
```

**Options:**
- `<INPUTS>...` — Input files (JPG, PNG, PDF, RAW, etc., including page ranges: `scan.pdf:1-5`), directories, or globs.
- `-o, --out <PATH>` — Output destination:
  - If `.pdf` extension (e.g. `-o searchable.pdf`) — outputs a **Searchable PDF**.
  - If `.md` or omitted — outputs Markdown files with recognized text.
- `--in-place` — Inject the searchable text layer directly into the source PDF file (overwrites original safely).
- `--engine <ENGINE>` — Recognition engine: `groq`, `windows` (or `winocr`), `auto`.
- `--lang <LANG>` — Language tag for Windows OCR (e.g. `en-US`, `ru`, `de-DE`).
- `--proxy <URL>` — HTTP or SOCKS5 proxy (`http://...` or `socks5://...`).
- `--model <NAME>` — Groq Vision model (default `qwen/qwen3.6-27b`).
- `--prompt <TEXT>` — Custom prompt instructions for vision model.
- `--endpoint <URL>` — Groq API endpoint URL.
- `--force-ocr` — Force OCR processing even if the PDF already contains extractable text.
- `--jobs <NUM>` — Concurrent worker threads (from 1 to 64, default `1`).
- `--no-cache` — Disable OCR disk cache.
- `--cache-dir <PATH>` — Custom path for OCR cache directory.

---

### 3. `bpdf split`
Splits a multipage PDF into separate single-page PDF files.

```powershell
bpdf split <INPUT> [OUTPUT_DIR]
```

---

### 4. `bpdf extract`
Extracts specified page ranges from a PDF into a new PDF document.

```powershell
bpdf extract <INPUT> <PAGES> [OUTPUT]
```

**Example:**
```powershell
bpdf extract document.pdf 1-5,8,last output.pdf
```

---

### 5. `bpdf inspect`
Displays detailed diagnostic information about a PDF file (PDF version, page count, embedded fonts, images, dimensions, rotation).

```powershell
bpdf inspect <INPUT> [--text]
```

---

### 6. `bpdf strip`
Removes EXIF metadata from JPEG/PNG images without re-encoding, and sanitizes PDF Info / XMP dictionaries.

```powershell
bpdf strip <INPUTS>... [OPTIONS]
```

---

### 7. `bpdf rotate`
Rotates PDF pages or JPEG images by multiples of 90 degrees, or aligns them to `portrait` or `landscape`.

```powershell
bpdf rotate <INPUTS>... <DEGREES> [-p <PAGES>] [-o <OUT>]
bpdf rotate <INPUTS>... --orient <landscape|portrait> [-p <PAGES>] [-o <OUT>]
```

---

### 8. `bpdf resize`
Scales and centers PDF pages onto A4 or Letter sheets, or resizes JPEG images by long/short edge dimensions.

```powershell
bpdf resize <INPUTS>... [-s <SIZE>] [--long-edge <PX>] [--short-edge <PX>] [-o <OUT>]
```

---

### 9. `bpdf text`
Instantly extracts the native text layer from a PDF document to stdout or file without network access.

```powershell
bpdf text <INPUT> [-o <OUT>]
```

---

### 10. `bpdf stamp`
Applies a transparent PNG stamp or seal to an existing PDF document.

```powershell
bpdf stamp <INPUT> <STAMP> [OPTIONS]
```

**Examples:**
```powershell
# Apply seal to last page in bottom-left with authentic ink multiplication
bpdf stamp "Contract.pdf" "seal.png" --position="-130,30" --mode over --blend multiply --pages last -o "Contract_stamped.pdf"

# Apply stamp across all pages in-place
bpdf stamp invoice.pdf stamp.png --position br --blend multiply -o invoice.pdf
```

---

### 11. `bpdf optimize`
Performs structural cleanup (removes dead objects, compresses streams) and resamples oversized embedded images to target `image_dpi`.

```powershell
bpdf optimize <INPUT> [-o <OUT>]
```

---

### 12. `bpdf metadata`
View and edit PDF document information dictionary entries (Title, Author, Subject, Keywords, Creator).

```powershell
# Show metadata
bpdf metadata show document.pdf

# Set metadata
bpdf metadata set document.pdf --title "Financial Report 2026" --author "John Doe" -o document.pdf
```

---

### 13. `bpdf convert`
Converts images (multipage TIFF, JPEG 2000, JPEG-LS, JPEG XR, ICO, camera RAW, etc.) and PDF pages to JPEG format.

```powershell
bpdf convert <INPUTS>... [OPTIONS]
```

---

### 14. `bpdf doctor`
Validates runtime environment, configuration file, helper binaries (`ffmpeg`, `powershell`, MS Office), RAW decoders, and Groq API authentication.

```powershell
bpdf doctor
```

---

## Page Selection Syntax

In commands like `extract`, `rotate`, `resize`, `stamp`, `ocr`, and input specs:
- `all` — all pages;
- `first` — first page;
- `last` (or `l`) — last page;
- `even` — even pages (2, 4, 6...);
- `odd` — odd pages (1, 3, 5...);
- `1-5,8,last` — combination of individual pages and ranges.

### Per-Input Page Specifier in `merge`
```powershell
bpdf merge cover.jpg report.pdf:1-3,last notes.txt -o combined.pdf
```

### File Lists (`@manifest.txt`)
```powershell
bpdf merge @files.txt -o output.pdf
```

*Example `files.txt`:*
```text
# Comments are ignored
C:\docs\cover.jpg
C:\docs\report.pdf:1-10
C:\docs\appendix.pdf:even
```

### Total Commander Integration
`bpdf` is fully optimized for **Total Commander**:
- **Button Bar:** Includes a pre-configured `bpdf.bar` button bar file for easy addition to your Total Commander toolbars.
- **List File Support:** Seamlessly works with Total Commander's selected files lists using parameter `@\"%UL\"` (UTF-8 list of selected files) or single-file `%P%N`.
- **Embedded Action Icons:** The Windows executable embeds individual action icons for each toolbar command (merge, OCR, split, extract, rotate, stamp, metadata).

---

## Configuration (`config.toml`)

`bpdf` automatically resolves configuration files in the following order:
1. Path passed via `--config <PATH>`;
2. `config.toml` in the current working directory;
3. `config.toml` in the executable's folder.

### Example `config.toml`

```toml
# Groq API Key for Vision OCR
groq_api_key = "%GROQ_API_KEY%"

# Proxy Server (HTTP or SOCKS5)
proxy = ""
# proxy = "socks5://127.0.0.1:10808"

# Default Metadata
author = "My Company"
creator = "bpdf toolkit"

# PDF Processing Defaults
auto_rotate = false
keep_icc = false
optimize = false
strip_metadata = false
bookmarks = false
page_size = "A4"
jpeg_quality = 95 # JPEG quality for merge and optimize
image_dpi = 150   # Target DPI for downsampling; 0 disables downsampling
raw_develop = false # false = fast pure Rust preview, true = WIC sensor development

# OCR Settings
ocr_engine = "groq" # "groq" (cloud vision), "windows" (local WinOCR), or "auto"
ocr_model = "qwen/qwen3.6-27b"
# ocr_prompt = '''Extract all text exactly as it appears...'''
ocr_endpoint = "https://api.groq.com/openai/v1/chat/completions"
ocr_jobs = 1
ocr_cache = true
# ocr_cache_dir = 'D:\cache\bpdf-ocr'
ocr_timeout_seconds = 120
ocr_max_tokens = 4096

# Third-party tools
ffmpeg = 'ffmpeg'
powershell = 'powershell.exe'
office_timeout_seconds = 120
```

See [`config.example.toml`](config.example.toml) for complete descriptions of all parameters.

---

## Practical Examples

```powershell
# Merge images, scanned PDF, and notes into a single PDF
bpdf merge scan.jpg invoice.pdf:1-3 notes.txt -o result.pdf

# Merge all images in folder to A4 with auto-rotation and DPI optimization
bpdf merge *.jpg -s A4 --auto-rotate --optimize -o scans.pdf

# Recognize scanned document with Groq Vision to Markdown
bpdf ocr scan.jpg -o scan.md

# Create Searchable PDF with invisible text layer
bpdf ocr scan.pdf -o searchable.pdf

# In-place Searchable PDF generation across multiple files
bpdf ocr doc1.pdf doc2.pdf --in-place

# Convert camera RAW files to JPEG instantly (Pure Rust preview extraction)
bpdf convert photo.cr3 photo.nef photo.arw --out converted/

# Strip EXIF metadata from all photos in folder in-place
bpdf strip photos/*.jpg

# Apply realistic wet-ink stamp to PDF
bpdf stamp contract.pdf seal.png --position br --blend multiply -o contract_stamped.pdf

# System & integrations health check
bpdf doctor
```
