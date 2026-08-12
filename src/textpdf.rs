use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use lopdf::{Dictionary, Document, Object, Stream, dictionary};
use ttf_parser::{Face, GlyphId};

#[derive(Clone, Debug)]
pub struct TextOptions {
    pub page_size: String,
    pub font_path: Option<PathBuf>,
    pub font_size: f64,
    pub margin: f64,
}

impl Default for TextOptions {
    fn default() -> Self {
        Self {
            page_size: "A4".to_owned(),
            font_path: None,
            font_size: 10.0,
            margin: 40.0,
        }
    }
}

pub fn render(text: &str, options: &TextOptions) -> Result<Document> {
    let font_path = find_font(options.font_path.as_deref())?;
    let font_data = fs::read(&font_path)
        .with_context(|| format!("failed to read font {}", font_path.display()))?;
    let face = Face::parse(&font_data, 0)
        .map_err(|error| anyhow::anyhow!("failed to parse {}: {error:?}", font_path.display()))?;

    let (page_width, page_height) = crate::pdf::paper_size(&options.page_size)?;
    let usable_width = page_width - 2.0 * options.margin;
    let line_height = options.font_size * 1.25;
    let lines_per_page = ((page_height - 2.0 * options.margin) / line_height)
        .floor()
        .max(1.0) as usize;
    let lines = wrap_text(text, &face, options.font_size, usable_width);
    let page_lines = if lines.is_empty() {
        vec![Vec::new()]
    } else {
        lines
            .chunks(lines_per_page)
            .map(<[String]>::to_vec)
            .collect::<Vec<_>>()
    };

    let mut used = BTreeMap::<u16, char>::new();
    let encoded_pages = page_lines
        .iter()
        .map(|lines| {
            lines
                .iter()
                .map(|line| encode_line(line, &face, &mut used))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    build_document(
        &font_data,
        &face,
        &used,
        &encoded_pages,
        options,
        PageLayout {
            width: page_width,
            height: page_height,
            line_height,
        },
    )
}

#[derive(Clone, Copy)]
struct PageLayout {
    width: f64,
    height: f64,
    line_height: f64,
}

fn build_document(
    font_data: &[u8],
    face: &Face<'_>,
    used: &BTreeMap<u16, char>,
    pages: &[Vec<Vec<u8>>],
    options: &TextOptions,
    layout: PageLayout,
) -> Result<Document> {
    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let units = f64::from(face.units_per_em());
    let scale_metric = |value: i16| f64::from(value) * 1000.0 / units;

    let mut font_stream_dict = Dictionary::new();
    font_stream_dict.set("Length1", font_data.len() as i64);
    let font_file_id = document.add_object(Stream::new(font_stream_dict, font_data.to_vec()));

    let bounds = face.global_bounding_box();
    let descriptor_id = document.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "BpdfEmbedded",
        "Flags" => 32,
        "FontBBox" => vec![
            scale_metric(bounds.x_min).into(),
            scale_metric(bounds.y_min).into(),
            scale_metric(bounds.x_max).into(),
            scale_metric(bounds.y_max).into(),
        ],
        "ItalicAngle" => 0,
        "Ascent" => scale_metric(face.ascender()),
        "Descent" => scale_metric(face.descender()),
        "CapHeight" => scale_metric(face.ascender()),
        "StemV" => 80,
        "FontFile2" => font_file_id,
    });

    let mut widths = Vec::<Object>::new();
    for glyph_id in used.keys() {
        let advance = face
            .glyph_hor_advance(GlyphId(*glyph_id))
            .unwrap_or(face.units_per_em());
        widths.push(i64::from(*glyph_id).into());
        widths.push(Object::Array(vec![
            (f64::from(advance) * 1000.0 / units).into(),
        ]));
    }

    let cid_font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => "BpdfEmbedded",
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::string_literal("Adobe"),
            "Ordering" => Object::string_literal("Identity"),
            "Supplement" => 0,
        },
        "FontDescriptor" => descriptor_id,
        "DW" => 1000,
        "W" => widths,
        "CIDToGIDMap" => "Identity",
    });
    let to_unicode_id = document.add_object(Stream::new(
        dictionary! {},
        build_to_unicode_cmap(used).into_bytes(),
    ));
    let type0_font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "BpdfEmbedded",
        "Encoding" => "Identity-H",
        "DescendantFonts" => vec![Object::Reference(cid_font_id)],
        "ToUnicode" => to_unicode_id,
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! {
            "F0" => type0_font_id,
        },
    });

    let mut page_ids = Vec::with_capacity(pages.len());
    for lines in pages {
        let mut content = String::from("BT\n/F0 ");
        content.push_str(&format!("{:.3} Tf\n", options.font_size));
        let start_y = layout.height - options.margin - options.font_size;
        for (index, encoded) in lines.iter().enumerate() {
            let y = start_y - index as f64 * layout.line_height;
            content.push_str(&format!(
                "1 0 0 1 {:.3} {:.3} Tm\n<{}> Tj\n",
                options.margin,
                y,
                hex(encoded)
            ));
        }
        content.push_str("ET\n");

        let content_id = document.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), layout.width.into(), layout.height.into()],
            "Resources" => resources_id,
            "Contents" => content_id,
        });
        page_ids.push(Object::Reference(page_id));
    }

    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => page_ids,
            "Count" => pages.len() as i64,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    Ok(document)
}

fn wrap_text(text: &str, face: &Face<'_>, font_size: f64, max_width: f64) -> Vec<String> {
    let mut output = Vec::new();

    for paragraph in text.replace("\r\n", "\n").split('\n') {
        if paragraph.trim().is_empty() {
            output.push(String::new());
            continue;
        }

        let mut line = String::new();
        let mut line_width = 0.0;
        for word in paragraph.split_inclusive(char::is_whitespace) {
            let word_width = text_width(word, face, font_size);
            if line.is_empty() || line_width + word_width <= max_width {
                line.push_str(word);
                line_width += word_width;
                continue;
            }
            output.extend(break_long_line(line.trim_end(), face, font_size, max_width));
            let trimmed = word.trim_start();
            line = trimmed.to_owned();
            line_width = text_width(trimmed, face, font_size);
        }
        output.extend(break_long_line(line.trim_end(), face, font_size, max_width));
    }

    output
}

fn break_long_line(line: &str, face: &Face<'_>, font_size: f64, max_width: f64) -> Vec<String> {
    if line.is_empty() || text_width(line, face, font_size) <= max_width {
        return vec![line.to_owned()];
    }

    let mut result = Vec::new();
    let mut chunk = String::new();
    let mut chunk_width = 0.0;
    let units = f64::from(face.units_per_em());
    for character in line.chars() {
        let glyph = face
            .glyph_index(character)
            .or_else(|| face.glyph_index('?'))
            .unwrap_or(GlyphId(0));
        let char_width = f64::from(face.glyph_hor_advance(glyph).unwrap_or(0)) * font_size / units;
        if !chunk.is_empty() && chunk_width + char_width > max_width {
            result.push(std::mem::take(&mut chunk));
            chunk_width = 0.0;
        }
        chunk.push(character);
        chunk_width += char_width;
    }
    if !chunk.is_empty() {
        result.push(chunk);
    }
    result
}

fn text_width(text: &str, face: &Face<'_>, font_size: f64) -> f64 {
    let units = f64::from(face.units_per_em());
    text.chars()
        .map(|character| {
            let glyph = face
                .glyph_index(character)
                .or_else(|| face.glyph_index('?'))
                .unwrap_or(GlyphId(0));
            f64::from(face.glyph_hor_advance(glyph).unwrap_or(0)) * font_size / units
        })
        .sum()
}

fn encode_line(line: &str, face: &Face<'_>, used: &mut BTreeMap<u16, char>) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(line.len() * 2);
    for character in line.chars() {
        let display = if face.glyph_index(character).is_some() {
            character
        } else {
            '?'
        };
        let glyph = face.glyph_index(display).unwrap_or(GlyphId(0)).0;
        used.entry(glyph).or_insert(display);
        encoded.extend_from_slice(&glyph.to_be_bytes());
    }
    encoded
}

fn build_to_unicode_cmap(used: &BTreeMap<u16, char>) -> String {
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /BpdfUnicode def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    for chunk in used.iter().collect::<Vec<_>>().chunks(100) {
        cmap.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (glyph, character) in chunk {
            let utf16 = character
                .encode_utf16(&mut [0; 2])
                .iter()
                .flat_map(|unit| unit.to_be_bytes())
                .collect::<Vec<_>>();
            cmap.push_str(&format!("<{glyph:04X}> <{}>\n", hex(&utf16)));
        }
        cmap.push_str("endbfchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    cmap
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

pub(crate) fn find_font(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        bail!("configured font does not exist: {}", path.display());
    }

    let candidates = [
        r"C:\Windows\Fonts\arial.ttf",
        r"C:\Windows\Fonts\segoeui.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/Library/Fonts/Arial Unicode.ttf",
    ];
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .ok_or_else(|| {
            anyhow::anyhow!("no Unicode TrueType font found; set font_path in config.jsonc")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_system_font_and_renders_cyrillic() {
        let options = TextOptions::default();
        let Ok(mut document) = render("Первая строка\n\nВторая строка", &options)
        else {
            // Non-desktop CI images may intentionally have no fonts.
            return;
        };
        let bytes = crate::pdf::save_to_bytes(&mut document).unwrap();
        let parsed = Document::load_mem(&bytes).unwrap();
        assert_eq!(parsed.get_pages().len(), 1);
        let text = parsed.extract_text(&[1]).unwrap();
        assert!(text.contains("Первая строка"));
        assert!(text.contains("Вторая строка"));
    }

    #[test]
    fn cmap_has_unique_glyph_entries() {
        let map = BTreeMap::from([(1, 'A'), (2, 'Я')]);
        let cmap = build_to_unicode_cmap(&map);
        assert!(cmap.contains("<0001> <0041>"));
        assert!(cmap.contains("<0002> <042F>"));
    }
}
