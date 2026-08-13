use super::{DecodeError, Decoder};
use crate::image_types::PixImage;
use std::io::Cursor;

/// Bridges the `image` crate for the common formats. Produces RGBA only
/// (palette is not preserved here) — see `pcx.rs` for the palette-preserving
/// pattern to copy for IFF/ILBM and other indexed formats.
pub struct ImageCrateDecoder;

impl Decoder for ImageCrateDecoder {
    fn name(&self) -> &'static str {
        "image-crate"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &[
            "png", "gif", "bmp", "jpg", "jpeg", "webp", "tga", "tif", "tiff", "ppm", "pgm", "pbm",
            "pnm", "qoi", "ico", "cur",
            // A DRAW project (.draw) is a valid PNG with an extra ancillary `drAw`
            // chunk (ignored by PNG decoders), so the flattened preview decodes here.
            "draw",
        ]
    }

    fn sniff(&self, header: &[u8]) -> bool {
        image::guess_format(header).is_ok()
    }

    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        let reader = image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|e| DecodeError::Io(e.to_string()))?;
        let dyn_img = reader
            .decode()
            .map_err(|e| DecodeError::Malformed(e.to_string()))?;
        Ok(to_pix(dyn_img))
    }
}

impl ImageCrateDecoder {
    /// Decode with the format supplied rather than guessed.
    ///
    /// **TGA has no magic bytes.** Unlike every other format in `extensions()` it carries no
    /// signature at the start of the file (only an optional *footer*, and only since v2.0), so
    /// `with_guessed_format` cannot identify it and `decode` fails on every `.tga` ever opened —
    /// the extension was advertised, the file was listed, and the thumbnail then spun forever.
    /// Routed by extension from `decode_bytes`, the same way source code and audio are, because
    /// the `Decoder::decode(bytes)` signature has no way to pass the hint.
    pub fn decode_with(bytes: &[u8], format: image::ImageFormat) -> Result<PixImage, DecodeError> {
        let dyn_img = image::load_from_memory_with_format(bytes, format)
            .map_err(|e| DecodeError::Malformed(e.to_string()))?;
        Ok(to_pix(dyn_img))
    }
}

/// `DynamicImage` → our RGBA `PixImage`. Shared so the guessed and hinted paths cannot diverge.
fn to_pix(dyn_img: image::DynamicImage) -> PixImage {
    let rgba = dyn_img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    let pixels = rgba
        .chunks_exact(4)
        .map(|c| [c[0], c[1], c[2], c[3]])
        .collect();
    PixImage::from_rgba(w, h, pixels)
}
