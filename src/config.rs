use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub const DEFAULT_OCR_ENDPOINT: &str = "https://api.groq.com/openai/v1/chat/completions";
pub const DEFAULT_OCR_MODEL: &str = "qwen/qwen3.6-27b";
pub const DEFAULT_OCR_PROMPT: &str = "You are a highly accurate OCR engine. Extract all text exactly as it appears. Preserve layout, lists, and tables using Markdown. IMPORTANT: Do NOT extract or transcribe any text from stamps or seals (печати и штампы). Ignore them completely. Keep your reasoning/thinking to an absolute minimum (under 50 words) and immediately output the extracted text. Do not add any conversational filler.";

#[derive(Clone, Debug)]
pub struct Config {
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
        let mut config = Self::default();

        for (index, raw_line) in text.lines().enumerate() {
            let line = strip_yaml_comment(raw_line).trim();
            if line.is_empty() {
                continue;
            }
            let Some((key, raw_value)) = line.split_once(':') else {
                bail!("line {}: expected key: value", index + 1);
            };
            let key = key.trim();
            let value = parse_scalar(raw_value.trim())
                .with_context(|| format!("line {} ({key})", index + 1))?;

            match key {
                "groq_api_key" => config.groq_api_key = value,
                "proxy" => config.proxy = value,
                "author" => config.author = value,
                "creator" => config.creator = value,
                "auto_rotate" => config.auto_rotate = parse_bool(&value, key)?,
                "keep_icc" => config.keep_icc = parse_bool(&value, key)?,
                "optimize" => config.optimize = parse_bool(&value, key)?,
                "page_size" => config.page_size = value,
                "strip_metadata" => config.strip_metadata = parse_bool(&value, key)?,
                "ocr_model" => config.ocr_model = value,
                "ocr_prompt" => config.ocr_prompt = value,
                "ocr_endpoint" => config.ocr_endpoint = value,
                "ffmpeg" => config.ffmpeg = PathBuf::from(value),
                "powershell" => config.powershell = PathBuf::from(value),
                "font_path" => {
                    config.font_path = (!value.is_empty()).then(|| PathBuf::from(value));
                }
                "jpeg_quality" => {
                    config.jpeg_quality = value
                        .parse::<u8>()
                        .with_context(|| format!("{key} must be between 1 and 100"))?;
                    if !(1..=100).contains(&config.jpeg_quality) {
                        bail!("{key} must be between 1 and 100");
                    }
                }
                "office_timeout_seconds" => {
                    config.office_timeout_seconds = parse_positive_u64(&value, key)?;
                }
                "ocr_timeout_seconds" => {
                    config.ocr_timeout_seconds = parse_positive_u64(&value, key)?;
                }
                "ocr_jobs" => {
                    config.ocr_jobs = parse_positive_u64(&value, key)?
                        .try_into()
                        .context("ocr_jobs is too large")?;
                }
                "ocr_cache" => config.ocr_cache = parse_bool(&value, key)?,
                "ocr_cache_dir" => {
                    if !value.is_empty() {
                        config.ocr_cache_dir = PathBuf::from(value);
                    }
                }
                _ => crate::output::warn(format!("unknown config key {key}, ignoring")),
            }
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
    let mut candidates = vec![PathBuf::from("config.yaml")];
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        let next_to_executable = directory.join("config.yaml");
        if next_to_executable != candidates[0] {
            candidates.push(next_to_executable);
        }
    }
    candidates
}

fn strip_yaml_comment(line: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote == Some('"') {
            escaped = true;
            continue;
        }
        if character == '\'' || character == '"' {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            }
            continue;
        }
        if character == '#' && quote.is_none() {
            return &line[..index];
        }
    }
    line
}

fn parse_scalar(value: &str) -> Result<String> {
    if value.starts_with('"') {
        return serde_json::from_str(value).context("invalid double-quoted string");
    }
    if value.starts_with('\'') {
        if !value.ends_with('\'') || value.len() < 2 {
            bail!("unterminated single-quoted string");
        }
        return Ok(value[1..value.len() - 1].replace("''", "'"));
    }
    Ok(value.trim().to_owned())
}

fn parse_bool(value: &str, key: &str) -> Result<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        _ => bail!("{key} must be true or false"),
    }
}

fn parse_positive_u64(value: &str, key: &str) -> Result<u64> {
    let number = value
        .parse::<u64>()
        .with_context(|| format!("{key} must be a positive integer"))?;
    if number == 0 {
        bail!("{key} must be positive");
    }
    Ok(number)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flat_yaml_without_a_yaml_dependency() {
        let config = Config::parse(
            r#"
                # comment
                auto_rotate: true
                keep_icc: false
                page_size: "Letter"
                ocr_prompt: "Keep # signs and: colons"
                jpeg_quality: 91
                font_path: 'C:\Windows\Fonts\arial.ttf'
                ocr_jobs: 3
                ocr_cache: false
                ocr_cache_dir: 'D:\cache\bpdf'
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
    fn rejects_bad_boolean() {
        assert!(Config::parse("optimize: perhaps").is_err());
    }
}
