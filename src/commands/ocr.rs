use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::common::{DOCUMENT_SEPARATOR, finish_batch, write_output};
use crate::cli::OcrArgs;
use crate::config::Config;
use crate::fileset::{InputSpec, expand};
use crate::formats::{self, Format, InputFormatSet};
use crate::ocr::{OcrBackend, OcrEngine, OcrOptions};
use crate::output;

pub fn run(args: OcrArgs, config: &Config, fail_fast: bool) -> Result<()> {
    let specs = expand(&args.inputs, InputFormatSet::Ocr)?;
    for spec in &specs {
        if spec.pages.is_some() && formats::detect(&spec.path) != Some(Format::Pdf) {
            bail!(
                "page ranges are only valid for PDF inputs: {}",
                spec.path.display()
            );
        }
    }

    let jobs = args.jobs.unwrap_or(config.ocr_jobs);
    if !(1..=64).contains(&jobs) {
        bail!("--jobs must be between 1 and 64");
    }
    let cache_dir = (!args.no_cache && config.ocr_cache).then(|| {
        args.cache_dir
            .clone()
            .unwrap_or_else(|| config.ocr_cache_dir.clone())
    });

    let engine_name = args.engine.as_deref().unwrap_or(&config.ocr_engine);
    let backend = OcrBackend::parse(engine_name, &config.groq_api_key)?;
    let lang = args.lang.or_else(|| config.ocr_lang.clone());

    let engine = OcrEngine::new(OcrOptions {
        backend,
        lang,
        api_key: config.groq_api_key.clone(),
        proxy: args.proxy.unwrap_or_else(|| config.proxy.clone()),
        model: args.model.unwrap_or_else(|| config.ocr_model.clone()),
        prompt: args.prompt.unwrap_or_else(|| config.ocr_prompt.clone()),
        endpoint: args.endpoint.unwrap_or_else(|| config.ocr_endpoint.clone()),
        timeout: Duration::from_secs(config.ocr_timeout_seconds),
        force_image_ocr: args.force_ocr,
        image: config.image_options(None, args.ffmpeg),
        jobs,
        max_tokens: config.ocr_max_tokens,
        cache_dir,
    })?;

    if args.in_place {
        for spec in &specs {
            if formats::detect(&spec.path) != Some(Format::Pdf) {
                bail!(
                    "--in-place is only supported for PDF files, but found {}",
                    spec.path.display()
                );
            }
        }

        let mut failures = 0usize;
        for spec in &specs {
            if let Some(pages) = &spec.pages {
                output::info(format!("Queued OCR (in-place): {}:{}", spec.path.display(), pages));
            } else {
                output::info(format!("Queued OCR (in-place): {}", spec.path.display()));
            }

            match engine.create_searchable_pdf_for_spec(spec, config) {
                Ok(mut doc) => match crate::pdf::save_to_bytes(&mut doc) {
                    Ok(bytes) => {
                        if let Err(error) = write_output(&spec.path, &bytes) {
                            failures += 1;
                            output::warn(format!(
                                "error writing {}: {error:#}",
                                spec.path.display()
                            ));
                            if fail_fast {
                                return Err(error).with_context(|| {
                                    format!("failed to process {}", spec.path.display())
                                });
                            }
                        }
                    }
                    Err(error) => {
                        failures += 1;
                        output::warn(format!(
                            "error generating PDF for {}: {error:#}",
                            spec.path.display()
                        ));
                        if fail_fast {
                            return Err(error).with_context(|| {
                                format!("failed to process {}", spec.path.display())
                            });
                        }
                    }
                },
                Err(error) => {
                    failures += 1;
                    output::warn(format!(
                        "error processing {}: {error:#}",
                        spec.path.display()
                    ));
                    if fail_fast {
                        return Err(error).with_context(|| {
                            format!("failed to process {}", spec.path.display())
                        });
                    }
                }
            }
        }
        return finish_batch("ocr", specs.len(), failures, specs.len() - failures);
    }

    let is_pdf_output = args
        .out
        .as_ref()
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("pdf"))
        .unwrap_or(false);

    if is_pdf_output {
        let output_path = args.out.as_ref().unwrap();
        let mut documents = Vec::new();
        for spec in &specs {
            let doc = engine.create_searchable_pdf_for_spec(spec, config)?;
            documents.push(doc);
        }
        if documents.is_empty() {
            bail!("no pages could be extracted for searchable PDF");
        }
        let mut doc = crate::pdf::merge_documents(documents)?;
        let bytes = crate::pdf::save_to_bytes(&mut doc)?;
        write_output(output_path, &bytes)?;
        return finish_batch("ocr", specs.len(), 0, 1);
    }

    for spec in &specs {
        if let Some(pages) = &spec.pages {
            output::info(format!("Queued OCR: {}:{}", spec.path.display(), pages));
        } else {
            output::info(format!("Queued OCR: {}", spec.path.display()));
        }
    }
    let results = extract(&engine, &specs, fail_fast);
    let combined = args.out.is_some();
    let mut parts = Vec::new();
    let mut failures = 0usize;

    for (spec, result) in specs.iter().zip(results) {
        match result {
            Ok(text) if combined => parts.push(text),
            Ok(text) => {
                let output_path = spec.path.with_extension("md");
                if let Err(error) = write_output(&output_path, text.as_bytes()) {
                    failures += 1;
                    output::warn(format!(
                        "error writing {}: {error:#}",
                        output_path.display()
                    ));
                }
            }
            Err(error) if fail_fast => {
                return Err(error)
                    .with_context(|| format!("failed to process {}", spec.path.display()));
            }
            Err(error) => {
                failures += 1;
                output::warn(format!(
                    "error processing {}: {error:#}",
                    spec.path.display()
                ));
            }
        }
    }

    if let Some(output_path) = args.out
        && !parts.is_empty()
    {
        write_output(&output_path, parts.join(DOCUMENT_SEPARATOR).as_bytes())?;
    }
    let outputs = if combined {
        usize::from(!parts.is_empty())
    } else {
        specs.len() - failures
    };
    finish_batch("ocr", specs.len(), failures, outputs)
}

fn extract(engine: &OcrEngine, specs: &[InputSpec], fail_fast: bool) -> Vec<Result<String>> {
    if !fail_fast {
        return engine.extract_many_specs(specs);
    }
    let mut results = Vec::new();
    for spec in specs {
        let result = engine.extract_spec(spec);
        let failed = result.is_err();
        results.push(result);
        if failed {
            break;
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    #[test]
    fn ocr_pdf_output_creates_searchable_pdf_on_windows() {
        if !cfg!(windows) || !crate::winocr::is_available() {
            return;
        }
        let temp_dir = tempfile::tempdir().unwrap();
        let image_path = temp_dir.path().join("scan.png");
        let output_pdf = temp_dir.path().join("searchable.pdf");

        // Create a 100x40 image with some pixels
        let mut img = image::RgbImage::new(100, 40);
        for pixel in img.pixels_mut() {
            *pixel = image::Rgb([255, 255, 255]);
        }
        img.save(&image_path).unwrap();

        let config = Config::default();
        let args = OcrArgs {
            inputs: vec![image_path.to_str().unwrap().to_owned()],
            out: Some(output_pdf.clone()),
            in_place: false,
            proxy: None,
            model: None,
            prompt: None,
            endpoint: None,
            force_ocr: false,
            ffmpeg: None,
            jobs: Some(1),
            no_cache: true,
            cache_dir: None,
            engine: Some("windows".to_owned()),
            lang: None,
        };

        let result = run(args, &config, true);
        assert!(result.is_ok(), "OCR to PDF failed: {:?}", result.err());
        assert!(output_pdf.is_file());

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

        let mut doc = lopdf::Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(lopdf::dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });

        let mut img = image::RgbImage::new(500, 500);
        for pixel in img.pixels_mut() {
            *pixel = image::Rgb([255, 255, 255]);
        }
        let mut img_bytes = std::io::Cursor::new(Vec::new());
        img.write_to(&mut img_bytes, image::ImageFormat::Jpeg).unwrap();
        
        let image_stream = lopdf::Stream::new(
            lopdf::dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 500,
                "Height" => 500,
                "ColorSpace" => "DeviceRGB",
                "BitsPerComponent" => 8,
                "Filter" => "DCTDecode",
            },
            img_bytes.into_inner(),
        );
        let image_id = doc.add_object(image_stream);

        let resources_id = doc.add_object(lopdf::dictionary! {
            "Font" => lopdf::dictionary! { "F1" => font_id },
            "XObject" => lopdf::dictionary! { "Im1" => image_id },
        });
        let content_id = doc.add_object(lopdf::Stream::new(
            lopdf::dictionary! {},
            b"q 100 0 0 40 0 0 cm /Im1 Do Q BT /F1 24 Tf 100 700 Td (Hello World) Tj ET".to_vec(),
        ));
        let page_id = doc.add_object(lopdf::dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        });
        doc.objects.insert(
            pages_id,
            lopdf::dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }
            .into(),
        );
        let catalog_id = doc.add_object(lopdf::dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
        doc.compress();
        doc.save(&input_pdf).unwrap();

        let config = Config::default();
        let args = OcrArgs {
            inputs: vec![input_pdf.to_str().unwrap().to_owned()],
            out: None,
            in_place: true,
            proxy: None,
            model: None,
            prompt: None,
            endpoint: None,
            force_ocr: true,
            ffmpeg: None,
            jobs: Some(1),
            no_cache: true,
            cache_dir: None,
            engine: Some("windows".to_owned()),
            lang: None,
        };

        let result = run(args, &config, true);
        assert!(result.is_ok(), "in-place OCR failed: {:?}", result.err());

        let doc2 = lopdf::Document::load(&input_pdf).unwrap();
        assert_eq!(doc2.get_pages().len(), 1);
    }
}
