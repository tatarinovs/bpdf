use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::imageconv::ImageOptions;

pub const DEFAULT_OCR_ENDPOINT: &str = "https://api.groq.com/openai/v1/chat/completions";
pub const DEFAULT_OCR_MODEL: &str = "qwen/qwen3.6-27b";
pub const DEFAULT_OCR_PROMPT: &str = "You are a highly accurate OCR engine. Extract all text exactly as it appears. Preserve layout, lists, and tables using Markdown. IMPORTANT: Do NOT extract or transcribe any text from stamps or seals (печати и штампы). Ignore them completely. Keep your reasoning/thinking to an absolute minimum (under 50 words) and immediately output the extracted text. Do not add any conversational filler.";

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
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
    pub page_size: String,
    pub strip_metadata: bool,
    pub ocr_model: String,
    pub ocr_prompt: String,
    pub ocr_endpoint: String,
    pub ffmpeg: PathBuf,
    pub powershell: PathBuf,
    pub font_path: Option<PathBuf>,
    pub jpeg_quality: u8,
    pub office_timeout_seconds: u64,
    pub ocr_timeout_seconds: u64,
    pub ocr_jobs: usize,
    pub ocr_max_tokens: u32,
    pub ocr_cache: bool,
    pub ocr_cache_dir: PathBuf,
    pub image_dpi: u32,
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
            page_size: "A4".to_owned(),
            strip_metadata: false,
            ocr_model: DEFAULT_OCR_MODEL.to_owned(),
            ocr_prompt: DEFAULT_OCR_PROMPT.to_owned(),
            ocr_endpoint: DEFAULT_OCR_ENDPOINT.to_owned(),
            ffmpeg: PathBuf::from("ffmpeg"),
            powershell: PathBuf::from("powershell.exe"),
            font_path: None,
            jpeg_quality: 95,
            office_timeout_seconds: 120,
            ocr_timeout_seconds: 120,
            ocr_jobs: 2,
            ocr_max_tokens: 4096,
            ocr_cache: true,
            ocr_cache_dir: default_cache_dir(),
            image_dpi: 150,
        }
    }
}

impl Config {
    /// Build `ImageOptions` from config with optional CLI overrides.
    pub fn image_options(&self, keep_icc: Option<bool>, ffmpeg: Option<PathBuf>) -> ImageOptions {
        ImageOptions {
            keep_icc: keep_icc.unwrap_or(self.keep_icc),
            ffmpeg: ffmpeg.unwrap_or_else(|| self.ffmpeg.clone()),
            jpeg_quality: self.jpeg_quality,
            image_dpi: self.image_dpi,
        }
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
        let mut clean_json = strip_jsonc_comments(text);

        clean_json = expand_env_vars(clean_json);

        let config: Self =
            serde_json::from_str(&clean_json).context("syntax error or invalid field type")?;

        if !(1..=100).contains(&config.jpeg_quality) {
            bail!("jpeg_quality must be between 1 and 100");
        }
        if config.office_timeout_seconds == 0 {
            bail!("office_timeout_seconds must be positive");
        }
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

fn default_cache_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(path).join("bpdf").join("ocr-cache");
    }
    if let Some(path) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(path).join("bpdf").join("ocr-cache");
    }
    std::env::temp_dir().join("bpdf-ocr-cache")
}

fn config_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from("config.jsonc"), PathBuf::from("config.json")];
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        let next_to_executable_jsonc = directory.join("config.jsonc");
        if !candidates.contains(&next_to_executable_jsonc) {
            candidates.push(next_to_executable_jsonc);
        }
        let next_to_executable_json = directory.join("config.json");
        if !candidates.contains(&next_to_executable_json) {
            candidates.push(next_to_executable_json);
        }
    }
    candidates
}

/// Strip `//` comments from JSONC text, respecting string literals.
fn strip_jsonc_comments(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim_start();
        // Full-line comment.
        if trimmed.starts_with("//") {
            output.push('\n');
            continue;
        }
        // Scan for inline `//` outside of string literals.
        let mut in_string = false;
        let mut escape = false;
        let bytes = line.as_bytes();
        let mut cut = line.len();
        for (index, &byte) in bytes.iter().enumerate() {
            if escape {
                escape = false;
                continue;
            }
            if byte == b'\\' && in_string {
                escape = true;
                continue;
            }
            if byte == b'"' {
                in_string = !in_string;
                continue;
            }
            if !in_string && byte == b'/' && index + 1 < bytes.len() && bytes[index + 1] == b'/' {
                cut = index;
                break;
            }
        }
        output.push_str(&line[..cut]);
        output.push('\n');
    }
    output
}

fn expand_env_vars(mut text: String) -> String {
    let mut i = 0;
    while let Some(start) = text[i..].find('%') {
        let absolute_start = i + start;
        if let Some(end) = text[absolute_start + 1..].find('%') {
            let absolute_end = absolute_start + 1 + end;
            let var_name = &text[absolute_start + 1..absolute_end];

            // Reject variable names with whitespace (avoids treating '% 10 % 20' as an env var)
            if var_name.contains(|c: char| c.is_whitespace()) || var_name.is_empty() {
                i = absolute_start + 1;
                continue;
            }

            if let Ok(val) = std::env::var(var_name) {
                let escaped = val.replace('\\', "\\\\").replace('"', "\\\"");
                text.replace_range(absolute_start..=absolute_end, &escaped);
                i = absolute_start + escaped.len();
            } else {
                i = absolute_end + 1;
            }
        } else {
            break;
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_with_comments() {
        let config = Config::parse(
            r#"
                {
                    // comment
                    "auto_rotate": true,
                    "keep_icc": false,
                    "page_size": "Letter",
                    "ocr_prompt": "Keep # signs and: colons",
                    "jpeg_quality": 91,
                    "font_path": "C:\\Windows\\Fonts\\arial.ttf",
                    "ocr_jobs": 3,
                    "ocr_cache": false,
                    "ocr_cache_dir": "D:\\cache\\bpdf"
                }
            "#,
        )
        .unwrap();

        assert!(config.auto_rotate);
        assert!(!config.keep_icc);
        assert_eq!(config.page_size, "Letter");
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
    fn inline_comment_is_stripped() {
        let config = Config::parse(
            r#"{
                "jpeg_quality": 80 // high quality
            }"#,
        )
        .unwrap();
        assert_eq!(config.jpeg_quality, 80);
    }

    #[test]
    fn url_inside_string_is_preserved() {
        let config = Config::parse(
            r#"{
                "ocr_endpoint": "https://api.groq.com/openai/v1/chat/completions"
            }"#,
        )
        .unwrap();
        assert!(config.ocr_endpoint.starts_with("https://"));
    }

    #[test]
    fn expand_env_vars_replaces_known_variable() {
        unsafe { std::env::set_var("BPDF_TEST_VAR", "hello") };
        let result = expand_env_vars("%BPDF_TEST_VAR% world".to_owned());
        assert_eq!(result, "hello world");
        unsafe { std::env::remove_var("BPDF_TEST_VAR") };
    }

    #[test]
    fn expand_env_vars_ignores_unknown_variable() {
        let result = expand_env_vars("%BPDF_DEFINITELY_NOT_SET% world".to_owned());
        assert_eq!(result, "%BPDF_DEFINITELY_NOT_SET% world");
    }

    #[test]
    fn expand_env_vars_rejects_whitespace_names() {
        let result = expand_env_vars("% not a var % rest".to_owned());
        assert_eq!(result, "% not a var % rest");
    }

    #[test]
    fn expand_env_vars_escapes_backslashes_and_quotes() {
        unsafe { std::env::set_var("BPDF_PATH_VAR", r#"C:\dir\"file"#) };
        let result = expand_env_vars(r#"{"key": "%BPDF_PATH_VAR%"}"#.to_owned());
        assert!(result.contains(r#"C:\\dir\\\"file"#));
        unsafe { std::env::remove_var("BPDF_PATH_VAR") };
    }

    #[test]
    fn max_tokens_default_is_4096() {
        let config = Config::default();
        assert_eq!(config.ocr_max_tokens, 4096);
    }
}
