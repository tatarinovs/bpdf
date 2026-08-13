use std::io::Cursor;

use anyhow::{Context, Result, bail};
use image::{DynamicImage, GrayImage, ImageReader, Limits, RgbImage};
use lopdf::{Object, Stream};

pub(crate) fn decode(stream: &Stream) -> Result<DynamicImage> {
    let filters = stream.filters().unwrap_or_default();
    if filters.as_slice() == [b"DCTDecode".as_slice()] {
        let mut reader = ImageReader::new(Cursor::new(&stream.content))
            .with_guessed_format()
            .context("failed to detect embedded JPEG")?;
        let mut limits = Limits::default();
        limits.max_alloc = Some(super::MAX_DECOMPRESSED_BYTES as u64);
        reader.limits(limits);
        return reader.decode().context("failed to decode embedded JPEG");
    }
    if filters
        .iter()
        .any(|filter| !matches!(*filter, b"FlateDecode" | b"LZWDecode" | b"ASCII85Decode"))
    {
        bail!("unsupported PDF image filter");
    }

    let width = dimension(stream, b"Width")?;
    let height = dimension(stream, b"Height")?;
    let bits = stream
        .dict
        .get(b"BitsPerComponent")
        .ok()
        .and_then(|value| value.as_i64().ok())
        .unwrap_or(8);
    if bits != 8 {
        bail!("only 8-bit PDF images are supported");
    }
    let color_space = stream
        .dict
        .get(b"ColorSpace")
        .ok()
        .and_then(color_space_name)
        .context("unsupported PDF image color space")?;
    let channels = match color_space {
        "DeviceGray" => 1usize,
        "DeviceRGB" => 3,
        "DeviceCMYK" => 4,
        _ => bail!("unsupported PDF image color space {color_space}"),
    };
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(channels))
        .context("embedded image dimensions are too large")?;
    if expected > super::MAX_DECOMPRESSED_BYTES {
        bail!("embedded image exceeds the 256 MiB decoded-size limit");
    }
    let raw = if filters.is_empty() {
        if stream.content.len() > expected {
            bail!("embedded image data is longer than expected");
        }
        stream.content.clone()
    } else {
        stream
            .decompressed_content_with_limit(expected)
            .context("failed to decompress embedded image")?
    };
    if raw.len() != expected {
        bail!("embedded image data length does not match its dimensions");
    }

    match color_space {
        "DeviceGray" => Ok(DynamicImage::ImageLuma8(
            GrayImage::from_raw(width, height, raw).context("invalid grayscale image data")?,
        )),
        "DeviceRGB" => Ok(DynamicImage::ImageRgb8(
            RgbImage::from_raw(width, height, raw).context("invalid RGB image data")?,
        )),
        "DeviceCMYK" => Ok(DynamicImage::ImageRgb8(
            RgbImage::from_raw(width, height, cmyk_to_rgb(&raw))
                .context("invalid converted CMYK image")?,
        )),
        _ => unreachable!(),
    }
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
