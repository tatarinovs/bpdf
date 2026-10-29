#[cfg(windows)]
use anyhow::Context;
use std::borrow::Cow;

use anyhow::{Result, bail};
use image::{DynamicImage, GrayImage};
use serde::{Deserialize, Serialize};

/// Long edge small scans are enlarged to: about 300 dpi on A4, where
/// Windows OCR reads small print reliably (it misreads much of a 150 dpi scan).
const TARGET_LONG_EDGE: u32 = 3500;
/// Enlargement limit; beyond it the glyphs only get blurrier.
const MAX_UPSCALE: f64 = 3.0;
/// Blur radius of the unsharp mask applied after enlarging, in pixels of
/// the enlarged image.
const UNSHARP_SIGMA: f32 = 1.5;
/// Share of the recognised characters in plausible words above which the
/// upright reading is kept without trying the other orientations.
const UPRIGHT_PLAUSIBILITY: f64 = 0.6;
/// Characters in plausible words below which the other orientations are
/// tried as well: a sideways page often yields a few short "words" only.
const MIN_PLAUSIBLE: usize = 200;
/// Lines whose centres are closer than this share of their height sit on
/// one row (cells of a table, label and value of a form).
const SAME_ROW: f64 = 0.35;

/// A recognised word; coordinates are image pixels from the top-left corner.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OcrWordBox {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub line_y: f64,
    pub line_height: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OcrPageResult {
    pub text: String,
    #[serde(default)]
    pub words: Vec<OcrWordBox>,
    /// Clockwise quarter turns of the image before recognition; word boxes
    /// are pixels of the turned image.
    #[serde(default)]
    pub quarter_turns: u8,
}

impl OcrPageResult {
    /// Plain recognised text without word geometry (vision OCR).
    pub fn text_only(text: String) -> Self {
        Self {
            text,
            words: Vec::new(),
            quarter_turns: 0,
        }
    }
}

pub fn is_available() -> bool {
    #[cfg(windows)]
    {
        available_languages()
            .map(|langs| !langs.is_empty())
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub fn available_languages() -> Result<Vec<String>> {
    #[cfg(windows)]
    {
        let _apartment = crate::com::ComApartment::initialize()?;
        let languages = windows::Media::Ocr::OcrEngine::AvailableRecognizerLanguages()
            .context("failed to query Windows OCR languages")?;
        let mut result = Vec::new();
        for lang in languages {
            let tag = lang.LanguageTag()?.to_string();
            result.push(tag);
        }
        Ok(result)
    }
    #[cfg(not(windows))]
    {
        Ok(Vec::new())
    }
}

/// Recognise an image. It is read in grey with stretched contrast and
/// enlarged when small; a reading that looks like noise is retried in the
/// other orientations (pages scanned sideways or upside down).
pub fn recognize_image_bytes(bytes: &[u8], lang_tag: Option<&str>) -> Result<OcrPageResult> {
    #[cfg(not(windows))]
    {
        let _ = (bytes, lang_tag);
        bail!("Windows Media OCR is only available on Windows");
    }
    #[cfg(windows)]
    {
        let _apartment = crate::com::ComApartment::initialize()?;
        use windows::Globalization::Language;
        use windows::Media::Ocr::OcrEngine;

        let engine = if let Some(tag) = lang_tag {
            let htag = windows::core::HSTRING::from(tag);
            let lang = Language::CreateLanguage(&htag)?;
            if !OcrEngine::IsLanguageSupported(&lang)? {
                bail!("language '{tag}' is not supported by Windows Media OCR on this system");
            }
            OcrEngine::TryCreateFromLanguage(&lang)?
        } else {
            OcrEngine::TryCreateFromUserProfileLanguages()?
        };

        let image =
            image::load_from_memory(bytes).context("failed to decode image for Windows OCR")?;
        let size = (image.width(), image.height());
        let max_dim = OcrEngine::MaxImageDimension()?;
        let grey = to_grey(&image);
        drop(image);
        let enlarged = resize_for_engine(&grey, TARGET_LONG_EDGE, max_dim);
        let read = |image: &GrayImage, turns: u8| {
            let mut result = recognize(&engine, image, turned_scale(size, image, turns))?;
            result.quarter_turns = turns;
            Ok::<_, anyhow::Error>(result)
        };

        let upright = read(&enlarged, 0)?;
        let (upright_score, total) = plausibility(&upright.text);
        if upright_score >= MIN_PLAUSIBLE
            && upright_score as f64 >= UPRIGHT_PLAUSIBILITY * total as f64
        {
            return Ok(upright);
        }
        // Orientations are compared on the unenlarged image, which is read
        // several times faster and well enough to tell text from noise;
        // only the winner is read again at full size.
        let probe = resize_for_engine(&grey, 0, max_dim);
        let mut best = None;
        let mut best_score = upright_score;
        for turns in [1, 3, 2] {
            let (score, _) = plausibility(&read(&turn(&probe, turns), turns)?.text);
            if score > best_score {
                best = Some(turns);
                best_score = score;
            }
        }
        let Some(turns) = best else {
            return Ok(upright);
        };
        let turned = read(&turn(&enlarged, turns), turns)?;
        Ok(if plausibility(&turned.text).0 > upright_score {
            turned
        } else {
            upright
        })
    }
}

/// The image turned clockwise by `turns` quarter turns.
#[cfg(windows)]
fn turn(image: &GrayImage, turns: u8) -> GrayImage {
    match turns % 4 {
        1 => image::imageops::rotate90(image),
        2 => image::imageops::rotate180(image),
        3 => image::imageops::rotate270(image),
        _ => image.clone(),
    }
}

/// Factors from the pixels of `image`, a resized copy of an image of
/// `size` turned by `turns`, to the pixels of the turned original.
#[cfg(windows)]
fn turned_scale(size: (u32, u32), image: &GrayImage, turns: u8) -> (f64, f64) {
    let (width, height) = if turns % 2 == 1 {
        (size.1, size.0)
    } else {
        size
    };
    (
        f64::from(width) / f64::from(image.width()),
        f64::from(height) / f64::from(image.height()),
    )
}

/// One recognition pass; `scale` maps the pixels of `image` to the pixels
/// the word boxes are reported in.
#[cfg(windows)]
fn recognize(
    engine: &windows::Media::Ocr::OcrEngine,
    image: &GrayImage,
    scale: (f64, f64),
) -> Result<OcrPageResult> {
    use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
    use windows::Storage::Streams::DataWriter;

    let writer = DataWriter::new().context("failed to create DataWriter")?;
    writer
        .WriteBytes(image.as_raw())
        .context("failed to write pixel bytes")?;
    let buffer = writer.DetachBuffer().context("failed to detach buffer")?;
    let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
        &buffer,
        BitmapPixelFormat::Gray8,
        image.width() as i32,
        image.height() as i32,
    )
    .context("failed to create SoftwareBitmap from grey pixels")?;
    let result = engine
        .RecognizeAsync(&bitmap)?
        .join()
        .context("OCR recognition failed")?;

    let mut lines = Vec::new();
    let mut words = Vec::new();
    for line in result.Lines()? {
        let first = words.len();
        let mut min_y = f64::MAX;
        let mut max_bottom = f64::MIN;
        let mut min_x = f64::MAX;
        for word in line.Words()? {
            let rect = word.BoundingRect()?;
            let x = f64::from(rect.X) * scale.0;
            let y = f64::from(rect.Y) * scale.1;
            let height = f64::from(rect.Height) * scale.1;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_bottom = max_bottom.max(y + height);
            words.push(OcrWordBox {
                text: word.Text()?.to_string(),
                x,
                y,
                width: f64::from(rect.Width) * scale.0,
                height,
                line_y: 0.0,
                line_height: 0.0,
            });
        }
        if words.len() == first {
            continue;
        }
        for word in &mut words[first..] {
            word.line_y = min_y;
            word.line_height = max_bottom - min_y;
        }
        lines.push(OcrLine {
            text: line.Text()?.to_string(),
            x: min_x,
            y: min_y,
            height: max_bottom - min_y,
        });
    }

    Ok(OcrPageResult {
        text: layout_text(lines),
        words,
        quarter_turns: 0,
    })
}

/// The image in grey, transparency on white, with stretched contrast.
fn to_grey(image: &DynamicImage) -> GrayImage {
    let mut grey = if image.color().has_alpha() {
        let mut rgba = image.to_rgba8();
        for pixel in rgba.pixels_mut() {
            let alpha = u16::from(pixel[3]);
            for channel in &mut pixel.0[..3] {
                *channel = ((u16::from(*channel) * alpha + 255 * (255 - alpha)) / 255) as u8;
            }
            pixel[3] = 255;
        }
        DynamicImage::ImageRgba8(rgba).to_luma8()
    } else {
        image.to_luma8()
    };
    stretch_contrast(&mut grey);
    grey
}

/// The image enlarged towards a long edge of `target` (at most
/// `MAX_UPSCALE` times) and reduced to what the engine accepts.
fn resize_for_engine(image: &GrayImage, target: u32, max_dim: u32) -> Cow<'_, GrayImage> {
    let (width, height) = image.dimensions();
    let long_edge = f64::from(width.max(height).max(1));
    let factor = (f64::from(target) / long_edge)
        .clamp(1.0, MAX_UPSCALE)
        .min(f64::from(max_dim) / long_edge);
    let side = |side: u32| ((f64::from(side) * factor).round() as u32).clamp(1, max_dim);
    let (target_width, target_height) = (side(width), side(height));
    if (target_width, target_height) == (width, height) {
        return Cow::Borrowed(image);
    }
    let resized = image::imageops::resize(
        image,
        target_width,
        target_height,
        image::imageops::FilterType::CatmullRom,
    );
    // Enlarging softens the glyph edges; sharpening them back gains a few
    // more correctly read words.
    Cow::Owned(if factor > 1.0 {
        image::imageops::unsharpen(&resized, UNSHARP_SIGMA, 2)
    } else {
        resized
    })
}

/// Stretch the grey levels between the darkest and the lightest percent of
/// the pixels to the full range: faded print darkens, while paper tint and
/// show-through from the back fade to white. A nearly uniform image (a
/// blank page) is left alone so that its noise is not amplified.
fn stretch_contrast(image: &mut GrayImage) {
    const MIN_SPREAD: usize = 32;
    let mut histogram = [0usize; 256];
    for &level in image.as_raw() {
        histogram[usize::from(level)] += 1;
    }
    let cut = image.as_raw().len() / 100;
    let percentile = |levels: &[usize]| {
        let mut seen = 0;
        levels
            .iter()
            .copied()
            .find(|&level| {
                seen += histogram[level];
                seen > cut
            })
            .unwrap_or(0)
    };
    let mut levels: Vec<usize> = (0..256).collect();
    let low = percentile(&levels);
    levels.reverse();
    let high = percentile(&levels);
    if high < low + MIN_SPREAD {
        return;
    }
    let mut table = [0u8; 256];
    for (level, value) in table.iter_mut().enumerate() {
        *value = ((level.clamp(low, high) - low) * 255 / (high - low)) as u8;
    }
    for level in image.iter_mut() {
        *level = table[usize::from(*level)];
    }
}

/// A recognised line: its text and its box (left edge, top, height).
struct OcrLine {
    text: String,
    x: f64,
    y: f64,
    height: f64,
}

/// Page text with one row per line: lines side by side (table cells, a
/// label and its value) join into one row, separated by tabs.
fn layout_text(mut lines: Vec<OcrLine>) -> String {
    let centre = |line: &OcrLine| line.y + line.height / 2.0;
    lines.sort_by(|a, b| centre(a).total_cmp(&centre(b)));
    let mut rows: Vec<Vec<OcrLine>> = Vec::new();
    for line in lines {
        if let Some(row) = rows.last_mut() {
            let last = &row[row.len() - 1];
            let height = line.height.max(last.height);
            if (centre(&line) - centre(last)).abs() < SAME_ROW * height {
                row.push(line);
                continue;
            }
        }
        rows.push(vec![line]);
    }
    rows.into_iter()
        .map(|mut row| {
            row.sort_by(|a, b| a.x.total_cmp(&b.x));
            row.into_iter()
                .map(|line| line.text)
                .collect::<Vec<_>>()
                .join("\t")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Characters in plausible words, and all characters but spaces. A word is
/// plausible when it is a number, or at least three letters of one script
/// with a vowel; a page read in the wrong orientation gives few of them.
fn plausibility(text: &str) -> (usize, usize) {
    let mut plausible = 0;
    let mut total = 0;
    for token in text.split_whitespace() {
        total += token.chars().count();
        plausible += token
            .split('-')
            .map(|part| plausible_word(part.trim_matches(|c: char| !c.is_alphanumeric())))
            .sum::<usize>();
    }
    (plausible, total)
}

/// Length of a plausible word (parts of a hyphenated one are judged apart).
fn plausible_word(word: &str) -> usize {
    const VOWELS: &str = "aeiouyаеёиоуыэюя";
    let length = word.chars().count();
    if length < 3 {
        return 0;
    }
    let is_number = word.chars().filter(char::is_ascii_digit).count() >= 2
        && word
            .chars()
            .all(|c| c.is_ascii_digit() || ".,/:%".contains(c));
    let one_script = word.chars().all(|c| c.is_ascii_alphabetic())
        || word.chars().all(|c| ('\u{400}'..='\u{4ff}').contains(&c));
    let has_vowel = word
        .chars()
        .any(|c| VOWELS.contains(c.to_lowercase().next().unwrap_or(c)));
    if is_number || (one_script && has_vowel) {
        length
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_available_languages_on_windows() {
        if cfg!(windows) {
            let languages = available_languages().unwrap();
            assert!(!languages.is_empty());
        }
    }

    #[test]
    fn plausibility_separates_text_from_noise() {
        let (score, total) =
            plausibility("Счёт-фактура № 260820009 от 20 августа 2026 г. Продавец: ООО «Металл»");
        assert!(score as f64 > UPRIGHT_PLAUSIBILITY * total as f64);
        let (score, total) = plausibility("dlil 'lil “llF о о [00L09GZG/t8889L9sas");
        assert!((score as f64) < UPRIGHT_PLAUSIBILITY / 2.0 * total as f64);
    }

    #[test]
    fn layout_joins_lines_of_one_row() {
        let line = |text: &str, x, y| OcrLine {
            text: text.to_owned(),
            x,
            y,
            height: 20.0,
        };
        let text = layout_text(vec![
            line("Покупатель:", 500.0, 102.0),
            line("Продавец:", 10.0, 100.0),
            line("Адрес:", 10.0, 130.0),
        ]);
        assert_eq!(text, "Продавец:\tПокупатель:\nАдрес:");
    }

    #[test]
    fn contrast_stretch_spans_the_full_range() {
        let mut image =
            GrayImage::from_fn(100, 100, |x, _| image::Luma([100 + (x % 2) as u8 * 80]));
        stretch_contrast(&mut image);
        assert_eq!(image.get_pixel(0, 0)[0], 0);
        assert_eq!(image.get_pixel(1, 0)[0], 255);
    }

    #[test]
    fn small_scans_are_enlarged_within_limits() {
        let size = |width, height, target, max_dim| {
            resize_for_engine(&GrayImage::new(width, height), target, max_dim).dimensions()
        };
        assert_eq!(size(1754, 1240, TARGET_LONG_EDGE, 10_000), (3500, 2474));
        assert_eq!(size(100, 50, TARGET_LONG_EDGE, 10_000), (300, 150));
        assert_eq!(size(1754, 1240, 0, 10_000), (1754, 1240));
        assert_eq!(size(2_000, 1_000, TARGET_LONG_EDGE, 1_000), (1_000, 500));
    }
}
