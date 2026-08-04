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

/// Rendering options for the TDF viewer (grouped so callers don't juggle a long arg list).
#[derive(Clone, Copy)]
pub struct TdfOpts {
    pub spacing: i32,   // inter-glyph gap delta (negative overlaps)
    pub line_gap: i32,  // vertical gap between multi-line rows
    pub font_9px: bool, // render the 9-dot VGA cell
    pub fg: u8,         // single-colour (outline/block) foreground palette index 0-15
    pub bg: u8,         // single-colour background palette index 0-15 (colour fonts ignore both)
    pub top_down: bool, // multi-line overlap order: true = earlier (upper) lines draw on top
}

impl Default for TdfOpts {
    fn default() -> Self {
        Self { spacing: 0, line_gap: 0, font_9px: false, fg: 15, bg: 0, top_down: true }
    }
}

/// Rasterise `text` in font `index` of the file → a `PixImage`. `None` if nothing drawable.
pub fn render_tdf(bytes: &[u8], index: usize, text: &str, opts: &TdfOpts) -> Option<PixImage> {
    let fonts = TdfFont::load(bytes).ok()?;
    let font = fonts.get(index)?;
    let (grid, w, h) = build_grid_lh(font, text, opts)?;
    Some(rasterize(&grid, w, h, opts.font_9px))
}

/// SAUCE metadata for the ANSI export (so editors like Moebius/PabloDraw open the canvas at the
/// right width instead of wrapping at 80, and the piece is credited to pixelview).
#[derive(Default)]
pub struct AnsiSauce<'a> {
    pub title: &'a str,
    pub author: &'a str,
    pub group: &'a str,
    pub comment: &'a str,
    pub date: &'a str, // CCYYMMDD (8 chars); spaces if unknown
}

/// Encode `text` in font `index` as **ANSI art** (CP437 bytes + SGR colour codes) with a **SAUCE
/// record** carrying the true width/height — a `.ans` file. Converts the cells' VGA-attribute
/// colours to SGR order (the red↔blue / cyan↔brown swap).
pub fn tdf_to_ansi(
    bytes: &[u8],
    index: usize,
    text: &str,
    opts: &TdfOpts,
    sauce: &AnsiSauce,
) -> Option<Vec<u8>> {
    let fonts = TdfFont::load(bytes).ok()?;
    let (grid, w, h) = build_grid_lh(fonts.get(index)?, text, opts)?;
    let mut out = grid_to_ansi(&grid, w, h);
    append_ansi_sauce(&mut out, w, h, sauce);
    Some(out)
}

/// Append an EOF marker + optional COMNT block + a 128-byte SAUCE record describing an ANSI file
/// of `cols`×`rows` characters (DataType 1 / FileType 1). Width in TInfo1 is what makes an editor
/// open the canvas wide enough (the wrap fix).
fn append_ansi_sauce(out: &mut Vec<u8>, cols: usize, rows: usize, s: &AnsiSauce) {
    fn pad_spaces(out: &mut Vec<u8>, text: &str, n: usize) {
        let b = text.as_bytes();
        let take = b.len().min(n);
        out.extend_from_slice(&b[..take]);
        out.extend(std::iter::repeat_n(b' ', n - take));
    }
    let data_len = out.len() as u32; // file size of the character data (before EOF + SAUCE)

    // Comment → COMNT block: up to 255 lines of exactly 64 chars.
    let comment: Vec<&str> = if s.comment.is_empty() { Vec::new() } else { vec![s.comment] };
    let clines: Vec<String> = comment
        .iter()
        .flat_map(|c| c.as_bytes().chunks(64).map(|ch| String::from_utf8_lossy(ch).into_owned()))
        .take(255)
        .collect();

    out.push(0x1A); // Ctrl-Z EOF (SAUCE follows)
    if !clines.is_empty() {
        out.extend_from_slice(b"COMNT");
        for line in &clines {
            pad_spaces(out, line, 64);
        }
    }
    out.extend_from_slice(b"SAUCE00");
    pad_spaces(out, s.title, 35);
    pad_spaces(out, s.author, 20);
    pad_spaces(out, s.group, 20);
    let date = if s.date.len() == 8 { s.date } else { "        " };
    pad_spaces(out, date, 8);
    out.extend_from_slice(&data_len.to_le_bytes()); // FileSize
    out.push(1); // DataType = Character
    out.push(1); // FileType = ANSi
    out.extend_from_slice(&(cols.min(u16::MAX as usize) as u16).to_le_bytes()); // TInfo1 = width
    out.extend_from_slice(&(rows.min(u16::MAX as usize) as u16).to_le_bytes()); // TInfo2 = height
    out.extend_from_slice(&0u16.to_le_bytes()); // TInfo3
    out.extend_from_slice(&0u16.to_le_bytes()); // TInfo4
    out.push(clines.len() as u8); // Comments
    out.push(0x01); // TFlags: bit0 = iCE colours (non-blink)
    // TInfoS (22): font name, null-terminated + null-padded.
    let font = b"IBM VGA";
    out.extend_from_slice(font);
    out.extend(std::iter::repeat_n(0u8, 22 - font.len()));
}

/// Export font `index` as a standalone `.tdf` file (via retrofont's serializer).
pub fn export_font(bytes: &[u8], index: usize) -> Option<Vec<u8>> {
    let fonts = TdfFont::load(bytes).ok()?;
    fonts.get(index)?.to_bytes().ok()
}

/// The TheDraw character slots `'!'..='~'` (33..=126) with whether font `index` defines each —
/// for the viewer's glyph grid ("find gaps"). `(char, is_defined, defined_count)` via the returned
/// list + a caller `.filter(|(_,d)| *d).count()`.
pub fn glyph_coverage(bytes: &[u8], index: usize) -> Vec<(char, bool)> {
    let Ok(fonts) = TdfFont::load(bytes) else { return Vec::new() };
    let Some(font) = fonts.get(index) else { return Vec::new() };
    (33u8..=126).map(|b| (b as char, font.glyph(b as char).is_some())).collect()
}

/// Render font `index`'s glyphs for `chars` into a paged grid — each defined glyph scaled to fit a
/// `cell`-px square (aspect-preserved, centred); undefined slots stay blank so gaps are obvious.
/// Returns `(image, rows)`. Mirrors `font::render_glyph_grid` / `fon::render_glyph_grid`.
pub fn render_glyph_grid(
    bytes: &[u8],
    index: usize,
    chars: &[char],
    cols: usize,
    cell: usize,
    opts: &TdfOpts,
) -> Option<(PixImage, usize)> {
    let fonts = TdfFont::load(bytes).ok()?;
    let font = fonts.get(index)?;
    if chars.is_empty() || cols == 0 || cell == 0 {
        return None;
    }
    let rows = chars.len().div_ceil(cols);
    let (gw, gh) = (cols * cell, rows * cell);
    let mut px = vec![[0u8, 0, 0, 255]; gw * gh];
    let pad = 4usize; // breathing room + implicit cell separation
    // Force a black background here (bg=0) so the near-black skip below leaves gaps transparent;
    // keep the picked fg + 9px so each glyph matches the preview's ink colour.
    let gopts = TdfOpts { bg: 0, spacing: 0, line_gap: 0, ..*opts };
    for (i, &ch) in chars.iter().enumerate() {
        let (cx, cy) = ((i % cols) * cell, (i / cols) * cell);
        let Some((grid, w, h)) = build_grid(font, &ch.to_string(), &gopts) else { continue };
        let img = rasterize(&grid, w, h, gopts.font_9px);
        let (iw, ih) = (img.width as usize, img.height as usize);
        if iw == 0 || ih == 0 {
            continue;
        }
        let avail = cell.saturating_sub(pad).max(1) as f32;
        // Fit the glyph in the cell. When it fits at ≥1×, snap UP to a whole integer scale so the
        // pixels stay crisp (nearest, no blur) — a big cell shows a big, sharp glyph. A glyph
        // larger than the cell downscales fractionally (unavoidable) to fit.
        let raw = (avail / iw as f32).min(avail / ih as f32);
        let scale = if raw >= 1.0 { raw.floor() } else { raw.max(0.01) };
        let (dw, dh) = ((iw as f32 * scale) as usize, (ih as f32 * scale) as usize);
        let (ox, oy) = (cx + (cell.saturating_sub(dw)) / 2, cy + (cell.saturating_sub(dh)) / 2);
        let src = img.rgba_bytes();
        for dy in 0..dh {
            let sy = ((dy as f32 / scale) as usize).min(ih - 1);
            for dx in 0..dw {
                let sx = ((dx as f32 / scale) as usize).min(iw - 1);
                let s = (sy * iw + sx) * 4;
                // Skip near-black (the glyph background) so cells don't paint solid boxes.
                if src[s] as u16 + src[s + 1] as u16 + src[s + 2] as u16 > 24 {
                    let (x, y) = (ox + dx, oy + dy);
                    if x < gw && y < gh {
                        px[y * gw + x] = [src[s], src[s + 1], src[s + 2], 255];
                    }
                }
            }
        }
    }
    Some((PixImage::from_rgba(gw as u32, gh as u32, px), rows))
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

/// A laid-out glyph: its cell rows + advance width.
struct Block {
    rows: Vec<Vec<Cell>>,
    width: usize,
}

/// Lay out `text` glyph-by-glyph into a CP437 cell grid. Glyphs are top-aligned; the inter-glyph
/// gap is the font's own `spacing` plus `extra_spacing` (may be **negative** → letters overlap;
/// later ink draws over earlier, transparent cells don't erase). Returns `(grid, cols, rows)`.
fn build_grid(font: &TdfFont, text: &str, opts: &TdfOpts) -> Option<(Vec<Cell>, usize, usize)> {
    build_grid_lh(font, text, opts)
}

/// The background cell for a render — a colour font stays on black; a single-colour (outline/block)
/// font fills with the picked `bg` so an fg/bg pick tints the whole canvas.
fn bg_cell(font: &TdfFont, opts: &TdfOpts) -> Cell {
    if font.font_type == TdfFontType::Color {
        Cell::BLANK
    } else {
        Cell { ch: b' ', fg: opts.fg & 0x0f, bg: opts.bg & 0x0f, ink: false }
    }
}

/// Multi-line layout: each `\n`-separated line is rendered as its own row of TheDraw letters and
/// stacked vertically, with `line_gap` extra blank cell-rows between lines (may be **negative** to
/// tighten). `top_down` chooses which line wins where they overlap.
fn build_grid_lh(font: &TdfFont, text: &str, opts: &TdfOpts) -> Option<(Vec<Cell>, usize, usize)> {
    let lines: Vec<&str> = text.split('\n').collect();
    // The natural line height = the tallest glyph the font defines (so blank lines match).
    let line_h = (33u8..=126)
        .filter_map(|b| font.glyph(b as char))
        .map(|g| g.height)
        .max()
        .unwrap_or(1)
        .max(1);
    // Build each line's own grid.
    let built: Vec<(Vec<Cell>, usize, usize)> =
        lines.iter().map(|l| build_line(font, l, opts).unwrap_or((Vec::new(), 1, line_h))).collect();
    if built.iter().all(|(_, _, h)| *h == 0) {
        return None;
    }
    let total_w = built.iter().map(|(_, w, _)| *w).max().unwrap_or(1).max(1);
    // Uniform line pitch = the font's line height + line_gap (clamped so it stays ≥ 1 row).
    let n = built.len();
    let pitch = (line_h as i32 + opts.line_gap).max(1);
    let total_h = (line_h as i32 + pitch * (n.saturating_sub(1)) as i32).max(1) as usize;

    let blank = bg_cell(font, opts);
    let mut grid = vec![blank; total_w * total_h];
    // Draw order: `top_down` draws the LAST line first so line 0 (top) overwrites in an overlap;
    // otherwise the last line lands on top.
    let order: Vec<usize> = if opts.top_down { (0..n).rev().collect() } else { (0..n).collect() };
    for &i in &order {
        let (cells, w, h) = &built[i];
        let y0 = i as i32 * pitch;
        for ry in 0..*h {
            let gy = y0 + ry as i32;
            if gy < 0 || gy as usize >= total_h {
                continue;
            }
            for cx in 0..*w {
                let cell = cells.get(ry * w + cx).copied().unwrap_or(blank);
                if cell.ink || cell.bg != 0 {
                    grid[gy as usize * total_w + cx] = cell;
                }
            }
        }
    }
    Some((grid, total_w, total_h))
}

/// Lay out a SINGLE line of TheDraw letters → `(cells, cols, rows)`.
fn build_line(font: &TdfFont, text: &str, opts: &TdfOpts) -> Option<(Vec<Cell>, usize, usize)> {
    let gap = (font.spacing.clamp(0, 40) + opts.spacing).max(-64); // allow overlap, bound it
    // Single-colour (outline/block) glyphs use the picked fg/bg; colour fonts keep per-cell colour.
    let color_font = font.font_type == TdfFontType::Color;
    let (gfg, gbg) = if color_font { (15u8, 0u8) } else { (opts.fg & 0x0f, opts.bg & 0x0f) };
    let blank = bg_cell(font, opts);

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
        let mut rows: Vec<Vec<Cell>> = vec![Vec::new()];
        let push = |rows: &mut Vec<Vec<Cell>>, cell: Cell| rows.last_mut().unwrap().push(cell);
        for part in &glyph.parts {
            match part {
                GlyphPart::NewLine => rows.push(Vec::new()),
                GlyphPart::EndMarker => {}
                GlyphPart::Skip => push(&mut rows, blank),
                GlyphPart::HardBlank | GlyphPart::FillMarker | GlyphPart::OutlineHole => {
                    push(&mut rows, Cell { ink: false, ..blank })
                }
                GlyphPart::OutlinePlaceholder(b) => {
                    let uc = transform_outline(OUTLINE_STYLE, *b);
                    push(&mut rows, Cell { ch: unicode_to_cp437(uc), fg: gfg, bg: gbg, ink: true })
                }
                GlyphPart::Char(c) => {
                    push(&mut rows, Cell { ch: unicode_to_cp437(*c), fg: gfg, bg: gbg, ink: true })
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
        blocks.push(Block { rows, width });
    }
    if blocks.is_empty() {
        return None;
    }

    // Per-glyph x positions (i32 so a negative gap can overlap; clamp the left margin ≥ 0).
    let mut positions: Vec<i32> = Vec::with_capacity(blocks.len());
    let mut x = 1i32;
    for b in &blocks {
        positions.push(x);
        x += b.width as i32 + gap;
    }
    let right = blocks
        .iter()
        .zip(&positions)
        .map(|(b, &p)| p + b.width as i32)
        .max()
        .unwrap_or(1);
    let total_w = (right + 1).max(1) as usize;
    let total_h = blocks.iter().map(|b| b.rows.len()).max().unwrap_or(1).max(1);

    let mut grid = vec![blank; total_w * total_h];
    for (b, &p) in blocks.iter().zip(&positions) {
        for (ry, row) in b.rows.iter().enumerate() {
            for (cx, cell) in row.iter().enumerate() {
                if cell.ink || cell.bg != 0 {
                    let gx = p + cx as i32;
                    if gx >= 0 && (gx as usize) < total_w && ry < total_h {
                        grid[ry * total_w + gx as usize] = *cell;
                    }
                }
            }
        }
    }
    Some((grid, total_w, total_h))
}

/// VGA attribute index → ANSI SGR colour number (fixes the red↔blue / cyan↔brown swap).
const VGA_TO_SGR: [u8; 8] = [0, 4, 2, 6, 1, 5, 3, 7];

/// Encode a CP437 cell grid as ANSI art bytes (SGR colour runs + CP437 glyph bytes, CRLF rows).
fn grid_to_ansi(grid: &[Cell], cols: usize, rows: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for r in 0..rows {
        // Trim trailing blank (space, black bg) cells so lines aren't padded with colour noise.
        let mut len = cols;
        while len > 0 {
            let c = grid[r * cols + len - 1];
            if c.ch == b' ' && c.bg == 0 {
                len -= 1;
            } else {
                break;
            }
        }
        let (mut lf, mut lb) = (u8::MAX, u8::MAX);
        for cx in 0..len {
            let cell = grid[r * cols + cx];
            if cell.fg != lf || cell.bg != lb {
                let f = 30 + VGA_TO_SGR[(cell.fg & 7) as usize];
                let b = 40 + VGA_TO_SGR[(cell.bg & 7) as usize];
                if cell.fg & 8 != 0 {
                    out.extend_from_slice(format!("\x1b[0;1;{f};{b}m").as_bytes());
                } else {
                    out.extend_from_slice(format!("\x1b[0;{f};{b}m").as_bytes());
                }
                lf = cell.fg;
                lb = cell.bg;
            }
            out.push(cell.ch);
        }
        out.extend_from_slice(b"\x1b[0m\r\n");
    }
    out
}

/// Blit a CP437 `(ch, fg, bg)` grid to RGBA using the embedded 8×16 VGA font + VGA palette.
fn rasterize(grid: &[Cell], cols: usize, rows: usize, font_9px: bool) -> PixImage {
    use super::ansi::VGA_PALETTE;
    use super::cp437_font::CP437_8X16;
    // 9-dot VGA cell: the 9th column is background for every glyph except the line-draw range
    // 0xC0..=0xDF, where it repeats column 8 so box rules connect (mirrors `ansi::dot_on`).
    let cell_w = if font_9px { FONT_W + 1 } else { FONT_W };
    let w = cols * cell_w;
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
                for rx in 0..cell_w {
                    let on = if rx < FONT_W {
                        (bits >> (7 - rx)) & 1 == 1
                    } else {
                        (0xC0u8..=0xDFu8).contains(&cell.ch) && (bits & 1 == 1)
                    };
                    let c = if on { fg } else { bg };
                    pixels[(cy * FONT_H + ry) * w + (cx * cell_w + rx)] = [c[0], c[1], c[2], 255];
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
        let (grid, w, h) = build_grid(font, &sample_text(font), &TdfOpts::default()).ok_or(DecodeError::Unsupported)?;
        Ok(rasterize(&grid, w, h, false))
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
            if let Some(img) = render_tdf(&bytes, 0, "HELLO World 123", &TdfOpts { spacing: 0, line_gap: 0, font_9px: false, ..Default::default() }) {
                let out = format!("/tmp/tdf_{}.png", name.replace('.', "_"));
                let buf: Vec<u8> = img.rgba_bytes().to_vec();
                image::save_buffer(&out, &buf, img.width, img.height, image::ColorType::Rgba8).unwrap();
                eprintln!("  wrote {out} ({}x{})", img.width, img.height);
            }
        }
    }
}

#[cfg(test)]
mod export_tests {
    use super::*;
    #[test]
    #[ignore]
    fn ans_and_tdf_export_roundtrip() {
        let dir = format!("{}/git/WAB_Ansi_Logo_Maker/FONTS", std::env::var("HOME").unwrap());
        let Ok(bytes) = std::fs::read(format!("{dir}/ARCHANA.TDF")) else { return };
        // .ans export non-empty + starts with an SGR escape
        let ans = tdf_to_ansi(&bytes, 0, "AB", &TdfOpts { spacing: 0, line_gap: 0, ..Default::default() }, &AnsiSauce::default()).unwrap();
        eprintln!("ans {} bytes, head: {:?}", ans.len(), &ans[..ans.len().min(12)]);
        assert!(ans.windows(2).any(|w| w == b"\x1b["));
        // overlap spacing shrinks the render width
        let w0 = render_tdf(&bytes, 0, "AB", &TdfOpts { spacing: 0, line_gap: 0, font_9px: false, ..Default::default() }).unwrap().width;
        let wn = render_tdf(&bytes, 0, "AB", &TdfOpts { spacing: -4, line_gap: 0, font_9px: false, ..Default::default() }).unwrap().width;
        eprintln!("width normal={w0} overlap={wn}");
        assert!(wn < w0);
        // .tdf export re-loads as a font
        let tdf = export_font(&bytes, 0).unwrap();
        let reload = TdfFont::load(&tdf).unwrap();
        eprintln!("reloaded {} font(s), name {:?}", reload.len(), reload.first().map(|f| &f.name));
        assert_eq!(reload.len(), 1);
        std::fs::write("/tmp/exported.ans", &ans).unwrap();
    }
}


#[cfg(test)]
mod grid_test {
    use super::*;
    #[test]
    #[ignore]
    fn dump_9px_and_grid() {
        let dir = format!("{}/git/WAB_Ansi_Logo_Maker/FONTS", std::env::var("HOME").unwrap());
        let Ok(bytes) = std::fs::read(format!("{dir}/ARCHANA.TDF")) else { return };
        // 8px vs 9px width delta
        let w8 = render_tdf(&bytes, 0, "WWW", &TdfOpts { spacing: 0, line_gap: 0, font_9px: false, ..Default::default() }).unwrap().width;
        let w9 = render_tdf(&bytes, 0, "WWW", &TdfOpts { spacing: 0, line_gap: 0, font_9px: true, ..Default::default() }).unwrap().width;
        eprintln!("width 8px={w8} 9px={w9}");
        assert!(w9 > w8);
        // coverage + grid
        let cov = glyph_coverage(&bytes, 0);
        let defined = cov.iter().filter(|(_,d)| *d).count();
        eprintln!("coverage {defined}/{}", cov.len());
        let chars: Vec<char> = cov.iter().map(|(c,_)| *c).collect();
        if let Some((img,rows)) = render_glyph_grid(&bytes, 0, &chars, 16, 48, &TdfOpts::default()) {
            image::save_buffer("/tmp/tdf_grid.png", &img.rgba_bytes(), img.width, img.height, image::ColorType::Rgba8).unwrap();
            eprintln!("grid {rows} rows → /tmp/tdf_grid.png {}x{}", img.width, img.height);
        }
    }
}

#[cfg(test)]
mod sauce_multiline {
    use super::*;
    #[test]
    #[ignore]
    fn multiline_and_sauce() {
        let dir = format!("{}/git/WAB_Ansi_Logo_Maker/FONTS", std::env::var("HOME").unwrap());
        let Ok(bytes) = std::fs::read(format!("{dir}/ARCHANA.TDF")) else { return };
        // multi-line: 2 lines should be taller than 1 line
        let h1 = render_tdf(&bytes, 0, "AB", &TdfOpts { spacing: 0, line_gap: 0, font_9px: false, ..Default::default() }).unwrap().height;
        let h2 = render_tdf(&bytes, 0, "AB\nCD", &TdfOpts { spacing: 0, line_gap: 2, font_9px: false, ..Default::default() }).unwrap().height;
        eprintln!("height 1-line={h1} 2-line={h2}");
        assert!(h2 > h1 + 5);
        // SAUCE on the ANS
        let sauce = AnsiSauce { title: "bbs main menu", author: "", group: "pixel-viewer",
            comment: "Created by pixel-viewer https://github.com/grymmjack/pixel-viewer", date: "20260804" };
        let ans = tdf_to_ansi(&bytes, 0, "WW", &TdfOpts { spacing: 0, line_gap: 0, ..Default::default() }, &sauce).unwrap();
        // last 128 bytes = SAUCE record
        let rec = &ans[ans.len()-128..];
        assert_eq!(&rec[0..7], b"SAUCE00");
        let tinfo1 = u16::from_le_bytes([rec[96], rec[97]]);
        let tinfo2 = u16::from_le_bytes([rec[98], rec[99]]);
        eprintln!("SAUCE width={tinfo1} height={tinfo2} datatype={} filetype={}", rec[94], rec[95]);
        assert_eq!(rec[94], 1); assert_eq!(rec[95], 1);
        assert!(tinfo1 > 0 && tinfo2 > 0);
        let title = String::from_utf8_lossy(&rec[7..42]);
        eprintln!("title={:?}", title.trim());
        assert!(title.starts_with("bbs main menu"));
        assert!(ans.windows(5).any(|w| w == b"COMNT"));
    }
}


#[cfg(test)]
mod color_test {
    use super::*;
    #[test]
    #[ignore]
    fn single_color_fg_bg() {
        let dir = format!("{}/git/WAB_Ansi_Logo_Maker/FONTS", std::env::var("HOME").unwrap());
        // find a block/outline (single-color) font
        for name in ["ALPHAX.TDF","CALVIN_S.TDF"] {
            let Ok(bytes) = std::fs::read(format!("{dir}/{name}")) else { continue };
            let fonts = TdfFont::load(&bytes).unwrap();
            let idx = fonts.iter().position(|f| f.font_type != TdfFontType::Color);
            let Some(i) = idx else { continue };
            let green = TdfOpts { fg: 2, bg: 1, ..Default::default() }; // green on blue
            let img = render_tdf(&bytes, i, "A", &green).unwrap();
            let px = img.rgba_bytes();
            let has_green = px.chunks_exact(4).any(|p| p[1] > 120 && p[0] < 80 && p[2] < 80);
            let has_blue = px.chunks_exact(4).any(|p| p[2] > 120 && p[0] < 80 && p[1] < 80);
            eprintln!("{name} font {i} ({:?}): green_ink={has_green} blue_bg={has_blue}", fonts[i].font_type);
            assert!(has_green, "expected green ink");
            assert!(has_blue, "expected blue bg");
            image::save_buffer("/tmp/tdf_color.png", &px, img.width, img.height, image::ColorType::Rgba8).unwrap();
            return;
        }
    }
}
