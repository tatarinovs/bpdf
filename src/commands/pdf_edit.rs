use std::fs;
use std::path::Path;

use anyhow::{Result, bail};
use serde_json::json;

use super::common::{edit_pdf, edited_output, same_path, write_output};
use crate::cli::{MetadataArgs, NumberArgs, ProtectArgs, StampArgs, UnlockArgs, WatermarkArgs};
use crate::config::Config;
use crate::output;
use crate::pdf;
use crate::pdf::transform::{self, StampOptions};
use crate::stamp_picker;
use crate::textpdf::{self, TextMarkStyle};

pub fn split(input: &Path, output_dir: Option<&Path>, force: bool) -> Result<()> {
    let directory = output_dir.map(Path::to_path_buf).unwrap_or_else(|| {
        input
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    });
    fs::create_dir_all(&directory)?;
    for output_path in pdf::split_file(input, &directory, force)? {
        output::written(&output_path);
    }
    Ok(())
}

pub fn info(input: &Path) -> Result<()> {
    let (report, data) = pdf::info(input)?;
    output::data("info", &report, data);
    Ok(())
}

pub fn text(input: &Path, output_path: Option<&Path>) -> Result<()> {
    if let Some(output_path) = output_path
        && same_path(input, output_path)
    {
        bail!(
            "{} would replace the source PDF with its text",
            output_path.display()
        );
    }
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
        blend_mode: args.blend,
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
        transform::set_info_fields(document, &fields)?;
        // Readers prefer XMP over the Info dictionary, so an old XMP packet
        // would keep showing the old properties.
        if document.catalog_mut()?.remove(b"Metadata").is_some() {
            output::info("Removed the XMP metadata so readers show the new properties");
        }
        Ok(())
    })
}

pub fn protect(args: ProtectArgs) -> Result<()> {
    let restrictions = pdf::Restrictions {
        print: args.no_print,
        copy: args.no_copy,
        modify: args.no_modify,
    };
    let user = args.password.unwrap_or_default();
    if user.is_empty() && !restrictions.any() {
        bail!("set --password, or restrict the PDF with --no-print, --no-copy or --no-modify");
    }
    let owner = match args.owner_password {
        Some(owner) if !owner.is_empty() => owner,
        // Without an owner password of its own anyone with the open
        // password could lift the restrictions.
        _ if restrictions.any() => {
            output::info("Restrictions are protected by a random owner password");
            pdf::random_password()?
        }
        _ => user.clone(),
    };
    edit_pdf(&args.input, &args.output, "protected", |document| {
        pdf::protect(document, &user, &owner, restrictions)
    })
}

pub fn unlock(args: UnlockArgs) -> Result<()> {
    let destination = edited_output(&args.input, &args.output, "unlocked")?;
    let mut document =
        pdf::load_with_password(&args.input, Some(args.password.as_deref().unwrap_or("")))?;
    if document.encryption_state.take().is_none() {
        bail!("{} is not encrypted", args.input.display());
    }
    write_output(&destination, &pdf::save_to_bytes(&mut document)?)
}
