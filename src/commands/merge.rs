use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

use super::common::{
    DOCUMENT_SEPARATOR, finish_batch, handle_results, join_numbers, reject_output_collision,
    same_path, suffixed_output, write_output,
};
use super::optimize_document;
use crate::cli::MergeArgs;
use crate::config::Config;
use crate::fileset::{InputSpec, expand};
use crate::formats::{self, Format, InputFormatSet};
use crate::input::{self, LoadOptions};
use crate::office::OfficeOptions;
use crate::output;
use crate::pdf;
use crate::pdf::transform::{self, StampMode, StampOptions};
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
        image: config.image_options(args.keep_icc, args.ffmpeg),
        office: OfficeOptions {
            powershell: config.powershell.clone(),
            timeout: Duration::from_secs(config.office_timeout_seconds),
        },
        text: TextOptions {
            page_size: page_size.clone(),
            font_path: config.font_path.clone(),
            ..TextOptions::default()
        },
    };

    output::info(format!("Reading {} input file(s)...", specs.len()));
    let loads = specs.iter().enumerate().map(|(index, spec)| {
        output::info(format!(
            "[{}/{}] {}",
            index + 1,
            specs.len(),
            spec.path.display()
        ));
        (&spec.path, input::load(spec, &options))
    });
    let mut documents = Vec::with_capacity(specs.len());
    let failures = handle_results(loads, fail_fast, "failed to load", "Skipping", |document| {
        documents.push(document);
    })?;

    if documents.is_empty() {
        return finish_batch("merge", specs.len(), failures, 0);
    }

    output::info("Merging page trees...");
    let mut document = pdf::merge_documents(documents)?;
    if !args.no_rotate && args.auto_rotate.unwrap_or(config.auto_rotate) {
        let pages = transform::auto_rotate(&mut document)?;
        if !pages.is_empty() {
            output::info(format!("Auto-rotated pages: {}", join_numbers(&pages)));
        }
    }
    if !keep_original_size {
        output::info(format!("Resizing pages to {page_size}..."));
        if args.no_rotate {
            transform::resize_pages_preserving_orientation(&mut document, &page_size, "all")?;
        } else {
            transform::resize_pages(&mut document, &page_size, "all")?;
        }
    }
    if let Some(path) = args.stamp {
        output::info(format!("Applying stamp {}...", path.display()));
        transform::apply_stamp(
            &mut document,
            &StampOptions {
                path,
                position: args.stamp_pos,
                scale: args.stamp_scale,
                opacity: args.stamp_op,
                pages: args.stamp_pages,
                mode: StampMode::parse(&args.stamp_mode)?,
            },
        )?;
    }
    if args.optimize.unwrap_or(config.optimize) {
        output::info("Optimizing document...");
        optimize_document(&mut document, config);
    }
    if args.strip_meta.unwrap_or(config.strip_metadata) {
        output::info("Stripping document metadata...");
        pdf::strip_document_metadata(&mut document);
    }
    transform::set_info_properties(
        &mut document,
        args.author.as_deref().unwrap_or(&config.author),
        args.creator.as_deref().unwrap_or(&config.creator),
    )?;

    write_output(&output, &pdf::save_to_bytes(&mut document)?)?;
    finish_batch("merge", specs.len(), failures, 1)
}

fn merge_text(specs: &[InputSpec], output_path: &Path, fail_fast: bool) -> Result<()> {
    let mut result = String::new();
    let mut count = 0;
    for spec in specs {
        match fs::read_to_string(&spec.path) {
            Ok(content) => {
                if count != 0 {
                    result.push_str(DOCUMENT_SEPARATOR);
                }
                result.push_str(&content);
                count += 1;
            }
            Err(error) if fail_fast => {
                return Err(error)
                    .with_context(|| format!("failed to read {}", spec.path.display()));
            }
            Err(error) => output::warn(format!("Skipping {}: {error:#}", spec.path.display())),
        }
    }
    if count == 0 {
        return finish_batch("merge", specs.len(), specs.len(), 0);
    }
    write_output(output_path, result.as_bytes())?;
    finish_batch("merge", specs.len(), specs.len() - count, 1)
}

fn default_output(specs: &[InputSpec]) -> PathBuf {
    let first = &specs[0].path;
    if specs
        .iter()
        .all(|spec| formats::detect(&spec.path) == Some(Format::Text))
    {
        let extension = first
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("txt");
        return PathBuf::from(format!("merged_output.{extension}"));
    }
    let candidate = first.with_extension("pdf");
    if specs.len() == 1 && !same_path(first, &candidate) {
        candidate
    } else {
        suffixed_output(first, "merged", "pdf")
    }
}

fn should_merge_as_text(specs: &[InputSpec], output: &Path) -> bool {
    specs
        .iter()
        .all(|spec| formats::detect(&spec.path) == Some(Format::Text))
        && !output
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
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
    fn explicit_pdf_output_renders_text_inputs_as_pdf() {
        let specs = [InputSpec {
            path: PathBuf::from("data.json"),
            pages: None,
        }];
        assert!(!should_merge_as_text(&specs, Path::new("data.pdf")));
        assert!(should_merge_as_text(&specs, Path::new("combined.json")));
    }
}
