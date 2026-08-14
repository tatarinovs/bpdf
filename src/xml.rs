use std::fs::File;
use std::io::Read;

use anyhow::{Context, Result};
use zip::ZipArchive;

/// Decode the five standard XML character entities plus `&nbsp;`.
pub fn decode_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
}

/// Strip all XML/HTML tags, returning only text content with entities decoded.
pub fn strip_tags(input: &str) -> String {
    let mut result = String::new();
    let mut inside = false;
    for c in input.chars() {
        if c == '<' {
            inside = true;
        } else if c == '>' {
            inside = false;
        } else if !inside {
            result.push(c);
        }
    }
    decode_entities(&result)
}

/// Read a named entry from a ZIP archive to `String`.
/// Normalises backslash paths and falls back to the original name.
pub fn read_zip_entry(archive: &mut ZipArchive<File>, name: &str) -> Result<String> {
    let normalized = name.replace('\\', "/");
    let lookup = if archive.by_name(&normalized).is_ok() {
        normalized
    } else {
        name.to_owned()
    };
    let mut entry = archive
        .by_name(&lookup)
        .with_context(|| format!("entry {name} not found in ZIP archive"))?;
    let mut content = String::new();
    entry.read_to_string(&mut content)?;
    Ok(content)
}

/// Collect text fragments enclosed between `open_tag` and `close_tag` in XML.
/// Each match starts at the first `open_tag`, skips to the closing `>` of that
/// tag (to handle attributes), then captures text up to `close_tag`.
pub fn collect_tag_texts(xml: &str, open_tag: &str, close_tag: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut cursor = xml;
    while let Some(start) = cursor.find(open_tag) {
        if let Some(gt) = cursor[start..].find('>') {
            let text_start = start + gt + 1;
            if let Some(end) = cursor[text_start..].find(close_tag) {
                let text = &cursor[text_start..text_start + end];
                let decoded = decode_entities(text);
                if !decoded.trim().is_empty() {
                    result.push(decoded);
                }
                cursor = &cursor[text_start + end + close_tag.len()..];
                continue;
            }
        }
        break;
    }
    result
}

/// List ZIP entries whose names start with `prefix` and end with `suffix`,
/// sorted in natural order.
pub fn collect_sorted_entries(
    archive: &mut ZipArchive<File>,
    prefix: &str,
    suffix: &str,
) -> Vec<String> {
    let mut names = Vec::new();
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            let name = entry.name().to_owned();
            if name.starts_with(prefix) && name.ends_with(suffix) {
                names.push(name);
            }
        }
    }
    names.sort_by(|a, b| crate::fileset::natural_compare(a, b));
    names
}
