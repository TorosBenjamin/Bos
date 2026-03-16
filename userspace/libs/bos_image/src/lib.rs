//! Image decoding for BosOS.
//!
//! Wraps `minipng` (PNG) and `zune-jpeg` (JPEG) into a unified API.
//! Works in both `std` and `no_std + alloc` environments.
//!
//! # Example
//! ```no_run
//! let data: &[u8] = /* file or HTTP body */;
//! let image = bos_image::decode(data).expect("unsupported image");
//! // image.width, image.height, image.pixels (RGBA8)
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

// ── Public types ──────────────────────────────────────────────────────────────

/// A decoded image stored as RGBA8 pixels.
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA pixels: `[R, G, B, A, R, G, B, A, ...]`.
    /// Length is always `width * height * 4`.
    pub pixels: Vec<u8>,
}

#[derive(Debug)]
pub enum DecodeError {
    /// Not a recognized image format (PNG or JPEG).
    UnknownFormat,
    /// PNG decoding failed.
    Png,
    /// JPEG decoding failed.
    Jpeg,
}

// ── Format detection ─────────────────────────────────────────────────────────

/// PNG files start with these 8 bytes.
const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// JPEG files start with FF D8 FF.
fn is_jpeg(data: &[u8]) -> bool {
    data.len() >= 3 && data[0] == 0xFF && data[1] == 0xD8 && data[2] == 0xFF
}

fn is_png(data: &[u8]) -> bool {
    data.len() >= 8 && data[..8] == *PNG_MAGIC
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Decode an image from raw bytes (auto-detects PNG or JPEG).
///
/// Returns RGBA8 pixel data regardless of the source format.
pub fn decode(data: &[u8]) -> Result<Image, DecodeError> {
    if is_png(data) {
        decode_png(data)
    } else if is_jpeg(data) {
        decode_jpeg(data)
    } else {
        Err(DecodeError::UnknownFormat)
    }
}

/// Decode a PNG image from raw bytes.
pub fn decode_png(data: &[u8]) -> Result<Image, DecodeError> {
    let header = minipng::decode_png_header(data).map_err(|_| DecodeError::Png)?;
    let mut buf = vec![0u8; header.required_bytes_rgba8bpc()];
    let mut image = minipng::decode_png(data, &mut buf).map_err(|_| DecodeError::Png)?;
    image.convert_to_rgba8bpc().map_err(|_| DecodeError::Png)?;

    let width = image.width();
    let height = image.height();
    let pixels = image.pixels().to_vec();

    Ok(Image { width, height, pixels })
}

/// Decode a JPEG image from raw bytes.
pub fn decode_jpeg(data: &[u8]) -> Result<Image, DecodeError> {
    use zune_core::bytestream::ZCursor;
    use zune_core::options::DecoderOptions;
    use zune_jpeg::JpegDecoder;

    let cursor = ZCursor::new(data);
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(zune_core::colorspace::ColorSpace::RGBA);
    let mut decoder = JpegDecoder::new_with_options(cursor, options);
    let pixels = decoder.decode().map_err(|_| DecodeError::Jpeg)?;

    let info = decoder.info().ok_or(DecodeError::Jpeg)?;
    let width = info.width as u32;
    let height = info.height as u32;

    Ok(Image { width, height, pixels })
}
