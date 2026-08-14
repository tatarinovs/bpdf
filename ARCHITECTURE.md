# bpdf architecture

Since version 0.3, `bpdf` is organised around explicit command boundaries and
shared services while keeping its executable and configuration compatible.

## Dependency direction

```text
main.rs
  -> lib.rs (process entry point)
    -> commands/* (use cases and output policy)
      -> pdf, imageconv, ocr, office, textpdf (domain services/adapters)
        -> atomic, process, encoding, hash (infrastructure)
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
- On Windows, camera RAW files use Windows Imaging Component (WIC) and the
  Microsoft Raw Image Extension, then return to that same sizing/encoding path.
- JPEG XR/HD Photo and ICO reuse the same WIC adapter. Multi-page TIFF also
  iterates WIC frames instead of introducing a separate TIFF implementation.
- GIF, APNG, and animated WebP frames share one animation-to-JPEG iterator in
  `imageconv.rs`; `input.rs` turns the resulting frames into the normal PDF
  documents and reuses the standard page-tree merger.
- Word, Excel, and PowerPoint share the same isolated temporary-copy and Office
  process boundary in `office.rs`; only their COM export scripts differ.
- PDF image decoding is implemented once in `pdf/image.rs` and is shared by
  optimize, OCR, and image extraction.
- PDF loading and bounded text extraction are implemented once in `pdf/mod.rs`.
- All material writes use `atomic::write_atomic`.

## Efficiency changes from v1

- `split` parses a PDF once and clones the owned object graph for each page,
  instead of parsing the same bytes once per output page.
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
