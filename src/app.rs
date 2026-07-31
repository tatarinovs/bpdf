use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use lopdf::Document;
use serde_json::json;

use crate::atomic::write_atomic;
use crate::cli::{Command, MergeArgs, MetadataCommand, OcrArgs, StampArgs, StripArgs};
use crate::config::Config;
use crate::fileset::{ExpandOptions, InputSpec, expand};
use crate::imageconv::{self, ImageOptions};
use crate::input::{self, LoadOptions};
use crate::ocr::{self, OcrEngine, OcrOptions};
use crate::office::OfficeOptions;
use crate::pdf;
use crate::pdf::transform::{self, StampMode, StampOptions};
use crate::textpdf::TextOptions;
use crate::{doctor, output};

const OCR_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "bmp", "gif", "tiff", "tif", "webp", "heic", "heif", "pdf",
];

pub fn run(command: Command, config: Config) -> Result<()> {
    match command {
        Command::Merge(args) => merge(args, &config),
        Command::Ocr(args) => ocr(args, &config),
        Command::Split { input, output_dir } => split(&input, output_dir.as_deref()),
        Command::Extract {
            input,
            pages,
            output,
        } => extract(&input, &pages, output.as_deref()),
        Command::Inspect { input, text } => {
            let report = pdf::inspect_file(&input, text)?;
            output::data(
                "inspect",
                &report,
                json!({"path": input.to_string_lossy(), "report": report}),
            );
            Ok(())
        }
        Command::Strip(args) => strip(args, &config),
        Command::Rotate {
            input,
            degrees,
            pages,
            out,
        } => edit_pdf(&input, out, "rotated", |document| {
            transform::rotate_pages(document, &pages, degrees)
        }),
        Command::Resize {
            input,
            size,
            pages,
            out,
        } => edit_pdf(&input, out, "resized", |document| {
            transform::resize_pages(document, &size, &pages)
        }),
        Command::Text { input, out } => extract_native_text(&input, out.as_deref()),
        Command::Doctor => doctor::run(&config),
        Command::Stamp(args) => stamp(args),
        Command::Optimize { input, out } => edit_pdf(&input, out, "optimized", |document| {
            transform::optimize(document);
            Ok(())
        }),
        Command::Metadata { command } => metadata(command),
        Command::Images {
            input,
            output_dir,
            ffmpeg,
        } => extract_images(&input, output_dir.as_deref(), ffmpeg, &config),
    }
}

fn merge(args: MergeArgs, config: &Config) -> Result<()> {
    let specs = expand(
        &args.inputs,
        ExpandOptions {
            directory_extensions: crate::fileset::MERGE_EXTENSIONS,
        },
    )?;
    let output = args.out.unwrap_or_else(|| default_merge_output(&specs));
    reject_output_collision(&output, &specs)?;

    if specs.iter().all(|spec| input::is_text(&spec.path)) {
        return merge_text(&specs, &output);
    }

    let page_size = args.size.unwrap_or_else(|| config.page_size.clone());
    let keep_original_size = matches!(
        page_size.to_ascii_lowercase().as_str(),
        "none" | "original" | "keep"
    );
    let image = ImageOptions {
        keep_icc: args.keep_icc.unwrap_or(config.keep_icc),
        ffmpeg: args.ffmpeg.unwrap_or_else(|| config.ffmpeg.clone()),
        jpeg_quality: config.jpeg_quality,
        image_dpi: config.image_dpi,
    };
    let options = LoadOptions {
        image,
        office: OfficeOptions {
            powershell: config.powershell.clone(),
            timeout: Duration::from_secs(config.office_timeout_seconds),
        },
        text: TextOptions {
            page_size: if keep_original_size {
                "A4".to_owned()
            } else {
                page_size.clone()
            },
            font_path: config.font_path.clone(),
            ..TextOptions::default()
        },
    };

    output::info(format!("Reading {} input file(s)...", specs.len()));
    let mut documents = Vec::with_capacity(specs.len());
    for (index, spec) in specs.iter().enumerate() {
        output::info(format!(
            "[{}/{}] {}",
            index + 1,
            specs.len(),
            spec.path.display()
        ));
        documents.push(input::load(spec, &options)?);
    }

    output::info("Merging page trees...");
    let mut document = pdf::merge_documents(documents)?;
    if args.auto_rotate.unwrap_or(config.auto_rotate) {
        let pages = transform::auto_rotate(&mut document)?;
        if !pages.is_empty() {
            output::info(format!("Auto-rotated pages: {}", join_numbers(&pages)));
        }
    }
    if !keep_original_size {
        output::info(format!("Resizing pages to {page_size}..."));
        transform::resize_pages(&mut document, &page_size, "all")?;
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
        transform::optimize(&mut document);
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

    let bytes = pdf::save_to_bytes(&mut document)?;
    write_atomic(&output, &bytes)
        .with_context(|| format!("failed to write {}", output.display()))?;
    output::written(&output);
    Ok(())
}

fn ocr(args: OcrArgs, config: &Config) -> Result<()> {
    let specs = expand(
        &args.inputs,
        ExpandOptions {
            directory_extensions: OCR_EXTENSIONS,
        },
    )?;
    if specs.iter().any(|spec| spec.pages.is_some()) {
        bail!("page ranges are not supported by ocr; extract the pages first");
    }

    let image = ImageOptions {
        keep_icc: config.keep_icc,
        ffmpeg: args.ffmpeg.unwrap_or_else(|| config.ffmpeg.clone()),
        jpeg_quality: config.jpeg_quality,
        image_dpi: config.image_dpi,
    };
    let jobs = args.jobs.unwrap_or(config.ocr_jobs);
    if !(1..=64).contains(&jobs) {
        bail!("--jobs must be between 1 and 64");
    }
    let cache_dir = (!args.no_cache && config.ocr_cache).then(|| {
        args.cache_dir
            .clone()
            .unwrap_or_else(|| config.ocr_cache_dir.clone())
    });
    let engine = OcrEngine::new(OcrOptions {
        api_key: args.groq_key.unwrap_or_else(|| config.groq_api_key.clone()),
        proxy: args.proxy.unwrap_or_else(|| config.proxy.clone()),
        model: args.model.unwrap_or_else(|| config.ocr_model.clone()),
        prompt: args.prompt.unwrap_or_else(|| config.ocr_prompt.clone()),
        endpoint: args.endpoint.unwrap_or_else(|| config.ocr_endpoint.clone()),
        timeout: Duration::from_secs(config.ocr_timeout_seconds),
        force_image_ocr: args.force_ocr,
        image,
        jobs,
        cache_dir,
    })?;

    let combined = args.out.is_some();
    let mut parts = Vec::new();
    let mut failures = 0usize;
    for spec in &specs {
        output::info(format!("Queued OCR: {}", spec.path.display()));
    }
    let paths = specs
        .iter()
        .map(|spec| spec.path.clone())
        .collect::<Vec<_>>();
    let results = engine.extract_many(&paths);
    for (spec, result) in specs.iter().zip(results) {
        match result {
            Ok(text) if combined => parts.push(text),
            Ok(text) => {
                let output = spec.path.with_extension("md");
                if let Err(error) = write_atomic(&output, text.as_bytes()) {
                    failures += 1;
                    output::warn(format!("error writing {}: {error:#}", output.display()));
                } else {
                    output::written(&output);
                }
            }
            Err(error) => {
                failures += 1;
                output::warn(format!(
                    "error processing {}: {error:#}",
                    spec.path.display()
                ));
            }
        }
    }
    if let Some(output) = args.out
        && !parts.is_empty()
    {
        write_atomic(&output, parts.join("\n\n---\n\n").as_bytes())?;
        output::written(&output);
    }
    if failures != 0 {
        bail!("{failures} of {} files failed", specs.len());
    }
    Ok(())
}

fn strip(args: StripArgs, config: &Config) -> Result<()> {
    let specs = expand(
        &args.inputs,
        ExpandOptions {
            directory_extensions: crate::fileset::STRIP_EXTENSIONS,
        },
    )?;
    if args.out.is_some() && specs.len() != 1 {
        bail!("--out is only valid with one input file");
    }
    let options = ImageOptions {
        keep_icc: args.keep_icc.unwrap_or(config.keep_icc),
        ffmpeg: args.ffmpeg.unwrap_or_else(|| config.ffmpeg.clone()),
        jpeg_quality: config.jpeg_quality,
        image_dpi: config.image_dpi,
    };
    let mut failures = 0usize;
    for spec in &specs {
        if let Err(error) = strip_one(spec, args.out.as_deref(), &options) {
            failures += 1;
            output::warn(format!(
                "error processing {}: {error:#}",
                spec.path.display()
            ));
        }
    }
    if failures != 0 {
        bail!("{failures} of {} files failed", specs.len());
    }
    Ok(())
}

fn strip_one(spec: &InputSpec, explicit_out: Option<&Path>, options: &ImageOptions) -> Result<()> {
    if spec.pages.is_some() {
        bail!("page ranges are not valid for strip");
    }
    let input = &spec.path;
    let extension = input
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let output = explicit_out.map(Path::to_path_buf).unwrap_or_else(|| {
        if imageconv::is_heic(input) {
            input.with_extension("jpg")
        } else {
            input.clone()
        }
    });
    let bytes = if imageconv::is_heic(input) {
        imageconv::heic_to_jpeg(input, options)?
    } else if imageconv::is_jpeg(input) {
        crate::metadata::strip_jpeg(&fs::read(input)?, options.keep_icc)?
    } else if imageconv::is_png(input) {
        crate::metadata::strip_png(&fs::read(input)?, options.keep_icc)?
    } else if extension == "pdf" {
        pdf::strip_pdf_metadata(input)?
    } else {
        bail!("unsupported strip format: .{extension}");
    };
    write_atomic(&output, &bytes)?;
    output::written(&output);
    Ok(())
}

fn split(input: &Path, output_dir: Option<&Path>) -> Result<()> {
    let directory = output_dir.map(Path::to_path_buf).unwrap_or_else(|| {
        input
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    });
    fs::create_dir_all(&directory)?;
    for output in pdf::split_file(input, &directory)? {
        output::written(&output);
    }
    Ok(())
}

fn extract(input: &Path, pages: &str, output: Option<&Path>) -> Result<()> {
    let output = output
        .map(Path::to_path_buf)
        .unwrap_or_else(|| suffixed_output(input, "extracted", "pdf"));
    if same_path(input, &output) {
        bail!("refusing to overwrite the input PDF");
    }
    write_atomic(&output, &pdf::extract_file(input, pages)?)?;
    output::written(&output);
    Ok(())
}

fn edit_pdf<F>(input: &Path, output: Option<PathBuf>, suffix: &str, edit: F) -> Result<()>
where
    F: FnOnce(&mut Document) -> Result<()>,
{
    let output = output.unwrap_or_else(|| suffixed_output(input, suffix, "pdf"));
    if same_path(input, &output) {
        bail!("refusing to overwrite the input PDF");
    }
    let mut document =
        Document::load(input).with_context(|| format!("failed to load {}", input.display()))?;
    edit(&mut document)?;
    write_atomic(&output, &pdf::save_to_bytes(&mut document)?)?;
    output::written(&output);
    Ok(())
}

fn extract_native_text(input: &Path, output: Option<&Path>) -> Result<()> {
    let document =
        Document::load(input).with_context(|| format!("failed to load {}", input.display()))?;
    let pages = document.get_pages().keys().copied().collect::<Vec<_>>();
    let text = document
        .extract_text_with_limit(&pages, 256 * 1024 * 1024)
        .context("failed to extract PDF text")?;
    if let Some(output) = output {
        write_atomic(output, text.as_bytes())?;
        output::written(output);
    } else {
        output::data(
            "text",
            &text,
            json!({"path": input.to_string_lossy(), "text": text}),
        );
    }
    Ok(())
}

fn merge_text(specs: &[InputSpec], output: &Path) -> Result<()> {
    let mut result = String::new();
    for (index, spec) in specs.iter().enumerate() {
        if index != 0 {
            result.push_str("\n\n---\n\n");
        }
        result.push_str(
            &fs::read_to_string(&spec.path)
                .with_context(|| format!("failed to read {}", spec.path.display()))?,
        );
    }
    write_atomic(output, result.as_bytes())?;
    output::written(output);
    Ok(())
}

fn stamp(args: StampArgs) -> Result<()> {
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

fn metadata(command: MetadataCommand) -> Result<()> {
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

fn extract_images(
    input: &Path,
    output_dir: Option<&Path>,
    ffmpeg: Option<PathBuf>,
    config: &Config,
) -> Result<()> {
    let document =
        Document::load(input).with_context(|| format!("failed to load {}", input.display()))?;
    let images = ocr::extract_pdf_images(
        &document,
        &ImageOptions {
            keep_icc: config.keep_icc,
            ffmpeg: ffmpeg.unwrap_or_else(|| config.ffmpeg.clone()),
            jpeg_quality: config.jpeg_quality,
            image_dpi: config.image_dpi,
        },
    )?;
    let directory = output_dir.map(Path::to_path_buf).unwrap_or_else(|| {
        let stem = input
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("document");
        input
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("{stem}_images"))
    });
    fs::create_dir_all(&directory)?;
    for image in &images {
        let path = directory.join(format!("{}.jpg", image.label));
        write_atomic(&path, &image.bytes)?;
        output::written(&path);
    }
    output::result(
        "images_extracted",
        format!("Extracted {} image(s)", images.len()),
        json!({
            "input": input.to_string_lossy(),
            "output_dir": directory.to_string_lossy(),
            "count": images.len(),
        }),
    );
    Ok(())
}

fn default_merge_output(specs: &[InputSpec]) -> PathBuf {
    let first = &specs[0].path;
    if specs.iter().all(|spec| input::is_text(&spec.path)) {
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

fn reject_output_collision(output: &Path, specs: &[InputSpec]) -> Result<()> {
    if specs.iter().any(|spec| same_path(output, &spec.path)) {
        bail!(
            "output {} is also an input; choose a different path",
            output.display()
        );
    }
    Ok(())
}

fn suffixed_output(input: &Path, suffix: &str, extension: &str) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("document");
    input.with_file_name(format!("{stem}_{suffix}.{extension}"))
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = fs::canonicalize(left).unwrap_or_else(|_| absolute(left));
    let right = fs::canonicalize(right).unwrap_or_else(|_| absolute(right));
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn join_numbers(values: &[u32]) -> String {
    values
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_for_single_image_uses_stem() {
        let specs = [InputSpec {
            path: PathBuf::from("scan.jpg"),
            pages: None,
        }];
        assert_eq!(default_merge_output(&specs), PathBuf::from("scan.pdf"));
    }

    #[test]
    fn output_for_pdf_cannot_overwrite_source() {
        let specs = [InputSpec {
            path: PathBuf::from("scan.pdf"),
            pages: None,
        }];
        assert_eq!(
            default_merge_output(&specs),
            PathBuf::from("scan_merged.pdf")
        );
    }
}
