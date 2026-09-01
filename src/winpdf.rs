use crate::imageconv::ImageOptions;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

#[cfg(windows)]
pub fn render_pdf_to_jpegs(
    input: &Path,
    output_dir: Option<&Path>,
    image_options: &ImageOptions,
) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
    use windows::Graphics::Imaging::BitmapEncoder;
    use windows::Storage::StorageFile;
    use windows::Storage::Streams::{DataReader, InMemoryRandomAccessStream};
    use windows::core::HSTRING;

    let path_hstring = HSTRING::from(input.as_os_str());

    (|| -> Result<Vec<(PathBuf, Vec<u8>)>> {
        let _apartment = crate::com::ComApartment::initialize()?;
        let file = StorageFile::GetFileFromPathAsync(&path_hstring)?
            .join()
            .context("failed to get storage file from path")?;

        let pdf = PdfDocument::LoadFromFileAsync(&file)?
            .join()
            .context("failed to load PDF document via Windows API")?;

        let count = pdf.PageCount()?;
        if count == 0 {
            bail!("PDF has no pages");
        }

        let file_stem = input
            .file_stem()
            .with_context(|| format!("{} has no file stem", input.display()))?
            .to_string_lossy();

        let mut outputs = Vec::new();

        for i in 0..count {
            let page = pdf.GetPage(i)?;
            let stream = InMemoryRandomAccessStream::new()?;
            let options = PdfPageRenderOptions::new()?;
            options.SetBitmapEncoderId(BitmapEncoder::JpegEncoderId()?)?;

            if image_options.image_dpi > 0 {
                let size = page.Size()?;
                let dpi = image_options.image_dpi as f64;
                let width_px = (size.Width as f64 * (dpi / 72.0)).round() as u32;
                let height_px = (size.Height as f64 * (dpi / 72.0)).round() as u32;
                options.SetDestinationWidth(width_px.max(1))?;
                options.SetDestinationHeight(height_px.max(1))?;
            }

            page.RenderWithOptionsToStreamAsync(&stream, &options)?
                .join()
                .with_context(|| format!("failed to render page {}", i + 1))?;

            let size = stream.Size()? as usize;
            let reader = DataReader::CreateDataReader(&stream)?;
            reader.LoadAsync(size as u32)?.join()?;

            let mut bytes = vec![0u8; size];
            reader.ReadBytes(&mut bytes)?;
            reader.Close()?;
            stream.Close()?;

            let suffix = format!("page_{}.jpg", i + 1);
            let output_path = output_dir
                .map(|dir| dir.join(file_stem.as_ref()).with_extension(&suffix))
                .unwrap_or_else(|| input.with_extension(&suffix));

            outputs.push((output_path, bytes));
        }

        Ok(outputs)
    })()
}

#[cfg(not(windows))]
pub fn render_pdf_to_jpegs(
    _input: &Path,
    _output_dir: Option<&Path>,
) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    bail!("PDF rendering is only supported on Windows 8.1+");
}
