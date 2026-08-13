use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, StringFormat, dictionary};

use super::{image as pdf_image, object_number, paper_size, parse_page_selection};
use crate::imageconv;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StampMode {
    Auto,
    Over,
    Under,
}

impl StampMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "over" => Ok(Self::Over),
            "under" => Ok(Self::Under),
            _ => bail!("stamp mode must be auto, over or under"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct StampOptions {
    pub path: PathBuf,
    pub position: String,
    pub scale: f64,
    pub opacity: f64,
    pub pages: String,
    pub mode: StampMode,
}

#[derive(Clone, Copy, Debug)]
pub struct PageGeometry {
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
    pub top: f64,
    pub rotation: i64,
}

#[derive(Debug, Default)]
pub struct OptimizeReport {
    pub resized_images: usize,
    pub skipped_images: usize,
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
struct ImageResize {
    id: ObjectId,
    target_width: u32,
    target_height: u32,
}

impl PageGeometry {
    pub fn raw_width(self) -> f64 {
        (self.right - self.left).abs()
    }

    pub fn raw_height(self) -> f64 {
        (self.top - self.bottom).abs()
    }

    pub fn display_width(self) -> f64 {
        if self.rotation.rem_euclid(180) == 90 {
            self.raw_height()
        } else {
            self.raw_width()
        }
    }

    pub fn display_height(self) -> f64 {
        if self.rotation.rem_euclid(180) == 90 {
            self.raw_width()
        } else {
            self.raw_height()
        }
    }
}

pub fn auto_rotate(document: &mut Document) -> Result<Vec<u32>> {
    let pages = document.get_pages();
    let geometries = page_geometries(document, &pages)?;
    if geometries.is_empty() {
        return Ok(Vec::new());
    }
    let target_landscape = dominant_landscape(geometries.iter().map(|(_, _, geometry)| *geometry));

    let mut rotated = Vec::new();
    for (number, page_id, geometry) in geometries {
        let landscape = geometry.display_width() > geometry.display_height();
        if landscape != target_landscape {
            set_page_rotation(document, page_id, geometry.rotation - 90)?;
            rotated.push(number);
        }
    }
    Ok(rotated)
}

pub fn rotate_pages(document: &mut Document, pages: &str, degrees: i64) -> Result<()> {
    if degrees % 90 != 0 {
        bail!("rotation must be a multiple of 90 degrees");
    }
    let page_map = document.get_pages();
    let selected = parse_page_selection(pages, page_map.len())?;
    for (number, page_id) in page_map {
        if selected.contains(&(number as usize)) {
            let geometry = page_geometry(document, page_id)?;
            set_page_rotation(document, page_id, geometry.rotation + degrees)?;
        }
    }
    Ok(())
}

pub fn resize_pages(document: &mut Document, size: &str, pages: &str) -> Result<()> {
    resize_pages_with_orientation(document, size, pages, false)
}

pub fn resize_pages_preserving_orientation(
    document: &mut Document,
    size: &str,
    pages: &str,
) -> Result<()> {
    resize_pages_with_orientation(document, size, pages, true)
}

fn resize_pages_with_orientation(
    document: &mut Document,
    size: &str,
    pages: &str,
    preserve_orientation: bool,
) -> Result<()> {
    let (target_portrait_width, target_portrait_height) = paper_size(size)?;
    let page_map = document.get_pages();
    let selected = parse_page_selection(pages, page_map.len())?;
    let geometries = page_geometries(document, &page_map)?;
    let common_orientation = (!preserve_orientation).then(|| {
        dominant_landscape(
            geometries
                .iter()
                .filter(|(number, _, _)| selected.contains(&(*number as usize)))
                .map(|(_, _, geometry)| *geometry),
        )
    });

    for (number, page_id, geometry) in geometries {
        if !selected.contains(&(number as usize)) {
            continue;
        }
        let landscape = common_orientation
            .unwrap_or_else(|| geometry.display_width() > geometry.display_height());
        let (target_display_width, target_display_height) = if landscape {
            (target_portrait_height, target_portrait_width)
        } else {
            (target_portrait_width, target_portrait_height)
        };
        let quarter_turn = geometry.rotation.rem_euclid(180) == 90;
        let (target_raw_width, target_raw_height) = if quarter_turn {
            (target_display_height, target_display_width)
        } else {
            (target_display_width, target_display_height)
        };

        let scale = (target_raw_width / geometry.raw_width())
            .min(target_raw_height / geometry.raw_height());
        let translate_x =
            (target_raw_width - geometry.raw_width() * scale) / 2.0 - geometry.left * scale;
        let translate_y =
            (target_raw_height - geometry.raw_height() * scale) / 2.0 - geometry.bottom * scale;
        wrap_page_contents(
            document,
            page_id,
            &format!("q\n{scale:.8} 0 0 {scale:.8} {translate_x:.8} {translate_y:.8} cm\n"),
            b"Q\n",
        )?;
        transform_annotation_rectangles(document, page_id, scale, translate_x, translate_y);

        let page = document.get_object_mut(page_id)?.as_dict_mut()?;
        let target_box = vec![
            0.into(),
            0.into(),
            target_raw_width.into(),
            target_raw_height.into(),
        ];
        page.set("MediaBox", target_box.clone());
        page.set("CropBox", target_box.clone());
        for key in [
            b"BleedBox".as_slice(),
            b"TrimBox".as_slice(),
            b"ArtBox".as_slice(),
        ] {
            if page.get(key).is_ok() {
                page.set(key, target_box.clone());
            }
        }
    }
    Ok(())
}

fn page_geometries(
    document: &Document,
    pages: &BTreeMap<u32, ObjectId>,
) -> Result<Vec<(u32, ObjectId, PageGeometry)>> {
    pages
        .iter()
        .map(|(&number, &page_id)| Ok((number, page_id, page_geometry(document, page_id)?)))
        .collect()
}

fn dominant_landscape(geometries: impl IntoIterator<Item = PageGeometry>) -> bool {
    let mut first = None;
    let (mut landscapes, mut portraits) = (0, 0);
    for geometry in geometries {
        let landscape = geometry.display_width() > geometry.display_height();
        first.get_or_insert(landscape);
        if landscape {
            landscapes += 1;
        } else {
            portraits += 1;
        }
    }
    if landscapes == portraits {
        first.unwrap_or(false)
    } else {
        landscapes > portraits
    }
}

pub fn apply_stamp(document: &mut Document, options: &StampOptions) -> Result<()> {
    if !(0.0..=1.0).contains(&options.opacity) {
        bail!("stamp opacity must be between 0 and 1");
    }
    if options.scale < 0.0 {
        bail!("stamp scale cannot be negative");
    }

    let image = image::open(&options.path)
        .with_context(|| format!("failed to read stamp {}", options.path.display()))?
        .to_rgba8();
    let (pixel_width, pixel_height) = image.dimensions();
    let mut rgb = Vec::with_capacity((pixel_width * pixel_height * 3) as usize);
    let mut alpha = Vec::with_capacity((pixel_width * pixel_height) as usize);
    let mut has_transparency = options.opacity < 1.0;
    for pixel in image.pixels() {
        rgb.extend_from_slice(&pixel.0[..3]);
        let value = (f64::from(pixel.0[3]) * options.opacity).round() as u8;
        alpha.push(value);
        has_transparency |= value != 255;
    }

    let alpha_id = has_transparency.then(|| {
        document.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => pixel_width as i64,
                "Height" => pixel_height as i64,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8,
            },
            alpha,
        ))
    });
    let mut image_dictionary = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Image",
        "Width" => pixel_width as i64,
        "Height" => pixel_height as i64,
        "ColorSpace" => "DeviceRGB",
        "BitsPerComponent" => 8,
    };
    if let Some(alpha_id) = alpha_id {
        image_dictionary.set("SMask", alpha_id);
    }
    let image_id = document.add_object(Stream::new(image_dictionary, rgb));

    let page_map = document.get_pages();
    let selected = parse_page_selection(&options.pages, page_map.len())?;
    for (number, page_id) in page_map {
        if !selected.contains(&(number as usize)) {
            continue;
        }
        let geometry = page_geometry(document, page_id)?;
        let resource_name = format!("BpdfStamp{number}");
        install_xobject_resource(document, page_id, resource_name.as_bytes(), image_id)?;

        let natural_width = f64::from(pixel_width) * 72.0 / 96.0;
        let natural_height = f64::from(pixel_height) * 72.0 / 96.0;
        let scale = if options.scale > 0.0 {
            options.scale
        } else {
            1.0f64
                .min(geometry.raw_width() * 0.25 / natural_width)
                .min(geometry.raw_height() * 0.25 / natural_height)
        };
        let width = natural_width * scale;
        let height = natural_height * scale;
        let (x, y) = stamp_position(&options.position, geometry, width, height)?;
        let content =
            format!("q\n{width:.6} 0 0 {height:.6} {x:.6} {y:.6} cm\n/{resource_name} Do\nQ\n");
        let content_id = document.add_object(Stream::new(dictionary! {}, content.into_bytes()));

        let under = match options.mode {
            StampMode::Under => true,
            StampMode::Over => false,
            StampMode::Auto => document
                .get_page_fonts(page_id)
                .map(|fonts| !fonts.is_empty())
                .unwrap_or(false),
        };
        add_page_content(document, page_id, content_id, under)?;
    }
    Ok(())
}

pub fn optimize(document: &mut Document, image_dpi: u32, jpeg_quality: u8) -> OptimizeReport {
    let mut report = downsample_images(document, image_dpi, jpeg_quality);
    document.delete_zero_length_streams();
    document.prune_objects();
    document.renumber_objects();
    document.compress();
    report.warnings.shrink_to_fit();
    report
}

fn downsample_images(document: &mut Document, image_dpi: u32, jpeg_quality: u8) -> OptimizeReport {
    let mut report = OptimizeReport::default();
    if image_dpi == 0 {
        return report;
    }

    let candidates = collect_image_resizes(document, image_dpi, &mut report.warnings);

    for candidate in candidates {
        match downsample_image(document, candidate, jpeg_quality) {
            Ok(()) => report.resized_images += 1,
            Err(error) => {
                report.skipped_images += 1;
                report.warnings.push(format!(
                    "image object {} {}: {error:#}",
                    candidate.id.0, candidate.id.1
                ));
            }
        }
    }
    report
}

fn collect_image_resizes(
    document: &Document,
    image_dpi: u32,
    warnings: &mut Vec<String>,
) -> Vec<ImageResize> {
    let mut targets = BTreeMap::<ObjectId, (u32, u32)>::new();
    for (page_number, page_id) in document.get_pages() {
        let geometry = match page_geometry(document, page_id) {
            Ok(geometry) => geometry,
            Err(error) => {
                warnings.push(format!(
                    "page {page_number}: cannot read page size: {error:#}"
                ));
                continue;
            }
        };
        let user_unit = inherited_value(document, page_id, b"UserUnit")
            .and_then(|value| object_number(&value).ok())
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(1.0);
        let page_width = geometry.display_width() * user_unit;
        let page_height = geometry.display_height() * user_unit;
        let images = match document.get_page_images(page_id) {
            Ok(images) => images,
            Err(error) => {
                warnings.push(format!(
                    "page {page_number}: cannot inspect images: {error:#}"
                ));
                continue;
            }
        };
        for image in images {
            let Ok(width) = u32::try_from(image.width) else {
                continue;
            };
            let Ok(height) = u32::try_from(image.height) else {
                continue;
            };
            let Some(target) = imageconv::fit_dimensions_for_dpi(
                width,
                height,
                page_width,
                page_height,
                image_dpi,
            ) else {
                continue;
            };
            targets
                .entry(image.id)
                .and_modify(|current| {
                    if target.0 > current.0 {
                        *current = target;
                    }
                })
                .or_insert(target);
        }
    }
    targets
        .into_iter()
        .map(|(id, (target_width, target_height))| ImageResize {
            id,
            target_width,
            target_height,
        })
        .collect()
}

fn downsample_image(document: &mut Document, resize: ImageResize, jpeg_quality: u8) -> Result<()> {
    let decoded = {
        let original = document.get_object(resize.id)?.as_stream()?;
        reject_unsafe_image_features(original)?;
        pdf_image::decode(original)?
    };
    let jpeg = imageconv::resize_to_jpeg(
        &decoded,
        Some((resize.target_width, resize.target_height)),
        jpeg_quality,
    )?;

    let stream = document.get_object_mut(resize.id)?.as_stream_mut()?;
    stream.dict.set("Width", i64::from(resize.target_width));
    stream.dict.set("Height", i64::from(resize.target_height));
    stream.dict.set("ColorSpace", "DeviceRGB");
    stream.dict.set("BitsPerComponent", 8);
    stream.dict.set("Filter", "DCTDecode");
    stream.dict.remove(b"DecodeParms");
    stream.dict.remove(b"Decode");
    stream.set_content(jpeg);
    Ok(())
}

fn reject_unsafe_image_features(stream: &Stream) -> Result<()> {
    if stream.dict.get(b"SMask").is_ok() || stream.dict.get(b"Mask").is_ok() {
        bail!("image masks are preserved without resampling");
    }
    if stream
        .dict
        .get(b"ImageMask")
        .ok()
        .and_then(|value| value.as_bool().ok())
        .unwrap_or(false)
    {
        bail!("stencil images are preserved without resampling");
    }
    if stream.dict.get(b"Decode").is_ok() {
        bail!("images with a custom Decode array are preserved");
    }
    let bits = stream
        .dict
        .get(b"BitsPerComponent")
        .ok()
        .and_then(|value| value.as_i64().ok())
        .unwrap_or(8);
    if bits != 8 {
        bail!("only 8-bit images can be resampled safely");
    }
    Ok(())
}

pub fn set_info_properties(document: &mut Document, author: &str, creator: &str) -> Result<()> {
    set_info_fields(
        document,
        &[
            ("Author", (!author.is_empty()).then_some(author)),
            ("Creator", (!creator.is_empty()).then_some(creator)),
        ],
    )
}

pub fn set_info_fields(document: &mut Document, fields: &[(&str, Option<&str>)]) -> Result<()> {
    if fields.iter().all(|(_, value)| value.is_none()) {
        return Ok(());
    }

    let info_id = match document
        .trailer
        .get(b"Info")
        .ok()
        .and_then(|value| value.as_reference().ok())
    {
        Some(id) => id,
        None => {
            let id = document.add_object(Dictionary::new());
            document.trailer.set("Info", id);
            id
        }
    };
    let info = document.get_object_mut(info_id)?.as_dict_mut()?;
    for (key, value) in fields {
        if let Some(value) = value {
            if value.is_empty() {
                info.remove(key.as_bytes());
            } else {
                info.set(key.as_bytes(), info_string(value));
            }
        }
    }
    Ok(())
}

fn info_string(value: &str) -> Object {
    if value.is_ascii() {
        return Object::string_literal(value);
    }
    let mut bytes = vec![0xfe, 0xff];
    for unit in value.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    Object::String(bytes, StringFormat::Hexadecimal)
}

pub fn page_geometry(document: &Document, page_id: ObjectId) -> Result<PageGeometry> {
    let media_box =
        inherited_value(document, page_id, b"MediaBox").context("page has no MediaBox")?;
    let values = media_box.as_array().context("MediaBox is not an array")?;
    if values.len() != 4 {
        bail!("MediaBox must contain four numbers");
    }
    let rotation = inherited_value(document, page_id, b"Rotate")
        .and_then(|value| value.as_i64().ok())
        .unwrap_or(0)
        .rem_euclid(360);
    Ok(PageGeometry {
        left: object_number(&values[0])?,
        bottom: object_number(&values[1])?,
        right: object_number(&values[2])?,
        top: object_number(&values[3])?,
        rotation,
    })
}

fn inherited_value(document: &Document, page_id: ObjectId, key: &[u8]) -> Option<Object> {
    let mut current = page_id;
    loop {
        let dictionary = document.get_dictionary(current).ok()?;
        if let Ok(value) = dictionary.get(key) {
            return Some(value.clone());
        }
        current = dictionary.get(b"Parent").ok()?.as_reference().ok()?;
    }
}

fn set_page_rotation(document: &mut Document, page_id: ObjectId, rotation: i64) -> Result<()> {
    document
        .get_object_mut(page_id)?
        .as_dict_mut()?
        .set("Rotate", rotation.rem_euclid(360));
    Ok(())
}

fn wrap_page_contents(
    document: &mut Document,
    page_id: ObjectId,
    prefix: &str,
    suffix: &[u8],
) -> Result<()> {
    let prefix_id = document.add_object(Stream::new(dictionary! {}, prefix.as_bytes().to_vec()));
    let suffix_id = document.add_object(Stream::new(dictionary! {}, suffix.to_vec()));
    let old = document
        .get_dictionary(page_id)?
        .get(b"Contents")
        .ok()
        .cloned();
    let mut contents = vec![Object::Reference(prefix_id)];
    append_content_objects(document, &mut contents, old);
    contents.push(Object::Reference(suffix_id));
    document
        .get_object_mut(page_id)?
        .as_dict_mut()?
        .set("Contents", contents);
    Ok(())
}

fn add_page_content(
    document: &mut Document,
    page_id: ObjectId,
    content_id: ObjectId,
    under: bool,
) -> Result<()> {
    let old = document
        .get_dictionary(page_id)?
        .get(b"Contents")
        .ok()
        .cloned();
    let mut contents = Vec::new();
    if under {
        contents.push(Object::Reference(content_id));
    }
    append_content_objects(document, &mut contents, old);
    if !under {
        contents.push(Object::Reference(content_id));
    }
    document
        .get_object_mut(page_id)?
        .as_dict_mut()?
        .set("Contents", contents);
    Ok(())
}

fn append_content_objects(
    document: &mut Document,
    destination: &mut Vec<Object>,
    old: Option<Object>,
) {
    match old {
        Some(Object::Array(items)) => destination.extend(items),
        Some(Object::Reference(id)) => destination.push(Object::Reference(id)),
        Some(Object::Stream(stream)) => {
            let id = document.add_object(stream);
            destination.push(Object::Reference(id));
        }
        Some(other) => destination.push(other),
        None => {}
    }
}

fn install_xobject_resource(
    document: &mut Document,
    page_id: ObjectId,
    name: &[u8],
    xobject_id: ObjectId,
) -> Result<()> {
    let mut resources = inherited_value(document, page_id, b"Resources")
        .and_then(|value| resolve_dictionary(document, &value))
        .unwrap_or_default();
    let mut xobjects = resources
        .get(b"XObject")
        .ok()
        .and_then(|value| resolve_dictionary(document, value))
        .unwrap_or_default();
    xobjects.set(name, xobject_id);
    resources.set("XObject", xobjects);
    let resources_id = document.add_object(resources);
    document
        .get_object_mut(page_id)?
        .as_dict_mut()?
        .set("Resources", resources_id);
    Ok(())
}

fn resolve_dictionary(document: &Document, object: &Object) -> Option<Dictionary> {
    match object {
        Object::Dictionary(dictionary) => Some(dictionary.clone()),
        Object::Reference(id) => document.get_dictionary(*id).ok().cloned(),
        _ => None,
    }
}

fn transform_annotation_rectangles(
    document: &mut Document,
    page_id: ObjectId,
    scale: f64,
    translate_x: f64,
    translate_y: f64,
) {
    let annotation_ids = document
        .get_dictionary(page_id)
        .ok()
        .and_then(|page| page.get(b"Annots").ok())
        .and_then(|annots| match annots {
            Object::Array(items) => Some(
                items
                    .iter()
                    .filter_map(|item| item.as_reference().ok())
                    .collect::<Vec<_>>(),
            ),
            Object::Reference(id) => document
                .get_object(*id)
                .ok()
                .and_then(|object| object.as_array().ok())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_reference().ok())
                        .collect()
                }),
            _ => None,
        })
        .unwrap_or_default();

    for annotation_id in annotation_ids {
        let Ok(annotation) = document
            .get_object_mut(annotation_id)
            .and_then(Object::as_dict_mut)
        else {
            continue;
        };
        let Ok(rectangle) = annotation.get(b"Rect").and_then(Object::as_array) else {
            continue;
        };
        if rectangle.len() != 4 {
            continue;
        }
        let Ok(values) = rectangle
            .iter()
            .map(object_number)
            .collect::<Result<Vec<_>>>()
        else {
            continue;
        };
        annotation.set(
            "Rect",
            vec![
                (values[0] * scale + translate_x).into(),
                (values[1] * scale + translate_y).into(),
                (values[2] * scale + translate_x).into(),
                (values[3] * scale + translate_y).into(),
            ],
        );
    }
}

fn stamp_position(value: &str, page: PageGeometry, width: f64, height: f64) -> Result<(f64, f64)> {
    const MM_TO_POINTS: f64 = 72.0 / 25.4;
    let margin = 10.0 * MM_TO_POINTS;
    let mut position = value.trim().to_ascii_lowercase();
    let mut offset_x = 0.0;
    let mut offset_y = 0.0;
    if let Some((x, y)) = position.split_once(',') {
        offset_x = x.trim().parse::<f64>().context("invalid stamp X offset")? * MM_TO_POINTS;
        offset_y = y.trim().parse::<f64>().context("invalid stamp Y offset")? * MM_TO_POINTS;
        position = "br".to_owned();
    }
    let position = match position.as_str() {
        "top-left" => "tl",
        "top-center" => "tc",
        "top-right" => "tr",
        "center-left" => "l",
        "center" => "c",
        "center-right" => "r",
        "bottom-left" => "bl",
        "bottom-center" => "bc",
        "bottom-right" | "" => "br",
        other => other,
    };

    let left = page.left + margin;
    let center_x = page.left + (page.raw_width() - width) / 2.0;
    let right = page.right - margin - width;
    let bottom = page.bottom + margin;
    let center_y = page.bottom + (page.raw_height() - height) / 2.0;
    let top = page.top - margin - height;
    let (x, y) = match position {
        "tl" => (left, top),
        "tc" => (center_x, top),
        "tr" => (right, top),
        "l" => (left, center_y),
        "c" => (center_x, center_y),
        "r" => (right, center_y),
        "bl" => (left, bottom),
        "bc" => (center_x, bottom),
        "br" => (right, bottom),
        _ => bail!("unsupported stamp position {position}"),
    };
    Ok((x + offset_x, y + offset_y))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::{DynamicImage, GenericImageView, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};

    use super::*;

    fn one_page() -> Document {
        let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 2, Rgb([20, 40, 60])));
        image_page(image, None)
    }

    fn placed_image(width: u32, height: u32, display_width: f64, display_height: f64) -> Document {
        let image = DynamicImage::ImageRgb8(RgbImage::from_fn(width, height, |x, y| {
            Rgb([
                (x % 251) as u8,
                (y % 241) as u8,
                ((x.wrapping_add(y)) % 239) as u8,
            ])
        }));
        image_page(image, Some((display_width, display_height)))
    }

    fn image_page(image: DynamicImage, placement: Option<(f64, f64)>) -> Document {
        let mut jpeg = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut jpeg), ImageFormat::Jpeg)
            .unwrap();
        let mut document = super::super::jpeg_document(jpeg, "A4").unwrap();
        if let Some((width, height)) = placement {
            let page_id = *document.get_pages().get(&1).unwrap();
            let content_id = document
                .get_dictionary(page_id)
                .unwrap()
                .get(b"Contents")
                .unwrap()
                .as_reference()
                .unwrap();
            document
                .get_object_mut(content_id)
                .unwrap()
                .as_stream_mut()
                .unwrap()
                .set_content(
                    format!("q\n{width} 0 0 {height} 10 20 cm\n/Im0 Do\nQ\n").into_bytes(),
                );
        }
        document
    }

    fn first_image_dimensions(document: &Document) -> (u32, u32) {
        let page_id = *document.get_pages().get(&1).unwrap();
        let image = document.get_page_images(page_id).unwrap().remove(0);
        (image.width as u32, image.height as u32)
    }

    fn duplicate_first_page(document: &mut Document) {
        let page_id = *document.get_pages().get(&1).unwrap();
        let page = document.get_dictionary(page_id).unwrap().clone();
        let parent_id = page.get(b"Parent").unwrap().as_reference().unwrap();
        let second_page_id = document.add_object(page);
        let pages = document
            .get_object_mut(parent_id)
            .unwrap()
            .as_dict_mut()
            .unwrap();
        let mut kids = pages.get(b"Kids").unwrap().as_array().unwrap().clone();
        kids.push(second_page_id.into());
        let count = kids.len() as i64;
        pages.set("Kids", kids);
        pages.set("Count", count);
    }

    #[test]
    fn rotate_changes_effective_orientation() {
        let mut document = one_page();
        let page_id = *document.get_pages().get(&1).unwrap();
        let before = page_geometry(&document, page_id).unwrap();
        rotate_pages(&mut document, "1", 90).unwrap();
        let after = page_geometry(&document, page_id).unwrap();
        assert_eq!(after.rotation, 90);
        assert_eq!(before.display_width(), after.display_height());
    }

    #[test]
    fn resize_sets_a4_box_and_preserves_page() {
        let mut document = one_page();
        resize_pages(&mut document, "A4", "all").unwrap();
        let page_id = *document.get_pages().get(&1).unwrap();
        let geometry = page_geometry(&document, page_id).unwrap();
        assert!((geometry.display_width() - 841.89).abs() < 0.1);
        assert!((geometry.display_height() - 595.28).abs() < 0.1);
    }

    #[test]
    fn auto_rotate_uses_majority_instead_of_first_page() {
        let mut document = one_page();
        duplicate_first_page(&mut document);
        duplicate_first_page(&mut document);
        rotate_pages(&mut document, "2-3", 90).unwrap();

        assert_eq!(auto_rotate(&mut document).unwrap(), vec![1]);
        assert!(document.get_pages().into_values().all(|page_id| {
            let geometry = page_geometry(&document, page_id).unwrap();
            geometry.display_width() < geometry.display_height()
        }));
    }

    #[test]
    fn resize_uses_majority_instead_of_first_page() {
        let mut document = one_page();
        duplicate_first_page(&mut document);
        duplicate_first_page(&mut document);
        rotate_pages(&mut document, "2-3", 90).unwrap();

        resize_pages(&mut document, "A4", "all").unwrap();
        assert!(document.get_pages().into_values().all(|page_id| {
            let geometry = page_geometry(&document, page_id).unwrap();
            geometry.display_width() < geometry.display_height()
        }));
    }

    #[test]
    fn resize_can_preserve_each_page_orientation() {
        let mut document = one_page();
        duplicate_first_page(&mut document);
        duplicate_first_page(&mut document);
        rotate_pages(&mut document, "2-3", 90).unwrap();

        resize_pages_preserving_orientation(&mut document, "A4", "all").unwrap();
        let orientations = document
            .get_pages()
            .into_values()
            .map(|page_id| {
                let geometry = page_geometry(&document, page_id).unwrap();
                geometry.display_width() > geometry.display_height()
            })
            .collect::<Vec<_>>();
        assert_eq!(orientations, vec![true, false, false]);
    }

    #[test]
    fn optimize_downsamples_image_to_page_dpi() {
        let mut document = placed_image(1200, 600, 144.0, 72.0);
        let report = optimize(&mut document, 36, 82);

        assert_eq!(report.resized_images, 1);
        assert_eq!(report.skipped_images, 0);
        assert!(report.warnings.is_empty());
        assert_eq!(first_image_dimensions(&document), (421, 211));

        let page_id = *document.get_pages().get(&1).unwrap();
        let embedded = document.get_page_images(page_id).unwrap().remove(0);
        let decoded = image::load_from_memory(embedded.content).unwrap();
        assert_eq!(decoded.dimensions(), (421, 211));

        let bytes = super::super::save_to_bytes(&mut document).unwrap();
        let parsed = Document::load_mem(&bytes).unwrap();
        assert_eq!(parsed.get_pages().len(), 1);
        assert_eq!(first_image_dimensions(&parsed), (421, 211));
    }

    #[test]
    fn optimize_dpi_zero_keeps_image_dimensions() {
        let mut document = placed_image(1200, 600, 144.0, 72.0);
        let report = optimize(&mut document, 0, 82);

        assert_eq!(report.resized_images, 0);
        assert_eq!(report.skipped_images, 0);
        assert_eq!(first_image_dimensions(&document), (1200, 600));
    }

    #[test]
    fn optimize_resizes_a_shared_image_only_once() {
        let mut document = placed_image(1200, 600, 144.0, 72.0);
        duplicate_first_page(&mut document);

        let report = optimize(&mut document, 36, 82);

        assert_eq!(report.resized_images, 1);
        assert_eq!(document.get_pages().len(), 2);
        for page_id in document.get_pages().into_values() {
            let image = document.get_page_images(page_id).unwrap().remove(0);
            assert_eq!((image.width, image.height), (421, 211));
        }
    }

    #[test]
    fn optimize_preserves_masked_images() {
        let mut document = placed_image(1200, 600, 144.0, 72.0);
        let page_id = *document.get_pages().get(&1).unwrap();
        let image_id = document.get_page_images(page_id).unwrap().remove(0).id;
        document
            .get_object_mut(image_id)
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .dict
            .set(
                "Mask",
                vec![0.into(), 0.into(), 0.into(), 0.into(), 0.into(), 0.into()],
            );

        let report = optimize(&mut document, 100, 82);

        assert_eq!(report.resized_images, 0);
        assert_eq!(report.skipped_images, 1);
        assert_eq!(first_image_dimensions(&document), (1200, 600));
        assert!(report.warnings[0].contains("image masks are preserved"));
    }

    #[test]
    fn transparent_stamp_adds_an_image_resource() {
        let temporary = tempfile::Builder::new().suffix(".png").tempfile().unwrap();
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(8, 4, Rgba([255, 0, 0, 128])))
            .save_with_format(temporary.path(), ImageFormat::Png)
            .unwrap();
        let mut document = one_page();
        apply_stamp(
            &mut document,
            &StampOptions {
                path: temporary.path().to_path_buf(),
                position: "br".to_owned(),
                scale: 1.0,
                opacity: 0.5,
                pages: "all".to_owned(),
                mode: StampMode::Over,
            },
        )
        .unwrap();
        let page_id = *document.get_pages().get(&1).unwrap();
        assert_eq!(document.get_page_images(page_id).unwrap().len(), 2);
    }

    #[test]
    fn metadata_uses_utf16_for_cyrillic() {
        let mut document = one_page();
        set_info_fields(&mut document, &[("Title", Some("Реквизиты"))]).unwrap();
        let info_id = document
            .trailer
            .get(b"Info")
            .unwrap()
            .as_reference()
            .unwrap();
        let value = document
            .get_dictionary(info_id)
            .unwrap()
            .get(b"Title")
            .unwrap()
            .as_str()
            .unwrap();
        assert!(value.starts_with(&[0xfe, 0xff]));
    }
}
