use anyhow::{Result, bail};

use super::common::PdfOrJpegEdit;
use crate::cli::RotateArgs;
use crate::config::Config;
use crate::formats::InputFormatSet;
use crate::imageconv::Orientation;
use crate::pdf::transform;

/// A rotation by a multiple of 90 degrees or to a target orientation.
#[derive(Clone, Copy, Debug)]
enum Turn {
    By(i64),
    To(Orientation),
}

impl Turn {
    fn parse(value: &str) -> Result<Self> {
        if let Ok(degrees) = value.trim().parse::<i64>() {
            if degrees % 90 != 0 {
                bail!("rotation must be a multiple of 90 degrees");
            }
            return Ok(Self::By(degrees));
        }
        Orientation::parse(value).map(Self::To)
    }
}

pub fn run(args: RotateArgs, config: &Config, fail_fast: bool) -> Result<()> {
    let turn = Turn::parse(&args.angle)?;
    let mut image_options = config.image_options();
    image_options.force_reencode = true;
    match turn {
        Turn::By(degrees) => image_options.rotation_degrees = Some(degrees),
        Turn::To(orientation) => image_options.orient = Some(orientation),
    }

    PdfOrJpegEdit {
        operation: "rotate",
        suffix: "rotated",
        formats: InputFormatSet::Rotate,
        pages: &args.pages,
        output: &args.output,
        jpeg: Ok(image_options),
        edit_pdf: |document: &mut lopdf::Document, pages: &str| match turn {
            Turn::By(degrees) => transform::rotate_pages(document, pages, degrees),
            Turn::To(orientation) => transform::orient_pages(document, pages, orientation),
        },
    }
    .run(&args.inputs, fail_fast)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use image::GenericImageView;

    use super::*;
    use crate::cli::OutputArgs;
    use crate::commands::common::test_utils::*;
    use crate::pdf;

    fn rotate(inputs: &[&Path], angle: &str, pages: &str, out: Option<PathBuf>) -> Result<()> {
        run(
            RotateArgs {
                inputs: inputs
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect(),
                angle: angle.to_owned(),
                pages: pages.to_owned(),
                output: OutputArgs {
                    in_place: out.is_none(),
                    out,
                },
            },
            &Config::default(),
            true,
        )
    }

    #[test]
    fn refuses_several_inputs_with_an_output_file() {
        let dir = tempfile::tempdir().unwrap();
        let (first, second) = (dir.path().join("a.jpg"), dir.path().join("b.jpg"));
        sample_jpeg(&first, 4, 2);
        sample_jpeg(&second, 4, 2);
        let error = rotate(
            &[&first, &second],
            "90",
            "all",
            Some(dir.path().join("x.jpg")),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("existing directory"));
    }

    #[test]
    fn several_inputs_go_into_an_output_directory() {
        let dir = tempfile::tempdir().unwrap();
        let (first, second) = (dir.path().join("a.jpg"), dir.path().join("b.jpg"));
        sample_jpeg(&first, 4, 2);
        sample_jpeg(&second, 4, 2);
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        rotate(&[&first, &second], "90", "all", Some(out.clone())).unwrap();
        assert_eq!(image::open(out.join("b.jpg")).unwrap().dimensions(), (2, 4));
    }

    #[test]
    fn default_output_gets_a_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("photo.jpg");
        sample_jpeg(&input, 4, 2);
        run(
            RotateArgs {
                inputs: vec![input.to_string_lossy().into_owned()],
                angle: "90".to_owned(),
                pages: "all".to_owned(),
                output: OutputArgs {
                    out: None,
                    in_place: false,
                },
            },
            &Config::default(),
            true,
        )
        .unwrap();
        assert_eq!(image::open(&input).unwrap().dimensions(), (4, 2));
        let rotated = dir.path().join("photo_rotated.jpg");
        assert_eq!(image::open(rotated).unwrap().dimensions(), (2, 4));
    }

    #[test]
    fn refuses_invalid_angles() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("sample.pdf");
        sample_pdf(&pdf);
        let error = rotate(&[&pdf], "45", "all", None).unwrap_err();
        assert!(format!("{error:#}").contains("multiple of 90"));
        let error = rotate(&[&pdf], "diagonal", "all", None).unwrap_err();
        assert!(format!("{error:#}").contains("invalid orientation 'diagonal'"));
    }

    #[test]
    fn rotates_and_orients_jpeg_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("photo.jpg");
        sample_jpeg(&input, 200, 100);
        rotate(&[&input], "-90", "all", None).unwrap();
        assert_eq!(image::open(&input).unwrap().dimensions(), (100, 200));
        rotate(&[&input], "portrait", "all", None).unwrap();
        assert_eq!(image::open(&input).unwrap().dimensions(), (100, 200));
        rotate(&[&input], "landscape", "all", None).unwrap();
        assert_eq!(image::open(&input).unwrap().dimensions(), (200, 100));
    }

    #[test]
    fn refuses_pages_on_jpeg_and_unsupported_formats() {
        let dir = tempfile::tempdir().unwrap();
        let jpeg = dir.path().join("photo.jpg");
        let png = dir.path().join("photo.png");
        sample_jpeg(&jpeg, 4, 2);
        sample_png(&png);
        let error = rotate(&[&jpeg], "90", "1-2", None).unwrap_err();
        assert!(format!("{error:#}").contains("only valid for PDF inputs"));
        let error = rotate(&[&png], "90", "all", None).unwrap_err();
        assert!(format!("{error:#}").contains("unsupported input"));
    }

    #[test]
    fn directory_input_skips_unsupported_formats() {
        let dir = tempfile::tempdir().unwrap();
        let jpeg = dir.path().join("photo.jpg");
        let txt = dir.path().join("ignored.txt");
        sample_jpeg(&jpeg, 200, 100);
        sample_pdf(&dir.path().join("document.pdf"));
        fs::write(&txt, "hello world").unwrap();
        rotate(&[dir.path()], "90", "all", None).unwrap();
        assert_eq!(image::open(&jpeg).unwrap().dimensions(), (100, 200));
        assert_eq!(fs::read_to_string(&txt).unwrap(), "hello world");
    }

    #[test]
    fn rotates_and_orients_pdf() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("sample.pdf");
        sample_pdf(&pdf);
        let rotation = |path: &Path| {
            let document = pdf::load(path).unwrap();
            let page_id = *document.get_pages().get(&1).unwrap();
            transform::page_geometry(&document, page_id).unwrap()
        };
        let out = dir.path().join("rotated.pdf");
        rotate(&[&pdf], "90", "all", Some(out.clone())).unwrap();
        assert_eq!(rotation(&out).rotation, 90);
        rotate(&[&pdf], "landscape", "all", None).unwrap();
        assert!(rotation(&pdf).is_landscape());
    }
}
