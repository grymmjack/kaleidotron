//! ADF (.adf) — Artworx Data Format: a 1-byte version, a 192-byte (64-color,
//! 6-bit) palette, a 4096-byte font, then uncompressed char/attribute pairs at a
//! fixed 80-column width. The 16 VGA slots map into the 64-color palette via a
//! fixed remap table. Ported from the ansilove reference loader (`artworx.c`).

use super::{DecodeError, Decoder};
use crate::image_types::PixImage;

pub struct AdfDecoder;

const HEADER: usize = 4289; // 1 version + 192 palette + 4096 font
const WIDTH: usize = 80;
const MAX_CELLS: usize = 250_000;

// Maps the 16 VGA colors onto entries of the stored 64-color (EGA) palette.
const ADF_COLORS: [usize; 16] = [0, 1, 2, 3, 4, 5, 20, 7, 56, 57, 58, 59, 60, 61, 62, 63];

impl Decoder for AdfDecoder {
    fn name(&self) -> &'static str {
        "adf"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["adf"]
    }

    fn sniff(&self, _header: &[u8]) -> bool {
        false // version byte 1 is too weak a magic — dispatch by .adf extension
    }

    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        if bytes.len() < HEADER {
            return Err(DecodeError::Malformed("not an ADF".into()));
        }
        // 16-color palette from the stored 64-color palette (offset 1, after version).
        let mut palette = [[0u8; 3]; 16];
        for (i, c) in palette.iter_mut().enumerate() {
            let o = ADF_COLORS[i] * 3 + 1;
            *c = [
                super::xbin::vga6to8(bytes[o]),
                super::xbin::vga6to8(bytes[o + 1]),
                super::xbin::vga6to8(bytes[o + 2]),
            ];
        }
        let font = &bytes[193..193 + 4096];

        // Image data (uncompressed pairs), minus any trailing SAUCE.
        let stripped = crate::sauce::strip(bytes);
        let data = stripped.get(HEADER..).unwrap_or(&[]);
        let pairs: Vec<(u8, u8)> = data.chunks_exact(2).map(|c| (c[0], c[1])).collect();
        if pairs.is_empty() {
            return Err(DecodeError::Malformed("empty ADF".into()));
        }
        let rows = pairs.len().div_ceil(WIDTH);
        if WIDTH * rows > MAX_CELLS {
            return Err(DecodeError::Malformed("ADF too large".into()));
        }
        Ok(super::xbin::render_textmode(
            WIDTH,
            rows,
            &pairs,
            &palette,
            Some(font),
            16,
            true,
            false,
        ))
    }
}

