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
            anyhow::anyhow!("no Unicode TrueType font found; set font_path in config.toml")
        })
}

#[derive(Clone, Debug)]
pub struct SearchablePageInput {
    pub jpeg_bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub words: Vec<crate::winocr::OcrWordBox>,
    pub fallback_text: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PageTextOverlay {
    pub page_id: lopdf::ObjectId,
    pub page_width: f64,
    pub page_height: f64,
    pub scaled_words: Vec<crate::winocr::OcrWordBox>,
    pub fallback_text: Option<String>,
}

pub fn overlay_searchable_text(
    document: &mut Document,
    overlays: &[PageTextOverlay],
    font_path: Option<&Path>,
) -> Result<()> {
    if overlays.is_empty() {
        return Ok(());
    }

    let font_path = find_font(font_path)?;
    let font_data = fs::read(&font_path)
        .with_context(|| format!("failed to read font {}", font_path.display()))?;
    let face = Face::parse(&font_data, 0)
        .map_err(|error| anyhow::anyhow!("failed to parse {}: {error:?}", font_path.display()))?;

    let mut used = BTreeMap::<u16, char>::new();

    // First, process all pages to build the 'used' glyph map and create content streams.
    // We cannot add objects to the document while building the font because we need all used characters first.
    let mut page_contents = Vec::with_capacity(overlays.len());

    for overlay in overlays {
        let mut content = String::from("\nq\nBT\n3 Tr\n");
        if !overlay.scaled_words.is_empty() {
            for word in &overlay.scaled_words {
                if word.text.trim().is_empty() {
                    continue;
                }
                let text = format!("{} ", word.text);
                let encoded = encode_line(&text, &face, &mut used);
                let word_pt_x = word.x;

                // We use line_height as the font size so that the entire line has a uniform font size,
                // which prevents the selection highlight from jumping in height.
                let font_size = word.line_height.max(word.height).max(4.0);

                // We position the baseline such that the top of the line bounding box matches the top of the font's Ascent.
                // Ascent is usually around 80% of the total font height. We'll compute it exactly from the font metrics.
                let _units_per_em = face.units_per_em() as f64;
                let ascender = face.ascender() as f64;
                let descender = face.descender() as f64;

                let line_top_y = overlay.page_height - word.line_y;
                let total_font_height = ascender - descender;

                // Baseline is positioned below the top of the line by the font's scaled ascent.
                let ascent_scaled = font_size * (ascender / total_font_height);
                let word_pt_y = line_top_y - ascent_scaled;

                let target_width = word.width;

                let natural_width = text_width(&text, &face, 1.0); // at 1 pt size
                let current_natural_width = natural_width * font_size;

                let scale = if current_natural_width > 0.0 {
                    (target_width / current_natural_width) * 100.0
                } else {
                    100.0
                };

                content.push_str(&format!("{:.1} Tz\n", scale));
                content.push_str(&format!("/BpdfF0 {:.3} Tf\n", font_size));
                content.push_str(&format!(
                    "1 0 0 1 {:.3} {:.3} Tm\n<{}> Tj\n",
                    word_pt_x,
                    word_pt_y,
                    hex(&encoded)
                ));
            }
        } else if let Some(fallback) = &overlay.fallback_text {
            let font_size = 10.0;
            let line_height = font_size * 1.25;
            let lines = wrap_text(fallback, &face, font_size, overlay.page_width - 40.0);
            content.push_str(&format!("100 Tz\n/BpdfF0 {:.3} Tf\n", font_size));
            let mut y = overlay.page_height - 20.0 - font_size;
            for line in lines {
                if !line.trim().is_empty() {
                    let text = format!("{} ", line);
                    let encoded = encode_line(&text, &face, &mut used);
                    content.push_str(&format!(
                        "1 0 0 1 20.000 {:.3} Tm\n<{}> Tj\n",
                        y,
                        hex(&encoded)
                    ));
                }
                y -= line_height;
                if y < 20.0 {
                    break;
                }
            }
        }
        content.push_str("100 Tz\nET\nQ\n");
        page_contents.push((overlay.page_id, content));
    }

    // Now that 'used' is fully populated, build the font objects.
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
        build_to_unicode_cmap(&used).into_bytes(),
    ));
    let type0_font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "BpdfEmbedded",
        "Encoding" => "Identity-H",
        "DescendantFonts" => vec![Object::Reference(cid_font_id)],
        "ToUnicode" => to_unicode_id,
    });

    // Inject the new text stream into each page
    for (page_id, content) in page_contents {
        let new_content_id = document.add_object(Stream::new(dictionary! {}, content.into_bytes()));

        let mut resource_id_to_update = None;

        {
            let page = document.get_object_mut(page_id)?.as_dict_mut()?;

            // Ensure Resources dictionary exists and has Font dict
            match page.get_mut(b"Resources") {
                Ok(Object::Reference(id)) => {
                    resource_id_to_update = Some(*id);
                }
                Ok(Object::Dictionary(r)) => {
                    let font_dict = match r.get_mut(b"Font") {
                        Ok(Object::Dictionary(f)) => f,
                        _ => {
                            r.set("Font", dictionary! {});
                            r.get_mut(b"Font").unwrap().as_dict_mut().unwrap()
                        }
                    };
                    font_dict.set("BpdfF0", type0_font_id);
                }
                _ => {
                    let mut r = dictionary! {};
                    r.set("Font", dictionary! { "BpdfF0" => type0_font_id });
                    page.set("Resources", r);
                }
            };

            // Append to Contents
            match page.get(b"Contents").cloned() {
                Ok(Object::Array(mut arr)) => {
                    arr.push(Object::Reference(new_content_id));
                    page.set("Contents", Object::Array(arr));
                }
                Ok(Object::Reference(id)) => {
                    page.set(
                        "Contents",
                        vec![Object::Reference(id), Object::Reference(new_content_id)],
                    );
                }
                Ok(val) => {
                    page.set("Contents", vec![val, Object::Reference(new_content_id)]);
                }
                Err(_) => {
                    page.set("Contents", Object::Reference(new_content_id));
                }
            }
        }

        if let Some(res_id) = resource_id_to_update {
            let res_dict = document.get_object_mut(res_id)?.as_dict_mut()?;
            let font_dict = match res_dict.get_mut(b"Font") {
                Ok(Object::Dictionary(f)) => f,
                _ => {
                    res_dict.set("Font", dictionary! {});
                    res_dict.get_mut(b"Font").unwrap().as_dict_mut().unwrap()
                }
            };
            font_dict.set("BpdfF0", type0_font_id);
        }
    }

    Ok(())
}

pub fn render_searchable_pdf(
    pages: &[SearchablePageInput],
    page_size: &str,
    font_path: Option<&Path>,
) -> Result<Document> {
    if pages.is_empty() {
        bail!("no pages to render in searchable PDF");
    }
    let font_path = find_font(font_path)?;
    let font_data = fs::read(&font_path)
        .with_context(|| format!("failed to read font {}", font_path.display()))?;
    let face = Face::parse(&font_data, 0)
        .map_err(|error| anyhow::anyhow!("failed to parse {}: {error:?}", font_path.display()))?;

    let mut used = BTreeMap::<u16, char>::new();
    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();

    let mut page_ids = Vec::with_capacity(pages.len());

    for page_input in pages {
        let (page_width, page_height) = if page_size.eq_ignore_ascii_case("none")
            || page_size.eq_ignore_ascii_case("original")
            || page_size.eq_ignore_ascii_case("keep")
        {
            (
                f64::from(page_input.width) * 72.0 / 150.0,
                f64::from(page_input.height) * 72.0 / 150.0,
            )
        } else {
            let (mut pw, mut ph) = crate::pdf::paper_size(page_size)?;
            if (page_input.width > page_input.height) != (pw > ph) {
                std::mem::swap(&mut pw, &mut ph);
            }
            (pw, ph)
        };

        let image_stream = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => page_input.width as i64,
                "Height" => page_input.height as i64,
                "ColorSpace" => "DeviceRGB",
                "BitsPerComponent" => 8,
                "Filter" => "DCTDecode",
            },
            page_input.jpeg_bytes.clone(),
        );
        let image_id = document.add_object(image_stream);

        let mut content = format!(
            "q\n{:.4} 0 0 {:.4} 0 0 cm\n/Im0 Do\nQ\nBT\n3 Tr\n",
            page_width, page_height
        );

        let scale_x = page_width / f64::from(page_input.width.max(1));
        let scale_y = page_height / f64::from(page_input.height.max(1));

        if !page_input.words.is_empty() {
            for word in &page_input.words {
                if word.text.trim().is_empty() {
                    continue;
                }
                let encoded = encode_line(&word.text, &face, &mut used);
                let word_pt_x = word.x * scale_x;
                let word_pt_y = page_height - (word.y + word.height) * scale_y;
                let font_size = (word.height * scale_y).max(4.0);

                content.push_str(&format!("/F0 {:.3} Tf\n", font_size));
                content.push_str(&format!(
                    "1 0 0 1 {:.3} {:.3} Tm\n<{}> Tj\n",
                    word_pt_x,
                    word_pt_y,
                    hex(&encoded)
                ));
            }
        } else if let Some(fallback) = &page_input.fallback_text {
            let font_size = 10.0;
            let line_height = font_size * 1.25;
            let lines = wrap_text(fallback, &face, font_size, page_width - 40.0);
            content.push_str(&format!("/F0 {:.3} Tf\n", font_size));
            let mut y = page_height - 20.0 - font_size;
            for line in lines {
                if !line.trim().is_empty() {
                    let encoded = encode_line(&line, &face, &mut used);
                    content.push_str(&format!(
                        "1 0 0 1 20.000 {:.3} Tm\n<{}> Tj\n",
                        y,
                        hex(&encoded)
                    ));
                }
                y -= line_height;
                if y < 20.0 {
                    break;
                }
            }
        }
        content.push_str("ET\n");

        let content_id = document.add_object(Stream::new(dictionary! {}, content.into_bytes()));

        let page_dict = dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), page_width.into(), page_height.into()],
            "Contents" => content_id,
        };
        let page_id = document.add_object(page_dict);
        page_ids.push((page_id, image_id));
    }

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
        build_to_unicode_cmap(&used).into_bytes(),
    ));
    let type0_font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "BpdfEmbedded-Identity-H",
        "Encoding" => "Identity-H",
        "DescendantFonts" => vec![Object::Reference(cid_font_id)],
        "ToUnicode" => to_unicode_id,
    });

    for (page_id, image_id) in &page_ids {
        let resources = dictionary! {
            "Font" => dictionary! {
                "F0" => type0_font_id,
            },
            "XObject" => dictionary! {
                "Im0" => *image_id,
            },
        };
        let resources_id = document.add_object(resources);
        document
            .get_object_mut(*page_id)?
            .as_dict_mut()?
            .set("Resources", resources_id);
    }

    let page_obj_ids = page_ids
        .iter()
        .map(|(pid, _)| Object::Reference(*pid))
        .collect::<Vec<_>>();
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => page_obj_ids,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_system_font_and_renders_cyrillic() {
        let options = TextOptions::default();
        let Ok(mut document) = render("Первая строка\n\nВторая строка", &options)
        else {
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
    fn render_searchable_pdf_creates_text_layer_and_image() {
        let dummy_jpeg = vec![
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00, 0x01, 0x01, 0x01,
            0x00, 0x48, 0x00, 0x48, 0x00, 0x00, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x08, 0x06, 0x06,
            0x07, 0x06, 0x05, 0x08, 0x07, 0x07, 0x07, 0x09, 0x09, 0x08, 0x0A, 0x0C, 0x14, 0x0D,
            0x0C, 0x0B, 0x0B, 0x0C, 0x19, 0x12, 0x13, 0x0F, 0x14, 0x1D, 0x1A, 0x1F, 0x1E, 0x1D,
            0x1A, 0x1C, 0x1C, 0x20, 0x24, 0x2E, 0x27, 0x20, 0x22, 0x2C, 0x23, 0x1C, 0x1C, 0x28,
            0x37, 0x29, 0x2C, 0x30, 0x31, 0x34, 0x34, 0x34, 0x1F, 0x27, 0x39, 0x3D, 0x38, 0x32,
            0x3C, 0x2E, 0x33, 0x34, 0x32, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x02, 0x00, 0x02,
            0x01, 0x01, 0x11, 0x00, 0xFF, 0xC4, 0x00, 0x1F, 0x00, 0x00, 0x01, 0x05, 0x01, 0x01,
            0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02,
            0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0xFF, 0xDA, 0x00, 0x08, 0x01,
            0x01, 0x00, 0x00, 0x3F, 0x00, 0x7F, 0x00, 0xFF, 0xD9,
        ];
        let page_input = SearchablePageInput {
            jpeg_bytes: dummy_jpeg,
            width: 2,
            height: 2,
            words: vec![crate::winocr::OcrWordBox {
                text: "ТестовоеСлово".to_owned(),
                x: 0.0,
                y: 0.0,
                width: 2.0,
                height: 2.0,
                line_y: 0.0,
                line_height: 2.0,
            }],
            fallback_text: None,
        };
        let Ok(mut document) = render_searchable_pdf(&[page_input], "A4", None) else {
            return;
        };
        let bytes = crate::pdf::save_to_bytes(&mut document).unwrap();
        let parsed = Document::load_mem(&bytes).unwrap();
        assert_eq!(parsed.get_pages().len(), 1);
        let text = parsed.extract_text(&[1]).unwrap();
        assert!(text.contains("ТестовоеСлово"));
    }

    #[test]
    fn cmap_has_unique_glyph_entries() {
        let map = BTreeMap::from([(1, 'A'), (2, 'Я')]);
        let cmap = build_to_unicode_cmap(&map);
        assert!(cmap.contains("<0001> <0041>"));
        assert!(cmap.contains("<0002> <042F>"));
    }
}
