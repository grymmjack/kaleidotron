//! Unicode text-art glyphs for the "Ramp" style of the Unicode converter. We rasterize a chosen
//! font for the user-selected ranges (+ extra codepoints) into a [`GlyphFont`], with a parallel
//! `char` list mapping each glyph back to its `char` for real UTF-8 text output. Any codepoint the
//! font can't draw is skipped (no `.notdef` boxes).
//!
//! The font is user-selectable (see [`set_ramp_src`]): the default **Perfect DOS VGA 437 (Nerd
//! Font)** is a pixel-perfect CP437 recreation — its block/shade/box glyphs render crisp at the
//! native 8×16 grid (no smooth-outline "crunch") and match the canonical CP437→Unicode codepoints
//! editors like Moebius use, and its Nerd Font patch carries thousands of icon glyphs. **DejaVu
//! Sans** (bundled) trades crispness for coverage — it has Braille + Geometric Shapes. Any TTF/OTF
//! on disk can also be chosen.

use super::rexfont::GlyphFont;
use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use std::sync::{Arc, Mutex, OnceLock};

/// The bundled default: pixel-perfect CP437 + Nerd Font icons (crisp, but no Braille/Geometric).
pub const PDV_FONT: &[u8] = include_bytes!("../../assets/fonts/PerfectDOSVGA437NerdFontMono.ttf");
/// A bundled fallback with wide Unicode coverage — including Braille + Geometric Shapes.
pub const DEJAVU_FONT: &[u8] = include_bytes!("../../assets/DejaVuSans.ttf");
const CELL_W: usize = 8; // Perfect DOS VGA's native glyph width
const CELL_H: usize = 16;

// Range bit flags (also the checkbox order) + their inclusive codepoint spans. Geometric Shapes and
// Braille render only when the chosen font has them (e.g. DejaVu Sans); the crisp CP437 default
// skips them (no tofu).
pub const R_ASCII: u8 = 1;
pub const R_BOX: u8 = 2;
pub const R_BLOCK: u8 = 4;
pub const R_GEOM: u8 = 8;
pub const R_BRAILLE: u8 = 16;

/// The (name, flag, span) of each selectable range, in checkbox order. Box Drawing + Block Elements
/// are the CP437 line/shade/block glyphs at their standard Unicode codepoints (what Moebius' UTF-8
/// ANS export emits), so the ramp's output pastes cleanly into any terminal / text-art tool.
pub const RANGES: [(&str, u8, (u32, u32)); 5] = [
    ("ASCII", R_ASCII, (0x20, 0x7E)),
    ("Box Drawing", R_BOX, (0x2500, 0x257F)),
    ("Block Elements", R_BLOCK, (0x2580, 0x259F)),
    ("Geometric Shapes", R_GEOM, (0x25A0, 0x25FF)),
    ("Braille", R_BRAILLE, (0x2800, 0x28FF)),
];

/// The chosen ramp font. A process-global (like the REXPaint viewer font) so the app hands it to
/// the stateless converter + picker without threading it through every call.
#[derive(Clone)]
pub enum RampSrc {
    /// Bundled Perfect DOS VGA 437 (Nerd Font) — the crisp CP437 default.
    Pdv,
    /// Bundled DejaVu Sans — wide coverage (Braille, Geometric Shapes).
    DejaVu,
    /// A user-chosen TTF/OTF, already read into memory, tagged by a stable id (path hash) for the
    /// memo cache.
    File(u64, Arc<Vec<u8>>),
}

impl RampSrc {
    fn bytes(&self) -> &[u8] {
        match self {
            RampSrc::Pdv => PDV_FONT,
            RampSrc::DejaVu => DEJAVU_FONT,
            RampSrc::File(_, b) => b,
        }
    }
    /// A stable id for cache keys (distinct per source).
    fn id(&self) -> u64 {
        match self {
            RampSrc::Pdv => 0,
            RampSrc::DejaVu => 1,
            RampSrc::File(h, _) => *h,
        }
    }
}

fn ramp_src() -> &'static Mutex<RampSrc> {
    static SRC: OnceLock<Mutex<RampSrc>> = OnceLock::new();
    SRC.get_or_init(|| Mutex::new(RampSrc::Pdv))
}

/// Set the font the Unicode ramp rasterizes from. The app calls this when the user changes the
/// Unicode font; the single-entry memo below rebuilds on the next `ramp()`.
pub fn set_ramp_src(src: RampSrc) {
    *ramp_src().lock().unwrap() = src;
}

/// The scaled render context for `font_bytes`: the font, its px scale, and the baseline. We scale Y
/// to fill the 16-row cell and squeeze X so a full block spans exactly `CELL_W` (8) px — giving
/// crisp, seam-free block/half-block tiling for pixel fonts, and a sensible fit for outline fonts.
/// `None` if the bytes aren't a valid font.
fn render_ctx(font_bytes: &[u8]) -> Option<(FontRef<'_>, PxScale, f32)> {
    let font = FontRef::try_from_slice(font_bytes).ok()?;
    let sy = CELL_H as f32;
    // The advance of a full block at an unsqueezed size — the intended cell width. Squeeze X so it
    // lands on CELL_W exactly (Perfect DOS VGA renders ~9px at 16px; we want 8). Fall back to the
    // font's own em width when it has no full block.
    let probe = font.as_scaled(PxScale::from(sy));
    let block = font.glyph_id('█');
    let adv = if block.0 != 0 { probe.h_advance(block) } else { probe.h_advance(font.glyph_id('M')) };
    let scale = PxScale { x: sy * CELL_W as f32 / adv.max(1.0), y: sy };
    let baseline = font.as_scaled(scale).ascent().round();
    Some((font, scale, baseline))
}

/// Rasterize a single `char` into one cell's worth of bit-rows (CELL_H rows). Returns `None` when
/// the font has no glyph for it (`.notdef`), so callers skip it rather than emit a tofu box.
fn raster_glyph(font: &FontRef, scale: PxScale, baseline: f32, ch: char) -> Option<Vec<u32>> {
    let id = font.glyph_id(ch);
    if id.0 == 0 && ch != ' ' {
        return None; // no glyph in this font
    }
    let mut rows = vec![0u32; CELL_H];
    let g = id.with_scale_and_position(scale, ab_glyph::point(0.0, baseline));
    if let Some(o) = font.outline_glyph(g) {
        let bb = o.px_bounds();
        o.draw(|x, y, c| {
            let px = bb.min.x as i32 + x as i32;
            let py = bb.min.y as i32 + y as i32;
            if (0..CELL_W as i32).contains(&px) && (0..CELL_H as i32).contains(&py) && c >= 0.5 {
                rows[py as usize] |= 1 << (CELL_W - 1 - px as usize);
            }
        });
    }
    Some(rows)
}

/// Rasterize `font_bytes` for the ranges enabled in `mask`, then append the `extra` codepoints
/// (deduped), into a [`GlyphFont`] (8×16) + parallel `char` list. A leading blank (space) anchors
/// the ramp's light end; codepoints the font can't draw are skipped (no tofu). Public so the app
/// can build a ramp from an arbitrary user font.
pub fn build_ramp(font_bytes: &[u8], mask: u8, extra: &[u32]) -> (GlyphFont, Vec<char>) {
    let mut glyphs: Vec<Vec<u32>> = vec![vec![0u32; CELL_H]]; // glyph 0 = blank
    let mut chars: Vec<char> = vec![' '];
    let Some((font, scale, baseline)) = render_ctx(font_bytes) else {
        return (GlyphFont { cell_w: CELL_W, cell_h: CELL_H, glyphs }, chars);
    };
    let push = |ch: char, glyphs: &mut Vec<Vec<u32>>, chars: &mut Vec<char>| {
        if chars.contains(&ch) {
            return;
        }
        if let Some(rows) = raster_glyph(&font, scale, baseline, ch) {
            glyphs.push(rows);
            chars.push(ch);
        }
    };
    for (_, flag, (a, b)) in RANGES {
        if mask & flag == 0 {
            continue;
        }
        for cp in a..=b {
            if let Some(ch) = char::from_u32(cp) {
                push(ch, &mut glyphs, &mut chars);
            }
        }
    }
    for &cp in extra {
        if let Some(ch) = char::from_u32(cp) {
            push(ch, &mut glyphs, &mut chars);
        }
    }
    (GlyphFont { cell_w: CELL_W, cell_h: CELL_H, glyphs }, chars)
}

/// Rasterize a TTF/OTF into a **CP437-ordered** [`GlyphFont`] (256 glyphs, 8×16): glyph `code` is
/// CP437 codepoint `code` mapped to Unicode. Used by the ASCII converter so it can render/measure
/// through an arbitrary outline font, indexed by the same CP437 codes as its built-in font. Missing
/// glyphs come out blank. Returns `None` if the bytes aren't a valid font.
pub fn build_cp437_font(font_bytes: &[u8]) -> Option<GlyphFont> {
    let (font, scale, baseline) = render_ctx(font_bytes)?;
    let glyphs = (0u16..256)
        .map(|code| {
            let ch = retrofont::tdf::CP437_TO_UNICODE[code as usize];
            raster_glyph(&font, scale, baseline, ch).unwrap_or_else(|| vec![0u32; CELL_H])
        })
        .collect();
    Some(GlyphFont { cell_w: CELL_W, cell_h: CELL_H, glyphs })
}

/// The rasterized ramp font + codepoint list for the current [`RampSrc`], `mask`, and `extra`.
/// Single-entry memo keyed by `(source id, mask, extra)`: the picker calls this every frame, but
/// rebuilds only when the font, ranges, or codepoints change. Returns an `Arc` so the converter,
/// picker, and export all share the one build cheaply.
pub fn ramp(mask: u8, extra: &[u32]) -> Arc<(GlyphFont, Vec<char>)> {
    type Memo = Mutex<Option<(u64, Arc<(GlyphFont, Vec<char>)>)>>;
    static MEMO: OnceLock<Memo> = OnceLock::new();
    let src = ramp_src().lock().unwrap().clone();
    // Cheap order-sensitive key over the source id, the mask, and the extra codepoints.
    let mut key = src.id() ^ (mask as u64).wrapping_mul(0x9E3779B97F4A7C15);
    for (i, cp) in extra.iter().enumerate() {
        key ^= (*cp as u64).wrapping_mul(0x100000001B3).rotate_left(i as u32 % 61 + 1);
    }
    let memo = MEMO.get_or_init(|| Mutex::new(None));
    let mut slot = memo.lock().unwrap();
    if let Some((k, arc)) = slot.as_ref() {
        if *k == key {
            return arc.clone();
        }
    }
    let built = Arc::new(build_ramp(src.bytes(), mask, extra));
    *slot = Some((key, built.clone()));
    built
}

/// Parse a user "codepoints" string into a codepoint list. Accepts, space/comma separated: literal
/// characters (`★♥`), hex codepoints (`2588`, `U+2588`, `0x2588`), and inclusive hex ranges
/// (`2591-2593`). Anything that parses as hex is treated as hex; a lone non-hex char is itself.
pub fn parse_codepoints(s: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let mut push = |cp: u32| {
        if char::from_u32(cp).is_some() && !out.contains(&cp) {
            out.push(cp);
        }
    };
    let strip = |t: &str| -> String {
        t.trim_start_matches("U+")
            .trim_start_matches("u+")
            .trim_start_matches("0x")
            .trim_start_matches("0X")
            .to_string()
    };
    for tok in s.split([' ', ',', '\n', '\t', '\r']).filter(|t| !t.is_empty()) {
        // A range "AAAA-BBBB" (both hex)?
        if let Some((a, b)) = tok.split_once('-') {
            if let (Ok(a), Ok(b)) =
                (u32::from_str_radix(&strip(a), 16), u32::from_str_radix(&strip(b), 16))
            {
                if a <= b && b - a <= 0x2000 {
                    for cp in a..=b {
                        push(cp);
                    }
                    continue;
                }
            }
        }
        // A bare hex codepoint?
        if let Ok(cp) = u32::from_str_radix(&strip(tok), 16) {
            // A single character that also happens to be hex digits (e.g. "abc") — only treat as
            // hex when it carried an explicit prefix or is >1 char; otherwise take the literal char.
            let prefixed = tok.len() != strip(tok).len();
            if prefixed || tok.chars().count() > 1 {
                push(cp);
                continue;
            }
        }
        // Otherwise: each literal character contributes its own codepoint.
        for ch in tok.chars() {
            push(ch as u32);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_range_ramps_blank_to_full() {
        let (font, chars) = build_ramp(PDV_FONT, R_BLOCK, &[]);
        assert_eq!(chars[0], ' ', "glyph 0 is blank");
        assert_eq!(font.coverage(0), 0.0);
        // The full block U+2588 must be (near) solid, so the ramp reaches black.
        let full = chars.iter().position(|&c| c == '\u{2588}').expect("has full block");
        assert!(font.coverage(full) > 0.85, "full block is dense ({})", font.coverage(full));
    }

    #[test]
    fn dejavu_font_has_braille_glyphs() {
        // The bundled DejaVu fallback exists specifically so Braille/Geometric render — the crisp
        // CP437 default skips them. Confirm the fallback actually draws a Braille cell.
        let (font, chars) = build_ramp(DEJAVU_FONT, R_BRAILLE, &[]);
        let dots = chars.iter().position(|&c| c == '\u{283F}').expect("has ⠿");
        assert!(font.coverage(dots) > 0.05, "braille cell has ink");
        // Perfect DOS VGA has no Braille, so its Braille ramp is just the blank glyph.
        let (_, pdv_chars) = build_ramp(PDV_FONT, R_BRAILLE, &[]);
        assert_eq!(pdv_chars.len(), 1, "PDV skips all Braille codepoints");
    }

    #[test]
    fn parse_codepoints_forms() {
        // Hex single, U+ prefix, 0x prefix, and an inclusive range.
        assert_eq!(parse_codepoints("2588"), vec![0x2588]);
        assert_eq!(parse_codepoints("U+2588 0x2591"), vec![0x2588, 0x2591]);
        assert_eq!(parse_codepoints("2591-2593"), vec![0x2591, 0x2592, 0x2593]);
        // Literal single characters (non-hex) contribute their own codepoints; dedup applies.
        assert_eq!(parse_codepoints("★ ♥ ★"), vec!['★' as u32, '♥' as u32]);
        // A lone single character that is also a hex digit is taken literally, not as hex.
        assert_eq!(parse_codepoints("a"), vec!['a' as u32]);
        // Comma separation + mixed.
        assert_eq!(parse_codepoints("2588, ♥"), vec![0x2588, '♥' as u32]);
    }

    #[test]
    fn extra_codepoints_appended_and_deduped() {
        // Extras beyond the ranges are appended; ones already in the set are skipped, and a
        // codepoint the font can't draw is dropped (no tofu). The Nerd Font carries the Powerline
        // separator U+E0B0 but not, say, a music note beyond its set.
        let base = build_ramp(PDV_FONT, R_ASCII, &[]).0.glyphs.len();
        let (font, chars) = build_ramp(
            PDV_FONT,
            R_ASCII,
            &[0xE0B0 /* powerline, present */, 0x41 /* 'A', already in ASCII */],
        );
        assert_eq!(font.glyphs.len(), base + 1, "only the powerline glyph is new");
        assert_eq!(*chars.last().unwrap(), '\u{E0B0}');
    }

    #[test]
    fn cp437_font_from_ttf_maps_codes() {
        // build_cp437_font indexes by CP437 code: code 0x41 is 'A' (ink), 0x20 is space (blank),
        // 0xB0..=0xB2 are the shade blocks (increasing ink). DejaVu covers all of these.
        let f = build_cp437_font(DEJAVU_FONT).expect("valid font");
        assert_eq!(f.glyphs.len(), 256);
        assert!(f.coverage(0x41) > 0.05, "'A' has ink");
        assert_eq!(f.coverage(0x20), 0.0, "space is blank");
        assert!(
            f.coverage(0xB0) < f.coverage(0xB2),
            "light shade ░ is lighter than dark shade ▓"
        );
    }

    #[test]
    fn box_drawing_horizontal_is_a_mid_row_line() {
        let (font, chars) = build_ramp(PDV_FONT, R_BOX, &[]);
        let dash = chars.iter().position(|&c| c == '\u{2500}').expect("has ─"); // light horizontal
        // A horizontal rule: some middle row is (nearly) full width, top/bottom rows empty.
        let cov = font.coverage(dash);
        assert!(cov > 0.03 && cov < 0.4, "─ is a thin line ({cov})");
    }
}
