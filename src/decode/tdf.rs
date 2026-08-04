//! TheDraw fonts (`.tdf`) — the classic DOS ANSI-art "figlet" fonts (one file holds several
//! named fonts, each spelling text as large CP437 letters). Three sub-types: **outline** (thin
//! box-drawing letters), **block** (solid CP437 letters, one colour) and **colour** (per-cell
//! fg/bg attributes). Parsing is delegated to Mike Krüger's **`retrofont`** crate (same icy
//! ecosystem as `icy_parser_core`), which fully resolves every glyph into a uniform stream of
//! [`GlyphPart`] cells; we rasterise those with pixelview's own CP437 8×16 font + VGA palette —
//! keeping the pixel-perfect zoom + thumbnail quality the other text-mode decoders enjoy.
//!
//! The grid tile shows the first font spelling a short sample; opening a `.tdf` enters a viewer
//! with a font picker + a type-to-sample box (see `app.rs`).

use super::{DecodeError, Decoder};
use crate::image_types::PixImage;
use retrofont::tdf::{TdfFont, TdfFontType};
use retrofont::{transform_outline, GlyphPart};
use std::collections::HashMap;
use std::sync::OnceLock;

/// Extensions handled here.
pub const TDF_EXTS: &[&str] = &["tdf"];

const FONT_W: usize = 8; // CP437 cell width (px)
const FONT_H: usize = 16; // CP437 cell height (px)
/// TheDraw outline fonts default to style 0 (single-line box drawing).
const OUTLINE_STYLE: usize = 0;

/// One CP437 cell in a rasterised glyph grid.
#[derive(Clone, Copy)]
struct Cell {
    ch: u8,  // CP437 byte
    fg: u8,  // palette index 0-15
    bg: u8,  // palette index 0-15
    ink: bool, // a visible glyph pixel (vs. transparent padding) — lets callers keep bg black
}

impl Cell {
    const BLANK: Cell = Cell { ch: b' ', fg: 15, bg: 0, ink: false };
}

/// Reverse of retrofont's `CP437_TO_UNICODE`: Unicode char → CP437 byte (best effort; unknown
/// chars fall back to a filled block so they're at least visible). Built once.
fn unicode_to_cp437(ch: char) -> u8 {
    static MAP: OnceLock<HashMap<char, u8>> = OnceLock::new();
    let map = MAP.get_or_init(|| {
        let mut m = HashMap::with_capacity(256);
        for (byte, &uni) in retrofont::tdf::CP437_TO_UNICODE.iter().enumerate() {
            // First writer wins so ASCII bytes map to themselves (they appear once anyway).
            m.entry(uni).or_insert(byte as u8);
        }
        m
    });
    if let Some(&b) = map.get(&ch) {
        return b;
    }
    // ASCII passes straight through; anything else → a solid block placeholder.
    let u = ch as u32;
    if u < 128 {
        u as u8
    } else {
        0xDB
    }
}

/// Load every font in a `.tdf` file. Returns `(name, type_label, glyph_count)` per font for the
/// viewer's picker. Empty if the file has no parseable fonts.
pub fn font_list(bytes: &[u8]) -> Vec<(String, &'static str, usize)> {
    TdfFont::load(bytes)
        .map(|fonts| {
            fonts
                .iter()
                .map(|f| (f.name.clone(), type_label(f.font_type), f.glyph_count()))
                .collect()
        })
        .unwrap_or_default()
}

fn type_label(t: TdfFontType) -> &'static str {
    match t {
        TdfFontType::Outline => "outline",
        TdfFontType::Block => "block",
        TdfFontType::Color => "color",
    }
}

/// How many fonts are in the file (cheap check for the viewer's picker visibility).
pub fn font_count(bytes: &[u8]) -> usize {
    TdfFont::load(bytes).map(|f| f.len()).unwrap_or(0)
}

/// Rasterise `text` in font `index` of the file → a `PixImage` (black background, VGA colours).
/// Returns `None` if the file/font/text yields nothing drawable.
pub fn render_tdf(bytes: &[u8], index: usize, text: &str) -> Option<PixImage> {
    let fonts = TdfFont::load(bytes).ok()?;
    let font = fonts.get(index)?;
    render_string(font, text)
}

/// A representative sample string for a grid tile: the font's own name (uppercased — many TDF
/// fonts are A–Z only), trimmed to something that fits, falling back to a stock string.
fn sample_text(font: &TdfFont) -> String {
    let name: String = font
        .name
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string();
    // Prefer the name if the font actually has glyphs for its letters; else a stock sample.
    let candidate = if name.is_empty() { "ABCabc".to_string() } else { name };
    let up = candidate.to_uppercase();
    // If the font has none of the sample's letters, fall back to A-Z it likely has.
    if up.chars().any(|c| font.glyph(c).is_some()) {
        up.chars().take(16).collect()
    } else {
        "ABCDEF".to_string()
    }
}

/// Lay out `text` glyph-by-glyph into a CP437 cell grid, then rasterise. Glyphs are top-aligned;
/// `spacing` blank columns separate them (TheDraw's per-font advance). Missing glyphs (e.g. a
/// space, or a char the font lacks) advance by a few blank columns.
fn render_string(font: &TdfFont, text: &str) -> Option<PixImage> {
    let spacing = font.spacing.clamp(0, 40) as usize;

    // Resolve each requested char to a laid-out cell block (rows of Cells) + its width/height.
    struct Block {
        rows: Vec<Vec<Cell>>,
        width: usize,
    }
    let mut blocks: Vec<Block> = Vec::new();
    for ch in text.chars() {
        if ch == ' ' {
            blocks.push(Block { rows: Vec::new(), width: 4 });
            continue;
        }
        let Some(glyph) = font.glyph(ch) else {
            blocks.push(Block { rows: Vec::new(), width: 3 });
            continue;
        };
        let color = font.font_type == TdfFontType::Color;
        let mut rows: Vec<Vec<Cell>> = vec![Vec::new()];
        let push = |rows: &mut Vec<Vec<Cell>>, cell: Cell| rows.last_mut().unwrap().push(cell);
        for part in &glyph.parts {
            match part {
                GlyphPart::NewLine => rows.push(Vec::new()),
                GlyphPart::EndMarker => {}
                GlyphPart::Skip => push(&mut rows, Cell::BLANK),
                GlyphPart::HardBlank | GlyphPart::FillMarker | GlyphPart::OutlineHole => {
                    push(&mut rows, Cell { ch: b' ', ink: false, ..Cell::BLANK })
                }
                GlyphPart::OutlinePlaceholder(b) => {
                    let uc = transform_outline(OUTLINE_STYLE, *b);
                    push(&mut rows, Cell { ch: unicode_to_cp437(uc), fg: 15, bg: 0, ink: true })
                }
                GlyphPart::Char(c) => {
                    push(&mut rows, Cell { ch: unicode_to_cp437(*c), fg: 15, bg: 0, ink: true })
                }
                GlyphPart::AnsiChar { ch, fg, bg, .. } => push(
                    &mut rows,
                    Cell {
                        ch: unicode_to_cp437(*ch),
                        fg: fg & 0x0f,
                        bg: bg & 0x0f,
                        ink: true,
                    },
                ),
            }
        }
        // Trim a trailing empty row (a glyph often ends with a NewLine).
        if rows.last().map(|r| r.is_empty()).unwrap_or(false) {
            rows.pop();
        }
        let width = glyph.width.max(rows.iter().map(|r| r.len()).max().unwrap_or(0));
        let _ = color; // color already applied per-cell above
        blocks.push(Block { rows, width });
    }
    if blocks.is_empty() {
        return None;
    }

    let total_w: usize = blocks.iter().map(|b| b.width).sum::<usize>()
        + spacing * blocks.len().saturating_sub(1)
        + 2; // small right/left margin
    let total_h = blocks
        .iter()
        .map(|b| b.rows.len())
        .max()
        .unwrap_or(1)
        .max(1);
    if total_w == 0 || total_h == 0 {
        return None;
    }

    // Compose the grid.
    let mut grid = vec![Cell::BLANK; total_w * total_h];
    let mut x = 1usize; // left margin
    for b in &blocks {
        for (ry, row) in b.rows.iter().enumerate() {
            for (cx, cell) in row.iter().enumerate() {
                if cell.ink || cell.bg != 0 {
                    let gx = x + cx;
                    if gx < total_w && ry < total_h {
                        grid[ry * total_w + gx] = *cell;
                    }
                }
            }
        }
        x += b.width + spacing;
    }

    Some(rasterize(&grid, total_w, total_h))
}

/// Blit a CP437 `(ch, fg, bg)` grid to RGBA using the embedded 8×16 VGA font + VGA palette.
fn rasterize(grid: &[Cell], cols: usize, rows: usize) -> PixImage {
    use super::ansi::VGA_PALETTE;
    use super::cp437_font::CP437_8X16;
    let w = cols * FONT_W;
    let h = rows * FONT_H;
    let mut pixels = vec![[0u8, 0, 0, 255]; w * h];
    for cy in 0..rows {
        for cx in 0..cols {
            let cell = grid[cy * cols + cx];
            let fg = VGA_PALETTE[cell.fg as usize & 0x0f];
            let bg = VGA_PALETTE[cell.bg as usize & 0x0f];
            let glyph = &CP437_8X16[cell.ch as usize];
            for ry in 0..FONT_H {
                let bits = glyph[ry];
                for rx in 0..FONT_W {
                    let on = (bits >> (7 - rx)) & 1 == 1;
                    let c = if on { fg } else { bg };
                    pixels[(cy * FONT_H + ry) * w + (cx * FONT_W + rx)] = [c[0], c[1], c[2], 255];
                }
            }
        }
    }
    PixImage::from_rgba(w as u32, h as u32, pixels)
}

pub struct TdfDecoder;

impl Decoder for TdfDecoder {
    fn name(&self) -> &'static str {
        "tdf"
    }
    fn extensions(&self) -> &'static [&'static str] {
        TDF_EXTS
    }
    fn sniff(&self, header: &[u8]) -> bool {
        // 0x13 + "TheDraw FONTS file".
        header.len() >= 20 && header[0] == 0x13 && &header[1..19] == b"TheDraw FONTS file"
    }
    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        let fonts = TdfFont::load(bytes).map_err(|e| DecodeError::Malformed(e.to_string()))?;
        let font = fonts.first().ok_or(DecodeError::Unsupported)?;
        render_string(font, &sample_text(font)).ok_or(DecodeError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A minimal valid header so the sniff test is real; full-font decoding is exercised against
    // the user's real .tdf corpus in a `#[ignore]` dev test below.
    #[test]
    fn sniffs_tdf_header() {
        let mut h = vec![0x13];
        h.extend_from_slice(b"TheDraw FONTS file");
        h.push(0x1A);
        assert!(TdfDecoder.sniff(&h));
        assert!(!TdfDecoder.sniff(b"not a tdf"));
    }

    #[test]
    fn unicode_roundtrip_ascii() {
        assert_eq!(unicode_to_cp437('A'), b'A');
        assert_eq!(unicode_to_cp437(' '), b' ');
        // A box-drawing char used by outline fonts maps back to its CP437 byte.
        assert_eq!(unicode_to_cp437('─'), 0xC4);
    }
}

#[cfg(test)]
mod dump {
    use super::*;
    // `cargo test tdf_dump -- --ignored --nocapture` — renders real corpus fonts to /tmp for eyeballing.
    #[test]
    #[ignore]
    fn tdf_dump() {
        let dir = format!("{}/git/WAB_Ansi_Logo_Maker/FONTS", std::env::var("HOME").unwrap());
        for name in ["ARCHANA.TDF", "THINX.TDF"] {
            let p = format!("{dir}/{name}");
            let Ok(bytes) = std::fs::read(&p) else { continue };
            let fonts = font_list(&bytes);
            eprintln!("{name}: {} fonts", fonts.len());
            for (i, (fname, ty, gc)) in fonts.iter().enumerate() {
                eprintln!("  [{i}] {fname:?} ({ty}, {gc} glyphs)");
            }
            if let Some(img) = render_tdf(&bytes, 0, "HELLO World 123") {
                let out = format!("/tmp/tdf_{}.png", name.replace('.', "_"));
                let buf: Vec<u8> = img.rgba_bytes().to_vec();
                image::save_buffer(&out, &buf, img.width, img.height, image::ColorType::Rgba8).unwrap();
                eprintln!("  wrote {out} ({}x{})", img.width, img.height);
            }
        }
    }
}
