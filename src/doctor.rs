use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::json;

use crate::config::Config;
use crate::ocr::{OcrEngine, OcrOptions};
use crate::office::{self, OfficeOptions};
use crate::{output, process, textpdf};

pub fn run(config: &Config) -> Result<()> {
    let mut failures = 0usize;

    check(
        "config",
        true,
        config
            .source_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "not found; built-in defaults are active".to_owned()),
        &mut failures,
    );
    check(
        "groq_api_key",
        !config.groq_api_key.trim().is_empty(),
        if config.groq_api_key.trim().is_empty() {
            "not configured"
        } else {
            "configured (redacted)"
        },
        &mut failures,
    );

    let ffmpeg_ok = probe_command(&config.ffmpeg, &["-version"]);
    check(
        "ffmpeg",
        ffmpeg_ok,
        if ffmpeg_ok {
            "available"
        } else {
            "not available"
        },
        &mut failures,
    );

    let raw_decoder = crate::wic::availability();
    check(
        "raw_decoder",
        raw_decoder.is_ok(),
        raw_decoder.unwrap_or_else(|error| format!("{error:#}")),
        &mut failures,
    );

    let office_options = OfficeOptions {
        powershell: config.powershell.clone(),
        timeout: Duration::from_secs(config.office_timeout_seconds),
    };
    match office::probe(&office_options) {
        Ok(availability) => {
            check(
                "word",
                availability.word,
                if availability.word {
                    "COM registration found"
                } else {
                    "COM registration not found"
                },
                &mut failures,
            );
            check(
                "excel",
                availability.excel,
                if availability.excel {
                    "COM registration found"
                } else {
                    "COM registration not found"
                },
                &mut failures,
            );
            check(
                "powerpoint",
                availability.powerpoint,
                if availability.powerpoint {
                    "COM registration found"
                } else {
                    "COM registration not found"
                },
                &mut failures,
            );
        }
        Err(error) => {
            check("word", false, format!("{error:#}"), &mut failures);
            check("excel", false, "Office probe failed", &mut failures);
            check("powerpoint", false, "Office probe failed", &mut failures);
        }
    }

    match textpdf::find_font(config.font_path.as_deref()) {
        Ok(path) => check("font", true, path.display().to_string(), &mut failures),
        Err(error) => check("font", false, format!("{error:#}"), &mut failures),
    }

    match probe_cache(&config.ocr_cache_dir) {
        Ok(()) => check(
            "ocr_cache",
            true,
            config.ocr_cache_dir.display().to_string(),
            &mut failures,
        ),
        Err(error) => check("ocr_cache", false, format!("{error:#}"), &mut failures),
    }

    let win_ocr_langs = crate::winocr::available_languages();
    match win_ocr_langs {
        Ok(langs) if !langs.is_empty() => {
            check(
                "windows_ocr",
                true,
                format!("available (languages: {})", langs.join(", ")),
                &mut failures,
            );
        }
        Ok(_) => {
            check(
                "windows_ocr",
                false,
                "no OCR language packs installed",
                &mut failures,
            );
        }
        Err(error) => {
            check("windows_ocr", false, format!("{error:#}"), &mut failures);
        }
    }

    let network_result = OcrEngine::new(OcrOptions {
        backend: crate::ocr::OcrBackend::Groq,
        lang: None,
        api_key: config.groq_api_key.clone(),
        proxy: config.proxy.clone(),
        model: config.ocr_model.clone(),
        prompt: config.ocr_prompt.clone(),
        endpoint: config.ocr_endpoint.clone(),
        timeout: Duration::from_secs(config.ocr_timeout_seconds),
        force_image_ocr: false,
        image: config.image_options(None, None),
        jobs: 1,
        max_tokens: config.ocr_max_tokens,
        cache_dir: None,
    })
    .and_then(|engine| engine.check_connection());
    let network_ok = network_result.is_ok();
    if config.proxy.trim().is_empty() {
        check(
            "proxy",
            true,
            "not configured; direct connection selected",
            &mut failures,
        );
    } else {
        check(
            "proxy",
            network_ok,
            if network_ok {
                "configured proxy accepted the Groq request"
            } else {
                "configured, but the network check failed"
            },
            &mut failures,
        );
    }
    match network_result {
        Ok(()) => check(
            "groq",
            true,
            "authentication and models endpoint available",
            &mut failures,
        ),
        Err(error) => check("groq", false, format!("{error:#}"), &mut failures),
    }

    output::result(
        "doctor_summary",
        format!("Doctor completed: {failures} failure(s)"),
        json!({"failures": failures, "ok": failures == 0}),
    );
    if failures != 0 {
        bail!("doctor found {failures} failed check(s)");
    }
    Ok(())
}

fn check(name: &str, ok: bool, detail: impl AsRef<str>, failures: &mut usize) {
    if !ok {
        *failures += 1;
    }
    let message = format!(
        "[{}] {name}: {}",
        if ok { "OK" } else { "FAIL" },
        detail.as_ref()
    );
    output::result(
        "doctor_check",
        message,
        json!({"name": name, "ok": ok, "detail": detail.as_ref()}),
    );
}

fn probe_command(program: &PathBuf, args: &[&str]) -> bool {
    let mut command = Command::new(program);
    command.args(args);
    process::run(command, Duration::from_secs(10), "dependency probe")
        .is_ok_and(|output| output.status.success())
}

fn probe_cache(directory: &PathBuf) -> Result<()> {
    fs::create_dir_all(directory)?;
    let path = directory.join(format!(".doctor-{}.tmp", std::process::id()));
    fs::write(&path, b"bpdf")?;
    fs::remove_file(path)?;
    Ok(())
}
