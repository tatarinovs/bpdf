use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    Jpeg,
    Png,
    Raster,
    FfmpegRaster,
    WicRaster,
    /// Decoded by the Windows codec when one is installed, otherwise by FFmpeg.
    SystemRaster,
    /// Photoshop: the built-in reader, FFmpeg for what it does not handle.
    Psd,
    CameraRaw,
    Pdf,
    Word,
    Excel,
    PowerPoint,
    Text,
    Html,
    /// Vector image, drawn by the same browser as HTML pages.
    Svg,
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
                | Self::SystemRaster
                | Self::Psd
                | Self::CameraRaw
        )
    }

    pub const fn requires_ffmpeg(self) -> bool {
        matches!(self, Self::FfmpegRaster)
    }

    pub const fn requires_wic(self) -> bool {
        matches!(self, Self::WicRaster)
    }

    pub const fn requires_jpeg_conversion(self) -> bool {
        self.requires_ffmpeg()
            || self.requires_wic()
            || matches!(self, Self::SystemRaster | Self::Psd | Self::CameraRaw)
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
    Rotate,
    Resize,
}

impl InputFormatSet {
    /// True when a directory listing or glob should include `path`: only
    /// files of a known format count, never ones that merely look like text.
    pub fn includes(self, path: &Path) -> bool {
        let format = if has_extension(path, "zip") {
            detect(path)
        } else {
            detect_by_extension(path)
        };
        format.is_some_and(|format| self.accepts(format))
    }

    fn accepts(self, format: Format) -> bool {
        match self {
            Self::Merge => true,
            // CBZ archives are only read by merge.
            Self::Ocr => format.is_image() || format == Format::Pdf,
            Self::Convert => {
                format.is_image() || matches!(format, Format::Pdf | Format::Html | Format::Svg)
            }
            Self::Strip => matches!(format, Format::Jpeg | Format::Png | Format::Pdf),
            Self::Rotate | Self::Resize => matches!(format, Format::Pdf | Format::Jpeg),
        }
    }
}

/// Format of a file by its extension, the entries of a `.zip`, or (for an
/// unknown extension) by looking like text. Results that read the file are
/// remembered for the rest of the run.
pub fn detect(path: &Path) -> Option<Format> {
    let by_content = if has_extension(path, "zip") && !is_fb2_zip(path) {
        detect_zip_content
    } else if let Some(format) = detect_by_extension(path) {
        return Some(format);
    } else {
        |path: &Path| is_text_file(path).then_some(Format::Text)
    };
    static SEEN: OnceLock<Mutex<HashMap<PathBuf, Option<Format>>>> = OnceLock::new();
    let seen = SEEN.get_or_init(Mutex::default);
    if let Some(format) = crate::parallel::lock(seen).get(path) {
        return *format;
    }
    let format = by_content(path);
    crate::parallel::lock(seen).insert(path.to_path_buf(), format);
    format
}

/// Format implied by the file name alone, without touching the file system.
/// Generic `.zip` archives and unknown extensions return `None`.
pub fn detect_by_extension(path: &Path) -> Option<Format> {
    if is_fb2_zip(path) {
        return Some(Format::Fb2);
    }
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" | "jpe" | "jfif" => Some(Format::Jpeg),
        "png" => Some(Format::Png),
        "bmp" | "gif" | "tiff" | "tif" | "webp" | "apng" => Some(Format::Raster),
        "heic" | "heif" | "avif" | "jxl" => Some(Format::SystemRaster),
        "psd" | "psb" => Some(Format::Psd),
        "dds" | "exr" | "hdr" | "qoi" | "tga" | "pcx" | "pnm" | "ppm" | "pgm" | "pbm" | "pam"
        | "sgi" | "xbm" | "jp2" | "j2k" | "j2c" | "jpc" | "jpf" | "jpx" | "jls" | "dpx"
        | "fits" | "fit" | "fts" | "pgx" | "ras" | "sun" | "xwd" | "pix" => {
            Some(Format::FfmpegRaster)
        }
        "jxr" | "wdp" | "hdp" | "ico" => Some(Format::WicRaster),
        "3fr" | "arw" | "bay" | "cr2" | "cr3" | "crw" | "dcr" | "dng" | "erf" | "fff" | "gpr"
        | "iiq" | "k25" | "kdc" | "mef" | "mos" | "mrw" | "nef" | "nrw" | "orf" | "pef" | "raf"
        | "raw" | "rw2" | "rwl" | "sr2" | "srf" | "srw" | "x3f" => Some(Format::CameraRaw),
        "pdf" => Some(Format::Pdf),
        "doc" | "docx" | "rtf" | "odt" => Some(Format::Word),
        "xls" | "xlsx" | "ods" => Some(Format::Excel),
        "ppt" | "pptx" | "pps" | "ppsx" | "odp" => Some(Format::PowerPoint),
        "epub" => Some(Format::Epub),
        "fb2" => Some(Format::Fb2),
        "html" | "htm" | "xhtml" => Some(Format::Html),
        "svg" => Some(Format::Svg),
        "htmlz" => Some(Format::Htmlz),
        "cbz" => Some(Format::Cbz),
        "md" | "txt" | "json" | "jsonc" | "xml" | "yaml" | "yml" | "log" | "ini" | "cfg"
        | "csv" | "tsv" => Some(Format::Text),
        _ => None,
    }
}

/// Case-insensitive extension check.
pub fn has_extension(path: &Path, expected: &str) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
}

pub fn is_tiff(path: &Path) -> bool {
    has_extension(path, "tif") || has_extension(path, "tiff")
}

fn is_fb2_zip(path: &Path) -> bool {
    has_extension(path, "zip")
        && path
            .file_stem()
            .is_some_and(|stem| has_extension(Path::new(stem), "fb2"))
}

/// Inspects ZIP archive entries to detect EPUB, FB2, HTMLZ, or CBZ format.
fn detect_zip_content(path: &Path) -> Option<Format> {
    let Ok(file) = File::open(path) else {
        return None;
    };
    let Ok(archive) = zip::ZipArchive::new(file) else {
        return None;
    };

    let mut has_image = false;
    let mut has_htmlz_index = false;
    let mut has_epub_container = false;
    let mut has_fb2 = false;

    for index in 0..archive.len() {
        let Some(Ok(name)) = archive.name_for_index(index) else {
            continue;
        };
        if crate::archive::is_hidden_entry(&name) {
            continue;
        }
        let name = name.to_ascii_lowercase();
        if name.ends_with("container.xml") {
            has_epub_container = true;
        } else if name == "index.html" {
            has_htmlz_index = true;
        } else if name.ends_with(".fb2") {
            has_fb2 = true;
        } else if detect_by_extension(Path::new(&name)).is_some_and(Format::is_image) {
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

/// Heuristic check to determine if a file is plain text by probing its first
/// 8 KB. An empty file has nothing to show and is not text.
pub fn is_text_file(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut buffer = [0u8; 8192];
    let Ok(bytes_read) = file.read(&mut buffer) else {
        return false;
    };

    if bytes_read == 0 {
        return false;
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
        assert_eq!(detect(Path::new("photo.AVIF")), Some(Format::SystemRaster));
        assert_eq!(detect(Path::new("scan.JP2")), Some(Format::FfmpegRaster));
        assert_eq!(detect(Path::new("photo.JXR")), Some(Format::WicRaster));
        assert_eq!(detect(Path::new("camera.CR3")), Some(Format::CameraRaw));
        assert_eq!(detect(Path::new("document.PDF")), Some(Format::Pdf));
        assert_eq!(detect(Path::new("table.XLSX")), Some(Format::Excel));
        assert_eq!(detect(Path::new("slides.PPTX")), Some(Format::PowerPoint));
        assert_eq!(detect(Path::new("data.JSON")), Some(Format::Text));
        assert_eq!(detect(Path::new("notes.MD")), Some(Format::Text));
        assert_eq!(detect(Path::new("page.HTML")), Some(Format::Html));
        assert_eq!(detect(Path::new("page.htm")), Some(Format::Html));
        assert_eq!(detect(Path::new("page.xhtml")), Some(Format::Html));
    }

    #[test]
    fn operation_sets_share_the_detected_format() {
        assert!(InputFormatSet::Merge.includes(Path::new("document.docx")));
        assert!(!InputFormatSet::Ocr.includes(Path::new("document.docx")));
        assert!(!InputFormatSet::Strip.includes(Path::new("photo.heic")));
        assert!(!InputFormatSet::Strip.includes(Path::new("photo.avif")));
        assert!(InputFormatSet::Convert.includes(Path::new("design.psd")));
        assert!(InputFormatSet::Convert.includes(Path::new("scan.jp2")));
        assert!(InputFormatSet::Convert.includes(Path::new("photo.jxr")));
        assert!(InputFormatSet::Merge.includes(Path::new("slides.pptx")));
        assert!(InputFormatSet::Merge.includes(Path::new("page.html")));
        assert!(!InputFormatSet::Ocr.includes(Path::new("page.html")));
        assert!(InputFormatSet::Convert.includes(Path::new("card.html")));
        assert!(!InputFormatSet::Ocr.includes(Path::new("slides.pptx")));
        assert!(InputFormatSet::Merge.includes(Path::new("photo.nef")));
        assert!(InputFormatSet::Ocr.includes(Path::new("photo.arw")));
        assert!(!InputFormatSet::Strip.includes(Path::new("photo.dng")));
        assert!(!InputFormatSet::Strip.includes(Path::new("photo.webp")));
        assert!(InputFormatSet::Strip.includes(Path::new("photo.png")));
        assert!(InputFormatSet::Strip.includes(Path::new("photo.jpg")));
        assert!(InputFormatSet::Rotate.includes(Path::new("document.pdf")));
        assert!(InputFormatSet::Rotate.includes(Path::new("photo.jpg")));
        assert!(InputFormatSet::Rotate.includes(Path::new("photo.jpeg")));
        assert!(!InputFormatSet::Rotate.includes(Path::new("photo.png")));
        assert!(!InputFormatSet::Rotate.includes(Path::new("photo.webp")));
        assert!(InputFormatSet::Resize.includes(Path::new("document.pdf")));
        assert!(InputFormatSet::Resize.includes(Path::new("photo.jpg")));
        assert!(InputFormatSet::Resize.includes(Path::new("photo.jpeg")));
        assert!(!InputFormatSet::Resize.includes(Path::new("photo.png")));
        assert!(!InputFormatSet::Resize.includes(Path::new("photo.webp")));
        assert!(InputFormatSet::Convert.includes(Path::new("logo.svg")));
        assert!(InputFormatSet::Rotate.includes(Path::new("download.JFIF")));
        assert!(InputFormatSet::Convert.includes(Path::new("photo.jxl")));
        // Listings take known formats only, not everything that looks like text.
        assert!(!InputFormatSet::Merge.includes(Path::new("Makefile")));
    }

    #[test]
    fn external_raster_extensions_share_the_ffmpeg_adapter() {
        for extension in [
            "dds", "exr", "hdr", "qoi", "tga", "pcx", "pnm", "ppm", "pgm", "pbm", "pam", "sgi",
            "xbm", "jp2", "j2k", "j2c", "jpc", "jpf", "jpx", "jls", "dpx", "fits", "fit", "fts",
            "pgx", "ras", "sun", "xwd", "pix",
        ] {
            let path = Path::new("image").with_extension(extension);
            let format = detect(&path).unwrap();
            assert_eq!(format, Format::FfmpegRaster, "{extension}");
            assert!(format.requires_ffmpeg(), "{extension}");
        }
    }

    #[test]
    fn photoshop_uses_the_built_in_reader() {
        for extension in ["psd", "PSB"] {
            let format = detect(&Path::new("image").with_extension(extension)).unwrap();
            assert_eq!(format, Format::Psd, "{extension}");
            assert!(format.requires_jpeg_conversion(), "{extension}");
            assert!(!format.requires_ffmpeg(), "{extension}");
        }
    }

    #[test]
    fn heif_family_prefers_the_system_codec() {
        for extension in ["heic", "HEIF", "avif"] {
            let format = detect(&Path::new("image").with_extension(extension)).unwrap();
            assert_eq!(format, Format::SystemRaster, "{extension}");
            assert!(format.requires_jpeg_conversion(), "{extension}");
            assert!(!format.requires_ffmpeg(), "{extension}");
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
    fn camera_raw_extensions_require_jpeg_conversion() {
        for extension in [
            "3fr", "arw", "bay", "cr2", "cr3", "crw", "dcr", "dng", "erf", "fff", "gpr", "iiq",
            "k25", "kdc", "mef", "mos", "mrw", "nef", "nrw", "orf", "pef", "raf", "raw", "rw2",
            "rwl", "sr2", "srf", "srw", "x3f",
        ] {
            let path = Path::new("camera").with_extension(extension);
            let format = detect(&path).unwrap();
            assert_eq!(format, Format::CameraRaw, "{extension}");
            assert!(format.requires_jpeg_conversion(), "{extension}");
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
        std::fs::write(&binary_path, [0x89, 0x50, 0x4E, 0x47, 0x00, 0x00, 0x00]).unwrap();
        assert_eq!(detect(&binary_path), None);
        assert!(!is_text_file(&binary_path));
    }
}
