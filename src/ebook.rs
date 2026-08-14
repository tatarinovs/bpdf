use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use zip::ZipArchive;

use crate::xml;

/// Reads an ebook (EPUB, FB2, FB2.ZIP, or HTMLZ) and returns its contents formatted as clean text.
pub fn load(path: &Path) -> Result<String> {
    let path_str = path.to_string_lossy();
    if path_str.to_ascii_lowercase().ends_with(".fb2.zip") {
        return load_fb2_zip(path);
    }

    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        match ext.to_ascii_lowercase().as_str() {
            "epub" => return load_epub(path),
            "fb2" => return load_fb2_raw(path),
            "htmlz" => return load_htmlz(path),
            "zip" => {
                if let Ok(text) = load_epub(path) {
                    return Ok(text);
                }
                if let Ok(text) = load_fb2_zip(path) {
                    return Ok(text);
                }
                if let Ok(text) = load_htmlz(path) {
                    return Ok(text);
                }
            }
            _ => {}
        }
    }

    // Fallback: try raw FB2 XML
    load_fb2_raw(path)
}

/// Load `.htmlz` archive containing an `index.html` or html file.
pub fn load_htmlz(path: &Path) -> Result<String> {
    let file = File::open(path)
        .with_context(|| format!("failed to open HTMLZ file {}", path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("failed to read ZIP archive {}", path.display()))?;

    let mut index_file = None;
    for i in 0..archive.len() {
        let Ok(entry) = archive.by_index(i) else { continue; };
        let name = entry.name().to_ascii_lowercase();
        if name == "index.html" || name == "index.htm" {
            index_file = Some(entry.name().to_owned());
            break;
        }
        if index_file.is_none() && (name.ends_with(".html") || name.ends_with(".htm")) {
            index_file = Some(entry.name().to_owned());
        }
    }

    let target_name = index_file.with_context(|| format!("no HTML file found in HTMLZ archive {}", path.display()))?;
    let html_content = xml::read_zip_entry(&mut archive, &target_name)?;
    Ok(convert_html_to_text(&html_content))
}

/// Load raw `.fb2` XML document.
fn load_fb2_raw(path: &Path) -> Result<String> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read FB2 file {}", path.display()))?;
    parse_fb2_xml(&content)
}

/// Load `.fb2.zip` archive containing an `.fb2` or `.xml` file.
fn load_fb2_zip(path: &Path) -> Result<String> {
    let file = File::open(path)
        .with_context(|| format!("failed to open FB2 archive {}", path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("failed to read ZIP structure in {}", path.display()))?;

    let mut target_index = None;
    for i in 0..archive.len() {
        let entry = archive.by_index(i)?;
        let name = entry.name().to_ascii_lowercase();
        if name.ends_with(".fb2") || name.ends_with(".xml") {
            target_index = Some(i);
            break;
        }
    }

    let index = target_index.unwrap_or(0);
    let mut zip_file = archive.by_index(index)?;
    let mut content = String::new();
    zip_file
        .read_to_string(&mut content)
        .with_context(|| format!("failed to read inner FB2 file from {}", path.display()))?;

    parse_fb2_xml(&content)
}

/// Parses FictionBook 2 (FB2) XML into readable text.
fn parse_fb2_xml(xml: &str) -> Result<String> {
    let mut output = String::new();

    // Extract book title & author if present in <title-info>
    if let Some(title) = extract_tag_value(xml, "book-title") {
        let clean_title = xml::strip_tags(&title);
        if !clean_title.trim().is_empty() {
            output.push_str(&clean_title);
            output.push('\n');
            output.push_str(&"=".repeat(clean_title.trim().chars().count().max(4)));
            output.push_str("\n\n");
        }
    }

    if let Some(authors) = extract_fb2_authors(xml) {
        if !authors.is_empty() {
            output.push_str("Author: ");
            output.push_str(&authors);
            output.push_str("\n\n");
        }
    }

    // Extract annotation if present
    if let Some(annotation_xml) = extract_tag_value(xml, "annotation") {
        let text = parse_fb2_text_block(&annotation_xml);
        if !text.trim().is_empty() {
            output.push_str("Annotation:\n");
            output.push_str(&text);
            output.push_str("\n\n---\n\n");
        }
    }

    // Extract body content
    if let Some(body_xml) = extract_tag_value(xml, "body") {
        let text = parse_fb2_text_block(&body_xml);
        output.push_str(&text);
    } else {
        // Fallback: parse entire XML as text block
        let text = parse_fb2_text_block(xml);
        output.push_str(&text);
    }

    if output.trim().is_empty() {
        bail!("FB2 file did not contain extractable text");
    }

    Ok(output)
}

/// Parses text elements (<title>, <p>, <v>, <empty-line>, <subtitle>) inside FB2 body.
fn parse_fb2_text_block(fb2_body: &str) -> String {
    let mut result = String::new();
    let mut pos = 0;
    let bytes = fb2_body.as_bytes();

    while pos < bytes.len() {
        if bytes[pos] == b'<' {
            // Find end of tag
            if let Some(end_tag) = fb2_body[pos..].find('>') {
                let tag_str = &fb2_body[pos + 1..pos + end_tag];
                let tag_name = tag_str
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_start_matches('/');

                let tag_name_lower = tag_name.to_ascii_lowercase();

                if tag_name_lower == "p" || tag_name_lower == "v" || tag_name_lower == "subtitle" {
                    let close_tag = format!("</{tag_name}>");
                    let close_tag_alt = format!("</{tag_name_lower}>");
                    let content_start = pos + end_tag + 1;
                    if let Some(rel_close) = fb2_body[content_start..]
                        .find(&close_tag)
                        .or_else(|| fb2_body[content_start..].find(&close_tag_alt))
                    {
                        let inner_raw = &fb2_body[content_start..content_start + rel_close];
                        let clean = xml::strip_tags(inner_raw);
                        let trimmed = clean.trim();
                        if !trimmed.is_empty() {
                            if tag_name_lower == "subtitle" {
                                result.push_str("\n### ");
                                result.push_str(trimmed);
                                result.push_str("\n\n");
                            } else {
                                result.push_str(trimmed);
                                result.push_str("\n\n");
                            }
                        }
                        pos = content_start + rel_close + close_tag.len();
                        continue;
                    }
                } else if tag_name_lower == "title" && !tag_str.starts_with('/') {
                    let close_tag = "</title>";
                    let content_start = pos + end_tag + 1;
                    if let Some(rel_close) = fb2_body[content_start..].find(close_tag) {
                        let inner_raw = &fb2_body[content_start..content_start + rel_close];
                        let clean = xml::strip_tags(inner_raw);
                        let trimmed = clean.trim();
                        if !trimmed.is_empty() {
                            result.push_str("\n## ");
                            result.push_str(trimmed);
                            result.push_str("\n\n");
                        }
                        pos = content_start + rel_close + close_tag.len();
                        continue;
                    }
                } else if tag_name_lower == "empty-line" {
                    result.push('\n');
                }
                pos += end_tag + 1;
                continue;
            }
        }
        pos += 1;
    }

    result
}

/// Load an EPUB document from a ZIP container.
fn load_epub(path: &Path) -> Result<String> {
    let file = File::open(path)
        .with_context(|| format!("failed to open EPUB file {}", path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("failed to read EPUB ZIP structure in {}", path.display()))?;

    // 1. Locate rootfile from META-INF/container.xml
    let opf_path = find_epub_opf_path(&mut archive)
        .unwrap_or_else(|_| "OEBPS/content.opf".to_string());

    // 2. Read OPF file content
    let opf_content = xml::read_zip_entry(&mut archive, &opf_path)
        .with_context(|| format!("failed to read OPF manifest at {opf_path}"))?;

    // Determine directory prefix for relative hrefs
    let opf_dir = Path::new(&opf_path)
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();

    // 3. Extract metadata (Title & Author)
    let mut output = String::new();
    if let Some(title) = extract_tag_value(&opf_content, "dc:title") {
        let clean = xml::strip_tags(&title);
        if !clean.trim().is_empty() {
            output.push_str(&clean);
            output.push('\n');
            output.push_str(&"=".repeat(clean.trim().chars().count().max(4)));
            output.push_str("\n\n");
        }
    }

    if let Some(author) = extract_tag_value(&opf_content, "dc:creator") {
        let clean = xml::strip_tags(&author);
        if !clean.trim().is_empty() {
            output.push_str("Author: ");
            output.push_str(&clean);
            output.push_str("\n\n");
        }
    }

    // 4. Parse manifest (id -> href) and spine (idref order)
    let manifest = parse_epub_manifest(&opf_content);
    let spine = parse_epub_spine(&opf_content);

    // 5. Load chapter files in spine order
    let mut chapter_count = 0;
    for idref in spine {
        if let Some(href) = manifest.get(&idref) {
            let full_href = if opf_dir.is_empty() {
                href.clone()
            } else {
                format!("{opf_dir}/{href}")
            };

            // Remove anchor fragment if present (e.g. chapter1.xhtml#section2)
            let clean_href = full_href.split('#').next().unwrap_or(&full_href);

            if let Ok(html) = xml::read_zip_entry(&mut archive, clean_href) {
                let text = convert_html_to_text(&html);
                if !text.trim().is_empty() {
                    output.push_str(&text);
                    output.push_str("\n\n");
                    chapter_count += 1;
                }
            }
        }
    }

    // Fallback if spine yielded nothing: iterate over all .html / .xhtml files in archive
    if chapter_count == 0 {
        for i in 0..archive.len() {
            if let Ok(mut entry) = archive.by_index(i) {
                let name = entry.name().to_ascii_lowercase();
                if name.ends_with(".html") || name.ends_with(".xhtml") || name.ends_with(".htm") {
                    let mut html = String::new();
                    if entry.read_to_string(&mut html).is_ok() {
                        let text = convert_html_to_text(&html);
                        if !text.trim().is_empty() {
                            output.push_str(&text);
                            output.push_str("\n\n");
                        }
                    }
                }
            }
        }
    }

    if output.trim().is_empty() {
        bail!("EPUB file did not contain extractable text");
    }

    Ok(output)
}

/// Helper: Find rootfile path in META-INF/container.xml
fn find_epub_opf_path(archive: &mut ZipArchive<File>) -> Result<String> {
    let mut container_file = archive
        .by_name("META-INF/container.xml")
        .context("META-INF/container.xml missing")?;
    let mut content = String::new();
    container_file.read_to_string(&mut content)?;

    if let Some(start) = content.find("full-path=\"") {
        let rest = &content[start + 11..];
        if let Some(end) = rest.find('"') {
            return Ok(rest[..end].to_string());
        }
    }
    bail!("full-path attribute not found in container.xml")
}



/// Parses EPUB OPF manifest items (<item id="..." href="..."/>).
fn parse_epub_manifest(opf: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();

    for line in opf.split('<') {
        if line.starts_with("item ") {
            let id = extract_xml_attribute(line, "id");
            let href = extract_xml_attribute(line, "href");
            if let (Some(id), Some(href)) = (id, href) {
                map.insert(id, href);
            }
        }
    }

    map
}

/// Parses EPUB OPF spine itemrefs (<itemref idref="..."/>).
fn parse_epub_spine(opf: &str) -> Vec<String> {
    let mut spine = Vec::new();

    for line in opf.split('<') {
        if line.starts_with("itemref ") {
            if let Some(idref) = extract_xml_attribute(line, "idref") {
                spine.push(idref);
            }
        }
    }

    spine
}

/// Extracts attribute value from XML element snippet (e.g., id="foo").
fn extract_xml_attribute(snippet: &str, attr: &str) -> Option<String> {
    let pattern = format!("{attr}=\"");
    if let Some(start) = snippet.find(&pattern) {
        let rest = &snippet[start + pattern.len()..];
        if let Some(end) = rest.find('"') {
            return Some(rest[..end].to_string());
        }
    }

    let pattern_single = format!("{attr}='");
    if let Some(start) = snippet.find(&pattern_single) {
        let rest = &snippet[start + pattern_single.len()..];
        if let Some(end) = rest.find('\'') {
            return Some(rest[..end].to_string());
        }
    }

    None
}

/// Converts HTML/XHTML to plain text, preserving headings and paragraph boundaries.
fn convert_html_to_text(html: &str) -> String {
    let mut result = String::new();
    let mut in_tag = false;
    let mut current_tag = String::new();
    let mut tag_is_closing = false;

    let mut buf = String::new();

    for c in html.chars() {
        if c == '<' {
            in_tag = true;
            current_tag.clear();
            tag_is_closing = false;
        } else if c == '>' {
            in_tag = false;
            let tag_lower = current_tag.trim().to_ascii_lowercase();

            if tag_lower.starts_with('/') {
                tag_is_closing = true;
            }

            let clean_tag_name = tag_lower
                .trim_start_matches('/')
                .split_whitespace()
                .next()
                .unwrap_or("");

            match clean_tag_name {
                "p" | "div" | "tr" => {
                    let text = xml::decode_entities(&buf);
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        result.push_str(trimmed);
                        result.push_str("\n\n");
                    }
                    buf.clear();
                }
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    let text = xml::decode_entities(&buf);
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        let level = clean_tag_name.chars().nth(1).unwrap_or('2');
                        let prefix = match level {
                            '1' => "# ",
                            '2' => "## ",
                            _ => "### ",
                        };
                        result.push_str(prefix);
                        result.push_str(trimmed);
                        result.push_str("\n\n");
                    }
                    buf.clear();
                }
                "br" => {
                    buf.push('\n');
                }
                "li" => {
                    if tag_is_closing {
                        let text = xml::decode_entities(&buf);
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            result.push_str("- ");
                            result.push_str(trimmed);
                            result.push('\n');
                        }
                        buf.clear();
                    }
                }
                _ => {}
            }
        } else if in_tag {
            current_tag.push(c);
        } else {
            buf.push(c);
        }
    }

    let remaining = xml::decode_entities(&buf);
    let trimmed = remaining.trim();
    if !trimmed.is_empty() {
        result.push_str(trimmed);
        result.push_str("\n\n");
    }

    result
}

fn extract_fb2_authors(xml: &str) -> Option<String> {
    let mut authors = Vec::new();
    let mut pos = 0;

    while let Some(start) = xml[pos..].find("<author>") {
        let abs_start = pos + start;
        if let Some(end) = xml[abs_start..].find("</author>") {
            let author_block = &xml[abs_start..abs_start + end];
            let first = extract_tag_value(author_block, "first-name").unwrap_or_default();
            let last = extract_tag_value(author_block, "last-name").unwrap_or_default();
            let full = format!("{first} {last}").trim().to_string();
            if !full.is_empty() {
                authors.push(full);
            }
            pos = abs_start + end + 9;
        } else {
            break;
        }
    }

    if authors.is_empty() {
        None
    } else {
        Some(authors.join(", "))
    }
}

fn extract_tag_value(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let open_alt = format!("<{tag} ");
    let close = format!("</{tag}>");

    let start_pos = xml.find(&open).or_else(|| {
        xml.find(&open_alt).and_then(|idx| {
            xml[idx..].find('>').map(|end| idx + end + 1)
        })
    })?;

    let content_start = if xml[start_pos..].starts_with(&open) {
        start_pos + open.len()
    } else {
        start_pos
    };

    let end_pos = xml[content_start..].find(&close)?;
    Some(xml[content_start..content_start + end_pos].to_string())
}





#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_fb2_xml() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0">
  <description>
    <title-info>
      <book-title>Test Book Title</book-title>
      <author>
        <first-name>John</first-name>
        <last-name>Doe</last-name>
      </author>
    </title-info>
  </description>
  <body>
    <title><p>Chapter One</p></title>
    <p>First paragraph of the book.</p>
    <p>Second paragraph with &amp; entity.</p>
  </body>
</FictionBook>"#;

        let parsed = parse_fb2_xml(xml).unwrap();
        assert!(parsed.contains("Test Book Title"));
        assert!(parsed.contains("Author: John Doe"));
        assert!(parsed.contains("## Chapter One"));
        assert!(parsed.contains("First paragraph of the book."));
        assert!(parsed.contains("Second paragraph with & entity."));
    }

    #[test]
    fn converts_html_to_text_with_markdown_headers() {
        let html = r#"<!DOCTYPE html>
<html>
<head><title>Chapter 1</title></head>
<body>
  <h1>Chapter 1: The Beginning</h1>
  <p>Once upon a time in a distant land.</p>
  <p>Another line of text with <b>bold</b> word.</p>
  <ul>
    <li>First item</li>
    <li>Second item</li>
  </ul>
</body>
</html>"#;

        let text = convert_html_to_text(html);
        assert!(text.contains("# Chapter 1: The Beginning"));
        assert!(text.contains("Once upon a time in a distant land."));
        assert!(text.contains("Another line of text with bold word."));
        assert!(text.contains("- First item"));
        assert!(text.contains("- Second item"));
    }

    #[test]
    fn parses_epub_zip_archive() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;

        let temp_dir = tempfile::tempdir().unwrap();
        let epub_path = temp_dir.path().join("test_book.epub");
        let file = File::create(&epub_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);

        let options = SimpleFileOptions::default();

        zip.start_file("META-INF/container.xml", options).unwrap();
        zip.write_all(
            r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#
                .as_bytes(),
        )
        .unwrap();

        zip.start_file("OEBPS/content.opf", options).unwrap();
        zip.write_all(
            r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>EPUB Test Book</dc:title>
    <dc:creator>Alice Smith</dc:creator>
  </metadata>
  <manifest>
    <item id="chap1" href="chapter1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="chap1"/>
  </spine>
</package>"#
                .as_bytes(),
        )
        .unwrap();

        zip.start_file("OEBPS/chapter1.xhtml", options).unwrap();
        zip.write_all(
            r#"<!DOCTYPE html>
<html>
<body>
  <h1>Chapter 1</h1>
  <p>Hello world from EPUB!</p>
</body>
</html>"#
                .as_bytes(),
        )
        .unwrap();

        zip.finish().unwrap();

        let text = load(&epub_path).unwrap();
        assert!(text.contains("EPUB Test Book"));
        assert!(text.contains("Author: Alice Smith"));
        assert!(text.contains("# Chapter 1"));
        assert!(text.contains("Hello world from EPUB!"));
    }
}
