use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::common::{
    OutputRegistry, err_pdf_only_page_ranges, finish_batch, handle_results, write_output,
};
use crate::cli::ConvertArgs;
use crate::config::Config;
use crate::fileset::{InputSpec, expand};
use crate::formats::{self, Format, InputFormatSet};
use crate::imageconv::{self, ImageOptions};
use crate::{ocr, output, pdf};

#[derive(Debug)]
enum Plan {
    Image(PathBuf),
    Tiff,
    Pdf(Option<String>),
}

pub fn run(args: ConvertArgs, config: &Config, fail_fast: bool) -> Result<()> {
    let specs = expand(&args.inputs, InputFormatSet::Convert)?;
    let mut image_options = config.image_options(args.keep_icc, args.ffmpeg.clone());
    image_options.long_edge = args.long_edge;
    image_options.short_edge = args.short_edge;
    image_options.orient = args.orient;
    if let Some(quality) = args.quality {
        image_options.jpeg_quality = quality;
        image_options.force_reencode = true;
    }
    let output_dir = args.out.as_deref();
    if let Some(directory) = output_dir {
        if !directory.exists() {
            fs::create_dir_all(directory)?;
        } else if !directory.is_dir() {
            bail!("--out {} is not a directory", directory.display());
        }
    }

    let mut registry = OutputRegistry::default();
    let plans = build_plans(&specs, output_dir, args.force, &mut registry);
    let mut outputs = 0usize;
    let results = plans.into_iter().map(|(input, plan)| {
        let result = plan.and_then(|plan| {
            execute(
                &input,
                plan,
                output_dir,
                args.force,
                args.render,
                &image_options,
                &mut registry,
            )
        });
        (input, result)
    });
    let failures = handle_results(
        results,
        fail_fast,
        "failed to convert",
        "error converting",
        |written| outputs += written,
    )?;
    finish_batch("convert", specs.len(), failures, outputs)
}

fn build_plans(
    specs: &[InputSpec],
    output_dir: Option<&Path>,
    force: bool,
    registry: &mut OutputRegistry,
) -> Vec<(PathBuf, Result<Plan>)> {
    specs
        .iter()
        .map(|spec| {
            let input = spec.path.clone();
            let plan = (|| match formats::detect(&input) {
                Some(format) if format.is_image() => {
                    if spec.pages.is_some() {
                        return Err(err_pdf_only_page_ranges(&input));
                    }
                    let is_tiff = input.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                        e.eq_ignore_ascii_case("tif") || e.eq_ignore_ascii_case("tiff")
                    });
                    if is_tiff {
                        Ok(Plan::Tiff)
                    } else {
                        let output = image_output_path(&input, output_dir)?;
                        registry.reserve(&input, &output, force)?;
                        Ok(Plan::Image(output))
                    }
                }
                Some(Format::Pdf) => Ok(Plan::Pdf(spec.pages.clone())),
                _ => bail!("unsupported convert input: {}", input.display()),
            })();
            (input, plan)
        })
        .collect()
}

fn execute(
    input: &Path,
    plan: Plan,
    output_dir: Option<&Path>,
    force: bool,
    render: bool,
    image_options: &ImageOptions,
    registry: &mut OutputRegistry,
) -> Result<usize> {
    match plan {
        Plan::Image(output) => {
            output::info(format!("Converting {}", input.display()));
            let jpeg = imageconv::to_jpeg(input, image_options, None)?;
            write_output(&output, &jpeg)?;
            Ok(1)
        }
        Plan::Tiff => {
            output::info(format!("Converting TIFF pages from {}", input.display()));
            let frames = imageconv::to_jpegs_for_pdf(input, image_options, None)?;
            if frames.is_empty() {
                bail!("no decodable frames found in {}", input.display());
            }
            let file_stem = input
                .file_stem()
                .with_context(|| format!("{} has no file stem", input.display()))?;
            let count = frames.len();
            for (i, bytes) in frames.into_iter().enumerate() {
                let suffix = if count > 1 {
                    format!("page_{}.jpg", i + 1)
                } else {
                    "jpg".to_string()
                };
                let output = output_dir
                    .map(|directory| directory.join(file_stem).with_extension(&suffix))
                    .unwrap_or_else(|| input.with_extension(&suffix));
                registry.reserve(input, &output, force)?;
                output::info(format!("Saving page to {}", output.display()));
                write_output(&output, &bytes)?;
            }
            Ok(count)
        }
        Plan::Pdf(pages) => {
            let mut document = pdf::load(input)?;

            // Auto-detect text pages if --render was not explicitly provided
            let mut should_render = render;
            if !should_render
                && let Ok(text) = pdf::extract_text(&document)
                && !text.trim().is_empty()
            {
                should_render = true;
            }

            if should_render {
                output::info(format!("Rendering PDF pages to JPEG: {}", input.display()));
                if pages.is_some() {
                    output::warn(
                        "Page selection is not yet supported for PDF rendering, rendering all pages.",
                    );
                }
                let rendered =
                    crate::winpdf::render_pdf_to_jpegs(input, output_dir, image_options)?;
                let count = rendered.len();
                for (output_path, bytes) in rendered {
                    output::info(format!("Saving rendered page to {}", output_path.display()));
                    registry.reserve(input, &output_path, force)?;
                    write_output(&output_path, &bytes)?;
                }
                return Ok(count);
            }

            output::info(format!("Extracting images from PDF {}", input.display()));
            if let Some(pages) = pages {
                pdf::select_pages(&mut document, &pages)?;
            }
            let images = ocr::extract_pdf_images(&document, image_options)
                .with_context(|| format!("failed to extract images from {}", input.display()))?;
            if images.is_empty() {
                bail!("no extractable images found in {}", input.display());
            }
            let file_stem = input
                .file_stem()
                .with_context(|| format!("{} has no file stem", input.display()))?;
            let outputs = images
                .into_iter()
                .map(|image| {
                    let suffix = format!("{}.jpg", image.label);
                    let output = output_dir
                        .map(|directory| directory.join(file_stem).with_extension(&suffix))
                        .unwrap_or_else(|| input.with_extension(&suffix));
                    registry.reserve(input, &output, force)?;
                    Ok((output, image.bytes))
                })
                .collect::<Result<Vec<_>>>()?;
            let count = outputs.len();
            for (output_path, bytes) in outputs {
                output::info(format!(
                    "Saving extracted image to {}",
                    output_path.display()
                ));
                write_output(&output_path, &bytes)?;
            }
            Ok(count)
        }
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

#[cfg(test)]
mod tests {
    use image::{DynamicImage, ImageFormat, Rgb, RgbImage};

    use super::*;

    fn sample_png(path: &Path) {
        DynamicImage::ImageRgb8(RgbImage::from_pixel(2, 2, Rgb([10, 20, 30])))
            .save_with_format(path, ImageFormat::Png)
            .unwrap();
    }

    fn args(inputs: &[&Path], out: Option<PathBuf>, force: bool) -> ConvertArgs {
        ConvertArgs {
            inputs: inputs
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            out,
            keep_icc: None,
            ffmpeg: None,
            force,
            long_edge: None,
            short_edge: None,
            orient: None,
            quality: None,
            render: false,
        }
    }

    #[test]
    fn refuses_in_place_conversion_without_force() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("photo.jpg");
        DynamicImage::ImageRgb8(RgbImage::from_pixel(2, 2, Rgb([10, 20, 30])))
            .save_with_format(&input, ImageFormat::Jpeg)
            .unwrap();
        let error = run(args(&[&input], None, false), &Config::default(), true).unwrap_err();
        assert!(format!("{error:#}").contains("--force"));
    }

    #[test]
    fn force_allows_in_place_conversion() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("photo.jpg");
        DynamicImage::ImageRgb8(RgbImage::from_pixel(2, 2, Rgb([10, 20, 30])))
            .save_with_format(&input, ImageFormat::Jpeg)
            .unwrap();
        run(args(&[&input], None, true), &Config::default(), false).unwrap();
        assert!(input.is_file());
    }

    #[test]
    fn best_effort_continues_after_error() {
        let directory = tempfile::tempdir().unwrap();
        let invalid = directory.path().join("broken.png");
        let valid = directory.path().join("valid.png");
        let output = directory.path().join("out");
        fs::write(&invalid, b"not an image").unwrap();
        sample_png(&valid);
        let error = run(
            args(&[&invalid, &valid], Some(output.clone()), false),
            &Config::default(),
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("1 of 2"));
        assert!(output.join("valid.jpg").is_file());
    }

    #[test]
    fn fail_fast_stops_before_next_input() {
        let directory = tempfile::tempdir().unwrap();
        let invalid = directory.path().join("broken.png");
        let valid = directory.path().join("valid.png");
        let output = directory.path().join("out");
        fs::write(&invalid, b"not an image").unwrap();
        sample_png(&valid);
        run(
            args(&[&invalid, &valid], Some(output.clone()), false),
            &Config::default(),
            true,
        )
        .unwrap_err();
        assert!(!output.join("valid.jpg").exists());
    }

    #[test]
    fn duplicate_names_are_rejected_before_writing() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first").join("photo.png");
        let second = directory.path().join("second").join("photo.png");
        fs::create_dir_all(first.parent().unwrap()).unwrap();
        fs::create_dir_all(second.parent().unwrap()).unwrap();
        sample_png(&first);
        sample_png(&second);
        let output = directory.path().join("out");
        fs::create_dir_all(&output).unwrap();
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
        let plans = build_plans(&specs, Some(&output), false, &mut registry);
        assert!(plans[0].1.is_ok());
        assert!(format!("{:#}", plans[1].1.as_ref().unwrap_err()).contains("multiple inputs"));
    }

    #[test]
    fn page_ranges_on_images_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let image = directory.path().join("photo.png");
        sample_png(&image);
        let specs = [InputSpec {
            path: image,
            pages: Some("1".to_owned()),
        }];
        let mut registry = OutputRegistry::default();
        let plans = build_plans(&specs, None, false, &mut registry);
        assert!(plans[0].1.is_err());
        assert!(
            format!("{:#}", plans[0].1.as_ref().unwrap_err())
                .contains("page ranges are only valid for PDF inputs")
        );
    }
}
