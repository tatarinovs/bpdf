use std::fs;

use anyhow::Result;

use super::common::{
    batch, edited_output, finish_batch, handle_results, validate_output, write_output,
};
use crate::cli::{OutputArgs, StripArgs};
use crate::config::Config;
use crate::fileset::{InputSpec, expand};
use crate::formats::{self, Format, InputFormatSet};
use crate::{metadata, output, pdf};

pub fn run(args: StripArgs, config: &Config, fail_fast: bool) -> Result<()> {
    let specs = expand(&args.inputs, InputFormatSet::Strip)?;
    validate_output(&args.output, specs.len())?;
    let mut outputs = 0usize;
    let results = batch(&specs, fail_fast, |spec| {
        (&spec.path, strip_one(spec, &args.output, config.keep_icc))
    });
    let failures = handle_results(
        results,
        fail_fast,
        "failed to process",
        "error processing",
        |stripped| outputs += usize::from(stripped),
    )?;
    finish_batch("strip", specs.len(), failures, outputs)
}

fn strip_one(spec: &InputSpec, output: &OutputArgs, keep_icc: bool) -> Result<bool> {
    if spec.pages.is_some() {
        anyhow::bail!("page ranges are not valid for strip");
    }
    let input = &spec.path;
    let bytes = match formats::detect(input) {
        Some(Format::Jpeg) => metadata::strip_jpeg(&fs::read(input)?, keep_icc)?,
        Some(Format::Png) => metadata::strip_png(&fs::read(input)?, keep_icc)?,
        Some(Format::Pdf) => pdf::transform_file(input, |document| {
            pdf::strip_document_metadata(document);
            Ok(())
        })?,
        _ => {
            output::info(format!("Skipping unsupported file: {}", input.display()));
            return Ok(false);
        }
    };
    write_output(&edited_output(input, output, "stripped"), &bytes)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::commands::common::test_utils::{
        sample_jpeg_default as sample_jpeg, sample_pdf, sample_png,
    };

    fn args(inputs: &[&Path], out: Option<PathBuf>) -> StripArgs {
        StripArgs {
            inputs: inputs
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            output: OutputArgs {
                in_place: out.is_none(),
                out,
            },
        }
    }

    #[test]
    fn refuses_several_inputs_with_an_output_file() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.png");
        let second = dir.path().join("second.png");
        sample_png(&first);
        sample_png(&second);
        let error = run(
            args(&[&first, &second], Some(dir.path().join("out.png"))),
            &Config::default(),
            true,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("existing directory"));
    }

    #[test]
    fn strips_jpeg_png_and_pdf_and_skips_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let jpg = dir.path().join("sample.jpg");
        let png = dir.path().join("sample.png");
        let pdf = dir.path().join("sample.pdf");
        let txt = dir.path().join("notes.txt");
        sample_jpeg(&jpg);
        sample_png(&png);
        sample_pdf(&pdf);
        fs::write(&txt, "hello world").unwrap();

        run(
            args(&[&jpg, &png, &pdf, &txt], None),
            &Config::default(),
            true,
        )
        .unwrap();
        assert!(image::open(&jpg).is_ok());
        assert!(image::open(&png).is_ok());
        assert!(pdf::load(&pdf).is_ok());
        assert_eq!(fs::read_to_string(&txt).unwrap(), "hello world");
    }

    #[test]
    fn refuses_page_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("sample.pdf");
        sample_pdf(&pdf);
        let mut arguments = args(&[], None);
        arguments.inputs = vec![format!("{}:1-2", pdf.to_string_lossy())];
        let error = run(arguments, &Config::default(), true).unwrap_err();
        assert!(format!("{error:#}").contains("page ranges are not valid for strip"));
    }
}
