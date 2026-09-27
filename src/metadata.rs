use anyhow::{Result, bail};

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

pub fn strip_png(input: &[u8], keep_icc: bool) -> Result<Vec<u8>> {
    if !input.starts_with(PNG_SIGNATURE) {
        bail!("not a valid PNG file");
    }

    let mut output = PNG_SIGNATURE.to_vec();
    let mut position = PNG_SIGNATURE.len();
    let mut saw_iend = false;

    while !saw_iend {
        if input.len().saturating_sub(position) < 12 {
            bail!("truncated PNG file");
        }
        let length = u32::from_be_bytes(
            input[position..position + 4]
                .try_into()
                .expect("four bytes"),
        ) as usize;
        if length > 0x7fff_ffff {
            bail!("invalid PNG chunk length {length}");
        }
        let end = position
            .checked_add(12)
            .and_then(|value| value.checked_add(length))
            .filter(|end| *end <= input.len())
            .ok_or_else(|| anyhow::anyhow!("truncated PNG file"))?;

        let kind = &input[position + 4..position + 8];
        saw_iend = kind == b"IEND";
        let drop = matches!(
            kind,
            b"tEXt" | b"zTXt" | b"iTXt" | b"tIME" | b"eXIf" | b"dSIG"
        ) || (kind == b"iCCP" && !keep_icc);

        if !drop {
            output.extend_from_slice(&input[position..end]);
        }
        position = end;
    }

    Ok(output)
}

pub fn strip_jpeg(input: &[u8], keep_icc: bool) -> Result<Vec<u8>> {
    if !input.starts_with(&[0xff, 0xd8]) {
        bail!("not a valid JPEG file");
    }

    let mut output = Vec::with_capacity(input.len());
    output.extend_from_slice(&[0xff, 0xd8]);
    let mut position = 2usize;
    let mut pending_marker = None;

    loop {
        let marker = if let Some(marker) = pending_marker.take() {
            marker
        } else {
            while position < input.len() && input[position] != 0xff {
                position += 1;
            }
            if position >= input.len() {
                break;
            }
            while position < input.len() && input[position] == 0xff {
                position += 1;
            }
            if position >= input.len() {
                bail!("truncated JPEG marker");
            }
            let marker = input[position];
            position += 1;
            marker
        };

        if marker == 0xd8 || marker == 0xd9 || (0xd0..=0xd7).contains(&marker) || marker == 0x01 {
            output.extend_from_slice(&[0xff, marker]);
            if marker == 0xd9 {
                break;
            }
            continue;
        }

        if input.len().saturating_sub(position) < 2 {
            bail!("truncated JPEG segment");
        }
        let length = u16::from_be_bytes([input[position], input[position + 1]]) as usize;
        if length < 2 {
            bail!("invalid JPEG marker length");
        }
        let segment_end = position
            .checked_add(length)
            .filter(|end| *end <= input.len())
            .ok_or_else(|| anyhow::anyhow!("truncated JPEG segment"))?;

        let drop =
            marker == 0xe1 || marker == 0xed || marker == 0xfe || (marker == 0xe2 && !keep_icc);
        if !drop {
            output.extend_from_slice(&[0xff, marker]);
            output.extend_from_slice(&input[position..segment_end]);
        }
        position = segment_end;

        if marker == 0xda {
            // Copy entropy-coded data in runs up to the next 0xFF.
            loop {
                let rest = &input[position..];
                let Some(offset) = memchr::memchr(0xff, rest) else {
                    bail!("truncated JPEG entropy data");
                };
                output.extend_from_slice(&rest[..offset]);
                let ff_start = position + offset;
                position = ff_start + 1;
                while position < input.len() && input[position] == 0xff {
                    position += 1;
                }
                if position >= input.len() {
                    bail!("truncated JPEG entropy marker");
                }
                let next = input[position];
                position += 1;

                if next == 0x00 || (0xd0..=0xd7).contains(&next) {
                    output.extend_from_slice(&input[ff_start..position]);
                } else {
                    pending_marker = Some(next);
                    break;
                }
            }
        }
    }

    Ok(output)
}

/// Physical resolution recorded in PNG pHYs, JFIF or EXIF metadata.
pub fn image_dpi(bytes: &[u8]) -> Option<f64> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        detect_png_dpi(bytes)
    } else if bytes.starts_with(b"\xFF\xD8") {
        detect_jpeg_dpi(bytes)
    } else {
        None
    }
}

fn detect_png_dpi(bytes: &[u8]) -> Option<f64> {
    let mut offset = 8;
    while offset + 8 <= bytes.len() {
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().ok()?) as usize;
        let chunk_type = &bytes[offset + 4..offset + 8];
        if chunk_type == b"pHYs" && offset + 8 + length <= bytes.len() && length >= 9 {
            let chunk_data = &bytes[offset + 8..offset + 8 + length];
            let ppu_x = u32::from_be_bytes(chunk_data[0..4].try_into().ok()?);
            let unit = chunk_data[8];
            if unit == 1 && ppu_x > 0 {
                let dpi = (ppu_x as f64) * 0.0254;
                return Some(dpi.round());
            }
        } else if chunk_type == b"IDAT" || chunk_type == b"IEND" {
            break;
        }
        offset += 12 + length;
    }
    None
}

fn detect_jpeg_dpi(bytes: &[u8]) -> Option<f64> {
    let mut offset = 2;
    while offset + 4 <= bytes.len() {
        if bytes[offset] != 0xFF {
            break;
        }
        let marker = bytes[offset + 1];
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        let length = u16::from_be_bytes(bytes[offset + 2..offset + 4].try_into().ok()?) as usize;
        if length < 2 || offset + 2 + length > bytes.len() {
            break;
        }
        let segment_data = &bytes[offset + 4..offset + 2 + length];

        if marker == 0xE0 && segment_data.starts_with(b"JFIF\0") && segment_data.len() >= 9 {
            let units = segment_data[7];
            let x_density = u16::from_be_bytes(segment_data[8..10].try_into().ok()?) as f64;
            if x_density > 0.0 {
                if units == 1 {
                    return Some(x_density);
                } else if units == 2 {
                    return Some((x_density * 2.54).round());
                }
            }
        }

        if marker == 0xE1 && segment_data.starts_with(b"Exif\0\0") && segment_data.len() >= 14 {
            let exif = &segment_data[6..];
            if let Some(dpi) = parse_exif_dpi(exif) {
                return Some(dpi);
            }
        }

        offset += 2 + length;
    }
    None
}

fn parse_exif_dpi(exif: &[u8]) -> Option<f64> {
    if exif.len() < 8 {
        return None;
    }
    let is_le = match &exif[0..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let read_u16 = |buf: &[u8], pos: usize| -> Option<u16> {
        let b = buf.get(pos..pos + 2)?;
        Some(if is_le {
            u16::from_le_bytes(b.try_into().ok()?)
        } else {
            u16::from_be_bytes(b.try_into().ok()?)
        })
    };
    let read_u32 = |buf: &[u8], pos: usize| -> Option<u32> {
        let b = buf.get(pos..pos + 4)?;
        Some(if is_le {
            u32::from_le_bytes(b.try_into().ok()?)
        } else {
            u32::from_be_bytes(b.try_into().ok()?)
        })
    };

    let ifd0_offset = read_u32(exif, 4)? as usize;
    if ifd0_offset + 2 > exif.len() {
        return None;
    }
    let num_entries = read_u16(exif, ifd0_offset)? as usize;
    let mut x_res: Option<f64> = None;
    let mut unit: u16 = 2;

    for i in 0..num_entries {
        let entry_offset = ifd0_offset + 2 + i * 12;
        if entry_offset + 12 > exif.len() {
            break;
        }
        let tag = read_u16(exif, entry_offset)?;
        let val_offset = read_u32(exif, entry_offset + 8)? as usize;

        match tag {
            0x011A => {
                if val_offset + 8 <= exif.len() {
                    let num = read_u32(exif, val_offset)? as f64;
                    let den = read_u32(exif, val_offset + 4)? as f64;
                    if den > 0.0 {
                        x_res = Some(num / den);
                    }
                }
            }
            0x0128 => {
                let u = read_u16(exif, entry_offset + 8)?;
                unit = u;
            }
            _ => {}
        }
    }

    let res = x_res?;
    if res <= 0.0 {
        return None;
    }
    if unit == 2 {
        Some(res.round())
    } else if unit == 3 {
        Some((res * 2.54).round())
    } else {
        Some(res.round())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::{DynamicImage, ImageFormat, Rgb, RgbImage};

    use super::*;

    #[test]
    fn jpeg_strip_removes_app1_without_reencoding_scan_data() {
        let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(2, 2, Rgb([20, 40, 60])));
        let mut original = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut original), ImageFormat::Jpeg)
            .unwrap();

        let app1 = [0xff, 0xe1, 0x00, 0x08, b'E', b'x', b'i', b'f', 0, 0];
        let mut tagged = original[..2].to_vec();
        tagged.extend_from_slice(&app1);
        tagged.extend_from_slice(&original[2..]);

        let stripped = strip_jpeg(&tagged, false).unwrap();
        assert_eq!(stripped, original);
    }

    #[test]
    fn rejects_truncated_png() {
        assert!(strip_png(PNG_SIGNATURE, false).is_err());
    }

    #[test]
    fn test_detect_png_phys_dpi() {
        // Construct minimal valid PNG with pHYs chunk at 300 DPI (11811 pixels/meter)
        let mut png = Vec::new();
        png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        // IHDR chunk
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
        png.extend_from_slice(&[0, 0, 0, 0]); // CRC placeholder
        // pHYs chunk: 11811 (0x2E23) ppm ≈ 300 dpi, unit 1 (meter)
        png.extend_from_slice(&9u32.to_be_bytes());
        png.extend_from_slice(b"pHYs");
        png.extend_from_slice(&11811u32.to_be_bytes());
        png.extend_from_slice(&11811u32.to_be_bytes());
        png.push(1); // unit = meter
        png.extend_from_slice(&[0, 0, 0, 0]); // CRC
        // IEND chunk
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(b"IEND");
        png.extend_from_slice(&[0, 0, 0, 0]);

        let dpi = image_dpi(&png);
        assert_eq!(dpi, Some(300.0));
    }

    #[test]
    fn test_detect_jpeg_jfif_dpi() {
        // Construct minimal JPEG with JFIF APP0 marker (300 DPI, unit = 1 inch)
        let mut jpeg = Vec::new();
        jpeg.extend_from_slice(b"\xFF\xD8"); // SOI
        jpeg.extend_from_slice(b"\xFF\xE0"); // APP0
        jpeg.extend_from_slice(&16u16.to_be_bytes()); // length
        jpeg.extend_from_slice(b"JFIF\0");
        jpeg.extend_from_slice(&[1, 2]); // version 1.2
        jpeg.push(1); // units: 1 = dots per inch
        jpeg.extend_from_slice(&300u16.to_be_bytes()); // X density
        jpeg.extend_from_slice(&300u16.to_be_bytes()); // Y density
        jpeg.extend_from_slice(&[0, 0]); // thumbnail
        jpeg.extend_from_slice(b"\xFF\xD9"); // EOI

        let dpi = image_dpi(&jpeg);
        assert_eq!(dpi, Some(300.0));
    }

    #[test]
    fn test_detect_jpeg_jfif_dpcm() {
        // Construct minimal JPEG with JFIF APP0 marker (118 DPCM ≈ 300 DPI, unit = 2 cm)
        let mut jpeg = Vec::new();
        jpeg.extend_from_slice(b"\xFF\xD8"); // SOI
        jpeg.extend_from_slice(b"\xFF\xE0"); // APP0
        jpeg.extend_from_slice(&16u16.to_be_bytes()); // length
        jpeg.extend_from_slice(b"JFIF\0");
        jpeg.extend_from_slice(&[1, 2]);
        jpeg.push(2); // units: 2 = dots per cm
        jpeg.extend_from_slice(&118u16.to_be_bytes()); // 118 * 2.54 = 299.72 ≈ 300
        jpeg.extend_from_slice(&118u16.to_be_bytes());
        jpeg.extend_from_slice(&[0, 0]);
        jpeg.extend_from_slice(b"\xFF\xD9");

        let dpi = image_dpi(&jpeg);
        assert_eq!(dpi, Some(300.0));
    }

    #[test]
    fn test_detect_jpeg_exif_dpi() {
        // Construct minimal JPEG with Exif APP1 marker (600 DPI, unit = 2 inch)
        let mut jpeg = Vec::new();
        jpeg.extend_from_slice(b"\xFF\xD8"); // SOI
        jpeg.extend_from_slice(b"\xFF\xE1"); // APP1

        let mut exif_payload = Vec::new();
        exif_payload.extend_from_slice(b"Exif\0\0");
        let tiff_start = exif_payload.len();
        exif_payload.extend_from_slice(b"II\x2A\x00"); // Little-endian TIFF header
        exif_payload.extend_from_slice(&8u32.to_le_bytes()); // Offset to IFD0 = 8

        // IFD0: 2 entries
        exif_payload.extend_from_slice(&2u16.to_le_bytes());

        // Entry 1: 0x011A (XResolution), type 5 (RATIONAL), count 1, offset 38 (from TIFF start)
        exif_payload.extend_from_slice(&0x011Au16.to_le_bytes());
        exif_payload.extend_from_slice(&5u16.to_le_bytes());
        exif_payload.extend_from_slice(&1u32.to_le_bytes());
        exif_payload.extend_from_slice(&38u32.to_le_bytes());

        // Entry 2: 0x0128 (ResolutionUnit), type 3 (SHORT), count 1, value 2 (inches)
        exif_payload.extend_from_slice(&0x0128u16.to_le_bytes());
        exif_payload.extend_from_slice(&3u16.to_le_bytes());
        exif_payload.extend_from_slice(&1u32.to_le_bytes());
        exif_payload.extend_from_slice(&2u16.to_le_bytes());
        exif_payload.extend_from_slice(&[0, 0]); // padding to 4 bytes

        // Next IFD offset = 0
        exif_payload.extend_from_slice(&0u32.to_le_bytes());

        // Offset 38 from TIFF start: Rational value (600 / 1)
        assert_eq!(exif_payload.len() - tiff_start, 38);
        exif_payload.extend_from_slice(&600u32.to_le_bytes()); // numerator
        exif_payload.extend_from_slice(&1u32.to_le_bytes()); // denominator

        let app1_len = (exif_payload.len() + 2) as u16;
        jpeg.extend_from_slice(&app1_len.to_be_bytes());
        jpeg.extend_from_slice(&exif_payload);
        jpeg.extend_from_slice(b"\xFF\xD9"); // EOI

        let dpi = image_dpi(&jpeg);
        assert_eq!(dpi, Some(600.0));
    }

    #[test]
    fn test_uncalibrated_image_returns_none() {
        let dummy_png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0DIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00IEND\x00\x00\x00\x00";
        assert_eq!(image_dpi(dummy_png), None);

        let dummy_jpeg = b"\xFF\xD8\xFF\xD9";
        assert_eq!(image_dpi(dummy_jpeg), None);
    }
}
