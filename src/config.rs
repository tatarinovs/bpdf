use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

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
            ocr_cache: true,
            ocr_cache_dir: default_cache_dir(),
            image_dpi: 150,
        }
    }
}

impl Config {
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
        let mut clean_json = text
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        clean_json = expand_env_vars(clean_json);

        let config: Self = serde_json::from_str(&clean_json)
            .context("syntax error or invalid field type")?;

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
                let escaped = val.replace('\\', "\\\\");
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
}
