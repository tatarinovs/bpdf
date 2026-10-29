use std::path::Path;

use anyhow::{Context, Result, bail};
use lopdf::Document;

use crate::fileset::InputSpec;
use crate::formats::{self, Format};
use crate::html::{self, HtmlOptions};
use crate::imageconv::{self, ImageOptions};
use crate::office::{self, OfficeOptions};
use crate::pdf::{self, PageSize};
use crate::textpdf::{self, TextOptions};

/// Dependencies needed to turn every supported source type into the common
/// in-memory PDF representation. New input adapters only need to be added here;
/// merge and all post-processing stages remain unchanged.
#[derive(Clone, Debug)]
pub struct LoadOptions {
    pub image: ImageOptions,
    pub office: OfficeOptions,
    pub html: HtmlOptions,
    pub text: TextOptions,
}

pub fn load(spec: &InputSpec, options: &LoadOptions) -> Result<Document> {
    match formats::detect(&spec.path) {
        Some(format) if format.is_image() => {
            spec.reject_pages()?;
            image_document(&spec.path, &options.image, options.text.page_size)
        }
        Some(Format::Pdf) => {
            let mut document = pdf::load(&spec.path)?;
            if let Some(pages) = &spec.pages {
                pdf::select_pages(&mut document, pages)?;
            }
            Ok(document)
        }
        Some(Format::Cbz) => {
            spec.reject_pages()?;
            crate::archive::load_cbz(&spec.path, &options.image, options.text.page_size)
        }
        Some(format) if format.is_office() => {
            spec.reject_pages()?;
            match office::convert_to_pdf(&spec.path, &options.office) {
                Ok(bytes) => Document::load_mem(&bytes).with_context(|| {
                    format!("Office output for {} is invalid", spec.path.display())
                }),
                Err(com_err) => {
                    if let Ok(text) = crate::office_fallback::extract_text(&spec.path) {
                        crate::output::warn(format!(
                            "COM automation unavailable for {}, using Pure-Rust text fallback: {com_err}",
                            spec.path.display()
                        ));
                        textpdf::render(&text, &options.text)
                    } else {
                        Err(com_err)
                    }
                }
            }
        }
        Some(Format::Html) => {
            spec.reject_pages()?;
            load_html(&spec.path, &options.html, &options.text)
        }
        Some(Format::Svg) => {
            spec.reject_pages()?;
            html::svg_to_pdf(&spec.path, &options.html)
                .with_context(|| format!("cannot draw {}", spec.path.display()))
        }
        Some(Format::Text) => {
            spec.reject_pages()?;
            textpdf::render(&crate::encoding::read_text(&spec.path)?, &options.text)
        }
        Some(format) if format.is_ebook() => {
            spec.reject_pages()?;
            let text = crate::ebook::load(&spec.path, format)?;
            textpdf::render(&text, &options.text)
        }
        _ => bail!("unsupported merge format: {}", spec.path.display()),
    }
}

/// One page per frame of an image file (TIFF pages, animation frames).
pub fn image_document(
    path: &Path,
    options: &ImageOptions,
    page_size: PageSize,
) -> Result<Document> {
    let jpegs = imageconv::to_jpegs_for_pdf(path, options, Some(page_size))?;
    // A fitted page uses the resolution of the source, which a re-encoded
    // JPEG no longer records.
    let dpi = if page_size == PageSize::Fit {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| crate::metadata::image_dpi(&bytes))
    } else {
        None
    };
    let documents = jpegs
        .into_iter()
        .map(|jpeg| pdf::jpeg_document_at(jpeg, page_size, dpi))
        .collect::<Result<Vec<_>>>()?;
    pdf::merge_documents(documents)
}

/// Browser layout of an HTML page, or its text when no browser can render it.
fn load_html(path: &Path, html: &HtmlOptions, text: &TextOptions) -> Result<Document> {
    let (width, height) = text.page_size.sheet();
    match html::convert_to_pdf(path, html, height / width) {
        Ok(document) => Ok(document),
        Err(browser_err) => {
            crate::output::warn(format!(
                "Browser rendering unavailable for {}, using text extraction: {browser_err:#}",
                path.display()
            ));
            let source = crate::encoding::read_text(path)?;
            textpdf::render(&crate::ebook::convert_html_to_text(&source), text)
        }
    }
}
