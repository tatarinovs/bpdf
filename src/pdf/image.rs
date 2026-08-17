use anyhow::{Context, Result, bail};
use image::{DynamicImage, GrayImage, RgbImage};
use lopdf::{Object, Stream};

fn decompress_flate(data: &[u8]) -> Result<Vec<u8>> {
    miniz_oxide::inflate::decompress_to_vec_zlib(data)
        .or_else(|_| miniz_oxide::inflate::decompress_to_vec(data))
        .map_err(|e| anyhow::anyhow!("flate decompression failed: {e:?}"))
}

pub(crate) fn decode(stream: &Stream) -> Result<DynamicImage> {
    let filters = stream.filters().unwrap_or_default();

    // Check if the stream contains JPEG (DCTDecode / DCT)
    if filters.iter().any(|f| *f == b"DCTDecode" || *f == b"DCT") {
        if stream.content.starts_with(&[0xff, 0xd8]) {
            if let Ok(img) = image::load_from_memory(&stream.content) {
                return Ok(img);
            }
        }
        if let Ok(decompressed) = decompress_flate(&stream.content) {
            if decompressed.starts_with(&[0xff, 0xd8]) {
                if let Ok(img) = image::load_from_memory(&decompressed) {
                    return Ok(img);
                }
            }
            if let Ok(img) = image::load_from_memory(&decompressed) {
                return Ok(img);
            }
        }
    }

    if stream.content.starts_with(&[0xff, 0xd8]) {
        if let Ok(img) = image::load_from_memory(&stream.content) {
            return Ok(img);
        }
    }

    let width = dimension(stream, b"Width")?;
    let height = dimension(stream, b"Height")?;
    let bits = stream
        .dict
        .get(b"BitsPerComponent")
        .ok()
        .and_then(|value| value.as_i64().ok())
        .unwrap_or(8);

    let color_space = stream
        .dict
        .get(b"ColorSpace")
        .ok()
        .and_then(color_space_name)
        .unwrap_or("DeviceGray");

    let channels = match color_space {
        "DeviceGray" | "CalGray" => 1usize,
        "DeviceRGB" | "CalRGB" => 3,
        "DeviceCMYK" => 4,
        _ => {
            if color_space.contains("RGB") {
                3
            } else if color_space.contains("CMYK") {
                4
            } else {
                1
            }
        }
    };

    let raw = if filters.is_empty() {
        stream.content.clone()
    } else if filters.iter().any(|f| *f == b"FlateDecode" || *f == b"Fl") {
        decompress_flate(&stream.content).unwrap_or_else(|_| stream.content.clone())
    } else {
        stream
            .decompressed_content_with_limit(super::MAX_DECOMPRESSED_BYTES)
            .unwrap_or_else(|_| stream.content.clone())
    };

    if raw.starts_with(&[0xff, 0xd8]) || raw.starts_with(&[0x89, b'P', b'N', b'G']) {
        if let Ok(img) = image::load_from_memory(&raw) {
            return Ok(img);
        }
    }

    if bits == 1 {
        let row_bytes = (width as usize + 7) / 8;
        let invert = stream
            .dict
            .get(b"Decode")
            .ok()
            .and_then(|d| d.as_array().ok())
            .and_then(|arr| arr.first().and_then(|v| v.as_i64().ok()))
            .map(|v| v == 1)
            .unwrap_or(false);

        let mut gray_pixels = Vec::with_capacity((width * height) as usize);
        for y in 0..height as usize {
            let row_start = y * row_bytes;
            if row_start >= raw.len() {
                break;
            }
            let row_end = (row_start + row_bytes).min(raw.len());
            let row = &raw[row_start..row_end];
            for x in 0..width as usize {
                let byte_idx = x / 8;
                let bit_idx = 7 - (x % 8);
                let bit = if byte_idx < row.len() {
                    (row[byte_idx] >> bit_idx) & 1
                } else {
                    0
                };
                let pixel = if (bit == 1) ^ invert { 255u8 } else { 0u8 };
                gray_pixels.push(pixel);
            }
        }
        if gray_pixels.len() < (width * height) as usize {
            gray_pixels.resize((width * height) as usize, 255);
        }
        return Ok(DynamicImage::ImageLuma8(
            GrayImage::from_raw(width, height, gray_pixels)
                .context("failed to create 1-bit grayscale image")?,
        ));
    }

    if bits == 8 {
        return match color_space {
            "DeviceGray" | "CalGray" => Ok(DynamicImage::ImageLuma8(
                GrayImage::from_raw(width, height, raw).context("invalid grayscale image data")?,
            )),
            "DeviceCMYK" => Ok(DynamicImage::ImageRgb8(
                RgbImage::from_raw(width, height, cmyk_to_rgb(&raw))
                    .context("invalid converted CMYK image")?,
            )),
            _ if channels == 1 => Ok(DynamicImage::ImageLuma8(
                GrayImage::from_raw(width, height, raw).context("invalid grayscale image data")?,
            )),
            _ if channels == 4 => Ok(DynamicImage::ImageRgb8(
                RgbImage::from_raw(width, height, cmyk_to_rgb(&raw))
                    .context("invalid converted CMYK image")?,
            )),
            _ => Ok(DynamicImage::ImageRgb8(
                RgbImage::from_raw(width, height, raw).context("invalid RGB image data")?,
            )),
        };
    }

    if bits == 16 {
        let raw8: Vec<u8> = raw.chunks_exact(2).map(|chunk| chunk[0]).collect();
        return match channels {
            1 => Ok(DynamicImage::ImageLuma8(
                GrayImage::from_raw(width, height, raw8)
                    .context("invalid 16-to-8 grayscale image data")?,
            )),
            4 => Ok(DynamicImage::ImageRgb8(
                RgbImage::from_raw(width, height, cmyk_to_rgb(&raw8))
                    .context("invalid converted 16-to-8 CMYK image")?,
            )),
            _ => Ok(DynamicImage::ImageRgb8(
                RgbImage::from_raw(width, height, raw8)
                    .context("invalid 16-to-8 RGB image data")?,
            )),
        };
    }

    bail!("unsupported bits per component: {bits}")
}

pub(crate) fn dimension(stream: &Stream, key: &[u8]) -> Result<u32> {
    let value = stream.dict.get(key)?.as_i64()?;
    u32::try_from(value).context("invalid embedded image dimension")
}

fn color_space_name(object: &Object) -> Option<&str> {
    let name = match object {
        Object::Name(name) => name.as_slice(),
        Object::Array(values) => values.first()?.as_name().ok()?,
        _ => return None,
    };
    std::str::from_utf8(name).ok()
}

fn cmyk_to_rgb(cmyk: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(cmyk.len() / 4 * 3);
    for pixel in cmyk.chunks_exact(4) {
        let c = f64::from(pixel[0]) / 255.0;
        let m = f64::from(pixel[1]) / 255.0;
        let y = f64::from(pixel[2]) / 255.0;
        let k = f64::from(pixel[3]) / 255.0;
        rgb.push((255.0 * (1.0 - c) * (1.0 - k)).round() as u8);
        rgb.push((255.0 * (1.0 - m) * (1.0 - k)).round() as u8);
        rgb.push((255.0 * (1.0 - y) * (1.0 - k)).round() as u8);
    }
    rgb
}

#[cfg(test)]
mod tests {
    use lopdf::{Stream, dictionary};

    use super::*;

    #[test]
    fn decodes_raw_rgb_stream() {
        let stream = Stream::new(
            dictionary! {
                "Subtype" => "Image",
                "Width" => 2,
                "Height" => 1,
                "ColorSpace" => "DeviceRGB",
                "BitsPerComponent" => 8,
            },
            vec![255, 0, 0, 0, 255, 0],
        );
        assert_eq!(
            decode(&stream).unwrap().into_rgb8().into_raw(),
            stream.content
        );
    }

    #[test]
    fn decodes_raw_cmyk_stream() {
        let stream = Stream::new(
            dictionary! {
                "Subtype" => "Image",
                "Width" => 1,
                "Height" => 1,
                "ColorSpace" => "DeviceCMYK",
                "BitsPerComponent" => 8,
            },
            vec![0, 255, 255, 0],
        );
        assert_eq!(decode(&stream).unwrap().into_rgb8().into_raw(), [255, 0, 0]);
    }
}
