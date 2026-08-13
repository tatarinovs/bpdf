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
            let jpeg =
                imageconv::to_jpeg(&spec.path, &options.image, Some(&options.text.page_size))?;
            pdf::jpeg_document(jpeg, &options.text.page_size)
        }
        Some(Format::Pdf) => {
            let mut document = pdf::load(&spec.path)?;
            if let Some(pages) = &spec.pages {
                pdf::select_pages(&mut document, pages)?;
            }
            Ok(document)
        }
        Some(Format::Word | Format::Excel) => {
            reject_pages(spec)?;
            let bytes = office::convert_to_pdf(&spec.path, &options.office)?;
            Document::load_mem(&bytes)
                .with_context(|| format!("Office output for {} is invalid", spec.path.display()))
        }
        Some(Format::Text) => {
            reject_pages(spec)?;
            let text = fs::read_to_string(&spec.path)
                .with_context(|| format!("failed to read text file {}", spec.path.display()))?;
            textpdf::render(&text, &options.text)
        }
        _ => bail!("unsupported merge format: {}", spec.path.display()),
    }
}

fn reject_pages(spec: &InputSpec) -> Result<()> {
    if spec.pages.is_some() {
        bail!("page ranges are only valid for PDF inputs");
    }
    Ok(())
}
