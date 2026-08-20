//! Unicode text-art glyphs for the "Ramp" style of the Unicode converter. We rasterize the
//! **bundled DejaVu Sans Mono** (permissive license, see `assets/fonts/DejaVu-LICENSE.txt`) for
//! the user-selected Unicode ranges into a [`GlyphFont`], so an image can be density-ramped over
//! Box Drawing / Block Elements / Geometric Shapes / Braille / ASCII glyphs. A parallel codepoint
//! list maps each glyph back to its `char` for real UTF-8 text output.

use super::rexfont::GlyphFont;
use ab_glyph::{Font, FontRef, ScaleFont};
use std::borrow::Cow;
use std::sync::OnceLock;

const FONT: &[u8] = include_bytes!("../../assets/fonts/DejaVuSansMono.ttf");
const CELL_W: usize = 9;
const CELL_H: usize = 16;

// Range bit flags (also the checkbox order) + their inclusive codepoint spans.
pub const R_ASCII: u8 = 1;
pub const R_BOX: u8 = 2;
pub const R_BLOCK: u8 = 4;
pub const R_GEOM: u8 = 8;
pub const R_BRAILLE: u8 = 16;

/// The (name, flag, span) of each selectable range, in checkbox order.
pub const RANGES: [(&str, u8, (u32, u32)); 5] = [
    ("ASCII", R_ASCII, (0x20, 0x7E)),
    ("Box Drawing", R_BOX, (0x2500, 0x257F)),
    ("Block Elements", R_BLOCK, (0x2580, 0x259F)),
    ("Geometric Shapes", R_GEOM, (0x25A0, 0x25FF)),
    ("Braille", R_BRAILLE, (0x2800, 0x28FF)),
];

/// The scaled DejaVu render context: the font, its px scale, the baseline, and the x-nudge that
/// centres a full-block in the 9-wide cell. Shared by the range rasterizer and the single-glyph
/// path (used for user codepoints), so both draw at exactly the same metrics.
fn render_ctx() -> (FontRef<'static>, ab_glyph::PxScale, f32, f32) {
    let font = FontRef::try_from_slice(FONT).expect("bundled DejaVu is a valid font");
    // Scale so the em's line height maps to the cell (block glyphs then fill it — the ramp's dark
    // end). The 9-wide cell gives DejaVu's ~8.3px advance a little breathing room so box-drawing
    // and diagonal glyphs don't clip on the right; a small x nudge centres them.
    let probe = font.as_scaled(ab_glyph::PxScale::from(CELL_H as f32));
    let factor = CELL_H as f32 / probe.height().max(1.0);
    let scale = ab_glyph::PxScale::from(CELL_H as f32 * factor);
    let sf = font.as_scaled(scale);
    let baseline = sf.ascent();
    let pen_x = ((CELL_W as f32 - sf.h_advance(font.glyph_id('█'))) * 0.5).max(0.0);
    (font, scale, baseline, pen_x)
}

/// Rasterize a single `char` into one cell's worth of bit-rows (CELL_H rows).
fn raster_glyph(
    font: &FontRef<'static>,
    scale: ab_glyph::PxScale,
    baseline: f32,
    pen_x: f32,
    ch: char,
) -> Vec<u32> {
    let mut rows = vec![0u32; CELL_H];
    let g = font
        .glyph_id(ch)
        .with_scale_and_position(scale, ab_glyph::point(pen_x, baseline));
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
    rows
}

/// Rasterize the codepoints of every range enabled in `mask` into a [`GlyphFont`] (9×16 cells) plus
/// a parallel `char` list. A leading blank (space) anchors the ramp's light end.
fn rasterize(mask: u8) -> (GlyphFont, Vec<char>) {
    let (font, scale, baseline, pen_x) = render_ctx();
    let mut glyphs: Vec<Vec<u32>> = vec![vec![0u32; CELL_H]]; // glyph 0 = blank
    let mut chars: Vec<char> = vec![' '];
    for (_, flag, (a, b)) in RANGES {
        if mask & flag == 0 {
            continue;
        }
        for cp in a..=b {
            let Some(ch) = char::from_u32(cp) else { continue };
            glyphs.push(raster_glyph(&font, scale, baseline, pen_x, ch));
            chars.push(ch);
        }
    }
    (GlyphFont { cell_w: CELL_W, cell_h: CELL_H, glyphs }, chars)
}

/// The ramp font for `mask`, with any user `extra` codepoints appended (deduped against the ranges
/// and each other). Borrows the cached per-mask font when there are no extras (the common case —
/// zero cost); otherwise clones it and rasterizes just the extra glyphs. The result is used
/// transiently by the picker and converter, so an owned value is fine (no unbounded leak).
pub fn ramp_font_owned(mask: u8, extra: &[u32]) -> Cow<'static, (GlyphFont, Vec<char>)> {
    let base = ramp_font(mask);
    if extra.is_empty() {
        return Cow::Borrowed(base);
    }
    let (font, scale, baseline, pen_x) = render_ctx();
    let mut out_font = base.0.clone();
    let mut chars = base.1.clone();
    for &cp in extra {
        let Some(ch) = char::from_u32(cp) else { continue };
        if chars.contains(&ch) {
            continue; // already covered by a range or an earlier extra
        }
        out_font.glyphs.push(raster_glyph(&font, scale, baseline, pen_x, ch));
        chars.push(ch);
    }
    Cow::Owned((out_font, chars))
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

/// The rasterized font + codepoint list for range-`mask`, parsed once per distinct mask (there are
/// only 32 possible masks, so a small cache never grows large).
pub fn ramp_font(mask: u8) -> &'static (GlyphFont, Vec<char>) {
    type Cache = std::sync::Mutex<std::collections::HashMap<u8, &'static (GlyphFont, Vec<char>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut map = cache.lock().unwrap();
    map.entry(mask)
        .or_insert_with(|| Box::leak(Box::new(rasterize(mask))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_range_ramps_blank_to_full() {
        let (font, chars) = ramp_font(R_BLOCK);
        assert_eq!(chars[0], ' ', "glyph 0 is blank");
        assert_eq!(font.coverage(0), 0.0);
        // The full block U+2588 must be (near) solid, so the ramp reaches black.
        let full = chars.iter().position(|&c| c == '\u{2588}').expect("has full block");
        assert!(font.coverage(full) > 0.85, "full block is dense ({})", font.coverage(full));
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
        // Extras beyond the ranges are appended; ones already in the range set are skipped.
        let base = ramp_font(R_ASCII).0.glyphs.len();
        let owned = ramp_font_owned(R_ASCII, &['★' as u32, 0x41 /* 'A', already in ASCII */]);
        let (font, chars) = owned.as_ref();
        assert_eq!(font.glyphs.len(), base + 1, "only the star is new");
        assert_eq!(*chars.last().unwrap(), '★');
    }

    #[test]
    fn box_drawing_horizontal_is_a_mid_row_line() {
        let (font, chars) = ramp_font(R_BOX);
        let dash = chars.iter().position(|&c| c == '\u{2500}').expect("has ─"); // light horizontal
        // A horizontal rule: some middle row is (nearly) full width, top/bottom rows empty.
        let cov = font.coverage(dash);
        assert!(cov > 0.03 && cov < 0.4, "─ is a thin line ({cov})");
    }
}
