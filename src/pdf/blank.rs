//! Blank page detection for scans: a page is blank when it has no text,
//! draws nothing but images, and every image is a sheet of one tone (paper
//! with scanner noise, dust and margin shadows).

use anyhow::Result;
use image::GenericImageView;
use lopdf::content::Content;
use lopdf::{Document, Object, ObjectId};

use super::{MAX_DECOMPRESSED_BYTES, image as pdf_image, resolve, select_pages};
use crate::parallel;

/// Long edge of the thumbnail an image is judged on; smaller specks vanish.
const THUMBNAIL_EDGE: u32 = 300;
/// Part of each edge ignored as scanner shadow.
const MARGIN: f64 = 0.04;
/// Luminance difference from the paper tone that counts as ink.
const INK_DIFFERENCE: u8 = 50;
/// Largest share of ink pixels on a blank sheet.
const MAX_INK_SHARE: f64 = 0.0025;

/// Remove blank pages; returns their numbers. A document whose pages are all
/// blank is left alone (an empty PDF helps nobody).
pub fn remove_blank_pages(document: &mut Document) -> Result<Vec<u32>> {
    let blank = blank_pages(document);
    if !blank.is_empty() && blank.len() == document.get_pages().len() {
        crate::output::warn("every page looks blank; none were removed");
        return Ok(Vec::new());
    }
    drop_pages(document, &blank)?;
    Ok(blank)
}

/// Remove the pages numbered `pages`; at least one page must stay.
pub fn drop_pages(document: &mut Document, pages: &[u32]) -> Result<()> {
    if pages.is_empty() {
        return Ok(());
    }
    let count = document.get_pages().len() as u32;
    let kept = (1..=count)
        .filter(|number| !pages.contains(number))
        .map(|number| number.to_string())
        .collect::<Vec<_>>()
        .join(",");
    select_pages(document, &kept)
}

/// Numbers of the blank pages, in order.
pub fn blank_pages(document: &Document) -> Vec<u32> {
    let pages = document.get_pages().into_iter().collect::<Vec<_>>();
    parallel::map(&pages, parallel::cpu_jobs(), |&(number, page_id)| {
        is_blank(document, number, page_id).then_some(number)
    })
    .into_iter()
    .flatten()
    .collect()
}

fn is_blank(document: &Document, number: u32, page_id: ObjectId) -> bool {
    let has_annotations = document
        .get_dictionary(page_id)
        .is_ok_and(|page| page.has(b"Annots"));
    let has_text = document
        .extract_text_with_limit(&[number], MAX_DECOMPRESSED_BYTES)
        .is_ok_and(|text| text.chars().any(|character| !character.is_whitespace()));
    if has_annotations || has_text {
        return false;
    }
    let Some(images) = drawn_images(document, page_id) else {
        return false;
    };
    images
        .into_iter()
        .all(|id| image_is_blank(document, id).unwrap_or(false))
}

/// Images the page content draws, or `None` when it draws anything else
/// (text, paths, shadings, inline images, forms) or cannot be read.
fn drawn_images(document: &Document, page_id: ObjectId) -> Option<Vec<ObjectId>> {
    let content = document
        .get_page_content_with_limit(page_id, MAX_DECOMPRESSED_BYTES)
        .ok()?;
    let operations = Content::decode(&content).ok()?.operations;
    let resources = super::transform::inherited_value(document, page_id, b"Resources");
    let xobjects = resources
        .as_ref()
        .and_then(|resources| resolve(document, resources).ok()?.as_dict().ok())
        .and_then(|resources| resolve(document, resources.get(b"XObject").ok()?).ok())
        .and_then(|xobjects| xobjects.as_dict().ok());
    let mut images = Vec::new();
    for operation in operations {
        match operation.operator.as_str() {
            "Tj" | "TJ" | "'" | "\"" | "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*"
            | "sh" | "BI" | "EI" => return None,
            "Do" => {
                let name = operation.operands.first()?.as_name().ok()?;
                let id = xobjects?.get(name).ok()?.as_reference().ok()?;
                let stream = document.get_object(id).ok()?.as_stream().ok()?;
                let is_image = stream
                    .dict
                    .get(b"Subtype")
                    .and_then(Object::as_name)
                    .is_ok_and(|kind| kind == b"Image");
                let is_stencil = stream
                    .dict
                    .get(b"ImageMask")
                    .and_then(Object::as_bool)
                    .unwrap_or(false);
                if !is_image || is_stencil {
                    return None;
                }
                images.push(id);
            }
            _ => {}
        }
    }
    Some(images)
}

fn image_is_blank(document: &Document, id: ObjectId) -> Result<bool> {
    let stream = document.get_object(id)?.as_stream()?;
    let image = match pdf_image::embedded_jpeg(stream) {
        Some(jpeg) => crate::imageconv::decode_bytes(jpeg)?,
        None => pdf_image::decode(document, stream)?,
    };
    let thumbnail = image.thumbnail(THUMBNAIL_EDGE, THUMBNAIL_EDGE).to_luma8();
    Ok(is_plain_sheet(&image::DynamicImage::ImageLuma8(thumbnail)))
}

/// True when almost every pixel inside the margins has the paper tone.
fn is_plain_sheet(image: &image::DynamicImage) -> bool {
    let (width, height) = image.dimensions();
    let (left, top) = (
        (f64::from(width) * MARGIN) as u32,
        (f64::from(height) * MARGIN) as u32,
    );
    let inner = image
        .crop_imm(
            left,
            top,
            width.saturating_sub(2 * left),
            height.saturating_sub(2 * top),
        )
        .to_luma8();
    let pixels = inner.as_raw();
    if pixels.is_empty() {
        return true;
    }
    let mut histogram = [0usize; 256];
    for &value in pixels {
        histogram[usize::from(value)] += 1;
    }
    let mut seen = 0;
    let paper = histogram
        .iter()
        .position(|&count| {
            seen += count;
            seen * 2 >= pixels.len()
        })
        .unwrap_or(255) as u8;
    let ink = pixels
        .iter()
        .filter(|&&value| value.abs_diff(paper) > INK_DIFFERENCE)
        .count();
    (ink as f64) / (pixels.len() as f64) <= MAX_INK_SHARE
}

#[cfg(test)]
mod tests {
    use image::{DynamicImage, GrayImage, Luma};

    use super::*;
    use crate::pdf::{PageSize, jpeg_document, merge_documents};
    use crate::textpdf::{self, TextOptions};

    fn scan(draw_text: bool) -> Document {
        let mut image = GrayImage::from_fn(600, 840, |x, y| {
            // Paper with mild noise and a dark shadow along the left edge.
            let noise = ((x * 7 + y * 13) % 9) as u8;
            Luma([if x < 10 { 40 } else { 240 - noise }])
        });
        if draw_text {
            for y in (200..600).step_by(30) {
                for x in 80..520 {
                    for line in 0..6 {
                        image.put_pixel(x, y + line, Luma([20]));
                    }
                }
            }
        }
        let jpeg =
            crate::imageconv::encode_jpeg_on_white(&DynamicImage::ImageLuma8(image), 85).unwrap();
        jpeg_document(jpeg, PageSize::A4).unwrap()
    }

    #[test]
    fn finds_blank_scans_and_keeps_content() {
        let text = textpdf::render("hello", &TextOptions::default()).unwrap();
        let mut document =
            merge_documents(vec![scan(true), scan(false), text, scan(false)]).unwrap();
        assert_eq!(blank_pages(&document), [2, 4]);
        assert_eq!(remove_blank_pages(&mut document).unwrap(), [2, 4]);
        assert_eq!(document.get_pages().len(), 2);
    }

    #[test]
    fn an_all_blank_document_is_kept() {
        let mut document = merge_documents(vec![scan(false), scan(false)]).unwrap();
        assert!(remove_blank_pages(&mut document).unwrap().is_empty());
        assert_eq!(document.get_pages().len(), 2);
    }
}
