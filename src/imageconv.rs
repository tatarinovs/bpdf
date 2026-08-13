use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{
    DynamicImage, ExtendedColorType, GenericImageView, ImageFormat, ImageReader, Rgb, RgbImage,
};
use tempfile::Builder;

use crate::formats::{self, Format};
use crate::metadata;
use crate::process;

#[derive(Clone, Debug)]
pub struct ImageOptions {
    pub keep_icc: bool,
    pub ffmpeg: PathBuf,
    pub jpeg_quality: u8,
    pub image_dpi: u32,
}

pub fn to_jpeg(path: &Path, options: &ImageOptions, page_size: Option<&str>) -> Result<Vec<u8>> {
    let format = formats::detect(path);
    if format == Some(Format::Heic) {
        return ffmpeg_to_jpeg(path, options);
    }

    let input = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;

    // Check if we need to resize before deciding to fast-path the JPEG
    let mut target_dimensions = None;
    if options.image_dpi > 0
        && let Some(size) = page_size
        && let Ok((pw, ph)) = crate::pdf::paper_size(size)
        && let Ok((w, h)) = image::image_dimensions(path)
    {
        target_dimensions = fit_dimensions_for_dpi(w, h, pw, ph, options.image_dpi);
    }

    if format == Some(Format::Jpeg) && target_dimensions.is_none() {
        return metadata::strip_jpeg(&input, options.keep_icc);
    }

    let image = ImageReader::new(Cursor::new(input))
        .with_guessed_format()
        .context("failed to determine image format")?
        .decode()
        .with_context(|| format!("failed to decode {}", path.display()))?;

    resize_to_jpeg(&image, target_dimensions, options.jpeg_quality)
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

    let target_dimensions = fit_dimensions(width, height, 1536, 1536);
    resize_to_jpeg_with_filter(&image, target_dimensions, 80, FilterType::CatmullRom)
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
    if let Some(rgb) = image.as_rgb8() {
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, quality)
            .encode(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                ExtendedColorType::Rgb8,
            )
            .context("failed to encode JPEG")?;
        return Ok(bytes);
    }

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

fn pixel_dimensions_for_dpi(width_points: f64, height_points: f64, dpi: u32) -> Option<(u32, u32)> {
    if dpi == 0 || width_points <= 0.0 || height_points <= 0.0 {
        return None;
    }
    let dpi = f64::from(dpi);
    Some((
        (width_points * dpi / 72.0).round().max(1.0) as u32,
        (height_points * dpi / 72.0).round().max(1.0) as u32,
    ))
}

fn fit_dimensions(width: u32, height: u32, max_width: u32, max_height: u32) -> Option<(u32, u32)> {
    if width == 0 || height == 0 || (width <= max_width && height <= max_height) {
        return None;
    }
    let scale =
        (f64::from(max_width) / f64::from(width)).min(f64::from(max_height) / f64::from(height));
    Some((
        (f64::from(width) * scale).round().max(1.0) as u32,
        (f64::from(height) * scale).round().max(1.0) as u32,
    ))
}

pub(crate) fn fit_dimensions_for_dpi(
    width: u32,
    height: u32,
    page_width_points: f64,
    page_height_points: f64,
    dpi: u32,
) -> Option<(u32, u32)> {
    let (mut max_width, mut max_height) =
        pixel_dimensions_for_dpi(page_width_points, page_height_points, dpi)?;
    if (width > height) != (max_width > max_height) {
        std::mem::swap(&mut max_width, &mut max_height);
    }
    fit_dimensions(width, height, max_width, max_height)
}

pub(crate) fn resize_to_jpeg(
    image: &DynamicImage,
    target_dimensions: Option<(u32, u32)>,
    quality: u8,
) -> Result<Vec<u8>> {
    resize_to_jpeg_with_filter(image, target_dimensions, quality, FilterType::Lanczos3)
}

fn resize_to_jpeg_with_filter(
    image: &DynamicImage,
    target_dimensions: Option<(u32, u32)>,
    quality: u8,
    filter: FilterType,
) -> Result<Vec<u8>> {
    match target_dimensions {
        Some((width, height)) => {
            let resized = image.resize_exact(width, height, filter);
            encode_jpeg_on_white(&resized, quality)
        }
        None => encode_jpeg_on_white(image, quality),
    }
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

    #[test]
    fn dpi_and_fit_helpers_preserve_aspect_ratio() {
        assert_eq!(pixel_dimensions_for_dpi(144.0, 72.0, 100), Some((200, 100)));
        assert_eq!(fit_dimensions(1200, 600, 200, 100), Some((200, 100)));
        assert_eq!(
            fit_dimensions_for_dpi(1200, 600, 144.0, 72.0, 100),
            Some((200, 100))
        );
        assert_eq!(fit_dimensions(100, 50, 200, 100), None);
    }

    #[test]
    fn webp_is_decoded_without_ffmpeg() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.webp");
        DynamicImage::ImageRgb8(RgbImage::from_pixel(2, 2, Rgb([10, 20, 30])))
            .save_with_format(&path, ImageFormat::WebP)
            .unwrap();
        let jpeg = to_jpeg(
            &path,
            &ImageOptions {
                keep_icc: false,
                ffmpeg: PathBuf::from("missing-ffmpeg"),
                jpeg_quality: 85,
                image_dpi: 0,
            },
            None,
        )
        .unwrap();
        assert_eq!(image::guess_format(&jpeg).unwrap(), ImageFormat::Jpeg);
    }
}
