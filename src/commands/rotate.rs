use std::path::Path;

use anyhow::{Result, bail};

use super::common::{
    err_pdf_only_page_ranges, finish_batch, handle_results, resolve_in_place_output,
    validate_single_out, write_output,
};
use crate::config::Config;
use crate::fileset::{InputSpec, expand};
use crate::formats::{self, Format, InputFormatSet};
use crate::imageconv::{self, ImageOptions};
use crate::pdf::{self, transform};

#[derive(Debug, Clone, Copy)]
pub enum RotationMode<'a> {
    Degrees(i64),
    Orient(&'a str),
}

pub fn run(
    inputs: &[String],
    degrees: Option<i64>,
    orient: Option<&str>,
    pages: &str,
    out: Option<&Path>,
    config: &Config,
    fail_fast: bool,
) -> Result<()> {
    let mode = match (degrees, orient) {
        (Some(deg), None) => {
            if deg % 90 != 0 {
                bail!("rotation must be a multiple of 90 degrees");
            }
            RotationMode::Degrees(deg)
        }
        (None, Some(orient)) => match orient.to_lowercase().as_str() {
            "portrait" | "landscape" => RotationMode::Orient(orient),
            _ => bail!("invalid orientation '{orient}': expected 'portrait' or 'landscape'"),
        },
        (Some(_), Some(_)) => bail!("cannot specify both degrees and --orient"),
        (None, None) => bail!("either degrees or --orient must be specified"),
    };

    let specs = expand(inputs, InputFormatSet::Rotate)?;
    validate_single_out(out, specs.len())?;

    let mut image_options = config.image_options(None, None);
    match mode {
        RotationMode::Degrees(deg) => image_options.rotation_degrees = Some(deg),
        RotationMode::Orient(orient) => image_options.orient = Some(orient.to_string()),
    }
    image_options.force_reencode = true;

    let results = specs.iter().map(|spec| {
        (
            &spec.path,
            rotate_one(spec, mode, pages, out, &image_options),
        )
    });
    let failures = handle_results(
        results,
        fail_fast,
        "failed to process",
        "error processing",
        drop,
    )?;
    finish_batch("rotate", specs.len(), failures, specs.len() - failures)
}

fn rotate_one(
    spec: &InputSpec,
    mode: RotationMode<'_>,
    pages: &str,
    explicit_out: Option<&Path>,
    image_options: &ImageOptions,
) -> Result<()> {
    let input = &spec.path;
    let format = formats::detect(input);
    let output_path = resolve_in_place_output(input, explicit_out, "Rotating");

    match format {
        Some(Format::Pdf) => {
            let pages = spec.pages.as_deref().unwrap_or(pages);
            let bytes = pdf::transform_file(input, |document| match mode {
                RotationMode::Degrees(deg) => transform::rotate_pages(document, pages, deg),
                RotationMode::Orient(orient) => transform::orient_pages(document, pages, orient),
            })?;
            write_output(&output_path, &bytes)?;
        }
        Some(Format::Jpeg) => {
            if spec.pages.is_some() || pages != "all" {
                return Err(err_pdf_only_page_ranges(input));
            }

            let bytes = imageconv::to_jpeg(input, image_options, None)?;
            write_output(&output_path, &bytes)?;
        }
        _ => bail!("unsupported input: {}", input.display()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::common::test_utils::*;
    use image::GenericImageView;
    use std::fs;

    #[test]
    fn refuses_multiple_inputs_with_explicit_out() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.png");
        let second = dir.path().join("second.png");
        sample_png(&first);
        sample_png(&second);
        let out = dir.path().join("out.jpg");

        let error = run(
            &[
                first.to_string_lossy().into_owned(),
                second.to_string_lossy().into_owned(),
            ],
            Some(90),
            None,
            "all",
            Some(&out),
            &Config::default(),
            true,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("--out is only valid with one input file"));
    }

    #[test]
    fn refuses_invalid_rotation_degrees() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("sample.pdf");
        sample_pdf(&pdf);

        let error = run(
            &[pdf.to_string_lossy().into_owned()],
            Some(45),
            None,
            "all",
            None,
            &Config::default(),
            true,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("rotation must be a multiple of 90 degrees"));
    }

    #[test]
    fn refuses_invalid_orientation() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("sample.pdf");
        sample_pdf(&pdf);

        let error = run(
            &[pdf.to_string_lossy().into_owned()],
            None,
            Some("diagonal"),
            "all",
            None,
            &Config::default(),
            true,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("invalid orientation 'diagonal'"));
    }

    #[test]
    fn rotates_jpeg_file_and_updates_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("photo.jpg");
        let output = dir.path().join("rotated.jpg");
        sample_jpeg(&input, 200, 100);

        run(
            &[input.to_string_lossy().into_owned()],
            Some(90),
            None,
            "all",
            Some(&output),
            &Config::default(),
            true,
        )
        .unwrap();

        assert!(output.is_file());
        let img = image::open(&output).unwrap();
        assert_eq!(img.dimensions(), (100, 200));
    }

    #[test]
    fn rotates_jpeg_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("photo.jpg");
        sample_jpeg(&input, 200, 100);

        run(
            &[input.to_string_lossy().into_owned()],
            Some(90),
            None,
            "all",
            None,
            &Config::default(),
            true,
        )
        .unwrap();

        let img = image::open(&input).unwrap();
        assert_eq!(img.dimensions(), (100, 200));
    }

    #[test]
    fn orients_jpeg_to_portrait_and_landscape() {
        let dir = tempfile::tempdir().unwrap();
        let wide = dir.path().join("wide.jpg");
        sample_jpeg(&wide, 200, 100);

        // Wide image to portrait -> rotated to 100x200
        run(
            &[wide.to_string_lossy().into_owned()],
            None,
            Some("portrait"),
            "all",
            None,
            &Config::default(),
            true,
        )
        .unwrap();

        let img = image::open(&wide).unwrap();
        assert_eq!(img.dimensions(), (100, 200));

        // Now tall image to landscape -> rotated back to 200x100
        run(
            &[wide.to_string_lossy().into_owned()],
            None,
            Some("landscape"),
            "all",
            None,
            &Config::default(),
            true,
        )
        .unwrap();

        let img2 = image::open(&wide).unwrap();
        assert_eq!(img2.dimensions(), (200, 100));
    }

    #[test]
    fn refuses_page_ranges_on_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("photo.jpg");
        sample_jpeg(&input, 200, 100);

        let error = run(
            &[input.to_string_lossy().into_owned()],
            Some(90),
            None,
            "1-2",
            None,
            &Config::default(),
            true,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("page ranges are only valid for PDF inputs"));
    }

    #[test]
    fn refuses_unsupported_format_direct_input() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("photo.png");
        sample_png(&input);

        let error = run(
            &[input.to_string_lossy().into_owned()],
            Some(90),
            None,
            "all",
            None,
            &Config::default(),
            true,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("unsupported input"));
    }

    #[test]
    fn ignores_unsupported_formats_in_directory() {
        let dir = tempfile::tempdir().unwrap();
        let jpeg = dir.path().join("photo.jpg");
        let pdf = dir.path().join("document.pdf");
        let png = dir.path().join("ignored.png");
        let txt = dir.path().join("ignored.txt");
        sample_jpeg(&jpeg, 200, 100);
        sample_pdf(&pdf);
        sample_png(&png);
        fs::write(&txt, "hello world").unwrap();

        run(
            &[dir.path().to_string_lossy().into_owned()],
            Some(90),
            None,
            "all",
            None,
            &Config::default(),
            true,
        )
        .unwrap();

        // JPEG was rotated
        let img = image::open(&jpeg).unwrap();
        assert_eq!(img.dimensions(), (100, 200));

        // PNG was left untouched
        let png_img = image::open(&png).unwrap();
        assert_eq!(png_img.dimensions(), (2, 2));

        // TXT was left untouched
        assert_eq!(fs::read_to_string(&txt).unwrap(), "hello world");
    }

    #[test]
    fn rotates_pdf_file() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("sample.pdf");
        let out = dir.path().join("rotated.pdf");
        sample_pdf(&pdf);

        run(
            &[pdf.to_string_lossy().into_owned()],
            Some(90),
            None,
            "all",
            Some(&out),
            &Config::default(),
            true,
        )
        .unwrap();

        assert!(out.is_file());
        let doc = pdf::load(&out).unwrap();
        let page_id = *doc.get_pages().get(&1).unwrap();
        let geom = transform::page_geometry(&doc, page_id).unwrap();
        assert_eq!(geom.rotation, 90);
    }

    #[test]
    fn orients_pdf_file() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("sample.pdf");
        sample_pdf(&pdf); // A4 portrait by default in sample_pdf (595x842)

        // Orient to landscape
        run(
            &[pdf.to_string_lossy().into_owned()],
            None,
            Some("landscape"),
            "all",
            None,
            &Config::default(),
            true,
        )
        .unwrap();

        let doc = pdf::load(&pdf).unwrap();
        let page_id = *doc.get_pages().get(&1).unwrap();
        let geom = transform::page_geometry(&doc, page_id).unwrap();
        assert_eq!(geom.rotation, 90);
        assert!(geom.display_width() > geom.display_height());
    }
}
