use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    Jpeg,
    Png,
    Raster,
    Heic,
    Pdf,
    Word,
    Excel,
    Text,
}

impl Format {
    pub const fn is_image(self) -> bool {
        matches!(self, Self::Jpeg | Self::Png | Self::Raster | Self::Heic)
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
                Format::Jpeg | Format::Png | Format::Heic | Format::Pdf
            ),
        })
    }
}

pub fn detect(path: &Path) -> Option<Format> {
    let extension = path.extension()?.to_str()?;
    Some(match extension.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Format::Jpeg,
        "png" => Format::Png,
        "bmp" | "gif" | "tiff" | "tif" | "webp" => Format::Raster,
        "heic" | "heif" => Format::Heic,
        "pdf" => Format::Pdf,
        "doc" | "docx" => Format::Word,
        "xls" | "xlsx" => Format::Excel,
        "md" | "txt" => Format::Text,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_is_case_insensitive() {
        assert_eq!(detect(Path::new("scan.WeBp")), Some(Format::Raster));
        assert_eq!(detect(Path::new("document.PDF")), Some(Format::Pdf));
        assert_eq!(detect(Path::new("table.XLSX")), Some(Format::Excel));
        assert_eq!(detect(Path::new("notes.MD")), Some(Format::Text));
    }

    #[test]
    fn operation_sets_share_the_detected_format() {
        assert!(InputFormatSet::Merge.supports(Path::new("document.docx")));
        assert!(!InputFormatSet::Ocr.supports(Path::new("document.docx")));
        assert!(InputFormatSet::Strip.supports(Path::new("photo.heic")));
        assert!(!InputFormatSet::Strip.supports(Path::new("photo.webp")));
    }
}
