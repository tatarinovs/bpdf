//! Decoding of PDF image XObjects into `DynamicImage`, shared by optimize,
//! OCR and image extraction.

use anyhow::{Context, Result, bail};
use image::{DynamicImage, GrayImage, RgbImage};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

use super::MAX_DECOMPRESSED_BYTES;
use crate::{imageconv, output, parallel};

/// A page image re-encoded (or copied) as JPEG, oriented like the page.
#[derive(Debug)]
pub struct ExtractedImage {
    pub label: String,
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Page images to extract: every image on a page, or only its largest one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Selection {
    Largest,
    /// All images of at least this many pixels.
    AllFrom(u64),
}

/// Images of every page, in page order. Pages are processed in parallel;
/// undecodable images are skipped.
pub fn extract_images(
    document: &Document,
    selection: Selection,
    jpeg_quality: u8,
) -> Vec<(ObjectId, Vec<ExtractedImage>)> {
    let pages = document.get_pages().into_iter().collect::<Vec<_>>();
    parallel::map(&pages, parallel::cpu_jobs(), |&(number, page_id)| {
        (
            page_id,
            page_images(document, number, page_id, selection, jpeg_quality),
        )
    })
}

fn page_images(
    document: &Document,
    page_number: u32,
    page_id: ObjectId,
    selection: Selection,
    jpeg_quality: u8,
) -> Vec<ExtractedImage> {
    let mut images = match document.get_page_images(page_id) {
        Ok(images) => images,
        Err(error) => {
            output::warn(format!(
                "page {page_number}: cannot inspect images: {error}"
            ));
            return Vec::new();
        }
    };
    let area = |image: &lopdf::xobject::PdfImage| {
        u64::try_from(image.width).unwrap_or(0) * u64::try_from(image.height).unwrap_or(0)
    };
    // Largest first; for `Largest`, fall back to smaller ones only when the
    // largest cannot be decoded.
    images.sort_by_key(|image| std::cmp::Reverse(area(image)));
    let rotation = super::transform::page_geometry(document, page_id)
        .map(|geometry| geometry.rotation)
        .unwrap_or(0);

    let mut output = Vec::new();
    for (index, image) in images.iter().enumerate() {
        if let Selection::AllFrom(minimum) = selection
            && area(image) < minimum
        {
            break;
        }
        let Ok(stream) = document.get_object(image.id).and_then(Object::as_stream) else {
            continue;
        };
        let label = match selection {
            Selection::Largest => format!("page-{page_number}"),
            Selection::AllFrom(_) => format!("page-{page_number}-image-{}", index + 1),
        };
        match extract_one(document, stream, rotation, jpeg_quality, label) {
            Ok(extracted) => output.push(extracted),
            Err(_) => continue,
        }
        if selection == Selection::Largest {
            break;
        }
    }
    output
}

fn extract_one(
    document: &Document,
    stream: &Stream,
    rotation: i64,
    jpeg_quality: u8,
    label: String,
) -> Result<ExtractedImage> {
    // An upright RGB/grayscale JPEG is copied without recompression.
    if rotation == 0
        && !stream.dict.has(b"Decode")
        && let Some(jpeg) = embedded_jpeg(stream)
        && let Ok(info) = super::jpeg_info(jpeg)
        && matches!(info.components, 1 | 3)
    {
        return Ok(ExtractedImage {
            label,
            bytes: jpeg.to_vec(),
            width: u32::from(info.width),
            height: u32::from(info.height),
        });
    }
    let image = match rotation {
        90 => decode(document, stream)?.rotate90(),
        180 => decode(document, stream)?.rotate180(),
        270 => decode(document, stream)?.rotate270(),
        _ => decode(document, stream)?,
    };
    Ok(ExtractedImage {
        label,
        bytes: imageconv::encode_jpeg_on_white(&image, jpeg_quality)?,
        width: image.width(),
        height: image.height(),
    })
}

/// Colour space of the decoded samples, resolved through references,
/// ICC profiles and indexed palettes.
#[derive(Clone, Debug, PartialEq)]
enum ColorSpace {
    Gray,
    Rgb,
    Cmyk,
    /// Single-component ink coverage: 0 is paper, the maximum is full ink.
    Separation,
    Indexed {
        base: Box<ColorSpace>,
        palette: Vec<u8>,
    },
}

impl ColorSpace {
    fn components(&self) -> usize {
        match self {
            Self::Gray | Self::Separation | Self::Indexed { .. } => 1,
            Self::Rgb => 3,
            Self::Cmyk => 4,
        }
    }
}

/// Returns the embedded JPEG when the stream is a plain `DCTDecode` image
/// that can be copied out without decoding.
pub(crate) fn embedded_jpeg(stream: &Stream) -> Option<&[u8]> {
    let filters = stream.filters().ok()?;
    let is_plain_dct = matches!(filters.as_slice(), [filter] if is_dct(filter));
    (is_plain_dct && stream.content.starts_with(&[0xff, 0xd8])).then_some(&stream.content)
}

pub(crate) fn decode(document: &Document, stream: &Stream) -> Result<DynamicImage> {
    let filters = stream.filters().unwrap_or_default();
    if let Some(last) = filters.last() {
        if is_dct(last) {
            return decode_dct(stream, filters.len());
        }
        if matches!(
            *last,
            b"JPXDecode" | b"JBIG2Decode" | b"CCITTFaxDecode" | b"CCF"
        ) {
            bail!("unsupported image filter {}", String::from_utf8_lossy(last));
        }
    }

    let width = dimension(stream, b"Width")?;
    let height = dimension(stream, b"Height")?;
    let dict = &stream.dict;
    let is_mask = dict
        .get(b"ImageMask")
        .and_then(Object::as_bool)
        .unwrap_or(false);
    let bits = if is_mask {
        1
    } else {
        dict.get(b"BitsPerComponent")
            .and_then(Object::as_i64)
            .unwrap_or(8)
    };
    if !matches!(bits, 1 | 2 | 4 | 8 | 16) {
        bail!("unsupported bits per component: {bits}");
    }
    let space = if is_mask {
        ColorSpace::Gray
    } else {
        match dict.get(b"ColorSpace") {
            Ok(object) => resolve_color_space(document, object, 0)?,
            Err(_) => ColorSpace::Gray,
        }
    };

    let raw = stream
        .decompressed_content_with_limit(MAX_DECOMPRESSED_BYTES)
        .context("failed to decompress image data")?;

    let components = space.components();
    let indexed = matches!(space, ColorSpace::Indexed { .. });
    let mut samples = unpack_samples(&raw, width, height, components, bits as u32, !indexed)?;
    if !indexed {
        apply_decode_inversion(&mut samples, dict, components);
    }
    to_image(width, height, &space, samples)
}

fn is_dct(filter: &[u8]) -> bool {
    filter == b"DCTDecode" || filter == b"DCT"
}

fn decode_dct(stream: &Stream, filter_count: usize) -> Result<DynamicImage> {
    let image = if filter_count == 1 {
        image::load_from_memory(&stream.content)
    } else {
        // Leading filters (typically FlateDecode) wrap the JPEG bytes.
        let mut dict = stream.dict.clone();
        let filters = stream.filters()?;
        dict.set(
            "Filter",
            filters[..filter_count - 1]
                .iter()
                .map(|name| Object::Name(name.to_vec()))
                .collect::<Vec<_>>(),
        );
        let jpeg = Stream::new(dict, stream.content.clone())
            .decompressed_content_with_limit(MAX_DECOMPRESSED_BYTES)
            .context("failed to decompress wrapped JPEG")?;
        image::load_from_memory(&jpeg)
    };
    image.context("failed to decode embedded JPEG")
}

pub(crate) fn dimension(stream: &Stream, key: &[u8]) -> Result<u32> {
    let value = stream.dict.get(key)?.as_i64()?;
    u32::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .context("invalid embedded image dimension")
}

fn resolve<'a>(document: &'a Document, object: &'a Object) -> Result<&'a Object> {
    match object {
        Object::Reference(id) => Ok(document.get_object(*id)?),
        other => Ok(other),
    }
}

fn resolve_color_space(document: &Document, object: &Object, depth: u8) -> Result<ColorSpace> {
    if depth > 8 {
        bail!("colour space nesting is too deep");
    }
    match resolve(document, object)? {
        Object::Name(name) => named_color_space(name),
        Object::Array(items) => {
            let family = items
                .first()
                .context("empty colour space array")?
                .as_name()?;
            match family {
                b"ICCBased" => {
                    let profile =
                        resolve(document, items.get(1).context("ICCBased without profile")?)?
                            .as_stream()?;
                    match profile.dict.get(b"N").and_then(Object::as_i64) {
                        Ok(1) => Ok(ColorSpace::Gray),
                        Ok(3) => Ok(ColorSpace::Rgb),
                        Ok(4) => Ok(ColorSpace::Cmyk),
                        _ => match profile.dict.get(b"Alternate") {
                            Ok(alternate) => resolve_color_space(document, alternate, depth + 1),
                            Err(_) => bail!("ICC profile has no valid component count"),
                        },
                    }
                }
                b"CalGray" => Ok(ColorSpace::Gray),
                b"CalRGB" => Ok(ColorSpace::Rgb),
                b"Separation" => Ok(ColorSpace::Separation),
                b"Indexed" | b"I" => {
                    let base = resolve_color_space(
                        document,
                        items.get(1).context("Indexed without base")?,
                        depth + 1,
                    )?;
                    if matches!(base, ColorSpace::Indexed { .. }) {
                        bail!("nested Indexed colour space");
                    }
                    let palette =
                        match resolve(document, items.get(3).context("Indexed without lookup")?)? {
                            Object::String(bytes, _) => bytes.clone(),
                            Object::Stream(stream) => stream
                                .decompressed_content_with_limit(MAX_DECOMPRESSED_BYTES)
                                .context("failed to read Indexed palette")?,
                            _ => bail!("invalid Indexed lookup table"),
                        };
                    Ok(ColorSpace::Indexed {
                        base: Box::new(base),
                        palette,
                    })
                }
                other => named_color_space(other),
            }
        }
        _ => bail!("invalid colour space object"),
    }
}

fn named_color_space(name: &[u8]) -> Result<ColorSpace> {
    match name {
        b"DeviceGray" | b"G" | b"CalGray" => Ok(ColorSpace::Gray),
        b"DeviceRGB" | b"RGB" | b"CalRGB" => Ok(ColorSpace::Rgb),
        b"DeviceCMYK" | b"CMYK" => Ok(ColorSpace::Cmyk),
        other => bail!(
            "unsupported colour space {}",
            String::from_utf8_lossy(other)
        ),
    }
}

/// Unpack row-aligned samples to one byte per component. When `scale` is set,
/// sub-byte values are stretched to 0..=255; otherwise (palette indices) they
/// are kept as-is. 16-bit samples keep their high byte.
fn unpack_samples(
    raw: &[u8],
    width: u32,
    height: u32,
    components: usize,
    bits: u32,
    scale: bool,
) -> Result<Vec<u8>> {
    let per_row = (width as usize)
        .checked_mul(components)
        .context("image row is too large")?;
    let total = per_row
        .checked_mul(height as usize)
        .context("image is too large")?;
    let row_bytes = (per_row * bits as usize).div_ceil(8);

    if bits == 8 {
        if raw.len() >= total {
            return Ok(raw[..total].to_vec());
        }
        let mut samples = raw.to_vec();
        samples.resize(total, 0);
        return Ok(samples);
    }

    let mut samples = Vec::with_capacity(total);
    for row in 0..height as usize {
        let start = row * row_bytes;
        let data = raw.get(start..).unwrap_or_default();
        let data = &data[..row_bytes.min(data.len())];
        if bits == 16 {
            samples.extend(data.chunks_exact(2).map(|pair| pair[0]).take(per_row));
        } else {
            let mask = (1u8 << bits) - 1;
            let per_byte = 8 / bits as usize;
            let factor = if scale { 255 / mask } else { 1 };
            samples.extend(
                data.iter()
                    .flat_map(|byte| {
                        (0..per_byte).map(move |index| {
                            let shift = 8 - bits as usize * (index + 1);
                            ((byte >> shift) & mask) * factor
                        })
                    })
                    .take(per_row),
            );
        }
        // Short rows (truncated data) are padded with zeros.
        samples.resize((row + 1) * per_row, 0);
    }
    Ok(samples)
}

fn apply_decode_inversion(samples: &mut [u8], dict: &Dictionary, components: usize) {
    let Ok(decode) = dict.get(b"Decode").and_then(Object::as_array) else {
        return;
    };
    let number = |object: &Object| object.as_float().map(f64::from).unwrap_or(0.0);
    let inverted = (0..components)
        .map(|index| {
            decode
                .get(index * 2..index * 2 + 2)
                .is_some_and(|pair| number(&pair[0]) > number(&pair[1]))
        })
        .collect::<Vec<_>>();
    if !inverted.contains(&true) {
        return;
    }
    for pixel in samples.chunks_exact_mut(components) {
        for (sample, invert) in pixel.iter_mut().zip(&inverted) {
            if *invert {
                *sample = 255 - *sample;
            }
        }
    }
}

fn to_image(width: u32, height: u32, space: &ColorSpace, samples: Vec<u8>) -> Result<DynamicImage> {
    let image = match space {
        ColorSpace::Gray => DynamicImage::ImageLuma8(
            GrayImage::from_raw(width, height, samples).context("invalid grayscale image data")?,
        ),
        ColorSpace::Separation => {
            let gray = samples.into_iter().map(|ink| 255 - ink).collect();
            DynamicImage::ImageLuma8(
                GrayImage::from_raw(width, height, gray)
                    .context("invalid separation image data")?,
            )
        }
        ColorSpace::Rgb => DynamicImage::ImageRgb8(
            RgbImage::from_raw(width, height, samples).context("invalid RGB image data")?,
        ),
        ColorSpace::Cmyk => DynamicImage::ImageRgb8(
            RgbImage::from_raw(width, height, cmyk_to_rgb(&samples))
                .context("invalid CMYK image data")?,
        ),
        ColorSpace::Indexed { base, palette } => {
            let base_components = base.components();
            let mut expanded = Vec::with_capacity(samples.len() * base_components);
            for index in samples {
                let start = usize::from(index) * base_components;
                match palette.get(start..start + base_components) {
                    Some(color) => expanded.extend_from_slice(color),
                    None => expanded.extend(std::iter::repeat_n(0, base_components)),
                }
            }
            return to_image(width, height, base, expanded);
        }
    };
    Ok(image)
}

fn cmyk_to_rgb(cmyk: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(cmyk.len() / 4 * 3);
    for pixel in cmyk.chunks_exact(4) {
        let white = 255 - u32::from(pixel[3]);
        for &ink in &pixel[..3] {
            rgb.push(((255 - u32::from(ink)) * white / 255) as u8);
        }
    }
    rgb
}

#[cfg(test)]
mod tests {
    use lopdf::{Stream, dictionary};

    use super::*;

    fn image_stream(dict: Dictionary, data: Vec<u8>) -> Stream {
        let mut dict = dict;
        dict.set("Subtype", "Image");
        Stream::new(dict, data)
    }

    #[test]
    fn decodes_raw_rgb_stream() {
        let stream = image_stream(
            dictionary! {
                "Width" => 2, "Height" => 1,
                "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
            },
            vec![255, 0, 0, 0, 255, 0],
        );
        let document = Document::new();
        assert_eq!(
            decode(&document, &stream).unwrap().into_rgb8().into_raw(),
            stream.content
        );
    }

    #[test]
    fn decodes_raw_cmyk_stream() {
        let stream = image_stream(
            dictionary! {
                "Width" => 1, "Height" => 1,
                "ColorSpace" => "DeviceCMYK", "BitsPerComponent" => 8,
            },
            vec![0, 255, 255, 0],
        );
        let document = Document::new();
        assert_eq!(
            decode(&document, &stream).unwrap().into_rgb8().into_raw(),
            [255, 0, 0]
        );
    }

    #[test]
    fn iccbased_rgb_keeps_its_colours() {
        let mut document = Document::with_version("1.7");
        let profile = document.add_object(Stream::new(dictionary! {"N" => 3}, Vec::new()));
        let pixels = vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0];
        let stream = image_stream(
            dictionary! {
                "Width" => 2, "Height" => 2, "BitsPerComponent" => 8,
                "ColorSpace" => vec![Object::Name(b"ICCBased".to_vec()), Object::Reference(profile)],
            },
            pixels.clone(),
        );
        let image = decode(&document, &stream).unwrap();
        assert!(image.as_rgb8().is_some());
        assert_eq!(image.into_rgb8().into_raw(), pixels);
    }

    #[test]
    fn indexed_palette_is_expanded() {
        let document = Document::new();
        let stream = image_stream(
            dictionary! {
                "Width" => 3, "Height" => 1, "BitsPerComponent" => 4,
                "ColorSpace" => vec![
                    Object::Name(b"Indexed".to_vec()),
                    Object::Name(b"DeviceRGB".to_vec()),
                    1.into(),
                    Object::string_literal(vec![10u8, 20, 30, 200, 210, 220]),
                ],
            },
            vec![0x01, 0x00],
        );
        assert_eq!(
            decode(&document, &stream).unwrap().into_rgb8().into_raw(),
            [10, 20, 30, 200, 210, 220, 10, 20, 30]
        );
    }

    #[test]
    fn one_bit_rows_are_byte_aligned_and_decode_inverts() {
        let document = Document::new();
        let stream = image_stream(
            dictionary! {
                "Width" => 3, "Height" => 2, "BitsPerComponent" => 1,
                "ColorSpace" => "DeviceGray",
                "Decode" => vec![1.into(), 0.into()],
            },
            vec![0b1010_0000, 0b0100_0000],
        );
        assert_eq!(
            decode(&document, &stream).unwrap().into_luma8().into_raw(),
            [0, 255, 0, 255, 0, 255]
        );
    }

    #[test]
    fn flate_predictor_is_applied() {
        use std::io::Write;
        // Two RGB pixels per row, PNG "Up" predictor on the second row.
        let rows = [[2u8, 10, 20, 30, 40, 50, 60], [2, 1, 1, 1, 1, 1, 1]];
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        for row in rows {
            encoder.write_all(&row).unwrap();
        }
        let document = Document::new();
        let stream = image_stream(
            dictionary! {
                "Width" => 2, "Height" => 2, "BitsPerComponent" => 8,
                "ColorSpace" => "DeviceRGB", "Filter" => "FlateDecode",
                "DecodeParms" => dictionary! {"Predictor" => 12, "Colors" => 3, "Columns" => 2},
            },
            encoder.finish().unwrap(),
        );
        assert_eq!(
            decode(&document, &stream).unwrap().into_rgb8().into_raw(),
            [10, 20, 30, 40, 50, 60, 11, 21, 31, 41, 51, 61]
        );
    }
}
