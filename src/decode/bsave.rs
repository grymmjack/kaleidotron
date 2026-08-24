//! BSAVE (`.bsv`) — the raw screen dump written by GW-BASIC / QuickBASIC's `BSAVE`, and a very
//! common way DOS art was stored. The file is a **7-byte header** (`0xFD`, then segment / offset /
//! length as little-endian `u16`s) followed by a straight copy of video RAM. The catch: the header
//! records *where* in memory it came from, but **not the video mode or the palette** — those lived
//! in the program that did the `BSAVE`. So the mode is inferred from the data length, and the
//! palette is the hardware default for that mode:
//!
//! - **16000 / 16384 bytes → CGA 320×200, 4 colours.** Two-bits-per-pixel, scanlines **interleaved**
//!   (even rows in the first bank, odd rows at offset `0x2000`). The palette is the fixed CGA
//!   hardware palette, so this is colour-accurate.
//! - **64000 bytes → VGA mode 13h, 320×200×256.** One byte per pixel, linear. There is no stored
//!   palette, so we use the standard VGA DAC default (the 16 EGA colours + a grey ramp + an HSV
//!   colour cube). A file whose program set a *custom* palette can't be colour-accurate — that
//!   information simply isn't in a `.bsv` — but the image still displays.
//!
//! Palette-preserving (`from_indexed`) like PCX/IFF, so swatches / `.GPL` export / recolor work.

use super::{DecodeError, Decoder};
use crate::image_types::PixImage;

pub struct BsaveDecoder;

impl Decoder for BsaveDecoder {
    fn name(&self) -> &'static str {
        "bsave"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["bsv", "bsave"]
    }
    fn sniff(&self, header: &[u8]) -> bool {
        // The BSAVE flag byte is 0xFD, and the recorded length must match the bytes that follow it
        // (the whole point of the header) — enough to tell it apart from other 0xFD-leading blobs.
        if header.len() < 7 || header[0] != 0xFD {
            return false;
        }
        let len = u16::from_le_bytes([header[5], header[6]]) as usize;
        // We only sniff-accept the two lengths we can actually render; other lengths fall through to
        // extension dispatch (still tried, just not claimed on content alone).
        matches!(len, 16000 | 16384 | 64000)
    }
    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        decode(bytes)
    }
}

pub fn decode(bytes: &[u8]) -> Result<PixImage, DecodeError> {
    let bad = |m: &str| DecodeError::Malformed(m.to_string());
    if bytes.len() < 7 || bytes[0] != 0xFD {
        return Err(bad("not a BSAVE file (no 0xFD header)"));
    }
    let len = u16::from_le_bytes([bytes[5], bytes[6]]) as usize;
    let data = &bytes[7..];
    match len {
        64000 => decode_mode13(data),
        16000 => decode_cga(data, 8000),  // odd bank packed right after even (no gap)
        16384 => decode_cga(data, 8192),  // odd bank at the hardware 0x2000 offset
        other => Err(bad(&format!("unsupported BSAVE length {other} (not CGA or mode 13h)"))),
    }
}

/// Mode 13h: 320×200, one byte per pixel = a palette index, linear. Uses the VGA default palette.
fn decode_mode13(data: &[u8]) -> Result<PixImage, DecodeError> {
    const W: usize = 320;
    const H: usize = 200;
    let indices: Vec<u8> = (0..W * H).map(|i| data.get(i).copied().unwrap_or(0)).collect();
    Ok(PixImage::from_indexed(W as u32, H as u32, indices, vga_default_palette()))
}

/// CGA 320×200, 2 bits per pixel, 4 colours, scanlines interleaved into two banks. `odd_bank` is the
/// byte offset where the odd scanlines start (8192 for a real memory-image dump, 8000 for a packed one).
fn decode_cga(data: &[u8], odd_bank: usize) -> Result<PixImage, DecodeError> {
    const W: usize = 320;
    const H: usize = 200;
    const ROW_BYTES: usize = W / 4; // 4 pixels per byte
    // CGA mode-4 palette 1 (high intensity): the ubiquitous cyan / magenta / white set. The real
    // background (index 0) colour was program-set; black is the common choice.
    let palette = vec![
        [0x00, 0x00, 0x00, 255],
        [0x55, 0xFF, 0xFF, 255],
        [0xFF, 0x55, 0xFF, 255],
        [0xFF, 0xFF, 0xFF, 255],
    ];
    let mut indices = vec![0u8; W * H];
    for y in 0..H {
        // Even rows come from the first bank, odd rows from the second.
        let bank_off = if y & 1 == 0 { 0 } else { odd_bank };
        let row_off = bank_off + (y / 2) * ROW_BYTES;
        for x in 0..W {
            let byte = data.get(row_off + x / 4).copied().unwrap_or(0);
            // Two bits per pixel, most-significant pair first.
            let shift = 6 - (x & 3) * 2;
            indices[y * W + x] = (byte >> shift) & 0b11;
        }
    }
    Ok(PixImage::from_indexed(W as u32, H as u32, indices, palette))
}

/// The standard IBM VGA 256-colour DAC default palette (what mode 13h shows until a program loads
/// its own). 0–15 are the EGA colours, 16–31 a 16-step grey ramp, 32–247 a 3×24×3 (value × hue ×
/// saturation) HSV colour cube, 248–255 black — the well-known layout. 6-bit DAC values scaled to 8.
fn vga_default_palette() -> Vec<[u8; 4]> {
    let s = |v: u8| ((v as u16 * 255 + 31) / 63) as u8; // 6-bit (0..63) → 8-bit
    let rgb = |r: u8, g: u8, b: u8| [s(r), s(g), s(b), 255u8];
    let mut p: Vec<[u8; 4]> = Vec::with_capacity(256);

    // 0..16: the 16 EGA/CGA colours.
    const EGA: [(u8, u8, u8); 16] = [
        (0, 0, 0), (0, 0, 42), (0, 42, 0), (0, 42, 42),
        (42, 0, 0), (42, 0, 42), (42, 21, 0), (42, 42, 42),
        (21, 21, 21), (21, 21, 63), (21, 63, 21), (21, 63, 63),
        (63, 21, 21), (63, 21, 63), (63, 63, 21), (63, 63, 63),
    ];
    for &(r, g, b) in &EGA {
        p.push(rgb(r, g, b));
    }

    // 16..32: grey ramp black → white.
    for i in 0..16u8 {
        let v = (i as u16 * 63 / 15) as u8;
        p.push(rgb(v, v, v));
    }

    // 32..248: 216-colour cube, value-major then hue then saturation (the VGA ordering).
    let hsv = |h: f32, sat: f32, val: f32| -> (u8, u8, u8) {
        let c = val * sat;
        let hp = h / 60.0;
        let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
        let (r1, g1, b1) = match hp as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let m = val - c;
        (((r1 + m) * 63.0) as u8, ((g1 + m) * 63.0) as u8, ((b1 + m) * 63.0) as u8)
    };
    for &val in &[63.0f32, 42.0, 21.0] {
        for hue in 0..24u32 {
            for &sat in &[63.0f32, 42.0, 21.0] {
                let (r, g, b) = hsv(hue as f32 * 15.0, sat / 63.0, val / 63.0);
                p.push(rgb(r, g, b));
            }
        }
    }

    // 248..256: black tail.
    while p.len() < 256 {
        p.push(rgb(0, 0, 0));
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(len: u16) -> Vec<u8> {
        let mut v = vec![0xFD, 0x00, 0xA0, 0x00, 0x00]; // flag + seg + offset
        v.extend_from_slice(&len.to_le_bytes());
        v
    }

    #[test]
    fn sniffs_only_renderable_lengths() {
        let d = BsaveDecoder;
        let mut m13 = header(64000);
        m13.resize(7 + 64000, 0);
        assert!(d.sniff(&m13), "mode 13h length must sniff");
        let mut junk = header(1234);
        junk.resize(7 + 1234, 0);
        assert!(!d.sniff(&junk), "an arbitrary length is not claimed on content");
        assert!(!d.sniff(&[0xFD, 0, 0])); // too short
    }

    #[test]
    fn decodes_mode13_indices_and_keeps_a_256_palette() {
        let mut f = header(64000);
        f.resize(7 + 64000, 0);
        f[7] = 4; // top-left pixel → palette index 4 (EGA red)
        f[7 + 319] = 15; // end of row 0 → white
        let img = decode(&f).expect("mode 13h decodes");
        assert_eq!((img.width, img.height), (320, 200));
        let idx = img.indexed.as_ref().expect("palette-preserving");
        assert_eq!(idx.palette.len(), 256);
        assert_eq!(idx.indices[0], 4);
        assert_eq!(idx.indices[319], 15);
        // Index 15 (white) is full-white in the VGA default.
        assert_eq!(idx.palette[15], [255, 255, 255, 255]);
    }

    #[test]
    fn decodes_cga_interleaved_banks() {
        // 16384-byte dump: even rows at 0, odd rows at 0x2000. Set row 0 col 0 and row 1 col 0.
        let mut f = header(16384);
        f.resize(7 + 16384, 0);
        f[7] = 0b11_00_00_00; // row 0 (even bank, offset 0): first pixel = index 3 (white)
        f[7 + 8192] = 0b10_00_00_00; // row 1 (odd bank, offset 0x2000): first pixel = index 2 (magenta)
        let img = decode(&f).expect("cga decodes");
        assert_eq!((img.width, img.height), (320, 200));
        let idx = img.indexed.as_ref().unwrap();
        assert_eq!(idx.indices[0], 3, "row 0 col 0 from the even bank");
        assert_eq!(idx.indices[320], 2, "row 1 col 0 from the odd bank");
        assert_eq!(idx.palette.len(), 4);
    }
}
