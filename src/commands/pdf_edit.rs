use std::fs;
use std::path::Path;

use anyhow::{Result, bail};
use serde_json::json;

use super::common::{edit_pdf, same_path, suffixed_output, write_output};
use crate::cli::{MetadataCommand, NumberArgs, StampArgs, WatermarkArgs};
use crate::config::Config;
use crate::output;
use crate::pdf;
use crate::pdf::transform::{self, StampMode, StampOptions};
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

pub fn text(input: &Path, output_path: Option<&Path>) -> Result<()> {
    let document = pdf::load(input)?;
    let text = pdf::extract_text(&document)?;
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
    let StampArgs {
        input,
        stamp,
        out,
        position,
        scale,
        dpi,
        opacity,
        pages,
        mode,
        blend,
        pick,
    } = args;
    let mut options = StampOptions {
        path: stamp,
        position,
        scale,
        dpi,
        opacity,
        pages,
        mode: StampMode::parse(&mode)?,
        blend_mode: transform::BlendMode::parse(&blend)?,
        placements: Vec::new(),
    };
    if pick {
        match stamp_picker::pick(&input, &options, config.browser.as_deref())? {
            Some(placements) => options.placements = placements,
            None => {
                output::info("Stamp placement cancelled; nothing was written");
                return Ok(());
            }
        }
    }
    edit_pdf(&input, out, "stamped", |document| {
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
        under: false,
    };
    let offset = args.start - 1;
    edit_pdf(&args.input, args.out, "numbered", |document| {
        textpdf::add_text_marks(
            document,
            &style,
            config.font_path.as_deref(),
            |number, total| {
                args.format
                    .replace("{n}", &(number as i64 + offset).to_string())
                    .replace("{total}", &(total as i64 + offset).to_string())
            },
        )
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
        under: args.under,
    };
    edit_pdf(&args.input, args.out, "watermarked", |document| {
        textpdf::add_text_marks(document, &style, config.font_path.as_deref(), |_, _| {
            args.text.clone()
        })
    })
}

pub fn metadata(command: MetadataCommand) -> Result<()> {
    match command {
        MetadataCommand::Show { input } => {
            let report = pdf::metadata_report(&input)?;
            let text = format!("{}\n", serde_json::to_string_pretty(&report)?);
            output::data("metadata", &text, report);
            Ok(())
        }
        MetadataCommand::Set {
            input,
            out,
            title,
            author,
            subject,
            keywords,
            creator,
        } => {
            if [&title, &author, &subject, &keywords, &creator]
                .iter()
                .all(|value| value.is_none())
            {
                bail!("metadata set requires at least one field");
            }
            edit_pdf(&input, out, "metadata", |document| {
                transform::set_info_fields(
                    document,
                    &[
                        ("Title", title.as_deref()),
                        ("Author", author.as_deref()),
                        ("Subject", subject.as_deref()),
                        ("Keywords", keywords.as_deref()),
                        ("Creator", creator.as_deref()),
                    ],
                )
            })
        }
    }
}
