pub(crate) mod cli;
pub(crate) mod config;

mod archive;
mod atomic;
#[cfg(windows)]
mod com;
mod commands;
mod doctor;
mod ebook;
mod encoding;
mod fileset;
mod font_subset;
mod formats;
mod hash;
mod html;
mod imageconv;
mod input;
mod metadata;
mod ocr;
mod office;
mod office_fallback;
mod output;
mod parallel;
mod pdf;
mod process;
mod psd;
mod raw;
mod stamp_picker;
mod textpdf;
mod wic;
mod winocr;
mod winpdf;
mod xml;

use std::ffi::OsString;

use anyhow::{Result, anyhow};
use clap::Parser;
use clap::error::ErrorKind;

use cli::Cli;
use config::Config;

pub fn run(arguments: Vec<OsString>) -> Result<()> {
    output::init(
        arguments.iter().any(|value| value == "--quiet"),
        arguments.iter().any(|value| value == "--json"),
    );
    let cli = match Cli::try_parse_from(arguments) {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            print!("{error}");
            return Ok(());
        }
        Err(error) => return Err(anyhow!(error.to_string())),
    };
    output::init(cli.quiet, cli.json);
    // Report-only commands keep their window open when launched from a file manager.
    if matches!(
        cli.command,
        cli::Command::Info { .. } | cli::Command::Doctor
    ) {
        SHOWS_REPORT.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    if cli.no_pause {
        NO_PAUSE.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let config = Config::load(cli.config.as_deref())?;
    commands::run(cli.command, config, cli.fail_fast)
}

pub fn report_error(error: &anyhow::Error) {
    output::error(format!("{error:#}"));
}

/// Process exit code for a failed run: 2 when only some inputs of a batch
/// failed, 1 otherwise.
pub fn exit_code(error: &anyhow::Error) -> i32 {
    if error
        .downcast_ref::<commands::common::PartialFailure>()
        .is_some()
    {
        2
    } else {
        1
    }
}

static SHOWS_REPORT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static NO_PAUSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// True after parsing a command whose output is a report to read.
pub fn shows_report() -> bool {
    SHOWS_REPORT.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn is_no_pause() -> bool {
    NO_PAUSE.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn is_json_mode() -> bool {
    output::is_json()
}
