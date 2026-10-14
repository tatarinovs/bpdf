//! PSD/PSB reader for the merged composite image (Section 5), so the common
//! documents need no FFmpeg. Layers are not rendered: Photoshop stores the
//! flattened result whenever "Maximize compatibility" is on (the default).

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use image::{DynamicImage, RgbImage, RgbaImage};

const PALETTE_LEN: usize = 768;
/// Photoshop's own limit.
const MAX_CHANNELS: usize = 56;
/// Planes and the output buffer together take up to 8 bytes per pixel.
const MAX_PIXELS: u64 = 100_000_000;

const MODE_GRAYSCALE: u16 = 1;
const MODE_INDEXED: u16 = 2;
const MODE_RGB: u16 = 3;
const MODE_CMYK: u16 = 4;

/// Decode the merged composite of an 8/16-bit Grayscale, Indexed, RGB or CMYK
/// PSD/PSB with raw or PackBits data. Anything else is an error, and the
/// caller hands the file to FFmpeg.
pub fn decode(path: &Path) -> Result<DynamicImage> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let file_len = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    decode_from(&mut reader, file_len).context("built-in PSD reader failed")
}

fn decode_from(reader: &mut BufReader<File>, file_len: u64) -> Result<DynamicImage> {
    let mut header = [0u8; 26];
    reader.read_exact(&mut header)?;
    ensure!(&header[0..4] == b"8BPS", "not a Photoshop file");
    let is_psb = match u16::from_be_bytes([header[4], header[5]]) {
        1 => false,
        2 => true,
        version => bail!("unknown PSD version {version}"),
    };
    let channels = u16::from_be_bytes([header[12], header[13]]) as usize;
    let height = u32::from_be_bytes([header[14], header[15], header[16], header[17]]);
    let width = u32::from_be_bytes([header[18], header[19], header[20], header[21]]);
    let depth = u16::from_be_bytes([header[22], header[23]]);
    let color_mode = u16::from_be_bytes([header[24], header[25]]);

    let bytes_per_sample = match depth {
        8 => 1,
        16 => 2,
        _ => bail!("{depth}-bit PSD is not supported"),
    };
    ensure!(
        matches!(
            color_mode,
            MODE_GRAYSCALE | MODE_INDEXED | MODE_RGB | MODE_CMYK
        ),
        "PSD color mode {color_mode} is not supported"
    );
    ensure!(
        width != 0 && height != 0 && channels != 0 && channels <= MAX_CHANNELS,
        "invalid PSD header"
    );
    let num_pixels = u64::from(width) * u64::from(height);
    ensure!(
        num_pixels <= MAX_PIXELS,
        "PSD is too large ({width}x{height})"
    );
    let num_pixels = num_pixels as usize;
    let (w, h) = (width as usize, height as usize);

    // Section 2: Color Mode Data (palette for indexed images).
    let color_data_len = read_u32(reader)? as usize;
    let mut palette = Vec::new();
    if color_mode == MODE_INDEXED {
        ensure!(color_data_len >= PALETTE_LEN, "indexed PSD has no palette");
        palette.resize(PALETTE_LEN, 0);
        reader.read_exact(&mut palette)?;
        reader.seek(SeekFrom::Current((color_data_len - PALETTE_LEN) as i64))?;
    } else {
        reader.seek(SeekFrom::Current(color_data_len as i64))?;
    }

    // Section 3: Image Resources.
    let resources_len = read_u32(reader)?;
    reader.seek(SeekFrom::Current(i64::from(resources_len)))?;

    // Section 4: Layer and Mask Information. Only the sign of the layer count
    // is needed: negative means the first extra channel of the merged image is
    // its transparency. Otherwise extra channels are saved selections and must
    // not make the picture see-through.
    let read_len = |reader: &mut BufReader<File>| -> Result<u64> {
        if is_psb {
            let mut bytes = [0u8; 8];
            reader.read_exact(&mut bytes)?;
            Ok(u64::from_be_bytes(bytes))
        } else {
            read_u32(reader).map(u64::from)
        }
    };
    let layers_len = read_len(reader)?;
    let layers_start = reader.stream_position()?;
    let mut transparent = false;
    if layers_len > 0 {
        let info_len = read_len(reader)?;
        if info_len >= 2 {
            let mut count = [0u8; 2];
            reader.read_exact(&mut count)?;
            transparent = i16::from_be_bytes(count) < 0;
        }
    }
    let image_data = layers_start
        .checked_add(layers_len)
        .context("invalid PSD layer section")?;
    reader.seek(SeekFrom::Start(image_data))?;

    // Section 5: Image Data.
    let mut compression = [0u8; 2];
    reader.read_exact(&mut compression)?;
    let compression = u16::from_be_bytes(compression);
    let row_bytes = w * bytes_per_sample;
    let remaining = file_len.saturating_sub(reader.stream_position()?);

    // The color channels, plus the transparency if the document has one.
    let color_channels = match color_mode {
        MODE_RGB => 3,
        MODE_CMYK => 4,
        _ => 1,
    };
    let active = channels.min(color_channels + usize::from(transparent));
    // Reserved only once the file is known to hold that much data.
    let new_planes = || -> Vec<Vec<u8>> { vec![Vec::with_capacity(num_pixels); active] };
    let truncated = || anyhow::anyhow!("PSD image data is truncated");

    let planes = match compression {
        0 => {
            let plane_bytes = (num_pixels * bytes_per_sample) as u64;
            if (channels as u64) * plane_bytes > remaining {
                return Err(truncated());
            }
            let mut planes = new_planes();
            let mut row = vec![0u8; row_bytes];
            for plane in &mut planes {
                for _ in 0..h {
                    reader.read_exact(&mut row)?;
                    push_row(plane, &row, bytes_per_sample);
                }
            }
            planes
        }
        1 => {
            let entry_size = if is_psb { 4 } else { 2 };
            // Checked before allocating: the header's sizes may be anything.
            let table_len = (channels as u64) * (h as u64) * entry_size as u64;
            if table_len > remaining {
                return Err(truncated());
            }
            let mut table = vec![0u8; table_len as usize];
            reader.read_exact(&mut table)?;
            let line_lens: Vec<usize> = table
                .chunks_exact(entry_size)
                .map(|bytes| bytes.iter().fold(0, |acc, &b| (acc << 8) | b as usize))
                .collect();
            let total: u64 = line_lens.iter().map(|&len| len as u64).sum();
            if total + table_len > remaining {
                return Err(truncated());
            }

            let mut planes = new_planes();
            let mut packed = Vec::new();
            let mut unpacked = Vec::with_capacity(row_bytes);
            for (channel, lens) in line_lens.chunks_exact(h).enumerate() {
                let Some(plane) = planes.get_mut(channel) else {
                    break;
                };
                for &len in lens {
                    packed.resize(len, 0);
                    reader.read_exact(&mut packed)?;
                    decode_packbits(&packed, row_bytes, &mut unpacked);
                    push_row(plane, &unpacked, bytes_per_sample);
                }
            }
            planes
        }
        _ => bail!("PSD compression {compression} is not supported"),
    };

    let has_alpha = planes.len() > color_channels;
    let pixels = compose(color_mode, &planes, &palette, num_pixels, has_alpha);
    let image = if has_alpha {
        RgbaImage::from_raw(width, height, pixels).map(DynamicImage::ImageRgba8)
    } else {
        RgbImage::from_raw(width, height, pixels).map(DynamicImage::ImageRgb8)
    };
    image.context("PSD pixel buffer has the wrong size")
}

fn read_u32(reader: &mut impl Read) -> Result<u32> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
}

/// Append one row of samples to an 8-bit plane (16-bit samples keep their
/// high byte).
fn push_row(plane: &mut Vec<u8>, row: &[u8], bytes_per_sample: usize) {
    if bytes_per_sample == 1 {
        plane.extend_from_slice(row);
    } else {
        plane.extend(row.chunks_exact(2).map(|sample| sample[0]));
    }
}

/// Interleave planes into RGB, or RGBA when the last plane is transparency.
fn compose(
    color_mode: u16,
    planes: &[Vec<u8>],
    palette: &[u8],
    num_pixels: usize,
    has_alpha: bool,
) -> Vec<u8> {
    // A document may have fewer channels than its mode implies.
    let plane = |index: usize| planes.get(index).unwrap_or(&planes[0]).as_slice();
    let stride = if has_alpha { 4 } else { 3 };
    let alpha = has_alpha.then(|| planes[planes.len() - 1].as_slice());
    let mut out = vec![0u8; num_pixels * stride];

    let fill = |out: &mut [u8], color: &dyn Fn(usize) -> [u8; 3]| {
        for (index, pixel) in out.chunks_exact_mut(stride).enumerate() {
            pixel[..3].copy_from_slice(&color(index));
            if let Some(alpha) = alpha {
                pixel[3] = alpha[index];
            }
        }
    };

    match color_mode {
        MODE_RGB => {
            let (r, g, b) = (plane(0), plane(1), plane(2));
            fill(&mut out, &|i| [r[i], g[i], b[i]]);
        }
        MODE_CMYK => {
            // Photoshop stores CMYK inverted (255 = no ink), so R = C' * K' / 255.
            let (c, m, y, k) = (plane(0), plane(1), plane(2), plane(3));
            let mix = |value: u8, k: u8| (u32::from(value) * u32::from(k) / 255) as u8;
            fill(&mut out, &|i| {
                [mix(c[i], k[i]), mix(m[i], k[i]), mix(y[i], k[i])]
            });
        }
        MODE_GRAYSCALE => {
            let gray = plane(0);
            fill(&mut out, &|i| [gray[i]; 3]);
        }
        MODE_INDEXED => {
            let indices = plane(0);
            fill(&mut out, &|i| {
                let v = usize::from(indices[i]);
                [palette[v], palette[256 + v], palette[512 + v]]
            });
        }
        _ => {}
    }
    out
}

/// Decode one PackBits row into `out`, padded or truncated to exactly
/// `expected_len` bytes.
fn decode_packbits(input: &[u8], expected_len: usize, out: &mut Vec<u8>) {
    out.clear();
    let mut i = 0;
    while i < input.len() && out.len() < expected_len {
        let n = input[i] as i8;
        i += 1;
        let room = expected_len - out.len();
        if n >= 0 {
            let end = (i + n as usize + 1).min(input.len());
            let take = (end - i).min(room);
            out.extend_from_slice(&input[i..i + take]);
            i = end;
        } else if n != -128 {
            let Some(&value) = input.get(i) else { break };
            i += 1;
            let count = (1 - isize::from(n)) as usize;
            out.resize(out.len() + count.min(room), value);
        }
    }
    out.resize(expected_len, 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_PSD_2X2: &[u8] = &[
        56, 66, 80, 83, 0, 1, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 2, 0, 0, 0, 2, 0, 8, 0, 3, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 3, 0, 3, 0, 3, 0, 3, 0, 3, 0, 3, 1, 255, 255, 1, 0, 0,
        1, 0, 0, 1, 255, 255, 1, 0, 0, 1, 0, 0,
    ];

    const TEST_PSD_RGBA: &[u8] = &[
        56, 66, 80, 83, 0, 1, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 2, 0, 0, 0, 2, 0, 8, 0, 3, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 3, 0, 3, 0, 3, 0, 3, 0, 3, 0, 3, 0, 3, 0, 3, 1, 255,
        255, 1, 0, 0, 1, 0, 0, 1, 255, 255, 1, 0, 0, 1, 0, 0, 1, 255, 0, 1, 128, 255,
    ];

    fn load(bytes: &[u8]) -> Result<DynamicImage> {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), bytes).unwrap();
        decode(file.path())
    }

    #[test]
    fn decodes_rgb_composite() {
        let image = load(TEST_PSD_2X2).unwrap();
        assert!(matches!(image, DynamicImage::ImageRgb8(_)));
        let rgb = image.to_rgb8();
        assert_eq!(rgb.dimensions(), (2, 2));
        assert_eq!(rgb.get_pixel(0, 0).0, [255, 0, 0]);
        assert_eq!(rgb.get_pixel(0, 1).0, [0, 255, 0]);
    }

    /// `TEST_PSD_RGBA` with a layer section whose layer count is negative
    /// (-1): the 4th channel is the merged transparency.
    fn transparent_rgba() -> Vec<u8> {
        let mut psd = TEST_PSD_RGBA.to_vec();
        // Layer section length (offset 34), then: layer info length 2, layer
        // count -1, no global layer mask.
        psd[34..38].copy_from_slice(&10u32.to_be_bytes());
        psd.splice(38..38, [0, 0, 0, 2, 0xFF, 0xFF, 0, 0, 0, 0]);
        psd
    }

    #[test]
    fn decodes_rgba_composite() {
        let rgba = load(&transparent_rgba()).unwrap().to_rgba8();
        assert_eq!(rgba.get_pixel(1, 0).0, [255, 0, 0, 0]);
        assert_eq!(rgba.get_pixel(0, 1).0, [0, 255, 0, 128]);
    }

    /// Without layers saying so, an extra channel is a saved selection: the
    /// picture stays opaque.
    #[test]
    fn saved_selection_is_not_transparency() {
        let image = load(TEST_PSD_RGBA).unwrap();
        assert!(matches!(image, DynamicImage::ImageRgb8(_)));
        let rgb = image.to_rgb8();
        assert_eq!(rgb.get_pixel(1, 0).0, [255, 0, 0]);
        assert_eq!(rgb.get_pixel(0, 1).0, [0, 255, 0]);
    }

    #[test]
    fn rejects_unsupported_depth_and_truncation() {
        let mut deep = TEST_PSD_2X2.to_vec();
        deep[23] = 32;
        assert!(load(&deep).is_err());
        for len in 0..TEST_PSD_2X2.len() {
            assert!(load(&TEST_PSD_2X2[..len]).is_err(), "{len}");
        }
    }

    /// A tiny RLE file whose header claims 64 M rows of 56 channels is
    /// rejected without allocating the line table (several GB).
    #[test]
    fn huge_rle_header_is_rejected_cheaply() {
        let mut psd = TEST_PSD_2X2.to_vec();
        psd[12..14].copy_from_slice(&56u16.to_be_bytes());
        psd[14..18].copy_from_slice(&64_000_000u32.to_be_bytes());
        psd[18..22].copy_from_slice(&1u32.to_be_bytes());
        assert!(load(&psd).is_err());
        psd[12..14].copy_from_slice(&57u16.to_be_bytes());
        assert!(load(&psd).is_err());
    }

    #[test]
    fn packbits_pads_and_truncates() {
        let mut out = Vec::new();
        decode_packbits(&[0xFE, 7, 0, 9], 5, &mut out);
        assert_eq!(out, [7, 7, 7, 9, 0]);
        decode_packbits(&[0x02, 1, 2, 3, 0xFD, 4], 4, &mut out);
        assert_eq!(out, [1, 2, 3, 4]);
    }
}
