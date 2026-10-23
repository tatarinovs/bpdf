use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use ::image::codecs::jpeg::JpegDecoder;
use ::image::{ColorType, ExtendedColorType, ImageDecoder};
use anyhow::{Context, Result, bail};
use lopdf::encryption::crypt_filters::{Aes256CryptFilter, CryptFilter};
use lopdf::{
    Dictionary, Document, EncryptionState, EncryptionVersion, Object, ObjectId, Permissions,
    Stream, dictionary,
};
use serde_json::{Value, json};

use crate::atomic::write_atomic;
pub(crate) mod blank;
pub(crate) mod image;
mod merge;
pub mod transform;

pub use merge::{merge_documents, merge_outlined};

/// Page attributes a page may inherit from its page-tree ancestors.
const INHERITABLE: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

pub const MAX_DECOMPRESSED_BYTES: usize = 256 * 1024 * 1024;

const POINTS_PER_MM: f64 = 72.0 / 25.4;

/// Named paper sizes, portrait width and height in millimetres.
const PAPERS: [(&str, f64, f64); 9] = [
    ("a3", 297.0, 420.0),
    ("a4", 210.0, 297.0),
    ("a5", 148.0, 210.0),
    ("a6", 105.0, 148.0),
    ("b4", 250.0, 353.0),
    ("b5", 176.0, 250.0),
    ("letter", 215.9, 279.4),
    ("legal", 215.9, 355.6),
    ("tabloid", 279.4, 431.8),
];

/// Target page size: a sheet of paper, the sizes of the source PDF pages
/// (`keep`), or each image at its own resolution (`fit`). Text, HTML and the
/// image resolution limit use A4 when no sheet is given.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PageSize {
    /// Portrait width and height in points.
    Paper {
        width: f64,
        height: f64,
    },
    Keep,
    Fit,
}

impl PageSize {
    pub const A4: Self = Self::Paper {
        width: 210.0 * POINTS_PER_MM,
        height: 297.0 * POINTS_PER_MM,
    };

    fn from_millimetres(width: f64, height: f64) -> Self {
        Self::Paper {
            width: width.min(height) * POINTS_PER_MM,
            height: width.max(height) * POINTS_PER_MM,
        }
    }

    /// Portrait size of the sheet; A4 for `keep` and `fit`.
    pub fn sheet(self) -> (f64, f64) {
        match self {
            Self::Paper { width, height } => (width, height),
            Self::Keep | Self::Fit => Self::A4.sheet(),
        }
    }

    /// True for a sheet every page is fitted onto.
    pub fn is_paper(self) -> bool {
        matches!(self, Self::Paper { .. })
    }
}

impl FromStr for PageSize {
    type Err = String;

    /// A paper name (A3-A6, B4, B5, Letter, Legal, Tabloid), `keep`, `fit`,
    /// or `WIDTHxHEIGHT` with a unit of mm (the default), cm, in or pt.
    fn from_str(value: &str) -> Result<Self, String> {
        let name = value.trim().to_ascii_lowercase();
        match name.as_str() {
            "keep" | "none" | "original" => return Ok(Self::Keep),
            "fit" => return Ok(Self::Fit),
            _ => {}
        }
        if let Some(&(_, width, height)) = PAPERS.iter().find(|(paper, ..)| *paper == name) {
            return Ok(Self::from_millimetres(width, height));
        }
        let invalid = || {
            format!(
                "unknown page size '{value}'; use A3, A4, A5, A6, B4, B5, Letter, Legal, \
                 Tabloid, keep, fit or WIDTHxHEIGHT[mm|cm|in|pt]"
            )
        };
        let (number, millimetres) = [("mm", 1.0), ("cm", 10.0), ("in", 25.4), ("pt", 25.4 / 72.0)]
            .into_iter()
            .find_map(|(unit, factor)| Some((name.strip_suffix(unit)?, factor)))
            .unwrap_or((name.as_str(), 1.0));
        let (width, height) = number.split_once(['x', '×', '*']).ok_or_else(invalid)?;
        let side = |text: &str| {
            text.trim()
                .replace(',', ".")
                .parse::<f64>()
                .ok()
                .map(|side| side * millimetres)
                // 1 pt to the 200-inch limit of PDF viewers.
                .filter(|side| (25.4 / 72.0..=5080.0).contains(side))
        };
        match (side(width), side(height)) {
            (Some(width), Some(height)) => Ok(Self::from_millimetres(width, height)),
            _ => Err(invalid()),
        }
    }
}

impl fmt::Display for PageSize {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Keep => formatter.write_str("keep"),
            Self::Fit => formatter.write_str("fit"),
            Self::Paper { width, height } => {
                let (width, height) = (width / POINTS_PER_MM, height / POINTS_PER_MM);
                match PAPERS.iter().find(|(_, paper_width, paper_height)| {
                    (paper_width - width).abs() < 0.2 && (paper_height - height).abs() < 0.2
                }) {
                    Some((name, ..)) if name.len() == 2 => {
                        formatter.write_str(&name.to_ascii_uppercase())
                    }
                    Some((name, ..)) => write!(
                        formatter,
                        "{}{}",
                        name[..1].to_ascii_uppercase(),
                        &name[1..]
                    ),
                    None => write!(formatter, "{width:.0}x{height:.0} mm"),
                }
            }
        }
    }
}

impl<'de> serde::Deserialize<'de> for PageSize {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Load a PDF. A document encrypted with an empty user password is decrypted
/// and encrypted again on save; one that needs a password is refused.
pub fn load(path: &Path) -> Result<Document> {
    load_with_password(path, None)
}

/// Load a PDF, opening an encrypted one with `password` (the user or the
/// owner password).
pub fn load_with_password(path: &Path, password: Option<&str>) -> Result<Document> {
    let loaded = match password {
        Some(password) => Document::load_with_password(path, password),
        None => Document::load(path),
    };
    let document = loaded.with_context(|| match password {
        Some(_) => format!(
            "failed to open PDF {}; is the password correct?",
            path.display()
        ),
        None => format!("failed to load PDF {}", path.display()),
    })?;
    if document.is_encrypted() {
        match password {
            Some(_) => bail!("wrong password for {}", path.display()),
            None => bail!(
                "{} is password-protected; remove the password first with `bpdf unlock`",
                path.display()
            ),
        }
    }
    Ok(document)
}

/// Restrictions of a protected PDF; every action is allowed by default.
#[derive(Clone, Copy, Debug, Default)]
pub struct Restrictions {
    pub print: bool,
    pub copy: bool,
    pub modify: bool,
}

impl Restrictions {
    pub fn any(self) -> bool {
        self.print || self.copy || self.modify
    }

    fn permissions(self) -> Permissions {
        let mut permissions = Permissions::all();
        if self.print {
            permissions.remove(Permissions::PRINTABLE | Permissions::PRINTABLE_IN_HIGH_QUALITY);
        }
        if self.copy {
            permissions.remove(Permissions::COPYABLE);
        }
        if self.modify {
            permissions.remove(
                Permissions::MODIFIABLE
                    | Permissions::ANNOTABLE
                    | Permissions::FILLABLE
                    | Permissions::ASSEMBLABLE,
            );
        }
        permissions
    }
}

/// Encrypt the document on save with AES-256 (PDF 2.0 security handler,
/// revision 6). `user_password` opens it (empty: anyone may open it), the
/// owner password lifts `restrictions`.
pub fn protect(
    document: &mut Document,
    user_password: &str,
    owner_password: &str,
    restrictions: Restrictions,
) -> Result<()> {
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|error| anyhow::anyhow!("no random source: {error}"))?;
    let filter: Arc<dyn CryptFilter> = Arc::new(Aes256CryptFilter);
    let state = EncryptionState::try_from(EncryptionVersion::V5 {
        encrypt_metadata: true,
        crypt_filters: std::collections::BTreeMap::from([(b"StdCF".to_vec(), filter)]),
        file_encryption_key: &key,
        stream_filter: b"StdCF".to_vec(),
        string_filter: b"StdCF".to_vec(),
        owner_password,
        user_password,
        permissions: restrictions.permissions(),
    })
    .context("failed to prepare PDF encryption")?;
    // AES-256 belongs to PDF 2.0; for 1.x readers it is Adobe extension level 8.
    if document.version.as_str() < "1.7" {
        document.version = "1.7".to_owned();
    }
    if document.version.as_str() < "2.0" {
        document.catalog_mut()?.set(
            "Extensions",
            dictionary! {
                "ADBE" => dictionary! {
                    "BaseVersion" => Object::Name(b"1.7".to_vec()),
                    "ExtensionLevel" => 8,
                },
            },
        );
    }
    document.encryption_state = Some(state);
    Ok(())
}

/// Random lowercase hex, for an owner password nobody needs to know.
pub fn random_password() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!("no random source: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub fn transform_file<F>(path: &Path, transform: F) -> Result<Vec<u8>>
where
    F: FnOnce(&mut Document) -> Result<()>,
{
    let mut document = load(path)?;
    transform(&mut document)?;
    save_to_bytes(&mut document)
}

pub fn extract_text(document: &Document) -> Result<String> {
    let pages = document.get_pages().keys().copied().collect::<Vec<_>>();
    document
        .extract_text_with_limit(&pages, MAX_DECOMPRESSED_BYTES)
        .context("failed to extract PDF text")
}

/// Human-readable report and JSON data describing a PDF: version, page
/// geometry, fonts, images and document properties.
pub fn info(path: &Path) -> Result<(String, Value)> {
    let document = load(path)?;
    let pages = document.get_pages();
    let mut report = format!(
        "File: {}
PDF version: {}
Pages: {}
",
        path.display(),
        document.version,
        pages.len()
    );
    let mut page_data = Vec::with_capacity(pages.len());
    for (number, page_id) in &pages {
        let count = |items: Result<usize, lopdf::Error>| items.unwrap_or_default();
        let fonts = count(document.get_page_fonts(*page_id).map(|items| items.len()));
        let images = count(document.get_page_images(*page_id).map(|items| items.len()));
        let crop = transform::page_crop_geometry(&document, *page_id).ok();
        let (width, height, rotation) = crop.map_or((0.0, 0.0, 0), |geometry| {
            (
                geometry.display_width(),
                geometry.display_height(),
                geometry.rotation,
            )
        });
        let millimetres = |points: f64| points * 25.4 / 72.0;
        report.push_str(&format!(
            "Page {number}: {width:.0}x{height:.0} pt ({:.0}x{:.0} mm), rotate={rotation}, fonts={fonts}, images={images}
",
            millimetres(width),
            millimetres(height),
        ));
        page_data.push(json!({
            "page": number,
            "width": width,
            "height": height,
            "rotate": rotation,
            "fonts": fonts,
            "images": images,
        }));
    }

    let mut fields = serde_json::Map::new();
    if let Some(info) = info_dictionary(&document) {
        for key in [
            "Title",
            "Author",
            "Subject",
            "Keywords",
            "Creator",
            "Producer",
            "CreationDate",
            "ModDate",
        ] {
            if let Ok(value) = info.get(key.as_bytes()) {
                let value = pdf_text(value);
                report.push_str(&format!(
                    "{key}: {value}
"
                ));
                fields.insert(key.to_owned(), Value::String(value));
            }
        }
    }
    let has_xmp = document
        .catalog()
        .ok()
        .is_some_and(|catalog| catalog.get(b"Metadata").is_ok());
    if has_xmp {
        report.push_str(
            "XMP metadata: present
",
        );
    }
    let data = json!({
        "path": path.to_string_lossy(),
        "version": document.version,
        "pages": page_data,
        "fields": fields,
        "xmp": has_xmp,
    });
    Ok((report, data))
}

/// The document information dictionary, whether the trailer holds it
/// directly or by reference.
pub(crate) fn info_dictionary(document: &Document) -> Option<&Dictionary> {
    resolve(document, document.trailer.get(b"Info").ok()?)
        .ok()?
        .as_dict()
        .ok()
}

/// One PDF per page in `output_dir`. Existing files are only replaced with
/// `force`; every name is checked before the first one is written.
pub fn split_file(path: &Path, output_dir: &Path, force: bool) -> Result<Vec<PathBuf>> {
    let document = load(path)?;
    let pages = document.get_pages().into_values().collect::<Vec<_>>();
    if pages.is_empty() {
        bail!("PDF contains no pages");
    }
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("page");
    let width = pages.len().to_string().len();
    let jobs = pages
        .into_iter()
        .enumerate()
        .map(|(index, page_id)| {
            let output = output_dir.join(format!("{stem}_{:0width$}.pdf", index + 1));
            (page_id, output)
        })
        .collect::<Vec<_>>();
    if !force && let Some((_, existing)) = jobs.iter().find(|(_, output)| output.exists()) {
        bail!(
            "{} already exists; pass --force to replace the split pages",
            existing.display()
        );
    }

    crate::parallel::map(&jobs, crate::parallel::cpu_jobs(), |(page_id, output)| {
        let mut one_page = page_document(&document, *page_id)?;
        write_atomic(output, &save_to_bytes(&mut one_page)?)?;
        Ok(output.clone())
    })
    .into_iter()
    .collect()
}

/// A copy of the page dictionary with the attributes it inherits from the
/// page tree written into it.
fn standalone_page(document: &Document, page_id: ObjectId) -> Result<Dictionary> {
    let mut page = document.get_dictionary(page_id)?.clone();
    for key in INHERITABLE {
        if page.get(key).is_err()
            && let Some(value) = transform::inherited_value(document, page_id, key)
        {
            page.set(key, value);
        }
    }
    Ok(page)
}

/// Build a standalone document from one page, copying only the objects that
/// page can reach. Other pages and page-tree nodes are cut off (replaced by
/// null) so a link to another page does not drag the whole document along.
fn page_document(source: &Document, page_id: ObjectId) -> Result<Document> {
    const CATALOG_SKIP: [&[u8]; 8] = [
        b"Type",
        b"Pages",
        b"Outlines",
        b"StructTreeRoot",
        b"PageLabels",
        b"Names",
        b"Dests",
        b"OpenAction",
    ];

    let mut page = standalone_page(source, page_id)?;
    page.remove(b"Parent");

    let mut target = Document::with_version(source.version.as_str());
    let pages_id: ObjectId = (1, 0);
    let new_page_id: ObjectId = (2, 0);
    let mut copier = ObjectCopier {
        source,
        ids: HashMap::from([(page_id, new_page_id)]),
        queue: Vec::new(),
        next_id: 3,
    };

    let mut page = Object::Dictionary(page);
    copier.remap(&mut page);
    let mut page = page.as_dict()?.clone();
    page.set("Parent", pages_id);
    target.objects.insert(new_page_id, Object::Dictionary(page));
    target.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(new_page_id)],
            "Count" => 1,
        }),
    );

    let mut catalog = dictionary! { "Type" => "Catalog", "Pages" => pages_id };
    if let Ok(source_catalog) = source.catalog() {
        for (key, value) in source_catalog.iter() {
            if !CATALOG_SKIP.contains(&key.as_slice()) {
                let mut value = value.clone();
                copier.remap(&mut value);
                catalog.set(key.clone(), value);
            }
        }
    }
    let mut info = source.trailer.get(b"Info").ok().cloned();
    if let Some(info) = &mut info {
        copier.remap(info);
    }

    while let Some((old_id, new_id)) = copier.queue.pop() {
        let mut object = source.get_object(old_id)?.clone();
        copier.remap(&mut object);
        target.objects.insert(new_id, object);
    }

    let catalog_id = (copier.next_id, 0);
    target
        .objects
        .insert(catalog_id, Object::Dictionary(catalog));
    target.trailer.set("Root", catalog_id);
    if let Some(info) = info {
        target.trailer.set("Info", info);
    }
    target.max_id = copier.next_id;
    Ok(target)
}

struct ObjectCopier<'a> {
    source: &'a Document,
    ids: HashMap<ObjectId, ObjectId>,
    queue: Vec<(ObjectId, ObjectId)>,
    next_id: u32,
}

impl ObjectCopier<'_> {
    /// Rewrite references inside `object` to target ids, scheduling newly
    /// reached objects for copying.
    fn remap(&mut self, object: &mut Object) {
        match object {
            Object::Reference(id) => *object = self.target_reference(*id),
            Object::Array(items) => items.iter_mut().for_each(|item| self.remap(item)),
            Object::Dictionary(dictionary) => {
                dictionary
                    .iter_mut()
                    .for_each(|(_, value)| self.remap(value));
            }
            Object::Stream(stream) => {
                stream
                    .dict
                    .iter_mut()
                    .for_each(|(_, value)| self.remap(value));
            }
            _ => {}
        }
    }

    fn target_reference(&mut self, id: ObjectId) -> Object {
        if let Some(new_id) = self.ids.get(&id) {
            return Object::Reference(*new_id);
        }
        let is_page_node = self
            .source
            .get_dictionary(id)
            .ok()
            .and_then(|dictionary| dictionary.get(b"Type").ok())
            .is_some_and(|kind| matches!(kind.as_name(), Ok(b"Page" | b"Pages")));
        if is_page_node || self.source.get_object(id).is_err() {
            return Object::Null;
        }
        let new_id = (self.next_id, 0);
        self.next_id += 1;
        self.ids.insert(id, new_id);
        self.queue.push((id, new_id));
        Object::Reference(new_id)
    }
}

/// Remove document properties, XMP packets and private application data
/// from every object, and EXIF/XMP segments from embedded JPEG images
/// (their pixels and colour profiles are kept as they are).
pub fn strip_document_metadata(document: &mut Document) {
    for object in document.objects.values_mut() {
        match object {
            Object::Dictionary(dictionary) => remove_keys(dictionary, &[b"Metadata", b"PieceInfo"]),
            Object::Stream(stream) => {
                remove_keys(&mut stream.dict, &[b"Metadata", b"PieceInfo"]);
                if let Some(jpeg) = image::embedded_jpeg(stream)
                    && let Ok(clean) = crate::metadata::strip_jpeg(jpeg, true)
                    && clean.len() < jpeg.len()
                {
                    stream.set_content(clean);
                }
            }
            _ => {}
        }
    }
    document.trailer.remove(b"Info");
    // An encrypted document's keys derive from its identifier.
    if !document.trailer.has(b"Encrypt") && !document.was_encrypted() {
        document.trailer.remove(b"ID");
    }
}

pub fn save_to_bytes(document: &mut Document) -> Result<Vec<u8>> {
    document.prune_objects();
    document.renumber_objects();
    document.compress();
    // The source's encryption and permissions carry over (or `protect` set
    // new ones). Before revision 5 the keys derive from the first file
    // identifier, so it must still be present.
    if let Some(state) = document.encryption_state.take() {
        if state.revision() >= 5 || document.trailer.has(b"ID") {
            document
                .encrypt(&state)
                .context("failed to encrypt PDF again")?;
        } else {
            crate::output::warn("the source PDF had no file identifier; saved without encryption");
        }
    }
    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .context("failed to serialize PDF")?;
    Ok(bytes)
}

/// Keep the pages named by `expression` in its order; a page listed twice
/// appears twice. The kept pages move under one new page-tree root, taking
/// their inherited attributes along, and the others are removed.
pub(crate) fn select_pages(document: &mut Document, expression: &str) -> Result<()> {
    let pages = document.get_pages();
    if pages.is_empty() {
        bail!("PDF contains no pages");
    }
    let list = parse_page_list(expression, pages.len())?;
    let root_id = document.new_object_id();
    let mut kept = HashSet::with_capacity(list.len());
    let mut kids = Vec::with_capacity(list.len());
    for number in &list {
        let page_id = pages[&(*number as u32)];
        let mut page = standalone_page(document, page_id)?;
        page.set("Parent", root_id);
        let id = if kept.insert(page_id) {
            document.objects.insert(page_id, Object::Dictionary(page));
            page_id
        } else {
            document.add_object(page)
        };
        kids.push(Object::Reference(id));
    }
    let count = kids.len() as i64;
    document.objects.insert(
        root_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count }),
    );
    document.catalog_mut()?.set("Pages", root_id);
    // Outlines and links may still point at dropped pages; without the page
    // objects those references read as null instead of keeping them alive.
    for page_id in pages.into_values().filter(|id| !kept.contains(id)) {
        document.objects.remove(&page_id);
    }
    document.prune_objects();
    Ok(())
}

/// Pages named by `expression`, as a set (selection for an edit).
pub(crate) fn parse_page_selection(expression: &str, page_count: usize) -> Result<BTreeSet<usize>> {
    Ok(parse_page_list(expression, page_count)?
        .into_iter()
        .collect())
}

/// Pages named by `expression`, in order and with repeats: comma-separated
/// numbers, `first`, `last` (`l`), ranges such as `2-5`, `5-` (to the end) or
/// `5-1` (backwards), and the words `all`, `even`, `odd` and `reverse`.
pub(crate) fn parse_page_list(expression: &str, page_count: usize) -> Result<Vec<usize>> {
    let expression = expression.trim().to_ascii_lowercase();
    let mut pages = Vec::new();
    for part in expression.split(',').map(str::trim) {
        match part {
            "" => bail!("empty page selection"),
            "all" => pages.extend(1..=page_count),
            "reverse" => pages.extend((1..=page_count).rev()),
            "even" => pages.extend((2..=page_count).step_by(2)),
            "odd" => pages.extend((1..=page_count).step_by(2)),
            _ => match part.split_once('-') {
                Some((start, end)) => {
                    let bound = |value: &str, open: usize| match value.trim() {
                        "" => Ok(open),
                        value => parse_page_number(value, page_count),
                    };
                    let (start, end) = (bound(start, 1)?, bound(end, page_count)?);
                    if start <= end {
                        pages.extend(start..=end);
                    } else {
                        pages.extend((end..=start).rev());
                    }
                }
                None => pages.push(parse_page_number(part, page_count)?),
            },
        }
    }
    if pages.is_empty() {
        bail!("no pages selected");
    }
    Ok(pages)
}

fn parse_page_number(value: &str, page_count: usize) -> Result<usize> {
    match value {
        "first" => return Ok(1),
        "l" | "last" => return Ok(page_count),
        _ => {}
    }
    let page = value
        .parse::<usize>()
        .with_context(|| format!("invalid page number {value}"))?;
    if page == 0 || page > page_count {
        bail!("page {page} is outside 1-{page_count}");
    }
    Ok(page)
}

/// Where an image is drawn on its page: the page follows the image
/// orientation and the image is fitted and centred.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ImagePlacement {
    pub page_width: f64,
    pub page_height: f64,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// `dpi` is the image resolution, used by `PageSize::Fit` (96 when unknown).
pub(crate) fn image_placement(
    pixel_width: u32,
    pixel_height: u32,
    page_size: PageSize,
    dpi: Option<f64>,
) -> ImagePlacement {
    if page_size == PageSize::Fit {
        let scale = 72.0 / dpi.filter(|dpi| *dpi >= 1.0).unwrap_or(96.0);
        let (width, height) = (
            f64::from(pixel_width) * scale,
            f64::from(pixel_height) * scale,
        );
        return ImagePlacement {
            page_width: width,
            page_height: height,
            x: 0.0,
            y: 0.0,
            width,
            height,
        };
    }
    let (mut page_width, mut page_height) = page_size.sheet();
    if (pixel_width > pixel_height) != (page_width > page_height) {
        std::mem::swap(&mut page_width, &mut page_height);
    }
    let scale = (page_width / f64::from(pixel_width)).min(page_height / f64::from(pixel_height));
    let width = f64::from(pixel_width) * scale;
    let height = f64::from(pixel_height) * scale;
    ImagePlacement {
        page_width,
        page_height,
        x: (page_width - width) / 2.0,
        y: (page_height - height) / 2.0,
        width,
        height,
    }
}

/// Image-space matrix (`cm` operands) drawing a stored JPEG into `rect`
/// upright according to its EXIF orientation (1-8). The pixels themselves are
/// never rotated, so the embedded JPEG stays byte-identical.
fn oriented_image_matrix(orientation: u16, rect: &ImagePlacement) -> [f64; 6] {
    let ImagePlacement {
        x,
        y,
        width: w,
        height: h,
        ..
    } = *rect;
    match orientation {
        2 => [-w, 0.0, 0.0, h, x + w, y],
        3 => [-w, 0.0, 0.0, -h, x + w, y + h],
        4 => [w, 0.0, 0.0, -h, x, y + h],
        5 => [0.0, -h, -w, 0.0, x + w, y + h],
        6 => [0.0, -h, w, 0.0, x, y + h],
        7 => [0.0, h, w, 0.0, x, y],
        8 => [0.0, h, -w, 0.0, x + w, y],
        _ => [w, 0.0, 0.0, h, x, y],
    }
}

/// A one-page document showing the JPEG; with `PageSize::Fit` the page takes
/// the image size at the resolution recorded in the JPEG.
pub(crate) fn jpeg_document(jpeg: Vec<u8>, page_size: PageSize) -> Result<Document> {
    jpeg_document_at(jpeg, page_size, None)
}

/// [`jpeg_document`] with the image resolution for `PageSize::Fit`; `None`
/// reads it from the JPEG.
pub(crate) fn jpeg_document_at(
    jpeg: Vec<u8>,
    page_size: PageSize,
    dpi: Option<f64>,
) -> Result<Document> {
    let info = jpeg_info(&jpeg)?;
    let orientation = crate::metadata::jpeg_orientation(&jpeg);
    let (display_width, display_height) = if orientation >= 5 {
        (u32::from(info.height), u32::from(info.width))
    } else {
        (u32::from(info.width), u32::from(info.height))
    };
    let dpi = match page_size {
        PageSize::Fit => dpi.or_else(|| crate::metadata::image_dpi(&jpeg)),
        _ => None,
    };
    let placement = image_placement(display_width, display_height, page_size, dpi);
    let (page_width, page_height) = (placement.page_width, placement.page_height);
    let matrix = oriented_image_matrix(orientation, &placement)
        .map(|value| format!("{value:.6}"))
        .join(" ");

    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();

    let mut image_dictionary = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Image",
        "Width" => info.width as i64,
        "Height" => info.height as i64,
        "BitsPerComponent" => 8,
        "Filter" => "DCTDecode",
    };
    match info.components {
        1 => image_dictionary.set("ColorSpace", "DeviceGray"),
        3 => image_dictionary.set("ColorSpace", "DeviceRGB"),
        4 => {
            image_dictionary.set("ColorSpace", "DeviceCMYK");
            image_dictionary.set(
                "Decode",
                vec![
                    1.into(),
                    0.into(),
                    1.into(),
                    0.into(),
                    1.into(),
                    0.into(),
                    1.into(),
                    0.into(),
                ],
            );
        }
        components => bail!("unsupported JPEG component count: {components}"),
    }
    let image_id = document.add_object(Stream::new(image_dictionary, jpeg));

    let content = format!("q\n{matrix} cm\n/Im0 Do\nQ\n");
    let content_id = document.add_object(Stream::new(dictionary! {}, content.into_bytes()));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! {
            "Im0" => image_id,
        },
    });
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), page_width.into(), page_height.into()],
        "Resources" => resources_id,
        "Contents" => content_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    document.compress();
    Ok(document)
}

pub(super) fn object_number(object: &Object) -> Result<f64> {
    match object {
        Object::Integer(value) => Ok(*value as f64),
        Object::Real(value) => Ok(f64::from(*value)),
        _ => bail!("expected PDF number"),
    }
}

#[derive(Debug)]
pub(crate) struct JpegInfo {
    pub width: u16,
    pub height: u16,
    pub components: u8,
}

pub(crate) fn jpeg_info(bytes: &[u8]) -> Result<JpegInfo> {
    let decoder = JpegDecoder::new(Cursor::new(bytes)).context("not a valid JPEG file")?;
    let (width, height) = decoder.dimensions();
    let components = match decoder.original_color_type() {
        ExtendedColorType::L8 => 1,
        ExtendedColorType::Rgb8 | ExtendedColorType::Bgr8 => 3,
        ExtendedColorType::Cmyk8 => 4,
        other => match decoder.color_type() {
            ColorType::L8 => 1,
            ColorType::Rgb8 => 3,
            _ => bail!("unsupported JPEG color format: {other:?}"),
        },
    };
    Ok(JpegInfo {
        width: u16::try_from(width).context("JPEG width exceeds u16")?,
        height: u16::try_from(height).context("JPEG height exceeds u16")?,
        components,
    })
}

/// The object a reference points to, or `object` itself.
pub(crate) fn resolve<'a>(document: &'a Document, object: &'a Object) -> Result<&'a Object> {
    match object {
        Object::Reference(id) => Ok(document.get_object(*id)?),
        other => Ok(other),
    }
}

fn remove_keys(dictionary: &mut Dictionary, keys: &[&[u8]]) {
    for key in keys {
        dictionary.remove(key);
    }
}

/// A text string in PDFDocEncoding, UTF-16BE or UTF-8 (with their BOMs).
pub(crate) fn pdf_text(object: &Object) -> String {
    match object {
        Object::String(bytes, _) => lopdf::decode_text_string(object)
            .map(|text| text.trim_start_matches('\u{feff}').to_owned())
            .unwrap_or_else(|_| String::from_utf8_lossy(bytes).into()),
        Object::Name(bytes) => String::from_utf8_lossy(bytes).into(),
        Object::Integer(value) => value.to_string(),
        Object::Real(value) => value.to_string(),
        _ => format!("{object:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::common::test_utils::sample_jpeg_bytes as sample_jpeg;

    #[test]
    fn parses_page_selections() {
        assert_eq!(
            parse_page_selection("1-3,5,last", 7).unwrap(),
            BTreeSet::from([1, 2, 3, 5, 7])
        );
        assert_eq!(
            parse_page_selection("even", 5).unwrap(),
            BTreeSet::from([2, 4])
        );
        assert!(parse_page_selection("0", 5).is_err());
        assert!(parse_page_selection("6", 5).is_err());
    }

    #[test]
    fn page_lists_keep_order_and_repeats() {
        assert_eq!(parse_page_list("3,1-2,3", 4).unwrap(), [3, 1, 2, 3]);
        assert_eq!(parse_page_list("3-", 4).unwrap(), [3, 4]);
        assert_eq!(parse_page_list("-2,last-3", 4).unwrap(), [1, 2, 4, 3]);
        assert_eq!(parse_page_list("reverse", 3).unwrap(), [3, 2, 1]);
        assert_eq!(parse_page_list("first,even", 5).unwrap(), [1, 2, 4]);
        assert!(parse_page_list(" , ", 3).is_err());
    }

    #[test]
    fn selection_reorders_and_duplicates_pages() {
        // Page 1 is landscape (3x2 image), page 2 portrait (2x3).
        let first = jpeg_document(sample_jpeg(3, 2), crate::pdf::PageSize::A4).unwrap();
        let second = jpeg_document(sample_jpeg(2, 3), crate::pdf::PageSize::A4).unwrap();
        let mut merged = merge_documents(vec![first, second]).unwrap();
        select_pages(&mut merged, "2,1,2").unwrap();
        let bytes = save_to_bytes(&mut merged).unwrap();
        let loaded = Document::load_mem(&bytes).unwrap();
        let landscape = loaded
            .get_pages()
            .into_values()
            .map(|page_id| {
                transform::page_geometry(&loaded, page_id)
                    .unwrap()
                    .is_landscape()
            })
            .collect::<Vec<_>>();
        assert_eq!(landscape, [false, true, false]);
    }

    #[test]
    fn creates_valid_image_pdf() {
        let mut document = jpeg_document(sample_jpeg(3, 2), crate::pdf::PageSize::A4).unwrap();
        let bytes = save_to_bytes(&mut document).unwrap();
        let loaded = Document::load_mem(&bytes).unwrap();
        assert_eq!(loaded.get_pages().len(), 1);
    }

    #[test]
    fn merged_page_trees_keep_all_pages() {
        let first = jpeg_document(sample_jpeg(3, 2), crate::pdf::PageSize::A4).unwrap();
        let second = jpeg_document(sample_jpeg(2, 3), crate::pdf::PageSize::A4).unwrap();
        let mut merged = merge_documents(vec![first, second]).unwrap();
        let bytes = save_to_bytes(&mut merged).unwrap();
        let loaded = Document::load_mem(&bytes).unwrap();
        assert_eq!(loaded.get_pages().len(), 2);
    }

    #[test]
    fn page_selection_removes_unselected_page_tree_branches() {
        let first = jpeg_document(sample_jpeg(3, 2), crate::pdf::PageSize::A4).unwrap();
        let second = jpeg_document(sample_jpeg(2, 3), crate::pdf::PageSize::A4).unwrap();
        let mut merged = merge_documents(vec![first, second]).unwrap();
        select_pages(&mut merged, "2").unwrap();
        let bytes = save_to_bytes(&mut merged).unwrap();
        let loaded = Document::load_mem(&bytes).unwrap();
        assert_eq!(loaded.get_pages().len(), 1);
        assert_eq!(
            loaded
                .get_page_images(*loaded.get_pages().get(&1).unwrap())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn split_clones_keep_pages_isolated() {
        let first = jpeg_document(sample_jpeg(3, 2), crate::pdf::PageSize::A4).unwrap();
        let second = jpeg_document(sample_jpeg(2, 3), crate::pdf::PageSize::A4).unwrap();
        let mut merged = merge_documents(vec![first, second]).unwrap();
        let source = tempfile::Builder::new().suffix(".pdf").tempfile().unwrap();
        merged.save(source.path()).unwrap();
        let output = tempfile::tempdir().unwrap();

        let files = split_file(source.path(), output.path(), false).unwrap();
        // A second split would replace the pages: only with --force.
        let error = split_file(source.path(), output.path(), false).unwrap_err();
        assert!(format!("{error:#}").contains("--force"), "{error:#}");
        assert_eq!(
            split_file(source.path(), output.path(), true).unwrap(),
            files
        );

        assert_eq!(files.len(), 2);
        for file in files {
            let split = load(&file).unwrap();
            assert_eq!(split.get_pages().len(), 1);
            let page_id = *split.get_pages().get(&1).unwrap();
            assert_eq!(split.get_page_images(page_id).unwrap().len(), 1);
        }
    }

    fn encrypted_file(user_password: &str) -> tempfile::NamedTempFile {
        use lopdf::{EncryptionState, EncryptionVersion, Permissions};
        let mut document = jpeg_document(sample_jpeg(3, 2), crate::pdf::PageSize::A4).unwrap();
        let id = Object::string_literal(b"0123456789abcdef".to_vec());
        document.trailer.set("ID", vec![id.clone(), id]);
        let state = EncryptionState::try_from(EncryptionVersion::V2 {
            document: &document,
            owner_password: "owner",
            user_password,
            key_length: 128,
            permissions: Permissions::PRINTABLE,
        })
        .unwrap();
        document.encrypt(&state).unwrap();
        let file = tempfile::Builder::new().suffix(".pdf").tempfile().unwrap();
        document.save(file.path()).unwrap();
        file
    }

    #[test]
    fn empty_password_encryption_survives_an_edit() {
        let source = encrypted_file("");
        for edit in [
            transform_file(source.path(), |document| {
                transform::rotate_pages(document, "all", 90)
            }),
            transform_file(source.path(), |document| {
                strip_document_metadata(document);
                Ok(())
            }),
        ] {
            let reloaded = Document::load_mem(&edit.unwrap()).unwrap();
            assert!(reloaded.was_encrypted());
            assert_eq!(reloaded.get_pages().len(), 1);
            let page_id = *reloaded.get_pages().get(&1).unwrap();
            assert_eq!(reloaded.get_page_images(page_id).unwrap().len(), 1);
        }
    }

    #[test]
    fn password_protected_pdf_is_refused_clearly() {
        let source = encrypted_file("secret");
        let error = load(source.path()).unwrap_err();
        assert!(
            format!("{error:#}").contains("password-protected"),
            "{error:#}"
        );
    }

    fn protected_file(
        user: &str,
        owner: &str,
        restrictions: Restrictions,
    ) -> tempfile::NamedTempFile {
        let mut document = jpeg_document(sample_jpeg(3, 2), PageSize::A4).unwrap();
        protect(&mut document, user, owner, restrictions).unwrap();
        let file = tempfile::Builder::new().suffix(".pdf").tempfile().unwrap();
        std::fs::write(file.path(), save_to_bytes(&mut document).unwrap()).unwrap();
        file
    }

    #[test]
    fn protected_pdf_opens_with_either_password_only() {
        let file = protected_file("open", "owner", Restrictions::default());
        assert!(load(file.path()).is_err());
        assert!(load_with_password(file.path(), Some("wrong")).is_err());
        for password in ["open", "owner"] {
            let document = load_with_password(file.path(), Some(password)).unwrap();
            assert!(document.was_encrypted());
            assert_eq!(document.get_pages().len(), 1);
        }
    }

    #[test]
    fn restrictions_survive_edits_and_unlock_removes_them() {
        let restrictions = Restrictions {
            print: true,
            copy: true,
            modify: false,
        };
        let file = protected_file("", "owner", restrictions);
        // No open password: the file loads, and an edit keeps the protection.
        let edited = transform_file(file.path(), |document| {
            transform::rotate_pages(document, "all", 90)
        })
        .unwrap();
        let reloaded = Document::load_mem(&edited).unwrap();
        let state = reloaded.encryption_state.as_ref().unwrap();
        assert_eq!(state.revision(), 6);
        assert!(!state.permissions().contains(Permissions::PRINTABLE));
        assert!(!state.permissions().contains(Permissions::COPYABLE));
        assert!(state.permissions().contains(Permissions::MODIFIABLE));

        let mut unlocked = load_with_password(file.path(), Some("owner")).unwrap();
        unlocked.encryption_state = None;
        let unlocked = Document::load_mem(&save_to_bytes(&mut unlocked).unwrap()).unwrap();
        assert!(!unlocked.was_encrypted());
        assert!(unlocked.trailer.get(b"Encrypt").is_err());
    }

    #[test]
    fn page_sizes_parse_names_units_and_modes() {
        let points = |value: &str| match value.parse::<PageSize>().unwrap() {
            PageSize::Paper { width, height } => (width.round(), height.round()),
            other => panic!("{other:?}"),
        };
        assert_eq!(points("A4"), (595.0, 842.0));
        assert_eq!(points("letter"), (612.0, 792.0));
        assert_eq!(points("Legal"), (612.0, 1008.0));
        assert_eq!(points("a3"), (842.0, 1191.0));
        assert_eq!(points("297x210mm"), (595.0, 842.0));
        assert_eq!(points("21 x 29,7 cm"), (595.0, 842.0));
        assert_eq!(points("8.5x11in"), (612.0, 792.0));
        assert_eq!(points("612x792pt"), (612.0, 792.0));
        assert_eq!("keep".parse::<PageSize>(), Ok(PageSize::Keep));
        assert_eq!("ORIGINAL".parse::<PageSize>(), Ok(PageSize::Keep));
        assert_eq!("fit".parse::<PageSize>(), Ok(PageSize::Fit));
        for invalid in ["A9", "x", "0x10", "10x10km", "100000x10"] {
            assert!(invalid.parse::<PageSize>().is_err(), "{invalid}");
        }
        assert_eq!(PageSize::A4.to_string(), "A4");
        assert_eq!("letter".parse::<PageSize>().unwrap().to_string(), "Letter");
        assert_eq!(
            "100x150".parse::<PageSize>().unwrap().to_string(),
            "100x150 mm"
        );
    }

    #[test]
    fn fitted_pages_take_the_image_size_at_its_resolution() {
        let placement = image_placement(300, 150, PageSize::Fit, Some(150.0));
        assert_eq!((placement.page_width, placement.page_height), (144.0, 72.0));
        assert_eq!((placement.x, placement.width), (0.0, 144.0));
        let unknown = image_placement(96, 96, PageSize::Fit, None);
        assert_eq!(unknown.page_width, 72.0);
    }

    #[test]
    fn info_strings_decode_pdf_doc_encoding_and_bom() {
        // 0xE9 is é in PDFDocEncoding, 0x8D a left double quotation mark.
        let literal = Object::String(
            vec![b'C', b'a', b'f', 0xE9, 0x8D],
            lopdf::StringFormat::Literal,
        );
        assert_eq!(pdf_text(&literal), "Caf\u{e9}\u{201c}");
        let utf8 = Object::String(
            "\u{feff}Привет".as_bytes().to_vec(),
            lopdf::StringFormat::Literal,
        );
        assert_eq!(pdf_text(&utf8), "Привет");
    }

    #[test]
    fn merge_keeps_form_fields_of_every_document() {
        let form_page = |name: &str| {
            let mut document = jpeg_document(sample_jpeg(3, 2), crate::pdf::PageSize::A4).unwrap();
            let page_id = *document.get_pages().get(&1).unwrap();
            let field = document.add_object(dictionary! {
                "Type" => "Annot",
                "Subtype" => "Widget",
                "FT" => "Tx",
                "T" => Object::string_literal(name),
                "Rect" => vec![0.into(), 0.into(), 10.into(), 10.into()],
                "P" => page_id,
            });
            document
                .get_object_mut(page_id)
                .unwrap()
                .as_dict_mut()
                .unwrap()
                .set("Annots", vec![Object::Reference(field)]);
            let form =
                document.add_object(dictionary! { "Fields" => vec![Object::Reference(field)] });
            document.catalog_mut().unwrap().set("AcroForm", form);
            document
                .catalog_mut()
                .unwrap()
                .set("PageLabels", dictionary! { "Nums" => vec![0.into()] });
            document
        };
        let mut merged = merge_documents(vec![form_page("first"), form_page("second")]).unwrap();
        let loaded = Document::load_mem(&save_to_bytes(&mut merged).unwrap()).unwrap();
        let catalog = loaded.catalog().unwrap();
        assert!(catalog.get(b"PageLabels").is_err());
        let form = loaded
            .get_dictionary(catalog.get(b"AcroForm").unwrap().as_reference().unwrap())
            .unwrap();
        let names = form
            .get(b"Fields")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|field| {
                let field = loaded
                    .get_dictionary(field.as_reference().unwrap())
                    .unwrap();
                pdf_text(field.get(b"T").unwrap())
            })
            .collect::<Vec<_>>();
        assert_eq!(names, ["first", "second"]);
    }

    #[test]
    fn pdf_strip_removes_info_catalog_and_page_metadata() {
        let mut document = jpeg_document(sample_jpeg(3, 2), crate::pdf::PageSize::A4).unwrap();
        let metadata_id = document.add_object(Stream::new(
            dictionary! {"Type" => "Metadata", "Subtype" => "XML"},
            b"<xmp>secret</xmp>".to_vec(),
        ));
        document.catalog_mut().unwrap().set("Metadata", metadata_id);
        let page_id = *document.get_pages().get(&1).unwrap();
        document
            .get_object_mut(page_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("PieceInfo", dictionary! {"Private" => "secret"});
        let info_id = document.add_object(dictionary! {
            "Title" => Object::string_literal("secret title"),
            "Producer" => Object::string_literal("secret producer"),
        });
        document.trailer.set("Info", info_id);

        let temporary = tempfile::NamedTempFile::new().unwrap();
        document.save(temporary.path()).unwrap();
        let stripped = transform_file(temporary.path(), |document| {
            strip_document_metadata(document);
            Ok(())
        })
        .unwrap();
        let clean = Document::load_mem(&stripped).unwrap();

        assert!(clean.catalog().unwrap().get(b"Metadata").is_err());
        let clean_page_id = *clean.get_pages().get(&1).unwrap();
        assert!(
            clean
                .get_dictionary(clean_page_id)
                .unwrap()
                .get(b"PieceInfo")
                .is_err()
        );
        assert!(clean.trailer.get(b"Info").is_err());
        assert!(!String::from_utf8_lossy(&stripped).contains("secret"));
    }
}
