//! XBin (.xb/.xbin) — "eXtended BIN": a binary text-mode art format with an
//! optional embedded palette, an optional embedded font (256 or 512 glyphs), and
//! optional RLE compression. Spec by Tasmaniac / ACiD Productions (1996).
//!
//! Layout: `"XBIN" 0x1A`, then `width:u16le height:u16le fontsize:u8 flags:u8`,
//! then (if flagged) a 48-byte palette and a `fontsize*chars` font, then the image
//! as char/attribute pairs (RLE-compressed when the Compress flag is set).

use super::cp437_font::CP437_8X16;
use super::{DecodeError, Decoder};
use crate::image_types::PixImage;

pub struct XBinDecoder;

const FONT_W: usize = 8;
const MAX_CELLS: usize = 250_000; // guard against absurd dimensions

impl Decoder for XBinDecoder {
    fn name(&self) -> &'static str {
        "xbin"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["xb", "xbin"]
    }

    fn sniff(&self, header: &[u8]) -> bool {
        header.starts_with(b"XBIN\x1a")
    }

    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        decode_xbin(bytes)
    }
}

fn malformed(m: &str) -> DecodeError {
    DecodeError::Malformed(m.into())
}

/// Expand a 6-bit VGA DAC value (0..=63) to 8-bit, the way the hardware does
/// (`v<<2 | v>>4`). Shared by the IDF/ADF decoders' palettes.
pub(super) fn vga6to8(v: u8) -> u8 {
    let v = v & 0x3f;
    (v << 2) | (v >> 4)
}

fn decode_xbin(data: &[u8]) -> Result<PixImage, DecodeError> {
    if data.len() < 11 || !data.starts_with(b"XBIN\x1a") {
        return Err(malformed("not an XBIN"));
    }
    let width = u16::from_le_bytes([data[5], data[6]]) as usize;
    let height = u16::from_le_bytes([data[7], data[8]]) as usize;
    let fontsize = data[9] as usize;
    let flags = data[10];
    let has_palette = flags & 0x01 != 0;
    let has_font = flags & 0x02 != 0;
    let compressed = flags & 0x04 != 0;
    let nonblink = flags & 0x08 != 0; // iCE colors (bit 7 of attr = bg intensity)
    let is_512 = flags & 0x10 != 0; // 512-char font (attr bit 3 selects the bank)

    if width == 0 || height == 0 {
        return Err(malformed("empty XBIN"));
    }
    // `fontsize` is the glyph height of an *embedded* font; it's legitimately 0 when
    // there's no embedded font (flag bit 1 clear), in which case the default 8×16 VGA
    // cell is used (see `glyph_h` below). Only reject a zero-height embedded font.
    if has_font && fontsize == 0 {
        return Err(malformed("XBIN embedded font has zero height"));
    }
    let cells = width
        .checked_mul(height)
        .filter(|&c| c <= MAX_CELLS)
        .ok_or_else(|| malformed("XBIN too large"))?;

    let mut pos = 11;
    // Optional 48-byte palette: 16 colors × 3 channels, each 6-bit.
    let palette: [[u8; 3]; 16] = if has_palette {
        if data.len() < pos + 48 {
            return Err(malformed("truncated XBIN palette"));
        }
        let mut p = [[0u8; 3]; 16];
        for (i, c) in p.iter_mut().enumerate() {
            *c = [
                vga6to8(data[pos + i * 3]),
                vga6to8(data[pos + i * 3 + 1]),
                vga6to8(data[pos + i * 3 + 2]),
            ];
        }
        pos += 48;
        p
    } else {
        // No embedded palette → the default. Attribute bytes are VGA-ordered, so use the
        // VGA palette (index 1=blue, 4=red), not the SGR-ordered ansi::PALETTE.
        super::ansi::VGA_PALETTE
    };

    // Optional embedded font: fontsize bytes per glyph, 8px wide.
    let num_chars = if is_512 { 512 } else { 256 };
    let font: Option<&[u8]> = if has_font {
        let flen = fontsize * num_chars;
        if data.len() < pos + flen {
            return Err(malformed("truncated XBIN font"));
        }
        let f = &data[pos..pos + flen];
        pos += flen;
        Some(f)
    } else {
        None
    };

    // Image data: width*height (char, attr) pairs, RLE-compressed when flagged.
    let raw = &data[pos..];
    let pairs = if compressed {
        decompress_rle(raw, cells)
    } else {
        let mut v = Vec::with_capacity(cells);
        for ck in raw.chunks_exact(2).take(cells) {
            v.push((ck[0], ck[1]));
        }
        v.resize(cells, (b' ', 0x07));
        v
    };

    // A custom font sets the cell height; otherwise we use the 8×16 VGA default.
    let glyph_h = if font.is_some() { fontsize } else { 16 };
    Ok(render_textmode(
        width, height, &pairs, &palette, font, glyph_h, nonblink, is_512,
    ))
}

/// Render text-mode cells (char/attribute pairs) to RGBA. Shared by XBin and the
/// raw BIN decoder. `font` is the embedded glyph table, else the CP437 8×16
/// default; `glyph_h` is the cell height. Missing cells render as a blank space.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_textmode(
    width: usize,
    height: usize,
    pairs: &[(u8, u8)],
    palette: &[[u8; 3]; 16],
    font: Option<&[u8]>,
    glyph_h: usize,
    nonblink: bool,
    is_512: bool,
) -> PixImage {
    let w = width * FONT_W;
    let h = height * glyph_h;
    let mut pixels = vec![[0u8, 0, 0, 255]; w * h];
    for cy in 0..height {
        for cx in 0..width {
            let (ch, attr) = pairs.get(cy * width + cx).copied().unwrap_or((b' ', 0x07));
            let (fg_idx, bg_idx, char_idx) = decode_attr(ch, attr, nonblink, is_512);
            let fg = palette[fg_idx & 0x0f];
            let bg = palette[bg_idx & 0x0f];
            for ry in 0..glyph_h {
                let bits = glyph_row(font, char_idx, ry, glyph_h);
                for rx in 0..FONT_W {
                    let on = (bits >> (7 - rx)) & 1 == 1;
                    let c = if on { fg } else { bg };
                    pixels[(cy * glyph_h + ry) * w + (cx * FONT_W + rx)] = [c[0], c[1], c[2], 255];
                }
            }
        }
    }
    PixImage::from_rgba(w as u32, h as u32, pixels)
}

/// Split a VGA attribute byte into `(fg, bg, char_index)`. In iCE/non-blink mode
/// the top attribute bit becomes background intensity (16 bg colors). In 512-char
/// mode attribute bit 3 selects the upper font bank, leaving fg a 3-bit value.
fn decode_attr(ch: u8, attr: u8, nonblink: bool, is_512: bool) -> (usize, usize, usize) {
    let bg = if nonblink {
        ((attr >> 4) & 0x0f) as usize
    } else {
        ((attr >> 4) & 0x07) as usize
    };
    if is_512 {
        let char_idx = ch as usize + if attr & 0x08 != 0 { 256 } else { 0 };
        ((attr & 0x07) as usize, bg, char_idx)
    } else {
        ((attr & 0x0f) as usize, bg, ch as usize)
    }
}

/// One glyph row: from the embedded font, or the CP437 default (clamped to 256).
fn glyph_row(font: Option<&[u8]>, char_idx: usize, row: usize, glyph_h: usize) -> u8 {
    match font {
        Some(f) => f.get(char_idx * glyph_h + row).copied().unwrap_or(0),
        None => CP437_8X16[char_idx & 0xff][row.min(15)],
    }
}

/// XBin RLE: a stream of runs, each led by a counter byte — top 2 bits = type,
/// low 6 bits = `count-1` (1..=64). Type 0 = literal pairs; 1 = one char + N
/// attrs; 2 = one attr + N chars; 3 = one char+attr repeated N times.
fn decompress_rle(raw: &[u8], cells: usize) -> Vec<(u8, u8)> {
    let mut out: Vec<(u8, u8)> = Vec::with_capacity(cells);
    let mut i = 0;
    while out.len() < cells && i < raw.len() {
        let lead = raw[i];
        i += 1;
        let count = (lead & 0x3f) as usize + 1;
        match lead >> 6 {
            0 => {
                for _ in 0..count {
                    let ch = raw.get(i).copied().unwrap_or(b' ');
                    let at = raw.get(i + 1).copied().unwrap_or(0x07);
                    out.push((ch, at));
                    i += 2;
                }
            }
            1 => {
                let ch = raw.get(i).copied().unwrap_or(b' ');
                i += 1;
                for _ in 0..count {
                    out.push((ch, raw.get(i).copied().unwrap_or(0x07)));
                    i += 1;
                }
            }
            2 => {
                let at = raw.get(i).copied().unwrap_or(0x07);
                i += 1;
                for _ in 0..count {
                    out.push((raw.get(i).copied().unwrap_or(b' '), at));
                    i += 1;
                }
            }
            _ => {
                let ch = raw.get(i).copied().unwrap_or(b' ');
                let at = raw.get(i + 1).copied().unwrap_or(0x07);
                i += 2;
                for _ in 0..count {
                    out.push((ch, at));
                }
            }
        }
    }
    out.resize(cells, (b' ', 0x07));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(w: u16, h: u16, fontsize: u8, flags: u8) -> Vec<u8> {
        let mut d = b"XBIN\x1a".to_vec();
        d.extend_from_slice(&w.to_le_bytes());
        d.extend_from_slice(&h.to_le_bytes());
        d.push(fontsize);
        d.push(flags);
        d
    }

    #[test]
    fn sniffs_magic() {
        assert!(XBinDecoder.sniff(b"XBIN\x1arest"));
        assert!(!XBinDecoder.sniff(b"XBINx"));
        assert!(!XBinDecoder.sniff(b"nope"));
    }

    #[test]
    fn decodes_uncompressed_cell() {
        // 1×1, default font/palette, a red full-block. Attribute bytes are VGA-ordered,
        // so red is index 4 (not the SGR order's 1) — see ansi::VGA_PALETTE.
        let mut d = header(1, 1, 16, 0);
        d.push(0xDB); // full block
        d.push(0x04); // attr: fg = red(4), bg = 0
        let img = XBinDecoder.decode(&d).unwrap();
        assert_eq!((img.width, img.height), (8, 16));
        assert_eq!(img.pixels[0], [170, 0, 0, 255]); // red
    }

    #[test]
    fn decodes_rle_char_attr_run() {
        // 2×1, compressed: type-3 run (char+attr) of length 2 → two red blocks.
        let mut d = header(2, 1, 16, 0x04);
        d.push(0b11_000001); // type 3, count = 1+1 = 2
        d.push(0xDB);
        d.push(0x04); // VGA red
        let img = XBinDecoder.decode(&d).unwrap();
        assert_eq!((img.width, img.height), (16, 16));
        assert_eq!(img.pixels[0], [170, 0, 0, 255]);
        assert_eq!(img.pixels[8], [170, 0, 0, 255]); // second cell also red
    }

    #[test]
    fn ice_gives_bright_background() {
        // Non-blink (iCE) flag: a space with bg nibble 0xC → bright background. In VGA
        // attribute order 0xC = bright red (index 12).
        let mut d = header(1, 1, 16, 0x08);
        d.push(b' ');
        d.push(0xC0); // bg = 0xC (bright red) in iCE mode, fg = 0
        let img = XBinDecoder.decode(&d).unwrap();
        assert_eq!(img.pixels[0], [255, 85, 85, 255]); // bright red
    }

    #[test]
    fn fontsize_zero_without_embedded_font_decodes() {
        // fontsize 0 + no embedded-font flag is legal (use the default 8×16 cell);
        // it used to be rejected as "empty XBIN". Regression for 6730302020_NFO.xb.
        let mut d = header(2, 1, 0, 0); // w=2, h=1, fontsize=0, flags=0
        d.extend_from_slice(&[0xDB, 0x04, 0xDB, 0x04]); // two red blocks
        let img = XBinDecoder.decode(&d).unwrap();
        assert_eq!((img.width, img.height), (16, 16)); // 2×8px wide, default 16px tall
        assert_eq!(img.pixels[0], [170, 0, 0, 255]); // red (VGA index 4)
    }

    #[test]
    fn custom_palette_is_used() {
        // Palette flag: set color 1 to full white (6-bit 63,63,63), draw fg=1 block.
        let mut d = header(1, 1, 16, 0x01);
        let mut pal = vec![0u8; 48];
        pal[3] = 63;
        pal[4] = 63;
        pal[5] = 63; // color index 1 = white
        d.extend_from_slice(&pal);
        d.push(0xDB);
        d.push(0x01);
        let img = XBinDecoder.decode(&d).unwrap();
        assert_eq!(img.pixels[0], [255, 255, 255, 255]);
    }

    #[test]
    fn xbin_encoder_embeds_used_colours_not_first_16() {
        // A 32-colour palette whose first 16 are cold blues/greys and whose WARM red
        // lives at index 20. A cell drawn in that red must decode back to pure red —
        // proving the encoder embeds the USED colours, not palette[..16] (which has no
        // red, so a first-16 mapping would pick a cold colour instead).
        use crate::thumb::{ansi_grid_to_xbin, AnsiCell, AnsiGrid};
        let mut palette: Vec<[u8; 4]> = (0..32)
            .map(|i| [0, 0, (60 + i * 4) as u8, 255]) // cold blues, no red anywhere else
            .collect();
        palette[20] = [255, 0, 0, 255]; // warm red at index 20
        let grid = AnsiGrid {
            cols: 1,
            rows: 1,
            cell_w: 8,
            cell_h: 16,
            palette,
            cells: vec![AnsiCell { fg: 20, bg: 0, ch: 0xDB }], // full block in red
        };
        let (bytes, reduced) = ansi_grid_to_xbin(&grid, false, false);
        assert!(!reduced, "only 2 distinct colours used → no reduction");
        let img = XBinDecoder.decode(&bytes).unwrap();
        assert_eq!(img.pixels[0], [255, 0, 0, 255], "best-16, not first-16");
    }

    #[test]
    fn xbin_9x16_omits_font_vga50_embeds_it() {
        use crate::thumb::{ansi_grid_to_xbin, AnsiCell, AnsiGrid};
        let mk = |font_8x8: bool| AnsiGrid {
            cols: 1,
            rows: 1,
            cell_w: if font_8x8 { 8 } else { 9 },
            cell_h: if font_8x8 { 8 } else { 16 },
            palette: vec![[255, 255, 255, 255], [0, 0, 0, 255]],
            cells: vec![AnsiCell { fg: 0, bg: 1, ch: 0xDB }],
        };
        // 9×16 path: no embedded font → fontsize 0, flag bit1 clear. Still decodes
        // (falls back to the default 8×16 VGA font).
        let (b916, _) = ansi_grid_to_xbin(&mk(false), false, false);
        assert_eq!(b916[9], 0, "9×16 fontsize byte = 0");
        assert_eq!(b916[10] & 0x02, 0, "9×16 font flag clear");
        assert_eq!(b916[10] & 0x01, 0x01, "palette flag still set");
        assert!(XBinDecoder.decode(&b916).is_ok());
        // VGA50 path: embeds the 8×8 font → fontsize 8, flag bit1 set.
        let (bv50, _) = ansi_grid_to_xbin(&mk(true), true, false);
        assert_eq!(bv50[9], 8, "VGA50 fontsize byte = 8");
        assert_eq!(bv50[10] & 0x02, 0x02, "VGA50 font flag set");
        assert!(XBinDecoder.decode(&bv50).is_ok());
    }

    #[test]
    fn xbin_encoder_reduces_when_over_16_used() {
        // >16 distinct colours used → median-cut to 16, `reduced` flag set, still decodes.
        use crate::thumb::{ansi_grid_to_xbin, AnsiCell, AnsiGrid};
        let palette: Vec<[u8; 4]> = (0..40).map(|i| [(i * 6) as u8, 0, 0, 255]).collect();
        let cells: Vec<AnsiCell> = (0..20)
            .map(|i| AnsiCell { fg: i as u8, bg: (i + 1) as u8, ch: 0xDB })
            .collect();
        let grid = AnsiGrid {
            cols: 20,
            rows: 1,
            cell_w: 8,
            cell_h: 16,
            palette,
            cells,
        };
        let (bytes, reduced) = ansi_grid_to_xbin(&grid, false, false);
        assert!(reduced, ">16 distinct colours used → reduced");
        assert!(XBinDecoder.decode(&bytes).is_ok());
    }
}
