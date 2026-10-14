pub(crate) mod common;
mod convert;
mod merge;
mod ocr;
mod pdf_edit;
mod resize;
mod rotate;
mod strip;

use anyhow::{Result, bail};

use crate::cli::Command;
use crate::config::Config;
use crate::pdf::transform;
use crate::{doctor, output, pdf};

pub fn run(command: Command, config: Config, fail_fast: bool) -> Result<()> {
    match command {
        Command::Merge(args) => merge::run(args, &config, fail_fast),
        Command::Ocr(args) => ocr::run(args, &config, fail_fast),
        Command::Split { input, out } => pdf_edit::split(&input, out.as_deref()),
        Command::Extract { input, pages, out } => pdf_edit::extract(&input, &pages, out.as_deref()),
        Command::Info { input } => pdf_edit::info(&input),
        Command::Text { input, out } => pdf_edit::text(&input, out.as_deref()),
        Command::Strip(args) => strip::run(args, &config, fail_fast),
        Command::Rotate(args) => rotate::run(args, &config, fail_fast),
        Command::Resize(args) => resize::run(args, &config, fail_fast),
        Command::Optimize(args) => {
            let mut document = pdf::load(&args.input)?;
            optimize_document(&mut document, &config);
            let bytes = pdf_bytes(document, &config, args.max_size)?;
            common::write_output(
                &common::edited_output(&args.input, &args.output, "optimized"),
                &bytes,
            )
        }
        Command::Stamp(args) => pdf_edit::stamp(args, &config),
        Command::Number(args) => pdf_edit::number(args, &config),
        Command::Watermark(args) => pdf_edit::watermark(args, &config),
        Command::Metadata(args) => pdf_edit::metadata(args),
        Command::Convert(args) => convert::run(args, &config, fail_fast),
        Command::Doctor => doctor::run(&config),
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

/// Serialise a document, lowering image resolution and JPEG quality until it
/// fits `max_size`. Without a limit the document is saved as is.
///
/// Steps get smaller monotonically, so the best fitting step is searched:
/// the gentlest one first (often enough), then the strongest (is it possible
/// at all?), then bisection in between. Each attempt re-encodes every image,
/// so this needs about four attempts instead of up to nine.
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
    .filter(|(dpi, quality)| *dpi <= base_dpi && *quality <= config.jpeg_quality)
    .collect::<Vec<_>>();
    if steps.is_empty() {
        bail!(
            "cannot reduce the PDF to {}; image_dpi and jpeg_quality are already below every step",
            human_size(limit)
        );
    }

    let attempt = |index: usize| -> Result<Vec<u8>> {
        let (dpi, quality) = steps[index];
        let mut candidate = document.clone();
        transform::optimize(&mut candidate, dpi, quality, true);
        let bytes = pdf::save_to_bytes(&mut candidate)?;
        output::info(format!(
            "Size fitting: {dpi} DPI, JPEG quality {quality}: {}",
            human_size(bytes.len() as u64)
        ));
        Ok(bytes)
    };

    let first = attempt(0)?;
    if fits(&first) {
        return Ok(first);
    }
    let last_index = steps.len() - 1;
    let mut best = attempt(last_index)?;
    if !fits(&best) {
        bail!(
            "cannot reduce the PDF to {}; the smallest result was {} (text, fonts and vector content are not reduced)",
            human_size(limit),
            human_size(best.len().min(first.len()) as u64)
        );
    }
    // Invariant: step `low` does not fit, step `high` does (its bytes in `best`).
    let (mut low, mut high) = (0, last_index);
    while high - low > 1 {
        let middle = (low + high) / 2;
        let bytes = attempt(middle)?;
        if fits(&bytes) {
            high = middle;
            best = bytes;
        } else {
            low = middle;
        }
    }
    Ok(best)
}

fn human_size(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.1} KB", bytes as f64 / 1024.0),
        _ => format!("{:.2} MB", bytes as f64 / 1_048_576.0),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::{DynamicImage, ImageFormat, Rgb, RgbImage};

    use super::*;

    fn noisy_page() -> lopdf::Document {
        let mut seed = 7u32;
        let image = RgbImage::from_fn(700, 990, |_, _| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let value = (seed >> 16) as u8;
            Rgb([value, value.wrapping_mul(3), value.wrapping_add(90)])
        });
        let mut jpeg = Vec::new();
        DynamicImage::ImageRgb8(image)
            .write_to(&mut Cursor::new(&mut jpeg), ImageFormat::Jpeg)
            .unwrap();
        pdf::jpeg_document(jpeg, "A4").unwrap()
    }

    #[test]
    fn size_fitting_finds_a_step_within_the_limit() {
        let config = Config::default();
        let full = pdf_bytes(noisy_page(), &config, None).unwrap().len() as u64;
        let limit = full * 2 / 5;
        let fitted = pdf_bytes(noisy_page(), &config, Some(limit)).unwrap();
        assert!(fitted.len() as u64 <= limit, "{} > {limit}", fitted.len());
        assert!(pdf_bytes(noisy_page(), &config, Some(1000)).is_err());
    }
}
