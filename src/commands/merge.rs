use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;

use super::common::{
    DOCUMENT_SEPARATOR, batch, finish_batch, handle_results, join_numbers, reject_output_collision,
    same_path, suffixed_output, write_output,
};
use super::optimize_document;
use crate::cli::MergeArgs;
use crate::config::Config;
use crate::fileset::{InputSpec, expand};
use crate::formats::{self, Format, InputFormatSet};
use crate::html::HtmlOptions;
use crate::input::{self, LoadOptions};
use crate::office::OfficeOptions;
use crate::output;
use crate::pdf;
use crate::pdf::transform;
use crate::textpdf::TextOptions;

pub fn run(args: MergeArgs, config: &Config, fail_fast: bool) -> Result<()> {
    let specs = expand(&args.inputs, InputFormatSet::Merge)?;
    let output = args.out.unwrap_or_else(|| default_output(&specs));
    reject_output_collision(&output, &specs)?;

    if should_merge_as_text(&specs, &output) {
        return merge_text(&specs, &output, fail_fast);
    }

    let page_size = args.size.unwrap_or_else(|| config.page_size.clone());
    let keep_original_size = matches!(
        page_size.to_ascii_lowercase().as_str(),
        "none" | "original" | "keep"
    );
    let options = LoadOptions {
        image: config.image_options(),
        office: OfficeOptions {
            powershell: config.powershell.clone(),
            timeout: Duration::from_secs(config.office_timeout_seconds),
        },
        html: HtmlOptions {
            browser: config.browser.clone(),
            timeout: Duration::from_secs(config.html_timeout_seconds),
        },
        text: TextOptions {
            page_size: page_size.clone(),
            font_path: config.font_path.clone(),
            ..TextOptions::default()
        },
    };

    output::info(format!("Reading {} input file(s)...", specs.len()));
    let loads = batch(&specs, fail_fast, |spec| {
        output::info(format!("Reading {}", spec.path.display()));
        (
            &spec.path,
            input::load(spec, &options).map(|doc| (spec.path.clone(), doc)),
        )
    });
    let mut documents = Vec::with_capacity(specs.len());
    let mut bookmarks = Vec::new();
    let mut current_page = 1u32;
    let generate_bookmarks = MergeArgs::switch(args.bookmarks, args.no_bookmarks, config.bookmarks);

    let failures = handle_results(
        loads,
        fail_fast,
        "failed to load",
        "Skipping",
        |(path, document)| {
            if generate_bookmarks {
                let title = path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or("document")
                    .to_owned();
                let page_count = document.get_pages().len() as u32;
                if page_count > 0 {
                    bookmarks.push((title, current_page));
                    current_page += page_count;
                }
            }
            documents.push(document);
        },
    )?;

    if documents.is_empty() {
        return finish_batch("merge", specs.len(), failures, 0);
    }

    output::info("Merging page trees...");
    let mut document = pdf::merge_documents(documents)?;
    if generate_bookmarks && !bookmarks.is_empty() {
        output::info("Adding outline bookmarks...");
        transform::create_bookmarks(&mut document, &bookmarks)?;
    }
    if MergeArgs::switch(args.auto_rotate, args.no_auto_rotate, config.auto_rotate) {
        let pages = transform::auto_rotate(&mut document)?;
        if !pages.is_empty() {
            output::info(format!("Auto-rotated pages: {}", join_numbers(&pages)));
        }
    }
    if !keep_original_size {
        output::info(format!("Resizing pages to {page_size}..."));
        transform::resize_pages(&mut document, &page_size, "all")?;
    }
    if MergeArgs::switch(args.optimize, args.no_optimize, config.optimize) {
        output::info("Optimizing document...");
        optimize_document(&mut document, config);
    }
    if MergeArgs::switch(args.strip_meta, args.no_strip_meta, config.strip_metadata) {
        output::info("Stripping document metadata...");
        pdf::strip_document_metadata(&mut document);
    }
    transform::set_info_properties(&mut document, &config.author, &config.creator)?;

    write_output(&output, &super::pdf_bytes(document, config, args.max_size)?)?;
    finish_batch("merge", specs.len(), failures, 1)
}

fn merge_text(specs: &[InputSpec], output_path: &Path, fail_fast: bool) -> Result<()> {
    let texts = specs
        .iter()
        .map(|spec| (&spec.path, crate::encoding::read_text(&spec.path)));
    let mut parts = Vec::with_capacity(specs.len());
    let failures = handle_results(texts, fail_fast, "failed to read", "Skipping", |text| {
        parts.push(text);
    })?;
    if parts.is_empty() {
        return finish_batch("merge", specs.len(), failures, 0);
    }
    write_output(output_path, parts.join(DOCUMENT_SEPARATOR).as_bytes())?;
    finish_batch("merge", specs.len(), failures, 1)
}

/// `first.pdf` for a single non-PDF input, otherwise `first_merged.pdf`;
/// a name already on disk or among the inputs moves on to `_merged_2`, ...
fn default_output(specs: &[InputSpec]) -> PathBuf {
    let first = &specs[0].path;
    let taken = |path: &Path| path.exists() || specs.iter().any(|spec| same_path(path, &spec.path));
    let candidate = first.with_extension("pdf");
    if specs.len() == 1 && !taken(&candidate) {
        return candidate;
    }
    let mut output = suffixed_output(first, "merged", "pdf");
    let mut index = 2;
    while taken(&output) {
        output = suffixed_output(first, &format!("merged_{index}"), "pdf");
        index += 1;
    }
    output
}

fn should_merge_as_text(specs: &[InputSpec], output: &Path) -> bool {
    specs
        .iter()
        .all(|spec| formats::detect(&spec.path) == Some(Format::Text))
        && !formats::has_extension(output, "pdf")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_image_uses_its_stem() {
        let specs = [InputSpec {
            path: PathBuf::from("scan.jpg"),
            pages: None,
        }];
        assert_eq!(default_output(&specs), PathBuf::from("scan.pdf"));
    }

    #[test]
    fn pdf_output_never_overwrites_source_by_default() {
        let specs = [InputSpec {
            path: PathBuf::from("scan.pdf"),
            pages: None,
        }];
        assert_eq!(default_output(&specs), PathBuf::from("scan_merged.pdf"));
    }

    #[test]
    fn previous_merge_result_among_inputs_gets_next_name() {
        let specs = ["scan.pdf", "scan_merged.pdf", "scan_merged_2.pdf"].map(|path| InputSpec {
            path: PathBuf::from(path),
            pages: None,
        });
        assert_eq!(default_output(&specs), PathBuf::from("scan_merged_3.pdf"));
    }

    #[test]
    fn explicit_pdf_output_renders_text_inputs_as_pdf() {
        let specs = [InputSpec {
            path: PathBuf::from("data.json"),
            pages: None,
        }];
        assert!(!should_merge_as_text(&specs, Path::new("data.pdf")));
        assert!(should_merge_as_text(&specs, Path::new("combined.json")));
    }

    #[test]
    fn text_inputs_default_to_pdf_output() {
        let specs = [InputSpec {
            path: PathBuf::from("notes.txt"),
            pages: None,
        }];
        assert_eq!(default_output(&specs), PathBuf::from("notes.pdf"));
    }
}
