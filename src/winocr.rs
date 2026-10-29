#[cfg(windows)]
use anyhow::Context;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

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
}

impl OcrPageResult {
    /// Plain recognised text without word geometry (vision OCR).
    pub fn text_only(text: String) -> Self {
        Self {
            text,
            words: Vec::new(),
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
        use windows::Graphics::Imaging::{BitmapDecoder, BitmapPixelFormat, SoftwareBitmap};
        use windows::Media::Ocr::OcrEngine;
        use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};

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

        let max_dim = OcrEngine::MaxImageDimension()?;
        // The system decoder first; an image larger than the engine reads, or
        // in a format the system cannot decode, is scaled down from our own
        // decoding. `scale` maps engine pixels back to image pixels.
        let system = (|| -> Result<SoftwareBitmap> {
            let stream = InMemoryRandomAccessStream::new()
                .context("failed to create InMemoryRandomAccessStream")?;
            let writer =
                DataWriter::CreateDataWriter(&stream).context("failed to create DataWriter")?;
            writer.WriteBytes(bytes).context("failed to write bytes")?;
            writer.StoreAsync()?.join()?;
            writer.DetachStream()?;
            stream.Seek(0)?;

            let decoder = BitmapDecoder::CreateAsync(&stream)?
                .join()
                .context("failed to create BitmapDecoder for image")?;
            if decoder.PixelWidth()? > max_dim || decoder.PixelHeight()? > max_dim {
                bail!("image too large for Windows OCR");
            }
            decoder
                .GetSoftwareBitmapAsync()?
                .join()
                .context("failed to get SoftwareBitmap")
        })();

        let (bitmap, scale) = match system {
            Ok(bitmap) => (bitmap, 1.0),
            Err(_) => {
                let mut image = image::load_from_memory(bytes)
                    .context("failed to decode image bytes via fallback image decoder")?;
                let original_width = image.width();
                if image.width() > max_dim || image.height() > max_dim {
                    image = image.resize(max_dim, max_dim, image::imageops::FilterType::Lanczos3);
                }
                let (width, height) = (image.width(), image.height());
                // The engine reads BGRA.
                let mut pixels = image.into_rgba8().into_raw();
                for pixel in pixels.chunks_exact_mut(4) {
                    pixel.swap(0, 2);
                }
                let writer = DataWriter::new().context("failed to create DataWriter")?;
                writer
                    .WriteBytes(&pixels)
                    .context("failed to write pixel bytes")?;
                let buffer = writer.DetachBuffer().context("failed to detach buffer")?;
                let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
                    &buffer,
                    BitmapPixelFormat::Bgra8,
                    width as i32,
                    height as i32,
                )
                .context("failed to create SoftwareBitmap from BGRA buffer")?;
                (bitmap, f64::from(original_width) / f64::from(width))
            }
        };

        let result = engine
            .RecognizeAsync(&bitmap)?
            .join()
            .context("OCR recognition failed")?;

        let full_text = result.Text()?.to_string();
        let mut words = Vec::new();

        for line in result.Lines()? {
            let mut words_in_line = Vec::new();
            let mut min_y = f64::MAX;
            let mut max_bottom = f64::MIN;

            for word in line.Words()? {
                let text = word.Text()?.to_string();
                let rect = word.BoundingRect()?;
                let y = (rect.Y as f64) * scale;
                let height = (rect.Height as f64) * scale;
                if y < min_y {
                    min_y = y;
                }
                if y + height > max_bottom {
                    max_bottom = y + height;
                }

                words_in_line.push((
                    text,
                    (rect.X as f64) * scale,
                    y,
                    (rect.Width as f64) * scale,
                    height,
                ));
            }

            let line_y = min_y;
            let line_height = max_bottom - min_y;

            for (text, x, y, width, height) in words_in_line {
                words.push(OcrWordBox {
                    text,
                    x,
                    y,
                    width,
                    height,
                    line_y,
                    line_height,
                });
            }
        }

        Ok(OcrPageResult {
            text: full_text,
            words,
        })
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
}
