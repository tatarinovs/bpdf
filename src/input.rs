use std::fs;

use anyhow::{Context, Result, bail};
use lopdf::Document;

use crate::fileset::InputSpec;
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
    if imageconv::is_supported_image(&spec.path) {
        reject_pages(spec)?;
        let jpeg = imageconv::to_jpeg(&spec.path, &options.image, Some(&options.text.page_size))?;
        return pdf::jpeg_document(jpeg, &options.text.page_size);
    }

    if is_pdf(spec) {
        let mut document = Document::load(&spec.path)
            .with_context(|| format!("failed to load PDF {}", spec.path.display()))?;
        if let Some(pages) = &spec.pages {
            pdf::select_pages(&mut document, pages)?;
        }
        return Ok(document);
    }

    if office::is_office(&spec.path) {
        reject_pages(spec)?;
        let bytes = office::convert_to_pdf(&spec.path, &options.office)?;
        return Document::load_mem(&bytes)
            .with_context(|| format!("Office output for {} is invalid", spec.path.display()));
    }

    if is_text(&spec.path) {
        reject_pages(spec)?;
        let text = fs::read_to_string(&spec.path)
            .with_context(|| format!("failed to read text file {}", spec.path.display()))?;
        return textpdf::render(&text, &options.text);
    }

    bail!("unsupported merge format: {}", spec.path.display())
}

pub fn is_text(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("md" | "txt")
    )
}

fn is_pdf(spec: &InputSpec) -> bool {
    spec.path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("pdf"))
}

fn reject_pages(spec: &InputSpec) -> Result<()> {
    if spec.pages.is_some() {
        bail!("page ranges are only valid for PDF inputs");
    }
    Ok(())
}
