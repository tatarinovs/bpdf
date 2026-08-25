use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use ::image::codecs::jpeg::JpegDecoder;
use ::image::{ColorType, ExtendedColorType, ImageDecoder};
use anyhow::{Context, Result, bail};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};
use serde_json::{Value, json};

use crate::atomic::write_atomic;
pub(crate) mod image;
pub mod transform;

pub const MAX_DECOMPRESSED_BYTES: usize = 256 * 1024 * 1024;

pub fn load(path: &Path) -> Result<Document> {
    Document::load(path).with_context(|| format!("failed to load PDF {}", path.display()))
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

/// Merge without flattening leaf pages. Each source Pages root becomes a child
/// of a new Pages root, preserving inherited resources, boxes and rotation.
pub fn merge_documents(mut documents: Vec<Document>) -> Result<Document> {
    if documents.is_empty() {
        bail!("no PDF documents to merge");
    }

    let pages_id: ObjectId = (1, 0);
    let catalog_id: ObjectId = (2, 0);
    let mut result = Document::with_version("1.7");
    result.max_id = 2;

    let mut next_id = 3u32;
    let mut page_roots = Vec::with_capacity(documents.len());
    let mut total_pages = 0usize;
    let mut first_catalog: Option<Dictionary> = None;
    let mut first_info: Option<Object> = None;

    for document in &mut documents {
        document.renumber_objects_with(next_id);
        next_id = document.max_id + 1;

        let root_id = document
            .trailer
            .get(b"Root")
            .context("PDF trailer has no Root")?
            .as_reference()
            .context("PDF Root is not an indirect object")?;
        let catalog = document
            .get_dictionary(root_id)
            .context("failed to read PDF catalog")?
            .clone();
        let source_pages_id = catalog
            .get(b"Pages")
            .context("PDF catalog has no Pages tree")?
            .as_reference()
            .context("PDF Pages root is not an indirect object")?;

        total_pages += document.get_pages().len();
        document
            .get_object_mut(source_pages_id)?
            .as_dict_mut()?
            .set("Parent", pages_id);
        page_roots.push(Object::Reference(source_pages_id));

        if first_catalog.is_none() {
            first_catalog = Some(catalog);
            first_info = document.trailer.get(b"Info").ok().cloned();
        }

        result.objects.extend(std::mem::take(&mut document.objects));
    }

    result.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => page_roots,
            "Count" => total_pages as i64,
        }),
    );

    let mut catalog = first_catalog.unwrap_or_default();
    catalog.set("Type", "Catalog");
    catalog.set("Pages", pages_id);
    result
        .objects
        .insert(catalog_id, Object::Dictionary(catalog));
    result.trailer.set("Root", catalog_id);
    if let Some(info) = first_info {
        result.trailer.set("Info", info);
    }

    result.max_id = next_id.saturating_sub(1).max(2);
    result.prune_objects();
    result.renumber_objects();
    Ok(result)
}

pub fn inspect_file(path: &Path, include_text: bool) -> Result<String> {
    let document = load(path)?;
    let pages = document.get_pages();
    let mut report = format!(
        "PDF version: {}\nPages: {}\n",
        document.version,
        pages.len()
    );

    for (number, page_id) in &pages {
        let fonts = document
            .get_page_fonts(*page_id)
            .map(|items| items.len())
            .unwrap_or_default();
        let images = document
            .get_page_images(*page_id)
            .map(|items| items.len())
            .unwrap_or_default();
        report.push_str(&format!("Page {number}: fonts={fonts}, images={images}\n"));
    }

    if include_text {
        let text = extract_text(&document)?;
        report.push_str("\n--- text ---\n");
        report.push_str(&text);
        if !text.ends_with('\n') {
            report.push('\n');
        }
    }

    Ok(report)
}

pub fn metadata_report(path: &Path) -> Result<Value> {
    let document = load(path)?;
    let mut fields = serde_json::Map::new();
    if let Ok(info_object) = document.trailer.get(b"Info") {
        let info = match info_object {
            Object::Reference(id) => document.get_dictionary(*id).ok(),
            Object::Dictionary(dictionary) => Some(dictionary),
            _ => None,
        };
        if let Some(info) = info {
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
                    fields.insert(key.to_owned(), Value::String(pdf_text(value)));
                }
            }
        }
    }

    let has_xmp = document
        .catalog()
        .ok()
        .is_some_and(|catalog| catalog.get(b"Metadata").is_ok());
    let page_metadata = document
        .get_pages()
        .into_values()
        .filter(|page_id| {
            document
                .get_dictionary(*page_id)
                .ok()
                .is_some_and(|page| page.get(b"Metadata").is_ok())
        })
        .count();
    Ok(json!({
        "path": path.to_string_lossy(),
        "fields": fields,
        "xmp": has_xmp,
        "pages_with_metadata": page_metadata,
    }))
}

pub fn split_file(path: &Path, output_dir: &Path) -> Result<Vec<PathBuf>> {
    let document = load(path)?;
    let page_count = document.get_pages().len();
    if page_count == 0 {
        bail!("PDF contains no pages");
    }
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("page");
    let width = page_count.to_string().len().max(1);
    let mut outputs = Vec::with_capacity(page_count);

    for page in 1..=page_count {
        // lopdf::Document owns its complete object graph, so Clone gives each
        // output an isolated page tree without parsing the source N times.
        let mut one_page = document.clone();
        select_pages(&mut one_page, &page.to_string())?;
        let bytes = save_to_bytes(&mut one_page)?;
        let output = output_dir.join(format!("{stem}_{page:0width$}.pdf"));
        write_atomic(&output, &bytes)?;
        outputs.push(output);
    }

    Ok(outputs)
}

pub fn strip_document_metadata(document: &mut Document) {
    if let Ok(root_id) = document.trailer.get(b"Root").and_then(Object::as_reference)
        && let Ok(catalog) = document
            .get_object_mut(root_id)
            .and_then(Object::as_dict_mut)
    {
        remove_keys(catalog, &[b"Metadata", b"PieceInfo"]);
    }

    let page_ids = document.get_pages().into_values().collect::<Vec<_>>();
    for page_id in page_ids {
        if let Ok(page) = document
            .get_object_mut(page_id)
            .and_then(Object::as_dict_mut)
        {
            remove_keys(page, &[b"Metadata", b"PieceInfo"]);
        }
    }

    if let Ok(info_id) = document.trailer.get(b"Info").and_then(Object::as_reference)
        && let Ok(info) = document
            .get_object_mut(info_id)
            .and_then(Object::as_dict_mut)
    {
        remove_keys(
            info,
            &[
                b"Title",
                b"Author",
                b"Subject",
                b"Keywords",
                b"Creator",
                b"Producer",
                b"CreationDate",
                b"ModDate",
                b"Trapped",
            ],
        );
    }

    document.prune_objects();
    document.renumber_objects();
}

pub fn save_to_bytes(document: &mut Document) -> Result<Vec<u8>> {
    document.prune_objects();
    document.renumber_objects();
    document.compress();
    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .context("failed to serialize PDF")?;
    Ok(bytes)
}

pub(crate) fn select_pages(document: &mut Document, expression: &str) -> Result<()> {
    let page_count = document.get_pages().len();
    if page_count == 0 {
        bail!("PDF contains no pages");
    }

    let selected = parse_page_selection(expression, page_count)?;
    let to_delete = (1..=page_count)
        .filter(|page| !selected.contains(page))
        .map(|page| page as u32)
        .collect::<Vec<_>>();
    document.delete_pages(&to_delete);
    document.prune_objects();

    let remaining = document.get_pages().len();
    if remaining != selected.len() {
        bail!(
            "page selection produced {remaining} pages, expected {}",
            selected.len()
        );
    }
    Ok(())
}

fn parse_page_selection(expression: &str, page_count: usize) -> Result<BTreeSet<usize>> {
    let expression = expression.trim().to_ascii_lowercase();
    if expression == "all" {
        return Ok((1..=page_count).collect());
    }
    if expression == "first" {
        return Ok(BTreeSet::from([1]));
    }
    if expression == "even" {
        return Ok((1..=page_count).filter(|page| page % 2 == 0).collect());
    }
    if expression == "odd" {
        return Ok((1..=page_count).filter(|page| page % 2 == 1).collect());
    }

    let mut selected = BTreeSet::new();
    for part in expression.split(',').map(str::trim) {
        if part.is_empty() {
            bail!("empty page selection");
        }
        if part == "first" {
            selected.insert(1);
            continue;
        }
        if part == "l" || part == "last" {
            selected.insert(page_count);
            continue;
        }

        if let Some((start, end)) = part.split_once('-') {
            let start = parse_page_number(start, page_count)?;
            let end = parse_page_number(end, page_count)?;
            if start > end {
                bail!("descending page range {part} is not supported");
            }
            selected.extend(start..=end);
        } else {
            selected.insert(parse_page_number(part, page_count)?);
        }
    }

    if selected.is_empty() {
        bail!("no pages selected");
    }
    Ok(selected)
}

fn parse_page_number(value: &str, page_count: usize) -> Result<usize> {
    if value == "l" || value == "last" {
        return Ok(page_count);
    }
    let page = value
        .parse::<usize>()
        .with_context(|| format!("invalid page number {value}"))?;
    if page == 0 || page > page_count {
        bail!("page {page} is outside 1-{page_count}");
    }
    Ok(page)
}

pub(crate) fn jpeg_document(jpeg: Vec<u8>, page_size: &str) -> Result<Document> {
    let info = jpeg_info(&jpeg)?;
    let (mut page_width, mut page_height) = paper_size(page_size)?;
    if (info.width > info.height) != (page_width > page_height) {
        std::mem::swap(&mut page_width, &mut page_height);
    }

    let scale = (page_width / f64::from(info.width)).min(page_height / f64::from(info.height));
    let image_width = f64::from(info.width) * scale;
    let image_height = f64::from(info.height) * scale;
    let offset_x = (page_width - image_width) / 2.0;
    let offset_y = (page_height - image_height) / 2.0;

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

    let content = format!(
        "q\n{image_width:.6} 0 0 {image_height:.6} {offset_x:.6} {offset_y:.6} cm\n/Im0 Do\nQ\n"
    );
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

pub(crate) fn paper_size(name: &str) -> Result<(f64, f64)> {
    match name.trim().to_ascii_lowercase().as_str() {
        "a4" | "none" | "original" | "keep" => Ok((595.275_590_551, 841.889_763_78)),
        "letter" => Ok((612.0, 792.0)),
        other => bail!("unsupported image page size {other}; use A4 or Letter"),
    }
}

pub(super) fn object_number(object: &Object) -> Result<f64> {
    match object {
        Object::Integer(value) => Ok(*value as f64),
        Object::Real(value) => Ok(f64::from(*value)),
        _ => bail!("expected PDF number"),
    }
}

#[derive(Debug)]
struct JpegInfo {
    width: u16,
    height: u16,
    components: u8,
}

fn jpeg_info(bytes: &[u8]) -> Result<JpegInfo> {
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

fn remove_keys(dictionary: &mut Dictionary, keys: &[&[u8]]) {
    for key in keys {
        dictionary.remove(key);
    }
}

fn pdf_text(object: &Object) -> String {
    match object {
        Object::String(bytes, _) if bytes.starts_with(&[0xfe, 0xff]) => {
            let units = bytes[2..]
                .chunks_exact(2)
                .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
                .collect::<Vec<_>>();
            String::from_utf16_lossy(&units)
        }
        Object::String(bytes, _) | Object::Name(bytes) => String::from_utf8_lossy(bytes).into(),
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
    fn creates_valid_image_pdf() {
        let mut document = jpeg_document(sample_jpeg(3, 2), "A4").unwrap();
        let bytes = save_to_bytes(&mut document).unwrap();
        let loaded = Document::load_mem(&bytes).unwrap();
        assert_eq!(loaded.get_pages().len(), 1);
    }

    #[test]
    fn merged_page_trees_keep_all_pages() {
        let first = jpeg_document(sample_jpeg(3, 2), "A4").unwrap();
        let second = jpeg_document(sample_jpeg(2, 3), "A4").unwrap();
        let mut merged = merge_documents(vec![first, second]).unwrap();
        let bytes = save_to_bytes(&mut merged).unwrap();
        let loaded = Document::load_mem(&bytes).unwrap();
        assert_eq!(loaded.get_pages().len(), 2);
    }

    #[test]
    fn page_selection_removes_unselected_page_tree_branches() {
        let first = jpeg_document(sample_jpeg(3, 2), "A4").unwrap();
        let second = jpeg_document(sample_jpeg(2, 3), "A4").unwrap();
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
        let first = jpeg_document(sample_jpeg(3, 2), "A4").unwrap();
        let second = jpeg_document(sample_jpeg(2, 3), "A4").unwrap();
        let mut merged = merge_documents(vec![first, second]).unwrap();
        let source = tempfile::Builder::new().suffix(".pdf").tempfile().unwrap();
        merged.save(source.path()).unwrap();
        let output = tempfile::tempdir().unwrap();

        let files = split_file(source.path(), output.path()).unwrap();

        assert_eq!(files.len(), 2);
        for file in files {
            let split = load(&file).unwrap();
            assert_eq!(split.get_pages().len(), 1);
            let page_id = *split.get_pages().get(&1).unwrap();
            assert_eq!(split.get_page_images(page_id).unwrap().len(), 1);
        }
    }

    #[test]
    fn pdf_strip_removes_info_catalog_and_page_metadata() {
        let mut document = jpeg_document(sample_jpeg(3, 2), "A4").unwrap();
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
        let clean_info_id = clean.trailer.get(b"Info").unwrap().as_reference().unwrap();
        let clean_info = clean.get_dictionary(clean_info_id).unwrap();
        assert!(clean_info.get(b"Title").is_err());
        assert!(clean_info.get(b"Producer").is_err());
    }
}
