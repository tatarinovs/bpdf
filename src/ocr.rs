use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
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
use crate::pdf::PageSize;
use crate::pdf::image::{self as pdf_image, ExtractedImage, Selection};
use crate::pdf::transform::PageGeometry;
use crate::textpdf::{OCR_FONT_NAME, PageTextOverlay, overlay_searchable_text};
use crate::winocr::{OcrPageResult, OcrWordBox};
use crate::{atomic, output, parallel, pdf};

const MAX_ATTEMPTS: usize = 10;
const MAX_RETRY_WAIT: Duration = Duration::from_secs(120);
const TEXT_LAYER_THRESHOLD: usize = 100;
/// Images smaller than this (in pixels) carry no useful text for a
/// searchable layer.
const MIN_SEARCHABLE_IMAGE_AREA: u64 = 100_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
pub enum OcrBackend {
    Groq,
    #[value(alias = "win", alias = "winocr")]
    Windows,
    Auto,
}

impl FromStr for OcrBackend {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, String> {
        <Self as clap::ValueEnum>::from_str(value, true).map_err(|_| {
            format!("unknown OCR engine '{value}'; valid options are groq, windows, auto")
        })
    }
}

impl<'de> serde::Deserialize<'de> for OcrBackend {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
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
            image: config.image_options(),
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
    /// Set when Groq refused the API key; `auto` then uses Windows OCR only.
    groq_refused: Arc<AtomicBool>,
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
        let mut state = crate::parallel::lock(&self.state);
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
        let mut state = crate::parallel::lock(&self.state);
        state.blocked_until = state.blocked_until.max(Instant::now() + duration);
        self.changed.notify_all();
    }
}

impl Drop for RequestPermit<'_> {
    fn drop(&mut self) {
        let mut state = crate::parallel::lock(&self.0.state);
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
        if options.backend == OcrBackend::Windows && !cfg!(windows) {
            bail!("Windows Media OCR is only available on Windows");
        }
        Ok(Self {
            agent: build_agent(&options)?,
            gate: Arc::new(RequestGate::new(options.jobs)),
            options,
            cache_locks: Arc::new(Mutex::new(HashMap::new())),
            groq_refused: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn extract_spec(&self, spec: &InputSpec) -> Result<String> {
        self.extract_many_specs(std::slice::from_ref(spec))
            .pop()
            .expect("one result per input")
    }

    /// Text of every input. The images of all inputs share one queue of
    /// `jobs` workers, so a long PDF next to small files keeps them all busy;
    /// inputs are prepared in chunks to bound the memory held by images.
    pub fn extract_many_specs(&self, specs: &[InputSpec]) -> Vec<Result<String>> {
        let chunk = (self.options.jobs * 4).max(8);
        specs
            .chunks(chunk)
            .flat_map(|chunk| self.extract_chunk(chunk))
            .collect()
    }

    fn extract_chunk(&self, specs: &[InputSpec]) -> Vec<Result<String>> {
        let jobs = parallel::map(specs, parallel::cpu_jobs(), |spec| self.prepare_text(spec));
        let tasks = jobs
            .iter()
            .enumerate()
            .filter_map(|(index, job)| Some((index, job.as_ref().ok()?)))
            .flat_map(|(index, job)| (0..job.images.len()).map(move |image| (index, image)))
            .collect::<Vec<_>>();
        let mut results = parallel::map(&tasks, self.options.jobs, |&(index, image)| {
            let Ok(job) = &jobs[index] else {
                unreachable!("tasks come from prepared jobs")
            };
            let image = &job.images[image].1;
            output::info(format!("OCR {}...", image.label));
            self.recognize_image(&image.bytes, &image.label)
        })
        .into_iter();
        jobs.into_iter()
            .zip(specs)
            .map(|(job, spec)| {
                let job = job?;
                let recognised = results.by_ref().take(job.images.len()).collect();
                job.finish(recognised, &spec.path)
            })
            .collect()
    }

    /// Known text and the images still to recognise for one input.
    fn prepare_text(&self, spec: &InputSpec) -> Result<TextJob> {
        let path = &spec.path;
        match formats::detect(path) {
            Some(Format::Pdf) => {
                let mut document = pdf::load(path)?;
                if let Some(pages) = &spec.pages {
                    pdf::select_pages(&mut document, pages)?;
                }
                // Every page contributes its own text layer, or the
                // recognised text of its largest image when it has none.
                let texts = page_texts(&document);
                let pending = self.pages_to_recognise(&document, &texts);
                if pending.is_empty() {
                    output::info(format!(
                        "Text layer found on all {} pages of {}, skipping image OCR",
                        texts.len(),
                        path.display()
                    ));
                }
                let images = pending
                    .iter()
                    .flat_map(|&(number, page_id)| {
                        pdf_image::page_images(
                            &document,
                            number,
                            page_id,
                            Selection::Largest,
                            self.options.image.jpeg_quality,
                        )
                        .into_iter()
                        .map(move |image| (number, image))
                    })
                    .collect();
                Ok(TextJob {
                    texts,
                    images,
                    is_pdf: true,
                })
            }
            Some(format) if format.is_image() => {
                spec.reject_pages()?;
                let bytes = imageconv::for_ocr(path, &self.options.image)?;
                let image = ExtractedImage {
                    id: None,
                    label: file_label(path).to_owned(),
                    bytes,
                    width: 0,
                    height: 0,
                };
                Ok(TextJob {
                    texts: BTreeMap::new(),
                    images: vec![(1, image)],
                    is_pdf: false,
                })
            }
            _ => bail!("unsupported OCR input: {}", path.display()),
        }
    }

    /// A PDF with an invisible text layer: existing PDF pages keep their
    /// content, images become single-page documents. Pages that already
    /// carry text are left alone unless OCR is forced. A page range of the
    /// input selects the pages of the result, or with `keep_all_pages` only
    /// the pages to recognise (the source is rewritten in place).
    pub fn create_searchable_pdf_for_spec(
        &self,
        spec: &InputSpec,
        page_size: PageSize,
        font_path: Option<&Path>,
        keep_all_pages: bool,
    ) -> Result<Document> {
        match formats::detect(&spec.path) {
            Some(Format::Pdf) => {
                let mut document = pdf::load(&spec.path)?;
                let mut only = None;
                if let Some(pages) = &spec.pages {
                    if keep_all_pages {
                        let count = document.get_pages().len();
                        only = Some(pdf::parse_page_selection(pages, count)?);
                    } else {
                        pdf::select_pages(&mut document, pages)?;
                    }
                }
                let mut pages = self.pages_to_recognise(&document, &page_texts(&document));
                if let Some(only) = &only {
                    pages.retain(|(number, _)| only.contains(&(*number as usize)));
                }
                let scans = parallel::map(&pages, parallel::cpu_jobs(), |&(number, page_id)| {
                    let images = pdf_image::page_images(
                        &document,
                        number,
                        page_id,
                        Selection::AllFrom(MIN_SEARCHABLE_IMAGE_AREA),
                        self.options.image.jpeg_quality,
                    );
                    (page_id, images)
                })
                .into_iter()
                .filter(|(_, images)| !images.is_empty())
                .map(|(page_id, images)| {
                    let crop = pdf::transform::page_crop_geometry(&document, page_id)?;
                    let boxes = pdf_image::placement_boxes(&document, page_id).unwrap_or_default();
                    let images = images
                        .into_iter()
                        .map(|image| {
                            let area = image
                                .id
                                .and_then(|id| boxes.get(&id))
                                .map(|bounds| displayed_area(crop, *bounds));
                            (image, area)
                        })
                        .collect::<Vec<_>>();
                    Ok((page_id, crop, images))
                })
                .collect::<Result<Vec<_>>>()?;
                if scans.is_empty() {
                    if pages.is_empty() {
                        output::info(format!(
                            "{} already has a text layer on every page",
                            spec.path.display()
                        ));
                        return Ok(document);
                    }
                    bail!("no extractable images found in {}", spec.path.display());
                }

                let overlays =
                    parallel::map(&scans, self.options.jobs, |(page_id, crop, images)| {
                        self.page_overlay(*page_id, *crop, images)
                    });
                overlay_searchable_text(&mut document, &overlays, font_path)?;
                Ok(document)
            }
            Some(format) if format.is_image() => {
                spec.reject_pages()?;
                let mut jpeg = imageconv::to_jpeg(&spec.path, &self.options.image, None)?;
                // Word boxes refer to the pixels the engine sees, so a photo
                // rotated only by its EXIF tag is made upright first.
                if crate::metadata::jpeg_orientation(&jpeg) != 1 {
                    jpeg = imageconv::encode_jpeg_on_white(
                        &imageconv::decode_bytes(&jpeg)?,
                        self.options.image.jpeg_quality,
                    )?;
                }
                let info = pdf::jpeg_info(&jpeg)?;
                let (width, height) = (u32::from(info.width), u32::from(info.height));
                output::info(format!("OCR {} (searchable layer)...", spec.path.display()));
                let result = self.recognize_image(&jpeg, file_label(&spec.path))?;

                let mut document = pdf::jpeg_document(jpeg, page_size)?;
                let page_id = *document
                    .get_pages()
                    .get(&1)
                    .context("image page is missing")?;
                let geometry = pdf::transform::page_geometry(&document, page_id)?;
                let placement = pdf::image_placement(
                    width,
                    height,
                    page_size,
                    // The size the page was given, whatever resolution it took.
                    (page_size == PageSize::Fit)
                        .then(|| 72.0 * f64::from(width) / geometry.raw_width()),
                );
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
                    matrix: crate::textpdf::IDENTITY,
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

    /// Recognise the images of one page and map their words onto the area
    /// each image covers on the visible page (`[left, top, width, height]`
    /// from the top-left corner), or the whole page when it is not known.
    /// Extracted images are upright, so words are placed on the displayed
    /// page and `matrix` turns them with it.
    fn page_overlay(
        &self,
        page_id: lopdf::ObjectId,
        crop: PageGeometry,
        images: &[(ExtractedImage, Option<[f64; 4]>)],
    ) -> PageTextOverlay {
        let (page_width, page_height) = (crop.display_width(), crop.display_height());
        let mut words = Vec::new();
        let mut fallback = Vec::new();
        for (image, area) in images {
            output::info(format!("OCR {} (searchable layer)...", image.label));
            let result = match self.recognize_image(&image.bytes, &image.label) {
                Ok(result) => result,
                Err(error) => {
                    output::warn(format!("Failed to OCR {}: {error:#}", image.label));
                    continue;
                }
            };
            let [left, top, width, height] = area.unwrap_or([0.0, 0.0, page_width, page_height]);
            let scale_x = width / f64::from(image.width.max(1));
            let scale_y = height / f64::from(image.height.max(1));
            words.extend(result.words.into_iter().map(|mut word| {
                word.x = left + word.x * scale_x;
                word.y = top + word.y * scale_y;
                word.width *= scale_x;
                word.height *= scale_y;
                word.line_y = top + word.line_y * scale_y;
                word.line_height *= scale_y;
                word
            }));
            if !result.text.is_empty() {
                fallback.push(result.text);
            }
        }
        PageTextOverlay {
            page_id,
            matrix: pdf::transform::upright_stamp_matrix(crop, 0.0, 0.0, 1.0, 1.0),
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

    /// Pages (number, id) that need OCR: all with `--force-ocr`, otherwise
    /// those without a meaningful text layer of their own (`texts`).
    fn pages_to_recognise(
        &self,
        document: &Document,
        texts: &BTreeMap<u32, String>,
    ) -> Vec<(u32, lopdf::ObjectId)> {
        document
            .get_pages()
            .into_iter()
            .filter(|&(number, page_id)| {
                self.options.force_image_ocr
                    || !has_text_layer(
                        document,
                        page_id,
                        texts.get(&number).map_or("", String::as_str),
                    )
            })
            .collect()
    }

    pub fn recognize_image(&self, original: &[u8], label: &str) -> Result<OcrPageResult> {
        match self.options.backend {
            OcrBackend::Windows => self.recognize_image_winocr(original, label),
            OcrBackend::Groq => self
                .run_vision(original, label)
                .map(OcrPageResult::text_only),
            OcrBackend::Auto => {
                if !self.options.api_key.trim().is_empty()
                    && !self.groq_refused.load(Ordering::Relaxed)
                {
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
        let item_lock = crate::parallel::lock(&self.cache_locks)
            .entry(key)
            .or_default()
            .clone();
        let _guard = crate::parallel::lock(&item_lock);

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
            let backoff = Duration::from_secs((attempt * 2) as u64);
            let mut response = match response {
                Ok(response) => response,
                Err(error) if attempt < MAX_ATTEMPTS => {
                    drop(permit);
                    self.gate.defer(backoff);
                    output::warn(format!(
                        "Network error: {error}. Retrying in {}s ({attempt}/{MAX_ATTEMPTS})",
                        backoff.as_secs()
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
            // Overloaded or restarting servers usually recover shortly.
            if matches!(status, 500 | 502 | 503 | 504) && attempt < MAX_ATTEMPTS {
                self.gate.defer(backoff);
                output::warn(format!(
                    "OCR server error {status}. Retrying in {}s ({attempt}/{MAX_ATTEMPTS})",
                    backoff.as_secs()
                ));
                continue;
            }
            if matches!(status, 401 | 403) {
                self.groq_refused.store(true, Ordering::Relaxed);
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
            let mut text = strip_think_blocks(content.trim());
            if response
                .pointer("/choices/0/finish_reason")
                .and_then(Value::as_str)
                == Some("length")
                && !text.ends_with(TRUNCATION_NOTE)
            {
                output::warn(format!(
                    "the OCR answer reached ocr_max_tokens ({}) and is cut off",
                    self.options.max_tokens
                ));
                text.push_str(TRUNCATION_NOTE);
            }
            return Ok(text);
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

/// Text work for one input: text already known per page number and the
/// images of the pages still to recognise.
struct TextJob {
    texts: BTreeMap<u32, String>,
    images: Vec<(u32, ExtractedImage)>,
    is_pdf: bool,
}

impl TextJob {
    /// Pages in order, recognised text replacing the page's own.
    fn finish(mut self, recognised: Vec<Result<OcrPageResult>>, path: &Path) -> Result<String> {
        let mut first_error = None;
        for ((number, image), result) in self.images.iter().zip(recognised) {
            match result {
                Ok(result) if !result.text.trim().is_empty() => {
                    self.texts.insert(*number, result.text.trim().to_owned());
                }
                Ok(_) => {}
                Err(error) => {
                    if self.images.len() > 1 {
                        output::warn(format!("{}: {error:#}", image.label));
                    }
                    first_error.get_or_insert(error);
                }
            }
        }
        let parts = self
            .texts
            .into_values()
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>();
        match first_error {
            Some(error) if parts.is_empty() => Err(error),
            None if parts.is_empty() && self.is_pdf => {
                bail!("no extractable images or text found in {}", path.display())
            }
            _ => Ok(parts.join("\n\n")),
        }
    }
}

fn models_endpoint(endpoint: &str) -> Result<String> {
    let base = endpoint
        .trim_end_matches('/')
        .strip_suffix("/chat/completions")
        .context("OCR endpoint must end with /chat/completions for doctor")?;
    Ok(format!("{base}/models"))
}

/// The existing text layer of every page, by page number.
fn page_texts(document: &Document) -> BTreeMap<u32, String> {
    document
        .get_pages()
        .into_keys()
        .map(|number| {
            let text = document
                .extract_text_with_limit(&[number], pdf::MAX_DECOMPRESSED_BYTES)
                .unwrap_or_default();
            (number, text)
        })
        .collect()
}

/// A page has a text layer when bpdf already added its OCR layer, or when
/// it carries a real amount of text (a page number alone does not count).
fn has_text_layer(document: &Document, page_id: lopdf::ObjectId, text: &str) -> bool {
    let has_bpdf_layer = document.get_page_fonts(page_id).is_ok_and(|fonts| {
        fonts
            .keys()
            .any(|name| name.starts_with(OCR_FONT_NAME.as_bytes()))
    });
    has_bpdf_layer || has_enough_text(text)
}

/// Where an image covers the displayed page, as `[left, top, width, height]`
/// in points from its top-left corner; `bounds` are in user space.
fn displayed_area(crop: PageGeometry, [left, bottom, right, top]: [f64; 4]) -> [f64; 4] {
    let corners = [(left, bottom), (right, top)].map(|(x, y)| crop.page_to_display(x, y));
    let (x0, x1) = (
        corners[0].0.min(corners[1].0),
        corners[0].0.max(corners[1].0),
    );
    let (y0, y1) = (
        corners[0].1.min(corners[1].1),
        corners[0].1.max(corners[1].1),
    );
    [x0, crop.display_height() - y1, x1 - x0, y1 - y0]
}

fn has_enough_text(text: &str) -> bool {
    text.chars()
        .filter(|character| !character.is_whitespace())
        .nth(TEXT_LAYER_THRESHOLD - 1)
        .is_some()
}

/// Delete every cached OCR response (`<sha256>.md` and `<sha256>.json`);
/// other files in the directory are left alone. Returns (files, bytes).
pub fn clear_cache(directory: &Path) -> Result<(usize, u64)> {
    if !directory.exists() {
        return Ok((0, 0));
    }
    let mut removed_count = 0usize;
    let mut bytes_freed = 0u64;
    let entries = fs::read_dir(directory)
        .with_context(|| format!("failed to read cache directory {}", directory.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_cache_entry = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| {
                stem.len() == 64 && stem.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            && (formats::has_extension(&path, "md") || formats::has_extension(&path, "json"));
        if !is_cache_entry {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_file() && fs::remove_file(entry.path()).is_ok() {
            removed_count += 1;
            bytes_freed = bytes_freed.saturating_add(metadata.len());
        }
    }
    Ok((removed_count, bytes_freed))
}

fn retry_after(header: Option<&str>, body: &str, attempt: usize) -> Duration {
    let mut seconds = header
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.0);
    // "Please try again in 2.25s" or "... in 1m30.5s".
    if seconds <= 0.0
        && let Some(rest) = body.split("try again in ").nth(1)
        && let Some(value) = rest.split('s').next()
    {
        seconds = match value.split_once('m') {
            Some((minutes, rest)) => {
                minutes.trim().parse::<f64>().unwrap_or(0.0) * 60.0
                    + rest.trim().parse::<f64>().unwrap_or(0.0)
            }
            None => value.trim().parse::<f64>().unwrap_or(0.0),
        };
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
            content.push_str(TRUNCATION_NOTE);
            return content;
        };
        let end = start + relative_end + "</think>".len();
        content.replace_range(start..end, "");
        content = content.trim().to_owned();
    }
}

/// Appended to an OCR answer cut off by the token limit.
const TRUNCATION_NOTE: &str = "\n\n*[Warning: the model's answer was cut off by the token limit]*";

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
        assert_eq!("groq".parse::<OcrBackend>().unwrap(), OcrBackend::Groq);
        assert_eq!("AUTO".parse::<OcrBackend>().unwrap(), OcrBackend::Auto);
        assert_eq!(
            "windows".parse::<OcrBackend>().unwrap(),
            OcrBackend::Windows
        );
        assert_eq!("winocr".parse::<OcrBackend>().unwrap(), OcrBackend::Windows);
        assert!("tesseract".parse::<OcrBackend>().is_err());
    }

    #[test]
    fn image_areas_follow_page_rotation() {
        let crop = PageGeometry {
            left: 0.0,
            bottom: 0.0,
            right: 600.0,
            top: 800.0,
            rotation: 0,
        };
        // Lower half of an upright page.
        assert_eq!(
            displayed_area(crop, [0.0, 0.0, 600.0, 400.0]),
            [0.0, 400.0, 600.0, 400.0]
        );
        // Turned a quarter: the page shows 800 wide and 600 high.
        let turned = PageGeometry {
            rotation: 90,
            ..crop
        };
        assert_eq!(
            displayed_area(turned, [0.0, 0.0, 600.0, 400.0]),
            [0.0, 0.0, 400.0, 600.0]
        );
    }

    #[test]
    fn short_labels_are_no_text_layer() {
        assert!(has_enough_text(&"a ".repeat(100)));
        assert!(!has_enough_text(&"a ".repeat(99)));
    }

    #[test]
    fn clears_cache_files_and_counts_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let key = "0123456789abcdef".repeat(4);
        fs::write(temp.path().join(format!("{key}.json")), b"12345").unwrap();
        fs::write(temp.path().join(format!("{key}.md")), b"1234567").unwrap();
        // Not cache entries: a user's notes beside the cache stay.
        fs::write(temp.path().join("notes.md"), b"mine").unwrap();
        fs::write(temp.path().join(format!("{key}.txt")), b"mine").unwrap();
        assert_eq!(clear_cache(temp.path()).unwrap(), (2, 12));
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);
    }

    #[test]
    fn retry_after_uses_header_body_and_cap() {
        assert_eq!(retry_after(Some("1.5"), "", 1), Duration::from_millis(1500));
        assert_eq!(
            retry_after(None, "try again in 2.25s.", 1),
            Duration::from_millis(2250)
        );
        assert_eq!(retry_after(Some("999"), "", 1), MAX_RETRY_WAIT);
        assert_eq!(
            retry_after(None, "Please try again in 1m2.5s.", 1),
            Duration::from_millis(62_500)
        );
    }

    #[test]
    fn removes_thinking_blocks() {
        assert_eq!(
            strip_think_blocks("<think>hidden</think>\nanswer"),
            "answer"
        );
        assert!(strip_think_blocks("<think>cut").ends_with(TRUNCATION_NOTE));
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
            .extract_spec(&InputSpec {
                path: temporary.path().to_path_buf(),
                pages: None,
            })
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
