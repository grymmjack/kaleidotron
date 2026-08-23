use super::cp437_font::CP437_8X16;
use super::cp437_font_8x8::CP437_8X8;
use super::{DecodeError, Decoder};
use crate::image_types::PixImage;

/// ANSI / ASCII art (.ans/.asc/.nfo/.diz) rendered with an embedded CP437 8×16
/// VGA font + the 16-color VGA palette. Handles SGR (colors incl. bright/bold,
/// reverse, blink→iCE bright-bg, attribute resets), cursor up/down/left/right +
/// absolute (CHA/VPA) + save/restore (ESC[s/u and ESC 7/8), CR/LF, the SAUCE-driven
/// canvas width + iCE flag (default on), auto-wrap, and the DOS-EOF/SAUCE trailer.
pub struct AnsiDecoder;

const FONT_W: usize = 8;
const FONT_H: usize = 16;
const WRAP: usize = 80; // classic ANSI terminal width

// Hard cap for cursor-addressed / SAUCE-declared columns. Real scene art is usually 80, but
// "wide" ANSI (e.g. Mistigris party pieces) declares hundreds of columns via SAUCE TInfo1 —
// THE_BIG_PIRANHA is 800. The cap only exists so a runaway cursor (ESC[99999C) can't grow an
// unbounded canvas; it must sit well above any real width, or the art auto-wraps at the cap
// and scrambles (800 clamped to 300 → reflowed to noise).
const MAX_COLS: usize = 20000;
const MAX_ROWS: usize = 10000; // safety cap for very long files (canvas sizes to the
                               // *actual* content rows; this is only the upper bound)
// Hard cap on the canvas *area* in cells, independent of the per-axis caps above. At a 9×16
// cell that's ~144 Mpx / 576 MB RGBA worst case — big, but bounded. Its job is to stop a
// corrupt SAUCE (see `TextStream::new`) or a runaway from allocating gigabytes and freezing
// the app: ACIDVIEW.TXT declares TInfo1 = 8272 columns × 1296 rows ≈ 6 GB. Real scene art is
// far below this (THE_BIG_PIRANHA, 800 wide, is ~0.5 M cells).
const MAX_CANVAS_CELLS: usize = 1_000_000;

/// Render the VGA font in a 9-dot-wide cell, the way real VGA text mode did —
/// applies to BOTH the 8×16 font and the 8×8 VGA50/EGA43 cell (→ 9×16 / 9×8). The
/// 9th column is background for every glyph except the line-draw range `0xC0..=0xDF`,
/// where the hardware repeated column 8 so horizontal rules joined across cells (the
/// repeat is font-independent — it reads the glyph's rightmost bit — so it works for
/// both fonts). Off → exact 8-pixel cells. A process-wide rendering preference (set
/// from the UI); read at decode time. See [`set_font_9px`].
static FONT_9PX: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Toggle the 9-dot VGA cell width for subsequent ANSI/CP437 decodes.
pub fn set_font_9px(on: bool) {
    FONT_9PX.store(on, std::sync::atomic::Ordering::Relaxed);
}

fn font_9px() -> bool {
    FONT_9PX.load(std::sync::atomic::Ordering::Relaxed)
}

/// 16-color palette in **ANSI SGR** index order (SGR 31=red→1, 34=blue→4). This is
/// what the ANSI parser maps SGR codes into, so it's indexed by `cell.fg/bg` here.
pub(super) const PALETTE: [[u8; 3]; 16] = [
    [0, 0, 0],       // 0 black
    [170, 0, 0],     // 1 red
    [0, 170, 0],     // 2 green
    [170, 85, 0],    // 3 brown/yellow
    [0, 0, 170],     // 4 blue
    [170, 0, 170],   // 5 magenta
    [0, 170, 170],   // 6 cyan
    [170, 170, 170], // 7 light grey
    [85, 85, 85],    // 8 dark grey
    [255, 85, 85],   // 9 bright red
    [85, 255, 85],   // 10 bright green
    [255, 255, 85],  // 11 bright yellow
    [85, 85, 255],   // 12 bright blue
    [255, 85, 255],  // 13 bright magenta
    [85, 255, 255],  // 14 bright cyan
    [255, 255, 255], // 15 white
];

/// The same 16 colors in **VGA hardware (attribute byte)** index order (index 1=blue,
/// 4=red — the bits are I·R·G·B). The binary text-mode formats (BIN/XBIN/…) store raw
/// VGA attribute bytes, so their default palette MUST be this order, not [`PALETTE`] —
/// otherwise red↔blue and cyan↔brown swap (the `ansi::PALETTE` order is for SGR codes).
pub(super) const VGA_PALETTE: [[u8; 3]; 16] = [
    [0, 0, 0],       // 0 black
    [0, 0, 170],     // 1 blue
    [0, 170, 0],     // 2 green
    [0, 170, 170],   // 3 cyan
    [170, 0, 0],     // 4 red
    [170, 0, 170],   // 5 magenta
    [170, 85, 0],    // 6 brown/yellow
    [170, 170, 170], // 7 light grey
    [85, 85, 85],    // 8 dark grey
    [85, 85, 255],   // 9 bright blue
    [85, 255, 85],   // 10 bright green
    [85, 255, 255],  // 11 bright cyan
    [255, 85, 85],   // 12 bright red
    [255, 85, 255],  // 13 bright magenta
    [255, 255, 85],  // 14 bright yellow
    [255, 255, 255], // 15 white
];

/// An xterm 256-color index → RGB: 0-15 = the base palette, 16-231 = a 6×6×6 cube,
/// 232-255 = a 24-step grayscale ramp.
fn xterm256_rgb(n: u8) -> (u8, u8, u8) {
    match n {
        0..=15 => {
            let p = PALETTE[n as usize];
            (p[0], p[1], p[2])
        }
        16..=231 => {
            let c = n - 16;
            let lv = [0u8, 95, 135, 175, 215, 255];
            (
                lv[(c / 36) as usize],
                lv[((c / 6) % 6) as usize],
                lv[(c % 6) as usize],
            )
        }
        _ => {
            let v = 8 + (n - 232) * 10;
            (v, v, v)
        }
    }
}

#[derive(Clone, Copy)]
struct Cell {
    ch: u8,
    fg: [u8; 3], // resolved RGB (so 24-bit PabloDraw colors render exactly)
    bg: [u8; 3],
}

const BLANK: Cell = Cell {
    ch: b' ',
    fg: PALETTE[7],
    bg: PALETTE[0],
};

impl Decoder for AnsiDecoder {
    fn name(&self) -> &'static str {
        "ansi"
    }

    fn extensions(&self) -> &'static [&'static str] {
        // .ice = iCE-colors ANSI, .cia = CIA-group ANSI — same CP437/SGR rendering.
        // .txt = plain ASCII/ANSI art (common in scene packs alongside readmes).
        &["ans", "asc", "nfo", "diz", "ice", "cia", "txt"]
    }

    fn sniff(&self, _header: &[u8]) -> bool {
        false // text art has no reliable magic; dispatched by extension
    }

    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        let s = TextStream::new(bytes)
            .ok_or_else(|| DecodeError::Malformed("empty ANSI/ASCII".into()))?;
        Ok(s.render(s.len()))
    }
}

/// A parsed ANSI/CP437 file ready to render either in full or as a byte *prefix* —
/// the latter drives baud-rate playback (watch the art "type out" over a simulated
/// modem). Canvas size is fixed from the *whole* file so prefix frames don't resize.
pub struct TextStream {
    content: Vec<u8>, // SAUCE-stripped bytes
    wrap: usize,
    ice: bool,
    glyph_h: usize, // 8 (VGA50) or 16
    cols: usize,    // full canvas columns
    rows: usize,    // full canvas rows
}

impl TextStream {
    /// Parse `bytes` and size the canvas from the whole file. None if it has no rows.
    pub fn new(bytes: &[u8]) -> Option<TextStream> {
        let (ice, sauce_w, font) = read_sauce(bytes);
        let content = crate::sauce::strip(bytes).to_vec();
        let wrap = sauce_w.unwrap_or(WRAP).clamp(1, MAX_COLS);
        let (grid, cursor_rows) = parse(&content, wrap, ice);
        if grid.is_empty() {
            return None;
        }
        let cols0 = grid.iter().map(Vec::len).max().unwrap_or(0).max(1);
        // 80×50-mode art (SAUCE font "IBM VGA50" / "IBM EGA43") uses an 8×8 cell.
        let glyph_h = if font_is_8x8(&font) { 8 } else { FONT_H };
        // A DOS screen is ≥25 rows; pad short art up so it isn't cropped. Size to the
        // *cursor* extent (≥ written rows), so trailing blank lines the cursor moved onto
        // are part of the canvas and baud auto-scroll can reach them. The full
        // SAUCE-declared width (TInfo1) is the canvas, not just the widest written row.
        let mut rows = cursor_rows.max(25);
        // The SAUCE-declared width (TInfo1) is normally the canvas, so cursor-addressed art
        // keeps its full width even where no single row is that long. But a *corrupt* SAUCE
        // can declare an absurd one — ACIDVIEW.TXT's TInfo1 reads 8272 (a buggy 1998 writer
        // left a space, 0x20, in the high byte of 80), which would size a ~6 GB canvas and
        // freeze the app. Guard it in two steps:
        //   1. If padding to the declared width would blow the cell budget while the real
        //      content (`cols0`, the widest *written* column — cursor moves included) is far
        //      narrower, the width is bogus: fall back to the content width. This only drops
        //      empty padding, never real cells, so legitimate wide art is untouched.
        //   2. If the canvas is still over budget (genuinely huge art / a runaway), trim the
        //      row count so the allocation stays bounded.
        let mut cols = sauce_w.map_or(cols0, |dw| cols0.max(dw.clamp(1, MAX_COLS)));
        if cols.saturating_mul(rows) > MAX_CANVAS_CELLS && cols > cols0 {
            cols = cols0;
        }
        if cols.saturating_mul(rows) > MAX_CANVAS_CELLS {
            rows = (MAX_CANVAS_CELLS / cols.max(1)).max(25);
        }
        Some(TextStream {
            content,
            wrap,
            ice,
            glyph_h,
            cols,
            rows,
        })
    }

    /// 9-dot cell (both the 8×16 font and the 8×8 VGA50/EGA43 cell) when the global
    /// toggle is on; exact 8-pixel cells otherwise.
    fn cell_w(&self) -> usize {
        if font_9px() {
            9
        } else {
            FONT_W
        }
    }

    pub fn len(&self) -> usize {
        self.content.len()
    }

    /// Render the first `limit` content bytes into the full (fixed) canvas, plus the
    /// pixel height of the content drawn so far — the typing "cursor" row × cell height
    /// — which the viewer uses to auto-scroll a long ANSImation, BBS-style.
    pub fn render_frame(&self, limit: usize) -> (PixImage, u32) {
        let lim = limit.min(self.content.len());
        let (grid, cursor_rows) = parse(&self.content[..lim], self.wrap, self.ice);
        // Follow the cursor's full extent (incl. trailing blank lines), capped at the
        // canvas so the scroll lands exactly at the bottom.
        let cursor_px = (cursor_rows.min(self.rows) * self.glyph_h) as u32;
        let img = render_grid(&grid, self.cols, self.rows, self.glyph_h, self.cell_w());
        (img, cursor_px)
    }

    /// Render the first `limit` content bytes into the full (fixed) canvas.
    pub fn render(&self, limit: usize) -> PixImage {
        self.render_frame(limit).0
    }
}

/// Rasterize a parsed cell grid into a `cols×rows`-cell canvas. Cells past the grid
/// (and the 25-row / full-width padding) stay background. Shared by the full decode
/// and by [`TextStream`] prefix frames.
fn render_grid(
    grid: &[Vec<Cell>],
    cols: usize,
    rows: usize,
    glyph_h: usize,
    cell_w: usize,
) -> PixImage {
    let w = cols * cell_w;
    let h = rows * glyph_h;
    let mut pixels = vec![[0u8, 0, 0, 255]; w * h];
    for (cy, row) in grid.iter().enumerate() {
        for (cx, cell) in row.iter().enumerate() {
            let glyph: &[u8] = if glyph_h == 8 {
                &CP437_8X8[cell.ch as usize]
            } else {
                &CP437_8X16[cell.ch as usize]
            };
            let (fg, bg) = (cell.fg, cell.bg); // already resolved to RGB
            for (ry, &bits) in glyph.iter().enumerate() {
                for rx in 0..cell_w {
                    let on = dot_on(bits, rx, cell.ch);
                    let c = if on { fg } else { bg };
                    let (px, py) = (cx * cell_w + rx, cy * glyph_h + ry);
                    if px < w && py < h {
                        pixels[py * w + px] = [c[0], c[1], c[2], 255];
                    }
                }
            }
        }
    }
    PixImage::from_rgba(w as u32, h as u32, pixels)
}

/// The SAUCE rendering hints: `(ice_colors, character_width)`. iCE defaults to ON
/// when there's no SAUCE — most BBS art is iCE, and a static viewer can't blink
/// anyway, so treating the blink bit as a bright background is the useful
/// interpretation. Width (TInfo1) is authoritative for Character-type art.
fn read_sauce(data: &[u8]) -> (bool, Option<usize>, String) {
    match crate::sauce::parse(data) {
        Some(s) => (s.ice, s.char_width(), s.font),
        None => (true, None, String::new()),
    }
}

/// Does this SAUCE font name denote an 8×8 (80×50 / 80×43) text mode? The name is
/// `IBM <mode> [codepage]`; only the `VGA50` / `EGA43` modes are 8×8. Matching the
/// *mode* token (not just "50") avoids a false hit on a codepage like "IBM VGA 850".
/// Is dot column `rx` lit, for glyph scanline `bits` of character `ch`? Columns
/// `0..8` read the 8-pixel glyph; column 8 (the 9th VGA dot) is background except
/// for the line-draw range `0xC0..=0xDF`, where it repeats column 8 so box rules
/// connect across cells — exactly what VGA 9-dot text mode did.
fn dot_on(bits: u8, rx: usize, ch: u8) -> bool {
    if rx < FONT_W {
        (bits >> (7 - rx)) & 1 == 1
    } else {
        (0xC0u8..=0xDFu8).contains(&ch) && (bits & 1 == 1)
    }
}

fn font_is_8x8(font: &str) -> bool {
    let mode = font.split_whitespace().nth(1).unwrap_or("");
    mode.eq_ignore_ascii_case("VGA50") || mode.eq_ignore_ascii_case("EGA43")
}

fn ensure(grid: &mut Vec<Vec<Cell>>, y: usize, x: usize) {
    while grid.len() <= y {
        grid.push(Vec::new());
    }
    while grid[y].len() <= x {
        grid[y].push(BLANK);
    }
}

/// Parse `data` into a cell grid. Returns the grid plus the number of rows the cursor
/// *occupied* (`max_y + 1`) — which counts trailing blank lines the cursor moved onto
/// but never wrote (e.g. a CRLF run at the end). `grid.len()` only counts written rows;
/// the cursor extent is what baud auto-scroll follows so it reaches the final blank line.
fn parse(data: &[u8], wrap: usize, ice: bool) -> (Vec<Vec<Cell>>, usize) {
    let mut grid: Vec<Vec<Cell>> = Vec::new();
    let (mut x, mut y) = (0usize, 0usize);
    let mut max_y = 0usize;
    let mut saved = (0usize, 0usize); // ESC[s/u and ESC 7/8 cursor save/restore
    let (mut fg, mut bg) = (7u8, 0u8);
    let (mut bold, mut blink, mut reverse) = (false, false, false);
    // 24-bit overrides (PabloDraw `ESC[…t` / SGR 38;2/48;2): `Some` wins over the 16-color
    // index, `None` falls back to the palette. Cleared whenever a 16-color SGR sets fg/bg.
    let (mut fg_rgb, mut bg_rgb): (Option<[u8; 3]>, Option<[u8; 3]>) = (None, None);
    let mut i = 0;
    while i < data.len() {
        if y >= MAX_ROWS {
            break;
        }
        // Auto-wrap at the right margin, checked *before* processing each byte. The cursor
        // parks at column `wrap` after the last column is written; the wrap then fires on
        // the *next* byte — including ESC, so an `ESC[s` saves the wrapped position, not
        // the parked one (ACID-RN.ANS / gj-os.ans).
        //
        // But CR/LF are EXCLUDED: a full-width row (every cell written through column
        // `wrap`) followed by CRLF must advance ONE row — the newline itself does it — not
        // two. Pre-wrapping on the CR/LF too would leave a blank row between every content
        // row, so a "full length" 80-wide piece renders as venetian-blind black stripes.
        // This matches ansilove, whose auto-wrap lives in the printable-char branch and so
        // never double-advances on a newline (overstrike art that does `\r\n ESC[A` then
        // returns to the *same* row it drew, as ansilove intends).
        if x >= wrap && !matches!(data[i], 0x0A | 0x0D) {
            x = 0;
            y += 1;
        }
        match data[i] {
            // ESC 7 / ESC 8 — the non-CSI save/restore cursor (DECSC/DECRC).
            0x1B if data.get(i + 1) == Some(&b'7') => {
                saved = (x, y);
                i += 2;
            }
            0x1B if data.get(i + 1) == Some(&b'8') => {
                (x, y) = saved;
                i += 2;
            }
            0x1B if data.get(i + 1) == Some(&b'[') => {
                // CSI: params (digits/';') until a final byte 0x40..=0x7E.
                let start = i + 2;
                let mut j = start;
                while j < data.len() && !(0x40..=0x7E).contains(&data[j]) {
                    j += 1;
                }
                if j >= data.len() {
                    break;
                }
                let nums: Vec<u32> = std::str::from_utf8(&data[start..j])
                    .unwrap_or("")
                    .split(';')
                    .map(|s| s.trim().parse().unwrap_or(0))
                    .collect();
                match data[j] {
                    b'm' => {
                        let mut k = 0;
                        while k < nums.len() {
                            match nums[k] {
                                0 => {
                                    fg = 7;
                                    bg = 0;
                                    bold = false;
                                    blink = false;
                                    reverse = false;
                                    fg_rgb = None;
                                    bg_rgb = None;
                                }
                                1 => bold = true,
                                5 | 6 => blink = true,
                                7 => reverse = true,
                                21 | 22 => bold = false,
                                25 => blink = false,
                                27 => reverse = false,
                                30..=37 => {
                                    fg = (nums[k] - 30) as u8;
                                    fg_rgb = None;
                                }
                                39 => {
                                    fg = 7;
                                    fg_rgb = None;
                                }
                                40..=47 => {
                                    bg = (nums[k] - 40) as u8;
                                    bg_rgb = None;
                                }
                                49 => {
                                    bg = 0;
                                    bg_rgb = None;
                                }
                                90..=97 => {
                                    fg = (nums[k] - 90 + 8) as u8;
                                    fg_rgb = None;
                                }
                                100..=107 => {
                                    bg = (nums[k] - 100 + 8) as u8;
                                    bg_rgb = None;
                                }
                                // Extended color: 38/48 ;5;n (256-color) or ;2;r;g;b
                                // (truecolor) — stored as an exact RGB override.
                                38 | 48 => {
                                    let to_fg = nums[k] == 38;
                                    let rgb = match nums.get(k + 1).copied() {
                                        Some(5) => {
                                            let n = nums.get(k + 2).copied().unwrap_or(0);
                                            k += 2;
                                            let (r, g, b) = xterm256_rgb(n.min(255) as u8);
                                            Some([r, g, b])
                                        }
                                        Some(2) => {
                                            let ch = |o: usize| {
                                                nums.get(k + o).copied().unwrap_or(0).min(255) as u8
                                            };
                                            let rgb = [ch(2), ch(3), ch(4)];
                                            k += 4;
                                            Some(rgb)
                                        }
                                        _ => None,
                                    };
                                    if let Some(rgb) = rgb {
                                        if to_fg {
                                            fg_rgb = Some(rgb);
                                        } else {
                                            bg_rgb = Some(rgb);
                                        }
                                    }
                                }
                                _ => {}
                            }
                            k += 1;
                        }
                    }
                    // PabloDraw 24-bit RGB: `ESC[<sel>;r;g;b t` — sel 0 = background,
                    // 1 = foreground. This is how Blocktronics/modern ANSI tweak palettes
                    // (the file also emits a 16-color SGR fallback we'd otherwise show).
                    b't' => {
                        if nums.len() >= 4 {
                            let rgb = [
                                nums[1].min(255) as u8,
                                nums[2].min(255) as u8,
                                nums[3].min(255) as u8,
                            ];
                            match nums[0] {
                                0 => bg_rgb = Some(rgb),
                                1 => fg_rgb = Some(rgb),
                                _ => {}
                            }
                        }
                    }
                    // Cursor moves (the `1`-default, clamped). ANSI art leans on these
                    // heavily — up/back/forward overlay half-blocks to build the image.
                    b'C' => x += nums.first().copied().unwrap_or(1).max(1) as usize,
                    b'D' => {
                        x = x.saturating_sub(nums.first().copied().unwrap_or(1).max(1) as usize)
                    }
                    b'A' => {
                        y = y.saturating_sub(nums.first().copied().unwrap_or(1).max(1) as usize)
                    }
                    b'B' => y += nums.first().copied().unwrap_or(1).max(1) as usize,
                    // CHA (G) / VPA (d): absolute column / row (1-based).
                    b'G' => x = (nums.first().copied().unwrap_or(1).max(1) - 1) as usize,
                    b'd' => y = (nums.first().copied().unwrap_or(1).max(1) - 1) as usize,
                    b's' => saved = (x, y),
                    b'u' => (x, y) = saved,
                    b'H' | b'f' => {
                        y = (nums.first().copied().unwrap_or(1).max(1) - 1) as usize;
                        x = (nums.get(1).copied().unwrap_or(1).max(1) - 1) as usize;
                    }
                    // Erase line: 0 = cursor→EOL (default), 1 = start→cursor, 2 = whole.
                    b'K' => {
                        if let Some(row) = grid.get_mut(y) {
                            match nums.first().copied().unwrap_or(0) {
                                1 => row.iter_mut().take(x + 1).for_each(|c| *c = BLANK),
                                2 => row.clear(),
                                _ => row.truncate(x),
                            }
                        }
                    }
                    // Clear screen (ESC[2J) — restart the grid at the origin.
                    b'J' if nums.first().copied() == Some(2) => {
                        grid.clear();
                        x = 0;
                        y = 0;
                        max_y = 0; // a clear restarts the screen extent
                    }
                    _ => {}
                }
                i = j + 1;
            }
            // A bare or not-yet-complete ESC (e.g. the last byte of a baud-playback
            // prefix, mid-sequence): consume it silently like ansilove, instead of
            // drawing CP437 0x1B (a ← arrow) that flickers at the typing cursor.
            0x1B => i += 1,
            0x09 => {
                // Tab → next 8-column stop.
                x = (x / 8 + 1) * 8;
                i += 1;
            }
            0x0A => {
                y += 1;
                x = 0;
                i += 1;
            }
            0x0D => {
                x = 0;
                i += 1;
            }
            // SUB (0x1A) = DOS end-of-file: stop rendering here, like ansilove. Scene art
            // appends SAUCE (and sometimes a stray run of 0x1A) after it; without this the
            // 0x1A renders as its CP437 glyph (→) at the end of the art.
            0x1A => break,
            ch => {
                if x < MAX_COLS {
                    // Resolve to RGB. A 24-bit override (`*_rgb`) wins; otherwise the
                    // 16-color index, with bold brightening fg and (iCE) blink brightening
                    // bg. Reverse swaps the two resolved colors.
                    let efg =
                        fg_rgb.unwrap_or(PALETTE[(if bold { fg | 8 } else { fg } & 0x0f) as usize]);
                    let ebg = bg_rgb.unwrap_or(
                        PALETTE[(if ice && blink { bg | 8 } else { bg } & 0x0f) as usize],
                    );
                    let (cfg, cbg) = if reverse { (ebg, efg) } else { (efg, ebg) };
                    ensure(&mut grid, y, x);
                    grid[y][x] = Cell {
                        ch,
                        fg: cfg,
                        bg: cbg,
                    };
                }
                x += 1;
                i += 1;
            }
        }
        max_y = max_y.max(y); // furthest-down the cursor has reached
    }
    max_y = max_y.max(y);
    (grid, max_y + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_dimensions() {
        // "AB\nC" -> widest row 2 cols -> 16px wide; \n started a 2nd row. Height is the
        // padded 25-row screen (16×400); the width is what this test pins down.
        let img = AnsiDecoder.decode(b"AB\nC").unwrap();
        assert_eq!((img.width, img.height), (16, 25 * 16));
    }

    #[test]
    fn cursor_up_and_back_reposition() {
        // ESC[A moves up a row: 'B' overwrites 'A' on row 0 (the half-block overlay
        // technique these 1994 pieces rely on — was previously ignored → scattered).
        let (g, _) = parse(b"A\n\x1b[AB", WRAP, true);
        assert_eq!(g[0][0].ch, b'B', "ESC[A returned to row 0");
        // ESC[2D steps the cursor back two columns: 'C' overwrites 'A' on col 0.
        let (g, _) = parse(b"AB\x1b[2DC", WRAP, true);
        assert_eq!(g[0][0].ch, b'C', "ESC[2D moved back to col 0");
    }

    #[test]
    fn font_8x8_detection_matches_mode_not_codepage() {
        assert!(font_is_8x8("IBM VGA50"));
        assert!(font_is_8x8("IBM VGA50 437"));
        assert!(font_is_8x8("IBM EGA43"));
        assert!(!font_is_8x8("IBM VGA")); // 8×16
        assert!(!font_is_8x8("IBM VGA 850")); // codepage 850, still 8×16
        assert!(!font_is_8x8("")); // no SAUCE → default 8×16
    }

    #[test]
    fn vga50_sauce_renders_at_8px_rows() {
        // One char "A" + a SAUCE record naming IBM VGA50 → an 8×8 cell, not 8×16.
        let mut file = b"A".to_vec();
        let mut s = vec![0u8; 128];
        s[..7].copy_from_slice(b"SAUCE00");
        s[94] = 1; // Character
        s[95] = 1; // ANSi
        s[96] = 1; // width = 1
        s[106..115].copy_from_slice(b"IBM VGA50");
        file.extend_from_slice(&s);
        let img = AnsiDecoder.decode(&file).unwrap();
        // 1 col → 8px wide; the 8×8 cell shows as a 25-row screen of 8px rows (200),
        // vs the 8×16 default which would be 25×16 = 400. Height distinguishes the cell.
        assert_eq!(
            (img.width, img.height),
            (8, 25 * 8),
            "VGA50 → 8px-tall cell"
        );
    }

    #[test]
    fn full_width_line_plus_newline_has_no_blank_row() {
        // A line of exactly `wrap` chars followed by CRLF must advance ONE row, not two:
        // the CR/LF is excluded from the margin pre-wrap so the newline alone steps the
        // row. Otherwise a full-width piece (every row filled to the margin then CRLF)
        // renders a blank row between every content row — venetian-blind black stripes.
        // Matches ansilove (wrap lives in the printable-char branch). wrap=4.
        let (g, _) = parse(b"AAAA\r\nB", 4, true);
        assert_eq!(
            g.len(),
            2,
            "4 chars + CRLF + B = rows 0,1 with no blank between"
        );
        assert_eq!(
            g[0].iter().filter(|c| c.ch != 0).count(),
            4,
            "row 0 stayed full"
        );
        assert_eq!(g[1][0].ch, b'B', "B on row 1, directly under the full row");
    }

    #[test]
    fn parse_reports_cursor_extent_past_blank_lines() {
        // The cursor moves onto blank lines via trailing newlines; cursor_rows counts
        // them (grid.len() doesn't) so baud auto-scroll can reach the final blank line.
        let (g, rows) = parse(b"AB\n\n\n", 80, true);
        assert_eq!(g.len(), 1, "only row 0 was written");
        assert_eq!(rows, 4, "cursor reached row 3 (3 newlines) → 4 rows");
    }

    #[test]
    fn text_stream_prefix_keeps_a_stable_canvas() {
        // Baud playback renders byte prefixes; every frame must be the SAME size as the
        // full image (sized from the whole file), so the view doesn't jump as it types.
        let s = TextStream::new(b"AB\nCD\nEF").unwrap();
        let full = s.render(s.len());
        let empty = s.render(0);
        let mid = s.render(3);
        assert_eq!((empty.width, empty.height), (full.width, full.height));
        assert_eq!((mid.width, mid.height), (full.width, full.height));
        // A zero-byte prefix is an all-background frame; the full one has glyph pixels.
        let lit = |im: &PixImage| im.pixels.iter().filter(|p| **p != [0, 0, 0, 255]).count();
        assert_eq!(lit(&empty), 0, "nothing typed yet");
        assert!(lit(&full) > lit(&mid), "more bytes → more drawn");
    }

    #[test]
    fn bare_esc_is_consumed_not_drawn() {
        // A lone/trailing ESC (e.g. a baud-playback prefix cut mid-sequence) must not
        // render as CP437 0x1B (a ← arrow) flickering at the cursor — ansilove eats it.
        let (g, _) = parse(b"AB\x1b", 80, true);
        assert_eq!(
            g[0].iter().filter(|c| c.ch != 0).count(),
            2,
            "only A and B, no ←"
        );
        // ESC followed by a non-CSI byte: ESC is dropped, the next byte still prints.
        let (g, _) = parse(b"A\x1bZ", 80, true);
        let chars: Vec<u8> = g[0].iter().filter(|c| c.ch != 0).map(|c| c.ch).collect();
        assert_eq!(chars, vec![b'A', b'Z']);
    }

    #[test]
    fn stops_rendering_at_the_dos_eof() {
        // SUB (0x1A) ends the art; bytes after it (a trailing run + SAUCE) aren't drawn,
        // so the EOF never shows up as its CP437 glyph (→). gj-9703c.ans has `…\x1a\x1aSAUCE`.
        let (g, _) = parse(b"AB\x1a\x1aXY", 80, true);
        assert_eq!(g.len(), 1, "only the row before the EOF");
        assert_eq!(g[0].iter().filter(|c| c.ch != 0).count(), 2, "just A and B");
    }

    #[test]
    fn sauce_width_pads_canvas_to_the_full_screen() {
        // An 80-col SAUCE ANSI whose art reaches only column 3 still renders 80 columns
        // wide (the declared screen), not cropped to the content bbox — like ansilove.
        let mut file = b"abc".to_vec();
        let mut s = vec![0u8; 128];
        s[..7].copy_from_slice(b"SAUCE00");
        s[94] = 1; // Character
        s[95] = 1; // ANSi
        s[96] = 80; // TInfo1 width = 80
        file.extend_from_slice(&s);
        let img = AnsiDecoder.decode(&file).unwrap();
        assert_eq!(img.width, 80 * 8, "padded to the full 80-column canvas");

        // With no declared width, stay at the content width (don't force 80).
        let bare = AnsiDecoder.decode(b"abc").unwrap();
        assert_eq!(bare.width, 3 * 8, "no SAUCE width → content width");
    }

    #[test]
    fn wide_sauce_width_is_not_clamped() {
        // "Wide" ANSI (Mistigris party pieces) declares hundreds of columns via SAUCE
        // TInfo1 — THE_BIG_PIRANHA.ANS is 800. It must render 800 cells wide, not auto-wrap
        // at a narrow cap (an old MAX_COLS=300 reflowed it into scrambled noise). Regression
        // guard for the cap bump — the SAUCE width has to survive `wrap`'s clamp.
        let mut file = b"X".to_vec();
        let mut s = vec![0u8; 128];
        s[..7].copy_from_slice(b"SAUCE00");
        s[94] = 1; // Character
        s[95] = 1; // ANSi
        s[96] = 0x20; // TInfo1 low byte
        s[97] = 0x03; // TInfo1 high byte → 0x0320 = 800 columns
        file.extend_from_slice(&s);
        let img = AnsiDecoder.decode(&file).unwrap();
        assert_eq!(
            img.width,
            800 * 8,
            "800-col SAUCE width renders full, not clamped to 300"
        );
    }

    #[test]
    fn corrupt_sauce_width_does_not_explode_the_canvas() {
        // ACIDVIEW.TXT: a buggy 1998 SAUCE writer left a space (0x20) in the high byte of
        // TInfo1, so 80 columns reads as 0x2050 = 8272. With 1296 rows that's a ~6 GB canvas
        // that froze the app on open. The decoder must fall back to the real content width
        // once the declared width would blow the cell budget, keeping the canvas bounded.
        let mut file = b"XXXXXXXXXX\r\n".repeat(130); // 130 rows, 10 written columns
        let mut s = vec![0u8; 128];
        s[..7].copy_from_slice(b"SAUCE00");
        s[94] = 1; // Character
        s[95] = 0; // ASCII
        s[96] = 0x50; // TInfo1 low byte = 80…
        s[97] = 0x20; // …high byte a space → 0x2050 = 8272 (the corruption)
        file.extend_from_slice(&s);
        let img = AnsiDecoder.decode(&file).unwrap();
        // Width collapses to the real content (10 cols), NOT the bogus 8272.
        assert_eq!(
            img.width,
            10 * 8,
            "bogus 8272-col width fell back to the content width"
        );
        assert!(
            (img.width as usize) * (img.height as usize) <= MAX_CANVAS_CELLS * FONT_W * FONT_H,
            "canvas stays within the cell budget"
        );
    }

    #[test]
    fn short_art_pads_to_a_full_25_row_screen() {
        // A few lines of art should still fill a 25-row screen — trailing blank rows
        // are trimmed during parse, so without padding it would crop to < a screen.
        let img = AnsiDecoder.decode(b"hello\r\nworld").unwrap();
        assert_eq!(img.height, 25 * 16, "padded up to a 25-row screen");

        // Taller art isn't shrunk. 30 "X" lines each + CRLF leaves the cursor on row 31
        // (the last newline's blank line); the canvas covers that cursor extent so baud
        // auto-scroll can reach the final blank line.
        let tall = b"X\r\n".repeat(30);
        let img = AnsiDecoder.decode(&tall).unwrap();
        assert_eq!(
            img.height,
            31 * 16,
            "30 content rows + the trailing cursor row"
        );
    }

    #[test]
    fn nine_dot_cell_replicates_column_8_only_for_line_draw() {
        // Columns 0..8 always read the glyph.
        assert!(dot_on(0b1000_0000, 0, b'A'), "MSB lights column 0");
        assert!(dot_on(0b0000_0001, 7, b'A'), "LSB lights column 7");
        // The 9th dot (rx=8) is background for an ordinary glyph like 'A'…
        assert!(
            !dot_on(0b0000_0001, 8, b'A'),
            "9th dot is blank off the line-draw range"
        );
        // …but repeats column 8 for the box/line-draw range 0xC0..=0xDF, so a
        // horizontal rule (0xC4 '─', whose lit rows end in a set LSB) connects.
        assert!(
            dot_on(0b0000_0001, 8, 0xC4),
            "9th dot repeats col 8 for 0xC4"
        );
        assert!(
            !dot_on(0b0000_0000, 8, 0xC4),
            "…but only when col 8 was lit"
        );
        // Boundaries of the line-draw range.
        assert!(dot_on(0b0000_0001, 8, 0xC0));
        assert!(dot_on(0b0000_0001, 8, 0xDF));
        assert!(
            !dot_on(0b0000_0001, 8, 0xBF),
            "just below the range stays blank"
        );
        assert!(
            !dot_on(0b0000_0001, 8, 0xE0),
            "just above the range stays blank"
        );
    }

    #[test]
    fn nine_dot_cell_widens_the_8x8_font_too() {
        // The 9-dot cell is font-independent, so an 8×8 (VGA50/EGA43) grid renders 9px-
        // wide cells (→ 9×8) exactly like the 8×16 font (→ 9×16). `render_grid` takes the
        // cell width explicitly, so this exercises the 9×8 combination directly without
        // flipping the process-global `font_9px` flag — a parallel test asserting an 8px
        // VGA50 width would otherwise race on that shared atomic.
        let grid = vec![vec![Cell {
            ch: b'A',
            fg: PALETTE[7],
            bg: PALETTE[0],
        }]];
        let img = render_grid(&grid, 1, 1, 8, 9);
        assert_eq!(
            (img.width, img.height),
            (9, 8),
            "1 col → 9-dot-wide, 8px-tall cell"
        );
    }

    #[test]
    fn overflow_without_newline_still_wraps() {
        // No newline: the 5th char wraps to row 1 (deferred wrap still happens).
        let (g, _) = parse(b"AAAAB", 4, true);
        assert_eq!(g.len(), 2);
        assert_eq!(g[1][0].ch, b'B');
    }

    #[test]
    fn save_at_right_margin_wraps_before_saving() {
        // Fill the row to the margin (wrap=4 → "AAAA" parks at col 4), then ESC[s. The
        // wrap must fire *before* the save (like ansilove), so it captures the wrapped
        // (0,1) position — not the parked (4,0). Otherwise cursor-addressed art that does
        // `…fill row…[s\r\n[u…` shears (ACID-RN.ANS, gj-os.ans). ESC[u then 'B' → (0,1).
        let (g, _) = parse(b"AAAA\x1b[s\x1b[uB", 4, true);
        assert_eq!(g.len(), 2, "cursor wrapped to row 1 before the save");
        assert_eq!(
            g[1][0].ch, b'B',
            "save/restore captured the wrapped position"
        );
        assert_eq!(
            g[0].iter().filter(|c| c.ch != 0).count(),
            4,
            "row 0 stayed full"
        );
    }

    #[test]
    fn tab_advances_to_next_8col_stop() {
        // "A\tB": A at col 0, tab → col 8, B at col 8.
        let (g, _) = parse(b"A\tB", WRAP, true);
        assert_eq!(g[0][0].ch, b'A');
        assert_eq!(g[0][8].ch, b'B', "tab jumped to column 8");
    }

    #[test]
    fn sgr_sets_foreground_red() {
        // Full-block (0xDB) in red on default bg -> the whole 8×16 cell is red.
        let img = AnsiDecoder.decode(b"\x1b[31m\xDB").unwrap();
        assert_eq!((img.width, img.height), (8, 25 * 16)); // 1 cell wide, 25-row screen
        assert_eq!(img.pixels[0], [170, 0, 0, 255]); // red
    }

    #[test]
    fn extended_color_is_exact_rgb() {
        // Truecolor is now stored exactly (24-bit), not folded to the nearest VGA color.
        let img = AnsiDecoder.decode(b"\x1b[38;2;255;0;0m\xDB").unwrap();
        assert_eq!(img.pixels[0], [255, 0, 0, 255]);
        // 256-color index 9 is bright red exactly.
        let img = AnsiDecoder.decode(b"\x1b[38;5;9m\xDB").unwrap();
        assert_eq!(img.pixels[0], [255, 85, 85, 255]);
    }

    #[test]
    fn pablodraw_24bit_t_sequence() {
        // ESC[1;r;g;b t sets a 24-bit foreground; ESC[0;r;g;b t the background.
        // (Blocktronics "tweaked palette" art, e.g. B-SiDES iNFO.ans.)
        let img = AnsiDecoder.decode(b"\x1b[1;0;23;1t\xDB").unwrap();
        assert_eq!(img.pixels[0], [0, 23, 1, 255], "fg = exact 24-bit RGB");
        let img = AnsiDecoder.decode(b"\x1b[0;187;249;255t ").unwrap();
        assert_eq!(img.pixels[0], [187, 249, 255, 255], "bg = exact 24-bit RGB");
    }

    #[test]
    fn reverse_swaps_fg_and_bg() {
        // A space (all-bg glyph) in reversed red -> the cell fills with the fg color.
        let img = AnsiDecoder.decode(b"\x1b[7;31m ").unwrap();
        assert_eq!(
            img.pixels[0],
            [170, 0, 0, 255],
            "reversed space shows fg (red)"
        );
    }

    #[test]
    fn ice_blink_brightens_background() {
        // iCE on (no SAUCE default): blink turns the blue bg bright-blue on a space.
        let img = AnsiDecoder.decode(b"\x1b[5;44m ").unwrap();
        assert_eq!(img.pixels[0], [85, 85, 255, 255], "iCE blink -> bright bg");
    }

    #[test]
    fn bold_off_resets_intensity() {
        // ESC[1m (bright) then ESC[22m (normal): red stays normal red, not bright.
        let img = AnsiDecoder.decode(b"\x1b[1;31m\x1b[22m\xDB").unwrap();
        assert_eq!(img.pixels[0], [170, 0, 0, 255], "22 turned bold off");
    }

    #[test]
    fn sauce_reports_ice_and_width() {
        // Minimal SAUCE: datatype=Character(1), TInfo1 width=44, TFlags iCE bit set.
        let mut s = vec![0u8; 128];
        s[..7].copy_from_slice(b"SAUCE00");
        s[94] = 1; // datatype = Character
        s[96] = 44; // TInfo1 low byte = width 44
        s[105] = 0x01; // TFlags iCE bit
        let (ice, width, _font) = read_sauce(&s);
        assert!(ice, "iCE flag read");
        assert_eq!(width, Some(44), "TInfo1 width read");
        // No SAUCE -> iCE defaults on, width unknown, no font name.
        assert_eq!(read_sauce(b"hi"), (true, None, String::new()));
    }

    #[test]
    fn bright_via_bold() {
        // bold + red (31) -> bright red (palette 9).
        let img = AnsiDecoder.decode(b"\x1b[1;31m\xDB").unwrap();
        assert_eq!(img.pixels[0], [255, 85, 85, 255]);
    }

    #[test]
    fn strips_sauce_record() {
        // "A" + DOS EOF + a 128-byte SAUCE record (0 comments) → only "A" renders.
        let mut data = b"A".to_vec();
        data.push(0x1A);
        let mut sauce = vec![0u8; 128];
        sauce[..7].copy_from_slice(b"SAUCE00");
        data.extend_from_slice(&sauce);
        let img = AnsiDecoder.decode(&data).unwrap();
        // Width 8 (one col) proves the SAUCE text wasn't rendered; height is the padded
        // 25-row screen (had the record leaked in, the canvas would be far wider).
        assert_eq!((img.width, img.height), (8, 25 * 16));
    }

    /// Dev harness (ignored): decode `ANSI_FILE` and write /tmp/ansi_out.png.
    /// Run: `ANSI_FILE=/path/x.ans cargo test ansi::tests::dump_ansi -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn dump_ansi() {
        let path = std::env::var("ANSI_FILE").expect("set ANSI_FILE");
        set_font_9px(std::env::var("FONT_9PX").is_ok()); // FONT_9PX=1 → 9-dot cell
        let bytes = std::fs::read(&path).unwrap();
        // PREFIX_PCT=NN renders the first NN% of bytes (baud-playback frame check).
        let img = match std::env::var("PREFIX_PCT")
            .ok()
            .and_then(|p| p.parse::<usize>().ok())
        {
            Some(pct) => {
                let s = TextStream::new(&bytes).unwrap();
                s.render(s.len() * pct.min(100) / 100)
            }
            None => AnsiDecoder.decode(&bytes).unwrap(),
        };
        let mut rgba = Vec::with_capacity((img.width * img.height * 4) as usize);
        for px in &img.pixels {
            rgba.extend_from_slice(px);
        }
        use image::ImageEncoder;
        let mut buf = Vec::new();
        image::codecs::png::PngEncoder::new(&mut buf)
            .write_image(
                &rgba,
                img.width,
                img.height,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        std::fs::write("/tmp/ansi_out.png", buf).unwrap();
        eprintln!("wrote /tmp/ansi_out.png {}x{}", img.width, img.height);
    }
}
