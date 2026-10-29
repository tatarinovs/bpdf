//! Decoding of PDF image XObjects into `DynamicImage`, shared by optimize,
//! OCR and image extraction.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use image::{DynamicImage, GrayImage, RgbImage};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

use super::transform::PageGeometry;
use super::{MAX_DECOMPRESSED_BYTES, resolve};
use crate::imageconv::{self, MAX_IMAGE_PIXELS};
use crate::{output, parallel};

/// A page image re-encoded (or copied) as JPEG, upright as it is shown on
/// the displayed page.
#[derive(Debug)]
pub struct ExtractedImage {
    pub label: String,
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// `cm` matrix of the upright image's unit square in page user space.
    pub placement: Matrix,
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

pub(crate) fn page_images(
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
    let crop = match super::transform::page_crop_geometry(document, page_id) {
        Ok(crop) => crop,
        Err(error) => {
            output::warn(format!("page {page_number}: {error:#}"));
            return Vec::new();
        }
    };
    let drawings = largest_drawings(document, page_id).unwrap_or_default();
    // An image the content does not draw itself (pattern, annotation) is
    // taken to cover the page as it is shown.
    let whole_page = super::transform::upright_stamp_matrix(
        crop,
        0.0,
        0.0,
        crop.display_width(),
        crop.display_height(),
    );

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
            Selection::Largest => format!("page_{page_number}"),
            Selection::AllFrom(_) => format!("page_{page_number}_image_{}", index + 1),
        };
        let matrix = drawings.get(&image.id).copied().unwrap_or(whole_page);
        match extract_one(document, stream, matrix, crop, jpeg_quality, label) {
            Ok(extracted) => output.push(extracted),
            Err(_) => continue,
        }
        if selection == Selection::Largest {
            break;
        }
    }
    output
}

/// Decode an image as the page shows it: the stored samples (a PDF viewer
/// ignores EXIF orientation), turned by quarter turns to stand upright
/// under the matrix it is drawn with and the page rotation.
fn extract_one(
    document: &Document,
    stream: &Stream,
    matrix: Matrix,
    crop: PageGeometry,
    jpeg_quality: u8,
    label: String,
) -> Result<ExtractedImage> {
    let turns = upright_turns(matrix, crop);
    let placement = multiply(TURNS[turns], matrix);
    // An upright RGB/grayscale JPEG is copied without recompression.
    if turns == 0
        && !stream.dict.has(b"Decode")
        && let Some(jpeg) = embedded_jpeg(stream)
        && crate::metadata::jpeg_orientation(jpeg) == 1
        && let Ok(info) = super::jpeg_info(jpeg)
        && matches!(info.components, 1 | 3)
    {
        return Ok(ExtractedImage {
            label,
            bytes: jpeg.to_vec(),
            width: u32::from(info.width),
            height: u32::from(info.height),
            placement,
        });
    }
    let image = decode(document, stream)?;
    let image = match turns {
        1 => image.rotate90(),
        2 => image.rotate180(),
        3 => image.rotate270(),
        _ => image,
    };
    Ok(ExtractedImage {
        label,
        bytes: imageconv::encode_jpeg_on_white(&image, jpeg_quality)?,
        width: image.width(),
        height: image.height(),
        placement,
    })
}

/// For each count of clockwise quarter turns of the stored samples, the map
/// from the turned image's unit square to the stored image's unit square.
const TURNS: [Matrix; 4] = [
    [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
    [0.0, 1.0, -1.0, 0.0, 1.0, 0.0],
    [-1.0, 0.0, 0.0, -1.0, 1.0, 1.0],
    [0.0, -1.0, 1.0, 0.0, 0.0, 1.0],
];

/// Clockwise quarter turns (0-3) of the stored samples that make an image
/// drawn with `matrix` stand upright on the displayed page.
fn upright_turns(matrix: Matrix, crop: PageGeometry) -> usize {
    let [a, b, c, d, e, f] = matrix;
    let (_, origin) = crop.page_to_display(e, f);
    // How far the image's x and y axes rise on the displayed page.
    let rise = |x: f64, y: f64| crop.page_to_display(e + x, f + y).1 - origin;
    let (right, up) = (rise(a, b), rise(c, d));
    // Rise of the turned image's y axis for each turn (see `TURNS`).
    [up, -right, -up, right]
        .into_iter()
        .enumerate()
        .max_by(|left, other| left.1.total_cmp(&other.1))
        .map_or(0, |(turns, _)| turns)
}

/// Largest size in points at which each image XObject is drawn on a page,
/// following `cm`, `q`/`Q` and nested forms. Images the content never draws
/// directly (patterns, annotations) are absent.
pub(crate) fn placements(
    document: &Document,
    page_id: ObjectId,
) -> Result<HashMap<ObjectId, (f64, f64)>> {
    let mut found = HashMap::<ObjectId, (f64, f64)>::new();
    for (id, matrix) in drawn_images(document, page_id)? {
        let size = found.entry(id).or_insert((0.0, 0.0));
        let (width, height) = drawn_size(matrix);
        *size = (size.0.max(width), size.1.max(height));
    }
    Ok(found)
}

/// The matrix of the largest drawing of each image XObject on a page.
fn largest_drawings(document: &Document, page_id: ObjectId) -> Result<HashMap<ObjectId, Matrix>> {
    let mut largest = HashMap::<ObjectId, (f64, Matrix)>::new();
    for (id, matrix) in drawn_images(document, page_id)? {
        let (width, height) = drawn_size(matrix);
        let area = width * height;
        if largest.get(&id).is_none_or(|(known, _)| area > *known) {
            largest.insert(id, (area, matrix));
        }
    }
    Ok(largest
        .into_iter()
        .map(|(id, (_, matrix))| (id, matrix))
        .collect())
}

/// Every image drawing on a page with its current transformation matrix.
fn drawn_images(document: &Document, page_id: ObjectId) -> Result<Vec<(ObjectId, Matrix)>> {
    let mut found = Vec::new();
    let Some(resources) = super::transform::inherited_value(document, page_id, b"Resources") else {
        return Ok(found);
    };
    let resources = resolve(document, &resources)?.as_dict()?;
    if !has_xobjects(document, resources) {
        return Ok(found);
    }
    let content = document.get_page_content_with_limit(page_id, MAX_DECOMPRESSED_BYTES)?;
    let mut walk = Walk {
        forms_left: MAX_FORM_VISITS,
        found: &mut found,
    };
    walk.content(document, &content, resources, IDENTITY, 0)?;
    Ok(found)
}

fn drawn_size(matrix: Matrix) -> (f64, f64) {
    (matrix[0].hypot(matrix[1]), matrix[2].hypot(matrix[3]))
}

/// A PDF transformation matrix `[a b c d e f]`.
pub(crate) type Matrix = [f64; 6];

const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `first` applied, then `then`.
pub(crate) fn multiply(first: Matrix, then: Matrix) -> Matrix {
    [
        first[0] * then[0] + first[1] * then[2],
        first[0] * then[1] + first[1] * then[3],
        first[2] * then[0] + first[3] * then[2],
        first[2] * then[1] + first[3] * then[3],
        first[4] * then[0] + first[5] * then[2] + then[4],
        first[4] * then[1] + first[5] * then[3] + then[5],
    ]
}

fn matrix(operands: &[Object]) -> Option<Matrix> {
    let values = operands
        .iter()
        .map(|operand| super::object_number(operand).ok())
        .collect::<Option<Vec<_>>>()?;
    <[f64; 6]>::try_from(values).ok()
}

fn sub_dictionary<'a>(
    document: &'a Document,
    dictionary: &'a Dictionary,
    key: &[u8],
) -> Option<&'a Dictionary> {
    resolve(document, dictionary.get(key).ok()?)
        .ok()?
        .as_dict()
        .ok()
}

fn has_xobjects(document: &Document, resources: &Dictionary) -> bool {
    sub_dictionary(document, resources, b"XObject").is_some_and(|xobjects| !xobjects.is_empty())
}

/// Form XObjects drawn while walking one page: a small form drawn many
/// times by forms that are themselves drawn many times would otherwise
/// multiply the work beyond any bound.
const MAX_FORM_VISITS: usize = 10_000;

/// The image drawings found on a page so far.
struct Walk<'a> {
    forms_left: usize,
    found: &'a mut Vec<(ObjectId, Matrix)>,
}

impl Walk<'_> {
    /// Follow `content`, drawn under `base`, into images and nested forms.
    fn content(
        &mut self,
        document: &Document,
        content: &[u8],
        resources: &Dictionary,
        base: Matrix,
        depth: u8,
    ) -> Result<()> {
        if depth > 8 {
            bail!("form XObjects are nested too deeply");
        }
        let xobjects = sub_dictionary(document, resources, b"XObject");
        let mut current = base;
        let mut saved = Vec::new();
        for operation in lopdf::content::Content::decode(content)?.operations {
            match operation.operator.as_str() {
                "q" => saved.push(current),
                "Q" => current = saved.pop().unwrap_or(base),
                "cm" => {
                    if let Some(applied) = matrix(&operation.operands) {
                        current = multiply(applied, current);
                    }
                }
                "Do" => {
                    let Some(id) = operation
                        .operands
                        .first()
                        .and_then(|name| name.as_name().ok())
                        .and_then(|name| xobjects?.get(name).ok()?.as_reference().ok())
                    else {
                        continue;
                    };
                    let Ok(stream) = document.get_object(id).and_then(Object::as_stream) else {
                        continue;
                    };
                    match stream.dict.get(b"Subtype").and_then(Object::as_name) {
                        Ok(b"Image") => self.found.push((id, current)),
                        Ok(b"Form") => self.form(document, stream, resources, current, depth)?,
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn form(
        &mut self,
        document: &Document,
        stream: &Stream,
        resources: &Dictionary,
        current: Matrix,
        depth: u8,
    ) -> Result<()> {
        let form_resources =
            sub_dictionary(document, &stream.dict, b"Resources").unwrap_or(resources);
        if !has_xobjects(document, form_resources) {
            return Ok(());
        }
        self.forms_left = self
            .forms_left
            .checked_sub(1)
            .context("the page draws too many form XObjects")?;
        let form_matrix = stream
            .dict
            .get(b"Matrix")
            .and_then(Object::as_array)
            .ok()
            .and_then(|values| matrix(values))
            .unwrap_or(IDENTITY);
        let content = stream.decompressed_content_with_limit(MAX_DECOMPRESSED_BYTES)?;
        self.content(
            document,
            &content,
            form_resources,
            multiply(form_matrix, current),
            depth + 1,
        )
    }
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
    // The size comes from the dictionary, not from the data: a tiny stream
    // may claim billions of pixels.
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        bail!("embedded image is too large ({width}x{height})");
    }
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

pub(crate) fn is_dct(filter: &[u8]) -> bool {
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
    fn oversized_dimensions_are_refused_before_allocating() {
        let stream = image_stream(
            dictionary! {
                "Width" => 100_000, "Height" => 100_000,
                "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
            },
            vec![0; 16],
        );
        let error = decode(&Document::new(), &stream).unwrap_err();
        assert!(error.to_string().contains("too large"), "{error:#}");
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

    /// Corners of the unit square under `matrix`, rounded to whole points.
    fn corners(matrix: Matrix) -> [(i64, i64); 4] {
        let [a, b, c, d, e, f] = matrix;
        [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)].map(|(u, v)| {
            (
                (a * u + c * v + e).round() as i64,
                (b * u + d * v + f).round() as i64,
            )
        })
    }

    #[test]
    fn images_are_extracted_as_the_page_shows_them() {
        // A 40x20 JPEG tagged "rotate 90° clockwise": the page shows it 20x40.
        let stored = crate::commands::common::test_utils::sample_jpeg_bytes(40, 20);
        let tagged = crate::metadata::with_jpeg_orientation(&stored, 6).unwrap();
        let mut document = super::super::jpeg_document(tagged, super::super::PageSize::A4).unwrap();
        let page_id = *document.get_pages().get(&1).unwrap();
        let shown = |document: &Document| {
            let image = page_images(document, 1, page_id, Selection::Largest, 90).remove(0);
            ((image.width, image.height), corners(image.placement))
        };
        let (size, upright) = shown(&document);
        assert_eq!(size, (20, 40));
        // The upright unit square: x to the right, y up, on the portrait page.
        let [origin, right, top, _] = upright;
        assert!(right.0 > origin.0 && right.1 == origin.1, "{upright:?}");
        assert!(top.1 > origin.1 && top.0 == origin.0, "{upright:?}");

        // A page turned a quarter shows the picture lying on its side again.
        super::super::transform::rotate_pages(&mut document, "all", 90).unwrap();
        let (size, turned) = shown(&document);
        assert_eq!(size, (40, 20));
        assert_eq!(
            turned
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            upright.iter().copied().collect()
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
