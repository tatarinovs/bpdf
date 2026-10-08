use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::process;

/// HTML pages are laid out by a headless Chromium-based browser (Microsoft
/// Edge ships with Windows; Chrome and Chromium are found elsewhere). Without
/// one, `input.rs` falls back to the plain-text HTML extractor.
#[derive(Clone, Debug)]
pub struct HtmlOptions {
    pub browser: Option<PathBuf>,
    pub timeout: Duration,
}

/// Browser window size in CSS pixels for screenshots, written as `1200x1600`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

impl Viewport {
    pub fn parse(value: &str) -> Result<Self> {
        let parsed = value
            .trim()
            .to_ascii_lowercase()
            .split_once(['x', '×'])
            .and_then(|(width, height)| {
                Some((width.trim().parse().ok()?, height.trim().parse().ok()?))
            });
        match parsed {
            Some((width @ 1..=16384, height @ 1..=16384)) => Ok(Self { width, height }),
            _ => bail!("invalid viewport {value:?}; expected WIDTHxHEIGHT, e.g. 1200x1600"),
        }
    }
}

pub fn convert_to_pdf(path: &Path, options: &HtmlOptions) -> Result<Vec<u8>> {
    let pdf = run_browser(
        path,
        options,
        "page.pdf",
        &[
            "--no-pdf-header-footer",
            // Pre-2023 spelling of the same switch; Chromium ignores unknown flags.
            "--print-to-pdf-no-header",
        ],
        "--print-to-pdf",
    )?;
    if !pdf.starts_with(b"%PDF-") {
        bail!("browser output for {} is not a PDF", path.display());
    }
    Ok(pdf)
}

/// PNG of the page as it appears in a browser window of `viewport` size.
pub fn screenshot(path: &Path, options: &HtmlOptions, viewport: Viewport) -> Result<Vec<u8>> {
    let window_size = format!("--window-size={},{}", viewport.width, viewport.height);
    run_browser(
        path,
        options,
        "page.png",
        &[
            "--hide-scrollbars",
            "--force-device-scale-factor=1",
            &window_size,
            // Lets web fonts, images and start-up scripts settle before the
            // capture; virtual time also waits for pending network loads.
            "--virtual-time-budget=1000",
        ],
        "--screenshot",
    )
}

fn run_browser(
    path: &Path,
    options: &HtmlOptions,
    output_name: &str,
    mode_args: &[&str],
    output_flag: &str,
) -> Result<Vec<u8>> {
    let browser = find_browser(options.browser.as_deref())?;
    // An absolute path, not a canonical one: Chromium does not understand the
    // `\\?\` prefix that `canonicalize` adds on Windows. Passing a path rather
    // than a hand-built file URL leaves non-ASCII names to the browser.
    let input = std::path::absolute(path)
        .with_context(|| format!("failed to resolve {}", path.display()))?;

    // A throwaway profile keeps the conversion independent of a running
    // browser window and of the user's extensions, cookies and history.
    let workspace = tempfile::tempdir()?;
    let output_path = workspace.path().join(output_name);

    let mut command = Command::new(&browser);
    command
        .args([
            "--headless",
            "--disable-gpu",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-extensions",
            "--disable-background-networking",
            "--disable-component-update",
            "--disable-sync",
        ])
        .args(mode_args)
        .arg(flag("--user-data-dir", &workspace.path().join("profile")))
        .arg(flag(output_flag, &output_path))
        .arg(&input);

    let output = process::run(command, options.timeout, "HTML rendering").with_context(|| {
        format!(
            "failed to render {} with {}",
            path.display(),
            browser.display()
        )
    })?;
    process::require_success(output, "HTML rendering")?;

    std::fs::read(&output_path)
        .with_context(|| format!("browser produced no output for {}", path.display()))
}

/// Browser used for HTML rendering: the configured one, or the first
/// Chromium-based browser installed in a standard location.
pub fn find_browser(configured: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = configured {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        return which(path)
            .with_context(|| format!("configured browser {} was not found", path.display()));
    }
    browser_candidates()
        .into_iter()
        .find(|path| path.is_file())
        .or_else(|| {
            ["msedge", "chrome", "chromium", "chromium-browser", "google-chrome", "microsoft-edge"]
                .into_iter()
                .find_map(|name| which(Path::new(name)))
        })
        .context("no Chromium-based browser (Edge, Chrome, Chromium) found; set `browser` in config.toml")
}

fn flag(name: &str, value: &Path) -> std::ffi::OsString {
    let mut flag = std::ffi::OsString::from(name);
    flag.push("=");
    flag.push(value);
    flag
}

#[cfg(windows)]
fn browser_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for (variable, relative) in [
        (
            "ProgramFiles(x86)",
            r"Microsoft\Edge\Application\msedge.exe",
        ),
        ("ProgramFiles", r"Microsoft\Edge\Application\msedge.exe"),
        ("ProgramFiles", r"Google\Chrome\Application\chrome.exe"),
        ("ProgramFiles(x86)", r"Google\Chrome\Application\chrome.exe"),
        ("LOCALAPPDATA", r"Google\Chrome\Application\chrome.exe"),
        ("LOCALAPPDATA", r"Chromium\Application\chrome.exe"),
    ] {
        if let Some(base) = std::env::var_os(variable) {
            candidates.push(PathBuf::from(base).join(relative));
        }
    }
    candidates
}

#[cfg(target_os = "macos")]
fn browser_candidates() -> Vec<PathBuf> {
    [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

#[cfg(not(any(windows, target_os = "macos")))]
fn browser_candidates() -> Vec<PathBuf> {
    Vec::new()
}

/// Resolves a bare program name through `PATH`.
fn which(program: &Path) -> Option<PathBuf> {
    if program.components().count() != 1 {
        return None;
    }
    let extensions: &[&str] = if cfg!(windows) { &["exe"] } else { &[] };
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|directory| {
        let candidate = directory.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
        extensions
            .iter()
            .map(|extension| candidate.with_extension(extension))
            .find(|path| path.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_configured_browser_is_reported() {
        let error = find_browser(Some(Path::new(r"Z:\missing\browser.exe"))).unwrap_err();
        assert!(format!("{error:#}").contains("was not found"));
    }

    #[test]
    fn parses_viewports() {
        assert_eq!(
            Viewport::parse("1200x1600").unwrap(),
            Viewport {
                width: 1200,
                height: 1600
            }
        );
        assert_eq!(
            Viewport::parse(" 800 X 600 ").unwrap(),
            Viewport {
                width: 800,
                height: 600
            }
        );
        assert!(Viewport::parse("1200").is_err());
        assert!(Viewport::parse("0x600").is_err());
        assert!(Viewport::parse("axb").is_err());
    }

    #[test]
    fn flags_keep_paths_with_spaces_in_one_argument() {
        let value = flag("--print-to-pdf", Path::new("C:/Temp dir/page.pdf"));
        assert_eq!(value, "--print-to-pdf=C:/Temp dir/page.pdf");
    }
}
