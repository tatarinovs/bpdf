use std::path::Path;

use anyhow::{Result, bail};

use super::common::PdfOrJpegEdit;
use crate::config::Config;
use crate::formats::InputFormatSet;
use crate::pdf::transform;

#[allow(clippy::too_many_arguments)]
pub fn run(
    inputs: &[String],
    size: &str,
    long_edge: Option<u32>,
    short_edge: Option<u32>,
    pages: &str,
    out: Option<&Path>,
    config: &Config,
    fail_fast: bool,
) -> Result<()> {
    if long_edge == Some(0) {
        bail!("--long-edge must be greater than zero");
    }
    if short_edge == Some(0) {
        bail!("--short-edge must be greater than zero");
    }
    let mut image_options = config.image_options(None, None);
    image_options.long_edge = long_edge;
    image_options.short_edge = short_edge;
    image_options.force_reencode = true;

    PdfOrJpegEdit {
        operation: "resize",
        verb: "Resizing",
        formats: InputFormatSet::Resize,
        pages,
        out,
        image_options,
        // Explicit edge limits replace fitting to the paper size.
        jpeg_page_size: (long_edge.is_none() && short_edge.is_none()).then_some(size),
        edit_pdf: |document: &mut lopdf::Document, pages: &str| {
            transform::resize_pages(document, size, pages)
        },
    }
    .run(inputs, fail_fast)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::common::test_utils::*;
    use crate::pdf;
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
            "A4",
            None,
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
    fn refuses_zero_long_edge() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("sample.pdf");
        sample_pdf(&pdf);

        let error = run(
            &[pdf.to_string_lossy().into_owned()],
            "A4",
            Some(0),
            None,
            "all",
            None,
            &Config::default(),
            true,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("--long-edge must be greater than zero"));
    }

    #[test]
    fn refuses_zero_short_edge() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("sample.pdf");
        sample_pdf(&pdf);

        let error = run(
            &[pdf.to_string_lossy().into_owned()],
            "A4",
            None,
            Some(0),
            "all",
            None,
            &Config::default(),
            true,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("--short-edge must be greater than zero"));
    }

    #[test]
    fn resizes_jpeg_file_with_long_edge() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("photo.jpg");
        let output = dir.path().join("resized.jpg");
        sample_jpeg(&input, 800, 400);

        run(
            &[input.to_string_lossy().into_owned()],
            "A4",
            Some(400),
            None,
            "all",
            Some(&output),
            &Config::default(),
            true,
        )
        .unwrap();

        assert!(output.is_file());
        let img = image::open(&output).unwrap();
        assert_eq!(img.dimensions(), (400, 200));
    }

    #[test]
    fn resizes_jpeg_file_with_short_edge() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("photo.jpg");
        let output = dir.path().join("resized.jpg");
        sample_jpeg(&input, 800, 400);

        run(
            &[input.to_string_lossy().into_owned()],
            "A4",
            None,
            Some(200),
            "all",
            Some(&output),
            &Config::default(),
            true,
        )
        .unwrap();

        assert!(output.is_file());
        let img = image::open(&output).unwrap();
        assert_eq!(img.dimensions(), (400, 200));
    }

    #[test]
    fn resizes_jpeg_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("photo.jpg");
        sample_jpeg(&input, 800, 400);

        run(
            &[input.to_string_lossy().into_owned()],
            "A4",
            Some(200),
            None,
            "all",
            None,
            &Config::default(),
            true,
        )
        .unwrap();

        let img = image::open(&input).unwrap();
        assert_eq!(img.dimensions(), (200, 100));
    }

    #[test]
    fn refuses_page_ranges_on_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("photo.jpg");
        sample_jpeg(&input, 800, 400);

        let error = run(
            &[input.to_string_lossy().into_owned()],
            "A4",
            Some(400),
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
            "A4",
            Some(400),
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
        sample_jpeg(&jpeg, 800, 400);
        sample_pdf(&pdf);
        sample_png(&png);
        fs::write(&txt, "hello world").unwrap();

        run(
            &[dir.path().to_string_lossy().into_owned()],
            "A4",
            Some(400),
            None,
            "all",
            None,
            &Config::default(),
            true,
        )
        .unwrap();

        // JPEG was resized
        let img = image::open(&jpeg).unwrap();
        assert_eq!(img.dimensions(), (400, 200));

        // PDF was resized to A4
        let doc = pdf::load(&pdf).unwrap();
        let page_id = *doc.get_pages().get(&1).unwrap();
        let geom = transform::page_geometry(&doc, page_id).unwrap();
        assert!((geom.display_height() - 841.89).abs() < 1.0);

        // PNG was left untouched
        let png_img = image::open(&png).unwrap();
        assert_eq!(png_img.dimensions(), (2, 2));

        // TXT was left untouched
        assert_eq!(fs::read_to_string(&txt).unwrap(), "hello world");
    }

    #[test]
    fn resizes_pdf_file() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("sample.pdf");
        let out = dir.path().join("resized.pdf");
        sample_pdf(&pdf);

        run(
            &[pdf.to_string_lossy().into_owned()],
            "Letter",
            None,
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
        assert!((geom.display_width() - 612.0).abs() < 1.0);
        assert!((geom.display_height() - 792.0).abs() < 1.0);
    }
}
