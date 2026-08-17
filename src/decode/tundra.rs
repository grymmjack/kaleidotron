//! TundraDraw (.tnd) — a binary text-mode format with **24-bit truecolor** cells.
//! Header: `0x18 "TUNDRA24"` (9 bytes), then a command stream. Ported from the
//! ansilove reference loader (`tundra.c`).
//!
//! Commands (first byte of each cell): 1 = absolute position (two big-endian u32
//! row/col), 2 = set foreground (char + RGB), 4 = set background (char + RGB),
//! 6 = set both (char + fg RGB + bg RGB); anything else is a literal char drawn
//! with the current fg/bg. Canvas width comes from SAUCE (TInfo1) or defaults to 80.

use super::cp437_font::CP437_8X16;
use super::{DecodeError, Decoder};
use crate::image_types::PixImage;

pub struct TundraDecoder;

const HEADER: usize = 9;
const MAX_CELLS: usize = 250_000;

impl Decoder for TundraDecoder {
    fn name(&self) -> &'static str {
        "tundra"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["tnd"]
    }

    fn sniff(&self, header: &[u8]) -> bool {
        header.len() >= HEADER && header[0] == 24 && &header[1..9] == b"TUNDRA24"
    }

    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        if !self.sniff(bytes) {
            return Err(DecodeError::Malformed("not a TundraDraw".into()));
        }
        let cols = crate::sauce::parse(bytes)
            .and_then(|s| s.char_width())
            .unwrap_or(80)
            .clamp(1, 4096);
        let b = crate::sauce::strip(bytes);

        // A drawn cell: (col, row, char, fg_rgb, bg_rgb).
        type Op = (usize, usize, u8, [u8; 3], [u8; 3]);
        let mut ops: Vec<Op> = Vec::new();
        let (mut col, mut row) = (0usize, 0usize);
        let (mut fg, mut bg) = ([0u8; 3], [0u8; 3]);
        let mut max_row = 0usize;
        let mut i = HEADER;
        let rgb = |b: &[u8], at: usize| [b[at], b[at + 1], b[at + 2]];
        while i < b.len() {
            if col == cols {
                col = 0;
                row += 1;
            }
            let cursor = b[i];
            let mut character = cursor;
            match cursor {
                1 => {
                    if i + 8 >= b.len() {
                        break;
                    }
                    row = u32::from_be_bytes([b[i + 1], b[i + 2], b[i + 3], b[i + 4]]) as usize;
                    col = u32::from_be_bytes([b[i + 5], b[i + 6], b[i + 7], b[i + 8]]) as usize;
                    i += 8;
                }
                2 => {
                    if i + 5 >= b.len() {
                        break;
                    }
                    fg = rgb(b, i + 3);
                    character = b[i + 1];
                    i += 5;
                }
                4 => {
                    if i + 5 >= b.len() {
                        break;
                    }
                    bg = rgb(b, i + 3);
                    character = b[i + 1];
                    i += 5;
                }
                6 => {
                    if i + 9 >= b.len() {
                        break;
                    }
                    fg = rgb(b, i + 3);
                    bg = rgb(b, i + 7);
                    character = b[i + 1];
                    i += 9;
                }
                _ => {}
            }
            // A char of 1/2/4/6 is a command marker, not drawn (ansilove quirk).
            if !matches!(character, 1 | 2 | 4 | 6) {
                if col < cols && row < MAX_CELLS / cols.max(1) {
                    ops.push((col, row, character, fg, bg));
                    max_row = max_row.max(row);
                }
                col += 1;
            }
            i += 1;
        }
        if ops.is_empty() {
            return Err(DecodeError::Malformed("empty TundraDraw".into()));
        }
        let rows = max_row + 1;
        let w = cols * 8;
        let h = rows * 16;
        let mut pixels = vec![[0u8, 0, 0, 255]; w * h];
        for (cx, cy, ch, f, g) in ops {
            let glyph = &CP437_8X16[ch as usize];
            for (ry, &bits) in glyph.iter().enumerate() {
                for rx in 0..8 {
                    let on = (bits >> (7 - rx)) & 1 == 1;
                    let c = if on { f } else { g };
                    pixels[(cy * 16 + ry) * w + (cx * 8 + rx)] = [c[0], c[1], c[2], 255];
                }
            }
        }
        Ok(PixImage::from_rgba(w as u32, h as u32, pixels))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_magic() {
        assert!(TundraDecoder.sniff(b"\x18TUNDRA24rest"));
        assert!(!TundraDecoder.sniff(b"\x18TUNDRA00"));
        assert!(!TundraDecoder.sniff(b"nope"));
    }

    #[test]
    fn decodes_a_truecolor_cell() {
        // Header + FG command: char 0xDB (full block) in pure red.
        let mut d = b"\x18TUNDRA24".to_vec();
        d.extend_from_slice(&[2, 0xDB, 0x00, 0xFF, 0x00, 0x00]); // FG red, char block
        let img = TundraDecoder.decode(&d).unwrap();
        assert_eq!((img.width, img.height), (80 * 8, 16)); // default 80 cols
        assert_eq!(img.pixels[0], [255, 0, 0, 255]); // 24-bit red, not palette red
    }

    #[test]
    fn tundra_encoder_round_trips_through_decoder() {
        // Build a 2-cell grid and prove `ansi_grid_to_tundra`'s bytes decode back to
        // the exact 24-bit colours — the encoder is the inverse of this decoder.
        use crate::thumb::{ansi_grid_to_tundra, AnsiCell, AnsiGrid};
        let palette = vec![[255, 0, 0, 255], [0, 0, 255, 255], [0, 255, 0, 255]]; // red, blue, green
        let grid = AnsiGrid {
            cols: 2,
            rows: 1,
            cell_w: 8,
            cell_h: 16,
            palette,
            cells: vec![
                AnsiCell { fg: 0, bg: 1, ch: 0xDB }, // full block, red fg / blue bg → cmd 6
                AnsiCell { fg: 0, bg: 2, ch: 32 },   // space, fg unchanged, green bg → cmd 4
            ],
        };
        let bytes = ansi_grid_to_tundra(&grid);
        // Header is 0x18 "TUNDRA24".
        assert_eq!(&bytes[..9], b"\x18TUNDRA24");
        // cmd 6 (both) for cell 0, then cmd 4 (bg only) for cell 1.
        assert_eq!(
            &bytes[9..],
            &[6, 0xDB, 0x00, 255, 0, 0, 0x00, 0, 0, 255, 4, 32, 0x00, 0, 255, 0]
        );
        let img = TundraDecoder.decode(&bytes).unwrap();
        // Cell 0 is a full block → its top-left pixel is the red fg.
        assert_eq!(img.pixels[0], [255, 0, 0, 255]);
        // Cell 1 is a space (all bg) at column 1 (x=8) → the green bg shows.
        assert_eq!(img.pixels[8], [0, 255, 0, 255]);
    }
}
