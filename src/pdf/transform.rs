use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use flate2::Compression;
use flate2::write::ZlibEncoder;
use image::GenericImageView;
use image::imageops::FilterType;
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, StringFormat, dictionary};

use super::{image as pdf_image, object_number, parse_page_selection, resolve};
use crate::imageconv::{self, Orientation};

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
pub enum BlendMode {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    #[value(alias = "colordodge")]
    ColorDodge,
    #[value(alias = "colorburn")]
    ColorBurn,
    #[value(alias = "hardlight")]
    HardLight,
    #[value(alias = "softlight")]
    SoftLight,
    Difference,
    Exclusion,
}

impl BlendMode {
    const ALL: [(Self, &'static str); 12] = [
        (Self::Normal, "Normal"),
        (Self::Multiply, "Multiply"),
        (Self::Screen, "Screen"),
        (Self::Overlay, "Overlay"),
        (Self::Darken, "Darken"),
        (Self::Lighten, "Lighten"),
        (Self::ColorDodge, "ColorDodge"),
        (Self::ColorBurn, "ColorBurn"),
        (Self::HardLight, "HardLight"),
        (Self::SoftLight, "SoftLight"),
        (Self::Difference, "Difference"),
        (Self::Exclusion, "Exclusion"),
    ];

    pub fn to_pdf_name(self) -> &'static [u8] {
        Self::ALL
            .iter()
            .find(|(mode, _)| *mode == self)
            .map_or(b"Normal", |(_, name)| name.as_bytes())
    }

    /// CSS `mix-blend-mode` value, for the stamp picker preview.
    pub fn css_name(self) -> String {
        let name = String::from_utf8_lossy(self.to_pdf_name());
        let mut css = String::with_capacity(name.len() + 1);
        for (index, character) in name.char_indices() {
            if index > 0 && character.is_ascii_uppercase() {
                css.push('-');
            }
            css.push(character.to_ascii_lowercase());
        }
        css
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
    pub blend_mode: BlendMode,
    /// Explicit placements; when non-empty they replace position, scale and
    /// pages.
    pub placements: Vec<StampPlacement>,
}

/// One stamp copy: the `cm` matrix mapping the unit image square into the
/// user space of page `page` (1-based).
#[derive(Clone, Copy, Debug)]
pub struct StampPlacement {
    pub page: u32,
    pub matrix: [f64; 6],
}

/// Natural stamp size in points, from the given or embedded DPI.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StampSize {
    pub width: f64,
    pub height: f64,
    calibrated: bool,
}

impl StampSize {
    pub(crate) fn new(
        bytes: &[u8],
        (pixel_width, pixel_height): (u32, u32),
        dpi: Option<f64>,
    ) -> Self {
        let detected = crate::metadata::image_dpi(bytes);
        let effective = dpi.or(detected).unwrap_or(96.0);
        Self {
            width: f64::from(pixel_width) * 72.0 / effective,
            height: f64::from(pixel_height) * 72.0 / effective,
            calibrated: dpi.is_some() || detected.is_some(),
        }
    }

    /// Size on a page. Explicit scale wins; 0 or an uncalibrated image
    /// auto-fits up to a quarter of the page.
    pub(crate) fn fitted(self, scale: Option<f64>, page: PageGeometry, paper: f64) -> (f64, f64) {
        let scale = match scale {
            Some(scale) if scale > 0.0 => scale * paper,
            None if self.calibrated => paper,
            _ => paper
                .min(page.raw_width() * 0.25 / self.width)
                .min(page.raw_height() * 0.25 / self.height),
        };
        (self.width * scale, self.height * scale)
    }
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

    fn quarter_turn(self) -> bool {
        self.rotation.rem_euclid(180) == 90
    }

    pub fn display_width(self) -> f64 {
        if self.quarter_turn() {
            self.raw_height()
        } else {
            self.raw_width()
        }
    }

    pub fn display_height(self) -> f64 {
        if self.quarter_turn() {
            self.raw_width()
        } else {
            self.raw_height()
        }
    }

    pub fn is_landscape(self) -> bool {
        self.display_width() > self.display_height()
    }
}

/// Pages matching a selection expression, with their numbers and geometry.
pub(crate) fn selected_pages(
    document: &Document,
    expression: &str,
) -> Result<Vec<(u32, ObjectId, PageGeometry)>> {
    let pages = document.get_pages();
    let selected = parse_page_selection(expression, pages.len())?;
    pages
        .into_iter()
        .filter(|(number, _)| selected.contains(&(*number as usize)))
        .map(|(number, page_id)| Ok((number, page_id, page_geometry(document, page_id)?)))
        .collect()
}

pub fn auto_rotate(document: &mut Document) -> Result<Vec<u32>> {
    let pages = selected_pages(document, "all")?;
    let target_landscape = dominant_landscape(pages.iter().map(|(_, _, geometry)| *geometry));
    let mut rotated = Vec::new();
    for (number, page_id, geometry) in pages {
        if geometry.is_landscape() != target_landscape {
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
    for (_, page_id, geometry) in selected_pages(document, pages)? {
        set_page_rotation(document, page_id, geometry.rotation + degrees)?;
    }
    Ok(())
}

pub fn orient_pages(document: &mut Document, pages: &str, orient: Orientation) -> Result<()> {
    for (_, page_id, geometry) in selected_pages(document, pages)? {
        if geometry.is_landscape() != orient.is_landscape() {
            set_page_rotation(document, page_id, geometry.rotation + 90)?;
        }
    }
    Ok(())
}

/// Fit the visible area (CropBox) of every selected page onto the paper
/// (portrait width and height in points), keeping each page's orientation;
/// pages already of that size stay as is. Annotations and every destination
/// that points into a resized page move with its content.
pub fn resize_pages(
    document: &mut Document,
    (portrait_width, portrait_height): (f64, f64),
    pages: &str,
) -> Result<()> {
    let pages = selected_pages(document, pages)?;
    let suffix_id = document.add_object(Stream::new(dictionary! {}, b"Q\n".to_vec()));
    let mut moves = HashMap::new();

    for (_, page_id, _) in pages {
        let geometry = page_crop_geometry(document, page_id)?;
        let (display_width, display_height) = if geometry.is_landscape() {
            (portrait_height, portrait_width)
        } else {
            (portrait_width, portrait_height)
        };
        let (target_width, target_height) = if geometry.quarter_turn() {
            (display_height, display_width)
        } else {
            (display_width, display_height)
        };

        let (left, bottom) = (
            geometry.left.min(geometry.right),
            geometry.bottom.min(geometry.top),
        );
        let fits = |actual: f64, target: f64| (actual - target).abs() < 0.5;
        let media = page_geometry(document, page_id)?;
        if left == 0.0
            && bottom == 0.0
            && fits(geometry.raw_width(), target_width)
            && fits(geometry.raw_height(), target_height)
            && fits(media.raw_width(), target_width)
            && fits(media.raw_height(), target_height)
        {
            continue;
        }
        let scale =
            (target_width / geometry.raw_width()).min(target_height / geometry.raw_height());
        let translate_x = (target_width - geometry.raw_width() * scale) / 2.0 - left * scale;
        let translate_y = (target_height - geometry.raw_height() * scale) / 2.0 - bottom * scale;
        let prefix = format!("q\n{scale:.8} 0 0 {scale:.8} {translate_x:.8} {translate_y:.8} cm\n");
        let prefix_id = document.add_object(Stream::new(dictionary! {}, prefix.into_bytes()));
        rewrite_contents(document, page_id, |old| {
            [
                vec![Object::Reference(prefix_id)],
                old,
                vec![Object::Reference(suffix_id)],
            ]
            .concat()
        })?;
        let movement = Movement {
            scale,
            translate_x,
            translate_y,
        };
        transform_annotation_geometry(document, page_id, movement);
        moves.insert(page_id, movement);

        let page = document.get_object_mut(page_id)?.as_dict_mut()?;
        let target_box: Vec<Object> = vec![
            0.into(),
            0.into(),
            target_width.into(),
            target_height.into(),
        ];
        page.set("MediaBox", target_box.clone());
        page.set("CropBox", target_box.clone());
        for key in [b"BleedBox".as_slice(), b"TrimBox", b"ArtBox"] {
            if page.has(key) {
                page.set(key, target_box.clone());
            }
        }
    }
    if !moves.is_empty() {
        for object in document.objects.values_mut() {
            transform_destinations(object, &moves);
        }
    }
    Ok(())
}

/// Uniform scale and translation applied to a resized page's content.
#[derive(Clone, Copy, Debug)]
struct Movement {
    scale: f64,
    translate_x: f64,
    translate_y: f64,
}

impl Movement {
    fn x(self, value: f64) -> f64 {
        value * self.scale + self.translate_x
    }

    fn y(self, value: f64) -> f64 {
        value * self.scale + self.translate_y
    }

    /// Move a number of a flat `x y x y ...` array.
    fn coordinate(self, value: &mut Object, is_x: bool) {
        if let Ok(number) = object_number(value) {
            *value = if is_x { self.x(number) } else { self.y(number) }.into();
        }
    }
}

/// Explicit destinations `[page /XYZ left top zoom]`, `[page /FitR l b r t]`
/// and the like, anywhere in `object`, that point into a moved page.
fn transform_destinations(object: &mut Object, moves: &HashMap<ObjectId, Movement>) {
    match object {
        Object::Array(items) => {
            if let [Object::Reference(page), Object::Name(kind), rest @ ..] = items.as_mut_slice()
                && let Some(&movement) = moves.get(&*page)
            {
                // Which coordinates follow the destination type.
                let axes: &[bool] = match kind.as_slice() {
                    b"XYZ" => &[true, false],
                    b"FitH" | b"FitBH" => &[false],
                    b"FitV" | b"FitBV" => &[true],
                    b"FitR" => &[true, false, true, false],
                    _ => &[],
                };
                for (value, is_x) in rest.iter_mut().zip(axes) {
                    movement.coordinate(value, *is_x);
                }
                return;
            }
            items
                .iter_mut()
                .for_each(|item| transform_destinations(item, moves));
        }
        Object::Dictionary(dictionary) => dictionary
            .iter_mut()
            .for_each(|(_, value)| transform_destinations(value, moves)),
        Object::Stream(stream) => stream
            .dict
            .iter_mut()
            .for_each(|(_, value)| transform_destinations(value, moves)),
        _ => {}
    }
}

fn dominant_landscape(geometries: impl IntoIterator<Item = PageGeometry>) -> bool {
    let mut first = None;
    let mut balance = 0i64;
    for geometry in geometries {
        let landscape = geometry.is_landscape();
        first.get_or_insert(landscape);
        balance += if landscape { 1 } else { -1 };
    }
    match balance {
        0 => first.unwrap_or(false),
        balance => balance > 0,
    }
}

pub fn apply_stamp(document: &mut Document, options: &StampOptions) -> Result<()> {
    if !(0.0..=1.0).contains(&options.opacity) {
        bail!("stamp opacity must be between 0 and 1");
    }
    if options.scale.is_some_and(|scale| scale < 0.0) {
        bail!("stamp scale cannot be negative");
    }
    if options.dpi.is_some_and(|dpi| dpi <= 0.0) {
        bail!("stamp dpi must be positive");
    }

    let bytes = std::fs::read(&options.path)
        .with_context(|| format!("failed to read stamp {}", options.path.display()))?;
    let image = image::load_from_memory(&bytes)
        .with_context(|| format!("failed to decode stamp {}", options.path.display()))?
        .to_rgba8();
    let (pixel_width, pixel_height) = image.dimensions();
    let size = StampSize::new(&bytes, (pixel_width, pixel_height), options.dpi);

    // Placement is resolved before the stamp joins the pages' images, which
    // `scan_paper_factor` inspects.
    let targets = if options.placements.is_empty() {
        let mut targets = Vec::new();
        for (_, page_id, geometry) in selected_pages(document, &options.pages)? {
            let paper = scan_paper_factor(document, page_id, geometry);
            // Position and size refer to the page as shown: its visible
            // area, upright after /Rotate.
            let crop = page_crop_geometry(document, page_id)?;
            let display = crop.displayed();
            let (width, height) = size.fitted(options.scale, display, paper);
            let (left, bottom) = stamp_position(&options.position, display, width, height, paper)?;
            targets.push((
                page_id,
                upright_stamp_matrix(crop, left, bottom, width, height),
            ));
        }
        targets
    } else {
        let pages = document.get_pages();
        options
            .placements
            .iter()
            .map(|placement| {
                pages
                    .get(&placement.page)
                    .map(|page_id| (*page_id, placement.matrix))
                    .with_context(|| format!("stamp page {} does not exist", placement.page))
            })
            .collect::<Result<Vec<_>>>()?
    };
    let pixel_count = image.len() / 4;
    let mut rgb = Vec::with_capacity(pixel_count * 3);
    let mut alpha = Vec::with_capacity(pixel_count);
    for pixel in image.pixels() {
        rgb.extend_from_slice(&pixel.0[..3]);
        alpha.push((f64::from(pixel.0[3]) * options.opacity).round() as u8);
    }
    let has_transparency = options.opacity < 1.0 || alpha.iter().any(|value| *value != 255);

    let image_stream = |color_space: &str, data: Vec<u8>| {
        Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => i64::from(pixel_width),
                "Height" => i64::from(pixel_height),
                "ColorSpace" => color_space,
                "BitsPerComponent" => 8,
            },
            data,
        )
    };
    let mut stamp = image_stream("DeviceRGB", rgb);
    if has_transparency {
        let alpha_id = document.add_object(image_stream("DeviceGray", alpha));
        stamp.dict.set("SMask", alpha_id);
    }
    let image_id = document.add_object(stamp);
    let gstate_id = (options.blend_mode != BlendMode::Normal).then(|| {
        document.add_object(dictionary! {
            "Type" => "ExtGState",
            "BM" => Object::Name(options.blend_mode.to_pdf_name().to_vec()),
        })
    });
    let isolation = ContentIsolation::new(document);
    let page_ids = targets.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    let stamp_name = free_resource_name(document, &page_ids, b"XObject", "BpdfStamp");
    let state_name = free_resource_name(document, &page_ids, b"ExtGState", "BpdfExtGState");

    let mut resources = vec![(b"XObject".as_slice(), stamp_name.as_bytes(), image_id)];
    if let Some(gstate_id) = gstate_id {
        resources.push((b"ExtGState", state_name.as_bytes(), gstate_id));
    }
    let mut installer = ResourceInstaller::new(resources);
    for (page_id, [a, b, c, d, e, f]) in targets {
        installer.install(document, page_id)?;

        let gstate = if gstate_id.is_some() {
            format!("/{state_name} gs\n")
        } else {
            String::new()
        };
        let content = format!(
            "q\n{gstate}{a:.6} {b:.6} {c:.6} {d:.6} {e:.6} {f:.6} cm\n/{stamp_name} Do\nQ\n"
        );
        let content_id = document.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        isolation.add_overlay(document, page_id, content_id)?;
    }
    Ok(())
}

/// Shared `q`/`Q` streams that fence the original page content, so overlays
/// drawn before or after it start from the default graphics state.
pub(crate) struct ContentIsolation {
    push: ObjectId,
    pop: ObjectId,
}

impl ContentIsolation {
    pub(crate) fn new(document: &mut Document) -> Self {
        Self {
            push: document.add_object(Stream::new(dictionary! {}, b"q\n".to_vec())),
            pop: document.add_object(Stream::new(dictionary! {}, b"Q\n".to_vec())),
        }
    }

    /// Draw `content_id` above the page content.
    pub(crate) fn add_overlay(
        &self,
        document: &mut Document,
        page_id: ObjectId,
        content_id: ObjectId,
    ) -> Result<()> {
        rewrite_contents(document, page_id, |old| {
            let mut contents = Vec::with_capacity(old.len() + 3);
            if !old.is_empty() {
                contents.push(Object::Reference(self.push));
                contents.extend(old);
                contents.push(Object::Reference(self.pop));
            }
            contents.push(Object::Reference(content_id));
            contents
        })
    }
}

impl PageGeometry {
    /// The page as it is displayed: rotation applied, origin at bottom-left.
    pub(crate) fn displayed(self) -> Self {
        Self {
            left: 0.0,
            bottom: 0.0,
            right: self.display_width(),
            top: self.display_height(),
            rotation: 0,
        }
    }

    /// Map a point of the displayed page to page (user space) coordinates.
    pub(crate) fn display_to_page(self, x: f64, y: f64) -> (f64, f64) {
        let (width, height) = (self.raw_width(), self.raw_height());
        let (raw_x, raw_y) = match self.rotation.rem_euclid(360) {
            90 => (width - y, x),
            180 => (width - x, height - y),
            270 => (y, height - x),
            _ => (x, y),
        };
        (
            self.left.min(self.right) + raw_x,
            self.bottom.min(self.top) + raw_y,
        )
    }

    /// Map a point in page (user space) coordinates to the displayed page;
    /// the inverse of [`Self::display_to_page`].
    pub(crate) fn page_to_display(self, x: f64, y: f64) -> (f64, f64) {
        let (width, height) = (self.raw_width(), self.raw_height());
        let (raw_x, raw_y) = (x - self.left.min(self.right), y - self.bottom.min(self.top));
        match self.rotation.rem_euclid(360) {
            90 => (raw_y, width - raw_x),
            180 => (width - raw_x, height - raw_y),
            270 => (height - raw_y, raw_x),
            _ => (raw_x, raw_y),
        }
    }
}

/// How `optimize` treats images.
#[derive(Clone, Copy, Debug)]
pub struct OptimizeOptions {
    /// Resolution limit where an image is drawn; 0 keeps every size.
    pub image_dpi: u32,
    pub jpeg_quality: u8,
    /// Re-encode images already within the limit at `jpeg_quality` too.
    pub recompress_all: bool,
    /// Convert colour images to grayscale.
    pub gray: bool,
}

/// Downsample images above the resolution limit and tidy the object graph.
/// A re-encoded image replaces the original only when it is smaller, unless
/// it was turned gray.
pub fn optimize(document: &mut Document, options: OptimizeOptions) -> OptimizeReport {
    let mut report = downsample_images(document, options);
    document.delete_zero_length_streams();
    document.prune_objects();
    document.renumber_objects();
    document.compress();
    report.warnings.shrink_to_fit();
    report
}

fn downsample_images(document: &mut Document, options: OptimizeOptions) -> OptimizeReport {
    let mut report = OptimizeReport::default();
    if options.image_dpi == 0 && !options.recompress_all && !options.gray {
        return report;
    }

    let candidates = collect_image_resizes(
        document,
        options.image_dpi,
        options.recompress_all || options.gray,
        &mut report.warnings,
    );
    // Decoding, resampling and encoding are independent per image.
    let encoded = crate::parallel::map(&candidates, crate::parallel::cpu_jobs(), |candidate| {
        encode_downsampled(document, *candidate, options)
    });

    for (candidate, result) in candidates.iter().zip(encoded) {
        let replaced = result.and_then(|encoded| match encoded {
            Some(encoded) => replace_image(document, *candidate, encoded),
            None => Ok(false),
        });
        match replaced {
            Ok(true) => report.resized_images += 1,
            Ok(false) => {}
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
    recompress_all: bool,
    warnings: &mut Vec<String>,
) -> Vec<ImageResize> {
    let pages = document.get_pages().into_iter().collect::<Vec<_>>();
    let per_page = crate::parallel::map(&pages, crate::parallel::cpu_jobs(), |&(_, page_id)| {
        page_image_targets(document, page_id, image_dpi)
    });
    // An image shown on several pages keeps the largest size any of them needs.
    let mut targets = BTreeMap::<ObjectId, ((u32, u32), (u32, u32))>::new();
    for ((page_number, _), result) in pages.iter().zip(per_page) {
        match result {
            Ok(images) => {
                for (id, original, target) in images {
                    targets
                        .entry(id)
                        .and_modify(|(_, current)| {
                            if target.0 > current.0 {
                                *current = target;
                            }
                        })
                        .or_insert((original, target));
                }
            }
            Err(error) => warnings.push(format!("page {page_number}: {error:#}")),
        }
    }
    targets
        .into_iter()
        .filter(|(_, (original, target))| recompress_all || target != original)
        .map(|(id, (_, (target_width, target_height)))| ImageResize {
            id,
            target_width,
            target_height,
        })
        .collect()
}

/// An image with its original and target pixel size.
type ImageTarget = (ObjectId, (u32, u32), (u32, u32));

/// Original and target size of every image on a page. An image is sized for
/// `image_dpi` where it is drawn; one the content does not draw directly
/// (patterns, annotations, unreadable content) is sized for the whole page.
fn page_image_targets(
    document: &Document,
    page_id: ObjectId,
    image_dpi: u32,
) -> Result<Vec<ImageTarget>> {
    let geometry = page_geometry(document, page_id).context("cannot read page size")?;
    let user_unit = inherited_value(document, page_id, b"UserUnit")
        .and_then(|value| object_number(&value).ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(1.0);
    let page_width = geometry.display_width() * user_unit;
    let page_height = geometry.display_height() * user_unit;
    let placed = if image_dpi == 0 {
        HashMap::new()
    } else {
        pdf_image::placements(document, page_id).unwrap_or_default()
    };

    let mut images = document
        .get_page_images(page_id)
        .context("cannot inspect images")?
        .into_iter()
        .filter_map(|image| {
            let size = (
                u32::try_from(image.width).ok()?,
                u32::try_from(image.height).ok()?,
            );
            Some((image.id, size))
        })
        .collect::<BTreeMap<_, _>>();
    // Images drawn only from inside forms.
    for &id in placed.keys() {
        if let Ok(stream) = document.get_object(id).and_then(Object::as_stream)
            && let (Ok(width), Ok(height)) = (
                pdf_image::dimension(stream, b"Width"),
                pdf_image::dimension(stream, b"Height"),
            )
        {
            images.entry(id).or_insert((width, height));
        }
    }

    Ok(images
        .into_iter()
        .map(|(id, (width, height))| {
            let target = match placed.get(&id) {
                Some(&(placed_width, placed_height))
                    if placed_width >= 1.0 && placed_height >= 1.0 =>
                {
                    imageconv::fit_dimensions_for_placement(
                        width,
                        height,
                        (placed_width * user_unit, placed_height * user_unit),
                        image_dpi,
                    )
                }
                _ => imageconv::fit_dimensions_for_dpi(
                    width,
                    height,
                    page_width,
                    page_height,
                    image_dpi,
                ),
            };
            (id, (width, height), target.unwrap_or((width, height)))
        })
        .collect())
}

/// A re-encoded image: JPEG for photographs, Flate for graphics.
struct EncodedImage {
    content: Vec<u8>,
    filter: &'static str,
    gray: bool,
    /// Replace the original even when the new stream is larger.
    required: bool,
}

/// Decode, resample and re-encode one image, or `None` when there is
/// nothing to gain. Lossless sources with few colours (charts, drawings,
/// screenshots) stay lossless: JPEG would blur their edges.
fn encode_downsampled(
    document: &Document,
    resize: ImageResize,
    options: OptimizeOptions,
) -> Result<Option<EncodedImage>> {
    let original = document.get_object(resize.id)?.as_stream()?;
    reject_unsafe_image_features(original)?;
    let mut decoded = pdf_image::decode(document, original)?;
    let target = (resize.target_width, resize.target_height);
    let turned_gray = options.gray && decoded.color().channel_count() > 2;
    if turned_gray {
        decoded = image::DynamicImage::ImageLuma8(decoded.to_luma8());
    } else if options.gray && !options.recompress_all && target == decoded.dimensions() {
        return Ok(None);
    }
    let is_jpeg = original
        .filters()
        .ok()
        .and_then(|filters| filters.last().map(|filter| pdf_image::is_dct(filter)))
        .unwrap_or(false);
    if is_jpeg || !imageconv::has_few_colours(&decoded) {
        let jpeg = imageconv::resize_to_jpeg(&decoded, Some(target), options.jpeg_quality)?;
        let gray = super::jpeg_info(&jpeg)?.components == 1;
        return Ok(Some(EncodedImage {
            content: jpeg,
            filter: "DCTDecode",
            gray,
            required: turned_gray,
        }));
    }
    if target == decoded.dimensions() && !turned_gray {
        return Ok(None);
    }
    let resized = imageconv::resize(&decoded, target.0, target.1, FilterType::Lanczos3);
    let rgb = resized.to_rgb8();
    let gray = rgb
        .pixels()
        .all(|pixel| pixel.0[0] == pixel.0[1] && pixel.0[1] == pixel.0[2]);
    let samples = if gray {
        resized.to_luma8().into_raw()
    } else {
        rgb.into_raw()
    };
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(&samples)?;
    Ok(Some(EncodedImage {
        content: encoder.finish()?,
        filter: "FlateDecode",
        gray,
        required: turned_gray,
    }))
}

/// Returns false (and keeps the original) when the new stream would not be
/// smaller.
fn replace_image(
    document: &mut Document,
    resize: ImageResize,
    encoded: EncodedImage,
) -> Result<bool> {
    let stream = document.get_object_mut(resize.id)?.as_stream_mut()?;
    if !encoded.required && encoded.content.len() >= stream.content.len() {
        return Ok(false);
    }
    stream.dict.set("Width", i64::from(resize.target_width));
    stream.dict.set("Height", i64::from(resize.target_height));
    stream.dict.set(
        "ColorSpace",
        if encoded.gray {
            "DeviceGray"
        } else {
            "DeviceRGB"
        },
    );
    stream.dict.set("BitsPerComponent", 8);
    stream.dict.set("Filter", encoded.filter);
    stream.dict.remove(b"DecodeParms");
    stream.dict.remove(b"Decode");
    stream.set_content(encoded.content);
    Ok(true)
}

fn reject_unsafe_image_features(stream: &Stream) -> Result<()> {
    let dict = &stream.dict;
    if dict.has(b"SMask") || dict.has(b"Mask") {
        bail!("image masks are preserved without resampling");
    }
    if dict
        .get(b"ImageMask")
        .and_then(Object::as_bool)
        .unwrap_or(false)
    {
        bail!("stencil images are preserved without resampling");
    }
    if dict.has(b"Decode") {
        bail!("images with a custom Decode array are preserved");
    }
    if dict
        .get(b"BitsPerComponent")
        .and_then(Object::as_i64)
        .unwrap_or(8)
        != 8
    {
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

    // A direct Info dictionary moves into its own object with its fields.
    let info_id = match document.trailer.get(b"Info") {
        Ok(Object::Reference(id)) if document.get_dictionary(*id).is_ok() => *id,
        other => {
            let existing = match other {
                Ok(Object::Dictionary(dictionary)) => dictionary.clone(),
                _ => Dictionary::new(),
            };
            let id = document.add_object(existing);
            document.trailer.set("Info", id);
            id
        }
    };
    let info = document.get_object_mut(info_id)?.as_dict_mut()?;
    for (key, value) in fields {
        match value {
            Some("") => {
                info.remove(key.as_bytes());
            }
            Some(value) => info.set(key.as_bytes(), info_string(value)),
            None => {}
        }
    }
    Ok(())
}

pub(crate) fn info_string(value: &str) -> Object {
    if value.is_ascii() {
        return Object::string_literal(value);
    }
    let mut bytes = vec![0xfe, 0xff];
    bytes.extend(value.encode_utf16().flat_map(u16::to_be_bytes));
    Object::String(bytes, StringFormat::Hexadecimal)
}

pub fn page_geometry(document: &Document, page_id: ObjectId) -> Result<PageGeometry> {
    let media_box =
        inherited_value(document, page_id, b"MediaBox").context("page has no MediaBox")?;
    let [left, bottom, right, top] =
        parse_box(&media_box).context("MediaBox must be an array of four numbers")?;
    let rotation = inherited_value(document, page_id, b"Rotate")
        .and_then(|value| value.as_i64().ok())
        .unwrap_or(0)
        .rem_euclid(360);
    Ok(PageGeometry {
        left,
        bottom,
        right,
        top,
        rotation,
    })
}

/// A page box `[left bottom right top]`.
fn parse_box(object: &Object) -> Option<[f64; 4]> {
    let [left, bottom, right, top] = object.as_array().ok()?.as_slice() else {
        return None;
    };
    Some([
        object_number(left).ok()?,
        object_number(bottom).ok()?,
        object_number(right).ok()?,
        object_number(top).ok()?,
    ])
}

/// The `cm` matrix drawing the unit image square as the `width` x `height`
/// box at (`left`, `bottom`) of the displayed `crop`, upright whatever the
/// page rotation.
pub(crate) fn upright_stamp_matrix(
    crop: PageGeometry,
    left: f64,
    bottom: f64,
    width: f64,
    height: f64,
) -> [f64; 6] {
    let (origin_x, origin_y) = crop.display_to_page(left, bottom);
    let (right_x, right_y) = crop.display_to_page(left + width, bottom);
    let (top_x, top_y) = crop.display_to_page(left, bottom + height);
    [
        right_x - origin_x,
        right_y - origin_y,
        top_x - origin_x,
        top_y - origin_y,
        origin_x,
        origin_y,
    ]
}

/// The visible page area: the CropBox when present, otherwise the MediaBox.
pub(crate) fn page_crop_geometry(document: &Document, page_id: ObjectId) -> Result<PageGeometry> {
    let mut geometry = page_geometry(document, page_id)?;
    if let Some([left, bottom, right, top]) =
        inherited_value(document, page_id, b"CropBox").and_then(|crop| parse_box(&crop))
    {
        geometry = PageGeometry {
            left,
            bottom,
            right,
            top,
            ..geometry
        };
    }
    Ok(geometry)
}

pub(crate) fn inherited_value(
    document: &Document,
    page_id: ObjectId,
    key: &[u8],
) -> Option<Object> {
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

/// Replace the page's content streams with `build(old streams)`; a direct
/// content stream becomes an object first.
fn rewrite_contents(
    document: &mut Document,
    page_id: ObjectId,
    build: impl FnOnce(Vec<Object>) -> Vec<Object>,
) -> Result<()> {
    let old = match document
        .get_dictionary(page_id)?
        .get(b"Contents")
        .ok()
        .cloned()
    {
        Some(Object::Array(items)) => items,
        Some(Object::Stream(stream)) => vec![Object::Reference(document.add_object(stream))],
        Some(other) => vec![other],
        None => Vec::new(),
    };
    let contents = build(old);
    document
        .get_object_mut(page_id)?
        .as_dict_mut()?
        .set("Contents", contents);
    Ok(())
}

/// A resource name, derived from `base`, that none of `pages` uses yet in
/// `category`. Repeated runs (a second stamp or watermark) thereby never
/// redirect content drawn by an earlier run.
pub(crate) fn free_resource_name(
    document: &Document,
    pages: &[ObjectId],
    category: &[u8],
    base: &str,
) -> String {
    let used = pages
        .iter()
        .filter_map(|page_id| inherited_value(document, *page_id, b"Resources"))
        .filter_map(|resources| resolve_dictionary(document, &resources))
        .filter_map(|resources| {
            resources
                .get(category)
                .ok()
                .and_then(|group| resolve_dictionary(document, group))
        })
        .flat_map(|group| {
            group
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>()
        })
        .collect::<std::collections::HashSet<_>>();
    std::iter::once(base.to_owned())
        .chain((1..).map(|index| format!("{base}{index}")))
        .find(|name| !used.contains(name.as_bytes()))
        .expect("an unused resource name exists")
}

/// Adds the same named resources (category, name, object) to many pages.
/// A page's effective (possibly inherited or shared) Resources dictionary is
/// copied into a new object so pages outside the edit are unaffected; pages
/// that shared one Resources object keep sharing one copy.
pub(crate) struct ResourceInstaller<'a> {
    entries: Vec<(&'a [u8], &'a [u8], ObjectId)>,
    copies: HashMap<ObjectId, ObjectId>,
}

impl<'a> ResourceInstaller<'a> {
    pub(crate) fn new(entries: Vec<(&'a [u8], &'a [u8], ObjectId)>) -> Self {
        Self {
            entries,
            copies: HashMap::new(),
        }
    }

    pub(crate) fn install(&mut self, document: &mut Document, page_id: ObjectId) -> Result<()> {
        let source = inherited_value(document, page_id, b"Resources");
        let shared = match source {
            Some(Object::Reference(id)) => Some(id),
            _ => None,
        };
        let resources_id = match shared.and_then(|id| self.copies.get(&id)) {
            Some(copy) => *copy,
            None => {
                let mut resources = source
                    .and_then(|value| resolve_dictionary(document, &value))
                    .unwrap_or_default();
                for (category, name, id) in &self.entries {
                    let mut group = resources
                        .get(category)
                        .ok()
                        .and_then(|value| resolve_dictionary(document, value))
                        .unwrap_or_default();
                    group.set(name.to_vec(), *id);
                    resources.set(category.to_vec(), group);
                }
                let copy = document.add_object(resources);
                if let Some(id) = shared {
                    self.copies.insert(id, copy);
                }
                copy
            }
        };
        document
            .get_object_mut(page_id)?
            .as_dict_mut()?
            .set("Resources", resources_id);
        Ok(())
    }
}

/// A copy of the dictionary `object` is or refers to.
pub(crate) fn resolve_dictionary(document: &Document, object: &Object) -> Option<Dictionary> {
    resolve(document, object).ok()?.as_dict().ok().cloned()
}

/// Move the annotation rectangles of a resized page together with their
/// point lists (highlight quads, ink strokes, polygon vertices, lines).
fn transform_annotation_geometry(document: &mut Document, page_id: ObjectId, movement: Movement) {
    let references = |items: &[Object]| {
        items
            .iter()
            .filter_map(|item| item.as_reference().ok())
            .collect::<Vec<_>>()
    };
    let annotation_ids = match document
        .get_dictionary(page_id)
        .and_then(|page| page.get(b"Annots"))
    {
        Ok(Object::Array(items)) => references(items),
        Ok(Object::Reference(id)) => document
            .get_object(*id)
            .and_then(Object::as_array)
            .map(|items| references(items))
            .unwrap_or_default(),
        _ => Vec::new(),
    };

    for annotation_id in annotation_ids {
        let Ok(annotation) = document
            .get_object_mut(annotation_id)
            .and_then(Object::as_dict_mut)
        else {
            continue;
        };
        if let Some([x1, y1, x2, y2]) = annotation.get(b"Rect").ok().and_then(parse_box) {
            annotation.set(
                "Rect",
                vec![
                    movement.x(x1).into(),
                    movement.y(y1).into(),
                    movement.x(x2).into(),
                    movement.y(y2).into(),
                ],
            );
        }
        let move_points = |points: &mut Vec<Object>| {
            for (index, value) in points.iter_mut().enumerate() {
                movement.coordinate(value, index % 2 == 0);
            }
        };
        for key in [b"QuadPoints".as_slice(), b"Vertices", b"L", b"CL"] {
            if let Ok(Object::Array(points)) = annotation.get_mut(key) {
                move_points(points);
            }
        }
        if let Ok(Object::Array(strokes)) = annotation.get_mut(b"InkList") {
            for stroke in strokes {
                if let Object::Array(points) = stroke {
                    move_points(points);
                }
            }
        }
    }
}

/// Pages that are a single low-resolution scan with an oversized MediaBox
/// (phone scanners write e.g. 2400x3376 pt for an A4 sheet) get a factor
/// that makes stamp sizes and mm offsets behave as on the real paper.
/// Returns 1.0 for every other page.
pub(crate) fn scan_paper_factor(
    document: &Document,
    page_id: ObjectId,
    geometry: PageGeometry,
) -> f64 {
    const A3_SHORT: f64 = 841.89;
    const PAPERS: [(f64, f64); 2] = [(595.28, 841.89), (612.0, 792.0)];
    let short = geometry.raw_width().min(geometry.raw_height());
    let long = geometry.raw_width().max(geometry.raw_height());
    // Fonts are no criterion: an OCR text layer must not change the result.
    if short <= A3_SHORT * 1.2 {
        return 1.0;
    }
    let Ok(images) = document.get_page_images(page_id) else {
        return 1.0;
    };
    // The scan is the largest image; earlier stamps are much smaller.
    let Some(image) = images.iter().max_by_key(|image| image.width * image.height) else {
        return 1.0;
    };
    let image_long = image.width.max(image.height) as f64;
    if image_long / (long / 72.0) >= 100.0 {
        return 1.0;
    }
    let aspect = long / short;
    let (paper_short, _) = PAPERS
        .into_iter()
        .min_by(|a, b| {
            ((a.1 / a.0) - aspect)
                .abs()
                .total_cmp(&((b.1 / b.0) - aspect).abs())
        })
        .unwrap_or(PAPERS[0]);
    short / paper_short
}

/// `unit` scales the 10 mm margin and the X,Y mm offset (see
/// `scan_paper_factor`).
pub(crate) fn stamp_position(
    value: &str,
    page: PageGeometry,
    width: f64,
    height: f64,
    unit: f64,
) -> Result<(f64, f64)> {
    let mm_to_points = 72.0 / 25.4 * unit;
    let margin = 10.0 * mm_to_points;
    let (position, (offset_x, offset_y)) = parse_position(value)?;
    let (offset_x, offset_y) = (offset_x * mm_to_points, offset_y * mm_to_points);

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
        _ => (right, bottom),
    };
    Ok((x + offset_x, y + offset_y))
}

/// An anchor (`br`, `top-left`, ...) or an `X,Y` millimetre offset from the
/// bottom-right anchor, as (short anchor, offset).
pub(crate) fn parse_position(value: &str) -> Result<(&'static str, (f64, f64))> {
    let position = value.trim().to_ascii_lowercase();
    if let Some((x, y)) = position.split_once(',') {
        let offset = |text: &str, axis: &str| {
            text.trim()
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .with_context(|| format!("invalid {axis} offset in position '{value}'"))
        };
        return Ok(("br", (offset(x, "X")?, offset(y, "Y")?)));
    }
    let anchor = match position.as_str() {
        "tl" | "top-left" => "tl",
        "tc" | "top-center" => "tc",
        "tr" | "top-right" => "tr",
        "l" | "center-left" => "l",
        "c" | "center" => "c",
        "r" | "center-right" => "r",
        "bl" | "bottom-left" => "bl",
        "bc" | "bottom-center" => "bc",
        "br" | "bottom-right" | "" => "br",
        _ => bail!(
            "unsupported position '{value}'; use tl, tc, tr, l, c, r, bl, bc, br or X,Y in mm"
        ),
    };
    Ok((anchor, (0.0, 0.0)))
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
        let mut document = super::super::jpeg_document(jpeg, crate::pdf::PageSize::A4).unwrap();
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

    fn options(image_dpi: u32, jpeg_quality: u8) -> OptimizeOptions {
        OptimizeOptions {
            image_dpi,
            jpeg_quality,
            recompress_all: false,
            gray: false,
        }
    }

    #[test]
    fn gray_optimize_turns_colour_images_gray() {
        let mut document = placed_image(400, 300, 400.0, 300.0);
        let report = optimize(
            &mut document,
            OptimizeOptions {
                gray: true,
                ..options(0, 80)
            },
        );
        assert_eq!(report.resized_images, 1);
        let page_id = *document.get_pages().get(&1).unwrap();
        let image = document.get_page_images(page_id).unwrap().remove(0);
        assert_eq!(image.color_space.as_deref(), Some("DeviceGray"));
        assert_eq!(first_image_dimensions(&document), (400, 300));
        // Already gray: nothing left to do.
        let report = optimize(
            &mut document,
            OptimizeOptions {
                gray: true,
                ..options(0, 80)
            },
        );
        assert_eq!(report.resized_images, 0);
    }

    #[test]
    fn resize_moves_annotation_points_and_destinations() {
        let mut document = one_page();
        let page_id = *document.get_pages().get(&1).unwrap();
        let geometry = page_geometry(&document, page_id).unwrap();
        // Shrink the page to half its size first, so A4 scales it by two.
        let (width, height) = (geometry.raw_width() / 2.0, geometry.raw_height() / 2.0);
        let highlight = document.add_object(dictionary! {
            "Type" => "Annot", "Subtype" => "Highlight",
            "Rect" => vec![10.into(), 10.into(), 20.into(), 20.into()],
            "QuadPoints" => vec![10.into(), 20.into(), 20.into(), 20.into(), 10.into(), 10.into(), 20.into(), 10.into()],
        });
        let page = document
            .get_object_mut(page_id)
            .unwrap()
            .as_dict_mut()
            .unwrap();
        page.set(
            "MediaBox",
            vec![0.into(), 0.into(), width.into(), height.into()],
        );
        page.set("Annots", vec![Object::Reference(highlight)]);
        let target = document.add_object(Object::Array(vec![
            Object::Reference(page_id),
            Object::Name(b"XYZ".to_vec()),
            10.into(),
            20.into(),
            Object::Null,
        ]));

        resize_pages(&mut document, crate::pdf::PageSize::A4.sheet(), "all").unwrap();

        let number = |object: &Object| object_number(object).unwrap().round() as i64;
        let quads = document
            .get_dictionary(highlight)
            .unwrap()
            .get(b"QuadPoints")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(number)
            .collect::<Vec<_>>();
        assert_eq!(quads, [20, 40, 40, 40, 20, 20, 40, 20]);
        let destination = document.get_object(target).unwrap().as_array().unwrap();
        assert_eq!((number(&destination[2]), number(&destination[3])), (20, 40));
        assert_eq!(destination[4], Object::Null);
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
        orient_pages(&mut document, "1", Orientation::Portrait).unwrap();
        let after = page_geometry(&document, page_id).unwrap();
        assert!(after.display_width() < after.display_height());

        // Target portrait again -> should remain portrait without double rotation
        orient_pages(&mut document, "1", Orientation::Portrait).unwrap();
        let after2 = page_geometry(&document, page_id).unwrap();
        assert_eq!(after.rotation, after2.rotation);

        // Target landscape -> should rotate back to landscape
        orient_pages(&mut document, "1", Orientation::Landscape).unwrap();
        let after3 = page_geometry(&document, page_id).unwrap();
        assert!(after3.display_width() > after3.display_height());
    }

    #[test]
    fn resize_sets_a4_box_and_preserves_page() {
        let mut document = one_page();
        resize_pages(&mut document, crate::pdf::PageSize::A4.sheet(), "all").unwrap();
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
    fn resize_keeps_each_page_orientation() {
        let mut document = one_page();
        duplicate_first_page(&mut document);
        duplicate_first_page(&mut document);
        rotate_pages(&mut document, "2-3", 90).unwrap();

        resize_pages(&mut document, crate::pdf::PageSize::A4.sheet(), "all").unwrap();
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
    fn optimize_downsamples_image_to_its_placed_size() {
        let mut document = placed_image(1200, 600, 144.0, 72.0);
        let report = optimize(&mut document, options(36, 82));

        assert_eq!(report.resized_images, 1);
        assert_eq!(report.skipped_images, 0);
        assert!(report.warnings.is_empty());
        assert_eq!(first_image_dimensions(&document), (72, 36));

        let page_id = *document.get_pages().get(&1).unwrap();
        let embedded = document.get_page_images(page_id).unwrap().remove(0);
        let decoded = image::load_from_memory(embedded.content).unwrap();
        assert_eq!(decoded.dimensions(), (72, 36));

        let bytes = super::super::save_to_bytes(&mut document).unwrap();
        let parsed = Document::load_mem(&bytes).unwrap();
        assert_eq!(parsed.get_pages().len(), 1);
        assert_eq!(first_image_dimensions(&parsed), (72, 36));
    }

    #[test]
    fn optimize_dpi_zero_keeps_image_dimensions() {
        let mut document = placed_image(1200, 600, 144.0, 72.0);
        let report = optimize(&mut document, options(0, 82));

        assert_eq!(report.resized_images, 0);
        assert_eq!(report.skipped_images, 0);
        assert_eq!(first_image_dimensions(&document), (1200, 600));
    }

    #[test]
    fn optimize_resizes_a_shared_image_only_once() {
        let mut document = placed_image(1200, 600, 144.0, 72.0);
        duplicate_first_page(&mut document);

        let report = optimize(&mut document, options(36, 82));

        assert_eq!(report.resized_images, 1);
        assert_eq!(document.get_pages().len(), 2);
        for page_id in document.get_pages().into_values() {
            let image = document.get_page_images(page_id).unwrap().remove(0);
            assert_eq!((image.width, image.height), (72, 36));
        }
    }

    fn redraw_second_page(document: &mut Document, content: &str) {
        let page_id = *document.get_pages().get(&2).unwrap();
        let content_id =
            document.add_object(Stream::new(Dictionary::new(), content.as_bytes().to_vec()));
        document
            .get_dictionary_mut(page_id)
            .unwrap()
            .set("Contents", content_id);
    }

    #[test]
    fn optimize_keeps_the_largest_size_an_image_is_drawn_at() {
        let mut document = placed_image(1200, 600, 144.0, 72.0);
        duplicate_first_page(&mut document);
        redraw_second_page(&mut document, "q 576 0 0 288 0 0 cm /Im0 Do Q");
        optimize(&mut document, options(36, 82));
        assert_eq!(first_image_dimensions(&document), (288, 144));

        // Drawn at full resolution on one page: nothing to downsample.
        let mut document = placed_image(1200, 600, 144.0, 72.0);
        duplicate_first_page(&mut document);
        redraw_second_page(&mut document, "q 2400 0 0 1200 0 0 cm /Im0 Do Q");
        let report = optimize(&mut document, options(36, 82));
        assert_eq!(report.resized_images, 0);
        assert_eq!(first_image_dimensions(&document), (1200, 600));
    }

    #[test]
    fn optimize_follows_images_into_forms() {
        let mut document = placed_image(1200, 600, 144.0, 72.0);
        let page_id = *document.get_pages().get(&1).unwrap();
        let image_id = document.get_page_images(page_id).unwrap().remove(0).id;
        let form_id = document.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 1.into(), 1.into()],
                "Matrix" => vec![2.into(), 0.into(), 0.into(), 2.into(), 0.into(), 0.into()],
                "Resources" => dictionary! {"XObject" => dictionary! {"Im0" => image_id}},
            },
            b"72 0 0 36 0 0 cm /Im0 Do".to_vec(),
        ));
        let page = document.get_dictionary_mut(page_id).unwrap();
        page.set(
            "Resources",
            dictionary! {"XObject" => dictionary! {"Fm0" => form_id}},
        );
        let content_id = page.get(b"Contents").unwrap().as_reference().unwrap();
        document
            .get_object_mut(content_id)
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_content(b"q 0.5 0 0 0.5 0 0 cm /Fm0 Do Q".to_vec());

        let report = optimize(&mut document, options(36, 82));
        assert_eq!(report.resized_images, 1);
        // Objects are renumbered: find the image by its type.
        let image = document
            .objects
            .values()
            .filter_map(|object| object.as_stream().ok())
            .find(|stream| stream.dict.has(b"Width"))
            .unwrap();
        // 72x36 points at 36 dpi.
        assert_eq!(
            (
                image.dict.get(b"Width").unwrap().as_i64().unwrap(),
                image.dict.get(b"Height").unwrap().as_i64().unwrap()
            ),
            (36, 18)
        );
    }

    #[test]
    fn optimize_keeps_colours_of_flate_iccbased_images() {
        let mut document = placed_image(1200, 600, 144.0, 72.0);
        let page_id = *document.get_pages().get(&1).unwrap();
        let image_id = document.get_page_images(page_id).unwrap().remove(0).id;
        let profile_id = document.add_object(Stream::new(dictionary! {"N" => 3}, Vec::new()));
        // Noisy orange (like a scan), stored as Flate RGB with an ICC profile.
        let mut seed = 1u32;
        let pixels = (0..1200 * 600)
            .flat_map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let noise = |shift: u32| (seed >> shift) as u8 % 16;
                [222 + noise(16), 112 + noise(20), 12 + noise(24)]
            })
            .collect::<Vec<_>>();
        let mut stream = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 1200,
                "Height" => 600,
                "BitsPerComponent" => 8,
                "ColorSpace" => vec![Object::Name(b"ICCBased".to_vec()), profile_id.into()],
            },
            pixels,
        );
        stream.compress().unwrap();
        document.objects.insert(image_id, Object::Stream(stream));

        let report = optimize(&mut document, options(36, 90));

        assert_eq!(report.resized_images, 1, "{:?}", report.warnings);
        let embedded = document.get_page_images(page_id).unwrap().remove(0);
        let decoded = image::load_from_memory(embedded.content).unwrap().to_rgb8();
        let mean = |channel: usize| {
            decoded
                .pixels()
                .map(|pixel| f64::from(pixel.0[channel]))
                .sum::<f64>()
                / f64::from(decoded.width() * decoded.height())
        };
        let means = [mean(0), mean(1), mean(2)];
        assert!(
            means
                .iter()
                .zip([229.5, 119.5, 19.5])
                .all(|(actual, expected)| (actual - expected).abs() < 4.0),
            "{means:?}"
        );
    }

    #[test]
    fn optimize_keeps_graphics_lossless() {
        let mut document = placed_image(1200, 600, 144.0, 72.0);
        let page_id = *document.get_pages().get(&1).unwrap();
        let image_id = document.get_page_images(page_id).unwrap().remove(0).id;
        // A two-colour chart: red bars on white, stored losslessly.
        let pixels = (0..1200 * 600)
            .flat_map(|index| {
                if (index % 1200) / 100 % 2 == 0 {
                    [200, 0, 0]
                } else {
                    [255, 255, 255]
                }
            })
            .collect::<Vec<u8>>();
        let mut stream = Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image",
                "Width" => 1200, "Height" => 600,
                "BitsPerComponent" => 8, "ColorSpace" => "DeviceRGB",
            },
            pixels,
        );
        stream.compress().unwrap();
        document.objects.insert(image_id, Object::Stream(stream));

        let report = optimize(&mut document, options(36, 90));

        assert_eq!(report.resized_images, 1, "{:?}", report.warnings);
        let stream = document.get_object(image_id).unwrap().as_stream().unwrap();
        assert_eq!(stream.filters().unwrap(), [b"FlateDecode".as_slice()]);
        let decoded = super::super::image::decode(&document, stream)
            .unwrap()
            .to_rgb8();
        assert_eq!(decoded.dimensions(), (72, 36));
        assert_eq!(decoded.get_pixel(2, 18).0, [200, 0, 0]);
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

        let report = optimize(&mut document, options(100, 82));

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
                blend_mode: BlendMode::Normal,
                placements: Vec::new(),
            },
        )
        .unwrap();
        let page_id = *document.get_pages().get(&1).unwrap();
        assert_eq!(document.get_page_images(page_id).unwrap().len(), 2);
    }

    #[test]
    fn oversized_phone_scan_is_treated_as_paper() {
        let mut document = placed_image(120, 169, 2400.0, 3376.0);
        let page_id = *document.get_pages().get(&1).unwrap();
        document
            .get_object_mut(page_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set(
                "MediaBox",
                vec![0.into(), 0.into(), 2400.into(), 3376.into()],
            );
        let geometry = page_geometry(&document, page_id).unwrap();
        let factor = scan_paper_factor(&document, page_id, geometry);
        assert!((factor - 2400.0 / 595.28).abs() < 1e-6);

        // A stamp already on the scan does not hide it.
        let stamp = tempfile::Builder::new().suffix(".png").tempfile().unwrap();
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(8, 4, Rgba([255, 0, 0, 128])))
            .save_with_format(stamp.path(), ImageFormat::Png)
            .unwrap();
        apply_stamp(
            &mut document,
            &StampOptions {
                path: stamp.path().to_path_buf(),
                position: "br".to_owned(),
                scale: Some(1.0),
                dpi: Some(96.0),
                opacity: 1.0,
                pages: "all".to_owned(),
                blend_mode: BlendMode::Normal,
                placements: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(scan_paper_factor(&document, page_id, geometry), factor);

        let a4 = one_page();
        let a4_page = *a4.get_pages().get(&1).unwrap();
        let a4_geometry = page_geometry(&a4, a4_page).unwrap();
        assert_eq!(scan_paper_factor(&a4, a4_page, a4_geometry), 1.0);
    }

    #[test]
    fn a_direct_info_dictionary_keeps_its_other_fields() {
        let mut document = one_page();
        document.trailer.set(
            "Info",
            dictionary! { "Author" => Object::string_literal("Ann") },
        );
        set_info_fields(&mut document, &[("Title", Some("Report"))]).unwrap();
        let info = crate::pdf::info_dictionary(&document).unwrap();
        assert_eq!(info.get(b"Author").unwrap().as_str().unwrap(), b"Ann");
        assert_eq!(info.get(b"Title").unwrap().as_str().unwrap(), b"Report");
        assert!(
            document
                .trailer
                .get(b"Info")
                .unwrap()
                .as_reference()
                .is_ok()
        );
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
    fn stamp_position_follows_crop_and_rotation() {
        let temporary = tempfile::Builder::new().suffix(".png").tempfile().unwrap();
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(8, 4, Rgba([255, 0, 0, 128])))
            .save_with_format(temporary.path(), ImageFormat::Png)
            .unwrap();
        let mut document = one_page();
        let page_id = *document.get_pages().get(&1).unwrap();
        let page = document
            .get_object_mut(page_id)
            .unwrap()
            .as_dict_mut()
            .unwrap();
        page.set("MediaBox", vec![0.into(), 0.into(), 600.into(), 800.into()]);
        page.set(
            "CropBox",
            vec![50.into(), 100.into(), 550.into(), 700.into()],
        );
        page.set("Rotate", 90);

        apply_stamp(
            &mut document,
            &StampOptions {
                path: temporary.path().to_path_buf(),
                position: "br".to_owned(),
                scale: Some(1.0),
                dpi: Some(96.0),
                opacity: 1.0,
                pages: "all".to_owned(),
                blend_mode: BlendMode::Normal,
                placements: Vec::new(),
            },
        )
        .unwrap();

        let contents = document.get_page_contents(page_id);
        let stamp = document
            .get_object(*contents.last().unwrap())
            .unwrap()
            .as_stream()
            .unwrap();
        let text = String::from_utf8_lossy(&stamp.content);
        let matrix: Vec<f64> = text
            .split(" cm")
            .next()
            .unwrap()
            .split_whitespace()
            .rev()
            .take(6)
            .map(|value| value.parse().unwrap())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        // The displayed page is 600x500 pt; a 6x3 pt stamp 10 mm from its
        // bottom-right corner, kept upright: image X runs along the raw Y axis.
        let margin = 10.0 * 72.0 / 25.4;
        let expected = [
            0.0,
            6.0,
            -3.0,
            0.0,
            550.0 - margin,
            100.0 + 600.0 - margin - 6.0,
        ];
        for (value, expected) in matrix.iter().zip(expected) {
            assert!(
                (value - expected).abs() < 1e-3,
                "{matrix:?} != {expected:?}"
            );
        }
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
                blend_mode: BlendMode::Normal,
                placements: Vec::new(),
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
