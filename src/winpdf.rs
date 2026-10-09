use std::path::Path;

use anyhow::Result;

use crate::imageconv::ImageOptions;

/// Render the given 1-based pages to JPEG with the Windows PDF renderer.
pub fn render_pdf_to_jpegs(
    input: &Path,
    pages: &[usize],
    image_options: &ImageOptions,
) -> Result<Vec<Vec<u8>>> {
    let scale = f64::from(image_options.image_dpi) / 72.0;
    render_pages(input, pages, |width, height| {
        (image_options.image_dpi > 0).then_some((width * scale, height * scale))
    })
}

/// Render one 1-based page to a JPEG whose longer edge is `long_edge` pixels.
pub fn render_pdf_preview(input: &Path, page: usize, long_edge: u32) -> Result<Vec<u8>> {
    let mut rendered = render_pages(input, &[page], |width, height| {
        let scale = f64::from(long_edge) / width.max(height);
        Some((width * scale, height * scale))
    })?;
    Ok(rendered.remove(0))
}

/// `size` maps the displayed page size in points to the bitmap size, or
/// `None` for the renderer's default.
#[cfg(windows)]
fn render_pages(
    input: &Path,
    pages: &[usize],
    size: impl Fn(f64, f64) -> Option<(f64, f64)>,
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
        let points = page.Size()?;
        if let Some((width, height)) = size(f64::from(points.Width), f64::from(points.Height)) {
            options.SetDestinationWidth((width.round() as u32).max(1))?;
            options.SetDestinationHeight((height.round() as u32).max(1))?;
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
fn render_pages(
    _input: &Path,
    _pages: &[usize],
    _size: impl Fn(f64, f64) -> Option<(f64, f64)>,
) -> Result<Vec<Vec<u8>>> {
    anyhow::bail!("PDF rendering is only supported on Windows 8.1+")
}
