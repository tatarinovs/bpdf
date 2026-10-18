use std::fs;
use std::path::Path;

use anyhow::Result;

use super::common::{batch, edited_outputs, finish_batch, handle_results, write_output};
use crate::cli::StripArgs;
use crate::config::Config;
use crate::fileset::{InputSpec, expand};
use crate::formats::{self, Format, InputFormatSet};
use crate::{metadata, output, pdf};

pub fn run(args: StripArgs, config: &Config, fail_fast: bool) -> Result<()> {
    let specs = expand(&args.inputs, InputFormatSet::Strip)?;
    let destinations = edited_outputs(&args.output, &specs, "stripped")?;
    let jobs = specs.iter().zip(&destinations).collect::<Vec<_>>();
    let mut outputs = 0usize;
    let results = batch(&jobs, fail_fast, |(spec, destination)| {
        (&spec.path, strip_one(spec, destination, config.keep_icc))
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

fn strip_one(spec: &InputSpec, destination: &Path, keep_icc: bool) -> Result<bool> {
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
    write_output(destination, &bytes)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::cli::OutputArgs;
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
    fn out_directory_holding_the_source_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("sample.png");
        sample_png(&png);
        let before = fs::read(&png).unwrap();
        let error = run(
            args(&[&png], Some(dir.path().to_path_buf())),
            &Config::default(),
            true,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("--in-place"), "{error:#}");
        assert_eq!(fs::read(&png).unwrap(), before);
    }

    #[test]
    fn inputs_sharing_a_destination_are_refused_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let (first, second) = (dir.path().join("a"), dir.path().join("b"));
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        sample_png(&first.join("x.png"));
        sample_png(&second.join("x.png"));
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        let error = run(
            args(
                &[&first.join("x.png"), &second.join("x.png")],
                Some(out.clone()),
            ),
            &Config::default(),
            false,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("several inputs"), "{error:#}");
        assert!(!out.join("x.png").exists());
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
