use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::common::{OutputRegistry, batch, finish_batch, handle_results, write_output};
use crate::cli::ConvertArgs;
use crate::config::Config;
use crate::fileset::{InputSpec, expand};
use crate::formats::{self, Format, InputFormatSet};
use crate::html::{self, HtmlOptions, Viewport};
use crate::imageconv::{self, ImageOptions};
use crate::pdf::image::{self as pdf_image, Selection};
use crate::{output, parallel, pdf};

#[derive(Debug)]
enum Plan {
    Image(PathBuf),
    Html(PathBuf),
    Svg(PathBuf),
    Tiff,
    Pdf(Option<String>),
}

struct Converter<'a> {
    output_dir: Option<&'a Path>,
    force: bool,
    render: bool,
    image_options: ImageOptions,
    html: HtmlOptions,
    viewport: Viewport,
    max_size: Option<u64>,
    registry: Mutex<OutputRegistry>,
}

pub fn run(args: ConvertArgs, config: &Config, fail_fast: bool) -> Result<()> {
    let specs = expand(&args.inputs, InputFormatSet::Convert)?;
    let mut image_options = config.image_options();
    image_options.long_edge = args.long_edge;
    image_options.short_edge = args.short_edge;
    image_options.orient = args.orient;
    if let Some(quality) = args.quality {
        image_options.jpeg_quality = quality;
        image_options.force_reencode = true;
    }

    let viewport = Viewport::parse(args.viewport.as_deref().unwrap_or(&config.html_viewport))?;

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
    let converter = Converter {
        output_dir,
        force: args.force,
        render: args.render,
        image_options,
        html: HtmlOptions {
            browser: config.browser.clone(),
            timeout: Duration::from_secs(config.html_timeout_seconds),
        },
        viewport,
        max_size: args.max_size,
        registry: Mutex::new(registry),
    };
    let results = batch(&plans, fail_fast, |(input, plan)| {
        let result = match plan {
            Ok(plan) => converter.execute(input, plan),
            Err(error) => Err(anyhow::anyhow!("{error:#}")),
        };
        (input, result)
    });
    let mut outputs = 0usize;
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
                    spec.reject_pages()?;
                    if formats::is_tiff(&input) {
                        return Ok(Plan::Tiff);
                    }
                    let file_name = input
                        .file_name()
                        .with_context(|| format!("{} has no file name", input.display()))?;
                    let output = output_dir
                        .map_or_else(|| input.clone(), |directory| directory.join(file_name))
                        .with_extension("jpg");
                    registry.reserve(&input, &output, force)?;
                    Ok(Plan::Image(output))
                }
                Some(Format::Pdf) => Ok(Plan::Pdf(spec.pages.clone())),
                Some(format @ (Format::Html | Format::Svg)) => {
                    spec.reject_pages()?;
                    let output = output_name(&input, output_dir, None)?;
                    registry.reserve(&input, &output, force)?;
                    Ok(if format == Format::Svg {
                        Plan::Svg(output)
                    } else {
                        Plan::Html(output)
                    })
                }
                _ => bail!("unsupported convert input: {}", input.display()),
            })();
            (input, plan)
        })
        .collect()
}

impl Converter<'_> {
    fn execute(&self, input: &Path, plan: &Plan) -> Result<usize> {
        match plan {
            Plan::Image(output) => {
                output::info(format!("Converting {}", input.display()));
                let jpeg = imageconv::to_jpeg(input, &self.image_options, None)?;
                write_output(output, &self.limit(jpeg)?)?;
                Ok(1)
            }
            Plan::Svg(output) => {
                output::info(format!("Drawing {}", input.display()));
                let png = html::svg_screenshot(input, &self.html)?;
                let jpeg = imageconv::bytes_to_jpeg(&png, &self.image_options, None)?;
                write_output(output, &self.limit(jpeg)?)?;
                Ok(1)
            }
            Plan::Html(output) => {
                output::info(format!(
                    "Rendering {} at {}x{}",
                    input.display(),
                    self.viewport.width,
                    self.viewport.height
                ));
                let png = html::screenshot(input, &self.html, self.viewport)?;
                let jpeg = imageconv::bytes_to_jpeg(&png, &self.image_options, None)?;
                write_output(output, &self.limit(jpeg)?)?;
                Ok(1)
            }
            Plan::Tiff => {
                output::info(format!("Converting TIFF pages from {}", input.display()));
                let frames = imageconv::to_jpegs_for_pdf(input, &self.image_options, None)?;
                if frames.is_empty() {
                    bail!("no decodable frames found in {}", input.display());
                }
                let single = frames.len() == 1;
                let outputs = frames
                    .into_iter()
                    .enumerate()
                    .map(|(index, bytes)| {
                        let tag = (!single).then(|| format!("page_{}", index + 1));
                        Ok((self.sibling_output(input, tag.as_deref())?, bytes))
                    })
                    .collect::<Result<Vec<_>>>()?;
                self.write_all(input, outputs, "Saving page to")
            }
            Plan::Pdf(pages) => self.convert_pdf(input, pages.as_deref()),
        }
    }

    fn convert_pdf(&self, input: &Path, pages: Option<&str>) -> Result<usize> {
        let mut document = pdf::load(input)?;
        // Pages with fonts carry vector text that image extraction would lose.
        let has_text = document.get_pages().values().any(|page_id| {
            document
                .get_page_fonts(*page_id)
                .is_ok_and(|fonts| !fonts.is_empty())
        });
        if self.render || has_text {
            output::info(format!("Rendering PDF pages to JPEG: {}", input.display()));
            let numbers =
                pdf::parse_page_selection(pages.unwrap_or("all"), document.get_pages().len())?
                    .into_iter()
                    .collect::<Vec<_>>();
            let rendered =
                crate::winpdf::render_pdf_to_jpegs(input, &numbers, &self.image_options)?
                    .into_iter()
                    .zip(&numbers)
                    .map(|(bytes, number)| {
                        Ok((
                            self.sibling_output(input, Some(&format!("page_{number}")))?,
                            self.adjust(bytes)?,
                        ))
                    })
                    .collect::<Result<Vec<_>>>()?;
            return self.write_all(input, rendered, "Saving rendered page to");
        }

        output::info(format!("Extracting images from PDF {}", input.display()));
        if let Some(pages) = pages {
            pdf::select_pages(&mut document, pages)?;
        }
        let outputs = pdf_image::extract_images(
            &document,
            Selection::Largest,
            self.image_options.jpeg_quality,
        )
        .into_iter()
        .flat_map(|(_, images)| images)
        .map(|image| {
            let output = self.sibling_output(input, Some(&image.label))?;
            Ok((output, self.adjust(image.bytes)?))
        })
        .collect::<Result<Vec<_>>>()?;
        if outputs.is_empty() {
            bail!("no extractable images found in {}", input.display());
        }
        self.write_all(input, outputs, "Saving extracted image to")
    }

    fn sibling_output(&self, input: &Path, tag: Option<&str>) -> Result<PathBuf> {
        output_name(input, self.output_dir, tag)
    }

    /// Apply `--long-edge`, `--short-edge`, `--orient` and `-q` to a page
    /// image that was rendered or extracted as-is.
    fn adjust(&self, jpeg: Vec<u8>) -> Result<Vec<u8>> {
        if self.image_options.needs_reencode() {
            imageconv::bytes_to_jpeg(&jpeg, &self.image_options, None)
        } else {
            Ok(jpeg)
        }
    }

    /// Keep a JPEG within `--max-size`.
    fn limit(&self, jpeg: Vec<u8>) -> Result<Vec<u8>> {
        match self.max_size {
            Some(limit) => {
                imageconv::fit_jpeg_to_size(jpeg, limit, self.image_options.jpeg_quality)
            }
            None => Ok(jpeg),
        }
    }

    /// Reserve every destination first, so a name clash writes nothing.
    fn write_all(
        &self,
        input: &Path,
        outputs: Vec<(PathBuf, Vec<u8>)>,
        message: &str,
    ) -> Result<usize> {
        {
            let mut registry = parallel::lock(&self.registry);
            for (path, _) in &outputs {
                registry.reserve(input, path, self.force)?;
            }
        }
        let count = outputs.len();
        for (path, bytes) in outputs {
            output::info(format!("{message} {}", path.display()));
            write_output(&path, &self.limit(bytes)?)?;
        }
        Ok(count)
    }
}

/// `<stem>.jpg` or `<stem>_<tag>.jpg` next to the input or inside `output_dir`.
fn output_name(input: &Path, output_dir: Option<&Path>, tag: Option<&str>) -> Result<PathBuf> {
    let stem = input
        .file_stem()
        .with_context(|| format!("{} has no file stem", input.display()))?;
    let mut name = stem.to_os_string();
    if let Some(tag) = tag {
        name.push("_");
        name.push(tag);
    }
    name.push(".jpg");
    let directory = output_dir.unwrap_or_else(|| input.parent().unwrap_or(Path::new("")));
    Ok(directory.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::common::test_utils::{sample_jpeg_default, sample_png};

    fn args(inputs: &[&Path], out: Option<PathBuf>, force: bool) -> ConvertArgs {
        ConvertArgs {
            inputs: inputs
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            out,
            force,
            long_edge: None,
            short_edge: None,
            orient: None,
            quality: None,
            max_size: None,
            render: false,
            viewport: None,
        }
    }

    #[test]
    fn refuses_in_place_conversion_without_force() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("photo.jpg");
        sample_jpeg_default(&input);
        let error = run(args(&[&input], None, false), &Config::default(), true).unwrap_err();
        assert!(format!("{error:#}").contains("--force"));
    }

    #[test]
    fn force_allows_in_place_conversion() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("photo.jpg");
        sample_jpeg_default(&input);
        run(args(&[&input], None, true), &Config::default(), false).unwrap();
        assert!(input.is_file());
    }

    #[test]
    fn max_size_keeps_each_jpeg_within_the_limit() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("noise.png");
        let mut seed = 11u32;
        image::RgbImage::from_fn(800, 600, |_, _| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let value = (seed >> 16) as u8;
            image::Rgb([value, value.wrapping_mul(7), value.wrapping_add(50)])
        })
        .save(&input)
        .unwrap();
        let mut arguments = args(&[&input], Some(directory.path().join("out")), false);
        arguments.max_size = Some(60_000);
        run(arguments, &Config::default(), true).unwrap();
        let written = fs::metadata(directory.path().join("out").join("noise.jpg")).unwrap();
        assert!(written.len() <= 60_000, "{}", written.len());
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
