use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use lopdf::Document;
use zip::ZipArchive;

use crate::fileset;
use crate::imageconv::{self, ImageOptions};
use crate::pdf;

/// Loads a `.cbz` archive (or image-containing ZIP), extracts image entries in natural sort order,
/// converts each page into JPEG format, and merges them into a PDF `Document`.
pub fn load_cbz(path: &Path, options: &ImageOptions, page_size: Option<&str>) -> Result<Document> {
    let file = File::open(path)
        .with_context(|| format!("failed to open CBZ archive {}", path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("failed to parse ZIP archive {}", path.display()))?;

    let mut image_names = Vec::new();
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .with_context(|| format!("failed to read ZIP entry index {i}"))?;
        let name = entry.name();
        if crate::formats::detect(Path::new(name)).is_some_and(|f| f.is_image()) {
            image_names.push(name.to_owned());
        }
    }

    if image_names.is_empty() {
        bail!("no images found in CBZ archive {}", path.display());
    }

    image_names.sort_by(|a, b| fileset::natural_compare(a, b));

    let mut documents = Vec::new();
    for name in image_names {
        let mut entry = archive
            .by_name(&name)
            .with_context(|| format!("failed to read archive entry {name}"))?;
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .with_context(|| format!("failed to extract image {name}"))?;

        let jpeg = imageconv::bytes_to_jpeg(&bytes, options, page_size)
            .with_context(|| format!("failed to convert image {name} to JPEG"))?;
        let doc = pdf::jpeg_document(jpeg, page_size.unwrap_or("A4"))?;
        documents.push(doc);
    }

    if documents.len() == 1 {
        documents.into_iter().next().ok_or_else(|| anyhow::anyhow!("no documents"))
    } else {
        pdf::merge_documents(documents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn loads_cbz_archive_and_builds_pdf() {
        use image::{ImageBuffer, Rgb};

        use zip::write::SimpleFileOptions;

        let temp_file = NamedTempFile::new().unwrap();
        let file = File::create(temp_file.path()).unwrap();
        let mut zip = zip::ZipWriter::new(file);

        let img: ImageBuffer<Rgb<u8>, _> = ImageBuffer::from_pixel(10, 10, Rgb([255, 0, 0]));
        let mut img_bytes = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut img_bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
        let zip_opts = SimpleFileOptions::default();

        zip.start_file("page_02.png", zip_opts).unwrap();
        zip.write_all(&img_bytes).unwrap();
        zip.start_file("page_01.png", zip_opts).unwrap();
        zip.write_all(&img_bytes).unwrap();
        zip.finish().unwrap();

        let image_opts = ImageOptions {
            keep_icc: false,
            ffmpeg: std::path::PathBuf::from("missing-ffmpeg"),
            jpeg_quality: 75,
            image_dpi: 0,
            force_reencode: false,
            long_edge: None,
            short_edge: None,
            orient: None,
            rotation_degrees: None,
            raw_develop: false,
        };

        let doc = load_cbz(temp_file.path(), &image_opts, Some("A4")).unwrap();
        assert_eq!(doc.get_pages().len(), 2);
    }
}
