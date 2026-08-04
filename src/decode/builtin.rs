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
        let rgba = dyn_img.to_rgba8();
        let (w, h) = (rgba.width(), rgba.height());
        let pixels = rgba
            .chunks_exact(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect();
        Ok(PixImage::from_rgba(w, h, pixels))
    }
}
