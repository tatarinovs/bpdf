pub(crate) mod common;
mod convert;
mod merge;
mod ocr;
mod pdf_edit;
mod resize;
mod rotate;
mod strip;

use anyhow::{Result, bail};
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
        Command::Doctor(args) => doctor::run(&config, &args),
        Command::Stamp(args) => pdf_edit::stamp(args, &config),
        Command::Number(args) => pdf_edit::number(args, &config),
        Command::Watermark(args) => pdf_edit::watermark(args, &config),
        Command::Optimize {
            input,
            out,
            max_size,
        } => {
            let output = out.unwrap_or_else(|| common::suffixed_output(&input, "optimized", "pdf"));
            let mut document = pdf::load(&input)?;
            optimize_document(&mut document, &config);
            common::write_output(&output, &pdf_bytes(document, &config, max_size)?)
        }
        Command::Metadata { command } => pdf_edit::metadata(command),
        Command::Convert(args) => convert::run(args, &config, fail_fast),
    }
}

fn optimize_document(document: &mut lopdf::Document, config: &Config) {
    let report = transform::optimize(document, config.image_dpi, config.jpeg_quality, false);
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

/// Serialise a document, lowering image resolution and JPEG quality step by
/// step until it fits `max_size`. Without a limit the document is saved as is.
fn pdf_bytes(document: lopdf::Document, config: &Config, max_size: Option<u64>) -> Result<Vec<u8>> {
    let mut document = document;
    let Some(limit) = max_size else {
        return pdf::save_to_bytes(&mut document);
    };
    let fits = |bytes: &[u8]| bytes.len() as u64 <= limit;
    let original = pdf::save_to_bytes(&mut document.clone())?;
    if fits(&original) {
        return Ok(original);
    }

    let base_dpi = if config.image_dpi == 0 {
        300
    } else {
        config.image_dpi
    };
    let steps = [
        (300, 85),
        (200, 80),
        (150, 75),
        (150, 65),
        (120, 60),
        (100, 55),
        (100, 45),
        (85, 40),
        (72, 35),
    ]
    .into_iter()
    .filter(|(dpi, quality)| *dpi <= base_dpi && *quality <= config.jpeg_quality);

    let mut smallest = original.len();
    for (dpi, quality) in steps {
        let mut attempt = document.clone();
        transform::optimize(&mut attempt, dpi, quality, true);
        let bytes = pdf::save_to_bytes(&mut attempt)?;
        output::info(format!(
            "Size fitting: {dpi} DPI, JPEG quality {quality}: {}",
            human_size(bytes.len() as u64)
        ));
        if fits(&bytes) {
            return Ok(bytes);
        }
        smallest = smallest.min(bytes.len());
    }
    bail!(
        "cannot reduce the PDF to {}; the smallest result was {} (text, fonts and vector content are not reduced)",
        human_size(limit),
        human_size(smallest as u64)
    )
}

fn human_size(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.1} KB", bytes as f64 / 1024.0),
        _ => format!("{:.2} MB", bytes as f64 / 1_048_576.0),
    }
}
