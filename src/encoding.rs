use std::path::Path;

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use encoding_rs::{Encoding, WINDOWS_1251};

/// Decode text honouring a BOM, then UTF-8, then an XML `encoding`
/// declaration; legacy files without one are read as Windows-1251.
pub fn decode_text(bytes: &[u8]) -> String {
    if let Some((encoding, bom_length)) = Encoding::for_bom(bytes) {
        return encoding
            .decode_without_bom_handling(&bytes[bom_length..])
            .0
            .into_owned();
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    declared_xml_encoding(bytes)
        .unwrap_or(WINDOWS_1251)
        .decode_without_bom_handling(bytes)
        .0
        .into_owned()
}

pub fn read_text(path: &Path) -> Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(decode_text(&bytes))
}

fn declared_xml_encoding(bytes: &[u8]) -> Option<&'static Encoding> {
    let header = &bytes[..bytes.len().min(200)];
    let declaration = &header[..memchr::memmem::find(header, b"?>")?];
    let start = memchr::memmem::find(declaration, b"encoding=")? + "encoding=".len();
    let quote = *declaration.get(start)?;
    let value = &declaration[start + 1..];
    let end = memchr::memchr(quote, value)?;
    Encoding::for_label(&value[..end])
}

pub fn base64(input: &[u8]) -> String {
    STANDARD.encode(input)
}

#[cfg(windows)]
pub fn powershell_encoded_command(script: &str) -> String {
    let utf16 = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    base64(&utf16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_standard_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64("Привет".as_bytes()), "0J/RgNC40LLQtdGC");
    }

    #[test]
    fn decodes_legacy_and_declared_encodings() {
        assert_eq!(decode_text("Привет".as_bytes()), "Привет");
        assert_eq!(decode_text(b"\xcf\xf0\xe8\xe2\xe5\xf2"), "Привет");
        assert_eq!(decode_text(b"\xef\xbb\xbfA"), "A");
        let koi8 = b"<?xml version=\"1.0\" encoding=\"koi8-r\"?><p>\xf0\xd2\xc9</p>";
        assert!(decode_text(koi8).contains("При"));
    }
}
