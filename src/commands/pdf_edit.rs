use std::fs;
use std::path::Path;

use anyhow::{Result, bail};
use serde_json::json;

use super::common::{edit_pdf, same_path, suffixed_output, write_output};
use crate::cli::{MetadataArgs, NumberArgs, StampArgs, WatermarkArgs};
use crate::config::Config;
use crate::output;
use crate::pdf;
use crate::pdf::transform::{self, StampOptions};
use crate::stamp_picker;
use crate::textpdf::{self, TextMarkStyle};

pub fn split(input: &Path, output_dir: Option<&Path>) -> Result<()> {
    let directory = output_dir.map(Path::to_path_buf).unwrap_or_else(|| {
        input
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    });
    fs::create_dir_all(&directory)?;
    for output_path in pdf::split_file(input, &directory)? {
        output::written(&output_path);
    }
    Ok(())
}

pub fn extract(input: &Path, pages: &str, output_path: Option<&Path>) -> Result<()> {
    let output_path = output_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| suffixed_output(input, "extracted", "pdf"));
    if same_path(input, &output_path) {
        bail!("refusing to overwrite the input PDF");
    }
    write_output(
        &output_path,
        &pdf::transform_file(input, |document| pdf::select_pages(document, pages))?,
    )
}

pub fn info(input: &Path) -> Result<()> {
    let (report, data) = pdf::info(input)?;
    output::data("info", &report, data);
    Ok(())
}

pub fn text(input: &Path, output_path: Option<&Path>) -> Result<()> {
    let document = pdf::load(input)?;
    let text = pdf::extract_text(&document)?;
    if text.trim().is_empty() {
        output::warn(format!(
            "{} has no text layer; use `bpdf ocr` to recognise it",
            input.display()
        ));
    }
    if let Some(output_path) = output_path {
        write_output(output_path, text.as_bytes())?;
    } else {
        output::data(
            "text",
            &text,
            json!({"path": input.to_string_lossy(), "text": text}),
        );
    }
    Ok(())
}

pub fn stamp(args: StampArgs, config: &Config) -> Result<()> {
    let mut options = StampOptions {
        path: args.stamp,
        position: args.position,
        scale: args.scale,
        dpi: args.dpi,
        opacity: args.opacity,
        pages: args.pages,
        blend_mode: transform::BlendMode::parse(&args.blend)?,
        placements: Vec::new(),
    };
    if args.pick {
        match stamp_picker::pick(&args.input, &options, config.browser.as_deref())? {
            Some(placements) => options.placements = placements,
            None => {
                output::info("Stamp placement cancelled; nothing was written");
                return Ok(());
            }
        }
    }
    edit_pdf(&args.input, &args.output, "stamped", |document| {
        transform::apply_stamp(document, &options)
    })
}

pub fn number(args: NumberArgs, config: &Config) -> Result<()> {
    if !args.format.contains("{n}") && !args.format.contains("{total}") {
        bail!("--format must contain {{n}} or {{total}}");
    }
    let style = TextMarkStyle {
        font_size: args.size,
        position: args.position,
        angle: 0.0,
        opacity: args.opacity,
        color: args.color,
        pages: args.pages,
    };
    // The first numbered page gets `--start`; {total} is the last number.
    let label = |ordinal: usize, count: usize| {
        args.format
            .replace("{n}", &(args.start + ordinal as i64 - 1).to_string())
            .replace("{total}", &(args.start + count as i64 - 1).to_string())
    };
    edit_pdf(&args.input, &args.output, "numbered", |document| {
        textpdf::add_text_marks(document, &style, config.font_path.as_deref(), label)
    })
}

pub fn watermark(args: WatermarkArgs, config: &Config) -> Result<()> {
    if args.text.trim().is_empty() {
        bail!("watermark text is empty");
    }
    let style = TextMarkStyle {
        font_size: args.size,
        position: args.position,
        angle: args.angle,
        opacity: args.opacity,
        color: args.color,
        pages: args.pages,
    };
    edit_pdf(&args.input, &args.output, "watermarked", |document| {
        textpdf::add_text_marks(document, &style, config.font_path.as_deref(), |_, _| {
            args.text.clone()
        })
    })
}

pub fn metadata(args: MetadataArgs) -> Result<()> {
    let fields = [
        ("Title", args.title.as_deref()),
        ("Author", args.author.as_deref()),
        ("Subject", args.subject.as_deref()),
        ("Keywords", args.keywords.as_deref()),
        ("Creator", args.creator.as_deref()),
    ];
    if fields.iter().all(|(_, value)| value.is_none()) {
        bail!(
            "set at least one of --title, --author, --subject, --keywords, --creator; `bpdf info` shows the current values"
        );
    }
    edit_pdf(&args.input, &args.output, "metadata", |document| {
        transform::set_info_fields(document, &fields)
    })
}
