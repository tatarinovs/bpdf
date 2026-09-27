use std::path::Path;

use anyhow::Result;

use crate::imageconv::ImageOptions;

/// Render the given 1-based pages to JPEG with the Windows PDF renderer.
#[cfg(windows)]
pub fn render_pdf_to_jpegs(
    input: &Path,
    pages: &[usize],
    image_options: &ImageOptions,
) -> Result<Vec<Vec<u8>>> {
    use anyhow::{Context, bail};
    use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
    use windows::Graphics::Imaging::BitmapEncoder;
    use windows::Storage::StorageFile;
    use windows::Storage::Streams::{DataReader, InMemoryRandomAccessStream};
    use windows::core::HSTRING;

    // StorageFile requires an absolute path.
    let absolute = std::path::absolute(input)
        .with_context(|| format!("failed to resolve {}", input.display()))?;
    let _apartment = crate::com::ComApartment::initialize()?;
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(absolute.as_os_str()))?
        .join()
        .context("failed to get storage file from path")?;
    let pdf = PdfDocument::LoadFromFileAsync(&file)?
        .join()
        .context("failed to load PDF document via Windows API")?;

    let count = pdf.PageCount()? as usize;
    if let Some(page) = pages.iter().find(|page| **page == 0 || **page > count) {
        bail!("page {page} is outside 1-{count}");
    }
    let options = PdfPageRenderOptions::new()?;
    options.SetBitmapEncoderId(BitmapEncoder::JpegEncoderId()?)?;

    let mut rendered = Vec::with_capacity(pages.len());
    for &number in pages {
        let page = pdf.GetPage((number - 1) as u32)?;
        if image_options.image_dpi > 0 {
            let size = page.Size()?;
            let scale = f64::from(image_options.image_dpi) / 72.0;
            options.SetDestinationWidth(((f64::from(size.Width) * scale).round() as u32).max(1))?;
            options
                .SetDestinationHeight(((f64::from(size.Height) * scale).round() as u32).max(1))?;
        }

        let stream = InMemoryRandomAccessStream::new()?;
        page.RenderWithOptionsToStreamAsync(&stream, &options)?
            .join()
            .with_context(|| format!("failed to render page {number}"))?;

        let size = u32::try_from(stream.Size()?).context("rendered page is too large")?;
        let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
        reader.LoadAsync(size)?.join()?;
        let mut bytes = vec![0u8; size as usize];
        reader.ReadBytes(&mut bytes)?;
        rendered.push(bytes);
    }
    Ok(rendered)
}

#[cfg(not(windows))]
pub fn render_pdf_to_jpegs(
    _input: &Path,
    _pages: &[usize],
    _image_options: &ImageOptions,
) -> Result<Vec<Vec<u8>>> {
    anyhow::bail!("PDF rendering is only supported on Windows 8.1+")
}
