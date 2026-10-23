//! Minimal TrueType subsetter for embedded PDF fonts.
//!
//! Glyph ids are preserved (so `CIDToGIDMap /Identity` stays valid): unused
//! glyph outlines are emptied, tables PDF viewers do not need for rendering
//! (layout, kerning, device metrics) are dropped, and `loca` is rewritten in
//! the long format. The first font of a TrueType collection (`.ttc`) is
//! written out as a standalone font. Anything unexpected returns `None` and
//! the caller embeds [`standalone_truetype`] instead.

use std::collections::BTreeSet;

/// Tables kept in the subset; everything else is irrelevant to PDF rendering.
const KEPT_TABLES: [&[u8; 4]; 12] = [
    b"OS/2", b"cmap", b"cvt ", b"fpgm", b"gasp", b"glyf", b"head", b"hhea", b"hmtx", b"loca",
    b"maxp", b"prep",
];

struct Table<'a> {
    tag: [u8; 4],
    data: &'a [u8],
}

pub fn subset_truetype(font: &[u8], used_glyphs: &BTreeSet<u16>) -> Option<Vec<u8>> {
    let tables = read_tables(font, face_offset(font)?)?;
    let table = |tag: &[u8; 4]| {
        tables
            .iter()
            .find(|table| &table.tag == tag)
            .map(|table| table.data)
    };
    let head = table(b"head")?;
    let glyph_count = usize::from(read_u16(table(b"maxp")?, 4)?);
    let long_loca = read_u16(head, 50)? == 1;
    let loca = table(b"loca")?;
    let glyf = table(b"glyf")?;

    let offset = |glyph: usize| -> Option<usize> {
        if long_loca {
            read_u32(loca, glyph * 4).map(|value| value as usize)
        } else {
            read_u16(loca, glyph * 2).map(|value| usize::from(value) * 2)
        }
    };
    let glyph_data = |glyph: usize| -> Option<&[u8]> {
        let (start, end) = (offset(glyph)?, offset(glyph + 1)?);
        (start <= end).then(|| glyf.get(start..end)).flatten()
    };

    // Close the set over composite glyph components; glyph 0 is .notdef.
    let mut keep = BTreeSet::from([0u16]);
    let mut pending = used_glyphs
        .iter()
        .copied()
        .filter(|glyph| usize::from(*glyph) < glyph_count)
        .collect::<Vec<_>>();
    while let Some(glyph) = pending.pop() {
        if !keep.insert(glyph) && glyph != 0 {
            continue;
        }
        let data = glyph_data(usize::from(glyph))?;
        if data.len() >= 10 && (read_u16(data, 0)? as i16) < 0 {
            for component in composite_components(data)? {
                if usize::from(component) < glyph_count && !keep.contains(&component) {
                    pending.push(component);
                }
            }
        }
    }

    let mut new_glyf = Vec::new();
    let mut new_loca = Vec::with_capacity((glyph_count + 1) * 4);
    for glyph in 0..glyph_count {
        new_loca.extend_from_slice(&u32::try_from(new_glyf.len()).ok()?.to_be_bytes());
        if keep.contains(&(glyph as u16)) {
            new_glyf.extend_from_slice(glyph_data(glyph)?);
            new_glyf.resize(new_glyf.len().next_multiple_of(4), 0);
        }
    }
    new_loca.extend_from_slice(&u32::try_from(new_glyf.len()).ok()?.to_be_bytes());

    let mut new_head = head.to_vec();
    new_head.get_mut(8..12)?.fill(0); // checkSumAdjustment, fixed below
    new_head
        .get_mut(50..52)?
        .copy_from_slice(&1u16.to_be_bytes());

    let output_tables = tables
        .iter()
        .filter(|table| KEPT_TABLES.contains(&&table.tag))
        .map(|table| {
            let data = match &table.tag {
                b"glyf" => new_glyf.as_slice(),
                b"loca" => new_loca.as_slice(),
                b"head" => new_head.as_slice(),
                _ => table.data,
            };
            (table.tag, data)
        })
        .collect::<Vec<_>>();
    finish_font(output_tables)
}

/// The font as a standalone TrueType file: unchanged for a `.ttf`, the first
/// font of a collection otherwise. `None` for CFF outlines and broken files.
pub fn standalone_truetype(font: &[u8]) -> Option<Vec<u8>> {
    let offset = face_offset(font)?;
    if offset == 0 {
        return Some(font.to_vec());
    }
    let tables = read_tables(font, offset)?
        .iter()
        .map(|table| {
            let data = if &table.tag == b"head" {
                let mut head = table.data.to_vec();
                head.get_mut(8..12)?.fill(0);
                head
            } else {
                table.data.to_vec()
            };
            Some((table.tag, data))
        })
        .collect::<Option<Vec<_>>>()?;
    finish_font(
        tables
            .iter()
            .map(|(tag, data)| (*tag, data.as_slice()))
            .collect(),
    )
}

/// Offset of the table directory of the (first) TrueType font in `font`;
/// `None` for CFF outlines ('OTTO') and unknown files.
fn face_offset(font: &[u8]) -> Option<usize> {
    match read_u32(font, 0)? {
        0x0001_0000 | 0x7472_7565 => Some(0),
        // 'ttcf': the first entry of the collection's offset table.
        0x7474_6366 => {
            let offset = read_u32(font, 12)? as usize;
            matches!(read_u32(font, offset)?, 0x0001_0000 | 0x7472_7565).then_some(offset)
        }
        _ => None,
    }
}

/// Write the tables sorted by tag and fix the whole-font checksum.
fn finish_font(mut tables: Vec<([u8; 4], &[u8])>) -> Option<Vec<u8>> {
    tables.sort_by_key(|(tag, _)| *tag);
    let mut font = write_font(&tables)?;
    let adjustment = 0xB1B0_AFBAu32.wrapping_sub(checksum(&font));
    let head_offset = head_offset(&font)?;
    font.get_mut(head_offset + 8..head_offset + 12)?
        .copy_from_slice(&adjustment.to_be_bytes());
    Some(font)
}

/// Table records of the font whose directory starts at `directory`; table
/// offsets are relative to the start of the file, also in collections.
fn read_tables(font: &[u8], directory: usize) -> Option<Vec<Table<'_>>> {
    let count = usize::from(read_u16(font, directory + 4)?);
    (0..count)
        .map(|index| {
            let record = directory + 12 + index * 16;
            let tag = font.get(record..record + 4)?.try_into().ok()?;
            let offset = read_u32(font, record + 8)? as usize;
            let length = read_u32(font, record + 12)? as usize;
            Some(Table {
                tag,
                data: font.get(offset..offset.checked_add(length)?)?,
            })
        })
        .collect()
}

fn composite_components(glyph: &[u8]) -> Option<Vec<u16>> {
    const ARGS_ARE_WORDS: u16 = 0x0001;
    const HAVE_SCALE: u16 = 0x0008;
    const MORE_COMPONENTS: u16 = 0x0020;
    const HAVE_XY_SCALE: u16 = 0x0040;
    const HAVE_TWO_BY_TWO: u16 = 0x0080;

    let mut components = Vec::new();
    let mut position = 10;
    loop {
        let flags = read_u16(glyph, position)?;
        components.push(read_u16(glyph, position + 2)?);
        position += 4 + if flags & ARGS_ARE_WORDS != 0 { 4 } else { 2 };
        position += if flags & HAVE_SCALE != 0 {
            2
        } else if flags & HAVE_XY_SCALE != 0 {
            4
        } else if flags & HAVE_TWO_BY_TWO != 0 {
            8
        } else {
            0
        };
        if flags & MORE_COMPONENTS == 0 {
            return Some(components);
        }
    }
}

fn write_font(tables: &[([u8; 4], &[u8])]) -> Option<Vec<u8>> {
    let count = u16::try_from(tables.len()).ok()?;
    let power = if count == 0 {
        0
    } else {
        15 - count.leading_zeros() as u16
    };
    let search_range = (1u16 << power) * 16;
    let mut font = Vec::new();
    font.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    for value in [count, search_range, power, count * 16 - search_range] {
        font.extend_from_slice(&value.to_be_bytes());
    }

    let mut offset = 12 + tables.len() * 16;
    for (tag, data) in tables {
        font.extend_from_slice(tag);
        font.extend_from_slice(&checksum(data).to_be_bytes());
        font.extend_from_slice(&u32::try_from(offset).ok()?.to_be_bytes());
        font.extend_from_slice(&u32::try_from(data.len()).ok()?.to_be_bytes());
        offset += data.len().next_multiple_of(4);
    }
    for (_, data) in tables {
        font.extend_from_slice(data);
        font.resize(font.len().next_multiple_of(4), 0);
    }
    Some(font)
}

fn head_offset(font: &[u8]) -> Option<usize> {
    let count = usize::from(read_u16(font, 4)?);
    (0..count)
        .map(|index| 12 + index * 16)
        .find(|record| font.get(*record..record + 4) == Some(b"head"))
        .and_then(|record| read_u32(font, record + 8))
        .map(|offset| offset as usize)
}

fn checksum(data: &[u8]) -> u32 {
    data.chunks(4).fold(0u32, |sum, chunk| {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum.wrapping_add(u32::from_be_bytes(word))
    })
}

fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        data.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subset_keeps_used_glyphs_and_parses() {
        let Some(path) = crate::textpdf::find_font(None).ok() else {
            return;
        };
        let font = std::fs::read(path).unwrap();
        let Ok(face) = ttf_parser::Face::parse(&font, 0) else {
            return;
        };
        let used = "Привет, world!"
            .chars()
            .filter_map(|character| face.glyph_index(character))
            .map(|glyph| glyph.0)
            .collect::<BTreeSet<_>>();
        let Some(subset) = subset_truetype(&font, &used) else {
            return;
        };
        assert!(subset.len() < font.len() / 4);

        let parsed = ttf_parser::Face::parse(&subset, 0).unwrap();
        assert_eq!(parsed.number_of_glyphs(), face.number_of_glyphs());
        for glyph in &used {
            let id = ttf_parser::GlyphId(*glyph);
            assert_eq!(parsed.glyph_bounding_box(id), face.glyph_bounding_box(id));
            assert_eq!(parsed.glyph_hor_advance(id), face.glyph_hor_advance(id));
        }
        assert_eq!(checksum(&subset), 0xB1B0_AFBA);
    }

    /// A tiny hand-made collection: a header pointing at one font whose
    /// tables sit after the header, as in real `.ttc` files.
    fn collection_of(font: &[u8]) -> Vec<u8> {
        let mut collection = b"ttcf\0\x01\0\0\0\0\0\x01\0\0\0\x10".to_vec();
        let shift = collection.len() as u32;
        let mut copy = font.to_vec();
        let count = usize::from(read_u16(font, 4).unwrap());
        for index in 0..count {
            let record = 12 + index * 16 + 8;
            let offset = read_u32(font, record).unwrap() + shift;
            copy[record..record + 4].copy_from_slice(&offset.to_be_bytes());
        }
        collection.extend_from_slice(&copy);
        collection
    }

    #[test]
    fn collections_give_a_standalone_font() {
        let Some(path) = crate::textpdf::find_font(None).ok() else {
            return;
        };
        let font = std::fs::read(path).unwrap();
        if face_offset(&font) != Some(0) {
            return;
        }
        let collection = collection_of(&font);
        let standalone = standalone_truetype(&collection).unwrap();
        assert_eq!(face_offset(&standalone), Some(0));
        let original = ttf_parser::Face::parse(&font, 0).unwrap();
        let parsed = ttf_parser::Face::parse(&standalone, 0).unwrap();
        assert_eq!(parsed.number_of_glyphs(), original.number_of_glyphs());
        assert_eq!(checksum(&standalone), 0xB1B0_AFBA);

        let used = BTreeSet::from([original.glyph_index('A').unwrap().0]);
        let subset = subset_truetype(&collection, &used).unwrap();
        assert!(ttf_parser::Face::parse(&subset, 0).is_ok());
    }

    #[test]
    fn cff_fonts_are_refused() {
        assert_eq!(standalone_truetype(b"OTTO\0\x01\0\0\0\0\0\0"), None);
    }
}
