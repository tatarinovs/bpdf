use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use zip::ZipArchive;

use crate::formats::Format;
use crate::{encoding, xml};

/// Reads an ebook of an already detected format and returns its contents
/// as clean text.
pub fn load(path: &Path, format: Format) -> Result<String> {
    match format {
        Format::Epub => load_epub(path),
        Format::Htmlz => load_htmlz(path),
        Format::Fb2 if is_zip(path) => load_fb2_zip(path),
        _ => load_fb2_raw(path),
    }
}

fn is_zip(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
}

/// Load `.htmlz` archive containing an `index.html` or html file.
pub fn load_htmlz(path: &Path) -> Result<String> {
    let file = File::open(path)
        .with_context(|| format!("failed to open HTMLZ file {}", path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("failed to read ZIP archive {}", path.display()))?;

    let mut index_file = None;
    for index in 0..archive.len() {
        let Some(name) = archive.name_for_index(index) else {
            continue;
        };
        let lower = name.to_ascii_lowercase();
        if lower == "index.html" || lower == "index.htm" {
            index_file = Some(name.to_owned());
            break;
        }
        if index_file.is_none() && (lower.ends_with(".html") || lower.ends_with(".htm")) {
            index_file = Some(name.to_owned());
        }
    }

    let target_name = index_file
        .with_context(|| format!("no HTML file found in HTMLZ archive {}", path.display()))?;
    let html_content = xml::read_zip_entry(&mut archive, &target_name)?;
    Ok(convert_html_to_text(&html_content))
}

/// Load raw `.fb2` XML document.
fn load_fb2_raw(path: &Path) -> Result<String> {
    parse_fb2_xml(&encoding::read_text(path)?)
}

/// Load `.fb2.zip` archive containing an `.fb2` or `.xml` file.
fn load_fb2_zip(path: &Path) -> Result<String> {
    let file = File::open(path)
        .with_context(|| format!("failed to open FB2 archive {}", path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("failed to read ZIP structure in {}", path.display()))?;

    let index = (0..archive.len())
        .find(|index| {
            archive.name_for_index(*index).is_some_and(|name| {
                let name = name.to_ascii_lowercase();
                name.ends_with(".fb2") || name.ends_with(".xml")
            })
        })
        .unwrap_or(0);
    let mut bytes = Vec::new();
    archive
        .by_index(index)?
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read inner FB2 file from {}", path.display()))?;
    parse_fb2_xml(&encoding::decode_text(&bytes))
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

    if let Some(authors) = extract_fb2_authors(xml)
        && !authors.is_empty()
    {
        output.push_str("Author: ");
        output.push_str(&authors);
        output.push_str("\n\n");
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
    let file =
        File::open(path).with_context(|| format!("failed to open EPUB file {}", path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("failed to read EPUB ZIP structure in {}", path.display()))?;

    // 1. Locate rootfile from META-INF/container.xml
    let opf_path =
        find_epub_opf_path(&mut archive).unwrap_or_else(|_| "OEBPS/content.opf".to_string());

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
        if line.starts_with("itemref ")
            && let Some(idref) = extract_xml_attribute(line, "idref")
        {
            spine.push(idref);
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
pub fn convert_html_to_text(html: &str) -> String {
    // ASCII lowercasing keeps every byte offset, so positions found in
    // `lower` index `html` as well.
    let lower = html.to_ascii_lowercase();
    let mut result = String::new();
    let mut buf = String::new();
    // Container whose content is metadata or graphics rather than readable text.
    let mut hidden: Option<&str> = None;
    let mut pre_depth = 0usize;
    let mut in_item = false;
    let mut pos = 0;

    while let Some(c) = html[pos..].chars().next() {
        pos += c.len_utf8();
        // `<` starts markup only before a name, `/`, `!` or `?`; otherwise
        // it is text, as in `a < b`.
        let starts_markup = c == '<'
            && lower[pos..]
                .starts_with(|next: char| next.is_ascii_alphabetic() || "/!?".contains(next));
        if !starts_markup {
            if hidden.is_none() {
                push_text(&mut buf, c, pre_depth > 0);
            }
            continue;
        }

        if lower[pos..].starts_with("!--") {
            pos = comment_end(&lower, pos + 3);
            continue;
        }
        let Some(length) = tag_length(&lower[pos..]) else {
            break;
        };
        let tag = lower[pos..pos + length].trim();
        pos += length + 1;

        let closing = tag.starts_with('/');
        let self_closing = tag.ends_with('/');
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("");

        // Script and style content is code, and may contain `<` or `</p>`
        // in strings; only the matching end tag closes it.
        if !closing && !self_closing && matches!(name, "script" | "style") {
            pos = lower[pos..]
                .find(&format!("</{name}"))
                .and_then(|start| {
                    let end = pos + start;
                    lower[end..].find('>').map(|close| end + close + 1)
                })
                .unwrap_or(html.len());
            continue;
        }

        if let Some(container) = hidden {
            if closing && name == container {
                hidden = None;
                continue;
            }
            // `</head>` is optional: the first body element ends the head.
            let head_ends = container == "head"
                && !closing
                && !matches!(
                    name,
                    "title" | "meta" | "link" | "base" | "noscript" | "template"
                );
            if !head_ends {
                continue;
            }
            hidden = None;
        }
        if !closing && !self_closing && matches!(name, "head" | "noscript" | "template" | "svg") {
            hidden = Some(name);
            continue;
        }

        match name {
            // `</li>` is optional: the next item or the end of the list closes it.
            "li" | "ul" | "ol" if in_item => {
                flush(&mut result, &mut buf, "- ", "\n", false);
                in_item = name == "li" && !closing;
            }
            "li" => in_item = !closing,
            "p" | "div" | "tr" | "pre" | "blockquote" | "section" | "article" | "header"
            | "footer" | "main" | "aside" | "nav" | "ul" | "ol" | "dl" | "dt" | "dd" | "table"
            | "figure" | "figcaption" | "hr" | "address" => {
                flush(&mut result, &mut buf, "", "\n\n", pre_depth > 0);
                if name == "pre" {
                    if closing {
                        pre_depth = pre_depth.saturating_sub(1);
                    } else {
                        pre_depth += 1;
                    }
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let prefix = match name {
                    "h1" => "# ",
                    "h2" => "## ",
                    _ => "### ",
                };
                flush(&mut result, &mut buf, prefix, "\n\n", false);
            }
            "br" => {
                if buf.ends_with(' ') {
                    buf.pop();
                }
                buf.push('\n');
            }
            "td" | "th" if closing => push_space(&mut buf),
            _ => {}
        }
    }

    flush(&mut result, &mut buf, "", "\n\n", pre_depth > 0);
    result
}

/// Appends the buffered text as one block. Preformatted text keeps its
/// leading indentation; everything else is trimmed.
fn flush(result: &mut String, buf: &mut String, prefix: &str, separator: &str, pre: bool) {
    let text = xml::decode_entities(buf);
    let trimmed = if pre {
        text.trim_matches('\n').trim_end()
    } else {
        text.trim()
    };
    if !trimmed.trim().is_empty() {
        result.push_str(prefix);
        result.push_str(trimmed);
        result.push_str(separator);
    }
    buf.clear();
}

fn push_text(buf: &mut String, c: char, pre: bool) {
    if pre {
        if c != '\r' {
            buf.push(c);
        }
    } else if c.is_whitespace() {
        // Source indentation and line breaks are not visible in HTML.
        push_space(buf);
    } else {
        buf.push(c);
    }
}

fn push_space(buf: &mut String) {
    if !buf.is_empty() && !buf.ends_with([' ', '\n']) {
        buf.push(' ');
    }
}

/// Position after a comment whose body starts at `start`; `<!-->` and
/// `<!--->` are complete (empty) comments in HTML.
fn comment_end(lower: &str, start: usize) -> usize {
    let body = &lower[start..];
    if body.starts_with('>') {
        start + 1
    } else if body.starts_with("->") {
        start + 2
    } else {
        body.find("-->").map_or(lower.len(), |end| start + end + 3)
    }
}

/// Length of a tag up to its closing `>`, which does not count inside a
/// quoted attribute value such as `title="a > b"`.
fn tag_length(tag: &str) -> Option<usize> {
    let mut quote = None;
    let mut after_equals = false;
    for (index, c) in tag.char_indices() {
        if let Some(open) = quote {
            if c == open {
                quote = None;
            }
            continue;
        }
        match c {
            '>' => return Some(index),
            '"' | '\'' if after_equals => {
                quote = Some(c);
                after_equals = false;
            }
            '=' => after_equals = true,
            c if c.is_whitespace() => {}
            _ => after_equals = false,
        }
    }
    None
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
        xml.find(&open_alt)
            .and_then(|idx| xml[idx..].find('>').map(|end| idx + end + 1))
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
    fn web_page_text_skips_code_and_collapses_whitespace() {
        let html = r#"<html><head><title>Ignored</title>
<style>p > b { color: red }</style>
<body>
  <script>if (a < b && c > d) { alert("x"); }</script>
  <!-- <p>commented out</p> -->
  <p>Первая
      строка<br>вторая</p>
  <table><tr><td>A</td><td>B</td></tr></table>
  <noscript>Enable JS</noscript>
</body></html>"#;

        let text = convert_html_to_text(html);
        assert_eq!(text, "Первая строка\nвторая\n\nA B\n\n");
    }

    #[test]
    fn web_page_text_survives_code_and_bare_angle_brackets() {
        let html = r#"<head><meta charset="utf-8"><script>for(i=0;i<n;i++){s+="</p>"}</script>
<script>document.write("<!-- not a comment")</script>
<p title="a > b">5 > 3, a < b &amp;lt; &laquo;да&raquo;&#160;&#x2014;</p>
<pre>
  fn main() {
      x();
  }</pre>
<ul><li>one<li>two</ul>"#;

        let text = convert_html_to_text(html);
        assert_eq!(
            text,
            "5 > 3, a < b &lt; «да»\u{a0}—\n\n  fn main() {\n      x();\n  }\n\n- one\n- two\n"
        );
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

        let text = load(&epub_path, Format::Epub).unwrap();
        assert!(text.contains("EPUB Test Book"));
        assert!(text.contains("Author: Alice Smith"));
        assert!(text.contains("# Chapter 1"));
        assert!(text.contains("Hello world from EPUB!"));
    }
}
