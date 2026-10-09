use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use lopdf::{Document, Object, ObjectId, Stream, dictionary};
use ttf_parser::{Face, GlyphId};

use crate::font_subset;
use crate::winocr::OcrWordBox;

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

/// Recognised words of one existing PDF page, in page units measured from
/// the top-left corner.
#[derive(Clone, Debug)]
pub struct PageTextOverlay {
    pub page_id: ObjectId,
    pub page_width: f64,
    pub page_height: f64,
    pub scaled_words: Vec<OcrWordBox>,
    pub fallback_text: Option<String>,
}

/// A loaded TrueType font that records which glyphs a document uses, so only
/// those are embedded.
struct FontWriter<'a> {
    data: &'a [u8],
    face: Face<'a>,
    used: BTreeMap<u16, char>,
    glyphs: HashMap<char, (u16, char)>,
}

/// Raw bytes of the configured or first available system Unicode font.
fn load_font(explicit: Option<&Path>) -> Result<Vec<u8>> {
    let path = find_font(explicit)?;
    fs::read(&path).with_context(|| format!("failed to read font {}", path.display()))
}

impl<'a> FontWriter<'a> {
    fn new(data: &'a [u8]) -> Result<Self> {
        let face = Face::parse(data, 0)
            .map_err(|error| anyhow::anyhow!("failed to parse font: {error:?}"))?;
        Ok(Self {
            data,
            face,
            used: BTreeMap::new(),
            glyphs: HashMap::new(),
        })
    }

    /// Glyph for a character, falling back to '?' (or .notdef); the second
    /// value is the character actually drawn.
    fn glyph(&mut self, character: char) -> (u16, char) {
        let face = &self.face;
        *self
            .glyphs
            .entry(character)
            .or_insert_with(|| match face.glyph_index(character) {
                Some(glyph) => (glyph.0, character),
                None => (face.glyph_index('?').map_or(0, |glyph| glyph.0), '?'),
            })
    }

    fn advance(&self, glyph: u16) -> f64 {
        f64::from(self.face.glyph_hor_advance(GlyphId(glyph)).unwrap_or(0))
            / f64::from(self.face.units_per_em())
    }

    fn text_width(&mut self, text: &str, font_size: f64) -> f64 {
        text.chars()
            .map(|character| {
                let glyph = self.glyph(character).0;
                self.advance(glyph)
            })
            .sum::<f64>()
            * font_size
    }

    /// Hex string of glyph ids for a `Tj` operator; records used glyphs.
    fn encode(&mut self, text: &str) -> String {
        let mut hex = String::with_capacity(text.len() * 4);
        for character in text.chars() {
            let (glyph, drawn) = self.glyph(character);
            self.used.entry(glyph).or_insert(drawn);
            let _ = write!(hex, "{glyph:04X}");
        }
        hex
    }

    fn wrap(&mut self, text: &str, font_size: f64, max_width: f64) -> Vec<String> {
        let mut output = Vec::new();
        for paragraph in text.lines() {
            if paragraph.trim().is_empty() {
                output.push(String::new());
                continue;
            }
            let mut line = String::new();
            let mut line_width = 0.0;
            for word in paragraph.split_inclusive(char::is_whitespace) {
                let word_width = self.text_width(word, font_size);
                if line.is_empty() || line_width + word_width <= max_width {
                    line.push_str(word);
                    line_width += word_width;
                    continue;
                }
                self.break_long_line(line.trim_end(), font_size, max_width, &mut output);
                line = word.trim_start().to_owned();
                line_width = self.text_width(&line, font_size);
            }
            self.break_long_line(line.trim_end(), font_size, max_width, &mut output);
        }
        output
    }

    fn break_long_line(
        &mut self,
        line: &str,
        font_size: f64,
        max_width: f64,
        output: &mut Vec<String>,
    ) {
        let mut chunk = String::new();
        let mut chunk_width = 0.0;
        for character in line.chars() {
            let glyph = self.glyph(character).0;
            let width = self.advance(glyph) * font_size;
            if !chunk.is_empty() && chunk_width + width > max_width {
                output.push(std::mem::take(&mut chunk));
                chunk_width = 0.0;
            }
            chunk.push(character);
            chunk_width += width;
        }
        output.push(chunk);
    }

    /// Write the subset font objects and return the Type0 font id.
    fn embed(self, document: &mut Document) -> ObjectId {
        let units = f64::from(self.face.units_per_em());
        let metric = |value: i16| f64::from(value) * 1000.0 / units;

        let used_glyphs = self.used.keys().copied().collect::<BTreeSet<_>>();
        let font_data = font_subset::subset_truetype(self.data, &used_glyphs)
            .unwrap_or_else(|| self.data.to_vec());
        let font_file_id = document.add_object(Stream::new(
            dictionary! { "Length1" => font_data.len() as i64 },
            font_data,
        ));

        let bounds = self.face.global_bounding_box();
        let descriptor_id = document.add_object(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "BpdfEmbedded",
            "Flags" => 32,
            "FontBBox" => vec![
                metric(bounds.x_min).into(),
                metric(bounds.y_min).into(),
                metric(bounds.x_max).into(),
                metric(bounds.y_max).into(),
            ],
            "ItalicAngle" => 0,
            "Ascent" => metric(self.face.ascender()),
            "Descent" => metric(self.face.descender()),
            "CapHeight" => metric(self.face.ascender()),
            "StemV" => 80,
            "FontFile2" => font_file_id,
        });

        let widths = self
            .used
            .keys()
            .flat_map(|glyph| {
                [
                    Object::from(i64::from(*glyph)),
                    Object::Array(vec![(self.advance(*glyph) * 1000.0).into()]),
                ]
            })
            .collect::<Vec<_>>();
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
            build_to_unicode_cmap(&self.used).into_bytes(),
        ));
        document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type0",
            "BaseFont" => "BpdfEmbedded",
            "Encoding" => "Identity-H",
            "DescendantFonts" => vec![Object::Reference(cid_font_id)],
            "ToUnicode" => to_unicode_id,
        })
    }
}

pub fn render(text: &str, options: &TextOptions) -> Result<Document> {
    let text = text.trim_start_matches('\u{feff}');
    let font_data = load_font(options.font_path.as_deref())?;
    let mut font = FontWriter::new(&font_data)?;

    let (page_width, page_height) = crate::pdf::paper_size(&options.page_size)?;
    let line_height = options.font_size * 1.25;
    let lines_per_page = ((page_height - 2.0 * options.margin) / line_height)
        .floor()
        .max(1.0) as usize;
    let lines = font.wrap(text, options.font_size, page_width - 2.0 * options.margin);
    let start_y = page_height - options.margin - options.font_size;

    let mut contents = Vec::new();
    for page_lines in lines.chunks(lines_per_page) {
        // `T*` line advances, unlike absolute `Tm` moves, also tell text
        // extractors where lines end.
        let mut content = format!(
            "BT\n/F0 {:.3} Tf\n{line_height:.3} TL\n{:.3} {start_y:.3} Td\n",
            options.font_size, options.margin
        );
        for (index, line) in page_lines.iter().enumerate() {
            if index > 0 {
                content.push_str("T*\n");
            }
            let _ = writeln!(content, "<{}> Tj", font.encode(line));
        }
        content.push_str("ET\n");
        contents.push(content);
    }
    if contents.is_empty() {
        contents.push("BT\nET\n".to_owned());
    }

    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let font_id = font.embed(&mut document);
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F0" => font_id },
    });
    let kids = contents
        .into_iter()
        .map(|content| {
            let content_id = document.add_object(Stream::new(dictionary! {}, content.into_bytes()));
            Object::Reference(document.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![0.into(), 0.into(), page_width.into(), page_height.into()],
                "Resources" => resources_id,
                "Contents" => content_id,
            }))
        })
        .collect::<Vec<_>>();
    let count = kids.len() as i64;
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count }),
    );
    let catalog_id = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    document.trailer.set("Root", catalog_id);
    Ok(document)
}

fn build_to_unicode_cmap(used: &BTreeMap<u16, char>) -> String {
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /BpdfUnicode def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    for chunk in used.iter().collect::<Vec<_>>().chunks(100) {
        let _ = writeln!(cmap, "{} beginbfchar", chunk.len());
        for (glyph, character) in chunk {
            let _ = write!(cmap, "<{glyph:04X}> <");
            for unit in character.encode_utf16(&mut [0; 2]) {
                let _ = write!(cmap, "{unit:04X}");
            }
            cmap.push_str(">\n");
        }
        cmap.push_str("endbfchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    cmap
}

pub(crate) fn find_font(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        bail!("configured font does not exist: {}", path.display());
    }

    [
        r"C:\Windows\Fonts\arial.ttf",
        r"C:\Windows\Fonts\segoeui.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/Library/Fonts/Arial Unicode.ttf",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
    .context("no Unicode TrueType font found; set font_path in config.toml")
}

/// Add an invisible (render mode 3) text layer to existing pages.
pub fn overlay_searchable_text(
    document: &mut Document,
    overlays: &[PageTextOverlay],
    font_path: Option<&Path>,
) -> Result<()> {
    if overlays.is_empty() {
        return Ok(());
    }
    let page_ids = overlays
        .iter()
        .map(|overlay| overlay.page_id)
        .collect::<Vec<_>>();
    let font_name =
        crate::pdf::transform::free_resource_name(document, &page_ids, b"Font", "BpdfF0");
    let font_data = load_font(font_path)?;
    let mut font = FontWriter::new(&font_data)?;
    let ascender = f64::from(font.face.ascender());
    let ascent_ratio = ascender / (ascender - f64::from(font.face.descender()));

    // Glyph usage must be complete before the font is embedded.
    let mut page_contents = Vec::with_capacity(overlays.len());
    for overlay in overlays {
        let mut content = String::from("\nq\nBT\n3 Tr\n");
        if !overlay.scaled_words.is_empty() {
            let mut line_y = None;
            for word in &overlay.scaled_words {
                if word.text.trim().is_empty() {
                    continue;
                }
                // One text object per line: text extractors end a line at
                // `ET`, while render mode and scaling carry over to the next.
                if line_y.is_some_and(|y| y != word.line_y) {
                    content.push_str("ET\nBT\n");
                }
                line_y = Some(word.line_y);
                let text = format!("{} ", word.text);
                // A uniform size per line keeps the selection highlight steady;
                // the baseline sits one ascent below the line top.
                let font_size = word.line_height.max(word.height).max(4.0);
                let baseline = overlay.page_height - word.line_y - font_size * ascent_ratio;
                let natural_width = font.text_width(&text, font_size);
                let horizontal_scale = if natural_width > 0.0 {
                    word.width / natural_width * 100.0
                } else {
                    100.0
                };
                let _ = write!(
                    content,
                    "{horizontal_scale:.1} Tz\n/{font_name} {font_size:.3} Tf\n1 0 0 1 {:.3} {baseline:.3} Tm\n<{}> Tj\n",
                    word.x,
                    font.encode(&text)
                );
            }
        } else if let Some(fallback) = &overlay.fallback_text {
            let font_size = 10.0;
            let _ = write!(content, "100 Tz\n/{font_name} {font_size:.3} Tf\n");
            let mut y = overlay.page_height - 20.0 - font_size;
            for line in font.wrap(fallback, font_size, overlay.page_width - 40.0) {
                if !line.trim().is_empty() {
                    let hex = font.encode(&format!("{line} "));
                    let _ = write!(content, "1 0 0 1 20.000 {y:.3} Tm\n<{hex}> Tj\nET\nBT\n");
                }
                y -= font_size * 1.25;
                if y < 20.0 {
                    break;
                }
            }
        }
        content.push_str("100 Tz\nET\nQ\n");
        page_contents.push((overlay.page_id, content));
    }

    let font_id = font.embed(document);
    for (page_id, content) in page_contents {
        let content_id = document.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        crate::pdf::transform::install_resources(
            document,
            page_id,
            &[(b"Font", font_name.as_bytes(), font_id)],
        )?;
        let old = document
            .get_dictionary(page_id)?
            .get(b"Contents")
            .ok()
            .cloned();
        let mut contents = Vec::new();
        crate::pdf::transform::append_content_objects(document, &mut contents, old);
        contents.push(Object::Reference(content_id));
        document
            .get_object_mut(page_id)?
            .as_dict_mut()?
            .set("Contents", contents);
    }
    Ok(())
}

/// Appearance of text drawn onto existing pages (page numbers, watermarks).
#[derive(Clone, Debug)]
pub struct TextMarkStyle {
    /// Font size in points; 0 fits the text to the page.
    pub font_size: f64,
    /// Anchor (br, bc, c, ...) or X,Y mm offset, as for stamps.
    pub position: String,
    /// Counter-clockwise angle relative to the displayed page, in degrees.
    pub angle: f64,
    pub opacity: f64,
    pub color: [f64; 3],
    pub pages: String,
    pub under: bool,
}

/// Draw `text_for(page_number, page_count)` on the selected pages. Text stays
/// horizontal relative to the displayed page whatever its /Rotate value.
pub fn add_text_marks(
    document: &mut Document,
    style: &TextMarkStyle,
    font_path: Option<&Path>,
    text_for: impl Fn(usize, usize) -> String,
) -> Result<()> {
    use crate::pdf::transform::{self, ContentIsolation};

    if !(0.0..=1.0).contains(&style.opacity) {
        bail!("opacity must be between 0 and 1");
    }
    if style.font_size < 0.0 {
        bail!("font size cannot be negative");
    }
    let page_count = document.get_pages().len();
    let pages = transform::selected_pages(document, &style.pages)?;
    let page_ids = pages.iter().map(|(_, id, _)| *id).collect::<Vec<_>>();
    let font_name = transform::free_resource_name(document, &page_ids, b"Font", "BpdfMark");
    let state_name = transform::free_resource_name(document, &page_ids, b"ExtGState", "BpdfMarkGS");
    let font_data = load_font(font_path)?;
    let mut font = FontWriter::new(&font_data)?;
    let (sin, cos) = style.angle.to_radians().sin_cos();

    let mut contents = Vec::with_capacity(pages.len());
    for (number, page_id, geometry) in pages {
        let text = text_for(number as usize, page_count);
        if text.trim().is_empty() {
            continue;
        }
        let display = geometry.displayed();
        let unit_width = font.text_width(&text, 1.0);
        let font_size = if style.font_size > 0.0 {
            style.font_size
        } else if unit_width > 0.0 {
            // Fit the text along its direction, never taller than a fifth of the page.
            let span = if sin.abs() > 0.01 {
                display.raw_width().hypot(display.raw_height()) * 0.6
            } else {
                display.raw_width() * 0.8
            };
            (span / unit_width).min(display.raw_height() * 0.2)
        } else {
            12.0
        };
        let (width, height) = (unit_width * font_size, font_size * 0.7);

        // Centre of the text box on the displayed page, then the baseline
        // start rotated around it.
        let (left, bottom) =
            transform::stamp_position(&style.position, display, width, height, 1.0)?;
        let (center_x, center_y) = (left + width / 2.0, bottom + height / 2.0);
        let (dx, dy) = (-width / 2.0, -height / 2.0);
        let (origin_x, origin_y) = geometry.display_to_page(
            center_x + dx * cos - dy * sin,
            center_y + dx * sin + dy * cos,
        );
        let (page_sin, page_cos) = (style.angle + geometry.rotation as f64)
            .to_radians()
            .sin_cos();

        let [red, green, blue] = style.color;
        let mut content = String::from("q\n");
        if style.opacity < 1.0 {
            let _ = writeln!(content, "/{state_name} gs");
        }
        let _ = write!(
            content,
            "{red:.3} {green:.3} {blue:.3} rg\nBT\n/{font_name} {font_size:.3} Tf\n\
             {page_cos:.6} {page_sin:.6} {:.6} {page_cos:.6} {origin_x:.3} {origin_y:.3} Tm\n\
             <{}> Tj\nET\nQ\n",
            -page_sin,
            font.encode(&text)
        );
        contents.push((page_id, content));
    }

    let font_id = font.embed(document);
    let state_id = (style.opacity < 1.0).then(|| {
        document.add_object(dictionary! {
            "Type" => "ExtGState",
            "ca" => style.opacity,
            "CA" => style.opacity,
        })
    });
    let isolation = ContentIsolation::new(document);
    for (page_id, content) in contents {
        let mut resources = vec![(b"Font".as_slice(), font_name.as_bytes(), font_id)];
        if let Some(state_id) = state_id {
            resources.push((b"ExtGState", state_name.as_bytes(), state_id));
        }
        transform::install_resources(document, page_id, &resources)?;
        let content_id = document.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        isolation.add_overlay(document, page_id, content_id, style.under)?;
    }
    Ok(())
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
        assert!(
            text.contains(
                "Первая строка
Вторая строка"
            ),
            "{text:?}"
        );
        // The embedded font is a subset, far smaller than a system font.
        assert!(bytes.len() < 300_000, "{} bytes", bytes.len());
    }

    #[test]
    fn overlay_adds_searchable_text_to_an_image_page() {
        let jpeg = crate::commands::common::test_utils::sample_jpeg_bytes(20, 10);
        let mut document = crate::pdf::jpeg_document(jpeg, "A4").unwrap();
        let page_id = *document.get_pages().get(&1).unwrap();
        let overlay = PageTextOverlay {
            page_id,
            page_width: 842.0,
            page_height: 595.0,
            scaled_words: ["ТестовоеСлово", "второе", "Следующая"]
                .into_iter()
                .enumerate()
                .map(|(index, text)| OcrWordBox {
                    text: text.to_owned(),
                    x: 10.0 + 110.0 * (index % 2) as f64,
                    y: 10.0,
                    width: 100.0,
                    height: 12.0,
                    line_y: if index < 2 { 10.0 } else { 30.0 },
                    line_height: 12.0,
                })
                .collect(),
            fallback_text: None,
        };
        if overlay_searchable_text(&mut document, &[overlay], None).is_err() {
            return;
        }
        let bytes = crate::pdf::save_to_bytes(&mut document).unwrap();
        let parsed = Document::load_mem(&bytes).unwrap();
        let text = parsed.extract_text(&[1]).unwrap();
        assert!(
            text.contains(
                "ТестовоеСлово второе 
Следующая"
            ),
            "{text:?}"
        );
        let page_id = *parsed.get_pages().get(&1).unwrap();
        assert_eq!(parsed.get_page_images(page_id).unwrap().len(), 1);
    }

    #[test]
    fn cmap_has_unique_glyph_entries() {
        let map = BTreeMap::from([(1, 'A'), (2, 'Я')]);
        let cmap = build_to_unicode_cmap(&map);
        assert!(cmap.contains("<0001> <0041>"));
        assert!(cmap.contains("<0002> <042F>"));
    }

    #[test]
    fn handles_utf8_bom_cleanly() {
        let text = "\u{feff}Header Line\nSecond Line";
        let options = TextOptions::default();
        if let Ok(mut doc) = render(text, &options) {
            let bytes = crate::pdf::save_to_bytes(&mut doc).unwrap();
            let parsed = Document::load_mem(&bytes).unwrap();
            let extracted = parsed.extract_text(&[1]).unwrap();
            assert!(extracted.contains("Header Line"));
            assert!(!extracted.contains('\u{feff}'));
        }
    }

    #[test]
    fn page_numbers_are_upright_on_rotated_pages() {
        let jpeg = crate::commands::common::test_utils::sample_jpeg_bytes(20, 10);
        let mut document = crate::pdf::jpeg_document(jpeg, "A4").unwrap();
        crate::pdf::transform::rotate_pages(&mut document, "all", 90).unwrap();
        let style = TextMarkStyle {
            font_size: 10.0,
            position: "bc".to_owned(),
            angle: 0.0,
            opacity: 1.0,
            color: [0.0, 0.0, 0.0],
            pages: "all".to_owned(),
            under: false,
        };
        if add_text_marks(&mut document, &style, None, |number, total| {
            format!("Стр. {number} из {total}")
        })
        .is_err()
        {
            return;
        }
        let bytes = crate::pdf::save_to_bytes(&mut document).unwrap();
        let parsed = Document::load_mem(&bytes).unwrap();
        assert!(parsed.extract_text(&[1]).unwrap().contains("Стр. 1 из 1"));

        // Rotated 90 degrees in page space so it reads horizontally on screen.
        let contents = parsed.get_page_content(*parsed.get_pages().get(&1).unwrap());
        let text = String::from_utf8_lossy(&contents);
        assert!(
            text.contains("0.000000 1.000000 -1.000000 0.000000"),
            "{text}"
        );
    }

    #[test]
    fn a_second_mark_run_keeps_the_first_visible() {
        let jpeg = crate::commands::common::test_utils::sample_jpeg_bytes(20, 10);
        let mut document = crate::pdf::jpeg_document(jpeg, "A4").unwrap();
        let style = TextMarkStyle {
            font_size: 10.0,
            position: "bc".to_owned(),
            angle: 0.0,
            opacity: 1.0,
            color: [0.0, 0.0, 0.0],
            pages: "all".to_owned(),
            under: false,
        };
        if add_text_marks(&mut document, &style, None, |_, _| "Первый".to_owned()).is_err() {
            return;
        }
        add_text_marks(&mut document, &style, None, |_, _| "Второй".to_owned()).unwrap();

        let page_id = *document.get_pages().get(&1).unwrap();
        let fonts = document.get_page_fonts(page_id).unwrap();
        assert_eq!(fonts.len(), 2, "each run keeps its own font resource");
        let text = document.extract_text(&[1]).unwrap();
        assert!(text.contains("Первый") && text.contains("Второй"), "{text}");
    }
}
