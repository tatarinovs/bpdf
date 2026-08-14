use std::fs;
use std::path::Path;

use anyhow::{Result, bail};

use super::common::{finish_batch, handle_results, same_path, write_output};
use crate::cli::StripArgs;
use crate::config::Config;
use crate::fileset::{InputSpec, expand};
use crate::formats::{self, Format, InputFormatSet};
use crate::imageconv::{self, ImageOptions};
use crate::{metadata, output, pdf};

pub fn run(args: StripArgs, config: &Config, fail_fast: bool) -> Result<()> {
    let specs = expand(&args.inputs, InputFormatSet::Strip)?;
    if args.out.is_some() && specs.len() != 1 {
        bail!("--out is only valid with one input file");
    }
    let options = config.image_options(args.keep_icc, args.ffmpeg);
    let results = specs
        .iter()
        .map(|spec| (&spec.path, strip_one(spec, args.out.as_deref(), &options)));
    let failures = handle_results(
        results,
        fail_fast,
        "failed to process",
        "error processing",
        drop,
    )?;
    finish_batch("strip", specs.len(), failures, specs.len() - failures)
}

fn strip_one(spec: &InputSpec, explicit_out: Option<&Path>, options: &ImageOptions) -> Result<()> {
    if spec.pages.is_some() {
        bail!("page ranges are not valid for strip");
    }
    let input = &spec.path;
    let format = formats::detect(input);
    let output_path = explicit_out.map(Path::to_path_buf).unwrap_or_else(|| {
        if format.is_some_and(Format::requires_jpeg_conversion) {
            input.with_extension("jpg")
        } else {
            input.clone()
        }
    });
    if same_path(input, &output_path) {
        output::info(format!("Stripping metadata in-place: {}", input.display()));
    }
    let bytes = match format {
        Some(Format::FfmpegRaster) => imageconv::ffmpeg_to_jpeg(input, options)?,
        Some(Format::WicRaster | Format::CameraRaw) => imageconv::to_jpeg(input, options, None)?,
        Some(Format::Jpeg) => metadata::strip_jpeg(&fs::read(input)?, options.keep_icc)?,
        Some(Format::Png) => metadata::strip_png(&fs::read(input)?, options.keep_icc)?,
        Some(Format::Pdf) => pdf::transform_file(input, |document| {
            pdf::strip_document_metadata(document);
            Ok(())
        })?,
        _ => bail!("unsupported strip format: {}", input.display()),
    };
    write_output(&output_path, &bytes)
}
