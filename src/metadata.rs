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

    let mut output = vec![0xff, 0xd8];
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
            loop {
                if position >= input.len() {
                    bail!("truncated JPEG entropy data");
                }
                let byte = input[position];
                position += 1;
                if byte != 0xff {
                    output.push(byte);
                    continue;
                }

                let ff_start = position - 1;
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
}
