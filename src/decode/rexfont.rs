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

impl GlyphFont {
    /// Wrap an 8×8 byte-row font (bit 7 = leftmost pixel), e.g. the C64 / ATASCII / Apple / CP437
    /// ROMs, as a [`GlyphFont`] so it can drive the shared glyph picker + coverage ramp.
    pub fn from_8x8(rows: &[[u8; 8]]) -> GlyphFont {
        let glyphs = rows
            .iter()
            .map(|g| g.iter().map(|&b| b as u32).collect())
            .collect();
        GlyphFont { cell_w: 8, cell_h: 8, glyphs }
    }

    /// Wrap an 8-wide × 16-tall byte-row font (the CP437 VGA font) as a [`GlyphFont`].
    pub fn from_8x16(rows: &[[u8; 16]]) -> GlyphFont {
        let glyphs = rows
            .iter()
            .map(|g| g.iter().map(|&b| b as u32).collect())
            .collect();
        GlyphFont { cell_w: 8, cell_h: 16, glyphs }
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

/// Parse a raw linear bitmap font (TheDraw / FONTRAPTION `.F08` / `.F16`, or any VGA-style ROM
/// dump): 256 glyphs, 8 px wide, `cell_h` rows, one byte per row (bit 7 = leftmost), CP437-ordered
/// and header-less. `None` if the blob is too short for 256 glyphs.
pub fn parse_raw(bytes: &[u8], cell_h: usize) -> Option<GlyphFont> {
    if cell_h == 0 || bytes.len() < 256 * cell_h {
        return None;
    }
    let glyphs = (0..256)
        .map(|g| bytes[g * cell_h..g * cell_h + cell_h].iter().map(|&b| b as u32).collect())
        .collect();
    Some(GlyphFont { cell_w: 8, cell_h, glyphs })
}

/// One bundled font: a display name + its embedded PNG bytes.
pub struct RexFontMeta {
    pub name: &'static str,
    pub bytes: &'static [u8],
}

/// One bundled raw-format font: display name + embedded `.F08`/`.F16` bytes + its cell height.
pub struct RawFontMeta {
    pub name: &'static str,
    pub bytes: &'static [u8],
    pub cell_h: usize,
}

macro_rules! rawfonts {
    ($(($name:literal, $file:literal, $h:literal)),+ $(,)?) => {
        pub const RAWFONTS: &[RawFontMeta] = &[
            $(RawFontMeta {
                name: $name,
                bytes: include_bytes!(concat!("../../assets/rexfonts/raw/", $file)),
                cell_h: $h,
            }),+
        ];
    };
}

// FONTRAPTION bitmap fonts from grymmjack's collection — the GJSCI custom "scientific" glyph sets
// plus a run of classic Amiga / ANSI text fonts (Topaz, mO'sOul, P0T-NOoDLE, MicroKnight, the
// newschool set, …). 8×16 unless noted.
rawfonts! {
    ("GJSCI (GJ)", "gjsci.F16", 16),
    ("GJSCI4 (GJ)", "gjsci4.F16", 16),
    ("GJSCI-4 (GJ)", "gjsci_4.F16", 16),
    ("GJSCI-X (GJ)", "gjsci_x.F16", 16),
    ("GJSCI-X6 (GJ)", "gjsci_x6.F16", 16),
    ("Topaz A1200 8x16", "topaz_a1200.F16", 16),
    ("mO'sOul 8x16", "mosoul.F16", 16),
    ("P0T-NOoDLE 8x16", "p0t_noodle.F16", 16),
    ("MicroKnight 8x16", "microknight.F16", 16),
    ("Donna 8x16", "donna.F16", 16),
    ("Orator 8x16", "orator.F16", 16),
    ("Wiggly 8x16", "wiggly.F16", 16),
    ("Newschool 1 8x16", "newschool_1.F16", 16),
    ("Newschool 2 8x16", "newschool_2.F16", 16),
    ("Newschool 3 8x16", "newschool_3.F16", 16),
    ("Newschool 4 8x16", "newschool_4.F16", 16),
    ("Newschool 5 8x16", "newschool_5.F16", 16),
    ("Newschool HF 8x16", "newschool_hf.F16", 16),
    ("Mini 8x8", "mini.F08", 8),
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

/// The bundled raw-format (`.F08`/`.F16`) fonts, parsed once (lazily).
fn parsed_raw() -> &'static [Option<GlyphFont>] {
    static PARSED: OnceLock<Vec<Option<GlyphFont>>> = OnceLock::new();
    PARSED.get_or_init(|| RAWFONTS.iter().map(|m| parse_raw(m.bytes, m.cell_h)).collect())
}

/// The two synthetic CP437 (DOS/ANSI) fonts appended after the bundled pack, so REXPaint art can
/// use the standard VGA glyphs too. Built from the embedded CP437 ROMs.
fn cp437_8x8() -> &'static GlyphFont {
    static F: OnceLock<GlyphFont> = OnceLock::new();
    F.get_or_init(|| GlyphFont::from_8x8(&crate::decode::cp437_font_8x8::CP437_8X8))
}
fn cp437_8x16() -> &'static GlyphFont {
    static F: OnceLock<GlyphFont> = OnceLock::new();
    F.get_or_init(|| GlyphFont::from_8x16(&crate::decode::cp437_font::CP437_8X16))
}

/// The parsed [`GlyphFont`] at index `i` — the bundled PNG pack, then CP437 8×8 / 8×16, then the
/// bundled raw-format (FONTRAPTION `.F08`/`.F16`) fonts.
pub fn rexfont(i: usize) -> Option<&'static GlyphFont> {
    let n = REXFONTS.len();
    if i < n {
        return parsed().get(i).and_then(|o| o.as_ref());
    }
    match i - n {
        0 => Some(cp437_8x8()),
        1 => Some(cp437_8x16()),
        k => parsed_raw().get(k - 2).and_then(|o| o.as_ref()),
    }
}

/// Number of selectable fonts (the PNG pack + the 2 CP437 fonts + the raw-format pack).
pub fn rexfont_count() -> usize {
    REXFONTS.len() + 2 + RAWFONTS.len()
}

/// Display name of font `i`.
pub fn rexfont_name(i: usize) -> &'static str {
    let n = REXFONTS.len();
    if i < n {
        return REXFONTS[i].name;
    }
    match i - n {
        0 => "CP437 8×8 (DOS)",
        1 => "CP437 8×16 (VGA)",
        k => RAWFONTS.get(k - 2).map(|m| m.name).unwrap_or("?"),
    }
}

// ── Viewer render font ──────────────────────────────────────────────────────────
// A process-global choice for the font the TEXTMODE VIEWER renders `.xp` cells in:
// 0 = the built-in VGA CP437 font (the decoder's default), 1..=REXFONTS.len() = a
// bundled font (index-1). Decoders are stateless (constructed once in the Registry),
// so this global is how the app hands them the user's pick; the app clears its decode
// caches when it changes so the file re-renders.
static VIEWER_FONT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Set the viewer render font: 0 = default VGA, else `rexfont` index + 1.
pub fn set_viewer_font(code: usize) {
    VIEWER_FONT.store(code, std::sync::atomic::Ordering::Relaxed);
}

/// The current viewer-font code (0 = default VGA).
pub fn viewer_font_code() -> usize {
    VIEWER_FONT.load(std::sync::atomic::Ordering::Relaxed)
}

/// The current viewer [`GlyphFont`], or `None` when the default VGA font is selected.
pub fn viewer_font() -> Option<&'static GlyphFont> {
    match viewer_font_code() {
        0 => None,
        n => rexfont(n - 1),
    }
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
        // 24 bundled pack fonts + 2 synthetic CP437 fonts + the raw-format pack.
        assert_eq!(rexfont_count(), REXFONTS.len() + 2 + RAWFONTS.len());
        assert_eq!(rexfont(24).unwrap().cell_h, 8, "CP437 8×8 appended");
        assert_eq!(rexfont(25).unwrap().cell_h, 16, "CP437 8×16 appended");
    }

    #[test]
    fn raw_fonts_parse_to_256_glyphs() {
        for (i, m) in RAWFONTS.iter().enumerate() {
            let f = parsed_raw()[i]
                .as_ref()
                .unwrap_or_else(|| panic!("raw font {:?} failed to parse", m.name));
            assert_eq!(f.glyphs.len(), 256, "{} has 256 glyphs", m.name);
            assert_eq!(f.cell_w, 8, "{} is 8px wide", m.name);
            assert_eq!(f.cell_h, m.cell_h, "{} cell height", m.name);
        }
        // They're reachable through the shared `rexfont` index, after the pack + 2 CP437 fonts.
        let first_raw = REXFONTS.len() + 2;
        assert!(rexfont(first_raw).is_some(), "first raw font is selectable");
        assert_eq!(rexfont_name(first_raw), RAWFONTS[0].name);
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
