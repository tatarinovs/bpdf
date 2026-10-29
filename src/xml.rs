use std::borrow::Cow;
use std::fs::File;

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

/// One piece of an XML document, in document order.
#[derive(Debug, PartialEq)]
pub enum Event<'a> {
    /// A start tag with its attribute text; a self-closing tag is followed
    /// by its `End`.
    Start {
        name: &'a str,
        attributes: &'a str,
    },
    End(&'a str),
    /// Character data with entities decoded; CDATA sections as written.
    Text(Cow<'a, str>),
}

/// The events of `xml`. Comments, processing instructions and declarations
/// are skipped; a tag that never closes ends the document.
pub fn events(xml: &str) -> Events<'_> {
    Events {
        rest: xml,
        pending_end: None,
    }
}

pub struct Events<'a> {
    rest: &'a str,
    pending_end: Option<&'a str>,
}

impl<'a> Iterator for Events<'a> {
    type Item = Event<'a>;

    fn next(&mut self) -> Option<Event<'a>> {
        if let Some(name) = self.pending_end.take() {
            return Some(Event::End(name));
        }
        loop {
            if self.rest.is_empty() {
                return None;
            }
            if !self.rest.starts_with('<') {
                let end = self.rest.find('<').unwrap_or(self.rest.len());
                let text = &self.rest[..end];
                self.rest = &self.rest[end..];
                return Some(Event::Text(decode(text)));
            }
            if let Some(comment) = self.rest.strip_prefix("<!--") {
                self.rest = comment.find("-->").map_or("", |end| &comment[end + 3..]);
                continue;
            }
            if let Some(instruction) = self.rest.strip_prefix("<?") {
                self.rest = instruction
                    .find("?>")
                    .map_or("", |end| &instruction[end + 2..]);
                continue;
            }
            if let Some(cdata) = self.rest.strip_prefix("<![CDATA[") {
                let end = cdata.find("]]>").unwrap_or(cdata.len());
                self.rest = cdata.get(end + 3..).unwrap_or("");
                return Some(Event::Text(Cow::Borrowed(&cdata[..end])));
            }
            if self.rest.starts_with("<!") {
                self.rest = self.rest.find('>').map_or("", |end| &self.rest[end + 1..]);
                continue;
            }
            // The tag ends at the first `>` outside a quoted attribute value.
            let mut quote = None;
            let end = self
                .rest
                .char_indices()
                .skip(1)
                .find_map(|(index, character)| {
                    match (quote, character) {
                        (None, '"' | '\'') => quote = Some(character),
                        (Some(open), _) if open == character => quote = None,
                        (None, '>') => return Some(index),
                        _ => {}
                    }
                    None
                })?;
            let tag = &self.rest[1..end];
            self.rest = &self.rest[end + 1..];
            if let Some(name) = tag.strip_prefix('/') {
                return Some(Event::End(name.trim()));
            }
            let (tag, empty) = match tag.strip_suffix('/') {
                Some(tag) => (tag, true),
                None => (tag, false),
            };
            let split = tag.find(char::is_whitespace).unwrap_or(tag.len());
            let (name, attributes) = tag.split_at(split);
            if empty {
                self.pending_end = Some(name);
            }
            return Some(Event::Start {
                name,
                attributes: attributes.trim(),
            });
        }
    }
}

/// The name without its namespace prefix (`w:p` is `p`).
pub fn local_name(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// Value of the attribute `name` (as written, prefix included) in the
/// attribute text of a start tag, entities decoded.
pub fn attribute<'a>(attributes: &'a str, name: &str) -> Option<Cow<'a, str>> {
    let mut rest = attributes;
    loop {
        let equals = rest.find('=')?;
        let key = rest[..equals].trim();
        let value = rest[equals + 1..].trim_start();
        let quote = value
            .chars()
            .next()
            .filter(|quote| matches!(quote, '"' | '\''))?;
        let end = value[1..].find(quote)? + 1;
        if key == name {
            return Some(decode(&value[1..end]));
        }
        rest = &value[end + 1..];
    }
}

fn decode(text: &str) -> Cow<'_, str> {
    if text.contains('&') {
        Cow::Owned(decode_entities(text))
    } else {
        Cow::Borrowed(text)
    }
}

/// The character data of an XML/HTML fragment, entities decoded.
pub fn strip_tags(input: &str) -> String {
    events(input)
        .filter_map(|event| match event {
            Event::Text(text) => Some(text),
            _ => None,
        })
        .collect()
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
    let entry = archive
        .by_name(lookup)
        .with_context(|| format!("entry {name} not found in ZIP archive"))?;
    crate::archive::read_text_entry(entry, name)
}

/// List ZIP entries whose names start with `prefix` and end with `suffix`,
/// sorted in natural order.
pub fn collect_sorted_entries(
    archive: &mut ZipArchive<File>,
    prefix: &str,
    suffix: &str,
) -> Vec<String> {
    let mut names = archive
        .file_names()
        .filter_map(Result::ok)
        .filter(|name| name.starts_with(prefix) && name.ends_with(suffix))
        .map(|name| name.into_owned())
        .collect::<Vec<_>>();
    crate::fileset::natural_sort_names(&mut names);
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_follow_the_document() {
        let xml =
            r#"<?xml version="1.0"?><!-- <a> --><a x='1>2' y="&amp;"><b/>t&lt;<![CDATA[<c>]]></a>"#;
        let events = events(xml).collect::<Vec<_>>();
        assert_eq!(
            events,
            [
                Event::Start {
                    name: "a",
                    attributes: r#"x='1>2' y="&amp;""#
                },
                Event::Start {
                    name: "b",
                    attributes: ""
                },
                Event::End("b"),
                Event::Text("t<".into()),
                Event::Text("<c>".into()),
                Event::End("a"),
            ]
        );
        let Event::Start { attributes, .. } = &events[0] else {
            unreachable!()
        };
        assert_eq!(attribute(attributes, "x").as_deref(), Some("1>2"));
        assert_eq!(attribute(attributes, "y").as_deref(), Some("&"));
        assert_eq!(attribute(attributes, "z"), None);
        assert_eq!(local_name("w:p"), "p");
    }

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
