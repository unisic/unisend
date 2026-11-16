//! Pictures prepared for sending and for the preview before it: orientation applied, metadata
//! gone (a re-encode carries none), long side bounded. One full picture is decoded at a time by
//! whoever calls these.

use std::io::Cursor;
use std::path::Path;

use anyhow::Result;
use image::{DynamicImage, ImageDecoder as _, ImageReader, codecs::jpeg::JpegEncoder, imageops::FilterType};

/// Longest side of a picture sent without HD.
pub const SEND_PX: u32 = 2048;
const SEND_QUALITY: u8 = 85;
/// Longest side of the picture the preview shows large.
pub const PREVIEW_PX: u32 = 1280;
/// Longest side of a filmstrip thumbnail.
pub const STRIP_PX: u32 = 160;

/// A picture ready to upload.
pub struct Prepared {
    pub bytes: Vec<u8>,
    pub name: String,
    pub mime: mime_guess::mime::Mime,
    pub width: u32,
    pub height: u32,
}

/// Formats re-encoded as a still: GIF and WebP may be animated, which a re-encode would flatten.
pub fn recodable(mime: &mime_guess::mime::Mime) -> bool {
    matches!(mime.subtype().as_str(), "jpeg" | "png" | "bmp" | "tiff")
}

/// Longest side of the thumbnail sent with a picture or a video.
pub const THUMB_PX: u32 = 800;

fn open(path: &Path) -> Result<DynamicImage> {
    upright(ImageReader::open(path)?)
}

fn upright<R: std::io::BufRead + std::io::Seek>(reader: ImageReader<R>) -> Result<DynamicImage> {
    let mut decoder = reader.with_guessed_format()?.into_decoder()?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut picture = DynamicImage::from_decoder(decoder)?;
    picture.apply_orientation(orientation);
    Ok(picture)
}

fn fit(picture: DynamicImage, max: u32, filter: FilterType) -> DynamicImage {
    if picture.width().max(picture.height()) > max { picture.resize(max, max, filter) } else { picture }
}

/// JPEG, or PNG for a picture with transparency.
fn encode(picture: &DynamicImage, quality: u8) -> Result<(Vec<u8>, &'static str)> {
    let mut out = Cursor::new(Vec::new());
    if picture.color().has_alpha() {
        picture.write_to(&mut out, image::ImageFormat::Png)?;
        Ok((out.into_inner(), "png"))
    } else {
        JpegEncoder::new_with_quality(&mut out, quality).encode_image(&picture.to_rgb8())?;
        Ok((out.into_inner(), "jpg"))
    }
}

/// The file as sent without HD: at most `SEND_PX` on the long side, upright, without metadata.
pub fn downscale(path: &Path) -> Result<Prepared> {
    let picture = fit(open(path)?, SEND_PX, FilterType::Lanczos3);
    let (bytes, ext) = encode(&picture, SEND_QUALITY)?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "image".to_owned());
    Ok(Prepared {
        bytes,
        name: format!("{stem}.{ext}"),
        mime: if ext == "png" { mime_guess::mime::IMAGE_PNG } else { mime_guess::mime::IMAGE_JPEG },
        width: picture.width(),
        height: picture.height(),
    })
}

/// The thumbnail other clients show before fetching `bytes` (a picture or a video still), upright
/// and at most `THUMB_PX` on the long side. `None` when the picture is no larger than that.
pub fn thumbnail(bytes: &[u8]) -> Result<Option<Prepared>> {
    let picture = upright(ImageReader::new(Cursor::new(bytes)))?;
    if picture.width().max(picture.height()) <= THUMB_PX {
        return Ok(None);
    }
    let picture = fit(picture, THUMB_PX, FilterType::Triangle);
    let (bytes, ext) = encode(&picture, 80)?;
    Ok(Some(Prepared {
        bytes,
        name: format!("thumbnail.{ext}"),
        mime: if ext == "png" { mime_guess::mime::IMAGE_PNG } else { mime_guess::mime::IMAGE_JPEG },
        width: picture.width(),
        height: picture.height(),
    }))
}

/// A filmstrip thumbnail, and when `large` the picture for the preview: both encoded, from one decode.
pub fn preview(path: &Path, large: bool) -> Result<(Vec<u8>, Option<Vec<u8>>)> {
    let picture = open(path)?;
    if large {
        let big = fit(picture, PREVIEW_PX, FilterType::Triangle);
        let strip = fit(big.clone(), STRIP_PX, FilterType::Triangle);
        Ok((encode(&strip, 80)?.0, Some(encode(&big, 85)?.0)))
    } else {
        Ok((encode(&fit(picture, STRIP_PX, FilterType::Triangle), 80)?.0, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_jpeg(path: &Path, w: u32, h: u32, orientation: u16) {
        // A JPEG with an EXIF segment holding only the orientation, and a GPS-looking marker.
        let mut plain = Cursor::new(Vec::new());
        JpegEncoder::new_with_quality(&mut plain, 90).encode_image(&DynamicImage::new_rgb8(w, h).to_rgb8()).unwrap();
        let plain = plain.into_inner();
        let mut exif = b"Exif\0\0MM\0\x2a\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01".to_vec();
        exif.extend_from_slice(&orientation.to_be_bytes());
        exif.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let mut out = plain[..2].to_vec();
        out.extend_from_slice(&[0xff, 0xe1]);
        out.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&exif);
        out.extend_from_slice(&plain[2..]);
        std::fs::write(path, out).unwrap();
    }

    #[test]
    fn a_big_sideways_picture_is_scaled_turned_upright_and_stripped() {
        let dir = std::env::temp_dir().join(format!("caption-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shot.jpg");
        // Orientation 6: rotate 90 degrees clockwise to display, so 4000x3000 stored is 3000x4000 shown.
        write_jpeg(&path, 4000, 3000, 6);
        let sent = downscale(&path).unwrap();
        assert_eq!(sent.name, "shot.jpg");
        assert_eq!((sent.width, sent.height), (1536, 2048));
        assert!(!sent.bytes.windows(4).any(|w| w == b"Exif"), "metadata survived");
        let (strip, large) = preview(&path, true).unwrap();
        let strip = image::load_from_memory(&strip).unwrap();
        let large = image::load_from_memory(&large.unwrap()).unwrap();
        assert_eq!((strip.width(), strip.height()), (120, 160));
        assert_eq!(large.height(), PREVIEW_PX);
        let thumb = thumbnail(&sent.bytes).unwrap().unwrap();
        assert_eq!((thumb.width, thumb.height), (600, THUMB_PX));
        assert!(thumbnail(&thumb.bytes).unwrap().is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn only_stills_are_recoded() {
        assert!(recodable(&mime_guess::mime::IMAGE_JPEG));
        assert!(recodable(&mime_guess::mime::IMAGE_PNG));
        assert!(!recodable(&mime_guess::mime::IMAGE_GIF));
        assert!(!recodable(&"image/webp".parse().unwrap()));
    }
}
