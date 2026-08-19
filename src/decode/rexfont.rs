//! REXPaint bitmap fonts — GridSage's "REXPaint fonts pack": a set of 16×16 CP437
//! glyph-grid PNGs. Each PNG holds 256 glyphs (glyph N at grid col `N%16`, row `N/16`),
//! and a pixel is "on" when it's light (white-on-black, or opaque non-black for the RGBA
//! sheets). We parse them into a generic [`GlyphFont`] of any cell size, shared by BOTH
//! the char-art converters (as a glyph set) and the textmode viewer (as a render font).

use std::sync::OnceLock;

/// A parsed bitmap font: 256 glyphs on a `cell_w`×`cell_h` grid. Each glyph is `cell_h`
/// rows; in a row, bit `cell_w-1-x` is pixel `x` (so bit-`(cell_w-1)` is the leftmost pixel).
#[derive(Clone)]
pub struct GlyphFont {
    pub cell_w: usize,
    pub cell_h: usize,
    pub glyphs: Vec<Vec<u32>>,
}

impl GlyphFont {
    /// Is pixel (`x`,`y`) of `glyph` set? Out-of-range → false.
    #[inline]
    pub fn on(&self, glyph: usize, x: usize, y: usize) -> bool {
        if x >= self.cell_w {
            return false;
        }
        self.glyphs
            .get(glyph)
            .and_then(|g| g.get(y))
            .map(|row| (row >> (self.cell_w - 1 - x)) & 1 == 1)
            .unwrap_or(false)
    }

    /// Ink coverage (0..1) of `glyph` — the fraction of set pixels. Drives the density ramp.
    pub fn coverage(&self, glyph: usize) -> f32 {
        let total = (self.cell_w * self.cell_h) as f32;
        if total == 0.0 {
            return 0.0;
        }
        let on: u32 = self
            .glyphs
            .get(glyph)
            .map(|g| g.iter().map(|r| r.count_ones()).sum())
            .unwrap_or(0);
        on as f32 / total
    }
}

/// Parse a REXPaint font PNG (16×16 glyph grid) into a [`GlyphFont`]. `None` if the image
/// isn't decodable or its dimensions aren't a clean 16×16 grid.
pub fn parse_rexpaint(bytes: &[u8]) -> Option<GlyphFont> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (iw, ih) = img.dimensions();
    let (iw, ih) = (iw as usize, ih as usize);
    if iw == 0 || ih == 0 || iw % 16 != 0 || ih % 16 != 0 {
        return None;
    }
    let (cw, ch) = (iw / 16, ih / 16);
    if cw > 32 || ch > 32 {
        return None; // a row is a u32 bitmask
    }
    let mut glyphs = Vec::with_capacity(256);
    for gi in 0..256usize {
        let (gx, gy) = ((gi % 16) * cw, (gi / 16) * ch);
        let mut rows = Vec::with_capacity(ch);
        for y in 0..ch {
            let mut bits = 0u32;
            for x in 0..cw {
                let [r, g, b, a] = img.get_pixel((gx + x) as u32, (gy + y) as u32).0;
                // "On" = opaque and light. Covers both white-on-black (opaque) sheets and
                // the RGBA sheets whose background is transparent black.
                if a > 127 && (r as u32 + g as u32 + b as u32) > 128 {
                    bits |= 1 << (cw - 1 - x);
                }
            }
            rows.push(bits);
        }
        glyphs.push(rows);
    }
    Some(GlyphFont { cell_w: cw, cell_h: ch, glyphs })
}

/// One bundled font: a display name + its embedded PNG bytes.
pub struct RexFontMeta {
    pub name: &'static str,
    pub bytes: &'static [u8],
}

macro_rules! rexfonts {
    ($(($name:literal, $file:literal)),+ $(,)?) => {
        pub const REXFONTS: &[RexFontMeta] = &[
            $(RexFontMeta { name: $name, bytes: include_bytes!(concat!("../../assets/rexfonts/", $file)) }),+
        ];
    };
}

rexfonts! {
    ("Aquarius 8x8", "aquarius_8x8.png"),
    ("Ceefax Teletext 6x10", "ceefax_teletext_6x10.png"),
    ("Ceefax Teletext 12x20", "ceefax_teletext_12x20.png"),
    ("Drake 10x10", "drake_10x10.png"),
    ("Galaksija 8x13", "galaksija_8x13.png"),
    ("Gumix CP437 6x6", "gumix_cp437_6x6.png"),
    ("Hitachi MB-6880 8x8", "hitachi_MB-6880_8x8.png"),
    ("Max Brazilian 8x8", "max_brazilian_8x8.png"),
    ("MSX Cyrillic 8x8", "msx_cyrillic_8x8.png"),
    ("Orao 8x8", "orao_8x8.png"),
    ("PETSCII 16x16", "petscii_16x16.png"),
    ("Philips VG-5000 8x10", "philips_vg_5000_8x10.png"),
    ("Pixelcod 8x8", "pixelcod_8x8.png"),
    ("Polyducks 12x12", "polyducks_12x12.png"),
    ("Polyducks Gloop 8x8", "polyducks_gloop_8x8.png"),
    ("Qbicfeet 10x10", "qbicfeet_10x10.png"),
    ("SAM Coupe 8x8", "sam_coupe_8x8.png"),
    ("SGI IRIS 3130 8x16", "sgi_iris_3130_8x16.png"),
    ("SGI IRIS 4D 8x16", "sgi_iris_4d_8x16.png"),
    ("Unifont 8x16", "unifont_8x16.png"),
    ("Unscii 8x16", "unscii_8x16.png"),
    ("ZX81 8x8", "zx81_8x8.png"),
    ("ZX Evolution 8x8", "zx_evolution_8x8.png"),
    ("ZX Spectrum 8x8", "zx_spectrum_8x8.png"),
}

/// The bundled fonts, parsed once (lazily). A font that fails to parse is `None`.
fn parsed() -> &'static [Option<GlyphFont>] {
    static PARSED: OnceLock<Vec<Option<GlyphFont>>> = OnceLock::new();
    PARSED.get_or_init(|| REXFONTS.iter().map(|m| parse_rexpaint(m.bytes)).collect())
}

/// The parsed [`GlyphFont`] at index `i` (into [`REXFONTS`]), or `None`.
pub fn rexfont(i: usize) -> Option<&'static GlyphFont> {
    parsed().get(i).and_then(|o| o.as_ref())
}

/// Number of bundled fonts.
pub fn rexfont_count() -> usize {
    REXFONTS.len()
}

/// Display name of bundled font `i`.
pub fn rexfont_name(i: usize) -> &'static str {
    REXFONTS.get(i).map(|m| m.name).unwrap_or("?")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_bundled_fonts_parse_to_256_glyphs() {
        for (i, m) in REXFONTS.iter().enumerate() {
            let f = rexfont(i).unwrap_or_else(|| panic!("font {:?} failed to parse", m.name));
            assert_eq!(f.glyphs.len(), 256, "{} has 256 glyphs", m.name);
            assert!(f.cell_w >= 4 && f.cell_h >= 4, "{} sane cell", m.name);
        }
        assert_eq!(rexfont_count(), 24);
    }

    #[test]
    fn fonts_span_blank_to_dense() {
        // Not all sheets are CP437-ordered (e.g. Aquarius is a native charset), so check the
        // coverage SPAN rather than specific codes: every font has a (near) blank glyph and a
        // (near) solid one — that's what makes a usable density ramp.
        for i in 0..rexfont_count() {
            let f = rexfont(i).unwrap();
            let (mut lo, mut hi) = (1.0f32, 0.0f32);
            for g in 0..256 {
                let c = f.coverage(g);
                lo = lo.min(c);
                hi = hi.max(c);
            }
            assert!(lo < 0.05, "{} has a blank glyph", rexfont_name(i));
            // A usable ramp needs real range; some native sets (e.g. Galaksija) have no full
            // block, so only require a solidly-filled glyph, not a 100% one.
            assert!(hi > 0.4, "{} has a dense glyph (span {:.2})", rexfont_name(i), hi);
        }
    }
}
