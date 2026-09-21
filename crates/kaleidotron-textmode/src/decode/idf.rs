//! IDF (.idf) — iCE Draw format: a 12-byte header (magic `0x04 "1.4"` + the
//! canvas bounds), an RLE-compressed char/attribute stream, then the embedded
//! font (4096 bytes) and palette (48 bytes, 6-bit) at the **end** of the file.
//! Ported from the ansilove reference loader (`icedraw.c`).

use super::{DecodeError, Decoder};
use crate::image_types::PixImage;

pub struct IdfDecoder;

const HEADER: usize = 12;
const FONT: usize = 4096; // 256 glyphs × 16 rows
const PALETTE: usize = 48; // 16 colors × 3, 6-bit
const MAX_CELLS: usize = 250_000;

impl Decoder for IdfDecoder {
    fn name(&self) -> &'static str {
        "idf"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["idf"]
    }

    fn sniff(&self, header: &[u8]) -> bool {
        // 0x04 then "1.4" (some files use "1.5"); accept either minor version.
        header.len() >= 4
            && header[0] == 0x04
            && (&header[1..4] == b"1.4" || &header[1..4] == b"1.5")
    }

    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        if bytes.len() < HEADER + FONT + PALETTE || !self.sniff(bytes) {
            return Err(DecodeError::Malformed("not an IDF".into()));
        }
        // Canvas width = x1 + 1 (x1 at bytes 8..10, little-endian).
        let width = (u16::from_le_bytes([bytes[8], bytes[9]]) as usize + 1).clamp(1, 4096);
        let data = &bytes[HEADER..bytes.len() - FONT - PALETTE];

        // RLE: a literal (char, attr) pair, unless the char byte is 1 — then it's a
        // run of `data[k+2]` copies of the pair at (data[k+4], data[k+5]).
        let mut pairs: Vec<(u8, u8)> = Vec::new();
        let mut k = 0;
        while k < data.len() {
            if data[k] == 1 {
                let count = data.get(k + 2).copied().unwrap_or(0) as usize;
                let ch = data.get(k + 4).copied().unwrap_or(b' ');
                let at = data.get(k + 5).copied().unwrap_or(0x07);
                for _ in 0..count {
                    pairs.push((ch, at));
                }
                k += 6;
            } else {
                pairs.push((data[k], data.get(k + 1).copied().unwrap_or(0x07)));
                k += 2;
            }
            if pairs.len() > MAX_CELLS {
                return Err(DecodeError::Malformed("IDF too large".into()));
            }
        }
        if pairs.is_empty() {
            return Err(DecodeError::Malformed("empty IDF".into()));
        }
        let rows = pairs.len().div_ceil(width);

        // Palette (last 48 bytes) + font (the 4096 before it).
        let pal_off = bytes.len() - PALETTE;
        let mut palette = [[0u8; 3]; 16];
        for (i, c) in palette.iter_mut().enumerate() {
            let o = pal_off + i * 3;
            *c = [
                super::xbin::vga6to8(bytes[o]),
                super::xbin::vga6to8(bytes[o + 1]),
                super::xbin::vga6to8(bytes[o + 2]),
            ];
        }
        let font = &bytes[bytes.len() - FONT - PALETTE..pal_off];
        // IDF is iCE (16 background colors, no blink).
        Ok(super::xbin::render_textmode(
            width,
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

