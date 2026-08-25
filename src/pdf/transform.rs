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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlendMode {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
}

impl BlendMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().replace("-", "").as_str() {
            "normal" => Ok(Self::Normal),
            "multiply" => Ok(Self::Multiply),
            "screen" => Ok(Self::Screen),
            "overlay" => Ok(Self::Overlay),
            "darken" => Ok(Self::Darken),
            "lighten" => Ok(Self::Lighten),
            "colordodge" => Ok(Self::ColorDodge),
            "colorburn" => Ok(Self::ColorBurn),
            "hardlight" => Ok(Self::HardLight),
            "softlight" => Ok(Self::SoftLight),
            "difference" => Ok(Self::Difference),
            "exclusion" => Ok(Self::Exclusion),
            _ => bail!("unsupported blend mode: {}", value),
        }
    }

    pub fn to_pdf_name(self) -> &'static [u8] {
        match self {
            Self::Normal => b"Normal",
            Self::Multiply => b"Multiply",
            Self::Screen => b"Screen",
            Self::Overlay => b"Overlay",
            Self::Darken => b"Darken",
            Self::Lighten => b"Lighten",
            Self::ColorDodge => b"ColorDodge",
            Self::ColorBurn => b"ColorBurn",
            Self::HardLight => b"HardLight",
            Self::SoftLight => b"SoftLight",
            Self::Difference => b"Difference",
            Self::Exclusion => b"Exclusion",
        }
    }
}

#[derive(Clone, Debug)]
pub struct StampOptions {
    pub path: PathBuf,
    pub position: String,
    pub scale: Option<f64>,
    pub dpi: Option<f64>,
    pub opacity: f64,
    pub pages: String,
    pub mode: StampMode,
    pub blend_mode: BlendMode,
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

pub fn orient_pages(document: &mut Document, pages: &str, orient: &str) -> Result<()> {
    let target_landscape = match orient.to_lowercase().as_str() {
        "landscape" => true,
        "portrait" => false,
        _ => bail!("invalid orientation '{orient}': expected 'portrait' or 'landscape'"),
    };
    let page_map = document.get_pages();
    let selected = parse_page_selection(pages, page_map.len())?;
    for (number, page_id) in page_map {
        if selected.contains(&(number as usize)) {
            let geometry = page_geometry(document, page_id)?;
            let is_landscape = geometry.display_width() > geometry.display_height();
            if is_landscape != target_landscape {
                set_page_rotation(document, page_id, geometry.rotation + 90)?;
            }
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
    if let Some(scale) = options.scale
        && scale < 0.0
    {
        bail!("stamp scale cannot be negative");
    }
    if let Some(dpi) = options.dpi
        && dpi <= 0.0
    {
        bail!("stamp dpi must be positive");
    }

    let detected_dpi = detect_image_dpi(&options.path);
    let is_calibrated = options.dpi.is_some() || detected_dpi.is_some();
    let dpi = options.dpi.or(detected_dpi).unwrap_or(96.0);

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

    let ext_gstate_id = if options.blend_mode != BlendMode::Normal {
        Some(document.add_object(dictionary! {
            "Type" => "ExtGState",
            "BM" => Object::Name(options.blend_mode.to_pdf_name().to_vec()),
        }))
    } else {
        None
    };

    const STAMP_RESOURCE: &[u8] = b"BpdfStamp";
    const GSTATE_RESOURCE: &[u8] = b"BpdfExtGState";

    let page_map = document.get_pages();
    let selected = parse_page_selection(&options.pages, page_map.len())?;
    for (number, page_id) in page_map {
        if !selected.contains(&(number as usize)) {
            continue;
        }
        let geometry = page_geometry(document, page_id)?;
        install_xobject_resource(document, page_id, STAMP_RESOURCE, image_id)?;
        if let Some(ext_gstate_id) = ext_gstate_id {
            install_extgstate_resource(document, page_id, GSTATE_RESOURCE, ext_gstate_id)?;
        }

        let natural_width = f64::from(pixel_width) * 72.0 / dpi;
        let natural_height = f64::from(pixel_height) * 72.0 / dpi;
        let scale = match options.scale {
            Some(s) if s > 0.0 => s,
            Some(_) => {
                // scale == 0.0: auto-fit up to 25% of the page
                1.0f64
                    .min(geometry.raw_width() * 0.25 / natural_width)
                    .min(geometry.raw_height() * 0.25 / natural_height)
            }
            None => {
                if is_calibrated {
                    // DPI was explicitly passed or detected from image metadata: use 100% natural physical size
                    1.0
                } else {
                    // Uncalibrated 96 DPI fallback: auto-fit up to 25% of the page
                    1.0f64
                        .min(geometry.raw_width() * 0.25 / natural_width)
                        .min(geometry.raw_height() * 0.25 / natural_height)
                }
            }
        };
        let width = natural_width * scale;
        let height = natural_height * scale;
        let (x, y) = stamp_position(&options.position, geometry, width, height)?;

        let mut content = String::new();
        content.push_str("q\n");
        if ext_gstate_id.is_some() {
            content.push_str("/BpdfExtGState gs\n");
        }
        content.push_str(&format!(
            "{width:.6} 0 0 {height:.6} {x:.6} {y:.6} cm\n/BpdfStamp Do\nQ\n"
        ));

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
    // Protect against infinite loops in malformed PDFs with cyclical Parent chains
    for _ in 0..100 {
        let dictionary = document.get_dictionary(current).ok()?;
        if let Ok(value) = dictionary.get(key) {
            return Some(value.clone());
        }
        current = dictionary.get(b"Parent").ok()?.as_reference().ok()?;
    }
    None
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
    if let Some(old_contents) = old {
        let prefix_id = document.add_object(Stream::new(dictionary! {}, b"q\n".to_vec()));
        let suffix_id = document.add_object(Stream::new(dictionary! {}, b"Q\n".to_vec()));
        contents.push(Object::Reference(prefix_id));
        append_content_objects(document, &mut contents, Some(old_contents));
        contents.push(Object::Reference(suffix_id));
    }
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

fn detect_image_dpi(path: &std::path::Path) -> Option<f64> {
    let bytes = std::fs::read(path).ok()?;
    detect_image_dpi_from_bytes(&bytes)
}

fn detect_image_dpi_from_bytes(bytes: &[u8]) -> Option<f64> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        detect_png_dpi(bytes)
    } else if bytes.starts_with(b"\xFF\xD8") {
        detect_jpeg_dpi(bytes)
    } else {
        None
    }
}

fn detect_png_dpi(bytes: &[u8]) -> Option<f64> {
    let mut offset = 8;
    while offset + 8 <= bytes.len() {
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().ok()?) as usize;
        let chunk_type = &bytes[offset + 4..offset + 8];
        if chunk_type == b"pHYs" && offset + 8 + length <= bytes.len() && length >= 9 {
            let chunk_data = &bytes[offset + 8..offset + 8 + length];
            let ppu_x = u32::from_be_bytes(chunk_data[0..4].try_into().ok()?);
            let unit = chunk_data[8];
            if unit == 1 && ppu_x > 0 {
                let dpi = (ppu_x as f64) * 0.0254;
                return Some(dpi.round());
            }
        } else if chunk_type == b"IDAT" || chunk_type == b"IEND" {
            break;
        }
        offset += 12 + length;
    }
    None
}

fn detect_jpeg_dpi(bytes: &[u8]) -> Option<f64> {
    let mut offset = 2;
    while offset + 4 <= bytes.len() {
        if bytes[offset] != 0xFF {
            break;
        }
        let marker = bytes[offset + 1];
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        let length = u16::from_be_bytes(bytes[offset + 2..offset + 4].try_into().ok()?) as usize;
        if length < 2 || offset + 2 + length > bytes.len() {
            break;
        }
        let segment_data = &bytes[offset + 4..offset + 2 + length];

        if marker == 0xE0 && segment_data.starts_with(b"JFIF\0") && segment_data.len() >= 9 {
            let units = segment_data[7];
            let x_density = u16::from_be_bytes(segment_data[8..10].try_into().ok()?) as f64;
            if x_density > 0.0 {
                if units == 1 {
                    return Some(x_density);
                } else if units == 2 {
                    return Some((x_density * 2.54).round());
                }
            }
        }

        if marker == 0xE1 && segment_data.starts_with(b"Exif\0\0") && segment_data.len() >= 14 {
            let exif = &segment_data[6..];
            if let Some(dpi) = parse_exif_dpi(exif) {
                return Some(dpi);
            }
        }

        offset += 2 + length;
    }
    None
}

fn parse_exif_dpi(exif: &[u8]) -> Option<f64> {
    if exif.len() < 8 {
        return None;
    }
    let is_le = match &exif[0..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let read_u16 = |buf: &[u8], pos: usize| -> Option<u16> {
        let b = buf.get(pos..pos + 2)?;
        Some(if is_le {
            u16::from_le_bytes(b.try_into().ok()?)
        } else {
            u16::from_be_bytes(b.try_into().ok()?)
        })
    };
    let read_u32 = |buf: &[u8], pos: usize| -> Option<u32> {
        let b = buf.get(pos..pos + 4)?;
        Some(if is_le {
            u32::from_le_bytes(b.try_into().ok()?)
        } else {
            u32::from_be_bytes(b.try_into().ok()?)
        })
    };

    let ifd0_offset = read_u32(exif, 4)? as usize;
    if ifd0_offset + 2 > exif.len() {
        return None;
    }
    let num_entries = read_u16(exif, ifd0_offset)? as usize;
    let mut x_res: Option<f64> = None;
    let mut unit: u16 = 2;

    for i in 0..num_entries {
        let entry_offset = ifd0_offset + 2 + i * 12;
        if entry_offset + 12 > exif.len() {
            break;
        }
        let tag = read_u16(exif, entry_offset)?;
        let val_offset = read_u32(exif, entry_offset + 8)? as usize;

        match tag {
            0x011A => {
                if val_offset + 8 <= exif.len() {
                    let num = read_u32(exif, val_offset)? as f64;
                    let den = read_u32(exif, val_offset + 4)? as f64;
                    if den > 0.0 {
                        x_res = Some(num / den);
                    }
                }
            }
            0x0128 => {
                let u = read_u16(exif, entry_offset + 8)?;
                unit = u;
            }
            _ => {}
        }
    }

    let res = x_res?;
    if res <= 0.0 {
        return None;
    }
    if unit == 2 {
        Some(res.round())
    } else if unit == 3 {
        Some((res * 2.54).round())
    } else {
        Some(res.round())
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

fn install_extgstate_resource(
    document: &mut Document,
    page_id: ObjectId,
    name: &[u8],
    extgstate_id: ObjectId,
) -> Result<()> {
    let mut resources = inherited_value(document, page_id, b"Resources")
        .and_then(|value| resolve_dictionary(document, &value))
        .unwrap_or_default();
    let mut extgstates = resources
        .get(b"ExtGState")
        .ok()
        .and_then(|value| resolve_dictionary(document, value))
        .unwrap_or_default();
    extgstates.set(name, extgstate_id);
    resources.set("ExtGState", extgstates);
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

pub fn create_bookmarks(document: &mut Document, entries: &[(String, u32)]) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let pages = document.get_pages();
    if pages.is_empty() {
        return Ok(());
    }

    let valid_entries: Vec<(&str, ObjectId)> = entries
        .iter()
        .filter_map(|(title, page_num)| {
            pages
                .get(page_num)
                .map(|&page_id| (title.as_str(), page_id))
        })
        .collect();

    if valid_entries.is_empty() {
        return Ok(());
    }

    let outline_root_id = document.new_object_id();
    let item_ids: Vec<ObjectId> = (0..valid_entries.len())
        .map(|_| document.new_object_id())
        .collect();

    for (i, (&(title, page_id), &item_id)) in valid_entries.iter().zip(&item_ids).enumerate() {
        let mut dict = dictionary! {
            "Title" => info_string(title),
            "Parent" => outline_root_id,
            "Dest" => vec![
                Object::Reference(page_id),
                Object::Name(b"Fit".to_vec()),
            ],
        };
        if i > 0 {
            dict.set("Prev", item_ids[i - 1]);
        }
        if i + 1 < item_ids.len() {
            dict.set("Next", item_ids[i + 1]);
        }
        document.objects.insert(item_id, Object::Dictionary(dict));
    }

    let root_dict = dictionary! {
        "Type" => "Outlines",
        "First" => item_ids[0],
        "Last" => item_ids[item_ids.len() - 1],
        "Count" => item_ids.len() as i64,
    };
    document
        .objects
        .insert(outline_root_id, Object::Dictionary(root_dict));

    let catalog_id = if let Ok(root) = document.trailer.get(b"Root") {
        root.as_reference()?
    } else {
        let id = document.new_object_id();
        document
            .objects
            .insert(id, Object::Dictionary(dictionary! { "Type" => "Catalog" }));
        document.trailer.set("Root", id);
        id
    };

    let catalog = document
        .get_object_mut(catalog_id)
        .context("catalog object not found")?
        .as_dict_mut()
        .context("catalog is not a dictionary")?;
    catalog.set("Outlines", outline_root_id);

    Ok(())
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
    fn orient_changes_page_orientation() {
        let mut document = one_page(); // default is landscape 842 x 595
        let page_id = *document.get_pages().get(&1).unwrap();
        let before = page_geometry(&document, page_id).unwrap();
        assert!(before.display_width() > before.display_height());

        // Target portrait -> should rotate to portrait
        orient_pages(&mut document, "1", "portrait").unwrap();
        let after = page_geometry(&document, page_id).unwrap();
        assert!(after.display_width() < after.display_height());

        // Target portrait again -> should remain portrait without double rotation
        orient_pages(&mut document, "1", "portrait").unwrap();
        let after2 = page_geometry(&document, page_id).unwrap();
        assert_eq!(after.rotation, after2.rotation);

        // Target landscape -> should rotate back to landscape
        orient_pages(&mut document, "1", "landscape").unwrap();
        let after3 = page_geometry(&document, page_id).unwrap();
        assert!(after3.display_width() > after3.display_height());
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
                scale: Some(1.0),
                dpi: Some(96.0),
                opacity: 0.5,
                pages: "all".to_owned(),
                mode: StampMode::Over,
                blend_mode: BlendMode::Normal,
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

    #[test]
    fn create_bookmarks_builds_outlines_tree() {
        let mut document = one_page();
        duplicate_first_page(&mut document);
        let entries = vec![
            ("First Section".to_owned(), 1),
            ("Вторая секция".to_owned(), 2),
        ];
        create_bookmarks(&mut document, &entries).unwrap();

        let root_id = document
            .trailer
            .get(b"Root")
            .unwrap()
            .as_reference()
            .unwrap();
        let catalog = document.get_dictionary(root_id).unwrap();
        let outlines_id = catalog.get(b"Outlines").unwrap().as_reference().unwrap();
        let outlines = document.get_dictionary(outlines_id).unwrap();

        assert_eq!(outlines.get(b"Count").unwrap().as_i64().unwrap(), 2);
        let first_id = outlines.get(b"First").unwrap().as_reference().unwrap();
        let last_id = outlines.get(b"Last").unwrap().as_reference().unwrap();

        let first_item = document.get_dictionary(first_id).unwrap();
        assert_eq!(
            first_item.get(b"Title").unwrap().as_str().unwrap(),
            b"First Section"
        );
        assert_eq!(
            first_item.get(b"Next").unwrap().as_reference().unwrap(),
            last_id
        );

        let last_item = document.get_dictionary(last_id).unwrap();
        assert_eq!(
            last_item.get(b"Prev").unwrap().as_reference().unwrap(),
            first_id
        );
        let title_bytes = last_item.get(b"Title").unwrap().as_str().unwrap();
        assert!(title_bytes.starts_with(&[0xfe, 0xff]));
    }

    #[test]
    fn test_detect_png_phys_dpi() {
        // Construct minimal valid PNG with pHYs chunk at 300 DPI (11811 pixels/meter)
        let mut png = Vec::new();
        png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        // IHDR chunk
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
        png.extend_from_slice(&[0, 0, 0, 0]); // CRC placeholder
        // pHYs chunk: 11811 (0x2E23) ppm ≈ 300 dpi, unit 1 (meter)
        png.extend_from_slice(&9u32.to_be_bytes());
        png.extend_from_slice(b"pHYs");
        png.extend_from_slice(&11811u32.to_be_bytes());
        png.extend_from_slice(&11811u32.to_be_bytes());
        png.push(1); // unit = meter
        png.extend_from_slice(&[0, 0, 0, 0]); // CRC
        // IEND chunk
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(b"IEND");
        png.extend_from_slice(&[0, 0, 0, 0]);

        let dpi = detect_image_dpi_from_bytes(&png);
        assert_eq!(dpi, Some(300.0));
    }

    #[test]
    fn test_detect_jpeg_jfif_dpi() {
        // Construct minimal JPEG with JFIF APP0 marker (300 DPI, unit = 1 inch)
        let mut jpeg = Vec::new();
        jpeg.extend_from_slice(b"\xFF\xD8"); // SOI
        jpeg.extend_from_slice(b"\xFF\xE0"); // APP0
        jpeg.extend_from_slice(&16u16.to_be_bytes()); // length
        jpeg.extend_from_slice(b"JFIF\0");
        jpeg.extend_from_slice(&[1, 2]); // version 1.2
        jpeg.push(1); // units: 1 = dots per inch
        jpeg.extend_from_slice(&300u16.to_be_bytes()); // X density
        jpeg.extend_from_slice(&300u16.to_be_bytes()); // Y density
        jpeg.extend_from_slice(&[0, 0]); // thumbnail
        jpeg.extend_from_slice(b"\xFF\xD9"); // EOI

        let dpi = detect_image_dpi_from_bytes(&jpeg);
        assert_eq!(dpi, Some(300.0));
    }

    #[test]
    fn test_detect_jpeg_jfif_dpcm() {
        // Construct minimal JPEG with JFIF APP0 marker (118 DPCM ≈ 300 DPI, unit = 2 cm)
        let mut jpeg = Vec::new();
        jpeg.extend_from_slice(b"\xFF\xD8"); // SOI
        jpeg.extend_from_slice(b"\xFF\xE0"); // APP0
        jpeg.extend_from_slice(&16u16.to_be_bytes()); // length
        jpeg.extend_from_slice(b"JFIF\0");
        jpeg.extend_from_slice(&[1, 2]);
        jpeg.push(2); // units: 2 = dots per cm
        jpeg.extend_from_slice(&118u16.to_be_bytes()); // 118 * 2.54 = 299.72 ≈ 300
        jpeg.extend_from_slice(&118u16.to_be_bytes());
        jpeg.extend_from_slice(&[0, 0]);
        jpeg.extend_from_slice(b"\xFF\xD9");

        let dpi = detect_image_dpi_from_bytes(&jpeg);
        assert_eq!(dpi, Some(300.0));
    }

    #[test]
    fn test_detect_jpeg_exif_dpi() {
        // Construct minimal JPEG with Exif APP1 marker (600 DPI, unit = 2 inch)
        let mut jpeg = Vec::new();
        jpeg.extend_from_slice(b"\xFF\xD8"); // SOI
        jpeg.extend_from_slice(b"\xFF\xE1"); // APP1

        let mut exif_payload = Vec::new();
        exif_payload.extend_from_slice(b"Exif\0\0");
        let tiff_start = exif_payload.len();
        exif_payload.extend_from_slice(b"II\x2A\x00"); // Little-endian TIFF header
        exif_payload.extend_from_slice(&8u32.to_le_bytes()); // Offset to IFD0 = 8

        // IFD0: 2 entries
        exif_payload.extend_from_slice(&2u16.to_le_bytes());

        // Entry 1: 0x011A (XResolution), type 5 (RATIONAL), count 1, offset 38 (from TIFF start)
        exif_payload.extend_from_slice(&0x011Au16.to_le_bytes());
        exif_payload.extend_from_slice(&5u16.to_le_bytes());
        exif_payload.extend_from_slice(&1u32.to_le_bytes());
        exif_payload.extend_from_slice(&38u32.to_le_bytes());

        // Entry 2: 0x0128 (ResolutionUnit), type 3 (SHORT), count 1, value 2 (inches)
        exif_payload.extend_from_slice(&0x0128u16.to_le_bytes());
        exif_payload.extend_from_slice(&3u16.to_le_bytes());
        exif_payload.extend_from_slice(&1u32.to_le_bytes());
        exif_payload.extend_from_slice(&2u16.to_le_bytes());
        exif_payload.extend_from_slice(&[0, 0]); // padding to 4 bytes

        // Next IFD offset = 0
        exif_payload.extend_from_slice(&0u32.to_le_bytes());

        // Offset 38 from TIFF start: Rational value (600 / 1)
        assert_eq!(exif_payload.len() - tiff_start, 38);
        exif_payload.extend_from_slice(&600u32.to_le_bytes()); // numerator
        exif_payload.extend_from_slice(&1u32.to_le_bytes()); // denominator

        let app1_len = (exif_payload.len() + 2) as u16;
        jpeg.extend_from_slice(&app1_len.to_be_bytes());
        jpeg.extend_from_slice(&exif_payload);
        jpeg.extend_from_slice(b"\xFF\xD9"); // EOI

        let dpi = detect_image_dpi_from_bytes(&jpeg);
        assert_eq!(dpi, Some(600.0));
    }

    #[test]
    fn test_uncalibrated_image_returns_none() {
        let dummy_png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0DIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00IEND\x00\x00\x00\x00";
        assert_eq!(detect_image_dpi_from_bytes(dummy_png), None);

        let dummy_jpeg = b"\xFF\xD8\xFF\xD9";
        assert_eq!(detect_image_dpi_from_bytes(dummy_jpeg), None);
    }

    #[test]
    fn stamp_isolates_page_graphics_state() {
        let temporary = tempfile::Builder::new().suffix(".png").tempfile().unwrap();
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(8, 4, Rgba([255, 0, 0, 128])))
            .save_with_format(temporary.path(), ImageFormat::Png)
            .unwrap();
        let mut document = one_page();
        let page_id = *document.get_pages().get(&1).unwrap();

        // Simulate a page with unclosed CTM (e.g. inverted Y coordinate transform)
        let unclosed_stream_id = document.add_object(Stream::new(
            dictionary! {},
            b"0.75 0 0 -0.75 0 595.32 cm\nq\n0 0 100 100 re f\nQ\n".to_vec(),
        ));
        document
            .get_object_mut(page_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Contents", vec![Object::Reference(unclosed_stream_id)]);

        apply_stamp(
            &mut document,
            &StampOptions {
                path: temporary.path().to_path_buf(),
                position: "br".to_owned(),
                scale: Some(1.0),
                dpi: Some(96.0),
                opacity: 0.5,
                pages: "all".to_owned(),
                mode: StampMode::Over,
                blend_mode: BlendMode::Normal,
            },
        )
        .unwrap();

        let contents = document.get_page_contents(page_id);
        // Expect: [q_prefix, original_stream, Q_suffix, stamp_stream]
        assert_eq!(contents.len(), 4);

        let q_stream = document
            .get_object(contents[0])
            .unwrap()
            .as_stream()
            .unwrap();
        assert_eq!(q_stream.content, b"q\n");

        let orig_stream = document
            .get_object(contents[1])
            .unwrap()
            .as_stream()
            .unwrap();
        assert_eq!(
            orig_stream.content,
            b"0.75 0 0 -0.75 0 595.32 cm\nq\n0 0 100 100 re f\nQ\n"
        );

        let q_close_stream = document
            .get_object(contents[2])
            .unwrap()
            .as_stream()
            .unwrap();
        assert_eq!(q_close_stream.content, b"Q\n");
    }
}
