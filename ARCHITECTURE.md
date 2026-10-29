# bpdf architecture

`bpdf` is organised around explicit command boundaries and shared services.
The CLI is not frozen yet: 0.8.0 unified output handling and removed redundant
options (see `CHANGELOG.md`).

## Dependency direction

```text
main.rs
  -> lib.rs (process entry point)
    -> commands/* (use cases and output policy)
      -> pdf, imageconv, ocr, office, textpdf, raw, ebook, archive (domain services/adapters)
        -> atomic, process, parallel, encoding, hash, font_subset, winocr, winpdf, wic (infrastructure)
```

The lower layers do not call command modules. Commands own CLI-specific policy
such as default output names, `--fail-fast`, summaries, and user messages.

## Command modules

- `commands/merge.rs`: input pipeline and PDF post-processing.
- `commands/ocr.rs`: batch OCR orchestration; network mechanics remain in
  `ocr.rs`.
- `commands/convert.rs`: complete output planning before writes and image/PDF
  conversion.
- `commands/strip.rs`: metadata-removal policy.
- `commands/pdf_edit.rs`: single-document PDF commands, including `protect`
  and `unlock`.
- `commands/common.rs`: atomic edit boundary, path identity, output registry,
  batch summaries (partial failure exits with code 2) and `edit_each`, the
  driver of per-file batch edits (`strip`, `rotate`, `resize`).
- `commands/mod.rs`: routing only.

## Shared contracts

- Format classification is implemented once in `formats.rs` and is reused by
  directory expansion, merge, OCR, strip, convert, Office, and image handling.
- DPI sizing, resampling, alpha composition, and JPEG encoding are implemented
  once in `imageconv.rs`.
- FFmpeg is an external decoder adapter only; decoded frames return to the same
  DPI sizing and JPEG encoding path as images handled by the Rust decoder.
- Camera RAW files extract embedded JPEG previews in pure Rust (`raw.rs`) on all
  platforms; full sensor demosaicing via Windows Imaging Component (WIC) and
  the Microsoft Raw Image Extension is supported on Windows as an opt-in mode
  or fallback.
- PSD/PSB composites are decoded in pure Rust (`psd.rs`); layouts it does not
  handle (32-bit, bitmap, ZIP) fall back to FFmpeg.
- HEIC/HEIF and AVIF try the WIC adapter first and fall back to FFmpeg when
  the codec is missing or fails mid-decode.
- JPEG XR/HD Photo, ICO and (on Windows) all TIFF files reuse the same WIC
  adapter; the Rust TIFF decoder is compiled only for other platforms.
- GIF, APNG, and animated WebP frames share one animation-to-JPEG iterator in
  `imageconv.rs`; `input.rs` turns the resulting frames into the normal PDF
  documents and reuses the standard page-tree merger.
- Word, Excel, and PowerPoint share the same isolated temporary-copy and Office
  process boundary in `office.rs`; their COM export scripts are generated
  from one PowerShell template that differs only in the ProgID and export
  call.
- HTML pages are printed to PDF by a headless Chromium-based browser in
  `html.rs` (throwaway profile, bounded by `process::run`); without one,
  `input.rs` falls back to the HTML text extractor shared with EPUB/HTMLZ.
- PDF image decoding and page-image extraction are implemented once in
  `pdf/image.rs` and are shared by optimize, OCR, and convert. Colour spaces
  are resolved through ICC profiles, Indexed palettes and Separation; stream
  filters, predictors and decompression limits come from lopdf. Images are
  extracted as the page shows them: the stored samples (EXIF is ignored, as
  by PDF viewers) are turned by quarter turns to stand upright under their
  drawing matrix and `/Rotate`, and each carries its `placement` matrix. The
  OCR text layer is laid out in the image's pixels through that matrix, and
  an image input is OCR'd through the same path after `input::image_document`
  builds its pages.
- XML is read by one event tokenizer (`xml::events`): the Office/OpenDocument
  text fallback (`office_fallback.rs`) and SVG sizes use it.
- `@list.txt` entries are expanded exactly like arguments (directories, globs,
  page ranges), relative to the list.
- Embedded text fonts (`textpdf.rs`) are written by one `FontWriter` and
  subset by `font_subset.rs`, keeping glyph ids so `CIDToGIDMap /Identity`
  stays valid.
- `rotate` and `resize` share `commands::common::PdfOrJpegEdit`. JPEG
  rotation is lossless: `metadata.rs` composes the EXIF orientation (rotation
  plus mirror) and patches or inserts the tag without touching scan data.
- Single-file edits share one output rule (`commands::common::edited_output`):
  `NAME_<action>`, `-o` file or directory, or `-i` in place.
- Page selection lives in `pdf/mod.rs`: `parse_page_list` keeps order and
  repeats (`extract`, `doc.pdf:3,1,1`), `parse_page_selection` is the set form
  used by editing commands. `select_pages` rebuilds a flat page tree and
  materialises inherited attributes.
- Stamps, page numbers, watermarks and the OCR text layer install their
  resources through `transform::ResourceInstaller`, which edits a shared
  `/Resources` dictionary once instead of copying it to every page. Overlays
  are positioned in the visible (CropBox, rotated) area of the page.
- Batch commands run independent inputs through `commands::common::batch`,
  which uses `parallel::map` unless `--fail-fast` requires sequential,
  lazy processing. Office conversions are serialised by a process-wide lock.
- PDF loading (with or without a password), encryption (`protect`), page
  sizes (`PageSize`: paper, `keep`, `fit`) and bounded text extraction are
  implemented once in `pdf/mod.rs`. Merging, with the outlines and named
  destinations of every source, lives in `pdf/merge.rs`; blank-page detection
  in `pdf/blank.rs`.
- CLI values (page sizes, OCR engine, blend mode, orientation, positions) are
  parsed into their domain types by clap, and the configuration deserialises
  `page_size` and `ocr_engine` through the same parsers, so invalid values
  fail before any work.
- All material writes use `atomic::write_atomic`.

## Efficiency notes

- Pixel codecs (`image`, `zune-jpeg`, `png`, inflate) are built with
  `opt-level = 2`; the rest of the release binary stays size-optimised.
- Strong image reductions first average pixel blocks down to twice the
  target and only then apply the Lanczos filter.
- `split` parses a PDF once and copies, per page, only the objects that page
  reaches (other pages are cut off), writing pages in parallel.
- `optimize` sizes every image for `image_dpi` at the size it is drawn:
  `pdf/image.rs::placements` follows `cm`, `q`/`Q` and nested form XObjects;
  images drawn indirectly fall back to the page size, and an image shown on
  several pages keeps the largest size any of them needs. Decoding, resampling
  and encoding run in parallel; grayscale images stay single-channel
  `DeviceGray` JPEGs, and lossless graphics with up to 256 colours stay
  Flate-encoded.
- `--max-size` bisects the quality ladder instead of walking it.
- OCR of several inputs prepares pages in chunks and recognises them through
  one task queue shared by all files.
- `parallel::map` runs nested calls inline on the worker thread, so a parallel
  stage inside a parallel batch does not multiply threads.
- Upright RGB/grayscale DCT images are extracted from PDFs without
  recompression.
- PDF optimization decodes an image through an immutable borrow and does not
  clone the complete compressed stream.
- RGB JPEG encoding writes the existing RGB buffer directly and avoids
  temporary RGBA and second RGB allocations.
- The executable entry point is also a library entry point, enabling cheap
  integration and differential tests without duplicating startup logic.

## Verification

`cargo test` covers the CLI definition (including clap's consistency checks),
the commands and the PDF transforms. `tools/benchmark-v1.ps1` compares split
timings of two executables over the same source PDF.

Live Groq OCR and Microsoft Office COM calls depend on external credentials and
installed applications. Their internal unit tests remain part of the normal
test suite; live checks must be run only in a configured environment.
