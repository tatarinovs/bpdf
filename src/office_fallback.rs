//! Text of Office Open XML and OpenDocument files without Office: the
//! fallback when Office automation is unavailable.

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result, bail};
use zip::ZipArchive;

use crate::xml::{self, Event, attribute, local_name};

/// The text of a DOCX, XLSX, PPTX or OpenDocument file, recognised by the
/// parts the archive holds (so a misnamed file is still read).
pub fn extract_text(path: &Path) -> Result<String> {
    let file = File::open(path)
        .with_context(|| format!("failed to open Office file {}", path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("failed to parse Office ZIP container {}", path.display()))?;
    let has = |archive: &ZipArchive<File>, name: &str| archive.index_for_name(name).is_some();

    let text = if has(&archive, "word/document.xml") {
        extract_docx(&mut archive)?
    } else if has(&archive, "xl/workbook.xml") {
        extract_xlsx(&mut archive)?
    } else if has(&archive, "ppt/presentation.xml") {
        extract_pptx(&mut archive)?
    } else if has(&archive, "content.xml") {
        extract_opendocument(&mut archive)?
    } else {
        bail!(
            "{} is neither Office Open XML nor OpenDocument",
            path.display()
        );
    };
    if text.trim().is_empty() {
        bail!("no readable text found in {}", path.display());
    }
    Ok(text.trim().to_owned())
}

/// Paragraphs of `word/document.xml`; headings become Markdown headings.
fn extract_docx(archive: &mut ZipArchive<File>) -> Result<String> {
    let document = xml::read_zip_entry(archive, "word/document.xml")?;
    let mut out = String::new();
    let mut paragraph = String::new();
    let mut heading = 0;
    // Tabs and breaks count inside runs only: paragraph properties hold tab
    // stops of the same name.
    let (mut in_run, mut in_text) = (false, false);
    for event in xml::events(&document) {
        match event {
            Event::Start { name, attributes } => match local_name(name) {
                "r" => in_run = true,
                "t" => in_text = in_run,
                "tab" if in_run => paragraph.push('\t'),
                "br" | "cr" if in_run => paragraph.push('\n'),
                "pStyle" => heading = heading_level(attribute(attributes, "w:val").as_deref()),
                _ => {}
            },
            Event::End(name) => match local_name(name) {
                "r" => in_run = false,
                "t" => in_text = false,
                "p" => {
                    push_paragraph(&mut out, &paragraph, heading);
                    paragraph.clear();
                    heading = 0;
                }
                _ => {}
            },
            Event::Text(text) if in_text => paragraph.push_str(&text),
            Event::Text(_) => {}
        }
    }
    Ok(out)
}

/// Level of a Word heading style (`Heading2`, or `2` in localised
/// documents), 0 for other styles.
fn heading_level(style: Option<&str>) -> usize {
    let Some(style) = style else {
        return 0;
    };
    let digits = style
        .get(..7)
        .filter(|prefix| prefix.eq_ignore_ascii_case("heading"))
        .map_or(style, |_| &style[7..]);
    digits
        .parse()
        .ok()
        .filter(|level| (1..=6).contains(level))
        .unwrap_or(0)
}

fn push_paragraph(out: &mut String, paragraph: &str, heading: usize) {
    let text = paragraph.trim();
    if text.is_empty() {
        return;
    }
    if heading > 0 {
        out.push_str(&"#".repeat(heading));
        out.push(' ');
    }
    out.push_str(text);
    out.push_str("\n\n");
}

/// Paragraphs and headings of an OpenDocument `content.xml` (text,
/// spreadsheet and presentation alike).
fn extract_opendocument(archive: &mut ZipArchive<File>) -> Result<String> {
    let content = xml::read_zip_entry(archive, "content.xml")?;
    let mut out = String::new();
    let mut paragraph = String::new();
    let mut heading = 0;
    // Paragraphs nest (notes, frames); the outermost one is the paragraph.
    let mut depth = 0usize;
    for event in xml::events(&content) {
        match event {
            Event::Start { name, attributes } => match name {
                "text:p" => depth += 1,
                "text:h" => {
                    if depth == 0 {
                        heading = attribute(attributes, "text:outline-level")
                            .and_then(|level| level.parse().ok())
                            .unwrap_or(1usize)
                            .clamp(1, 6);
                    }
                    depth += 1;
                }
                "text:tab" if depth > 0 => paragraph.push('\t'),
                "text:line-break" if depth > 0 => paragraph.push('\n'),
                "text:s" if depth > 0 => {
                    let count = attribute(attributes, "text:c")
                        .and_then(|count| count.parse().ok())
                        .unwrap_or(1usize);
                    paragraph.extend(std::iter::repeat_n(' ', count.min(1000)));
                }
                _ => {}
            },
            Event::End("text:p" | "text:h") => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    push_paragraph(&mut out, &paragraph, heading);
                    paragraph.clear();
                    heading = 0;
                }
            }
            Event::Text(text) if depth > 0 => paragraph.push_str(&text),
            _ => {}
        }
    }
    Ok(out)
}

/// One section per slide, one list item per text paragraph.
fn extract_pptx(archive: &mut ZipArchive<File>) -> Result<String> {
    let mut out = String::new();
    for (index, name) in xml::collect_sorted_entries(archive, "ppt/slides/slide", ".xml")
        .iter()
        .enumerate()
    {
        let slide = xml::read_zip_entry(archive, name)?;
        let mut lines = Vec::new();
        let mut line = String::new();
        let mut in_text = false;
        for event in xml::events(&slide) {
            match event {
                Event::Start { name: "a:t", .. } => in_text = true,
                Event::Start { name: "a:br", .. } => line.push(' '),
                Event::End("a:t") => in_text = false,
                Event::End("a:p") => {
                    let text = line.trim();
                    if !text.is_empty() {
                        lines.push(text.to_owned());
                    }
                    line.clear();
                }
                Event::Text(text) if in_text => line.push_str(&text),
                _ => {}
            }
        }
        if !lines.is_empty() {
            out.push_str(&format!("# Slide {}\n\n", index + 1));
            for line in lines {
                out.push_str("- ");
                out.push_str(&line);
                out.push('\n');
            }
            out.push('\n');
        }
    }
    Ok(out)
}

/// One section per worksheet, one ` | `-separated line per row.
fn extract_xlsx(archive: &mut ZipArchive<File>) -> Result<String> {
    let shared = match xml::read_zip_entry(archive, "xl/sharedStrings.xml") {
        Ok(strings) => shared_strings(&strings),
        Err(_) => Vec::new(),
    };
    let mut out = String::new();
    for (index, name) in xml::collect_sorted_entries(archive, "xl/worksheets/sheet", ".xml")
        .iter()
        .enumerate()
    {
        let sheet = xml::read_zip_entry(archive, name)?;
        out.push_str(&format!("# Sheet {}\n\n", index + 1));
        let mut row = Vec::new();
        // Type of the current cell, its <v> value and inline string.
        let mut kind = String::new();
        let (mut value, mut inline) = (String::new(), String::new());
        let mut target = None;
        for event in xml::events(&sheet) {
            match event {
                Event::Start {
                    name: "c",
                    attributes,
                } => {
                    kind = attribute(attributes, "t").unwrap_or_default().into_owned();
                    value.clear();
                    inline.clear();
                }
                Event::Start { name: "v", .. } => target = Some(true),
                Event::Start { name: "t", .. } => target = Some(false),
                Event::End("v" | "t") => target = None,
                Event::Text(text) => match target {
                    Some(true) => value.push_str(&text),
                    Some(false) => inline.push_str(&text),
                    None => {}
                },
                Event::End("c") => {
                    let text = match kind.as_str() {
                        "s" => value
                            .trim()
                            .parse::<usize>()
                            .ok()
                            .and_then(|index| shared.get(index))
                            .cloned()
                            .unwrap_or_default(),
                        "inlineStr" => std::mem::take(&mut inline),
                        "b" => match value.trim() {
                            "1" => "TRUE".to_owned(),
                            "0" => "FALSE".to_owned(),
                            other => other.to_owned(),
                        },
                        _ => std::mem::take(&mut value),
                    };
                    let text = text.trim();
                    if !text.is_empty() {
                        row.push(text.to_owned());
                    }
                }
                Event::End("row") => {
                    if !row.is_empty() {
                        out.push_str(&row.join(" | "));
                        out.push('\n');
                    }
                    row.clear();
                }
                _ => {}
            }
        }
        out.push('\n');
    }
    Ok(out)
}

/// The shared strings of a workbook: each `<si>` is one string, its rich
/// text runs joined; phonetic readings (`<rPh>`) are left out.
fn shared_strings(xml: &str) -> Vec<String> {
    let mut strings = Vec::new();
    let mut current = String::new();
    let (mut in_text, mut in_phonetic) = (false, false);
    for event in xml::events(xml) {
        match event {
            Event::Start { name: "si", .. } => current.clear(),
            Event::Start { name: "rPh", .. } => in_phonetic = true,
            Event::End("rPh") => in_phonetic = false,
            Event::Start { name: "t", .. } => in_text = !in_phonetic,
            Event::End("t") => in_text = false,
            Event::Text(text) if in_text => current.push_str(&text),
            Event::End("si") => strings.push(std::mem::take(&mut current)),
            _ => {}
        }
    }
    strings
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;
    use zip::write::SimpleFileOptions;

    fn archive(parts: &[(&str, &str)]) -> NamedTempFile {
        let temp_file = NamedTempFile::new().unwrap();
        let mut zip = zip::ZipWriter::new(File::create(temp_file.path()).unwrap());
        for (name, content) in parts {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        temp_file
    }

    #[test]
    fn extracts_docx_paragraphs_as_word_writes_them() {
        let document = r#"<?xml version="1.0" encoding="UTF-8"?>
        <w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p w:rsidR="00AB"><w:pPr><w:pStyle w:val="Heading1"/><w:tabs><w:tab w:val="left" w:pos="720"/></w:tabs></w:pPr><w:r><w:t>Document Header</w:t></w:r></w:p>
                <w:p w:rsidR="00AC"><w:r><w:t xml:space="preserve">First </w:t></w:r><w:r><w:tab/><w:t>with &amp; entity.</w:t></w:r></w:p>
                <w:tbl><w:tr><w:tc><w:p><w:r><w:t>Cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
                <w:p><w:pPr><w:pStyle w:val="2"/></w:pPr><w:r><w:t>Localised heading</w:t></w:r></w:p>
            </w:body>
        </w:document>"#;
        let file = archive(&[("word/document.xml", document)]);
        assert_eq!(
            extract_text(file.path()).unwrap(),
            "# Document Header\n\nFirst \twith & entity.\n\nCell\n\n## Localised heading"
        );
    }

    #[test]
    fn extracts_pptx_slides_text() {
        let slide = r#"<?xml version="1.0" encoding="UTF-8"?>
        <p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
            <a:p><a:r><a:t>Slide Title</a:t></a:r><a:r><a:t> Text</a:t></a:r></a:p>
            <a:tbl><a:tr><a:tc><a:txBody><a:p><a:r><a:t>Cell</a:t></a:r></a:p></a:txBody></a:tc></a:tr></a:tbl>
        </p:sld>"#;
        let file = archive(&[
            ("ppt/presentation.xml", "<p/>"),
            ("ppt/slides/slide1.xml", slide),
        ]);
        assert_eq!(
            extract_text(file.path()).unwrap(),
            "# Slide 1\n\n- Slide Title Text\n- Cell"
        );
    }

    #[test]
    fn extracts_xlsx_cells_and_types() {
        let shared = r#"<?xml version="1.0" encoding="UTF-8"?>
        <sst count="2" uniqueCount="2">
            <si><t>Header 1</t></si>
            <si><r><t>Rich </t></r><r><t>text</t></r><rPh><t>reading</t></rPh></si>
        </sst>"#;
        let sheet = r#"<?xml version="1.0" encoding="UTF-8"?>
        <worksheet><sheetData>
            <row r="1">
                <c r="A1" t="s"><v>0</v></c>
                <c r="B1" t="inlineStr"><is><t>Inline Value</t></is></c>
                <c r="C1" t="s"><v>1</v></c>
            </row>
            <row r="2">
                <c r="A2"><v>123.45</v></c>
                <c r="B2" t="b"><v>1</v></c>
                <c r="C2" t="str"><f>CONCAT("A","B")</f><v>AB</v></c>
            </row>
        </sheetData></worksheet>"#;
        let file = archive(&[
            ("xl/workbook.xml", "<workbook/>"),
            ("xl/sharedStrings.xml", shared),
            ("xl/worksheets/sheet1.xml", sheet),
        ]);
        assert_eq!(
            extract_text(file.path()).unwrap(),
            "# Sheet 1\n\nHeader 1 | Inline Value | Rich text\n123.45 | TRUE | AB"
        );
    }

    #[test]
    fn extracts_opendocument_text() {
        let content = r#"<?xml version="1.0" encoding="UTF-8"?>
        <office:document-content><office:body><office:text>
            <text:h text:outline-level="2">Title</text:h>
            <text:p>a<text:s text:c="2"/>b<text:tab/>c<text:note><text:note-body><text:p>note</text:p></text:note-body></text:note></text:p>
        </office:text></office:body></office:document-content>"#;
        let file = archive(&[("content.xml", content)]);
        assert_eq!(
            extract_text(file.path()).unwrap(),
            "## Title\n\na  b\tcnote"
        );
    }
}
