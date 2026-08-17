pub(crate) mod common;
mod convert;
mod merge;
mod ocr;
mod pdf_edit;
mod resize;
mod rotate;
mod strip;

use anyhow::Result;
use serde_json::json;

use crate::cli::Command;
use crate::config::Config;
use crate::pdf::transform;
use crate::{doctor, output, pdf};

pub fn run(command: Command, config: Config, fail_fast: bool) -> Result<()> {
    match command {
        Command::Merge(args) => merge::run(args, &config, fail_fast),
        Command::Ocr(args) => ocr::run(args, &config, fail_fast),
        Command::Split { input, output_dir } => pdf_edit::split(&input, output_dir.as_deref()),
        Command::Extract {
            input,
            pages,
            output,
        } => pdf_edit::extract(&input, &pages, output.as_deref()),
        Command::Inspect { input, text } => {
            let report = pdf::inspect_file(&input, text)?;
            output::data(
                "inspect",
                &report,
                json!({"path": input.to_string_lossy(), "report": report}),
            );
            Ok(())
        }
        Command::Strip(args) => strip::run(args, &config, fail_fast),
        Command::Rotate {
            inputs,
            degrees,
            orient,
            pages,
            out,
        } => rotate::run(
            &inputs,
            degrees,
            orient.as_deref(),
            &pages,
            out.as_deref(),
            &config,
            fail_fast,
        ),
        Command::Resize {
            inputs,
            size,
            long_edge,
            short_edge,
            pages,
            out,
        } => resize::run(
            &inputs,
            &size,
            long_edge,
            short_edge,
            &pages,
            out.as_deref(),
            &config,
            fail_fast,
        ),
        Command::Text { input, out } => pdf_edit::text(&input, out.as_deref()),
        Command::Doctor => doctor::run(&config),
        Command::Stamp(args) => pdf_edit::stamp(args),
        Command::Optimize { input, out } => {
            common::edit_pdf(&input, out, "optimized", |document| {
                optimize_document(document, &config);
                Ok(())
            })
        }
        Command::Metadata { command } => pdf_edit::metadata(command),
        Command::Convert(args) => convert::run(args, &config, fail_fast),
    }
}

fn optimize_document(document: &mut lopdf::Document, config: &Config) {
    let report = transform::optimize(document, config.image_dpi, config.jpeg_quality);
    for warning in report.warnings {
        output::warn(format!("Image optimization warning: {warning}"));
    }
    if config.image_dpi == 0 {
        output::info("Image downsampling is disabled by image_dpi=0");
    } else {
        output::info(format!(
            "Images downsampled to at most {} DPI: {}; skipped: {}",
            config.image_dpi, report.resized_images, report.skipped_images
        ));
    }
}
