use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use lopdf::Document;

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

/// PDF of an HTML page on pages shaped like the target paper: a page that
/// fits one sheet of `page_ratio` (height / width) becomes one PDF page of
/// exactly its own size, a longer one is split into sheets of its width.
pub fn convert_to_pdf(path: &Path, options: &HtmlOptions, page_ratio: f64) -> Result<Document> {
    let workspace = tempfile::tempdir()?;
    let page = prepare_page(path, workspace.path(), page_ratio)?;
    let pdf = run_browser(
        &page,
        path,
        options,
        workspace.path(),
        "page.pdf",
        &[
            "--no-pdf-header-footer",
            // Pre-2023 spelling of the same switch; Chromium ignores unknown flags.
            "--print-to-pdf-no-header",
            // Lets web fonts and images load and the paper script run
            // before printing.
            "--virtual-time-budget=1000",
        ],
        "--print-to-pdf",
    )?;
    if !pdf.starts_with(b"%PDF-") {
        bail!("browser output for {} is not a PDF", path.display());
    }
    Document::load_mem(&pdf)
        .with_context(|| format!("browser output for {} is invalid", path.display()))
}

/// PNG of the page as it appears in a browser window of `viewport` size.
pub fn screenshot(path: &Path, options: &HtmlOptions, viewport: Viewport) -> Result<Vec<u8>> {
    let workspace = tempfile::tempdir()?;
    let window_size = format!("--window-size={},{}", viewport.width, viewport.height);
    run_browser(
        path,
        path,
        options,
        workspace.path(),
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

/// Runs the browser on `page` and returns its output file. `source` is the
/// user's file, named in messages.
fn run_browser(
    page: &Path,
    source: &Path,
    options: &HtmlOptions,
    workspace: &Path,
    output_name: &str,
    mode_args: &[&str],
    output_flag: &str,
) -> Result<Vec<u8>> {
    let browser = find_browser(options.browser.as_deref())?;
    // An absolute path, not a canonical one: Chromium does not understand the
    // `\\?\` prefix that `canonicalize` adds on Windows. Passing a path rather
    // than a hand-built file URL leaves non-ASCII names to the browser.
    let input = std::path::absolute(page)
        .with_context(|| format!("failed to resolve {}", page.display()))?;
    let output_path = workspace.join(output_name);

    // A throwaway profile keeps the conversion independent of a running
    // browser window and of the user's extensions, cookies and history.
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
        .arg(flag("--user-data-dir", &workspace.join("profile")))
        .arg(flag(output_flag, &output_path))
        .arg(&input);

    let output = process::run(command, options.timeout, "HTML rendering").with_context(|| {
        format!(
            "failed to render {} with {}",
            source.display(),
            browser.display()
        )
    })?;
    process::require_success(output, "HTML rendering")?;

    std::fs::read(&output_path)
        .with_context(|| format!("browser produced no output for {}", source.display()))
}

/// Script added to the printed page. Once fonts and images are in, it sizes
/// the paper to the content, unless the page sets `@page { size }` itself.
/// Without it the browser prints on Letter or A4, cutting off whatever is
/// wider and pushing the rest to further pages.
const LAYOUT_SCRIPT: &str = include_str!("html/layout.js");

/// Copy of the page with the paper script; relative links still resolve
/// next to the original through `<base>`. XHTML and UTF-16 pages, where the
/// insertion could break parsing, are printed as they are.
fn prepare_page(path: &Path, workspace: &Path, page_ratio: f64) -> Result<PathBuf> {
    let source =
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let xhtml = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("xhtml"));
    if xhtml || source.starts_with(&[0xFF, 0xFE]) || source.starts_with(&[0xFE, 0xFF]) {
        return Ok(path.to_path_buf());
    }
    let directory = std::path::absolute(path)?
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let script = format!(
        "<script>{}</script>",
        LAYOUT_SCRIPT.replace("__RATIO__", &format!("{page_ratio:.6}"))
    );
    let copy = workspace.join(path.file_name().unwrap_or("page.html".as_ref()));
    std::fs::write(&copy, inject(&source, &file_url(&directory), &script))?;
    Ok(copy)
}

/// Adds `<base href>` at the start of the head (unless the page has its own
/// base) and the script at the end. Both are ASCII, so the page's encoding is
/// untouched, and the base is short enough to keep a `<meta charset>` within
/// the first 1024 bytes the browser inspects.
fn inject(source: &[u8], base_url: &str, script: &str) -> Vec<u8> {
    let lower = source.to_ascii_lowercase();
    let mut page = Vec::with_capacity(source.len() + base_url.len() + script.len() + 16);
    if find(&lower, b"<base").is_some() {
        page.extend_from_slice(source);
    } else {
        let position = tag_end(&lower, b"<head")
            .or_else(|| tag_end(&lower, b"<html"))
            .or_else(|| tag_end(&lower, b"<!doctype"))
            .unwrap_or(0);
        page.extend_from_slice(&source[..position]);
        page.extend_from_slice(format!("<base href=\"{base_url}\">").as_bytes());
        page.extend_from_slice(&source[position..]);
    }
    page.extend_from_slice(script.as_bytes());
    page
}

/// Byte position just after the opening tag `name` (`<head`, not `<header`).
fn tag_end(lower: &[u8], name: &[u8]) -> Option<usize> {
    let mut from = 0;
    while let Some(offset) = find(&lower[from..], name) {
        let after = from + offset + name.len();
        if lower
            .get(after)
            .is_some_and(|&byte| byte == b'>' || byte.is_ascii_whitespace())
        {
            let end = lower[after..].iter().position(|&byte| byte == b'>')?;
            return Some(after + end + 1);
        }
        from = after;
    }
    None
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// `file:` URL of a directory, with the trailing slash that makes relative
/// links resolve inside it.
fn file_url(directory: &Path) -> String {
    let path = directory.to_string_lossy().replace('\\', "/");
    let mut url = String::from(if path.starts_with('/') {
        "file://"
    } else {
        "file:///"
    });
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/:".contains(&byte) {
            url.push(char::from(byte));
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    if !url.ends_with('/') {
        url.push('/');
    }
    url
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

    #[test]
    fn injects_base_into_head_and_script_at_end() {
        let page = inject(
            b"<!DOCTYPE html><html><header-x><HEAD lang=ru><meta charset=utf-8><p>x",
            "file:///D:/a%20b/",
            "<script>s</script>",
        );
        assert_eq!(
            String::from_utf8(page).unwrap(),
            "<!DOCTYPE html><html><header-x><HEAD lang=ru><base href=\"file:///D:/a%20b/\">\
             <meta charset=utf-8><p>x<script>s</script>"
        );
        // A page with its own base keeps it; the script is still added.
        let page = inject(b"<head><base href=x></head>", "file:///y/", "<s>");
        assert_eq!(page, b"<head><base href=x></head><s>");
    }

    #[test]
    fn file_urls_escape_spaces_and_cyrillic() {
        assert_eq!(
            file_url(Path::new(r"D:\PROJECT\ник ель")),
            "file:///D:/PROJECT/%D0%BD%D0%B8%D0%BA%20%D0%B5%D0%BB%D1%8C/"
        );
    }

    #[test]
    fn layout_script_cannot_close_its_own_tag() {
        assert!(!LAYOUT_SCRIPT.to_ascii_lowercase().contains("</script"));
    }
}
