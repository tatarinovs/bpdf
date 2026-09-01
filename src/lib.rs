pub mod cli;
pub mod config;

mod archive;
mod atomic;
mod com;
mod commands;
mod doctor;
mod ebook;
mod encoding;
mod fileset;
mod formats;
mod hash;
mod imageconv;
mod input;
mod metadata;
mod ocr;
mod office;
mod office_fallback;
mod output;
mod pdf;
mod process;
mod raw;
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
    let config = Config::load(cli.config.as_deref())?;
    commands::run(cli.command, config, cli.fail_fast)
}

pub fn report_error(error: &anyhow::Error) {
    output::error(format!("{error:#}"));
}

pub fn is_json_mode() -> bool {
    output::is_json()
}
