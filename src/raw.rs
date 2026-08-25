use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow};

/// Extract the largest embedded JPEG preview from a RAW file buffer.
pub fn extract_preview(bytes: &[u8]) -> Option<&[u8]> {
    let mut best_preview: Option<(&[u8], u64)> = None;
    let mut i = 0;

    while i + 4 < bytes.len() {
        // Look for JPEG Start of Image (SOI) marker: 0xFF, 0xD8, 0xFF
        if bytes[i] == 0xFF
            && bytes[i + 1] == 0xD8
            && bytes[i + 2] == 0xFF
            && let Some((jpeg_slice, width, height)) = parse_jpeg_stream(&bytes[i..])
        {
            let area = u64::from(width) * u64::from(height);
            match best_preview {
                Some((_, best_area)) if area > best_area => {
                    best_preview = Some((jpeg_slice, area));
                }
                None => {
                    best_preview = Some((jpeg_slice, area));
                }
                _ => {}
            }
            // Skip past this JPEG stream to continue search
            i += jpeg_slice.len();
            continue;
        }
        i += 1;
    }

    best_preview.map(|(slice, _)| slice)
}

/// Try to parse a contiguous JPEG stream starting at the given buffer.
/// Returns `Some((slice, width, height))` on success.
fn parse_jpeg_stream(bytes: &[u8]) -> Option<(&[u8], u32, u32)> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }

    let mut pos = 2;
    let mut dimensions: Option<(u32, u32)> = None;

    while pos + 1 < bytes.len() {
        if bytes[pos] != 0xFF {
            return None;
        }

        // Skip extra 0xFF padding bytes
        while pos < bytes.len() && bytes[pos] == 0xFF {
            pos += 1;
        }
        if pos >= bytes.len() {
            return None;
        }

        let marker = bytes[pos];
        pos += 1;

        // Standalone markers without payload
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }

        // End of Image marker
        if marker == 0xD9 {
            let (w, h) = dimensions?;
            return Some((&bytes[..pos], w, h));
        }

        // Variable-length markers: length is stored in 2 bytes (including length bytes themselves)
        if pos + 2 > bytes.len() {
            return None;
        }
        let length = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]) as usize;
        if length < 2 || pos + length > bytes.len() {
            return None;
        }

        // SOF markers (Start of Frame) containing dimensions:
        // SOF0 (0xC0), SOF1 (0xC1), SOF2 (0xC2), SOF3 (0xC3),
        // SOF5 (0xC5), SOF6 (0xC6), SOF7 (0xC7), SOF9 (0xC9),
        // SOF10 (0xCA), SOF11 (0xCB), SOF13 (0xCD), SOF14 (0xCE), SOF15 (0xCF)
        let is_sof = matches!(
            marker,
            0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF
        );

        if is_sof && length >= 8 && pos + 7 < bytes.len() {
            // SOF payload: [length: 2] [precision: 1] [height: 2] [width: 2] [components: 1]
            let height = u16::from_be_bytes([bytes[pos + 3], bytes[pos + 4]]) as u32;
            let width = u16::from_be_bytes([bytes[pos + 5], bytes[pos + 6]]) as u32;
            dimensions = Some((width, height));
        }

        // Start of Scan (SOS): image entropy data follows
        if marker == 0xDA {
            pos += length;
            // Scan through entropy data until next non-stuffed 0xFF marker
            while pos + 1 < bytes.len() {
                if bytes[pos] == 0xFF {
                    let next = bytes[pos + 1];
                    // 0x00 is byte stuffing, 0xD0..=0xD7 are restart markers (RSTm)
                    if next == 0x00 || (0xD0..=0xD7).contains(&next) {
                        pos += 2;
                        continue;
                    }
                    // End of Image marker
                    if next == 0xD9 {
                        let (w, h) = dimensions?;
                        return Some((&bytes[..pos + 2], w, h));
                    }
                    if next != 0xFF {
                        // Another marker encountered
                        break;
                    }
                }
                pos += 1;
            }
            continue;
        }

        pos += length;
    }

    None
}

/// Read preview from file or return error if not found.
pub fn read_preview(path: &Path) -> Result<Vec<u8>> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    extract_preview(&bytes)
        .map(|slice| slice.to_vec())
        .ok_or_else(|| anyhow!("no embedded preview found in RAW image {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_dummy_jpeg(width: u16, height: u16) -> Vec<u8> {
        let mut jpeg = Vec::new();
        // SOI
        jpeg.extend_from_slice(&[0xFF, 0xD8]);
        // APP0
        jpeg.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
        jpeg.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        // SOF0 (0xC0), length = 8 + 3 = 11 (0x00, 0x0B)
        // payload: precision 8, height, width, components 1
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        jpeg.extend_from_slice(&height.to_be_bytes());
        jpeg.extend_from_slice(&width.to_be_bytes());
        jpeg.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]);
        // SOS
        jpeg.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00]);
        // Scan payload with byte stuffing
        jpeg.extend_from_slice(&[0x12, 0x34, 0xFF, 0x00, 0x56, 0x78]);
        // EOI
        jpeg.extend_from_slice(&[0xFF, 0xD9]);
        jpeg
    }

    #[test]
    fn parses_single_jpeg_stream() {
        let dummy = create_dummy_jpeg(800, 600);
        let preview = extract_preview(&dummy).expect("should extract preview");
        assert_eq!(preview, dummy.as_slice());
    }

    #[test]
    fn selects_largest_resolution_preview() {
        let thumb = create_dummy_jpeg(160, 120);
        let medium = create_dummy_jpeg(1024, 768);
        let full = create_dummy_jpeg(6000, 4000);

        // Pack them inside a fake raw container buffer with dummy data
        let mut raw_container = Vec::new();
        raw_container.extend_from_slice(b"TIFF_HEADER_OR_RAW_DATA_PADDING");
        raw_container.extend_from_slice(&thumb);
        raw_container.extend_from_slice(b"INTERMEDIATE_RAW_SENSOR_DATA");
        raw_container.extend_from_slice(&full);
        raw_container.extend_from_slice(b"MORE_DATA");
        raw_container.extend_from_slice(&medium);
        raw_container.extend_from_slice(b"TRAILING_DATA");

        let preview = extract_preview(&raw_container).expect("should find largest preview");
        assert_eq!(preview, full.as_slice());
    }

    #[test]
    fn returns_none_on_arbitrary_binary_data() {
        let noise = vec![0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0];
        assert!(extract_preview(&noise).is_none());
    }
}
