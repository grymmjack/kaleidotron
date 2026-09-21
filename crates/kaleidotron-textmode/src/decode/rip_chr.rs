//! BGI stroke fonts — the RIP scalable text fonts 1–10 (Triplex, Small, Sans Serif,
//! Gothic, Script, Simplex, Triplex Script, Complex, European, Bold).
//!
//! Each `.CHR` is a Borland vector font: a header, a per-character offset/width table,
//! then per-character *stroke* lists (MoveTo/LineTo in a 6-bit signed coordinate
//! space). We parse them into glyph stroke lists and render text by emitting line
//! segments — the caller draws them with the same pixel-exact `line()` the rest of
//! `rip.rs` uses. Format + size-scale tables ported from icy_engine's BGI renderer;
//! the `.CHR` data is the standard Borland set (redistributed under MIT/Apache).

use std::sync::OnceLock;

/// BGI char-size (1..=10) → magnification = `UP[size] / DOWN[size]` (size 4 ≈ 1:1).
const UP: [i32; 11] = [1, 6, 2, 3, 1, 4, 5, 2, 5, 3, 4];
const DOWN: [i32; 11] = [1, 10, 3, 4, 1, 3, 3, 1, 2, 1, 1];

struct Stroke {
    line: bool, // true = LineTo, false = MoveTo
    x: i32,
    y: i32,
}

struct Glyph {
    strokes: Vec<Stroke>,
    width: i32,
}

pub struct ChrFont {
    glyphs: Vec<Option<Glyph>>, // indexed directly by char code
    height: i32,
}

/// Minimal little-endian byte cursor (avoids a byteorder dep).
struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}
impl<'a> Rd<'a> {
    fn u8(&mut self) -> u8 {
        let v = self.b.get(self.p).copied().unwrap_or(0);
        self.p += 1;
        v
    }
    fn u16(&mut self) -> u16 {
        u16::from(self.u8()) | (u16::from(self.u8()) << 8)
    }
    fn i8(&mut self) -> i8 {
        self.u8() as i8
    }
}

impl ChrFont {
    fn parse(buf: &[u8]) -> Option<ChrFont> {
        let mut r = Rd { b: buf, p: 0 };
        // Header text terminated by 0x1A.
        while r.p < buf.len() && r.u8() != 0x1A {}
        let header_size = r.u16() as usize;
        r.p += 4; // name[4]
        let _font_size = r.u16();
        r.p += 4; // version bytes
        if header_size >= buf.len() {
            return None;
        }
        r.p = header_size;
        let _sig = r.u8();
        let count = r.u16() as usize;
        r.u8(); // unused
        let first = r.u8() as usize;
        let _char_offset = r.u16();
        let _scan_flag = r.u8();
        let org_to_cap = r.i8();
        let _org_to_base = r.i8();
        let org_to_dec = r.i8();
        r.p += 5; // short name[4] + unused

        let offsets: Vec<u16> = (0..count).map(|_| r.u16()).collect();
        let widths: Vec<u8> = (0..count).map(|_| r.u8()).collect();
        let start = r.p;

        let mut glyphs: Vec<Option<Glyph>> = (0..first).map(|_| None).collect();
        for i in 0..count {
            let mut sr = Rd {
                b: buf,
                p: start + offsets[i] as usize,
            };
            let mut strokes = Vec::new();
            loop {
                if sr.p + 1 >= buf.len() {
                    break;
                }
                let b1 = sr.u8();
                let b2 = sr.u8();
                let f1 = b1 & 0x80 != 0;
                let f2 = b2 & 0x80 != 0;
                let dec6 = |b: u8| {
                    if b & 0x40 != 0 {
                        -((!b & 0x3F) as i32) - 1
                    } else {
                        (b & 0x3F) as i32
                    }
                };
                if !f1 && !f2 {
                    break; // End of glyph
                }
                strokes.push(Stroke {
                    line: f1 && f2,
                    x: dec6(b1),
                    y: dec6(b2),
                });
            }
            glyphs.push(Some(Glyph {
                strokes,
                width: widths[i] as i32,
            }));
        }
        Some(ChrFont {
            glyphs,
            height: (org_to_cap.unsigned_abs() as i32) + (org_to_dec.unsigned_abs() as i32),
        })
    }

    /// The pixel height of this font at BGI size 1..=10.
    pub fn scaled_height(&self, size: usize) -> i32 {
        let size = size.clamp(1, 10);
        self.height * UP[size] / DOWN[size]
    }

    /// Render `s` from (x,y) with `dir` (0=horizontal, 1=vertical) at BGI size 1..=10,
    /// calling `emit(x0,y0,x1,y1)` for each line segment. Returns the advanced x.
    pub fn draw(
        &self,
        s: &str,
        x: i32,
        y: i32,
        dir: u16,
        size: usize,
        emit: &mut impl FnMut(i32, i32, i32, i32),
    ) -> i32 {
        let size = size.clamp(1, 10);
        let (up, down) = (UP[size], DOWN[size]);
        let sc = |v: i32| v * up / down;
        let height = sc(self.height);
        let vertical = dir == 1;
        // The pen advances **along the text axis**: x rightward for horizontal text, y
        // *downward* for vertical — so a vertical label stacks top-to-bottom from its
        // origin (PabloDraw's RIP convention: MAIN/MSG's "The Far Side BBS" starts at the
        // top and reads down). The cross-axis (the passed `x` for vertical, `y` for
        // horizontal) stays fixed so the glyphs form a column. Advancing `pen_x`
        // regardless of direction was the bug that laid vertical text out as overlapping
        // rotated glyphs in a row.
        let mut pen = if vertical { y } else { x };
        for ch in s.bytes() {
            let Some(Some(g)) = self.glyphs.get(ch as usize) else {
                pen += sc(8); // unknown glyph → advance a nominal cell
                continue;
            };
            let (ox, oy) = if vertical { (x, pen) } else { (pen, y) };
            let (mut cx, mut cy) = (ox, oy);
            for st in &g.strokes {
                let (nx, ny) = if vertical {
                    (ox + sc(st.y), oy + sc(st.x))
                } else {
                    (ox + sc(st.x), oy + height - sc(st.y))
                };
                if st.line {
                    emit(cx, cy, nx, ny);
                }
                cx = nx;
                cy = ny;
            }
            pen += sc(g.width);
        }
        // Callers treat the return as the advanced x; vertical text leaves x fixed.
        if vertical {
            x
        } else {
            pen
        }
    }
}

/// The RIP scalable font for number `n` (1..=10), parsed once and cached. Font 0 (the
/// default 8×8 bitmap) is handled directly in `rip.rs`, so it returns None here.
pub fn font(n: usize) -> Option<&'static ChrFont> {
    static FONTS: OnceLock<Vec<Option<ChrFont>>> = OnceLock::new();
    // Order matches the RIP font numbers 1..=10 (see the spec's font table).
    let raw: [&[u8]; 10] = [
        include_bytes!("rip_chr/TRIP.CHR"), // 1  Triplex
        include_bytes!("rip_chr/LITT.CHR"), // 2  Small
        include_bytes!("rip_chr/SANS.CHR"), // 3  Sans Serif
        include_bytes!("rip_chr/GOTH.CHR"), // 4  Gothic
        include_bytes!("rip_chr/SCRI.CHR"), // 5  Script
        include_bytes!("rip_chr/SIMP.CHR"), // 6  Simplex
        include_bytes!("rip_chr/TSCR.CHR"), // 7  Triplex Script
        include_bytes!("rip_chr/LCOM.CHR"), // 8  Complex
        include_bytes!("rip_chr/EURO.CHR"), // 9  European
        include_bytes!("rip_chr/BOLD.CHR"), // 10 Bold
    ];
    let fonts = FONTS.get_or_init(|| raw.iter().map(|b| ChrFont::parse(b)).collect());
    n.checked_sub(1)
        .and_then(|i| fonts.get(i))
        .and_then(|f| f.as_ref())
}

