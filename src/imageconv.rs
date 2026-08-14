use std::fs::{self, File};
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use image::AnimationDecoder;
use image::codecs::gif::GifDecoder;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::imageops::FilterType;
use image::{
    DynamicImage, ExtendedColorType, GenericImageView, ImageFormat, ImageReader, Rgb, RgbImage,
};
use tempfile::Builder;

use crate::formats::{self, Format};
use crate::metadata;
use crate::process;

const MAX_IMAGE_FRAMES: usize = 10_000;

#[derive(Clone, Debug)]
pub struct ImageOptions {
    pub keep_icc: bool,
    pub ffmpeg: PathBuf,
    pub jpeg_quality: u8,
    pub image_dpi: u32,
}

pub fn to_jpeg(path: &Path, options: &ImageOptions, page_size: Option<&str>) -> Result<Vec<u8>> {
    let format = formats::detect(path);
    if format.is_some_and(Format::requires_ffmpeg) {
        return ffmpeg_to_jpeg_for_page(path, options, page_size);
    }
    if format.is_some_and(Format::requires_wic) {
        return decoded_to_jpeg_for_page(crate::wic::decode(path)?, options, page_size);
    }

    let input = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;

    // Preserve the lossless JPEG fast path when its dimensions already fit.
    let jpeg_target = (format == Some(Format::Jpeg))
        .then(|| image::image_dimensions(path).ok())
        .flatten()
        .and_then(|(width, height)| {
            dpi_target_for_dimensions(width, height, page_size, options.image_dpi)
        });

    if format == Some(Format::Jpeg) && jpeg_target.is_none() {
        return metadata::strip_jpeg(&input, options.keep_icc);
    }

    let native = ImageReader::new(Cursor::new(input))
        .with_guessed_format()
        .context("failed to determine image format")?
        .decode();
    let image = match native {
        Ok(image) => image,
        Err(native_error) => decode_with_ffmpeg(path, options).with_context(|| {
            format!(
                "native decoder failed for {}: {native_error}",
                path.display()
            )
        })?,
    };
    let target_dimensions =
        jpeg_target.or_else(|| dpi_target(&image, page_size, options.image_dpi));

    resize_to_jpeg(&image, target_dimensions, options.jpeg_quality)
}

/// Convert raw image bytes into JPEG for PDF page creation.
pub fn bytes_to_jpeg(
    bytes: &[u8],
    options: &ImageOptions,
    page_size: Option<&str>,
) -> Result<Vec<u8>> {
    if let Ok(ImageFormat::Jpeg) = image::guess_format(bytes) {
        let jpeg_target = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .ok()
            .and_then(|r| r.into_dimensions().ok())
            .and_then(|(w, h)| dpi_target_for_dimensions(w, h, page_size, options.image_dpi));
        if jpeg_target.is_none() {
            if let Ok(stripped) = metadata::strip_jpeg(bytes, options.keep_icc) {
                return Ok(stripped);
            }
        }
    }

    let image = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("failed to determine image format")?
        .decode()
        .context("failed to decode image bytes")?;

    let target_dimensions = dpi_target(&image, page_size, options.image_dpi);
    resize_to_jpeg(&image, target_dimensions, options.jpeg_quality)
}

/// Decode every logical page/frame for PDF construction. Single-image callers
/// keep using `to_jpeg`, so convert/OCR naming and behavior remain stable.
pub fn to_jpegs_for_pdf(
    path: &Path,
    options: &ImageOptions,
    page_size: Option<&str>,
) -> Result<Vec<Vec<u8>>> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "gif" => {
            let decoder = GifDecoder::new(BufReader::new(File::open(path)?))
                .with_context(|| format!("failed to decode GIF {}", path.display()))?;
            encode_animation_frames(decoder.into_frames(), options, page_size, path)
        }
        "png" | "apng" => {
            let decoder = PngDecoder::new(BufReader::new(File::open(path)?))
                .with_context(|| format!("failed to decode PNG {}", path.display()))?;
            if decoder.is_apng()? {
                encode_animation_frames(decoder.apng()?.into_frames(), options, page_size, path)
            } else {
                Ok(vec![to_jpeg(path, options, page_size)?])
            }
        }
        "webp" => {
            let decoder = WebPDecoder::new(BufReader::new(File::open(path)?))
                .with_context(|| format!("failed to decode WebP {}", path.display()))?;
            if decoder.has_animation() {
                encode_animation_frames(decoder.into_frames(), options, page_size, path)
            } else {
                Ok(vec![to_jpeg(path, options, page_size)?])
            }
        }
        "tif" | "tiff" => decode_tiff_pages(path, options, page_size),
        _ => Ok(vec![to_jpeg(path, options, page_size)?]),
    }
}

fn encode_animation_frames(
    frames: impl Iterator<Item = image::ImageResult<image::Frame>>,
    options: &ImageOptions,
    page_size: Option<&str>,
    path: &Path,
) -> Result<Vec<Vec<u8>>> {
    let mut output = Vec::new();
    for (index, frame) in frames.enumerate() {
        if index == MAX_IMAGE_FRAMES {
            bail!(
                "{} contains more than {MAX_IMAGE_FRAMES} frames",
                path.display()
            );
        }
        let image = DynamicImage::ImageRgba8(frame?.into_buffer());
        output.push(decoded_to_jpeg_for_page(image, options, page_size)?);
    }
    if output.is_empty() {
        bail!("{} contains no decodable frames", path.display());
    }
    Ok(output)
}

#[cfg(windows)]
fn decode_tiff_pages(
    path: &Path,
    options: &ImageOptions,
    page_size: Option<&str>,
) -> Result<Vec<Vec<u8>>> {
    let decoder = crate::wic::Decoder::open(path)?;
    let count = usize::try_from(decoder.frame_count()?)?;
    if count > MAX_IMAGE_FRAMES {
        bail!(
            "{} contains {count} pages; maximum is {MAX_IMAGE_FRAMES}",
            path.display()
        );
    }
    (0..count)
        .map(|index| {
            decoded_to_jpeg_for_page(
                decoder.decode_frame(u32::try_from(index)?)?,
                options,
                page_size,
            )
        })
        .collect()
}

#[cfg(not(windows))]
fn decode_tiff_pages(
    path: &Path,
    options: &ImageOptions,
    page_size: Option<&str>,
) -> Result<Vec<Vec<u8>>> {
    Ok(vec![to_jpeg(path, options, page_size)?])
}

pub fn for_ocr(path: &Path, options: &ImageOptions) -> Result<Vec<u8>> {
    if formats::detect(path).is_some_and(Format::requires_jpeg_conversion) {
        return to_jpeg(path, options, None);
    }
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
    ffmpeg_to_jpeg_for_page(path, options, None)
}

fn ffmpeg_to_jpeg_for_page(
    path: &Path,
    options: &ImageOptions,
    page_size: Option<&str>,
) -> Result<Vec<u8>> {
    decoded_to_jpeg_for_page(decode_with_ffmpeg(path, options)?, options, page_size)
}

fn decoded_to_jpeg_for_page(
    image: DynamicImage,
    options: &ImageOptions,
    page_size: Option<&str>,
) -> Result<Vec<u8>> {
    let target_dimensions = dpi_target(&image, page_size, options.image_dpi);
    resize_to_jpeg(&image, target_dimensions, options.jpeg_quality)
}

fn decode_with_ffmpeg(path: &Path, options: &ImageOptions) -> Result<DynamicImage> {
    let temporary = Builder::new().suffix(".png").tempfile()?;
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
        .arg("-pix_fmt")
        .arg("rgba")
        .arg(&output_path);
    let output = process::run(command, Duration::from_secs(120), "ffmpeg").with_context(|| {
        format!(
            "install ffmpeg or pass --ffmpeg {}",
            options.ffmpeg.display()
        )
    })?;
    process::require_success(output, "ffmpeg").with_context(|| {
        format!(
            "the configured FFmpeg build cannot decode {}",
            path.display()
        )
    })?;

    image::open(&output_path).context("ffmpeg produced no decodable PNG output")
}

fn dpi_target(image: &DynamicImage, page_size: Option<&str>, image_dpi: u32) -> Option<(u32, u32)> {
    let (width, height) = image.dimensions();
    dpi_target_for_dimensions(width, height, page_size, image_dpi)
}

fn dpi_target_for_dimensions(
    width: u32,
    height: u32,
    page_size: Option<&str>,
    image_dpi: u32,
) -> Option<(u32, u32)> {
    if image_dpi == 0 {
        return None;
    }
    let size = page_size?;
    let (page_width, page_height) = crate::pdf::paper_size(size).ok()?;
    fit_dimensions_for_dpi(width, height, page_width, page_height, image_dpi)
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
    use image::codecs::gif::GifEncoder;
    use image::{Delay, Frame, Rgba, RgbaImage};

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
    fn decoded_external_image_uses_the_common_page_dpi_target() {
        let image = DynamicImage::ImageRgb8(RgbImage::new(2400, 1600));
        assert_eq!(dpi_target(&image, Some("A4"), 150), Some((1754, 1169)));
        assert_eq!(dpi_target(&image, Some("A4"), 0), None);
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

    #[test]
    fn animated_gif_frames_share_the_pdf_image_pipeline() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("animation.gif");
        let file = File::create(&path).unwrap();
        let mut encoder = GifEncoder::new(file);
        encoder
            .encode_frames([
                Frame::from_parts(
                    RgbaImage::from_pixel(3, 2, Rgba([255, 0, 0, 255])),
                    0,
                    0,
                    Delay::from_numer_denom_ms(100, 1),
                ),
                Frame::from_parts(
                    RgbaImage::from_pixel(3, 2, Rgba([0, 255, 0, 255])),
                    0,
                    0,
                    Delay::from_numer_denom_ms(100, 1),
                ),
            ])
            .unwrap();
        drop(encoder);

        let frames = to_jpegs_for_pdf(
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
        assert_eq!(frames.len(), 2);
        assert!(
            frames
                .iter()
                .all(|frame| image::guess_format(frame).unwrap() == ImageFormat::Jpeg)
        );
    }
}
