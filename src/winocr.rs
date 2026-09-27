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
    #[serde(default, rename = "width")]
    pub image_width: u32,
    #[serde(default, rename = "height")]
    pub image_height: u32,
}

impl OcrPageResult {
    /// Plain recognised text without word geometry (vision OCR).
    pub fn text_only(text: String) -> Self {
        Self {
            text,
            words: Vec::new(),
            image_width: 0,
            image_height: 0,
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

        let decode_result = (|| -> Result<(SoftwareBitmap, u32, u32, f64)> {
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
            let pixel_width = decoder.PixelWidth()?;
            let pixel_height = decoder.PixelHeight()?;

            if pixel_width > max_dim || pixel_height > max_dim {
                bail!("image too large for Windows OCR, fallback to scaling");
            }

            let bitmap = decoder
                .GetSoftwareBitmapAsync()?
                .join()
                .context("failed to get SoftwareBitmap")?;
            Ok((bitmap, pixel_width, pixel_height, 1.0))
        })();

        let (bitmap, pixel_width, pixel_height, scale_factor) = match decode_result {
            Ok(tuple) => tuple,
            Err(_) => {
                let mut dynamic_image = image::load_from_memory(bytes)
                    .context("failed to decode image bytes via fallback image decoder")?;
                let mut width = dynamic_image.width();
                let mut height = dynamic_image.height();

                let scale_factor = if width > max_dim || height > max_dim {
                    let sf = (max_dim as f64) / (width.max(height) as f64);
                    let new_w = (width as f64 * sf).round() as u32;
                    let new_h = (height as f64 * sf).round() as u32;
                    dynamic_image = dynamic_image.resize_exact(
                        new_w,
                        new_h,
                        image::imageops::FilterType::Lanczos3,
                    );
                    width = new_w;
                    height = new_h;
                    sf
                } else {
                    1.0
                };

                let rgba = dynamic_image.to_rgba8();

                let writer = DataWriter::new().context("failed to create DataWriter")?;
                writer
                    .WriteBytes(rgba.as_raw())
                    .context("failed to write pixel bytes")?;
                let buffer = writer.DetachBuffer().context("failed to detach buffer")?;

                let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
                    &buffer,
                    BitmapPixelFormat::Rgba8,
                    width as i32,
                    height as i32,
                )
                .context("failed to create SoftwareBitmap from RGBA buffer")?;
                (bitmap, width, height, scale_factor)
            }
        };

        let result = engine
            .RecognizeAsync(&bitmap)?
            .join()
            .context("OCR recognition failed")?;

        let full_text = result.Text()?.to_string();
        let mut words = Vec::new();

        let inv_scale = if scale_factor < 1.0 {
            1.0 / scale_factor
        } else {
            1.0
        };

        for line in result.Lines()? {
            let mut words_in_line = Vec::new();
            let mut min_y = f64::MAX;
            let mut max_bottom = f64::MIN;

            for word in line.Words()? {
                let text = word.Text()?.to_string();
                let rect = word.BoundingRect()?;
                let y = (rect.Y as f64) * inv_scale;
                let height = (rect.Height as f64) * inv_scale;
                if y < min_y {
                    min_y = y;
                }
                if y + height > max_bottom {
                    max_bottom = y + height;
                }

                words_in_line.push((
                    text,
                    (rect.X as f64) * inv_scale,
                    y,
                    (rect.Width as f64) * inv_scale,
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
            image_width: pixel_width,
            image_height: pixel_height,
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
