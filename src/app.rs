use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use lopdf::Document;
use serde_json::json;

use crate::atomic::write_atomic;
use crate::cli::{Command, ConvertArgs, MergeArgs, MetadataCommand, OcrArgs, StampArgs, StripArgs};
use crate::config::Config;
use crate::fileset::{ExpandOptions, InputSpec, expand};
use crate::imageconv::{self, ImageOptions};
use crate::input::{self, LoadOptions};
use crate::ocr::{OcrEngine, OcrOptions};
use crate::office::OfficeOptions;
use crate::pdf;
use crate::pdf::transform::{self, StampMode, StampOptions};
use crate::textpdf::TextOptions;
use crate::{doctor, output};

const OCR_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "bmp", "gif", "tiff", "tif", "webp", "heic", "heif", "pdf",
];

pub fn run(command: Command, config: Config, fail_fast: bool) -> Result<()> {
    match command {
        Command::Merge(args) => merge(args, &config, fail_fast),
        Command::Ocr(args) => ocr(args, &config, fail_fast),
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
        Command::Strip(args) => strip(args, &config, fail_fast),
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
        Command::Convert(args) => run_convert(args, &config, fail_fast),
    }
}

fn merge(args: MergeArgs, config: &Config, fail_fast: bool) -> Result<()> {
    let specs = expand(
        &args.inputs,
        ExpandOptions {
            directory_extensions: crate::fileset::MERGE_EXTENSIONS,
        },
    )?;
    let output = args.out.unwrap_or_else(|| default_merge_output(&specs));
    reject_output_collision(&output, &specs)?;

    if specs.iter().all(|spec| input::is_text(&spec.path)) {
        return merge_text(&specs, &output, fail_fast);
    }

    let page_size = args.size.unwrap_or_else(|| config.page_size.clone());
    let keep_original_size = matches!(
        page_size.to_ascii_lowercase().as_str(),
        "none" | "original" | "keep"
    );
    let image = config.image_options(args.keep_icc, args.ffmpeg);
    let options = LoadOptions {
        image,
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
    let mut documents = Vec::with_capacity(specs.len());
    let mut failures = 0usize;
    for (index, spec) in specs.iter().enumerate() {
        output::info(format!(
            "[{}/{}] {}",
            index + 1,
            specs.len(),
            spec.path.display()
        ));
        match input::load(spec, &options) {
            Ok(document) => documents.push(document),
            Err(error) => {
                if fail_fast {
                    return Err(error)
                        .with_context(|| format!("failed to load {}", spec.path.display()));
                }
                failures += 1;
                output::warn(format!("Skipping {}: {error:#}", spec.path.display()));
            }
        }
    }

    if documents.is_empty() {
        return finish_batch("merge", specs.len(), 0, failures, 0);
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
    finish_batch("merge", specs.len(), specs.len() - failures, failures, 1)
}

fn ocr(args: OcrArgs, config: &Config, fail_fast: bool) -> Result<()> {
    let specs = expand(
        &args.inputs,
        ExpandOptions {
            directory_extensions: OCR_EXTENSIONS,
        },
    )?;
    if specs.iter().any(|spec| spec.pages.is_some()) {
        bail!("page ranges are not supported by ocr; extract the pages first");
    }

    let image = config.image_options(None, args.ffmpeg);
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
        api_key: config.groq_api_key.clone(),
        proxy: args.proxy.unwrap_or_else(|| config.proxy.clone()),
        model: args.model.unwrap_or_else(|| config.ocr_model.clone()),
        prompt: args.prompt.unwrap_or_else(|| config.ocr_prompt.clone()),
        endpoint: args.endpoint.unwrap_or_else(|| config.ocr_endpoint.clone()),
        timeout: Duration::from_secs(config.ocr_timeout_seconds),
        force_image_ocr: args.force_ocr,
        image,
        jobs,
        max_tokens: config.ocr_max_tokens,
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
    let results = if fail_fast {
        let mut results = Vec::new();
        for path in &paths {
            let result = engine.extract_text(path);
            let failed = result.is_err();
            results.push(result);
            if failed {
                break;
            }
        }
        results
    } else {
        engine.extract_many(&paths)
    };
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
                if fail_fast {
                    return Err(error)
                        .with_context(|| format!("failed to process {}", spec.path.display()));
                }
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
    let outputs = if combined {
        usize::from(!parts.is_empty())
    } else {
        specs.len() - failures
    };
    finish_batch(
        "ocr",
        specs.len(),
        specs.len() - failures,
        failures,
        outputs,
    )
}

fn strip(args: StripArgs, config: &Config, fail_fast: bool) -> Result<()> {
    let specs = expand(
        &args.inputs,
        ExpandOptions {
            directory_extensions: crate::fileset::STRIP_EXTENSIONS,
        },
    )?;
    if args.out.is_some() && specs.len() != 1 {
        bail!("--out is only valid with one input file");
    }
    let options = config.image_options(args.keep_icc, args.ffmpeg);
    let mut failures = 0usize;
    for spec in &specs {
        if let Err(error) = strip_one(spec, args.out.as_deref(), &options) {
            if fail_fast {
                return Err(error)
                    .with_context(|| format!("failed to process {}", spec.path.display()));
            }
            failures += 1;
            output::warn(format!(
                "error processing {}: {error:#}",
                spec.path.display()
            ));
        }
    }
    finish_batch(
        "strip",
        specs.len(),
        specs.len() - failures,
        failures,
        specs.len() - failures,
    )
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
    if same_path(input, &output) {
        output::info(format!("Stripping metadata in-place: {}", input.display()));
    }
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
    // Read into memory so we can safely write back to the same path.
    let data = fs::read(input).with_context(|| format!("failed to read {}", input.display()))?;
    let mut document = Document::load_mem(&data)
        .with_context(|| format!("failed to parse {}", input.display()))?;
    drop(data);
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

fn merge_text(specs: &[InputSpec], output: &Path, fail_fast: bool) -> Result<()> {
    let mut result = String::new();
    let mut count = 0;
    for spec in specs {
        match fs::read_to_string(&spec.path) {
            Ok(content) => {
                if count != 0 {
                    result.push_str("\n\n---\n\n");
                }
                result.push_str(&content);
                count += 1;
            }
            Err(error) => {
                if fail_fast {
                    return Err(error)
                        .with_context(|| format!("failed to read {}", spec.path.display()));
                }
                output::warn(format!("Skipping {}: {error:#}", spec.path.display()));
            }
        }
    }
    if count == 0 {
        return finish_batch("merge", specs.len(), 0, specs.len(), 0);
    }
    write_atomic(output, result.as_bytes())?;
    output::written(output);
    finish_batch("merge", specs.len(), count, specs.len() - count, 1)
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

fn finish_batch(
    operation: &str,
    total: usize,
    succeeded: usize,
    failed: usize,
    outputs: usize,
) -> Result<()> {
    output::result(
        "batch_summary",
        format!("{operation}: {succeeded} succeeded, {failed} failed, {outputs} output(s)"),
        json!({
            "operation": operation,
            "total": total,
            "succeeded": succeeded,
            "failed": failed,
            "outputs": outputs,
        }),
    );
    if failed != 0 {
        bail!("{failed} of {total} files failed");
    }
    Ok(())
}

#[derive(Debug)]
enum ConvertPlan {
    Image { input: PathBuf, output: PathBuf },
    Pdf { input: PathBuf },
}

#[derive(Default)]
struct OutputRegistry {
    destinations: HashSet<String>,
}

impl OutputRegistry {
    fn reserve(&mut self, input: &Path, output_path: &Path, force: bool) -> Result<()> {
        if !self.destinations.insert(path_key(output_path)) {
            bail!(
                "multiple inputs would write {}; choose unique names or output directories",
                output_path.display()
            );
        }
        if output_path.exists() && !force {
            if same_path(input, output_path) {
                bail!(
                    "{} would overwrite the input; pass --force to convert in place",
                    output_path.display()
                );
            }
            bail!(
                "output {} already exists; pass --force to replace it",
                output_path.display()
            );
        }
        Ok(())
    }
}

fn path_key(path: &Path) -> String {
    let resolved = fs::canonicalize(path).unwrap_or_else(|_| {
        path.parent()
            .and_then(|parent| fs::canonicalize(parent).ok())
            .and_then(|parent| path.file_name().map(|name| parent.join(name)))
            .unwrap_or_else(|| absolute(path))
    });
    let value = resolved.to_string_lossy().into_owned();
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    }
}

fn image_output_path(input: &Path, output_dir: Option<&Path>) -> Result<PathBuf> {
    let file_name = input
        .file_name()
        .with_context(|| format!("{} has no file name", input.display()))?;
    Ok(output_dir
        .map(|directory| directory.join(file_name).with_extension("jpg"))
        .unwrap_or_else(|| input.with_extension("jpg")))
}

fn build_convert_plans(
    specs: &[InputSpec],
    output_dir: Option<&Path>,
    force: bool,
    registry: &mut OutputRegistry,
) -> Vec<(PathBuf, Result<ConvertPlan>)> {
    specs
        .iter()
        .map(|spec| {
            let input = spec.path.clone();
            let plan = (|| {
                if spec.pages.is_some() {
                    bail!("page ranges are not valid for convert");
                }
                if imageconv::is_supported_image(&input) {
                    let output = image_output_path(&input, output_dir)?;
                    registry.reserve(&input, &output, force)?;
                    Ok(ConvertPlan::Image {
                        input: input.clone(),
                        output,
                    })
                } else if input
                    .extension()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| value.eq_ignore_ascii_case("pdf"))
                {
                    Ok(ConvertPlan::Pdf {
                        input: input.clone(),
                    })
                } else {
                    bail!("unsupported convert input: {}", input.display())
                }
            })();
            (input, plan)
        })
        .collect()
}

fn run_convert(args: ConvertArgs, config: &Config, fail_fast: bool) -> Result<()> {
    let specs = expand(
        &args.inputs,
        ExpandOptions {
            directory_extensions: crate::fileset::CONVERT_EXTENSIONS,
        },
    )?;

    let image_options = config.image_options(args.keep_icc, args.ffmpeg.clone());

    let output_dir = args.out.as_deref();
    if let Some(dir) = output_dir {
        if !dir.exists() {
            fs::create_dir_all(dir)?;
        } else if !dir.is_dir() {
            bail!("--out {} is not a directory", dir.display());
        }
    }

    let mut registry = OutputRegistry::default();
    let plans = build_convert_plans(&specs, output_dir, args.force, &mut registry);
    let mut succeeded = 0usize;
    let mut failures = 0usize;
    let mut outputs = 0usize;

    for (input, plan) in plans {
        let result = plan.and_then(|plan| {
            convert_one(plan, output_dir, args.force, &image_options, &mut registry)
        });
        match result {
            Ok(written) => {
                succeeded += 1;
                outputs += written;
            }
            Err(error) => {
                if fail_fast {
                    return Err(error)
                        .with_context(|| format!("failed to convert {}", input.display()));
                }
                failures += 1;
                output::warn(format!("error converting {}: {error:#}", input.display()));
            }
        }
    }

    finish_batch("convert", specs.len(), succeeded, failures, outputs)
}

fn convert_one(
    plan: ConvertPlan,
    output_dir: Option<&Path>,
    force: bool,
    image_options: &ImageOptions,
    registry: &mut OutputRegistry,
) -> Result<usize> {
    match plan {
        ConvertPlan::Image { input, output } => {
            output::info(format!("Converting {}", input.display()));
            let jpeg_bytes = imageconv::to_jpeg(&input, image_options, None)?;
            write_atomic(&output, &jpeg_bytes)?;
            output::written(&output);
            Ok(1)
        }
        ConvertPlan::Pdf { input } => {
            output::info(format!("Extracting images from PDF {}", input.display()));
            let data =
                fs::read(&input).with_context(|| format!("failed to read {}", input.display()))?;
            let document = lopdf::Document::load_mem(&data)
                .with_context(|| format!("failed to parse {}", input.display()))?;
            let images = crate::ocr::extract_pdf_images(&document, image_options)
                .with_context(|| format!("failed to extract images from {}", input.display()))?;
            if images.is_empty() {
                bail!("no extractable images found in {}", input.display());
            }

            let file_stem = input
                .file_stem()
                .with_context(|| format!("{} has no file stem", input.display()))?;
            let planned = images
                .into_iter()
                .map(|image| {
                    let suffix = format!("{}.jpg", image.label);
                    let output_path = output_dir
                        .map(|directory| directory.join(file_stem).with_extension(&suffix))
                        .unwrap_or_else(|| input.with_extension(&suffix));
                    registry.reserve(&input, &output_path, force)?;
                    Ok((output_path, image.bytes))
                })
                .collect::<Result<Vec<_>>>()?;

            let count = planned.len();
            for (output_path, bytes) in planned {
                output::info(format!(
                    "Saving extracted image to {}",
                    output_path.display()
                ));
                write_atomic(&output_path, &bytes)?;
                output::written(&output_path);
            }
            Ok(count)
        }
    }
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

    #[test]
    fn convert_refuses_to_overwrite_input_without_force() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("photo.jpg");
        sample_jpeg_file(&input);

        let error = run_convert(
            ConvertArgs {
                inputs: vec![input.to_string_lossy().into_owned()],
                out: None,
                keep_icc: None,
                ffmpeg: None,
                force: false,
            },
            &Config::default(),
            true,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("--force"));
    }

    #[test]
    fn convert_overwrites_input_with_force() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("photo.jpg");
        sample_jpeg_file(&input);

        run_convert(
            ConvertArgs {
                inputs: vec![input.to_string_lossy().into_owned()],
                out: None,
                keep_icc: None,
                ffmpeg: None,
                force: true,
            },
            &Config::default(),
            false,
        )
        .unwrap();
        assert!(input.is_file());
    }

    #[test]
    fn convert_rejects_duplicate_output_names() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first").join("photo.png");
        let second = directory.path().join("second").join("photo.png");
        fs::create_dir_all(first.parent().unwrap()).unwrap();
        fs::create_dir_all(second.parent().unwrap()).unwrap();
        let output = directory.path().join("out");
        fs::create_dir_all(&output).unwrap();
        sample_png_file(&first);
        sample_png_file(&second);
        let specs = [
            InputSpec {
                path: first,
                pages: None,
            },
            InputSpec {
                path: second,
                pages: None,
            },
        ];
        let mut registry = OutputRegistry::default();

        let plans = build_convert_plans(&specs, Some(&output), false, &mut registry);

        assert!(plans[0].1.is_ok());
        assert!(
            plans[1]
                .1
                .as_ref()
                .unwrap_err()
                .to_string()
                .contains("multiple inputs")
        );
    }

    #[test]
    fn convert_best_effort_continues_after_an_input_error() {
        let directory = tempfile::tempdir().unwrap();
        let invalid = directory.path().join("broken.png");
        let valid = directory.path().join("valid.png");
        let output = directory.path().join("out");
        fs::write(&invalid, b"not an image").unwrap();
        sample_png_file(&valid);

        let error = run_convert(
            ConvertArgs {
                inputs: vec![
                    invalid.to_string_lossy().into_owned(),
                    valid.to_string_lossy().into_owned(),
                ],
                out: Some(output.clone()),
                keep_icc: None,
                ffmpeg: None,
                force: false,
            },
            &Config::default(),
            false,
        )
        .unwrap_err();

        assert!(error.to_string().contains("1 of 2"));
        assert!(output.join("valid.jpg").is_file());
    }

    #[test]
    fn convert_fail_fast_stops_before_the_next_input() {
        let directory = tempfile::tempdir().unwrap();
        let invalid = directory.path().join("broken.png");
        let valid = directory.path().join("valid.png");
        let output = directory.path().join("out");
        fs::write(&invalid, b"not an image").unwrap();
        sample_png_file(&valid);

        run_convert(
            ConvertArgs {
                inputs: vec![
                    invalid.to_string_lossy().into_owned(),
                    valid.to_string_lossy().into_owned(),
                ],
                out: Some(output.clone()),
                keep_icc: None,
                ffmpeg: None,
                force: false,
            },
            &Config::default(),
            true,
        )
        .unwrap_err();

        assert!(!output.join("valid.jpg").exists());
    }

    fn sample_jpeg_file(path: &Path) {
        use image::{ImageFormat, Rgb, RgbImage};
        image::DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, Rgb([10, 20, 30])))
            .save_with_format(path, ImageFormat::Jpeg)
            .unwrap();
    }

    fn sample_png_file(path: &Path) {
        use image::{ImageFormat, Rgb, RgbImage};
        image::DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, Rgb([10, 20, 30])))
            .save_with_format(path, ImageFormat::Png)
            .unwrap();
    }
}
