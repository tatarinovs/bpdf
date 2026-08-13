use std::fs;
use std::path::Path;

use anyhow::{Result, bail};
use serde_json::json;

use super::common::{edit_pdf, same_path, suffixed_output, write_output};
use crate::cli::{MetadataCommand, StampArgs};
use crate::output;
use crate::pdf;
use crate::pdf::transform::{self, StampMode, StampOptions};

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

pub fn stamp(args: StampArgs) -> Result<()> {
    let StampArgs {
        input,
        stamp,
        out,
        position,
        scale,
        opacity,
        pages,
        mode,
    } = args;
    edit_pdf(&input, out, "stamped", |document| {
        transform::apply_stamp(
            document,
            &StampOptions {
                path: stamp,
                position,
                scale,
                opacity,
                pages,
                mode: StampMode::parse(&mode)?,
            },
        )
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
