use std::fs;

use anyhow::{Context, Result, bail};
use lopdf::Document;

use crate::fileset::InputSpec;
use crate::formats::{self, Format};
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
            let mut documents = jpegs
                .into_iter()
                .map(|jpeg| pdf::jpeg_document(jpeg, &options.text.page_size))
                .collect::<Result<Vec<_>>>()?;
            if documents.len() == 1 {
                Ok(documents.pop().expect("one image document"))
            } else {
                pdf::merge_documents(documents)
            }
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
        Some(Format::Text) => {
            reject_pages(spec)?;
            let text = fs::read_to_string(&spec.path)
                .with_context(|| format!("failed to read text file {}", spec.path.display()))?;
            textpdf::render(&text, &options.text)
        }
        Some(format) if format.is_ebook() => {
            reject_pages(spec)?;
            let text = crate::ebook::load(&spec.path)?;
            textpdf::render(&text, &options.text)
        }
        _ => bail!("unsupported merge format: {}", spec.path.display()),
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
