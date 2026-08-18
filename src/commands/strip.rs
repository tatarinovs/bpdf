use std::fs;
use std::path::Path;

use anyhow::{Result, bail};

use super::common::{
    finish_batch, handle_results, resolve_in_place_output, validate_single_out, write_output,
};
use crate::cli::StripArgs;
use crate::config::Config;
use crate::fileset::{InputSpec, expand};
use crate::formats::{self, Format, InputFormatSet};
use crate::imageconv::ImageOptions;
use crate::{metadata, output, pdf};

pub fn run(args: StripArgs, config: &Config, fail_fast: bool) -> Result<()> {
    let specs = expand(&args.inputs, InputFormatSet::Strip)?;
    validate_single_out(args.out.as_deref(), specs.len())?;
    let options = config.image_options(args.keep_icc, args.ffmpeg);
    let mut outputs = 0usize;
    let results = specs
        .iter()
        .map(|spec| (&spec.path, strip_one(spec, args.out.as_deref(), &options)));
    let failures = handle_results(
        results,
        fail_fast,
        "failed to process",
        "error processing",
        |stripped| {
            if stripped {
                outputs += 1;
            }
        },
    )?;
    finish_batch("strip", specs.len(), failures, outputs)
}

fn strip_one(
    spec: &InputSpec,
    explicit_out: Option<&Path>,
    options: &ImageOptions,
) -> Result<bool> {
    if spec.pages.is_some() {
        bail!("page ranges are not valid for strip");
    }
    let input = &spec.path;
    let format = formats::detect(input);
    let output_path = resolve_in_place_output(input, explicit_out, "Stripping metadata");

    let bytes = match format {
        Some(Format::Jpeg) => metadata::strip_jpeg(&fs::read(input)?, options.keep_icc)?,
        Some(Format::Png) => metadata::strip_png(&fs::read(input)?, options.keep_icc)?,
        Some(Format::Pdf) => pdf::transform_file(input, |document| {
            pdf::strip_document_metadata(document);
            Ok(())
        })?,
        _ => {
            output::info(format!("Skipping unsupported file: {}", input.display()));
            return Ok(false);
        }
    };

    write_output(&output_path, &bytes)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::common::test_utils::{
        sample_jpeg_default as sample_jpeg, sample_pdf, sample_png,
    };

    fn args(inputs: &[&Path], out: Option<std::path::PathBuf>) -> StripArgs {
        StripArgs {
            inputs: inputs
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            out,
            keep_icc: None,
            ffmpeg: None,
        }
    }

    #[test]
    fn refuses_multiple_inputs_with_explicit_out() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.png");
        let second = dir.path().join("second.png");
        sample_png(&first);
        sample_png(&second);
        let out = dir.path().join("out.png");

        let error = run(
            args(&[&first, &second], Some(out)),
            &Config::default(),
            true,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("--out is only valid with one input file"));
    }

    #[test]
    fn strips_jpeg_and_png_and_pdf() {
        let dir = tempfile::tempdir().unwrap();
        let jpg = dir.path().join("sample.jpg");
        let png = dir.path().join("sample.png");
        let pdf = dir.path().join("sample.pdf");
        sample_jpeg(&jpg);
        sample_png(&png);
        sample_pdf(&pdf);

        run(args(&[&jpg, &png, &pdf], None), &Config::default(), true).unwrap();
        assert!(jpg.is_file());
        assert!(png.is_file());
        assert!(pdf.is_file());
    }

    #[test]
    fn skips_unsupported_formats() {
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("notes.txt");
        let docx = dir.path().join("document.docx");
        fs::write(&txt, "hello world").unwrap();
        fs::write(&docx, "not actually a docx").unwrap();

        run(args(&[&txt, &docx], None), &Config::default(), true).unwrap();

        // Files were untouched
        assert_eq!(fs::read_to_string(&txt).unwrap(), "hello world");
        assert_eq!(fs::read_to_string(&docx).unwrap(), "not actually a docx");
    }

    #[test]
    fn skips_unsupported_format_mixed_with_supported() {
        let dir = tempfile::tempdir().unwrap();
        let jpg = dir.path().join("sample.jpg");
        let txt = dir.path().join("notes.txt");
        sample_jpeg(&jpg);
        fs::write(&txt, "hello world").unwrap();

        run(args(&[&jpg, &txt], None), &Config::default(), true).unwrap();

        assert!(jpg.is_file());
        assert_eq!(fs::read_to_string(&txt).unwrap(), "hello world");
    }

    #[test]
    fn refuses_page_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("sample.pdf");
        sample_pdf(&pdf);

        let error = run(
            StripArgs {
                inputs: vec![format!("{}:1-2", pdf.to_string_lossy())],
                out: None,
                keep_icc: None,
                ffmpeg: None,
            },
            &Config::default(),
            true,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("page ranges are not valid for strip"));
    }
}
