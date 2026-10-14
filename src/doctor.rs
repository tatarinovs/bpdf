use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::json;

use crate::config::Config;
use crate::ocr::{OcrEngine, OcrOptions};
use crate::office::{self, OfficeOptions};
use crate::{output, process, textpdf};

/// Outcome of one check. Optional integrations only warn: bpdf works
/// without them, just not for the formats or engines they provide.
#[derive(Clone, Copy, PartialEq)]
enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    fn from(ok: bool, otherwise: Self) -> Self {
        if ok { Self::Ok } else { otherwise }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
        }
    }
}

#[derive(Default)]
struct Report {
    warnings: usize,
    failures: usize,
}

impl Report {
    fn check(&mut self, name: &str, status: Status, detail: impl AsRef<str>) {
        match status {
            Status::Ok => {}
            Status::Warn => self.warnings += 1,
            Status::Fail => self.failures += 1,
        }
        let detail = detail.as_ref();
        output::result(
            "doctor_check",
            format!("[{}] {name}: {detail}", status.label()),
            json!({"name": name, "status": status.label(), "detail": detail}),
        );
    }
}

pub fn run(config: &Config) -> Result<()> {
    let mut report = Report::default();
    let groq_used = config.ocr_engine.eq_ignore_ascii_case("groq")
        || config.ocr_engine.eq_ignore_ascii_case("auto");

    report.check(
        "config",
        Status::Ok,
        config.source_path.as_ref().map_or_else(
            || "not found; built-in defaults are active".to_owned(),
            |path| path.display().to_string(),
        ),
    );

    match textpdf::find_font(config.font_path.as_deref()) {
        Ok(path) => report.check("font", Status::Ok, path.display().to_string()),
        Err(error) => report.check("font", Status::Fail, format!("{error:#}")),
    }

    if config.ocr_cache {
        match probe_cache(&config.ocr_cache_dir) {
            Ok(()) => report.check(
                "ocr_cache",
                Status::Ok,
                config.ocr_cache_dir.display().to_string(),
            ),
            Err(error) => report.check("ocr_cache", Status::Fail, format!("{error:#}")),
        }
    }

    let ffmpeg = probe_command(&config.ffmpeg, &["-version"]);
    report.check(
        "ffmpeg",
        Status::from(ffmpeg, Status::Warn),
        if ffmpeg {
            "available"
        } else {
            "not available; HEIC, AVIF, PSD, JPEG 2000 and similar formats cannot be read"
        },
    );

    let wic = crate::wic::availability();
    let raw_status = match (&wic, config.raw_develop) {
        (Ok(name), true) => format!("develop mode: {name}"),
        (Err(error), true) => format!("develop mode, but the WIC codec failed: {error:#}"),
        (Ok(_), false) => "embedded previews (WIC develop codec also installed)".to_owned(),
        (Err(_), false) => "embedded previews".to_owned(),
    };
    report.check(
        "raw_decoder",
        Status::from(!config.raw_develop || wic.is_ok(), Status::Fail),
        raw_status,
    );

    let office_options = OfficeOptions {
        powershell: config.powershell.clone(),
        timeout: Duration::from_secs(config.office_timeout_seconds),
    };
    match office::probe(&office_options) {
        Ok(availability) => {
            for (name, found) in [
                ("word", availability.word),
                ("excel", availability.excel),
                ("powerpoint", availability.powerpoint),
            ] {
                report.check(
                    name,
                    Status::from(found, Status::Warn),
                    if found {
                        "COM registration found"
                    } else {
                        "not installed; only the text of such documents is merged"
                    },
                );
            }
        }
        Err(error) => report.check("office", Status::Warn, format!("{error:#}")),
    }

    match crate::html::find_browser(config.browser.as_deref()) {
        Ok(path) => report.check("browser", Status::Ok, path.display().to_string()),
        Err(error) => report.check("browser", Status::Warn, format!("{error:#}")),
    }

    // Required only when it is the configured engine; `auto` can use Groq.
    let windows_ocr_missing = if groq_used {
        Status::Warn
    } else {
        Status::Fail
    };
    match crate::winocr::available_languages() {
        Ok(languages) if !languages.is_empty() => report.check(
            "windows_ocr",
            Status::Ok,
            format!("languages: {}", languages.join(", ")),
        ),
        Ok(_) => report.check(
            "windows_ocr",
            windows_ocr_missing,
            "no OCR language packs installed",
        ),
        Err(error) => report.check("windows_ocr", windows_ocr_missing, format!("{error:#}")),
    }

    let missing_key = if groq_used {
        Status::Fail
    } else {
        Status::Warn
    };
    if config.groq_api_key.trim().is_empty() {
        report.check("groq", missing_key, "groq_api_key is not configured");
    } else {
        let network = OcrEngine::new(OcrOptions::from_config(config))
            .and_then(|engine| engine.check_connection());
        let via = if config.proxy.trim().is_empty() {
            "direct connection"
        } else {
            "configured proxy"
        };
        match network {
            Ok(()) => report.check("groq", Status::Ok, format!("API key accepted ({via})")),
            Err(error) => report.check("groq", missing_key, format!("{via}: {error:#}")),
        }
    }

    output::result(
        "doctor_summary",
        format!(
            "Doctor completed: {} failure(s), {} warning(s)",
            report.failures, report.warnings
        ),
        json!({
            "failures": report.failures,
            "warnings": report.warnings,
            "ok": report.failures == 0,
        }),
    );
    if report.failures != 0 {
        bail!("doctor found {} failed check(s)", report.failures);
    }
    Ok(())
}

fn probe_command(program: &Path, args: &[&str]) -> bool {
    let mut command = Command::new(program);
    command.args(args);
    process::run(command, Duration::from_secs(10), "dependency probe")
        .is_ok_and(|output| output.status.success())
}

fn probe_cache(directory: &Path) -> Result<()> {
    fs::create_dir_all(directory)?;
    let path = directory.join(format!(".doctor-{}.tmp", std::process::id()));
    fs::write(&path, b"bpdf")?;
    fs::remove_file(path)?;
    Ok(())
}
