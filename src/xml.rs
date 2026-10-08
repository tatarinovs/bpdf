use std::fs::File;
use std::io::Read;

use anyhow::{Context, Result};
use zip::ZipArchive;

/// Decode numeric character references, the five XML entities and the HTML
/// named entities common in books and web pages. One pass, so `&amp;lt;`
/// stays `&lt;`; unknown or malformed references are kept as written.
pub fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        result.push_str(&rest[..start]);
        rest = &rest[start..];
        let decoded = rest[1..]
            .find(';')
            .filter(|&end| end <= 32)
            .and_then(|end| Some((decode_entity(&rest[1..=end])?, end + 2)));
        match decoded {
            Some((character, length)) => {
                result.push(character);
                rest = &rest[length..];
            }
            None => {
                result.push('&');
                rest = &rest[1..];
            }
        }
    }
    result.push_str(rest);
    result
}

fn decode_entity(name: &str) -> Option<char> {
    if let Some(number) = name.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse().ok()?,
        };
        return char::from_u32(code).filter(|&c| c != '\0');
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "shy" => '\u{AD}',
        "laquo" => '«',
        "raquo" => '»',
        "lsquo" => '‘',
        "rsquo" => '’',
        "sbquo" => '‚',
        "ldquo" => '“',
        "rdquo" => '”',
        "bdquo" => '„',
        "ndash" => '–',
        "mdash" => '—',
        "hellip" => '…',
        "bull" => '•',
        "middot" => '·',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "deg" => '°',
        "plusmn" => '±',
        "times" => '×',
        "divide" => '÷',
        "minus" => '−',
        "le" => '≤',
        "ge" => '≥',
        "ne" => '≠',
        "asymp" => '≈',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "cent" => '¢',
        "sect" => '§',
        "para" => '¶',
        "numero" => '№',
        "larr" => '←',
        "rarr" => '→',
        "uarr" => '↑',
        "darr" => '↓',
        "frac12" => '½',
        "frac14" => '¼',
        "frac34" => '¾',
        "sup2" => '²',
        "sup3" => '³',
        "micro" => 'µ',
        "iexcl" => '¡',
        "iquest" => '¿',
        "thinsp" => '\u{2009}',
        "ensp" => '\u{2002}',
        "emsp" => '\u{2003}',
        "zwnj" => '\u{200C}',
        "zwj" => '\u{200D}',
        _ => return None,
    })
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
    let lookup = if archive.index_for_name(&normalized).is_some() {
        normalized.as_str()
    } else {
        name
    };
    let mut entry = archive
        .by_name(lookup)
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
    crate::fileset::natural_sort_names(&mut names);
    names
}

#[cfg(test)]
mod tests {
    use super::decode_entities;

    #[test]
    fn decodes_entities_in_one_pass() {
        assert_eq!(decode_entities("&amp;lt;b&amp;gt;"), "&lt;b&gt;");
        assert_eq!(
            decode_entities("&#1071;&#x44F; &mdash; &laquo;x&raquo;"),
            "Яя — «x»"
        );
        assert_eq!(
            decode_entities("AT&T &unknown; &#xZZ; &"),
            "AT&T &unknown; &#xZZ; &"
        );
        assert_eq!(decode_entities("&#0;"), "&#0;");
    }
}
