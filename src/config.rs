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
    pub bookmarks: bool,
    pub ocr_engine: String,
    pub ocr_lang: Option<String>,
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
            bookmarks: false,
            ocr_engine: "groq".to_owned(),
            ocr_lang: None,
            ocr_model: DEFAULT_OCR_MODEL.to_owned(),
            ocr_prompt: DEFAULT_OCR_PROMPT.to_owned(),
            ocr_endpoint: DEFAULT_OCR_ENDPOINT.to_owned(),
            ffmpeg: PathBuf::from("ffmpeg"),
            powershell: PathBuf::from("powershell.exe"),
            font_path: None,
            jpeg_quality: 95,
            office_timeout_seconds: 120,
            ocr_timeout_seconds: 120,
            ocr_jobs: 1,
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
            long_edge: None,
            short_edge: None,
            orient: None,
            rotation_degrees: None,
            force_reencode: false,
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
        let clean_text = expand_env_string(text);

        let config: Self = basic_toml::from_str(&clean_text)
            .context("syntax error or invalid field type in configuration")?;

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
    let mut candidates = vec![PathBuf::from("config.toml")];
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        let next_to_executable = directory.join("config.toml");
        if !candidates.contains(&next_to_executable) {
            candidates.push(next_to_executable);
        }
    }
    candidates
}

fn expand_env_string(text: &str) -> String {
    let mut result = text.to_owned();
    let mut i = 0;
    while let Some(start) = result[i..].find('%') {
        let absolute_start = i + start;
        if let Some(end) = result[absolute_start + 1..].find('%') {
            let absolute_end = absolute_start + 1 + end;
            let var_name = &result[absolute_start + 1..absolute_end];

            // Reject variable names with whitespace (avoids treating '% 10 % 20' as an env var)
            if var_name.contains(|c: char| c.is_whitespace()) || var_name.is_empty() {
                i = absolute_start + 1;
                continue;
            }

            if let Ok(val) = std::env::var(var_name) {
                result.replace_range(absolute_start..=absolute_end, &val);
                i = absolute_start + val.len();
            } else {
                i = absolute_end + 1;
            }
        } else {
            break;
        }
    }
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
    fn toml_example_file_parses_successfully() {
        let example_toml = include_str!("../config.example.toml");
        let config = Config::parse(example_toml).expect("config.example.toml must be valid");
        assert_eq!(config.page_size, "A4");
        assert_eq!(config.jpeg_quality, 95);
        assert_eq!(config.ocr_engine, "groq");
    }

    #[test]
    fn toml_dist_config_parses_successfully() {
        let dist_toml = include_str!("../dist/config.toml");
        let config = Config::parse(dist_toml).expect("dist/config.toml must be valid");
        assert!(config.auto_rotate);
        assert!(config.keep_icc);
        assert!(config.optimize);
        assert!(config.strip_metadata);
        assert_eq!(config.page_size, "A4");
        assert_eq!(config.jpeg_quality, 95);
        assert_eq!(config.image_dpi, 150);
        assert_eq!(config.ocr_jobs, 1);
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

    #[test]
    fn expand_env_string_replaces_known_variable() {
        unsafe { std::env::set_var("BPDF_TEST_VAR", "hello") };
        let result = expand_env_string("%BPDF_TEST_VAR% world");
        assert_eq!(result, "hello world");
        unsafe { std::env::remove_var("BPDF_TEST_VAR") };
    }

    #[test]
    fn expand_env_string_ignores_unknown_variable() {
        let result = expand_env_string("%BPDF_DEFINITELY_NOT_SET% world");
        assert_eq!(result, "%BPDF_DEFINITELY_NOT_SET% world");
    }

    #[test]
    fn expand_env_string_rejects_whitespace_names() {
        let result = expand_env_string("% not a var % rest");
        assert_eq!(result, "% not a var % rest");
    }

    #[test]
    fn expand_env_in_config_fields() {
        unsafe {
            std::env::set_var("BPDF_PATH_VAR", r"C:\Tools\bin");
            std::env::set_var("BPDF_KEY_VAR", "secret_key_123");
        }
        let config = Config::parse(
            r#"
                groq_api_key = "%BPDF_KEY_VAR%"
                ffmpeg = '%BPDF_PATH_VAR%\ffmpeg.exe'
            "#,
        )
        .unwrap();
        assert_eq!(config.groq_api_key, "secret_key_123");
        assert_eq!(config.ffmpeg, PathBuf::from(r"C:\Tools\bin\ffmpeg.exe"));
        unsafe {
            std::env::remove_var("BPDF_PATH_VAR");
            std::env::remove_var("BPDF_KEY_VAR");
        }
    }

    #[test]
    fn max_tokens_default_is_4096() {
        let config = Config::default();
        assert_eq!(config.ocr_max_tokens, 4096);
    }
}
