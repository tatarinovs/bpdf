use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::common::{DOCUMENT_SEPARATOR, finish_batch, write_output};
use crate::cli::OcrArgs;
use crate::config::Config;
use crate::fileset::expand;
use crate::formats::InputFormatSet;
use crate::ocr::{OcrEngine, OcrOptions};
use crate::output;

pub fn run(args: OcrArgs, config: &Config, fail_fast: bool) -> Result<()> {
    let specs = expand(&args.inputs, InputFormatSet::Ocr)?;
    if specs.iter().any(|spec| spec.pages.is_some()) {
        bail!("page ranges are not supported by ocr; extract the pages first");
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
    let engine = OcrEngine::new(OcrOptions {
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

    for spec in &specs {
        output::info(format!("Queued OCR: {}", spec.path.display()));
    }
    let paths = specs
        .iter()
        .map(|spec| spec.path.clone())
        .collect::<Vec<_>>();
    let results = extract(&engine, &paths, fail_fast);
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

fn extract(engine: &OcrEngine, paths: &[PathBuf], fail_fast: bool) -> Vec<Result<String>> {
    if !fail_fast {
        return engine.extract_many(paths);
    }
    let mut results = Vec::new();
    for path in paths {
        let result = engine.extract_text(path);
        let failed = result.is_err();
        results.push(result);
        if failed {
            break;
        }
    }
    results
}
