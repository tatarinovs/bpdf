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
            Self::Ocr | Self::Convert => format.is_image() || format == Format::Pdf,
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
    let extension = path.extension()?.to_str()?;
    Some(match extension.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Format::Jpeg,
        "png" => Format::Png,
        "bmp" | "gif" | "tiff" | "tif" | "webp" | "apng" => Format::Raster,
        "heic" | "heif" | "avif" | "psd" | "dds" | "exr" | "hdr" | "qoi" | "tga" | "pcx"
        | "pnm" | "ppm" | "pgm" | "pbm" | "pam" | "sgi" | "xbm" | "jp2" | "j2k" | "j2c" | "jpc"
        | "jpf" | "jpx" | "jls" | "dpx" | "fits" | "fit" | "fts" | "pgx" | "ras" | "sun"
        | "xwd" | "pix" => Format::FfmpegRaster,
        "jxr" | "wdp" | "hdp" | "ico" => Format::WicRaster,
        "3fr" | "arw" | "bay" | "cr2" | "cr3" | "crw" | "dcr" | "dng" | "erf" | "fff" | "gpr"
        | "iiq" | "k25" | "kdc" | "mef" | "mos" | "mrw" | "nef" | "nrw" | "orf" | "pef" | "raf"
        | "raw" | "rw2" | "rwl" | "sr2" | "srf" | "srw" | "x3f" => Format::CameraRaw,
        "pdf" => Format::Pdf,
        "doc" | "docx" | "rtf" | "odt" => Format::Word,
        "xls" | "xlsx" | "ods" => Format::Excel,
        "ppt" | "pptx" | "pps" | "ppsx" | "odp" => Format::PowerPoint,
        "md" | "txt" | "json" | "jsonc" | "xml" | "yaml" | "yml" | "log" | "ini" | "cfg"
        | "csv" | "tsv" => Format::Text,
        _ => return None,
    })
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
}
