use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, ImageFormat, ImageReader, Rgb, RgbImage};
use tempfile::Builder;

use crate::metadata;
use crate::process;

#[derive(Clone, Debug)]
pub struct ImageOptions {
    pub keep_icc: bool,
    pub ffmpeg: PathBuf,
    pub jpeg_quality: u8,
    pub image_dpi: u32,
}

pub fn is_jpeg(path: &Path) -> bool {
    extension(path).is_some_and(|value| value == "jpg" || value == "jpeg")
}

pub fn is_png(path: &Path) -> bool {
    extension(path).is_some_and(|value| value == "png")
}

pub fn is_heic(path: &Path) -> bool {
    extension(path).is_some_and(|value| value == "heic" || value == "heif")
}

pub fn is_supported_image(path: &Path) -> bool {
    matches!(
        extension(path).as_deref(),
        Some("jpg" | "jpeg" | "png" | "bmp" | "gif" | "tiff" | "tif" | "webp" | "heic" | "heif")
    )
}

pub fn to_jpeg(path: &Path, options: &ImageOptions, page_size: Option<&str>) -> Result<Vec<u8>> {
    if is_heic(path) {
        return heic_to_jpeg(path, options);
    }

    let input = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;

    // Check if we need to resize before deciding to fast-path the JPEG
    let mut target_dimensions = None;
    if options.image_dpi > 0
        && let Some(size) = page_size
        && let Ok((pw, ph)) = crate::pdf::paper_size(size)
    {
        let dpi = f64::from(options.image_dpi);
        let mut max_w = (pw / 72.0 * dpi).round() as u32;
        let mut max_h = (ph / 72.0 * dpi).round() as u32;
        let (w, h) = image::image_dimensions(path).unwrap_or((0, 0));

        if (w > h) != (max_w > max_h) {
            std::mem::swap(&mut max_w, &mut max_h);
        }

        if w > max_w || h > max_h {
            target_dimensions = Some((max_w, max_h));
        }
    }

    if is_jpeg(path) && target_dimensions.is_none() {
        return metadata::strip_jpeg(&input, options.keep_icc);
    }

    let mut image = ImageReader::new(Cursor::new(input))
        .with_guessed_format()
        .context("failed to determine image format")?
        .decode()
        .with_context(|| format!("failed to decode {}", path.display()))?;

    if let Some((max_w, max_h)) = target_dimensions {
        image = image.resize(max_w, max_h, FilterType::Lanczos3);
    }

    encode_jpeg_on_white(&image, options.jpeg_quality)
}

pub fn for_ocr(path: &Path, options: &ImageOptions) -> Result<Vec<u8>> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let format = image::guess_format(&bytes).ok();
    if matches!(
        format,
        Some(ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP | ImageFormat::Gif)
    ) {
        return Ok(bytes);
    }
    to_jpeg(path, options, None)
}

pub fn optimize_for_ocr(bytes: &[u8]) -> Result<Vec<u8>> {
    let image = image::load_from_memory(bytes).context("failed to decode OCR image")?;
    let (width, height) = image.dimensions();
    if width <= 1536 && height <= 1536 && bytes.len() < 3 * 1024 * 1024 {
        return Ok(bytes.to_vec());
    }

    let scale = (1536.0 / f64::from(width))
        .min(1536.0 / f64::from(height))
        .min(1.0);
    let target_width = (f64::from(width) * scale).round().max(1.0) as u32;
    let target_height = (f64::from(height) * scale).round().max(1.0) as u32;
    let resized = image.resize_exact(target_width, target_height, FilterType::CatmullRom);
    encode_jpeg_on_white(&resized, 80)
}

pub fn heic_to_jpeg(path: &Path, options: &ImageOptions) -> Result<Vec<u8>> {
    ffmpeg_to_jpeg(path, options)
}

pub fn ffmpeg_to_jpeg(path: &Path, options: &ImageOptions) -> Result<Vec<u8>> {
    let temporary = Builder::new().suffix(".jpg").tempfile()?;
    let output_path = temporary.path().to_path_buf();

    let mut command = Command::new(&options.ffmpeg);
    command
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-i")
        .arg(path)
        // HEIC files often expose both the primary image and a thumbnail.
        .arg("-map")
        .arg("0:v:0")
        .arg("-frames:v")
        .arg("1")
        .arg("-map_metadata")
        .arg("-1")
        .arg("-q:v")
        .arg("2")
        .arg(&output_path);
    let output = process::run(command, Duration::from_secs(120), "ffmpeg").with_context(|| {
        format!(
            "install ffmpeg or pass --ffmpeg {}",
            options.ffmpeg.display()
        )
    })?;
    process::require_success(output, "ffmpeg")?;

    let jpeg = fs::read(&output_path).context("ffmpeg produced no JPEG output")?;
    metadata::strip_jpeg(&jpeg, options.keep_icc)
}

pub(crate) fn encode_jpeg_on_white(image: &DynamicImage, quality: u8) -> Result<Vec<u8>> {
    let rgba = image.to_rgba8();
    let (width, height) = image.dimensions();
    let rgb = RgbImage::from_fn(width, height, |x, y| {
        let pixel = rgba.get_pixel(x, y).0;
        let alpha = u16::from(pixel[3]);
        let blend = |channel: u8| -> u8 {
            ((u16::from(channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8
        };
        Rgb([blend(pixel[0]), blend(pixel[1]), blend(pixel[2])])
    });

    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, quality)
        .encode_image(&DynamicImage::ImageRgb8(rgb))
        .context("failed to encode JPEG")?;
    Ok(bytes)
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_is_composited_onto_white() {
        let image = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([0, 0, 0, 0]),
        ));
        let jpeg = encode_jpeg_on_white(&image, 100).unwrap();
        let decoded = image::load_from_memory(&jpeg).unwrap().to_rgb8();
        assert!(
            decoded
                .get_pixel(0, 0)
                .0
                .iter()
                .all(|channel| *channel > 245)
        );
    }
}
