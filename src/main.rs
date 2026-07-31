mod app;
mod atomic;
mod cli;
mod config;
mod doctor;
mod encoding;
mod fileset;
mod hash;
mod imageconv;
mod input;
mod metadata;
mod ocr;
mod office;
mod output;
mod pdf;
mod process;
mod textpdf;

use anyhow::{Result, anyhow};
use clap::Parser;
use clap::error::ErrorKind;

use crate::cli::Cli;
use crate::config::Config;

fn main() {
    if let Err(error) = run() {
        output::error(format!("{error:#}"));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let arguments = std::env::args_os().collect::<Vec<_>>();
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
    app::run(cli.command, config)
}
