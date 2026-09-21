use super::{DecodeError, Decoder};
use crate::image_types::PixImage;
use std::io::Cursor;

/// Aseprite (`.aseprite` / `.ase`) via the `asefile` crate — decodes the
/// composited first frame. Aseprite is *the* pixel-art editor, so this is a
/// high-value format for this viewer. (asefile pulls its own `image` version,
/// so we only touch its raw bytes, never mix image types.)
pub struct AsepriteDecoder;

impl Decoder for AsepriteDecoder {
    fn name(&self) -> &'static str {
        "aseprite"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["aseprite", "ase"]
    }

    fn sniff(&self, header: &[u8]) -> bool {
        // The Aseprite header has the little-endian magic word 0xA5E0 at offset 4.
        header.len() >= 6 && header[4] == 0xE0 && header[5] == 0xA5
    }

    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        let ase = asefile::AsepriteFile::read(Cursor::new(bytes))
            .map_err(|e| DecodeError::Malformed(e.to_string()))?;
        let img = ase.frame(0).image(); // composited frame 0
        let (w, h) = (img.width(), img.height());
        let pixels = img
            .into_raw()
            .chunks_exact(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect();
        Ok(PixImage::from_rgba(w, h, pixels))
    }
}
