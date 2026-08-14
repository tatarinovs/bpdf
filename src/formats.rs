use std::fs::File;
use std::io::Read;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    Jpeg,
    Png,
    Raster,
    FfmpegRaster,
    WicRaster,
    CameraRaw,
    Pdf,
    Word,
    Excel,
    PowerPoint,
    Text,
    Epub,
    Fb2,
    Htmlz,
    Cbz,
}

impl Format {
    pub const fn is_image(self) -> bool {
        matches!(
            self,
            Self::Jpeg
                | Self::Png
                | Self::Raster
                | Self::FfmpegRaster
                | Self::WicRaster
                | Self::CameraRaw
        )
    }

    pub const fn requires_ffmpeg(self) -> bool {
        matches!(self, Self::FfmpegRaster)
    }

    pub const fn requires_wic(self) -> bool {
        matches!(self, Self::WicRaster | Self::CameraRaw)
    }

    pub const fn requires_jpeg_conversion(self) -> bool {
        self.requires_ffmpeg() || self.requires_wic()
    }

    pub const fn is_office(self) -> bool {
        matches!(self, Self::Word | Self::Excel | Self::PowerPoint)
    }

    pub const fn is_ebook(self) -> bool {
        matches!(self, Self::Epub | Self::Fb2 | Self::Htmlz)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum InputFormatSet {
    Merge,
    Ocr,
    Strip,
    Convert,
}

impl InputFormatSet {
    pub fn supports(self, path: &Path) -> bool {
        detect(path).is_some_and(|format| match self {
            Self::Merge => true,
            Self::Ocr | Self::Convert => format.is_image() || format == Format::Pdf || format == Format::Cbz,
            Self::Strip => matches!(
                format,
                Format::Jpeg
                    | Format::Png
                    | Format::FfmpegRaster
                    | Format::WicRaster
                    | Format::CameraRaw
                    | Format::Pdf
            ),
        })
    }
}

pub fn detect(path: &Path) -> Option<Format> {
    let path_str = path.to_string_lossy();
    if path_str.to_ascii_lowercase().ends_with(".fb2.zip") {
        return Some(Format::Fb2);
    }

    if let Some(extension) = path.extension().and_then(|ext| ext.to_str()) {
        match extension.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" => return Some(Format::Jpeg),
            "png" => return Some(Format::Png),
            "bmp" | "gif" | "tiff" | "tif" | "webp" | "apng" => return Some(Format::Raster),
            "heic" | "heif" | "avif" | "psd" | "dds" | "exr" | "hdr" | "qoi" | "tga" | "pcx"
            | "pnm" | "ppm" | "pgm" | "pbm" | "pam" | "sgi" | "xbm" | "jp2" | "j2k" | "j2c" | "jpc"
            | "jpf" | "jpx" | "jls" | "dpx" | "fits" | "fit" | "fts" | "pgx" | "ras" | "sun"
            | "xwd" | "pix" => return Some(Format::FfmpegRaster),
            "jxr" | "wdp" | "hdp" | "ico" => return Some(Format::WicRaster),
            "3fr" | "arw" | "bay" | "cr2" | "cr3" | "crw" | "dcr" | "dng" | "erf" | "fff" | "gpr"
            | "iiq" | "k25" | "kdc" | "mef" | "mos" | "mrw" | "nef" | "nrw" | "orf" | "pef" | "raf"
            | "raw" | "rw2" | "rwl" | "sr2" | "srf" | "srw" | "x3f" => return Some(Format::CameraRaw),
            "pdf" => return Some(Format::Pdf),
            "doc" | "docx" | "rtf" | "odt" => return Some(Format::Word),
            "xls" | "xlsx" | "ods" => return Some(Format::Excel),
            "ppt" | "pptx" | "pps" | "ppsx" | "odp" => return Some(Format::PowerPoint),
            "epub" => return Some(Format::Epub),
            "fb2" => return Some(Format::Fb2),
            "htmlz" => return Some(Format::Htmlz),
            "cbz" => return Some(Format::Cbz),
            "zip" => return detect_zip_content(path),
            "md" | "txt" | "json" | "jsonc" | "xml" | "yaml" | "yml" | "log" | "ini" | "cfg"
            | "csv" | "tsv" => return Some(Format::Text),
            _ => {}
        }
    }

    if is_text_file(path) {
        Some(Format::Text)
    } else {
        None
    }
}

/// Inspects ZIP archive entries to detect EPUB, FB2, HTMLZ, or CBZ format.
fn detect_zip_content(path: &Path) -> Option<Format> {
    let Ok(file) = File::open(path) else { return None; };
    let Ok(mut archive) = zip::ZipArchive::new(file) else { return None; };

    let mut has_image = false;
    let mut has_htmlz_index = false;
    let mut has_epub_container = false;
    let mut has_fb2 = false;

    for i in 0..archive.len() {
        let Ok(entry) = archive.by_index(i) else { continue; };
        let name = entry.name().to_ascii_lowercase();
        if name.ends_with("container.xml") {
            has_epub_container = true;
        } else if name == "index.html" {
            has_htmlz_index = true;
        } else if name.ends_with(".fb2") {
            has_fb2 = true;
        } else if detect(Path::new(&name)).is_some_and(|f| f.is_image()) {
            has_image = true;
        }
    }

    if has_epub_container {
        Some(Format::Epub)
    } else if has_fb2 {
        Some(Format::Fb2)
    } else if has_htmlz_index {
        Some(Format::Htmlz)
    } else if has_image {
        Some(Format::Cbz)
    } else {
        None
    }
}

/// Heuristic check to determine if a file is plain text by probing its first 8 KB.
pub fn is_text_file(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut buffer = [0u8; 8192];
    let Ok(bytes_read) = file.read(&mut buffer) else {
        return false;
    };

    if bytes_read == 0 {
        return true;
    }

    let sample = &buffer[..bytes_read];

    // Binary files almost universally contain null bytes.
    if sample.contains(&0) {
        return false;
    }

    // Valid UTF-8 text.
    if std::str::from_utf8(sample).is_ok() {
        return true;
    }

    // For single-byte text encodings (e.g. CP1251/ANSI), verify control character ratio.
    let control_chars = sample
        .iter()
        .filter(|&&b| b < 32 && b != b'\t' && b != b'\n' && b != b'\r')
        .count();

    (control_chars as f64 / bytes_read as f64) < 0.01
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_is_case_insensitive() {
        assert_eq!(detect(Path::new("scan.WeBp")), Some(Format::Raster));
        assert_eq!(detect(Path::new("animation.APNG")), Some(Format::Raster));
        assert_eq!(detect(Path::new("photo.AVIF")), Some(Format::FfmpegRaster));
        assert_eq!(detect(Path::new("scan.JP2")), Some(Format::FfmpegRaster));
        assert_eq!(detect(Path::new("photo.JXR")), Some(Format::WicRaster));
        assert_eq!(detect(Path::new("camera.CR3")), Some(Format::CameraRaw));
        assert_eq!(detect(Path::new("document.PDF")), Some(Format::Pdf));
        assert_eq!(detect(Path::new("table.XLSX")), Some(Format::Excel));
        assert_eq!(detect(Path::new("slides.PPTX")), Some(Format::PowerPoint));
        assert_eq!(detect(Path::new("data.JSON")), Some(Format::Text));
        assert_eq!(detect(Path::new("notes.MD")), Some(Format::Text));
    }

    #[test]
    fn operation_sets_share_the_detected_format() {
        assert!(InputFormatSet::Merge.supports(Path::new("document.docx")));
        assert!(!InputFormatSet::Ocr.supports(Path::new("document.docx")));
        assert!(InputFormatSet::Strip.supports(Path::new("photo.heic")));
        assert!(InputFormatSet::Strip.supports(Path::new("photo.avif")));
        assert!(InputFormatSet::Convert.supports(Path::new("design.psd")));
        assert!(InputFormatSet::Convert.supports(Path::new("scan.jp2")));
        assert!(InputFormatSet::Convert.supports(Path::new("photo.jxr")));
        assert!(InputFormatSet::Merge.supports(Path::new("slides.pptx")));
        assert!(!InputFormatSet::Ocr.supports(Path::new("slides.pptx")));
        assert!(InputFormatSet::Merge.supports(Path::new("photo.nef")));
        assert!(InputFormatSet::Ocr.supports(Path::new("photo.arw")));
        assert!(InputFormatSet::Strip.supports(Path::new("photo.dng")));
        assert!(!InputFormatSet::Strip.supports(Path::new("photo.webp")));
    }

    #[test]
    fn external_raster_extensions_share_the_ffmpeg_adapter() {
        for extension in [
            "heic", "heif", "avif", "psd", "dds", "exr", "hdr", "qoi", "tga", "pcx", "pnm", "ppm",
            "pgm", "pbm", "pam", "sgi", "xbm", "jp2", "j2k", "j2c", "jpc", "jpf", "jpx", "jls",
            "dpx", "fits", "fit", "fts", "pgx", "ras", "sun", "xwd", "pix",
        ] {
            let path = Path::new("image").with_extension(extension);
            let format = detect(&path).unwrap();
            assert_eq!(format, Format::FfmpegRaster, "{extension}");
            assert!(format.requires_ffmpeg(), "{extension}");
        }
    }

    #[test]
    fn wic_raster_extensions_share_the_wic_adapter() {
        for extension in ["jxr", "wdp", "hdp", "ico"] {
            let path = Path::new("image").with_extension(extension);
            let format = detect(&path).unwrap();
            assert_eq!(format, Format::WicRaster, "{extension}");
            assert!(format.requires_wic(), "{extension}");
        }
    }

    #[test]
    fn office_and_text_extensions_share_existing_adapters() {
        for extension in ["doc", "docx", "rtf", "odt"] {
            assert_eq!(
                detect(&Path::new("document").with_extension(extension)),
                Some(Format::Word)
            );
        }
        for extension in ["xls", "xlsx", "ods"] {
            assert_eq!(
                detect(&Path::new("sheet").with_extension(extension)),
                Some(Format::Excel)
            );
        }
        for extension in ["ppt", "pptx", "pps", "ppsx", "odp"] {
            assert_eq!(
                detect(&Path::new("slides").with_extension(extension)),
                Some(Format::PowerPoint)
            );
        }
        for extension in [
            "md", "txt", "json", "jsonc", "xml", "yaml", "yml", "log", "ini", "cfg", "csv", "tsv",
        ] {
            assert_eq!(
                detect(&Path::new("text").with_extension(extension)),
                Some(Format::Text)
            );
        }
    }

    #[test]
    fn camera_raw_extensions_share_the_wic_adapter() {
        for extension in [
            "3fr", "arw", "bay", "cr2", "cr3", "crw", "dcr", "dng", "erf", "fff", "gpr", "iiq",
            "k25", "kdc", "mef", "mos", "mrw", "nef", "nrw", "orf", "pef", "raf", "raw", "rw2",
            "rwl", "sr2", "srf", "srw", "x3f",
        ] {
            let path = Path::new("camera").with_extension(extension);
            let format = detect(&path).unwrap();
            assert_eq!(format, Format::CameraRaw, "{extension}");
            assert!(format.requires_wic(), "{extension}");
        }
    }

    #[test]
    fn detects_unknown_text_and_code_files_by_content() {
        let temp_dir = tempfile::tempdir().unwrap();

        let code_path = temp_dir.path().join("main.rs");
        std::fs::write(&code_path, "fn main() { println!(\"Hello!\"); }").unwrap();
        assert_eq!(detect(&code_path), Some(Format::Text));

        let env_path = temp_dir.path().join(".env");
        std::fs::write(&env_path, "PORT=8080\nDEBUG=true").unwrap();
        assert_eq!(detect(&env_path), Some(Format::Text));

        let no_ext_path = temp_dir.path().join("LICENSE");
        std::fs::write(&no_ext_path, "MIT License\nCopyright (c) 2026").unwrap();
        assert_eq!(detect(&no_ext_path), Some(Format::Text));
    }

    #[test]
    fn rejects_binary_files_with_null_bytes() {
        let temp_dir = tempfile::tempdir().unwrap();
        let binary_path = temp_dir.path().join("data.bin");
        std::fs::write(&binary_path, &[0x89, 0x50, 0x4E, 0x47, 0x00, 0x00, 0x00]).unwrap();
        assert_eq!(detect(&binary_path), None);
        assert!(!is_text_file(&binary_path));
    }
}
