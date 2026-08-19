//! REXPaint `.xp` — GridSage Games' ASCII/CP437 art format (see the REXPaint manual's
//! "Appendix B: .xp Format Specification"). An `.xp` file is a **gzip** stream; once
//! inflated the layout is little-endian binary:
//!
//! ```text
//! version   (i32)         # negative for R9+; informational, ignored
//! layers    (i32)         # 1..9
//! per layer:
//!   width   (i32)
//!   height  (i32)
//!   width*height cells in COLUMN-MAJOR order (x outer, y inner):
//!     glyph (u32)         # CP437 code
//!     fg r,g,b (u8 each)
//!     bg r,g,b (u8 each)
//! ```
//!
//! A cell whose background is `255,0,255` (hot pink) is **transparent**; higher layers
//! (later in the file) draw over lower ones, and a visible transparent cell on the base
//! layer renders black. We flatten the layers top-down and render the CP437 glyphs with
//! their 24-bit fg/bg — the same pixel model as TundraDraw (`tundra.rs`).

use super::cp437_font::CP437_8X16;
use super::{DecodeError, Decoder};
use crate::image_types::PixImage;
use std::io::Read;

pub struct RexPaintDecoder;

/// REXPaint's transparent-cell background marker.
pub(crate) const XP_TRANSPARENT: [u8; 3] = [255, 0, 255];
const MAX_DIM: usize = 4096;

/// One flattened cell: CP437 glyph + fg/bg RGB (bg may still be the transparent marker
/// on the base layer, which renders black).
#[derive(Clone, Copy)]
pub(crate) struct XpCell {
    pub glyph: u8,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
}

/// Inflate + flatten an `.xp` into a `width`×`height` row-major cell grid (top layer wins).
pub(crate) fn parse_xp(bytes: &[u8]) -> Result<(usize, usize, Vec<XpCell>), DecodeError> {
    let mut gz = flate2::read::GzDecoder::new(bytes);
    let mut raw = Vec::new();
    gz.read_to_end(&mut raw)
        .map_err(|_| DecodeError::Malformed("not a gzip (.xp) stream".into()))?;
    if raw.len() < 8 {
        return Err(DecodeError::Malformed(".xp truncated".into()));
    }
    let rd = |o: usize| -> i32 { i32::from_le_bytes([raw[o], raw[o + 1], raw[o + 2], raw[o + 3]]) };
    let _version = rd(0);
    let layers = rd(4);
    if !(1..=9).contains(&layers) {
        return Err(DecodeError::Malformed(".xp layer count out of range".into()));
    }
    let mut off = 8usize;
    let (mut gw, mut gh) = (0usize, 0usize);
    let mut comp: Vec<XpCell> = Vec::new();
    for li in 0..layers {
        if off + 8 > raw.len() {
            return Err(DecodeError::Malformed(".xp truncated (layer header)".into()));
        }
        let w = rd(off).max(0) as usize;
        let h = rd(off + 4).max(0) as usize;
        off += 8;
        if w == 0 || h == 0 || w > MAX_DIM || h > MAX_DIM {
            return Err(DecodeError::Malformed(".xp layer dimensions".into()));
        }
        if off + w * h * 10 > raw.len() {
            return Err(DecodeError::Malformed(".xp truncated (cell data)".into()));
        }
        if li == 0 {
            (gw, gh) = (w, h);
            // Base fill: transparent → rendered black (per the spec).
            comp = vec![
                XpCell { glyph: 32, fg: [0, 0, 0], bg: XP_TRANSPARENT };
                w * h
            ];
        }
        // Cells are column-major: the sequential index maps to (x = i/h, y = i%h).
        for x in 0..w {
            for y in 0..h {
                let g = u32::from_le_bytes([raw[off], raw[off + 1], raw[off + 2], raw[off + 3]]);
                let fg = [raw[off + 4], raw[off + 5], raw[off + 6]];
                let bg = [raw[off + 7], raw[off + 8], raw[off + 9]];
                off += 10;
                // A transparent cell lets the layer below show through (skip it). This also
                // keeps a mismatched later layer from painting outside the base dimensions.
                if bg != XP_TRANSPARENT && x < gw && y < gh {
                    comp[y * gw + x] = XpCell { glyph: (g & 0xff) as u8, fg, bg };
                }
            }
        }
    }
    if gw == 0 || gh == 0 {
        return Err(DecodeError::Malformed(".xp has no base layer".into()));
    }
    Ok((gw, gh, comp))
}

impl Decoder for RexPaintDecoder {
    fn name(&self) -> &'static str {
        "rexpaint"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["xp"]
    }

    fn sniff(&self, header: &[u8]) -> bool {
        // gzip magic. `.xp` is always gzipped; a gzip that isn't a valid .xp fails `decode`
        // and the registry falls through to the next decoder.
        header.len() >= 2 && header[0] == 0x1f && header[1] == 0x8b
    }

    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        let (w, h, cells) = parse_xp(bytes)?;
        let (pw, ph) = (w * 8, h * 16);
        let mut pixels = vec![[0u8, 0, 0, 255]; pw * ph];
        for cy in 0..h {
            for cx in 0..w {
                let cell = cells[cy * w + cx];
                let bg = if cell.bg == XP_TRANSPARENT { [0, 0, 0] } else { cell.bg };
                let glyph = &CP437_8X16[cell.glyph as usize];
                for (ry, &bits) in glyph.iter().enumerate() {
                    for rx in 0..8 {
                        let on = (bits >> (7 - rx)) & 1 == 1;
                        let c = if on { cell.fg } else { bg };
                        pixels[(cy * 16 + ry) * pw + (cx * 8 + rx)] = [c[0], c[1], c[2], 255];
                    }
                }
            }
        }
        Ok(PixImage::from_rgba(pw as u32, ph as u32, pixels))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a minimal single-layer `.xp`: `w`×`h` cells (column-major), gzipped.
    fn make_xp(w: usize, h: usize, cells: &[(u8, [u8; 3], [u8; 3])]) -> Vec<u8> {
        let mut raw = Vec::new();
        raw.extend_from_slice(&(-1i32).to_le_bytes()); // version
        raw.extend_from_slice(&1i32.to_le_bytes()); // 1 layer
        raw.extend_from_slice(&(w as i32).to_le_bytes());
        raw.extend_from_slice(&(h as i32).to_le_bytes());
        for &(g, fg, bg) in cells {
            raw.extend_from_slice(&(g as u32).to_le_bytes());
            raw.extend_from_slice(&fg);
            raw.extend_from_slice(&bg);
        }
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&raw).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn sniffs_gzip_magic() {
        assert!(RexPaintDecoder.sniff(&[0x1f, 0x8b, 0x08, 0x00]));
        assert!(!RexPaintDecoder.sniff(b"not gzip"));
    }

    #[test]
    fn decodes_a_truecolor_cell() {
        // 1×1: full block (0xDB) red on blue.
        let xp = make_xp(1, 1, &[(0xDB, [255, 0, 0], [0, 0, 255])]);
        let img = RexPaintDecoder.decode(&xp).unwrap();
        assert_eq!((img.width, img.height), (8, 16));
        assert_eq!(img.pixels[0], [255, 0, 0, 255], "full block → fg red");
    }

    #[test]
    fn transparent_base_cell_renders_black() {
        // A space with the transparent bg marker → black on the base layer.
        let xp = make_xp(1, 1, &[(32, [10, 20, 30], XP_TRANSPARENT)]);
        let img = RexPaintDecoder.decode(&xp).unwrap();
        assert_eq!(img.pixels[0], [0, 0, 0, 255]);
    }

    #[test]
    fn column_major_order_is_respected() {
        // 2 wide × 1 tall: cell (0,0)=red block, (1,0)=green block. Column-major, so the
        // sequential cells are (x=0,y=0) then (x=1,y=0).
        let xp = make_xp(
            2,
            1,
            &[
                (0xDB, [255, 0, 0], [0, 0, 0]),
                (0xDB, [0, 255, 0], [0, 0, 0]),
            ],
        );
        let img = RexPaintDecoder.decode(&xp).unwrap();
        assert_eq!((img.width, img.height), (16, 16));
        assert_eq!(img.pixels[0], [255, 0, 0, 255], "left cell red");
        assert_eq!(img.pixels[8], [0, 255, 0, 255], "right cell green");
    }
}
