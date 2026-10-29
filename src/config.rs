use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::imageconv::ImageOptions;
use crate::ocr::OcrBackend;
use crate::pdf::PageSize;

pub const DEFAULT_OCR_ENDPOINT: &str = "https://api.groq.com/openai/v1/chat/completions";
pub const DEFAULT_OCR_MODEL: &str = "qwen/qwen3.8-27b";
pub const DEFAULT_OCR_PROMPT: &str = "You are a highly accurate OCR engine. Extract all text exactly as it appears. Preserve layout, lists, and tables using Markdown. IMPORTANT: Do NOT extract or transcribe any text from stamps or seals (печати и штампы). Ignore them completely. Keep your reasoning/thinking to an absolute minimum (under 50 words) and immediately output the extracted text. Do not add any conversational filler.";

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    #[serde(skip)]
    pub source_path: Option<PathBuf>,
    pub groq_api_key: String,
    pub proxy: String,
    pub author: String,
    pub creator: String,
    pub auto_rotate: bool,
    pub keep_icc: bool,
    pub optimize: bool,
    pub page_size: PageSize,
    pub strip_metadata: bool,
    pub bookmarks: bool,
    pub ocr_engine: OcrBackend,
    pub ocr_lang: Option<String>,
    pub ocr_model: String,
    pub ocr_prompt: String,
    pub ocr_endpoint: String,
    pub ffmpeg: PathBuf,
    pub powershell: PathBuf,
    pub font_path: Option<PathBuf>,
    pub browser: Option<PathBuf>,
    pub jpeg_quality: u8,
    pub office_timeout_seconds: u64,
    pub html_timeout_seconds: u64,
    pub html_viewport: String,
    pub ocr_timeout_seconds: u64,
    pub ocr_jobs: usize,
    pub ocr_max_tokens: u32,
    pub ocr_cache: bool,
    pub ocr_cache_dir: PathBuf,
    pub image_dpi: u32,
    pub raw_develop: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            source_path: None,
            groq_api_key: String::new(),
            proxy: String::new(),
            author: String::new(),
            creator: String::new(),
            auto_rotate: false,
            keep_icc: false,
            optimize: false,
            page_size: PageSize::A4,
            strip_metadata: false,
            bookmarks: false,
            ocr_engine: OcrBackend::Groq,
            ocr_lang: None,
            ocr_model: DEFAULT_OCR_MODEL.to_owned(),
            ocr_prompt: DEFAULT_OCR_PROMPT.to_owned(),
            ocr_endpoint: DEFAULT_OCR_ENDPOINT.to_owned(),
            ffmpeg: PathBuf::from("ffmpeg"),
            powershell: PathBuf::from("powershell.exe"),
            font_path: None,
            browser: None,
            jpeg_quality: 95,
            office_timeout_seconds: 120,
            html_timeout_seconds: 60,
            html_viewport: "1200x1600".to_owned(),
            ocr_timeout_seconds: 120,
            ocr_jobs: 1,
            ocr_max_tokens: 4096,
            ocr_cache: true,
            ocr_cache_dir: default_cache_dir(),
            image_dpi: 150,
            raw_develop: false,
        }
    }
}

impl Config {
    pub fn image_options(&self) -> ImageOptions {
        ImageOptions {
            keep_icc: self.keep_icc,
            ffmpeg: self.ffmpeg.clone(),
            jpeg_quality: self.jpeg_quality,
            image_dpi: self.image_dpi,
            long_edge: None,
            short_edge: None,
            orient: None,
            rotation_degrees: None,
            force_reencode: false,
            raw_develop: self.raw_develop,
        }
    }

    /// The OCR cache directory, when caching is on and a directory is known.
    pub fn active_ocr_cache_dir(&self) -> Option<&Path> {
        (self.ocr_cache && !self.ocr_cache_dir.as_os_str().is_empty())
            .then_some(self.ocr_cache_dir.as_path())
    }

    pub fn load(explicit_path: Option<&Path>) -> Result<Self> {
        if let Some(path) = explicit_path {
            let text = fs::read_to_string(path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let mut config = Self::parse(&text)
                .with_context(|| format!("invalid configuration {}", path.display()))?;
            config.source_path = Some(path.to_path_buf());
            return Ok(config);
        }

        for path in config_candidates() {
            match fs::read_to_string(&path) {
                Ok(text) => {
                    let mut config = Self::parse(&text)
                        .with_context(|| format!("invalid configuration {}", path.display()))?;
                    config.source_path = Some(path);
                    return Ok(config);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to read {}", path.display()));
                }
            }
        }
        Ok(Self::default())
    }

    fn parse(text: &str) -> Result<Self> {
        Self::parse_with(text, &|name| std::env::var(name).ok())
    }

    /// Parse with `variable` looking up the `%NAME%` references.
    fn parse_with(text: &str, variable: &dyn Fn(&str) -> Option<String>) -> Result<Self> {
        let mut config: Self =
            basic_toml::from_str(text).map_err(|error| anyhow::anyhow!("{error}"))?;
        // `%NAME%` is expanded in the parsed values, so a variable holding
        // backslashes or quotes cannot break the TOML syntax.
        for value in [&mut config.groq_api_key, &mut config.proxy] {
            *value = expand_variables(value, variable);
        }
        for path in [
            Some(&mut config.ffmpeg),
            Some(&mut config.powershell),
            Some(&mut config.ocr_cache_dir),
            config.font_path.as_mut(),
            config.browser.as_mut(),
        ]
        .into_iter()
        .flatten()
        {
            *path = PathBuf::from(expand_variables(&path.to_string_lossy(), variable));
        }

        if !(1..=100).contains(&config.jpeg_quality) {
            bail!("jpeg_quality must be between 1 and 100");
        }
        if config.office_timeout_seconds == 0 {
            bail!("office_timeout_seconds must be positive");
        }
        if config.html_timeout_seconds == 0 {
            bail!("html_timeout_seconds must be positive");
        }
        crate::html::Viewport::parse(&config.html_viewport).context("html_viewport")?;
        if config.ocr_timeout_seconds == 0 {
            bail!("ocr_timeout_seconds must be positive");
        }
        if config.ocr_jobs == 0 {
            bail!("ocr_jobs must be positive");
        }
        if config.ocr_max_tokens == 0 {
            bail!("ocr_max_tokens must be positive");
        }

        Ok(config)
    }
}

/// The user's own cache directory. Recognised text is private, so a shared
/// location such as `/tmp` is never used; without one the cache is off
/// until `ocr_cache_dir` is set.
fn default_cache_dir() -> PathBuf {
    let env = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    let base = if cfg!(windows) {
        env("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|home| PathBuf::from(home).join("Library").join("Caches"))
    } else {
        env("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| env("HOME").map(|home| PathBuf::from(home).join(".cache")))
    };
    base.map(|base| base.join("bpdf").join("ocr-cache"))
        .unwrap_or_default()
}

/// Only the file beside the executable: the working directory belongs to the
/// user's documents (a file manager starts bpdf in the current panel), where a
/// `config.toml` of another program must not be picked up.
fn config_candidates() -> Vec<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|executable| Some(executable.parent()?.join("config.toml")))
        .into_iter()
        .collect()
}

/// Replace every `%NAME%` that `variable` knows; names with whitespace (as
/// in `% 10 % 20`) and unknown names stay as written.
fn expand_variables(text: &str, variable: &dyn Fn(&str) -> Option<String>) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('%') else {
            break;
        };
        let name = &after[..end];
        let value = (!name.is_empty() && !name.contains(char::is_whitespace))
            .then(|| variable(name))
            .flatten();
        result.push_str(&rest[..start]);
        match value {
            Some(value) => {
                result.push_str(&value);
                rest = &after[end + 1..];
            }
            // The closing `%` may open the next reference.
            None => {
                result.push('%');
                rest = after;
            }
        }
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_toml_config() {
        let config = Config::parse(
            r#"
                # Top-level comment
                auto_rotate = true
                keep_icc = false
                page_size = "Letter"
                ocr_prompt = '''Keep # signs and: colons'''
                jpeg_quality = 91
                font_path = 'C:\Windows\Fonts\arial.ttf'
                ocr_jobs = 3
                ocr_cache = false
                ocr_cache_dir = 'D:\cache\bpdf'
            "#,
        )
        .unwrap();

        assert!(config.auto_rotate);
        assert!(!config.keep_icc);
        assert_eq!(config.page_size, "Letter".parse().unwrap());
        assert_eq!(config.ocr_prompt, "Keep # signs and: colons");
        assert_eq!(config.jpeg_quality, 91);
        assert_eq!(config.ocr_jobs, 3);
        assert!(!config.ocr_cache);
        assert_eq!(config.ocr_cache_dir, PathBuf::from(r"D:\cache\bpdf"));
        assert_eq!(
            config.font_path,
            Some(PathBuf::from(r"C:\Windows\Fonts\arial.ttf"))
        );
    }

    #[test]
    fn toml_example_file_parses_successfully() {
        let example_toml = include_str!("../config.example.toml");
        let config = Config::parse(example_toml).expect("config.example.toml must be valid");
        assert_eq!(config.page_size, PageSize::A4);
        assert_eq!(config.jpeg_quality, 95);
        assert_eq!(config.ocr_engine, OcrBackend::Groq);
    }

    #[test]
    fn invalid_page_size_and_engine_are_reported() {
        let error = Config::parse("page_size = \"A9\"").unwrap_err();
        assert!(
            format!("{error:#}").contains("unknown page size"),
            "{error:#}"
        );
        let error = Config::parse("ocr_engine = \"tesseract\"").unwrap_err();
        assert!(
            format!("{error:#}").contains("unknown OCR engine"),
            "{error:#}"
        );
    }

    #[test]
    fn unknown_keys_are_reported() {
        let error = Config::parse("jpeg_qualty = 80").unwrap_err();
        assert!(format!("{error:#}").contains("jpeg_qualty"), "{error:#}");
    }

    #[test]
    fn toml_inline_comment_is_handled() {
        let config = Config::parse(
            r#"
                jpeg_quality = 80 # high quality
            "#,
        )
        .unwrap();
        assert_eq!(config.jpeg_quality, 80);
    }

    #[test]
    fn url_inside_string_is_preserved() {
        let config = Config::parse(
            r#"
                ocr_endpoint = "https://api.groq.com/openai/v1/chat/completions"
            "#,
        )
        .unwrap();
        assert!(config.ocr_endpoint.starts_with("https://"));
    }

    fn variables(name: &str) -> Option<String> {
        match name {
            "BPDF_TEST_VAR" => Some("hello".to_owned()),
            "BPDF_PATH_VAR" => Some(r"C:\Tools\bin".to_owned()),
            "BPDF_KEY_VAR" => Some("secret_key_123".to_owned()),
            _ => None,
        }
    }

    #[test]
    fn expands_known_variables_only() {
        assert_eq!(
            expand_variables("%BPDF_TEST_VAR% world", &variables),
            "hello world"
        );
        assert_eq!(
            expand_variables("%BPDF_UNKNOWN% world", &variables),
            "%BPDF_UNKNOWN% world"
        );
        assert_eq!(
            expand_variables("% not a var % rest", &variables),
            "% not a var % rest"
        );
        assert_eq!(
            expand_variables("100%%BPDF_TEST_VAR%", &variables),
            "100%hello"
        );
    }

    #[test]
    fn expand_env_in_config_fields() {
        let config = Config::parse_with(
            r#"
                groq_api_key = "%BPDF_KEY_VAR%"
                ffmpeg = '%BPDF_PATH_VAR%\ffmpeg.exe'
                ocr_cache_dir = "%BPDF_PATH_VAR%/cache"
            "#,
            &variables,
        )
        .unwrap();
        assert_eq!(config.groq_api_key, "secret_key_123");
        assert_eq!(config.ffmpeg, PathBuf::from(r"C:\Tools\bin\ffmpeg.exe"));
        // Backslashes of the value inside a "..." string are no TOML escapes.
        assert_eq!(config.ocr_cache_dir, PathBuf::from(r"C:\Tools\bin/cache"));
    }

    #[test]
    fn max_tokens_default_is_4096() {
        let config = Config::default();
        assert_eq!(config.ocr_max_tokens, 4096);
    }
}
