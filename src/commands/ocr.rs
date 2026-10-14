use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde_json::json;

use super::common::{
    DOCUMENT_SEPARATOR, OutputRegistry, finish_batch, handle_results, write_output,
};
use crate::cli::OcrArgs;
use crate::config::Config;
use crate::fileset::{InputSpec, expand};
use crate::formats::{self, Format, InputFormatSet};
use crate::ocr::{self, OcrBackend, OcrEngine, OcrOptions};
use crate::{output, pdf};

pub fn run(args: OcrArgs, config: &Config, fail_fast: bool) -> Result<()> {
    if args.clear_cache {
        clear_cache(config)?;
        if args.inputs.is_empty() {
            return Ok(());
        }
    }

    let specs = expand(&args.inputs, InputFormatSet::Ocr)?;
    for spec in &specs {
        if formats::detect(&spec.path) != Some(Format::Pdf) {
            spec.reject_pages()?;
        }
    }
    let engine = OcrEngine::new(OcrOptions {
        backend: OcrBackend::parse(args.engine.as_deref().unwrap_or(&config.ocr_engine))?,
        lang: args.lang.or_else(|| config.ocr_lang.clone()),
        force_image_ocr: args.force_ocr,
        jobs: config.ocr_jobs.clamp(1, 64),
        cache_dir: (!args.no_cache && config.ocr_cache).then(|| config.ocr_cache_dir.clone()),
        ..OcrOptions::from_config(config)
    })?;

    if args.in_place {
        return searchable_in_place(&engine, &specs, config, fail_fast);
    }
    match &args.out {
        Some(path) if has_extension(path, "pdf") => {
            searchable_pdf(&engine, &specs, path, config, fail_fast)
        }
        out => markdown(&engine, &specs, out.as_deref(), fail_fast),
    }
}

fn clear_cache(config: &Config) -> Result<()> {
    let (removed, bytes) = ocr::clear_cache(&config.ocr_cache_dir)?;
    output::result(
        "clear_cache",
        format!(
            "Cleared OCR cache {}: {removed} file(s), {bytes} bytes",
            config.ocr_cache_dir.display()
        ),
        json!({
            "cache_dir": config.ocr_cache_dir.to_string_lossy(),
            "removed_files": removed,
            "bytes_freed": bytes,
        }),
    );
    Ok(())
}

/// Add a text layer to every source PDF. Pages of one file are recognised
/// in parallel; files follow one another.
fn searchable_in_place(
    engine: &OcrEngine,
    specs: &[InputSpec],
    config: &Config,
    fail_fast: bool,
) -> Result<()> {
    if let Some(spec) = specs
        .iter()
        .find(|spec| formats::detect(&spec.path) != Some(Format::Pdf))
    {
        bail!(
            "--in-place is only supported for PDF files, but found {}",
            spec.path.display()
        );
    }
    let results = specs.iter().map(|spec| {
        output::info(format!("OCR (in-place): {}", describe(spec)));
        let result = searchable(engine, spec, config)
            .and_then(|mut document| pdf::save_to_bytes(&mut document))
            .and_then(|bytes| write_output(&spec.path, &bytes));
        (&spec.path, result)
    });
    let failures = handle_results(
        results,
        fail_fast,
        "failed to process",
        "error processing",
        drop,
    )?;
    finish_batch("ocr", specs.len(), failures, specs.len() - failures)
}

/// One searchable PDF made of all inputs.
fn searchable_pdf(
    engine: &OcrEngine,
    specs: &[InputSpec],
    output_path: &Path,
    config: &Config,
    fail_fast: bool,
) -> Result<()> {
    let results = specs.iter().map(|spec| {
        output::info(format!("OCR: {}", describe(spec)));
        (&spec.path, searchable(engine, spec, config))
    });
    let mut documents = Vec::with_capacity(specs.len());
    let failures = handle_results(
        results,
        fail_fast,
        "failed to process",
        "error processing",
        |document| documents.push(document),
    )?;
    if documents.is_empty() {
        return finish_batch("ocr", specs.len(), failures, 0);
    }
    let mut document = pdf::merge_documents(documents)?;
    write_output(output_path, &pdf::save_to_bytes(&mut document)?)?;
    finish_batch("ocr", specs.len(), failures, 1)
}

/// Recognised text: combined into `output_path`, or one `.md` per input.
fn markdown(
    engine: &OcrEngine,
    specs: &[InputSpec],
    output_path: Option<&Path>,
    fail_fast: bool,
) -> Result<()> {
    for spec in specs {
        output::info(format!("Queued OCR: {}", describe(spec)));
    }
    let results: Box<dyn Iterator<Item = Result<String>>> = if fail_fast {
        Box::new(specs.iter().map(|spec| engine.extract_spec(spec)))
    } else {
        Box::new(engine.extract_many_specs(specs).into_iter())
    };
    let mut texts = Vec::with_capacity(specs.len());
    let failures = handle_results(
        specs
            .iter()
            .enumerate()
            .zip(results)
            .map(|((index, spec), result)| (&spec.path, result.map(|text| (index, text)))),
        fail_fast,
        "failed to process",
        "error processing",
        |text| texts.push(text),
    )?;

    let outputs = match output_path {
        Some(_) if texts.is_empty() => 0,
        Some(path) => {
            let combined = texts
                .into_iter()
                .map(|(_, text)| text)
                .collect::<Vec<_>>()
                .join(DOCUMENT_SEPARATOR);
            write_output(path, combined.as_bytes())?;
            1
        }
        None => {
            let paths = markdown_paths(specs);
            for (index, text) in &texts {
                write_output(&paths[*index], text.as_bytes())?;
            }
            texts.len()
        }
    };
    finish_batch("ocr", specs.len(), failures, outputs)
}

fn searchable(engine: &OcrEngine, spec: &InputSpec, config: &Config) -> Result<lopdf::Document> {
    engine.create_searchable_pdf_for_spec(spec, &config.page_size, config.font_path.as_deref())
}

/// `NAME.md` beside each input; inputs that would share a name (`a.pdf` and
/// `a.jpg`, or two ranges of one PDF) get `NAME_2.md`, ...
fn markdown_paths(specs: &[InputSpec]) -> Vec<PathBuf> {
    let mut registry = OutputRegistry::default();
    specs
        .iter()
        .map(|spec| registry.unique(spec.path.with_extension("md")))
        .collect()
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .is_some_and(|value| value.eq_ignore_ascii_case(extension))
}

fn describe(spec: &InputSpec) -> String {
    match &spec.pages {
        Some(pages) => format!("{}:{pages}", spec.path.display()),
        None => spec.path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(path: &str, pages: Option<&str>) -> InputSpec {
        InputSpec {
            path: PathBuf::from(path),
            pages: pages.map(str::to_owned),
        }
    }

    #[test]
    fn markdown_names_never_collide() {
        let paths = markdown_paths(&[
            spec("a.pdf", Some("1-3")),
            spec("a.pdf", Some("4-6")),
            spec("a.jpg", None),
            spec("b.png", None),
        ]);
        let names = paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, ["a.md", "a_2.md", "a_3.md", "b.md"]);
    }

    fn windows_ocr_args(inputs: Vec<String>, out: Option<PathBuf>, in_place: bool) -> OcrArgs {
        OcrArgs {
            inputs,
            out,
            in_place,
            engine: Some("windows".to_owned()),
            lang: None,
            force_ocr: in_place,
            no_cache: true,
            clear_cache: false,
        }
    }

    #[test]
    fn ocr_pdf_output_creates_searchable_pdf_on_windows() {
        if !cfg!(windows) || !crate::winocr::is_available() {
            return;
        }
        let temp_dir = tempfile::tempdir().unwrap();
        let image_path = temp_dir.path().join("scan.png");
        let output_pdf = temp_dir.path().join("searchable.pdf");
        image::RgbImage::from_pixel(100, 40, image::Rgb([255, 255, 255]))
            .save(&image_path)
            .unwrap();

        let args = windows_ocr_args(
            vec![image_path.to_str().unwrap().to_owned()],
            Some(output_pdf.clone()),
            false,
        );
        let result = run(args, &Config::default(), true);
        assert!(result.is_ok(), "OCR to PDF failed: {:?}", result.err());
        let doc = lopdf::Document::load(&output_pdf).unwrap();
        assert_eq!(doc.get_pages().len(), 1);
    }

    #[test]
    fn ocr_in_place_updates_source_pdf_on_windows() {
        if !cfg!(windows) || !crate::winocr::is_available() {
            return;
        }
        let temp_dir = tempfile::tempdir().unwrap();
        let input_pdf = temp_dir.path().join("source.pdf");
        let jpeg = crate::commands::common::test_utils::sample_jpeg_bytes(500, 500);
        let mut document = crate::pdf::jpeg_document(jpeg, "A4").unwrap();
        crate::pdf::transform::rotate_pages(&mut document, "all", 90).unwrap();
        document.save(&input_pdf).unwrap();

        let args = windows_ocr_args(vec![input_pdf.to_str().unwrap().to_owned()], None, true);
        let result = run(args, &Config::default(), true);
        assert!(result.is_ok(), "in-place OCR failed: {:?}", result.err());
        let doc = lopdf::Document::load(&input_pdf).unwrap();
        assert_eq!(doc.get_pages().len(), 1);
    }
}
