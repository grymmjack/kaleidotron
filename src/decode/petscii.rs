//! PETSCII — Commodore 8-bit scene art (.seq / .pet / .petscii streams).
//!
//! The *parsing* is Mike Krüger's `icy_parser_core::PetsciiParser` (the lightweight,
//! no-tokio parser crate from his icy ecosystem); we drive its `CommandSink` into a
//! 40-column character/colour grid and render that ourselves with the embedded C64
//! character ROM ([`C64_FONT`]) + the VIC-II palette — so PETSCII inherits the same
//! pixel-perfect rendering / device-pixel zoom / area-averaged thumbnails as our
//! other text-mode art. The parser hands us C64 **screen codes** (bit 7 = reverse)
//! via `print`, and colours / cursor moves via `emit`.

use super::c64_font::C64_FONT;
use super::{DecodeError, Decoder};
use crate::image_types::{PixImage, Rgba};
use icy_parser_core::{
    Color, CommandParser, CommandSink, Direction, EraseInDisplayMode, PetsciiParser, SgrAttribute,
    TerminalCommand,
};

const COLS: usize = 40; // the C64 text screen is 40 columns
pub(crate) const CELL: usize = 8; // 8×8 character cells (shared with the petmate decoder)
const BG: u8 = 0; // background colour index (black) — .seq carries no bg colour
const DEFAULT_FG: u8 = 14; // C64 power-on text colour (light blue)
const MAX_ROWS: usize = 1000; // guard against a runaway cursor-down stream

/// VIC-II 16-colour palette — the "C64 Community Colors V1.2a" values icy_engine
/// uses, so our PETSCII colours match Mike's renderer exactly. Index order matches
/// the C64 colour codes the PETSCII control bytes set (0=black, 1=white, 2=red, …).
pub(crate) const VIC2: [Rgba; 16] = [
    [0x00, 0x00, 0x00, 255], //  0 black
    [0xff, 0xff, 0xff, 255], //  1 white
    [0xaf, 0x2a, 0x29, 255], //  2 red
    [0x62, 0xd8, 0xcc, 255], //  3 cyan
    [0xb0, 0x3f, 0xb6, 255], //  4 purple
    [0x4a, 0xc6, 0x4a, 255], //  5 green
    [0x37, 0x39, 0xc4, 255], //  6 blue
    [0xe4, 0xed, 0x4e, 255], //  7 yellow
    [0xb6, 0x59, 0x1c, 255], //  8 orange
    [0x68, 0x38, 0x08, 255], //  9 brown
    [0xea, 0x74, 0x6c, 255], // 10 light red
    [0x4d, 0x4d, 0x4d, 255], // 11 dark grey
    [0x84, 0x84, 0x84, 255], // 12 grey
    [0xa6, 0xfa, 0x9e, 255], // 13 light green
    [0x70, 0x7c, 0xe6, 255], // 14 light blue
    [0xb6, 0xb6, 0xb5, 255], // 15 light grey
];

// ── petmate's four C64 palettes ────────────────────────────────────────────────
// The PETSCII *converter* renders with one of these so the exported art matches
// whatever palette the user has selected in petmate (Preferences → "Select C64 color
// palette"). RGB copied verbatim from petmate's `src/utils/palette.ts`. Index order is
// the C64 colour code order (0 black, 1 white, 2 red, …), same as [`VIC2`].

/// petmate's own default palette (a slightly muted, warm set).
pub const PAL_PETMATE: [Rgba; 16] = [
    [0x00, 0x00, 0x00, 255],
    [0xff, 0xff, 0xff, 255],
    [0x92, 0x4a, 0x40, 255],
    [0x84, 0xc5, 0xcc, 255],
    [0x93, 0x51, 0xb6, 255],
    [0x72, 0xb1, 0x4b, 255],
    [0x48, 0x3a, 0xa4, 255],
    [0xd5, 0xdf, 0x7c, 255],
    [0x99, 0x69, 0x2d, 255],
    [0x67, 0x52, 0x01, 255],
    [0xc0, 0x81, 0x78, 255],
    [0x60, 0x60, 0x60, 255],
    [0x8a, 0x8a, 0x8a, 255],
    [0xb2, 0xec, 0x91, 255],
    [0x86, 0x7a, 0xde, 255],
    [0xae, 0xae, 0xae, 255],
];

/// Pepto's "Colodore" recalculated palette (the community-accurate reference; matches
/// the bundled `C=64 (16).GPL` / `COLODORE (16).GPL`).
pub const PAL_COLODORE: [Rgba; 16] = [
    [0x00, 0x00, 0x00, 255],
    [0xff, 0xff, 0xff, 255],
    [0x81, 0x33, 0x38, 255],
    [0x75, 0xce, 0xc8, 255],
    [0x8e, 0x3c, 0x97, 255],
    [0x56, 0xac, 0x4d, 255],
    [0x2e, 0x2c, 0x9b, 255],
    [0xed, 0xf1, 0x71, 255],
    [0x8e, 0x50, 0x29, 255],
    [0x55, 0x38, 0x00, 255],
    [0xc4, 0x6c, 0x71, 255],
    [0x4a, 0x4a, 0x4a, 255],
    [0x7b, 0x7b, 0x7b, 255],
    [0xa9, 0xff, 0x9f, 255],
    [0x70, 0x6d, 0xeb, 255],
    [0xb2, 0xb2, 0xb2, 255],
];

/// Pepto's original PAL-measured palette.
pub const PAL_PEPTO: [Rgba; 16] = [
    [0x00, 0x00, 0x00, 255],
    [0xff, 0xff, 0xff, 255],
    [0x67, 0x37, 0x2d, 255],
    [0x73, 0xa3, 0xb1, 255],
    [0x6e, 0x3e, 0x83, 255],
    [0x5b, 0x8d, 0x48, 255],
    [0x36, 0x29, 0x76, 255],
    [0xb7, 0xc5, 0x76, 255],
    [0x6c, 0x4f, 0x2a, 255],
    [0x42, 0x39, 0x08, 255],
    [0x98, 0x67, 0x5b, 255],
    [0x44, 0x44, 0x44, 255],
    [0x6c, 0x6c, 0x6c, 255],
    [0x9d, 0xd2, 0x8a, 255],
    [0x6d, 0x5f, 0xb0, 255],
    [0x95, 0x95, 0x95, 255],
];

/// The VICE emulator's default (brighter, higher-contrast) palette.
pub const PAL_VICE: [Rgba; 16] = [
    [0x00, 0x00, 0x00, 255],
    [0xff, 0xff, 0xff, 255],
    [0xb9, 0x6a, 0x54, 255],
    [0xac, 0xf3, 0xfe, 255],
    [0xbe, 0x73, 0xf8, 255],
    [0x9a, 0xe3, 0x5b, 255],
    [0x69, 0x5a, 0xf1, 255],
    [0xff, 0xfd, 0x84, 255],
    [0xc5, 0x91, 0x3c, 255],
    [0x8c, 0x78, 0x17, 255],
    [0xf3, 0xab, 0x98, 255],
    [0x81, 0x81, 0x81, 255],
    [0xb6, 0xb6, 0xb6, 255],
    [0xdc, 0xfe, 0xa3, 255],
    [0xb1, 0xa0, 0xfc, 255],
    [0xe0, 0xe0, 0xe0, 255],
];

/// The PETSCII-converter palette choices, in dropdown order. `.0` is the display name
/// (also the stem of the matching bundled `.GPL`), `.1` the 16 colours in C64-code order.
pub const PETSCII_PALETTES: [(&str, [Rgba; 16]); 4] = [
    ("petmate", PAL_PETMATE),
    ("colodore", PAL_COLODORE),
    ("pepto", PAL_PEPTO),
    ("vice", PAL_VICE),
];

/// The converter palette for dropdown index `i` (clamped), as a fixed 16-colour array.
pub fn petscii_palette(i: u8) -> &'static [Rgba; 16] {
    &PETSCII_PALETTES[(i as usize).min(PETSCII_PALETTES.len() - 1)].1
}

#[derive(Clone, Copy)]
struct Cell {
    glyph: u16, // index into C64_FONT (page*256 + screen-code, bit 7 = reverse)
    fg: u8,     // VIC-II palette index
}

/// The growing 40-column grid the parser writes into.
struct Canvas {
    rows: Vec<[Cell; COLS]>,
    x: usize,
    y: usize,
    fg: u8,
    page: usize, // font page: 0 = uppercase/graphics (PETSCII default), 1 = lowercase
}

impl Canvas {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            x: 0,
            y: 0,
            fg: DEFAULT_FG,
            page: 0,
        }
    }

    fn blank() -> Cell {
        Cell {
            glyph: 0x20,
            fg: BG,
        } // a space
    }

    fn ensure_row(&mut self, y: usize) {
        while self.rows.len() <= y && self.rows.len() < MAX_ROWS {
            self.rows.push([Self::blank(); COLS]);
        }
    }
}

impl CommandSink for Canvas {
    fn print(&mut self, text: &[u8]) {
        for &code in text {
            if self.x >= COLS {
                self.x = 0;
                self.y += 1;
            }
            let (x, y) = (self.x, self.y);
            self.ensure_row(y);
            if let Some(row) = self.rows.get_mut(y) {
                row[x] = Cell {
                    glyph: (self.page * 256 + code as usize) as u16,
                    fg: self.fg,
                };
            }
            self.x += 1;
        }
    }

    fn emit(&mut self, cmd: TerminalCommand) {
        match cmd {
            TerminalCommand::CsiSelectGraphicRendition(SgrAttribute::Foreground(Color::Base(
                c,
            ))) => {
                self.fg = c & 0x0f;
            }
            TerminalCommand::CarriageReturn => self.x = 0,
            TerminalCommand::LineFeed => {
                self.y += 1;
                self.x = 0;
            }
            TerminalCommand::CsiCursorPosition(row, col) => {
                self.y = row.max(1) as usize - 1;
                self.x = (col.max(1) as usize - 1).min(COLS - 1);
            }
            TerminalCommand::CsiMoveCursor(dir, n, _) => {
                let n = n.max(1) as usize;
                match dir {
                    Direction::Up => self.y = self.y.saturating_sub(n),
                    Direction::Down => self.y += n,
                    Direction::Left => self.x = self.x.saturating_sub(n),
                    Direction::Right => self.x = (self.x + n).min(COLS - 1),
                }
            }
            TerminalCommand::CsiEraseInDisplay(EraseInDisplayMode::All) => {
                self.rows.clear();
                self.x = 0;
                self.y = 0;
            }
            TerminalCommand::SetFontPage(p) => self.page = p.min(1),
            _ => {} // bell, underline, insert/delete-line, etc. — not visible here
        }
    }
}

pub struct PetsciiDecoder;

impl Decoder for PetsciiDecoder {
    fn name(&self) -> &'static str {
        "petscii"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["seq", "pet", "petscii"]
    }

    fn sniff(&self, _header: &[u8]) -> bool {
        false // a PETSCII stream has no reliable magic — dispatched by extension
    }

    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        let data = crate::sauce::strip(bytes); // some .seq carry a SAUCE trailer
        let mut canvas = Canvas::new();
        PetsciiParser::new().parse(data, &mut canvas);
        let rows = canvas.rows.len().clamp(1, MAX_ROWS);
        let (w, h) = (COLS * CELL, rows * CELL);

        // Render to a palette-indexed image (VIC-II), preserving the 16-colour palette
        // for the recolor / export pipeline — every pixel is either its cell's fg or BG.
        let mut indices = vec![BG; w * h];
        for (cy, row) in canvas.rows.iter().enumerate() {
            for (cx, cell) in row.iter().enumerate() {
                let glyph = &C64_FONT[cell.glyph as usize % C64_FONT.len()];
                for (ry, &bits) in glyph.iter().enumerate() {
                    for rx in 0..CELL {
                        let on = (bits >> (7 - rx)) & 1 == 1;
                        let px = cx * CELL + rx;
                        let py = cy * CELL + ry;
                        indices[py * w + px] = if on { cell.fg } else { BG };
                    }
                }
            }
        }
        Ok(PixImage::from_indexed(
            w as u32,
            h as u32,
            indices,
            VIC2.to_vec(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_petscii_line() {
        // "HI" then a carriage-return-style newline. Screen codes for H,I are 8,9.
        // PETSCII 'H'=0x48, 'I'=0x49 → internal screen codes 8, 9. 0x0D = CR.
        let img = PetsciiDecoder.decode(b"HI").unwrap();
        // 40 cols × 8 = 320 wide; one row × 8 = 8 tall.
        assert_eq!((img.width, img.height), (320, 8));
        // Palette-preserving (16 VIC-II colours).
        assert!(img.indexed.as_ref().is_some_and(|i| i.palette.len() == 16));
    }

    #[test]
    fn color_code_sets_foreground() {
        // 0x1C = RED foreground, then a reverse-space solid block (0x12 reverse-on,
        // 0xA0 = reverse space). The first lit pixel should be RED (index 2).
        let img = PetsciiDecoder.decode(&[0x1C, 0x12, 0xA0]).unwrap();
        assert!(img.pixels.iter().any(|p| *p == VIC2[2]));
    }
}
