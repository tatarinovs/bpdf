use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use lopdf::Document;
use serde_json::{Value, json};
use tempfile::Builder;
use ureq::{Agent, Proxy};

use crate::encoding::base64;
use crate::formats::{self, Format};
use crate::hash::sha256_hex;
use crate::imageconv::{self, ImageOptions};
use crate::{atomic, output, pdf};

const MAX_ATTEMPTS: usize = 10;
const MAX_RETRY_WAIT: Duration = Duration::from_secs(120);
const TEXT_LAYER_THRESHOLD: usize = 100;

#[derive(Clone, Debug)]
pub struct OcrOptions {
    pub api_key: String,
    pub proxy: String,
    pub model: String,
    pub prompt: String,
    pub endpoint: String,
    pub timeout: Duration,
    pub force_image_ocr: bool,
    pub image: ImageOptions,
    pub jobs: usize,
    pub max_tokens: u32,
    pub cache_dir: Option<PathBuf>,
}

#[derive(Clone)]
pub struct OcrEngine {
    agent: Agent,
    options: OcrOptions,
    gate: Arc<RequestGate>,
    cache_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

struct RequestGate {
    maximum: usize,
    state: Mutex<GateState>,
    changed: Condvar,
}

struct GateState {
    active: usize,
    blocked_until: Instant,
}

struct RequestPermit<'a>(&'a RequestGate);

impl RequestGate {
    fn new(maximum: usize) -> Self {
        Self {
            maximum,
            state: Mutex::new(GateState {
                active: 0,
                blocked_until: Instant::now(),
            }),
            changed: Condvar::new(),
        }
    }

    fn enter(&self) -> RequestPermit<'_> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            let now = Instant::now();
            if state.blocked_until > now {
                let wait = state.blocked_until - now;
                let (next, _) = self
                    .changed
                    .wait_timeout(state, wait)
                    .unwrap_or_else(|error| error.into_inner());
                state = next;
            } else if state.active < self.maximum {
                state.active += 1;
                return RequestPermit(self);
            } else {
                state = self
                    .changed
                    .wait(state)
                    .unwrap_or_else(|error| error.into_inner());
            }
        }
    }

    fn defer(&self, duration: Duration) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.blocked_until = state.blocked_until.max(Instant::now() + duration);
        self.changed.notify_all();
    }
}

impl Drop for RequestPermit<'_> {
    fn drop(&mut self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.active = state.active.saturating_sub(1);
        self.0.changed.notify_all();
    }
}

impl OcrEngine {
    pub fn new(options: OcrOptions) -> Result<Self> {
        if options.jobs == 0 {
            bail!("OCR jobs must be at least 1");
        }

        let mut builder = Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .timeout_global(Some(options.timeout))
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(options.timeout));
        if !options.proxy.trim().is_empty() {
            builder = builder.proxy(Some(
                Proxy::new(options.proxy.trim()).context("invalid proxy URL")?,
            ));
        }
        let agent = builder.build().into();
        let jobs = options.jobs;
        Ok(Self {
            agent,
            options,
            gate: Arc::new(RequestGate::new(jobs)),
            cache_locks: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn extract_text(&self, path: &Path) -> Result<String> {
        self.extract_text_with_mode(path, true)
    }

    fn extract_text_with_mode(&self, path: &Path, parallel_pdf_images: bool) -> Result<String> {
        match formats::detect(path) {
            Some(Format::Pdf) => self.process_pdf(path, parallel_pdf_images),
            Some(format) if format.is_image() => {
                let image = imageconv::for_ocr(path, &self.options.image)?;
                self.run_vision(&image, file_label(path))
            }
            _ => bail!("unsupported OCR input: {}", path.display()),
        }
    }

    pub fn extract_many(&self, paths: &[PathBuf]) -> Vec<Result<String>> {
        if paths.len() <= 1 {
            return paths.iter().map(|path| self.extract_text(path)).collect();
        }
        parallel_map(paths, self.options.jobs, |path| {
            self.extract_text_with_mode(path, false)
        })
    }

    pub fn check_connection(&self) -> Result<()> {
        self.ensure_api_key()?;
        let endpoint = models_endpoint(&self.options.endpoint)?;
        let authorization = format!("Bearer {}", self.options.api_key);
        let _permit = self.gate.enter();
        let response = self
            .agent
            .get(&endpoint)
            .header("Authorization", &authorization)
            .call()
            .context("failed to connect to Groq through the configured network path")?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            bail!("Groq API returned HTTP {status}");
        }
        Ok(())
    }

    fn process_pdf(&self, path: &Path, parallel_images: bool) -> Result<String> {
        let document = pdf::load(path)?;
        let pages = document.get_pages();
        let native_text = pdf::extract_text(&document).unwrap_or_default();
        let mut parts = Vec::new();
        if !native_text.trim().is_empty() {
            parts.push(native_text.trim().to_owned());
        }

        if !self.options.force_image_ocr && has_text_layer(&native_text, pages.len()) {
            output::info(format!(
                "Text layer found ({} pages), skipping image OCR",
                pages.len()
            ));
            return Ok(parts.join("\n\n"));
        }

        let images = extract_pdf_images(&document, &self.options.image)?;
        if images.is_empty() {
            if parts.is_empty() {
                bail!("no extractable images or text found in {}", path.display());
            }
            return Ok(parts.join("\n\n"));
        }

        let recognize = |image: &ExtractedImage| {
            output::info(format!("OCR {}...", image.label));
            self.run_vision(&image.bytes, &image.label)
        };
        let results = if parallel_images {
            parallel_map(&images, self.options.jobs, recognize)
        } else {
            images.iter().map(recognize).collect()
        };
        let mut failures = 0usize;
        for (image, result) in images.iter().zip(results) {
            match result {
                Ok(text) if !text.trim().is_empty() => parts.push(text.trim().to_owned()),
                Ok(_) => {}
                Err(error) => {
                    failures += 1;
                    output::warn(format!("{}: {error:#}", image.label));
                }
            }
        }
        if failures == images.len() && parts.is_empty() {
            bail!("all {failures} embedded images failed OCR");
        }
        Ok(parts.join("\n\n---\n\n"))
    }

    fn run_vision(&self, original: &[u8], label: &str) -> Result<String> {
        let image = imageconv::optimize_for_ocr(original).unwrap_or_else(|error| {
            output::warn(format!("failed to optimize {label}: {error:#}"));
            original.to_vec()
        });
        let Some(cache_dir) = &self.options.cache_dir else {
            return self.run_vision_uncached(&image);
        };
        let key = sha256_hex(&[
            b"bpdf-ocr-v1\0",
            self.options.endpoint.as_bytes(),
            b"\0",
            self.options.model.as_bytes(),
            b"\0",
            self.options.prompt.as_bytes(),
            b"\0",
            &image,
        ]);
        let path = cache_dir.join(format!("{key}.md"));
        let item_lock = {
            let mut locks = self
                .cache_locks
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            locks
                .entry(key)
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = item_lock.lock().unwrap_or_else(|error| error.into_inner());
        match fs::read_to_string(&path) {
            Ok(text) => {
                output::info(format!("OCR cache hit: {label}"));
                return Ok(text);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                output::warn(format!("cannot read OCR cache {}: {error}", path.display()))
            }
        }

        let text = self.run_vision_uncached(&image)?;
        let cache_result: Result<()> = (|| {
            fs::create_dir_all(cache_dir)?;
            atomic::write_atomic(&path, text.as_bytes())?;
            Ok(())
        })();
        if let Err(error) = cache_result {
            output::warn(format!(
                "cannot write OCR cache {}: {error}",
                path.display()
            ));
        }
        Ok(text)
    }

    fn run_vision_uncached(&self, image: &[u8]) -> Result<String> {
        self.ensure_api_key()?;
        let mime = match image::guess_format(image).ok() {
            Some(image::ImageFormat::Png) => "image/png",
            Some(image::ImageFormat::WebP) => "image/webp",
            Some(image::ImageFormat::Gif) => "image/gif",
            _ => "image/jpeg",
        };
        let payload = json!({
            "model": self.options.model,
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": self.options.prompt},
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": format!("data:{mime};base64,{}", base64(image))
                        }
                    }
                ]
            }],
            "temperature": 0.0,
            "max_tokens": self.options.max_tokens
        });
        let authorization = format!("Bearer {}", self.options.api_key);

        for attempt in 1..=MAX_ATTEMPTS {
            let permit = self.gate.enter();
            let response = self
                .agent
                .post(&self.options.endpoint)
                .header("Authorization", &authorization)
                .send_json(&payload);
            let mut response = match response {
                Ok(response) => response,
                Err(error) if attempt < MAX_ATTEMPTS => {
                    let wait = Duration::from_secs((attempt * 2) as u64);
                    drop(permit);
                    self.gate.defer(wait);
                    output::warn(format!(
                        "Network error: {error}. Retrying in {}s ({attempt}/{MAX_ATTEMPTS})",
                        wait.as_secs()
                    ));
                    continue;
                }
                Err(error) => return Err(error).context("OCR request failed"),
            };

            let status = response.status().as_u16();
            let retry_header = response
                .headers()
                .get("Retry-After")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let body = response
                .body_mut()
                .read_to_string()
                .context("failed to read OCR response")?;
            drop(permit);

            if status == 429 && attempt < MAX_ATTEMPTS {
                let wait = retry_after(retry_header.as_deref(), &body, attempt);
                self.gate.defer(wait);
                output::warn(format!(
                    "Rate limited. Retrying in {:.1}s ({attempt}/{MAX_ATTEMPTS})",
                    wait.as_secs_f64()
                ));
                continue;
            }
            if !(200..300).contains(&status) {
                bail!("OCR API returned {status}: {body}");
            }

            let response: Value =
                serde_json::from_str(&body).context("invalid OCR API response JSON")?;
            let content = response
                .pointer("/choices/0/message/content")
                .and_then(Value::as_str)
                .context("OCR API response has no choices[0].message.content")?;
            return Ok(strip_think_blocks(content.trim()));
        }

        bail!("OCR exceeded {MAX_ATTEMPTS} attempts")
    }

    fn ensure_api_key(&self) -> Result<()> {
        if self.options.api_key.trim().is_empty() {
            bail!("Groq API key is empty; set groq_api_key in config");
        }
        Ok(())
    }
}

fn parallel_map<T, R, F>(items: &[T], jobs: usize, work: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
{
    if items.len() <= 1 || jobs == 1 {
        return items.iter().map(work).collect();
    }

    let next = AtomicUsize::new(0);
    let results = Mutex::new(
        std::iter::repeat_with(|| None)
            .take(items.len())
            .collect::<Vec<Option<R>>>(),
    );
    thread::scope(|scope| {
        for _ in 0..jobs.min(items.len()) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(index) else {
                        break;
                    };
                    let result = work(item);
                    results.lock().unwrap_or_else(|error| error.into_inner())[index] = Some(result);
                }
            });
        }
    });
    results
        .into_inner()
        .unwrap_or_else(|error| error.into_inner())
        .into_iter()
        .map(|result| result.expect("each parallel item is processed"))
        .collect()
}

fn models_endpoint(endpoint: &str) -> Result<String> {
    let endpoint = endpoint.trim_end_matches('/');
    let base = endpoint
        .strip_suffix("/chat/completions")
        .context("OCR endpoint must end with /chat/completions for doctor")?;
    Ok(format!("{base}/models"))
}

#[derive(Debug)]
pub struct ExtractedImage {
    pub label: String,
    pub bytes: Vec<u8>,
}

pub fn extract_pdf_images(
    document: &Document,
    options: &ImageOptions,
) -> Result<Vec<ExtractedImage>> {
    let mut output = Vec::new();
    for (page_number, page_id) in document.get_pages() {
        let images = match document.get_page_images(page_id) {
            Ok(images) => images,
            Err(error) => {
                output::warn(format!(
                    "page {page_number}: cannot inspect images: {error}"
                ));
                continue;
            }
        };
        for (index, image) in images.into_iter().enumerate() {
            let stream = document.get_object(image.id)?.as_stream()?;
            let filters = image.filters.unwrap_or_default();
            let label = format!("page-{page_number}-image-{}", index + 1);
            let bytes = if filters.iter().any(|filter| filter == "DCTDecode") {
                stream.content.clone()
            } else if filters.iter().any(|filter| filter == "JPXDecode") {
                convert_encoded_image(&stream.content, ".jp2", options)?
            } else {
                let image = pdf::image::decode(stream)
                    .with_context(|| format!("{label}: unsupported PDF image encoding"))?;
                imageconv::encode_jpeg_on_white(&image, options.jpeg_quality)?
            };
            output.push(ExtractedImage { label, bytes });
        }
    }
    Ok(output)
}

fn convert_encoded_image(bytes: &[u8], suffix: &str, options: &ImageOptions) -> Result<Vec<u8>> {
    let mut temporary = Builder::new().suffix(suffix).tempfile()?;
    temporary.write_all(bytes)?;
    temporary.as_file_mut().sync_all()?;
    imageconv::ffmpeg_to_jpeg(temporary.path(), options)
}

fn has_text_layer(text: &str, pages: usize) -> bool {
    if pages == 0 {
        return false;
    }
    let characters = text
        .chars()
        .filter(|character| !character.is_whitespace())
        .count();
    characters / pages >= TEXT_LAYER_THRESHOLD
}

fn retry_after(header: Option<&str>, body: &str, attempt: usize) -> Duration {
    let mut seconds = header
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.0);
    if seconds <= 0.0
        && let Some(rest) = body.split("try again in ").nth(1)
        && let Some(value) = rest.split('s').next()
    {
        seconds = value.trim().parse::<f64>().unwrap_or(0.0);
    }
    if seconds <= 0.0 {
        seconds = (attempt * 5) as f64;
    }
    Duration::from_secs_f64(seconds.max(0.1)).min(MAX_RETRY_WAIT)
}

fn strip_think_blocks(content: &str) -> String {
    let mut content = content.to_owned();
    loop {
        let Some(start) = content.find("<think>") else {
            return content;
        };
        let Some(relative_end) = content[start..].find("</think>") else {
            content.truncate(start);
            content.push_str("\n\n*[ВНИМАНИЕ: Ответ модели был оборван из-за лимита токенов]*");
            return content;
        };
        let end = start + relative_end + "</think>".len();
        content.replace_range(start..end, "");
        content = content.trim().to_owned();
    }
}

fn file_label(path: &Path) -> &str {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("image")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options_without_api_key() -> OcrOptions {
        let config = crate::config::Config::default();
        let image = config.image_options(None, None);
        OcrOptions {
            api_key: String::new(),
            proxy: String::new(),
            model: config.ocr_model,
            prompt: config.ocr_prompt,
            endpoint: config.ocr_endpoint,
            timeout: Duration::from_secs(1),
            force_image_ocr: false,
            image,
            jobs: 1,
            max_tokens: config.ocr_max_tokens,
            cache_dir: None,
        }
    }

    #[test]
    fn text_layer_threshold_is_per_page() {
        assert!(has_text_layer(&"a".repeat(200), 2));
        assert!(!has_text_layer(&"a".repeat(199), 2));
    }

    #[test]
    fn retry_after_uses_header_body_and_cap() {
        assert_eq!(retry_after(Some("1.5"), "", 1), Duration::from_millis(1500));
        assert_eq!(
            retry_after(None, "try again in 2.25s.", 1),
            Duration::from_millis(2250)
        );
        assert_eq!(retry_after(Some("999"), "", 1), MAX_RETRY_WAIT);
    }

    #[test]
    fn removes_thinking_blocks() {
        assert_eq!(
            strip_think_blocks("<think>hidden</think>\nanswer"),
            "answer"
        );
        assert!(strip_think_blocks("<think>cut").contains("ВНИМАНИЕ"));
    }

    #[test]
    fn parallel_map_preserves_input_order() {
        assert_eq!(
            parallel_map(&[3, 1, 2, 4], 3, |value| value * 2),
            vec![6, 2, 4, 8]
        );
    }

    #[test]
    fn derives_models_endpoint_without_exposing_credentials() {
        assert_eq!(
            models_endpoint("https://api.groq.com/openai/v1/chat/completions").unwrap(),
            "https://api.groq.com/openai/v1/models"
        );
    }

    #[test]
    fn native_pdf_text_does_not_require_api_key() {
        let temporary = tempfile::Builder::new().suffix(".pdf").tempfile().unwrap();
        let text = "Текстовый слой документа. ".repeat(20);
        let mut document =
            crate::textpdf::render(&text, &crate::textpdf::TextOptions::default()).unwrap();
        document.save(temporary.path()).unwrap();
        let engine = OcrEngine::new(options_without_api_key()).unwrap();

        let extracted = engine.extract_text(temporary.path()).unwrap();

        assert!(!extracted.trim().is_empty());
    }

    #[test]
    fn image_ocr_still_requires_api_key() {
        let engine = OcrEngine::new(options_without_api_key()).unwrap();
        let error = engine.run_vision_uncached(b"not-an-image").unwrap_err();
        assert!(error.to_string().contains("API key"));
    }
}
