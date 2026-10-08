use std::path::Path;

use anyhow::{Context, Result, bail};
use lopdf::Document;

use crate::fileset::InputSpec;
use crate::formats::{self, Format};
use crate::html::{self, HtmlOptions};
use crate::imageconv::{self, ImageOptions};
use crate::office::{self, OfficeOptions};
use crate::pdf;
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
            reject_pages(spec)?;
            let jpegs = imageconv::to_jpegs_for_pdf(
                &spec.path,
                &options.image,
                Some(&options.text.page_size),
            )?;
            let documents = jpegs
                .into_iter()
                .map(|jpeg| pdf::jpeg_document(jpeg, &options.text.page_size))
                .collect::<Result<Vec<_>>>()?;
            pdf::merge_documents(documents)
        }
        Some(Format::Pdf) => {
            let mut document = pdf::load(&spec.path)?;
            if let Some(pages) = &spec.pages {
                pdf::select_pages(&mut document, pages)?;
            }
            Ok(document)
        }
        Some(Format::Cbz) => {
            reject_pages(spec)?;
            crate::archive::load_cbz(&spec.path, &options.image, Some(&options.text.page_size))
        }
        Some(format) if format.is_office() => {
            reject_pages(spec)?;
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
            reject_pages(spec)?;
            load_html(&spec.path, &options.html, &options.text)
        }
        Some(Format::Text) => {
            reject_pages(spec)?;
            textpdf::render(&crate::encoding::read_text(&spec.path)?, &options.text)
        }
        Some(format) if format.is_ebook() => {
            reject_pages(spec)?;
            let text = crate::ebook::load(&spec.path, format)?;
            textpdf::render(&text, &options.text)
        }
        _ => bail!("unsupported merge format: {}", spec.path.display()),
    }
}

/// Browser layout of an HTML page, or its text when no browser can render it.
fn load_html(path: &Path, html: &HtmlOptions, text: &TextOptions) -> Result<Document> {
    match html::convert_to_pdf(path, html) {
        Ok(bytes) => Document::load_mem(&bytes)
            .with_context(|| format!("browser output for {} is invalid", path.display())),
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

fn reject_pages(spec: &InputSpec) -> Result<()> {
    if spec.pages.is_some() {
        return Err(crate::commands::common::err_pdf_only_page_ranges(
            &spec.path,
        ));
    }
    Ok(())
}
