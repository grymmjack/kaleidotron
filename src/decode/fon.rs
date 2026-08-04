//! Windows bitmap fonts — `.fon` (a 16-bit **NE executable** wrapping one or more **FNT** font
//! resources) and bare `.fnt`. No Rust crate reads these (the crates.io `*fnt*` crates are all
//! BMFont game atlases or Infinity-Engine's format — a different thing), so this is a hand-rolled
//! parser in the project's pcx/cp437 ethos: walk MZ → NE → the resource table → each `RT_FONT`
//! resource, parse the FNT header + glyph table, and decode the **column-major** 1bpp bitmaps.
//! Verified against the user's real corpus (System/MS-Serif/Fixedsys/… all render).
//!
//! A `.fon` typically holds the same face at several point sizes; we expose them all. The grid
//! tile renders the shared preview sample (see `font::thumb_sample`) in the largest face; opening
//! one enters a viewer with a size picker + type-to-sample + glyph grid (see `draw_fon_ui`).

use super::{DecodeError, Decoder};
use crate::image_types::PixImage;

/// Extensions handled here (`.fnt` = a bare FNT with no NE wrapper).
pub const FON_EXTS: &[&str] = &["fon", "fnt"];

/// A decoded glyph: a `w×h` 1bpp bitmap flattened row-major (`true` = ink).
#[derive(Clone)]
pub struct FonGlyph {
    pub ch: char,
    pub w: usize,
    pub bits: Vec<bool>, // len = w * height
}

/// One embedded raster face (a single FNT resource / point size).
#[derive(Clone)]
pub struct FonFace {
    pub name: String,
    pub points: u16,
    pub height: usize, // pixel cell height
    pub ascent: usize,
    pub glyphs: Vec<FonGlyph>, // dfFirstChar..=dfLastChar (CP437/ANSI byte → char)
}

impl FonFace {
    /// Look up a glyph by char (linear — faces are small).
    fn glyph(&self, ch: char) -> Option<&FonGlyph> {
        self.glyphs.iter().find(|g| g.ch == ch)
    }
}

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// A raster (not vector) FNT byte maps to this CP437/Latin-1 codepoint. Windows FNTs are
/// single-byte (ANSI/OEM); we map bytes through CP437 so the box-drawing + block glyphs show.
fn byte_to_char(byte: u8) -> char {
    // ASCII stays itself; high bytes → CP437 (the DOS-heritage code page these fonts target),
    // via retrofont's table (already a dependency).
    if byte < 0x80 {
        byte as char
    } else {
        retrofont::tdf::CP437_TO_UNICODE[byte as usize]
    }
}

/// Parse a single FNT structure at `base` in `b`. Returns the face, or `None` (vector font,
/// truncated, or unsupported version).
fn parse_fnt(b: &[u8], base: usize) -> Option<FonFace> {
    if base + 0x76 > b.len() {
        return None;
    }
    let ver = le16(b, base);
    if !matches!(ver, 0x0100 | 0x0200 | 0x0300) {
        return None;
    }
    let df_type = le16(b, base + 0x42);
    if df_type & 1 != 0 {
        return None; // bit 0 set = vector font (stroke glyphs, not bitmaps) — unsupported
    }
    let points = le16(b, base + 0x44);
    let ascent = le16(b, base + 0x4A) as usize;
    let pix_height = le16(b, base + 0x58) as usize;
    let first = b[base + 0x5F];
    let last = b[base + 0x60];
    if pix_height == 0 || pix_height > 256 || last < first {
        return None;
    }
    // Char table: v2 is 4-byte entries at 0x76; v3 is 6-byte entries at 0x94.
    let (gtab, entry) = if ver == 0x0300 { (0x94usize, 6usize) } else { (0x76usize, 4usize) };
    let n = (last as usize - first as usize) + 1; // real chars (ignore the trailing sentinel)

    let mut glyphs = Vec::with_capacity(n);
    for i in 0..n {
        let eo = base + gtab + i * entry;
        if eo + entry > b.len() {
            break;
        }
        let gw = le16(b, eo) as usize;
        let goff = if entry == 6 { le32(b, eo + 4) as usize } else { le16(b, eo + 2) as usize };
        if gw == 0 || gw > 256 {
            continue;
        }
        let strips = gw.div_ceil(8);
        let need = base + goff + strips * pix_height;
        if goff == 0 || need > b.len() {
            continue;
        }
        // Column-major: byte = data[goff + strip*height + row]; bit 7-(col%8) is the leftmost.
        let mut bits = vec![false; gw * pix_height];
        for row in 0..pix_height {
            for col in 0..gw {
                let byte = b[base + goff + (col / 8) * pix_height + row];
                if (byte >> (7 - (col % 8))) & 1 == 1 {
                    bits[row * gw + col] = true;
                }
            }
        }
        glyphs.push(FonGlyph { ch: byte_to_char(first + i as u8), w: gw, bits });
    }
    if glyphs.is_empty() {
        return None;
    }
    // Face name: the null-terminated string at dfFace (dword @ 0x69), relative to the FNT base.
    let name = {
        let face_off = le32(b, base + 0x69) as usize;
        let start = base + face_off;
        if face_off != 0 && start < b.len() {
            let end = b[start..].iter().position(|&c| c == 0).map(|p| start + p).unwrap_or(b.len());
            String::from_utf8_lossy(&b[start..end]).trim().to_string()
        } else {
            String::new()
        }
    };
    Some(FonFace { name, points, height: pix_height, ascent, glyphs })
}

/// Parse every raster face in a `.fon` (NE) or bare `.fnt`. Empty if none decode.
pub fn parse_faces(b: &[u8]) -> Vec<FonFace> {
    // Bare FNT (no MZ header): the whole file is one FNT.
    if b.len() >= 2 && matches!(le16(b, 0), 0x0100 | 0x0200 | 0x0300) {
        return parse_fnt(b, 0).into_iter().collect();
    }
    // NE .fon: MZ → e_lfanew → NE → resource table → RT_FONT (type 0x8008) resources.
    if b.len() < 0x40 || &b[0..2] != b"MZ" {
        return Vec::new();
    }
    let ne = le32(b, 0x3C) as usize;
    if ne + 0x26 > b.len() || &b[ne..ne + 2] != b"NE" {
        return Vec::new();
    }
    let rsrc = ne + le16(b, ne + 0x24) as usize;
    if rsrc + 2 > b.len() {
        return Vec::new();
    }
    let shift = le16(b, rsrc) as u32;
    let mut p = rsrc + 2;
    let mut faces = Vec::new();
    // TYPEINFO records until a 0 type id.
    while p + 8 <= b.len() {
        let rtype = le16(b, p);
        if rtype == 0 {
            break;
        }
        let count = le16(b, p + 2) as usize;
        p += 8; // skip rtype, count, reserved(4)
        for _ in 0..count {
            if p + 12 > b.len() {
                return faces;
            }
            if rtype == 0x8008 {
                // RT_FONT
                let off = (le16(b, p) as usize) << shift;
                if let Some(f) = parse_fnt(b, off) {
                    faces.push(f);
                }
            }
            p += 12; // NAMEINFO
        }
    }
    faces
}

/// `(name, points, height)` per face for the viewer picker.
pub fn face_list(b: &[u8]) -> Vec<(String, u16, usize)> {
    parse_faces(b)
        .into_iter()
        .map(|f| (f.name, f.points, f.height))
        .collect()
}

/// Render `text` in face `idx` → a `PixImage` (white ink on transparent). 1px inter-glyph gap.
pub fn render_text(b: &[u8], idx: usize, text: &str, ink: [u8; 3]) -> Option<PixImage> {
    let faces = parse_faces(b);
    let face = faces.get(idx)?;
    render_face_text(face, text, ink)
}

fn render_face_text(face: &FonFace, text: &str, ink: [u8; 3]) -> Option<PixImage> {
    let gap = 1usize;
    let h = face.height;
    // Split on newlines; total size = widest line × (lines · height).
    let lines: Vec<&str> = text.split('\n').collect();
    let mut line_w: Vec<usize> = Vec::new();
    for line in &lines {
        let mut w = 0usize;
        for ch in line.chars() {
            let gw = face.glyph(ch).map(|g| g.w).unwrap_or(h / 3);
            w += gw + gap;
        }
        line_w.push(w.max(1));
    }
    let total_w = line_w.iter().copied().max().unwrap_or(1).max(1);
    let total_h = (lines.len() * (h + gap)).max(1);
    let mut px = vec![[0u8, 0, 0, 0]; total_w * total_h];
    for (li, line) in lines.iter().enumerate() {
        let mut x = 0usize;
        let y0 = li * (h + gap);
        for ch in line.chars() {
            if let Some(g) = face.glyph(ch) {
                for row in 0..h {
                    for col in 0..g.w {
                        if g.bits[row * g.w + col] {
                            let (gx, gy) = (x + col, y0 + row);
                            if gx < total_w && gy < total_h {
                                px[gy * total_w + gx] = [ink[0], ink[1], ink[2], 255];
                            }
                        }
                    }
                }
                x += g.w + gap;
            } else {
                x += h / 3 + gap; // space / missing glyph
            }
        }
    }
    Some(PixImage::from_rgba(total_w as u32, total_h as u32, px))
}

/// Render face `idx`'s glyphs into a paged grid (each cell `cell×cell`, `cols` wide) → the image +
/// row count + the chars drawn (for click-to-copy). Mirrors `font::render_glyph_grid`.
pub fn render_glyph_grid(
    b: &[u8],
    idx: usize,
    start: usize,
    max_cells: usize,
    cols: usize,
    cell: usize,
    ink: [u8; 3],
) -> Option<(PixImage, usize, Vec<char>)> {
    let faces = parse_faces(b);
    let face = faces.get(idx)?;
    let chars: Vec<char> = face.glyphs.iter().skip(start).take(max_cells).map(|g| g.ch).collect();
    if chars.is_empty() {
        return None;
    }
    let rows = chars.len().div_ceil(cols);
    let (w, h) = (cols * cell, rows * cell);
    let mut px = vec![[0u8, 0, 0, 0]; w * h];
    let scale = ((cell as f32 - 6.0) / face.height as f32).max(0.5);
    for (i, ch) in chars.iter().enumerate() {
        let Some(g) = face.glyph(*ch) else { continue };
        let (cx, cy) = ((i % cols) * cell, (i / cols) * cell);
        let gw = (g.w as f32 * scale) as usize;
        let gh = (face.height as f32 * scale) as usize;
        let ox = cx + (cell.saturating_sub(gw)) / 2;
        let oy = cy + (cell.saturating_sub(gh)) / 2;
        for row in 0..gh {
            for col in 0..gw {
                let sx = (col as f32 / scale) as usize;
                let sy = (row as f32 / scale) as usize;
                if sx < g.w && sy < face.height && g.bits[sy * g.w + sx] {
                    let (x, y) = (ox + col, oy + row);
                    if x < w && y < h {
                        px[y * w + x] = [ink[0], ink[1], ink[2], 255];
                    }
                }
            }
        }
    }
    Some((PixImage::from_rgba(w as u32, h as u32, px), rows, chars))
}

/// Glyph count of face `idx` (for the viewer's paging).
pub fn face_glyph_count(b: &[u8], idx: usize) -> usize {
    parse_faces(b).get(idx).map(|f| f.glyphs.len()).unwrap_or(0)
}

pub struct FonDecoder;

impl Decoder for FonDecoder {
    fn name(&self) -> &'static str {
        "fon"
    }
    fn extensions(&self) -> &'static [&'static str] {
        FON_EXTS
    }
    fn sniff(&self, header: &[u8]) -> bool {
        // A bare FNT: version 0x0100/0200/0300 as the first LE u16 + a plausible header. (.fon
        // NE files are matched by extension — "MZ" alone is far too broad to sniff.)
        header.len() >= 0x62
            && matches!(le16(header, 0), 0x0100 | 0x0200 | 0x0300)
            && le16(header, 0x58) as usize <= 256 // dfPixHeight plausible
    }
    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        let faces = parse_faces(bytes);
        if faces.is_empty() {
            return Err(DecodeError::Unsupported);
        }
        // Tile: the shared preview sample in the LARGEST face (most legible thumbnail).
        let idx = faces
            .iter()
            .enumerate()
            .max_by_key(|(_, f)| f.height)
            .map(|(i, _)| i)
            .unwrap_or(0);
        render_face_text(&faces[idx], &super::font::thumb_sample(), [235, 235, 235])
            .ok_or(DecodeError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_rejects_random() {
        assert!(!FonDecoder.sniff(b"\x89PNG\r\n\x1a\n"));
        assert!(!FonDecoder.sniff(b"MZ")); // too short / NE matched by extension only
    }

    // Renders real .fon faces from the user's corpus (if present) to /tmp for eyeballing.
    #[test]
    #[ignore]
    fn fon_dump() {
        let dir = "/home/grymmjack/FontBase/Fonts/!FROM-K Drive/Sorted/Bitmap";
        for name in ["sseriff0.fon", "8514fix0.fon", "8514oem0.fon"] {
            let p = format!("{dir}/{name}");
            let Ok(bytes) = std::fs::read(&p) else { continue };
            let faces = face_list(&bytes);
            eprintln!("{name}: {} faces {:?}", faces.len(), faces);
            if let Some(img) = render_text(&bytes, faces.len().saturating_sub(1), "Hello 123!", [235, 235, 235]) {
                let out = format!("/tmp/fon_{}.png", name.replace('.', "_"));
                image::save_buffer(&out, &img.rgba_bytes(), img.width, img.height, image::ColorType::Rgba8).unwrap();
                eprintln!("  wrote {out} ({}×{})", img.width, img.height);
            }
        }
    }
}
