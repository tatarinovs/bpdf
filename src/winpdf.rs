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

    let _apartment = crate::com::ComApartment::initialize()?;
    let open = |path: &Path| -> Result<PdfDocument> {
        // StorageFile requires an absolute path.
        let absolute = std::path::absolute(path)
            .with_context(|| format!("failed to resolve {}", path.display()))?;
        let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(absolute.as_os_str()))?
            .join()
            .context("failed to get storage file from path")?;
        // The empty password opens files encrypted only for restrictions.
        PdfDocument::LoadFromFileWithPasswordAsync(&file, &HSTRING::new())?
            .join()
            .context("failed to load PDF document via Windows API")
    };
    // The Windows renderer cannot read AES-256 (bpdf protect): it gets a
    // decrypted copy of a file bpdf itself can open.
    let mut _decrypted = None;
    let pdf = match open(input) {
        Ok(pdf) => pdf,
        Err(error) => {
            let Ok(mut document) = crate::pdf::load(input) else {
                return Err(error);
            };
            if document.encryption_state.take().is_none() {
                return Err(error);
            }
            // A closed file: the Windows API cannot share an open temporary one.
            let copy = tempfile::Builder::new()
                .suffix(".pdf")
                .tempfile()?
                .into_temp_path();
            std::fs::write(&copy, crate::pdf::save_to_bytes(&mut document)?)?;
            let pdf = open(&copy)?;
            _decrypted = Some(copy);
            pdf
        }
    };

    let count = pdf.PageCount()? as usize;
    if let Some(page) = pages.iter().find(|page| **page == 0 || **page > count) {
        bail!("page {page} is outside 1-{count}");
    }
    let options = PdfPageRenderOptions::new()?;
    options.SetBitmapEncoderId(BitmapEncoder::JpegEncoderId()?)?;

    let mut rendered = Vec::with_capacity(pages.len());
    for &number in pages {
        let page = pdf.GetPage((number - 1) as u32)?;
        // `Size` is in device-independent pixels (1/96 inch).
        let dips = page.Size()?;
        let to_points = |dips: f32| f64::from(dips) * 72.0 / 96.0;
        if let Some((width, height)) = size(to_points(dips.Width), to_points(dips.Height)) {
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
