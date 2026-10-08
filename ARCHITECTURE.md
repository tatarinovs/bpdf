# bpdf architecture

Since version 0.3, `bpdf` is organised around explicit command boundaries and
shared services while keeping its executable and configuration compatible.

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
- `commands/pdf_edit.rs`: single-document PDF commands.
- `commands/common.rs`: atomic edit boundary, path identity, output registry,
  and batch summaries.
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
- JPEG XR/HD Photo, ICO and (on Windows) all TIFF files reuse the same WIC
  adapter; the Rust TIFF decoder is compiled only for other platforms.
- GIF, APNG, and animated WebP frames share one animation-to-JPEG iterator in
  `imageconv.rs`; `input.rs` turns the resulting frames into the normal PDF
  documents and reuses the standard page-tree merger.
- Word, Excel, and PowerPoint share the same isolated temporary-copy and Office
  process boundary in `office.rs`; only their COM export scripts differ.
- HTML pages are printed to PDF by a headless Chromium-based browser in
  `html.rs` (throwaway profile, bounded by `process::run`); without one,
  `input.rs` falls back to the HTML text extractor shared with EPUB/HTMLZ.
- PDF image decoding and page-image extraction are implemented once in
  `pdf/image.rs` and are shared by optimize, OCR, and convert. Colour spaces
  are resolved through ICC profiles, Indexed palettes and Separation; stream
  filters, predictors and decompression limits come from lopdf.
- Embedded text fonts (`textpdf.rs`) are written by one `FontWriter` and
  subset by `font_subset.rs`, keeping glyph ids so `CIDToGIDMap /Identity`
  stays valid.
- `rotate` and `resize` share `commands::common::PdfOrJpegEdit`.
- Batch commands run independent inputs through `commands::common::batch`,
  which uses `parallel::map` unless `--fail-fast` requires sequential,
  lazy processing. Office conversions are serialised by a process-wide lock.
- PDF loading and bounded text extraction are implemented once in `pdf/mod.rs`.
- All material writes use `atomic::write_atomic`.

## Efficiency notes

- Pixel codecs (`image`, `zune-jpeg`, `png`, inflate) are built with
  `opt-level = 3`; the rest of the release binary stays size-optimised.
- Strong image reductions first average pixel blocks down to twice the
  target and only then apply the Lanczos filter.
- `split` parses a PDF once and copies, per page, only the objects that page
  reaches (other pages are cut off), writing pages in parallel.
- `optimize` decodes, resamples and encodes images in parallel; grayscale
  images stay single-channel `DeviceGray` JPEGs.
- Upright RGB/grayscale DCT images are extracted from PDFs without
  recompression.
- PDF optimization decodes an image through an immutable borrow and does not
  clone the complete compressed stream.
- RGB JPEG encoding writes the existing RGB buffer directly and avoids
  temporary RGBA and second RGB allocations.
- The executable entry point is also a library entry point, enabling cheap
  integration and differential tests without duplicating startup logic.

## Verification

`tools/compare-v1.ps1` compares the public CLI and deterministic output hashes
for merge, optimize, rotate, resize, metadata, strip, convert, and split.
`tools/benchmark-v1.ps1` compares split timings over the same source PDF.

Live Groq OCR and Microsoft Office COM calls depend on external credentials and
installed applications. Their internal unit tests remain part of the normal
test suite; live checks must be run only in a configured environment.
