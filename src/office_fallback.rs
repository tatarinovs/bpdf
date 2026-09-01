use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result, bail};
use zip::ZipArchive;

use crate::xml;

/// Primary entry point for Pure-Rust text extraction from Office documents.
/// Dispatches based on file extension to extract text from DOCX, XLSX, PPTX, ODT, ODS, ODP archives.
pub fn extract_text(path: &Path) -> Result<String> {
    let file = File::open(path)
        .with_context(|| format!("failed to open Office file {}", path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("failed to parse Office ZIP container {}", path.display()))?;

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    match ext.as_str() {
        "docx" | "doc" | "rtf" => extract_docx(&mut archive),
        "odt" | "ods" => extract_opendocument(&mut archive),
        "xlsx" | "xls" => extract_xlsx(&mut archive),
        "pptx" | "ppt" | "pps" | "ppsx" => extract_pptx(&mut archive),
        "odp" => extract_opendocument(&mut archive),
        _ => {
            if let Ok(text) = extract_docx(&mut archive)
                && !text.is_empty()
            {
                return Ok(text);
            }
            if let Ok(text) = extract_pptx(&mut archive)
                && !text.is_empty()
            {
                return Ok(text);
            }
            if let Ok(text) = extract_xlsx(&mut archive)
                && !text.is_empty()
            {
                return Ok(text);
            }
            extract_opendocument(&mut archive)
        }
    }
}

/// Extract text from Word `.docx` (`word/document.xml`).
fn extract_docx(archive: &mut ZipArchive<File>) -> Result<String> {
    let doc_xml = xml::read_zip_entry(archive, "word/document.xml")
        .with_context(|| "word/document.xml missing in DOCX file")?;
    let mut out = String::new();

    for p in doc_xml.split("<w:p>") {
        let p_text = xml::collect_tag_texts(p, "<w:t", "</w:t>").join("");
        let trimmed = p_text.trim();
        if !trimmed.is_empty() {
            if p.contains("<w:pStyle w:val=\"Heading1\"") || p.contains("Heading 1") {
                out.push_str("# ");
            } else if p.contains("<w:pStyle w:val=\"Heading2\"") || p.contains("Heading 2") {
                out.push_str("## ");
            } else if p.contains("<w:pStyle w:val=\"Heading3\"") || p.contains("Heading 3") {
                out.push_str("### ");
            }
            out.push_str(trimmed);
            out.push_str("\n\n");
        }
    }

    if out.trim().is_empty() {
        bail!("no readable text found in DOCX file");
    }
    Ok(out.trim().to_owned())
}

/// Extract text from OpenDocument files (`content.xml`).
fn extract_opendocument(archive: &mut ZipArchive<File>) -> Result<String> {
    let content_xml = xml::read_zip_entry(archive, "content.xml")
        .with_context(|| "content.xml missing in OpenDocument file")?;
    let mut out = String::new();

    let mut cursor = content_xml.as_str();
    while let Some(start) = cursor.find("<text:") {
        let tag = &cursor[start..];
        if tag.starts_with("<text:h") {
            if let Some(gt) = tag.find('>') {
                let content_start = gt + 1;
                if let Some(end) = tag[content_start..].find("</text:h>") {
                    let h_text = xml::strip_tags(&tag[content_start..content_start + end]);
                    let trimmed = h_text.trim();
                    if !trimmed.is_empty() {
                        out.push_str("## ");
                        out.push_str(trimmed);
                        out.push_str("\n\n");
                    }
                    cursor = &tag[content_start + end + 9..];
                    continue;
                }
            }
        } else if tag.starts_with("<text:p")
            && let Some(gt) = tag.find('>')
        {
            let content_start = gt + 1;
            if let Some(end) = tag[content_start..].find("</text:p>") {
                let p_text = xml::strip_tags(&tag[content_start..content_start + end]);
                let trimmed = p_text.trim();
                if !trimmed.is_empty() {
                    out.push_str(trimmed);
                    out.push_str("\n\n");
                }
                cursor = &tag[content_start + end + 9..];
                continue;
            }
        }
        cursor = &tag[6..];
    }

    if out.trim().is_empty() {
        bail!("no readable text found in OpenDocument file");
    }
    Ok(out.trim().to_owned())
}

/// Extract text from PowerPoint `.pptx` (`ppt/slides/slide*.xml`).
fn extract_pptx(archive: &mut ZipArchive<File>) -> Result<String> {
    let slide_names = xml::collect_sorted_entries(archive, "ppt/slides/slide", ".xml");

    let mut out = String::new();
    for (index, name) in slide_names.iter().enumerate() {
        if let Ok(slide_xml) = xml::read_zip_entry(archive, name) {
            let slide_text = xml::collect_tag_texts(&slide_xml, "<a:t", "</a:t>");
            if !slide_text.is_empty() {
                out.push_str(&format!("# Slide {}\n\n", index + 1));
                for line in slide_text {
                    out.push_str("- ");
                    out.push_str(line.trim());
                    out.push('\n');
                }
                out.push('\n');
            }
        }
    }

    if out.trim().is_empty() {
        bail!("no readable slides found in PPTX file");
    }
    Ok(out.trim().to_owned())
}

/// Extract text from Excel `.xlsx` (`xl/sharedStrings.xml` and worksheet XMLs).
fn extract_xlsx(archive: &mut ZipArchive<File>) -> Result<String> {
    let shared_strings = xml::read_zip_entry(archive, "xl/sharedStrings.xml")
        .map(|s| xml::collect_tag_texts(&s, "<t", "</t>"))
        .unwrap_or_default();

    let sheet_names = xml::collect_sorted_entries(archive, "xl/worksheets/sheet", ".xml");

    let mut out = String::new();
    for (sheet_idx, sheet_file) in sheet_names.iter().enumerate() {
        if let Ok(sheet_xml) = xml::read_zip_entry(archive, sheet_file) {
            out.push_str(&format!("# Sheet {}\n\n", sheet_idx + 1));
            for row in sheet_xml.split("<row") {
                let mut row_values = Vec::new();
                for cell in row.split("<c ") {
                    if let Some(val) = extract_cell_text(cell, &shared_strings) {
                        let trimmed = val.trim();
                        if !trimmed.is_empty() {
                            row_values.push(trimmed.to_owned());
                        }
                    }
                }

                if !row_values.is_empty() {
                    out.push_str(&row_values.join(" | "));
                    out.push('\n');
                }
            }
            out.push('\n');
        }
    }

    if out.trim().is_empty() && !shared_strings.is_empty() {
        out = shared_strings.join("\n");
    }

    if out.trim().is_empty() {
        bail!("no readable data found in XLSX file");
    }
    Ok(out.trim().to_owned())
}

fn extract_cell_text(cell: &str, shared_strings: &[String]) -> Option<String> {
    // Shared string: <c t="s">...<v>0</v>
    if cell.contains("t=\"s\"") || cell.contains("t='s'") {
        if let Some(v_start) = cell.find("<v>")
            && let Some(v_end) = cell[v_start + 3..].find("</v>")
            && let Ok(idx) = cell[v_start + 3..v_start + 3 + v_end].parse::<usize>()
            && let Some(val) = shared_strings.get(idx)
        {
            return Some(val.clone());
        }
        return None;
    }

    // Inline string: <c t="inlineStr">...<is><t>...</t></is>
    if cell.contains("t=\"inlineStr\"") || cell.contains("t='inlineStr'") {
        let texts = xml::collect_tag_texts(cell, "<t", "</t>");
        if !texts.is_empty() {
            return Some(texts.join(""));
        }
        return None;
    }

    // Boolean: <c t="b">...<v>1</v>
    if cell.contains("t=\"b\"") || cell.contains("t='b'") {
        if let Some(v_start) = cell.find("<v>")
            && let Some(v_end) = cell[v_start + 3..].find("</v>")
        {
            let val = cell[v_start + 3..v_start + 3 + v_end].trim();
            return match val {
                "1" => Some("TRUE".to_owned()),
                "0" => Some("FALSE".to_owned()),
                other => Some(other.to_owned()),
            };
        }
        return None;
    }

    // Standard value or formula result: <v>123.45</v>
    if let Some(v_start) = cell.find("<v>")
        && let Some(v_end) = cell[v_start + 3..].find("</v>")
    {
        let val = &cell[v_start + 3..v_start + 3 + v_end];
        return Some(xml::decode_entities(val));
    }

    // Fallback: any inline <t> tags
    let texts = xml::collect_tag_texts(cell, "<t", "</t>");
    if !texts.is_empty() {
        return Some(texts.join(""));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;
    use zip::write::SimpleFileOptions;

    #[test]
    fn extracts_docx_xml_text() {
        let temp_file = NamedTempFile::new().unwrap();
        let file = File::create(temp_file.path()).unwrap();
        let mut zip = zip::ZipWriter::new(file);

        let docx_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
        <w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Document Header</w:t></w:r></w:p>
                <w:p><w:r><w:t>First paragraph content with &amp; entity.</w:t></w:r></w:p>
            </w:body>
        </w:document>"#;

        zip.start_file("word/document.xml", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(docx_xml.as_bytes()).unwrap();
        zip.finish().unwrap();

        let extracted = extract_text(temp_file.path()).unwrap();
        assert!(extracted.contains("# Document Header"));
        assert!(extracted.contains("First paragraph content with & entity."));
    }

    #[test]
    fn extracts_pptx_slides_text() {
        let temp_file = NamedTempFile::new().unwrap();
        let file = File::create(temp_file.path()).unwrap();
        let mut zip = zip::ZipWriter::new(file);

        let slide_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
        <p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
            <a:t>Slide Title Text</a:t>
        </p:sld>"#;

        zip.start_file("ppt/slides/slide1.xml", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(slide_xml.as_bytes()).unwrap();
        zip.finish().unwrap();

        let extracted = extract_text(temp_file.path()).unwrap();
        assert!(extracted.contains("# Slide 1"));
        assert!(extracted.contains("- Slide Title Text"));
    }

    #[test]
    fn extracts_xlsx_cells_and_types() {
        let temp_file = NamedTempFile::new().unwrap();
        let file = File::create(temp_file.path()).unwrap();
        let mut zip = zip::ZipWriter::new(file);

        let shared_strings_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
        <sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="2" uniqueCount="2">
            <si><t>Header 1</t></si>
            <si><t>Shared Text</t></si>
        </sst>"#;

        let sheet1_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
        <worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
            <sheetData>
                <row r="1">
                    <c r="A1" t="s"><v>0</v></c>
                    <c r="B1" t="inlineStr"><is><t>Inline Value</t></is></c>
                </row>
                <row r="2">
                    <c r="A2"><v>123.45</v></c>
                    <c r="B2" t="b"><v>1</v></c>
                    <c r="C2" t="str"><f>CONCAT("A","B")</f><v>AB</v></c>
                </row>
            </sheetData>
        </worksheet>"#;

        zip.start_file("xl/sharedStrings.xml", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(shared_strings_xml.as_bytes()).unwrap();
        zip.start_file("xl/worksheets/sheet1.xml", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(sheet1_xml.as_bytes()).unwrap();
        zip.finish().unwrap();

        let extracted = extract_text(temp_file.path()).unwrap();
        assert!(extracted.contains("# Sheet 1"));
        assert!(extracted.contains("Header 1 | Inline Value"));
        assert!(extracted.contains("123.45 | TRUE | AB"));
    }
}
