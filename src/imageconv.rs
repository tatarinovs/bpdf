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
use image::metadata::Orientation as ExifOrientation;
use image::{
    DynamicImage, ExtendedColorType, GenericImageView, ImageDecoder, ImageFormat, ImageReader,
};
use tempfile::Builder;

use crate::formats::{self, Format};
use crate::metadata;
use crate::process;

const MAX_IMAGE_FRAMES: usize = 10_000;
/// Longest edge sent to vision OCR; larger images are downscaled first.
const OCR_MAX_EDGE: u32 = 1536;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Orientation {
    Portrait,
    Landscape,
}

impl Orientation {
    pub fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "portrait" => Ok(Self::Portrait),
            "landscape" => Ok(Self::Landscape),
            _ => bail!("invalid orientation '{value}': expected 'portrait' or 'landscape'"),
        }
    }

    pub fn is_landscape(self) -> bool {
        self == Self::Landscape
    }
}

#[derive(Clone, Debug)]
pub struct ImageOptions {
    pub keep_icc: bool,
    pub ffmpeg: PathBuf,
    pub jpeg_quality: u8,
    pub image_dpi: u32,
    pub long_edge: Option<u32>,
    pub short_edge: Option<u32>,
    pub orient: Option<Orientation>,
    pub rotation_degrees: Option<i64>,
    pub force_reencode: bool,
    pub raw_develop: bool,
}

impl Default for ImageOptions {
    fn default() -> Self {
        Self {
            keep_icc: false,
            ffmpeg: PathBuf::from("ffmpeg"),
            jpeg_quality: 95,
            image_dpi: 150,
            long_edge: None,
            short_edge: None,
            orient: None,
            rotation_degrees: None,
            force_reencode: false,
            raw_develop: false,
        }
    }
}

impl ImageOptions {
    /// True when pixels must be decoded even if the source already is a JPEG
    /// that fits the page.
    fn needs_reencode(&self) -> bool {
        self.force_reencode
            || self.long_edge.is_some()
            || self.short_edge.is_some()
            || self.orient.is_some()
            || self
                .rotation_degrees
                .is_some_and(|degrees| degrees.rem_euclid(360) != 0)
    }
}

pub fn to_jpeg(path: &Path, options: &ImageOptions, page_size: Option<&str>) -> Result<Vec<u8>> {
    match formats::detect(path) {
        Some(Format::FfmpegRaster) => {
            return apply_transformations(decode_with_ffmpeg(path, options)?, options, page_size);
        }
        Some(Format::WicRaster) => {
            return apply_transformations(crate::wic::decode(path)?, options, page_size);
        }
        Some(Format::CameraRaw) => return raw_to_jpeg(path, options, page_size),
        #[cfg(windows)]
        Some(Format::Raster) if is_tiff(path) => {
            return apply_transformations(crate::wic::decode(path)?, options, page_size);
        }
        _ => {}
    }

    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    if let Some(jpeg) = jpeg_fast_path(&bytes, options, page_size) {
        return Ok(jpeg);
    }
    let image = match decode_bytes(&bytes) {
        Ok(image) => image,
        Err(native_error) => decode_with_ffmpeg(path, options).with_context(|| {
            format!(
                "native decoder failed for {}: {native_error:#}",
                path.display()
            )
        })?,
    };
    apply_transformations(image, options, page_size)
}

/// Convert in-memory image bytes into a JPEG suitable for a PDF page.
pub fn bytes_to_jpeg(
    bytes: &[u8],
    options: &ImageOptions,
    page_size: Option<&str>,
) -> Result<Vec<u8>> {
    if let Some(jpeg) = jpeg_fast_path(bytes, options, page_size) {
        return Ok(jpeg);
    }
    apply_transformations(decode_bytes(bytes)?, options, page_size)
}

/// A JPEG that already fits the page is only stripped of metadata, which
/// keeps the original compressed pixels.
fn jpeg_fast_path(
    bytes: &[u8],
    options: &ImageOptions,
    page_size: Option<&str>,
) -> Option<Vec<u8>> {
    if options.needs_reencode() || image::guess_format(bytes).ok()? != ImageFormat::Jpeg {
        return None;
    }
    let (width, height) = ImageReader::with_format(Cursor::new(bytes), ImageFormat::Jpeg)
        .into_dimensions()
        .ok()?;
    if dpi_target_for_dimensions(width, height, page_size, options.image_dpi).is_some() {
        return None;
    }
    metadata::strip_jpeg(bytes, options.keep_icc).ok()
}

/// Decode image bytes and apply their EXIF orientation to the pixels.
pub(crate) fn decode_bytes(bytes: &[u8]) -> Result<DynamicImage> {
    let mut decoder = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("failed to determine image format")?
        .into_decoder()
        .context("failed to decode image")?;
    let orientation = decoder
        .orientation()
        .unwrap_or(ExifOrientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder).context("failed to decode image")?;
    image.apply_orientation(orientation);
    Ok(image)
}

#[cfg(windows)]
fn is_tiff(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| {
            value.eq_ignore_ascii_case("tif") || value.eq_ignore_ascii_case("tiff")
        })
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
    let open = || -> Result<BufReader<File>> {
        Ok(BufReader::new(File::open(path).with_context(|| {
            format!("failed to open {}", path.display())
        })?))
    };
    match extension.as_str() {
        "gif" => {
            let decoder = GifDecoder::new(open()?)
                .with_context(|| format!("failed to decode GIF {}", path.display()))?;
            encode_animation_frames(decoder.into_frames(), options, page_size, path)
        }
        "png" | "apng" => {
            let decoder = PngDecoder::new(open()?)
                .with_context(|| format!("failed to decode PNG {}", path.display()))?;
            if decoder.is_apng()? {
                encode_animation_frames(decoder.apng()?.into_frames(), options, page_size, path)
            } else {
                Ok(vec![to_jpeg(path, options, page_size)?])
            }
        }
        "webp" => {
            let decoder = WebPDecoder::new(open()?)
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
        output.push(apply_transformations(image, options, page_size)?);
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
    let count = decoder.frame_count()?;
    if count as usize > MAX_IMAGE_FRAMES {
        bail!(
            "{} contains {count} pages; maximum is {MAX_IMAGE_FRAMES}",
            path.display()
        );
    }
    (0..count)
        .map(|index| apply_transformations(decoder.decode_frame(index)?, options, page_size))
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

fn raw_to_jpeg(path: &Path, options: &ImageOptions, page_size: Option<&str>) -> Result<Vec<u8>> {
    if !options.raw_develop {
        match crate::raw::read_preview(path) {
            Ok(preview) => return bytes_to_jpeg(&preview, options, page_size),
            Err(preview_error) => {
                return crate::wic::decode(path)
                    .map_err(|_| preview_error)
                    .and_then(|image| apply_transformations(image, options, page_size));
            }
        }
    }
    match crate::wic::decode(path) {
        Ok(image) => apply_transformations(image, options, page_size),
        Err(error) if cfg!(windows) => Err(error),
        Err(_) => bytes_to_jpeg(&crate::raw::read_preview(path)?, options, page_size),
    }
}

pub fn for_ocr(path: &Path, options: &ImageOptions) -> Result<Vec<u8>> {
    if formats::detect(path).is_some_and(Format::requires_jpeg_conversion) {
        return to_jpeg(path, options, None);
    }
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let passthrough = match image::guess_format(&bytes) {
        // OCR engines ignore EXIF orientation, so rotated photos are decoded.
        Ok(ImageFormat::Jpeg) => metadata::jpeg_orientation(&bytes) == 1,
        Ok(ImageFormat::Png | ImageFormat::WebP | ImageFormat::Gif) => true,
        _ => false,
    };
    if passthrough {
        return Ok(bytes);
    }
    to_jpeg(path, options, None)
}

/// Downscale an image for vision OCR. Small images are passed through
/// untouched; only their header is parsed.
pub fn optimize_for_ocr(bytes: &[u8]) -> Result<Vec<u8>> {
    let (width, height) = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("failed to determine OCR image format")?
        .into_dimensions()
        .context("failed to read OCR image dimensions")?;
    if width <= OCR_MAX_EDGE && height <= OCR_MAX_EDGE && bytes.len() < 3 * 1024 * 1024 {
        return Ok(bytes.to_vec());
    }
    let image = image::load_from_memory(bytes).context("failed to decode OCR image")?;
    let target = fit_dimensions(width, height, OCR_MAX_EDGE, OCR_MAX_EDGE);
    resize_to_jpeg_with_filter(&image, target, 80, FilterType::CatmullRom)
}

fn decode_with_ffmpeg(path: &Path, options: &ImageOptions) -> Result<DynamicImage> {
    let temporary = Builder::new().suffix(".png").tempfile()?;
    let output_path = temporary.path().to_path_buf();

    let mut command = Command::new(&options.ffmpeg);
    command
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(path)
        // HEIC files often expose both the primary image and a thumbnail.
        .args([
            "-map",
            "0:v:0",
            "-frames:v",
            "1",
            "-map_metadata",
            "-1",
            "-pix_fmt",
            "rgba",
        ])
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

fn dpi_target_for_dimensions(
    width: u32,
    height: u32,
    page_size: Option<&str>,
    image_dpi: u32,
) -> Option<(u32, u32)> {
    if image_dpi == 0 {
        return None;
    }
    let (page_width, page_height) = crate::pdf::paper_size(page_size?).ok()?;
    fit_dimensions_for_dpi(width, height, page_width, page_height, image_dpi)
}

/// Encode as baseline JPEG. Grayscale stays single-channel and so do colour
/// images that are gray in practice (scans of black-and-white documents);
/// transparency is composited onto white.
pub(crate) fn encode_jpeg_on_white(image: &DynamicImage, quality: u8) -> Result<Vec<u8>> {
    let (width, height) = image.dimensions();
    let encode = |data: &[u8], color: ExtendedColorType| {
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, quality)
            .encode(data, width, height, color)
            .context("failed to encode JPEG")?;
        Ok(bytes)
    };
    let encode_rgb = |rgb: &[u8]| {
        if is_effectively_gray(rgb) {
            encode(&rgb_to_luma(rgb), ExtendedColorType::L8)
        } else {
            encode(rgb, ExtendedColorType::Rgb8)
        }
    };
    let blend = |channel: u8, alpha: u8| -> u8 {
        let alpha = u16::from(alpha);
        ((u16::from(channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8
    };

    match image {
        DynamicImage::ImageLuma8(gray) => encode(gray.as_raw(), ExtendedColorType::L8),
        DynamicImage::ImageRgb8(rgb) => encode_rgb(rgb.as_raw()),
        DynamicImage::ImageLuma16(_) => encode(image.to_luma8().as_raw(), ExtendedColorType::L8),
        DynamicImage::ImageLumaA8(_) | DynamicImage::ImageLumaA16(_) => {
            let gray = image
                .to_luma_alpha8()
                .as_raw()
                .chunks_exact(2)
                .map(|pixel| blend(pixel[0], pixel[1]))
                .collect::<Vec<_>>();
            encode(&gray, ExtendedColorType::L8)
        }
        _ if !image.color().has_alpha() => encode_rgb(image.to_rgb8().as_raw()),
        _ => {
            let rgb = image
                .to_rgba8()
                .as_raw()
                .chunks_exact(4)
                .flat_map(|pixel| {
                    let alpha = pixel[3];
                    [
                        blend(pixel[0], alpha),
                        blend(pixel[1], alpha),
                        blend(pixel[2], alpha),
                    ]
                })
                .collect::<Vec<_>>();
            encode_rgb(&rgb)
        }
    }
}

/// True when at most one pixel in 5000 has visibly different channels.
/// The bound is strict on purpose: a small coloured stamp or signature keeps
/// the image in colour, while scanner and JPEG chroma noise does not.
fn is_effectively_gray(rgb: &[u8]) -> bool {
    const MAX_CHANNEL_SPREAD: u8 = 24;
    let allowed = rgb.len() / 3 / 5000;
    let mut coloured = 0;
    for pixel in rgb.chunks_exact(3) {
        let max = pixel[0].max(pixel[1]).max(pixel[2]);
        let min = pixel[0].min(pixel[1]).min(pixel[2]);
        if max - min > MAX_CHANNEL_SPREAD {
            coloured += 1;
            if coloured > allowed {
                return false;
            }
        }
    }
    true
}

/// ITU-R BT.601 luma in 8-bit fixed point.
fn rgb_to_luma(rgb: &[u8]) -> Vec<u8> {
    rgb.chunks_exact(3)
        .map(|pixel| {
            let luma =
                77 * u32::from(pixel[0]) + 150 * u32::from(pixel[1]) + 29 * u32::from(pixel[2]);
            ((luma + 128) >> 8) as u8
        })
        .collect()
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
    Some(scaled(width, height, scale))
}

fn scaled(width: u32, height: u32, scale: f64) -> (u32, u32) {
    (
        (f64::from(width) * scale).round().max(1.0) as u32,
        (f64::from(height) * scale).round().max(1.0) as u32,
    )
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
            encode_jpeg_on_white(&resize(image, width, height, filter), quality)
        }
        None => encode_jpeg_on_white(image, quality),
    }
}

/// High-quality resize. Strong reductions first average whole pixel blocks
/// (cheap and alias-free) down to twice the target, then the requested
/// filter produces the final size; this is several times faster than running
/// a wide Lanczos kernel over the full-resolution image.
pub(crate) fn resize(
    image: &DynamicImage,
    width: u32,
    height: u32,
    filter: FilterType,
) -> DynamicImage {
    let (source_width, source_height) = image.dimensions();
    if source_width >= width.saturating_mul(4) && source_height >= height.saturating_mul(4) {
        let reduced = image.thumbnail_exact(width * 2, height * 2);
        return reduced.resize_exact(width, height, filter);
    }
    image.resize_exact(width, height, filter)
}

pub(crate) fn apply_transformations(
    mut image: DynamicImage,
    options: &ImageOptions,
    page_size: Option<&str>,
) -> Result<Vec<u8>> {
    if let Some(orient) = options.orient {
        let (width, height) = image.dimensions();
        if (width > height) != orient.is_landscape() && width != height {
            image = image.rotate90();
        }
    }

    match options
        .rotation_degrees
        .map(|degrees| degrees.rem_euclid(360))
    {
        Some(90) => image = image.rotate90(),
        Some(180) => image = image.rotate180(),
        Some(270) => image = image.rotate270(),
        _ => {}
    }

    let (width, height) = image.dimensions();
    let target_dimensions = if let Some(long) = options.long_edge {
        fit_dimensions(width, height, long, long)
    } else if let Some(short) = options.short_edge {
        let scale = (f64::from(short) / f64::from(width)).max(f64::from(short) / f64::from(height));
        ((scale - 1.0).abs() > f64::EPSILON).then(|| scaled(width, height, scale))
    } else {
        dpi_target_for_dimensions(width, height, page_size, options.image_dpi)
    };

    resize_to_jpeg(&image, target_dimensions, options.jpeg_quality)
}

#[cfg(test)]
mod tests {
    use image::codecs::gif::GifEncoder;
    use image::{Delay, Frame, Rgb, RgbImage, Rgba, RgbaImage};

    use super::*;

    fn no_dpi() -> ImageOptions {
        ImageOptions {
            ffmpeg: PathBuf::from("missing-ffmpeg"),
            jpeg_quality: 85,
            image_dpi: 0,
            ..ImageOptions::default()
        }
    }

    #[test]
    fn alpha_is_composited_onto_white() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(1, 1, Rgba([0, 0, 0, 0])));
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
    fn grayscale_stays_single_channel() {
        let image = DynamicImage::ImageLuma8(image::GrayImage::from_pixel(4, 4, image::Luma([90])));
        let jpeg = encode_jpeg_on_white(&image, 90).unwrap();
        assert_eq!(
            image::load_from_memory(&jpeg).unwrap().color(),
            image::ColorType::L8
        );
    }

    #[test]
    fn gray_rgb_scan_is_encoded_as_grayscale() {
        let scan = DynamicImage::ImageRgb8(RgbImage::from_fn(200, 100, |x, _| {
            let value = if x % 7 == 0 { 20 } else { 245 };
            Rgb([value, value, value.saturating_sub(8)])
        }));
        let jpeg = encode_jpeg_on_white(&scan, 90).unwrap();
        assert_eq!(
            image::load_from_memory(&jpeg).unwrap().color(),
            image::ColorType::L8
        );
    }

    #[test]
    fn small_colour_stamp_keeps_the_image_in_colour() {
        // A 10x10 blue mark on a 200x100 page is 0.5% of the pixels.
        let page = DynamicImage::ImageRgb8(RgbImage::from_fn(200, 100, |x, y| {
            if x < 10 && y < 10 {
                Rgb([30, 60, 200])
            } else {
                Rgb([240, 240, 240])
            }
        }));
        let jpeg = encode_jpeg_on_white(&page, 90).unwrap();
        assert_eq!(
            image::load_from_memory(&jpeg).unwrap().color(),
            image::ColorType::Rgb8
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
        assert_eq!(
            dpi_target_for_dimensions(2400, 1600, Some("A4"), 150),
            Some((1754, 1169))
        );
        assert_eq!(dpi_target_for_dimensions(2400, 1600, Some("A4"), 0), None);
    }

    #[test]
    fn strong_reduction_keeps_exact_target_and_colour() {
        let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(1000, 600, Rgb([200, 100, 50])));
        let resized = resize(&image, 100, 60, FilterType::Lanczos3);
        assert_eq!(resized.dimensions(), (100, 60));
        let pixel = resized.to_rgb8().get_pixel(50, 30).0;
        assert!(
            pixel
                .iter()
                .zip([200, 100, 50])
                .all(|(a, b)| a.abs_diff(b) <= 1)
        );
    }

    #[test]
    fn orientation_parsing_is_case_insensitive() {
        assert_eq!(
            Orientation::parse("Landscape").unwrap(),
            Orientation::Landscape
        );
        assert!(Orientation::parse("diagonal").is_err());
    }

    #[test]
    fn webp_is_decoded_without_ffmpeg() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.webp");
        DynamicImage::ImageRgb8(RgbImage::from_pixel(2, 2, Rgb([10, 20, 30])))
            .save_with_format(&path, ImageFormat::WebP)
            .unwrap();
        let jpeg = to_jpeg(&path, &no_dpi(), None).unwrap();
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

        let frames = to_jpegs_for_pdf(&path, &no_dpi(), None).unwrap();
        assert_eq!(frames.len(), 2);
        assert!(
            frames
                .iter()
                .all(|frame| image::guess_format(frame).unwrap() == ImageFormat::Jpeg)
        );
    }

    #[test]
    fn camera_raw_preview_is_extracted_and_converted_to_jpeg() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("photo.CR2");

        let mut jpeg_bytes = Vec::new();
        DynamicImage::ImageRgb8(RgbImage::from_pixel(10, 10, Rgb([200, 100, 50])))
            .write_to(&mut Cursor::new(&mut jpeg_bytes), ImageFormat::Jpeg)
            .unwrap();

        let mut fake_raw = Vec::new();
        fake_raw.extend_from_slice(b"RAW_HEADER_PADDING");
        fake_raw.extend_from_slice(&jpeg_bytes);
        fake_raw.extend_from_slice(b"RAW_FOOTER");
        fs::write(&path, fake_raw).unwrap();

        let result = to_jpeg(&path, &no_dpi(), None).unwrap();
        assert_eq!(image::guess_format(&result).unwrap(), ImageFormat::Jpeg);
    }
}
