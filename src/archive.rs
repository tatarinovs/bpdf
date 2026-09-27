use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use lopdf::Document;
use zip::ZipArchive;

use crate::formats::{self, Format};
use crate::imageconv::{self, ImageOptions};
use crate::{fileset, parallel, pdf};

/// Loads a `.cbz` archive (or image-containing ZIP), extracts image entries in natural sort order,
/// converts each page into JPEG format, and merges them into a PDF `Document`.
pub fn load_cbz(path: &Path, options: &ImageOptions, page_size: Option<&str>) -> Result<Document> {
    let file = File::open(path)
        .with_context(|| format!("failed to open CBZ archive {}", path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("failed to parse ZIP archive {}", path.display()))?;

    let mut image_names = (0..archive.len())
        .filter_map(|index| archive.name_for_index(index).map(str::to_owned))
        .filter(|name| formats::detect_by_extension(Path::new(name)).is_some_and(Format::is_image))
        .collect::<Vec<_>>();
    if image_names.is_empty() {
        bail!("no images found in CBZ archive {}", path.display());
    }
    fileset::natural_sort_names(&mut image_names);

    // Reading the archive is sequential; decoding and encoding run in parallel.
    let pages = image_names
        .into_iter()
        .map(|name| {
            let mut bytes = Vec::new();
            archive
                .by_name(&name)
                .with_context(|| format!("failed to read archive entry {name}"))?
                .read_to_end(&mut bytes)
                .with_context(|| format!("failed to extract image {name}"))?;
            Ok((name, bytes))
        })
        .collect::<Result<Vec<_>>>()?;
    let documents = parallel::map(&pages, parallel::cpu_jobs(), |(name, bytes)| {
        let jpeg = imageconv::bytes_to_jpeg(bytes, options, page_size)
            .with_context(|| format!("failed to convert image {name} to JPEG"))?;
        pdf::jpeg_document(jpeg, page_size.unwrap_or("A4"))
    })
    .into_iter()
    .collect::<Result<Vec<_>>>()?;
    pdf::merge_documents(documents)
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
            ffmpeg: std::path::PathBuf::from("missing-ffmpeg"),
            jpeg_quality: 75,
            image_dpi: 0,
            ..ImageOptions::default()
        };

        let doc = load_cbz(temp_file.path(), &image_opts, Some("A4")).unwrap();
        assert_eq!(doc.get_pages().len(), 2);
    }
}
