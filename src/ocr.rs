use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use lopdf::Document;
use serde_json::{Value, json};
use ureq::{Agent, Proxy};

use crate::config::Config;
use crate::encoding::base64;
use crate::fileset::InputSpec;
use crate::formats::{self, Format};
use crate::hash::sha256_hex;
use crate::imageconv::{self, ImageOptions};
use crate::pdf::image::{self as pdf_image, ExtractedImage, Selection};
use crate::textpdf::{PageTextOverlay, overlay_searchable_text};
use crate::winocr::{OcrPageResult, OcrWordBox};
use crate::{atomic, output, parallel, pdf};

const MAX_ATTEMPTS: usize = 10;
const MAX_RETRY_WAIT: Duration = Duration::from_secs(120);
const TEXT_LAYER_THRESHOLD: usize = 100;
/// Images smaller than this (in pixels) carry no useful text for a
/// searchable layer.
const MIN_SEARCHABLE_IMAGE_AREA: u64 = 100_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OcrBackend {
    Groq,
    Windows,
    Auto,
}

impl OcrBackend {
    pub fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "groq" => Ok(Self::Groq),
            "windows" | "win" | "winocr" => {
                if !cfg!(windows) {
                    bail!("Windows Media OCR is only available on Windows");
                }
                Ok(Self::Windows)
            }
            "auto" => Ok(Self::Auto),
            _ => bail!("unknown OCR engine '{value}'; valid options are groq, windows, auto"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct OcrOptions {
    pub backend: OcrBackend,
    pub lang: Option<String>,
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

impl OcrOptions {
    /// Options from configuration alone: Groq backend, one job, no cache.
    pub fn from_config(config: &Config) -> Self {
        Self {
            backend: OcrBackend::Groq,
            lang: config.ocr_lang.clone(),
            api_key: config.groq_api_key.clone(),
            proxy: config.proxy.clone(),
            model: config.ocr_model.clone(),
            prompt: config.ocr_prompt.clone(),
            endpoint: config.ocr_endpoint.clone(),
            timeout: Duration::from_secs(config.ocr_timeout_seconds),
            force_image_ocr: false,
            image: config.image_options(None, None),
            jobs: 1,
            max_tokens: config.ocr_max_tokens,
            cache_dir: None,
        }
    }
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
                state = self
                    .changed
                    .wait_timeout(state, wait)
                    .unwrap_or_else(|error| error.into_inner())
                    .0;
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

fn build_agent(options: &OcrOptions) -> Result<Agent> {
    let mut builder = Agent::config_builder()
        .proxy(None)
        .http_status_as_error(false)
        .timeout_global(Some(options.timeout))
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(options.timeout));
    #[cfg(windows)]
    {
        use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
        // SChannel with the Windows certificate store.
        builder = builder.tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::NativeTls)
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        );
    }
    let proxy = options.proxy.trim();
    if !proxy.is_empty() {
        builder = builder.proxy(Some(Proxy::new(proxy).context("invalid proxy URL")?));
    }
    Ok(builder.build().into())
}

impl OcrEngine {
    pub fn new(options: OcrOptions) -> Result<Self> {
        if options.jobs == 0 {
            bail!("OCR jobs must be at least 1");
        }
        Ok(Self {
            agent: build_agent(&options)?,
            gate: Arc::new(RequestGate::new(options.jobs)),
            options,
            cache_locks: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn extract_spec(&self, spec: &InputSpec) -> Result<String> {
        self.extract_text_with_mode(&spec.path, spec.pages.as_deref(), true)
    }

    fn extract_text_with_mode(
        &self,
        path: &Path,
        pages_filter: Option<&str>,
        parallel_pdf_images: bool,
    ) -> Result<String> {
        match formats::detect(path) {
            Some(Format::Pdf) => self.process_pdf(path, pages_filter, parallel_pdf_images),
            Some(format) if format.is_image() => {
                reject_pages(path, pages_filter)?;
                let image = imageconv::for_ocr(path, &self.options.image)?;
                Ok(self.recognize_image(&image, file_label(path))?.text)
            }
            _ => bail!("unsupported OCR input: {}", path.display()),
        }
    }

    pub fn extract_many_specs(&self, specs: &[InputSpec]) -> Vec<Result<String>> {
        if specs.len() <= 1 {
            return specs.iter().map(|spec| self.extract_spec(spec)).collect();
        }
        parallel::map(specs, self.options.jobs, |spec| {
            self.extract_text_with_mode(&spec.path, spec.pages.as_deref(), false)
        })
    }

    /// A PDF with an invisible text layer: existing PDF pages keep their
    /// content, images become single-page documents.
    pub fn create_searchable_pdf_for_spec(
        &self,
        spec: &InputSpec,
        page_size: &str,
        font_path: Option<&Path>,
    ) -> Result<Document> {
        match formats::detect(&spec.path) {
            Some(Format::Pdf) => {
                let mut document = pdf::load(&spec.path)?;
                if let Some(pages) = &spec.pages {
                    pdf::select_pages(&mut document, pages)?;
                }
                let pages = pdf_image::extract_images(
                    &document,
                    Selection::AllFrom(MIN_SEARCHABLE_IMAGE_AREA),
                    self.options.image.jpeg_quality,
                )
                .into_iter()
                .filter(|(_, images)| !images.is_empty())
                .map(|(page_id, images)| {
                    let geometry = pdf::transform::page_geometry(&document, page_id).ok();
                    let size = geometry.map_or((595.0, 842.0), |geometry| {
                        (geometry.raw_width(), geometry.raw_height())
                    });
                    (page_id, size, images)
                })
                .collect::<Vec<_>>();
                if pages.is_empty() {
                    bail!("no extractable images found in {}", spec.path.display());
                }

                let overlays =
                    parallel::map(&pages, self.options.jobs, |(page_id, size, images)| {
                        self.page_overlay(*page_id, *size, images)
                    });
                overlay_searchable_text(&mut document, &overlays, font_path)?;
                Ok(document)
            }
            Some(format) if format.is_image() => {
                reject_pages(&spec.path, spec.pages.as_deref())?;
                let jpeg = imageconv::to_jpeg(&spec.path, &self.options.image, None)?;
                let info = pdf::jpeg_info(&jpeg)?;
                let (width, height) = (u32::from(info.width), u32::from(info.height));
                output::info(format!("OCR {} (searchable layer)...", spec.path.display()));
                let result = self.recognize_image(&jpeg, file_label(&spec.path))?;

                let placement = pdf::image_placement(width, height, page_size)?;
                let mut document = pdf::jpeg_document(jpeg, page_size)?;
                let page_id = *document
                    .get_pages()
                    .get(&1)
                    .context("image page is missing")?;
                // Image pixels -> page points, measured from the page top.
                let scale = placement.width / f64::from(width);
                let top = placement.page_height - placement.y - placement.height;
                let words = result
                    .words
                    .into_iter()
                    .map(|word| OcrWordBox {
                        x: placement.x + word.x * scale,
                        y: top + word.y * scale,
                        width: word.width * scale,
                        height: word.height * scale,
                        line_y: top + word.line_y * scale,
                        line_height: word.line_height * scale,
                        text: word.text,
                    })
                    .collect();
                let overlay = PageTextOverlay {
                    page_id,
                    page_width: placement.page_width,
                    page_height: placement.page_height,
                    scaled_words: words,
                    fallback_text: Some(result.text),
                };
                overlay_searchable_text(&mut document, &[overlay], font_path)?;
                Ok(document)
            }
            _ => bail!(
                "unsupported OCR input for PDF output: {}",
                spec.path.display()
            ),
        }
    }

    /// Recognise the images of one page and map their words onto the page,
    /// assuming each image covers the whole page.
    fn page_overlay(
        &self,
        page_id: lopdf::ObjectId,
        (page_width, page_height): (f64, f64),
        images: &[ExtractedImage],
    ) -> PageTextOverlay {
        let mut words = Vec::new();
        let mut fallback = Vec::new();
        for image in images {
            output::info(format!("OCR {} (searchable layer)...", image.label));
            let result = match self.recognize_image(&image.bytes, &image.label) {
                Ok(result) => result,
                Err(error) => {
                    output::warn(format!("Failed to OCR {}: {error:#}", image.label));
                    continue;
                }
            };
            let scale_x = page_width / f64::from(image.width.max(1));
            let scale_y = page_height / f64::from(image.height.max(1));
            words.extend(result.words.into_iter().map(|mut word| {
                word.x *= scale_x;
                word.y *= scale_y;
                word.width *= scale_x;
                word.height *= scale_y;
                word.line_y *= scale_y;
                word.line_height *= scale_y;
                word
            }));
            if !result.text.is_empty() {
                fallback.push(result.text);
            }
        }
        PageTextOverlay {
            page_id,
            page_width,
            page_height,
            scaled_words: words,
            fallback_text: (!fallback.is_empty()).then(|| fallback.join("\n\n")),
        }
    }

    pub fn check_connection(&self) -> Result<()> {
        self.ensure_api_key()?;
        let endpoint = models_endpoint(&self.options.endpoint)?;
        let _permit = self.gate.enter();
        let response = self
            .agent
            .get(&endpoint)
            .header("Authorization", &self.authorization())
            .call()
            .context("failed to connect to Groq through the configured network path")?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            bail!("Groq API returned HTTP {status}");
        }
        Ok(())
    }

    fn process_pdf(
        &self,
        path: &Path,
        pages_filter: Option<&str>,
        parallel_images: bool,
    ) -> Result<String> {
        let mut document = pdf::load(path)?;
        if let Some(pages) = pages_filter {
            pdf::select_pages(&mut document, pages)?;
        }
        let page_count = document.get_pages().len();
        let native_text = pdf::extract_text(&document).unwrap_or_default();
        let mut parts = Vec::new();
        if !native_text.trim().is_empty() {
            parts.push(native_text.trim().to_owned());
        }

        if !self.options.force_image_ocr && has_text_layer(&native_text, page_count) {
            output::info(format!(
                "Text layer found ({page_count} pages), skipping image OCR"
            ));
            return Ok(parts.join("\n\n"));
        }

        let images = extract_pdf_images(&document, &self.options.image);
        if images.is_empty() {
            if parts.is_empty() {
                bail!("no extractable images or text found in {}", path.display());
            }
            return Ok(parts.join("\n\n"));
        }

        let recognize = |image: &ExtractedImage| {
            output::info(format!("OCR {}...", image.label));
            self.recognize_image(&image.bytes, &image.label)
                .map(|result| result.text)
        };
        let jobs = if parallel_images {
            self.options.jobs
        } else {
            1
        };
        let results = parallel::map(&images, jobs, recognize);
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

    pub fn recognize_image(&self, original: &[u8], label: &str) -> Result<OcrPageResult> {
        match self.options.backend {
            OcrBackend::Windows => self.recognize_image_winocr(original, label),
            OcrBackend::Groq => self
                .run_vision(original, label)
                .map(OcrPageResult::text_only),
            OcrBackend::Auto => {
                if !self.options.api_key.trim().is_empty() {
                    match self.run_vision(original, label) {
                        Ok(text) => return Ok(OcrPageResult::text_only(text)),
                        Err(error) => output::warn(format!(
                            "Groq OCR failed for {label}: {error}, falling back to Windows OCR..."
                        )),
                    }
                }
                if cfg!(windows) && crate::winocr::is_available() {
                    self.recognize_image_winocr(original, label)
                } else {
                    bail!(
                        "no OCR engine available (Groq failed/missing key, Windows OCR unavailable)"
                    );
                }
            }
        }
    }

    fn recognize_image_winocr(&self, original: &[u8], label: &str) -> Result<OcrPageResult> {
        let lang = self.options.lang.as_deref();
        let key_parts: [&[u8]; 4] = [
            b"bpdf-ocr-winocr-v1\0",
            lang.unwrap_or("default").as_bytes(),
            b"\0",
            original,
        ];
        self.cached(
            &key_parts,
            "json",
            label,
            |data| serde_json::from_str(&data).ok(),
            |result| serde_json::to_string(result).ok(),
            || crate::winocr::recognize_image_bytes(original, lang),
        )
    }

    fn run_vision(&self, original: &[u8], label: &str) -> Result<String> {
        let image = imageconv::optimize_for_ocr(original).unwrap_or_else(|error| {
            output::warn(format!("failed to optimize {label}: {error:#}"));
            original.to_vec()
        });
        let key_parts: [&[u8]; 8] = [
            b"bpdf-ocr-v1\0",
            self.options.endpoint.as_bytes(),
            b"\0",
            self.options.model.as_bytes(),
            b"\0",
            self.options.prompt.as_bytes(),
            b"\0",
            &image,
        ];
        self.cached(
            &key_parts,
            "md",
            label,
            Some,
            |text| Some(text.clone()),
            || self.run_vision_uncached(&image),
        )
    }

    /// Content-addressed cache around an OCR call. Concurrent requests for
    /// the same key wait for the first one instead of repeating it.
    fn cached<T>(
        &self,
        key_parts: &[&[u8]],
        extension: &str,
        label: &str,
        decode: impl FnOnce(String) -> Option<T>,
        encode: impl FnOnce(&T) -> Option<String>,
        compute: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let Some(cache_dir) = &self.options.cache_dir else {
            return compute();
        };
        let key = sha256_hex(key_parts);
        let path = cache_dir.join(format!("{key}.{extension}"));
        let item_lock = self
            .cache_locks
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(key)
            .or_default()
            .clone();
        let _guard = item_lock.lock().unwrap_or_else(|error| error.into_inner());

        match fs::read_to_string(&path) {
            Ok(data) => {
                if let Some(value) = decode(data) {
                    output::info(format!("OCR cache hit: {label}"));
                    return Ok(value);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                output::warn(format!("cannot read OCR cache {}: {error}", path.display()));
            }
        }

        let value = compute()?;
        if let Some(data) = encode(&value)
            && let Err(error) = fs::create_dir_all(cache_dir)
                .map_err(anyhow::Error::from)
                .and_then(|()| atomic::write_atomic(&path, data.as_bytes()))
        {
            output::warn(format!(
                "cannot write OCR cache {}: {error:#}",
                path.display()
            ));
        }
        Ok(value)
    }

    fn authorization(&self) -> String {
        format!("Bearer {}", self.options.api_key)
    }

    fn run_vision_uncached(&self, image: &[u8]) -> Result<String> {
        self.ensure_api_key()?;
        let mime = match image::guess_format(image) {
            Ok(image::ImageFormat::Png) => "image/png",
            Ok(image::ImageFormat::WebP) => "image/webp",
            Ok(image::ImageFormat::Gif) => "image/gif",
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
        let authorization = self.authorization();

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

fn reject_pages(path: &Path, pages: Option<&str>) -> Result<()> {
    if pages.is_some() {
        return Err(crate::commands::common::err_pdf_only_page_ranges(path));
    }
    Ok(())
}

fn models_endpoint(endpoint: &str) -> Result<String> {
    let base = endpoint
        .trim_end_matches('/')
        .strip_suffix("/chat/completions")
        .context("OCR endpoint must end with /chat/completions for doctor")?;
    Ok(format!("{base}/models"))
}

/// The largest image of every page, oriented like the page.
pub fn extract_pdf_images(document: &Document, options: &ImageOptions) -> Vec<ExtractedImage> {
    pdf_image::extract_images(document, Selection::Largest, options.jpeg_quality)
        .into_iter()
        .flat_map(|(_, images)| images)
        .collect()
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
        OcrOptions {
            api_key: String::new(),
            timeout: Duration::from_secs(1),
            ..OcrOptions::from_config(&Config::default())
        }
    }

    #[test]
    fn ocr_backend_parsing() {
        assert_eq!(OcrBackend::parse("groq").unwrap(), OcrBackend::Groq);
        assert_eq!(OcrBackend::parse("auto").unwrap(), OcrBackend::Auto);
        if cfg!(windows) {
            assert_eq!(OcrBackend::parse("windows").unwrap(), OcrBackend::Windows);
            assert_eq!(OcrBackend::parse("winocr").unwrap(), OcrBackend::Windows);
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
    fn derives_models_endpoint_without_exposing_credentials() {
        assert_eq!(
            models_endpoint("https://api.groq.com/openai/v1/chat/completions").unwrap(),
            "https://api.groq.com/openai/v1/models"
        );
    }

    #[test]
    fn winocr_cache_round_trips_word_boxes() {
        let result = OcrPageResult {
            text: "Слово".to_owned(),
            words: vec![OcrWordBox {
                text: "Слово".to_owned(),
                x: 1.0,
                y: 2.0,
                width: 3.0,
                height: 4.0,
                line_y: 2.0,
                line_height: 4.0,
            }],
            image_width: 10,
            image_height: 20,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"width\":10"));
        let parsed: OcrPageResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.words[0].line_height, 4.0);
        assert_eq!((parsed.image_width, parsed.image_height), (10, 20));
    }

    #[test]
    fn cache_returns_stored_value_without_recomputing() {
        let directory = tempfile::tempdir().unwrap();
        let engine = OcrEngine::new(OcrOptions {
            cache_dir: Some(directory.path().to_path_buf()),
            ..options_without_api_key()
        })
        .unwrap();
        let store = |value: &str| {
            engine.cached(
                &[b"key"],
                "md",
                "test",
                Some,
                |text| Some(text.clone()),
                || Ok(value.to_owned()),
            )
        };
        assert_eq!(store("first").unwrap(), "first");
        assert_eq!(store("second").unwrap(), "first");
    }

    #[test]
    fn native_pdf_text_does_not_require_api_key() {
        let temporary = tempfile::Builder::new().suffix(".pdf").tempfile().unwrap();
        let text = "Текстовый слой документа. ".repeat(20);
        let Ok(mut document) =
            crate::textpdf::render(&text, &crate::textpdf::TextOptions::default())
        else {
            return;
        };
        document.save(temporary.path()).unwrap();
        let engine = OcrEngine::new(options_without_api_key()).unwrap();

        let extracted = engine
            .extract_text_with_mode(temporary.path(), None, false)
            .unwrap();

        assert!(!extracted.trim().is_empty());
    }

    #[test]
    fn native_pdf_text_with_page_filter() {
        let temporary = tempfile::Builder::new().suffix(".pdf").tempfile().unwrap();
        let options = crate::textpdf::TextOptions::default();
        let (Ok(first), Ok(second)) = (
            crate::textpdf::render("Первая страница", &options),
            crate::textpdf::render("Вторая страница", &options),
        ) else {
            return;
        };
        let mut merged = crate::pdf::merge_documents(vec![first, second]).unwrap();
        merged.save(temporary.path()).unwrap();
        let engine = OcrEngine::new(options_without_api_key()).unwrap();

        let spec = InputSpec {
            path: temporary.path().to_path_buf(),
            pages: Some("2".to_owned()),
        };
        let extracted = engine.extract_spec(&spec).unwrap();
        assert!(extracted.contains("Вторая"));
        assert!(!extracted.contains("Первая"));
    }

    #[test]
    fn image_ocr_still_requires_api_key() {
        let engine = OcrEngine::new(options_without_api_key()).unwrap();
        let error = engine.run_vision_uncached(b"not-an-image").unwrap_err();
        assert!(error.to_string().contains("API key"));
    }
}
