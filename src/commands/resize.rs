use anyhow::Result;

use super::common::PdfOrJpegEdit;
use crate::cli::ResizeArgs;
use crate::config::Config;
use crate::formats::InputFormatSet;
use crate::pdf::transform;

pub fn run(args: ResizeArgs, config: &Config, fail_fast: bool) -> Result<()> {
    // Paper sizes apply to PDF pages only; a JPEG needs explicit pixel edges.
    let jpeg = if args.long_edge.is_some() || args.short_edge.is_some() {
        let mut options = config.image_options();
        options.long_edge = args.long_edge;
        options.short_edge = args.short_edge;
        Ok(options)
    } else {
        Err("resizing a JPEG needs --long-edge or --short-edge")
    };

    PdfOrJpegEdit {
        operation: "resize",
        suffix: "resized",
        formats: InputFormatSet::Resize,
        pages: &args.pages,
        output: &args.output,
        jpeg,
        edit_pdf: |document: &mut lopdf::Document, pages: &str| {
            transform::resize_pages(document, &args.size, pages)
        },
    }
    .run(&args.inputs, fail_fast)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use image::GenericImageView;

    use super::*;
    use crate::cli::OutputArgs;
    use crate::commands::common::test_utils::*;
    use crate::pdf;

    fn resize(input: &Path, size: &str, edges: (Option<u32>, Option<u32>)) -> Result<()> {
        run(
            ResizeArgs {
                inputs: vec![input.to_string_lossy().into_owned()],
                size: size.to_owned(),
                long_edge: edges.0,
                short_edge: edges.1,
                pages: "all".to_owned(),
                output: OutputArgs {
                    out: None,
                    in_place: true,
                },
            },
            &Config::default(),
            true,
        )
    }

    #[test]
    fn resizes_jpeg_by_long_or_short_edge() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("photo.jpg");
        sample_jpeg(&input, 800, 400);
        resize(&input, "A4", (Some(400), None)).unwrap();
        assert_eq!(image::open(&input).unwrap().dimensions(), (400, 200));
        resize(&input, "A4", (None, Some(100))).unwrap();
        assert_eq!(image::open(&input).unwrap().dimensions(), (200, 100));
    }

    #[test]
    fn jpeg_without_edges_is_refused_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("photo.jpg");
        sample_jpeg(&input, 800, 400);
        let error = resize(&input, "A4", (None, None)).unwrap_err();
        assert!(format!("{error:#}").contains("--long-edge"));
        assert_eq!(image::open(&input).unwrap().dimensions(), (800, 400));
    }

    #[test]
    fn resizes_pdf_pages_to_paper() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("sample.pdf");
        sample_pdf(&input);
        resize(&input, "Letter", (None, None)).unwrap();
        let document = pdf::load(&input).unwrap();
        let page_id = *document.get_pages().get(&1).unwrap();
        let geometry = transform::page_geometry(&document, page_id).unwrap();
        assert!((geometry.display_width() - 612.0).abs() < 1.0);
        assert!((geometry.display_height() - 792.0).abs() < 1.0);
    }
}
