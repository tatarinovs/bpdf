//! Merging page trees, with the outlines, named destinations and form fields
//! of every source document.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, Result};
use lopdf::{Dictionary, Document, Object, ObjectId, StringFormat, dictionary};

use super::resolve;
use super::transform::{self, info_string};

/// Merge without flattening leaf pages. Each source Pages root becomes a child
/// of a new Pages root, preserving inherited resources, boxes and rotation.
pub fn merge_documents(mut documents: Vec<Document>) -> Result<Document> {
    if documents.len() <= 1 {
        return documents.pop().context("no PDF documents to merge");
    }
    merge_outlined(
        documents
            .into_iter()
            .map(|document| (document, None))
            .collect(),
    )
}

/// Merge like [`merge_documents`]. A source with a title gets a top-level
/// bookmark on its first page holding the source's own outline; the outline
/// items of the others follow at the top level.
pub fn merge_outlined(sources: Vec<(Document, Option<String>)>) -> Result<Document> {
    let pages_id: ObjectId = (1, 0);
    let catalog_id: ObjectId = (2, 0);
    let mut result = Document::with_version("1.7");
    result.max_id = 2;

    let mut next_id = 3u32;
    let mut page_roots = Vec::with_capacity(sources.len());
    let mut total_pages = 0usize;
    let mut first_catalog: Option<Dictionary> = None;
    let mut first_info: Option<Object> = None;
    let mut first_form: Option<Dictionary> = None;
    let mut all_fields = Vec::new();
    let mut field_names = HashSet::new();
    // PDF 1.1 `Dests` dictionary (names) and the `Dests` name tree (strings).
    let mut legacy_destinations = BTreeMap::<Vec<u8>, Object>::new();
    let mut destinations = BTreeMap::<Vec<u8>, Object>::new();
    let mut outlines = Vec::new();

    for (index, (mut document, title)) in sources.into_iter().enumerate() {
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

        let pages = document.get_pages();
        total_pages += pages.len();
        document
            .get_object_mut(source_pages_id)?
            .as_dict_mut()?
            .set("Parent", pages_id);
        page_roots.push(Object::Reference(source_pages_id));

        // Form fields of every document join one AcroForm; the first
        // document's form settings (default appearance, resources) stay.
        let form = catalog
            .get(b"AcroForm")
            .ok()
            .and_then(|form| transform::resolve_dictionary(&document, form));
        if let Some(form) = &form
            && let Some(mut fields) = form
                .get(b"Fields")
                .ok()
                .and_then(|fields| resolve(&document, fields).ok()?.as_array().ok())
                .cloned()
        {
            // Fields of one name share one value: a second copy of a form
            // must not fill in the first.
            for field in &mut fields {
                rename_field(&mut document, field, &mut field_names, index + 1);
            }
            all_fields.extend(fields);
        }
        if let Some(form) = form {
            first_form.get_or_insert(form);
        }

        // A name another document already uses gets a suffix, and this
        // document's links follow it.
        let (legacy, tree) = named_destinations(&document, &catalog);
        let renamed = Renamed {
            names: add_unique(&mut legacy_destinations, legacy, index + 1),
            strings: add_unique(&mut destinations, tree, index + 1),
        };
        if !renamed.names.is_empty() || !renamed.strings.is_empty() {
            for object in document.objects.values_mut() {
                rename_destination_links(object, &renamed);
            }
        }

        outlines.push(SourceOutline {
            title,
            first_page: pages.values().next().copied(),
            items: top_level_items(&document, &catalog),
        });

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
    // Labels of the first document would number every later page too.
    catalog.remove(b"PageLabels");
    catalog.remove(b"AcroForm");
    catalog.remove(b"Outlines");
    catalog.remove(b"Dests");
    if let Some(mut form) = first_form {
        // An XFA form describes only the first document's fields.
        form.remove(b"XFA");
        form.set("Fields", all_fields);
        let form_id = (next_id, 0);
        next_id += 1;
        result.objects.insert(form_id, Object::Dictionary(form));
        catalog.set("AcroForm", form_id);
    }
    let mut names = catalog
        .get(b"Names")
        .ok()
        .and_then(|names| transform::resolve_dictionary(&result, names))
        .unwrap_or_default();
    names.remove(b"Dests");
    if !destinations.is_empty() {
        let flat = destinations
            .into_iter()
            .flat_map(|(name, destination)| {
                [Object::String(name, StringFormat::Literal), destination]
            })
            .collect::<Vec<_>>();
        names.set("Dests", dictionary! { "Names" => flat });
    }
    if names.is_empty() {
        catalog.remove(b"Names");
    } else {
        catalog.set("Names", names);
    }
    if !legacy_destinations.is_empty() {
        catalog.set("Dests", Dictionary::from_iter(legacy_destinations));
    }
    if let Some(outline_id) = build_outline(&mut result, &mut next_id, outlines)? {
        catalog.set("Outlines", outline_id);
    }
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

/// The outline of one source: its top-level items and, with a title, the
/// bookmark that will hold them.
struct SourceOutline {
    title: Option<String>,
    first_page: Option<ObjectId>,
    items: Vec<ObjectId>,
}

/// Top-level outline items of a document, following `First` and `Next`.
fn top_level_items(document: &Document, catalog: &Dictionary) -> Vec<ObjectId> {
    let Some(root) = catalog
        .get(b"Outlines")
        .ok()
        .and_then(|outlines| transform::resolve_dictionary(document, outlines))
    else {
        return Vec::new();
    };
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    let mut current = root.get(b"First").and_then(Object::as_reference).ok();
    while let Some(id) = current {
        if !seen.insert(id) || document.get_dictionary(id).is_err() {
            break;
        }
        items.push(id);
        current = document
            .get_dictionary(id)
            .and_then(|item| item.get(b"Next"))
            .and_then(Object::as_reference)
            .ok();
    }
    items
}

/// One outline with a (closed) bookmark per titled source and the items of
/// the other sources between them.
fn build_outline(
    document: &mut Document,
    next_id: &mut u32,
    sources: Vec<SourceOutline>,
) -> Result<Option<ObjectId>> {
    let root_id = (*next_id, 0);
    *next_id += 1;
    let mut top = Vec::new();
    for source in sources {
        match (source.title, source.first_page) {
            (Some(title), Some(page)) => {
                let id = (*next_id, 0);
                *next_id += 1;
                let mut bookmark = dictionary! {
                    "Title" => info_string(&title),
                    "Dest" => vec![Object::Reference(page), Object::Name(b"Fit".to_vec())],
                };
                if let (Some(first), Some(last)) = (source.items.first(), source.items.last()) {
                    bookmark.set("First", *first);
                    bookmark.set("Last", *last);
                    bookmark.set("Count", -(source.items.len() as i64));
                    link_siblings(document, &source.items, id)?;
                }
                document.objects.insert(id, Object::Dictionary(bookmark));
                top.push(id);
            }
            _ => top.extend(source.items),
        }
    }
    if top.is_empty() {
        return Ok(None);
    }
    link_siblings(document, &top, root_id)?;
    document.objects.insert(
        root_id,
        Object::Dictionary(dictionary! {
            "Type" => "Outlines",
            "First" => top[0],
            "Last" => top[top.len() - 1],
            "Count" => top.len() as i64,
        }),
    );
    Ok(Some(root_id))
}

/// Make `items` one chain of siblings under `parent`.
fn link_siblings(document: &mut Document, items: &[ObjectId], parent: ObjectId) -> Result<()> {
    for (index, id) in items.iter().enumerate() {
        let item = document.get_object_mut(*id)?.as_dict_mut()?;
        item.set("Parent", parent);
        match index.checked_sub(1) {
            Some(previous) => item.set("Prev", items[previous]),
            None => {
                item.remove(b"Prev");
            }
        }
        match items.get(index + 1) {
            Some(next) => item.set("Next", *next),
            None => {
                item.remove(b"Next");
            }
        }
    }
    Ok(())
}

/// Named destinations of a document: those of the PDF 1.1 `Dests`
/// dictionary (named by name objects) and those of the `Dests` name tree
/// (named by strings).
fn named_destinations(document: &Document, catalog: &Dictionary) -> (Destinations, Destinations) {
    let legacy = catalog
        .get(b"Dests")
        .ok()
        .and_then(|dests| transform::resolve_dictionary(document, dests))
        .map(|dests| {
            dests
                .iter()
                .map(|(name, destination)| (name.clone(), destination.clone()))
                .collect()
        })
        .unwrap_or_default();
    let mut tree = Vec::new();
    if let Some(root) = catalog
        .get(b"Names")
        .ok()
        .and_then(|names| transform::resolve_dictionary(document, names))
        .and_then(|names| transform::resolve_dictionary(document, names.get(b"Dests").ok()?))
    {
        collect_name_tree(document, &root, 0, &mut tree);
    }
    (legacy, tree)
}

type Destinations = Vec<(Vec<u8>, Object)>;

/// Add `found` to `all`. A name already taken gets a suffix (`_2`, ...,
/// counted from `first_suffix`); the returned map holds the renames.
fn add_unique(
    all: &mut BTreeMap<Vec<u8>, Object>,
    found: Destinations,
    first_suffix: usize,
) -> HashMap<Vec<u8>, Vec<u8>> {
    let mut renamed = HashMap::new();
    for (name, destination) in found {
        let mut unique = name.clone();
        let mut counter = first_suffix;
        while all.contains_key(&unique) {
            unique = [name.as_slice(), format!("_{counter}").as_bytes()].concat();
            counter += 1;
        }
        if unique != name {
            renamed.insert(name, unique.clone());
        }
        all.insert(unique, destination);
    }
    renamed
}

/// Give a top-level form field (an entry of `Fields`) a name no earlier
/// document used, and remember it.
fn rename_field(
    document: &mut Document,
    field: &mut Object,
    taken: &mut HashSet<String>,
    first_suffix: usize,
) {
    let field = match field {
        Object::Reference(id) => match document.get_object_mut(*id).and_then(Object::as_dict_mut) {
            Ok(field) => field,
            Err(_) => return,
        },
        Object::Dictionary(field) => field,
        _ => return,
    };
    let Ok(name) = field.get(b"T").map(super::pdf_text) else {
        return;
    };
    let mut unique = name.clone();
    let mut counter = first_suffix;
    while taken.contains(&unique) {
        unique = format!("{name}_{counter}");
        counter += 1;
    }
    if unique != name {
        field.set("T", info_string(&unique));
    }
    taken.insert(unique);
}

fn collect_name_tree(
    document: &Document,
    node: &Dictionary,
    depth: u8,
    found: &mut Vec<(Vec<u8>, Object)>,
) {
    if depth > 32 {
        return;
    }
    if let Some(pairs) = node
        .get(b"Names")
        .ok()
        .and_then(|names| resolve(document, names).ok()?.as_array().ok())
    {
        for pair in pairs.chunks_exact(2) {
            if let Ok(name) = pair[0].as_str() {
                found.push((name.to_vec(), pair[1].clone()));
            }
        }
    }
    if let Some(kids) = node
        .get(b"Kids")
        .ok()
        .and_then(|kids| resolve(document, kids).ok()?.as_array().ok())
    {
        for kid in kids {
            if let Some(kid) = transform::resolve_dictionary(document, kid) {
                collect_name_tree(document, &kid, depth + 1, found);
            }
        }
    }
}

/// Renamed destinations of one document: names of the `Dests` dictionary
/// and strings of the name tree.
struct Renamed {
    names: HashMap<Vec<u8>, Vec<u8>>,
    strings: HashMap<Vec<u8>, Vec<u8>>,
}

/// Point `Dest` entries and GoTo actions that name a renamed destination at
/// its new name.
fn rename_destination_links(object: &mut Object, renamed: &Renamed) {
    let rename = |value: &mut Object| {
        let (name, map) = match value {
            Object::Name(name) => (name, &renamed.names),
            Object::String(name, _) => (name, &renamed.strings),
            _ => return,
        };
        if let Some(new) = map.get(name.as_slice()) {
            *name = new.clone();
        }
    };
    match object {
        Object::Dictionary(dictionary) => {
            let is_goto = dictionary
                .get(b"S")
                .and_then(Object::as_name)
                .is_ok_and(|kind| kind == b"GoTo");
            for (key, value) in dictionary.iter_mut() {
                if key == b"Dest" || (is_goto && key == b"D") {
                    rename(value);
                } else {
                    rename_destination_links(value, renamed);
                }
            }
        }
        Object::Array(items) => items
            .iter_mut()
            .for_each(|item| rename_destination_links(item, renamed)),
        Object::Stream(stream) => stream
            .dict
            .iter_mut()
            .for_each(|(_, value)| rename_destination_links(value, renamed)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::common::test_utils::sample_jpeg_bytes as sample_jpeg;
    use crate::pdf::{PageSize, jpeg_document, pdf_text, save_to_bytes};

    /// A two-page document with an outline item per page and a named
    /// destination `start` that a link annotation on page 2 uses.
    fn outlined(label: &str) -> Document {
        let page = || jpeg_document(sample_jpeg(3, 2), PageSize::A4).unwrap();
        let mut document = merge_documents(vec![page(), page()]).unwrap();
        let pages = document.get_pages().into_values().collect::<Vec<_>>();
        let root = document.new_object_id();
        let items = pages
            .iter()
            .enumerate()
            .map(|(index, page)| {
                document.add_object(dictionary! {
                    "Title" => Object::string_literal(format!("{label} {}", index + 1)),
                    "Parent" => root,
                    "Dest" => vec![Object::Reference(*page), Object::Name(b"Fit".to_vec())],
                })
            })
            .collect::<Vec<_>>();
        link_siblings(&mut document, &items, root).unwrap();
        document.objects.insert(
            root,
            Object::Dictionary(dictionary! {
                "Type" => "Outlines", "First" => items[0], "Last" => items[1], "Count" => 2,
            }),
        );
        let link = document.add_object(dictionary! {
            "Type" => "Annot", "Subtype" => "Link",
            "Rect" => vec![0.into(), 0.into(), 10.into(), 10.into()],
            "Dest" => Object::string_literal("start"),
        });
        document
            .get_object_mut(pages[1])
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Annots", vec![Object::Reference(link)]);
        let destination = vec![Object::Reference(pages[0]), Object::Name(b"Fit".to_vec())];
        let catalog = document.catalog_mut().unwrap();
        catalog.set("Outlines", root);
        catalog.set(
            "Names",
            dictionary! { "Dests" => dictionary! {
                "Names" => vec![Object::string_literal("start"), destination.into()],
            }},
        );
        document
    }

    fn reload(document: &mut Document) -> Document {
        Document::load_mem(&save_to_bytes(document).unwrap()).unwrap()
    }

    fn titles(document: &Document, parent: &Dictionary) -> Vec<String> {
        let mut titles = Vec::new();
        let mut current = parent.get(b"First").and_then(Object::as_reference).ok();
        while let Some(id) = current {
            let item = document.get_dictionary(id).unwrap();
            titles.push(pdf_text(item.get(b"Title").unwrap()));
            current = item.get(b"Next").and_then(Object::as_reference).ok();
        }
        titles
    }

    fn outline_root(document: &Document) -> Dictionary {
        let catalog = document.catalog().unwrap();
        transform::resolve_dictionary(document, catalog.get(b"Outlines").unwrap()).unwrap()
    }

    #[test]
    fn source_outlines_nest_under_file_bookmarks() {
        let mut merged = merge_outlined(vec![
            (outlined("a"), Some("first".to_owned())),
            (outlined("b"), Some("второй".to_owned())),
        ])
        .unwrap();
        let merged = reload(&mut merged);
        let root = outline_root(&merged);
        assert_eq!(titles(&merged, &root), ["first", "второй"]);
        let second_id = root.get(b"Last").unwrap().as_reference().unwrap();
        let second = merged.get_dictionary(second_id).unwrap();
        assert_eq!(titles(&merged, second), ["b 1", "b 2"]);
        assert_eq!(second.get(b"Count").unwrap().as_i64().unwrap(), -2);
        // The bookmark opens the first page of its document.
        let third_page = *merged.get_pages().get(&3).unwrap();
        let dest = second.get(b"Dest").unwrap().as_array().unwrap();
        assert_eq!(dest[0].as_reference().unwrap(), third_page);
    }

    #[test]
    fn untitled_outlines_follow_each_other() {
        let mut merged = merge_documents(vec![outlined("a"), outlined("b")]).unwrap();
        let merged = reload(&mut merged);
        let root = outline_root(&merged);
        assert_eq!(titles(&merged, &root), ["a 1", "a 2", "b 1", "b 2"]);
    }

    #[test]
    fn legacy_destinations_stay_names_and_clashing_fields_are_renamed() {
        let form_copy = || {
            let mut document = jpeg_document(sample_jpeg(3, 2), PageSize::A4).unwrap();
            let page = *document.get_pages().get(&1).unwrap();
            let field = document.add_object(dictionary! {
                "FT" => "Tx", "T" => Object::string_literal("name"),
            });
            let form =
                document.add_object(dictionary! { "Fields" => vec![Object::Reference(field)] });
            let catalog = document.catalog_mut().unwrap();
            catalog.set(
                "Dests",
                dictionary! { "start" => vec![Object::Reference(page), Object::Name(b"Fit".to_vec())] },
            );
            catalog.set("AcroForm", form);
            document
        };
        let mut merged = merge_documents(vec![form_copy(), form_copy()]).unwrap();
        let merged = reload(&mut merged);
        let catalog = merged.catalog().unwrap();
        let dests = transform::resolve_dictionary(&merged, catalog.get(b"Dests").unwrap()).unwrap();
        let keys = dests
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        assert_eq!(keys, [b"start".to_vec(), b"start_2".to_vec()]);
        let form =
            transform::resolve_dictionary(&merged, catalog.get(b"AcroForm").unwrap()).unwrap();
        let names = form
            .get(b"Fields")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|field| {
                let field = merged
                    .get_dictionary(field.as_reference().unwrap())
                    .unwrap();
                pdf_text(field.get(b"T").unwrap())
            })
            .collect::<Vec<_>>();
        assert_eq!(names, ["name", "name_2"]);
    }

    #[test]
    fn clashing_named_destinations_are_renamed_with_their_links() {
        let mut merged = merge_documents(vec![outlined("a"), outlined("b")]).unwrap();
        let merged = reload(&mut merged);
        let catalog = merged.catalog().unwrap();
        let names = transform::resolve_dictionary(&merged, catalog.get(b"Names").unwrap()).unwrap();
        let dests = transform::resolve_dictionary(&merged, names.get(b"Dests").unwrap()).unwrap();
        let pairs = dests.get(b"Names").unwrap().as_array().unwrap();
        let keys = pairs
            .chunks_exact(2)
            .map(|pair| pdf_text(&pair[0]))
            .collect::<Vec<_>>();
        assert_eq!(keys, ["start", "start_2"]);
        // The link of the second document now names its own destination.
        let page_four = *merged.get_pages().get(&4).unwrap();
        let annots = merged
            .get_dictionary(page_four)
            .unwrap()
            .get(b"Annots")
            .unwrap()
            .as_array()
            .unwrap();
        let link = merged
            .get_dictionary(annots[0].as_reference().unwrap())
            .unwrap();
        assert_eq!(pdf_text(link.get(b"Dest").unwrap()), "start_2");
        let target = pairs[3].as_array().unwrap()[0].as_reference().unwrap();
        assert_eq!(target, *merged.get_pages().get(&3).unwrap());
    }
}
