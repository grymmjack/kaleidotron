//! Unicode text-art glyphs for the "Ramp" style of the Unicode converter. We rasterize the
//! **bundled DejaVu Sans Mono** (permissive license, see `assets/fonts/DejaVu-LICENSE.txt`) for
//! the user-selected Unicode ranges into a [`GlyphFont`], so an image can be density-ramped over
//! Box Drawing / Block Elements / Geometric Shapes / Braille / ASCII glyphs. A parallel codepoint
//! list maps each glyph back to its `char` for real UTF-8 text output.

use super::rexfont::GlyphFont;
use ab_glyph::{Font, FontRef, ScaleFont};
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

/// Rasterize the codepoints of every range enabled in `mask` into a [`GlyphFont`] (8×16 cells) plus
/// a parallel `char` list. A leading blank (space) anchors the ramp's light end.
fn rasterize(mask: u8) -> (GlyphFont, Vec<char>) {
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

    let mut glyphs: Vec<Vec<u32>> = vec![vec![0u32; CELL_H]]; // glyph 0 = blank
    let mut chars: Vec<char> = vec![' '];
    for (_, flag, (a, b)) in RANGES {
        if mask & flag == 0 {
            continue;
        }
        for cp in a..=b {
            let Some(ch) = char::from_u32(cp) else { continue };
            let mut rows = vec![0u32; CELL_H];
            let g = font
                .glyph_id(ch)
                .with_scale_and_position(scale, ab_glyph::point(pen_x, baseline));
            if let Some(o) = font.outline_glyph(g) {
                let bb = o.px_bounds();
                o.draw(|x, y, c| {
                    let px = bb.min.x as i32 + x as i32;
                    let py = bb.min.y as i32 + y as i32;
                    if (0..CELL_W as i32).contains(&px)
                        && (0..CELL_H as i32).contains(&py)
                        && c >= 0.5
                    {
                        rows[py as usize] |= 1 << (CELL_W - 1 - px as usize);
                    }
                });
            }
            glyphs.push(rows);
            chars.push(ch);
        }
    }
    (GlyphFont { cell_w: CELL_W, cell_h: CELL_H, glyphs }, chars)
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
    fn box_drawing_horizontal_is_a_mid_row_line() {
        let (font, chars) = ramp_font(R_BOX);
        let dash = chars.iter().position(|&c| c == '\u{2500}').expect("has ─"); // light horizontal
        // A horizontal rule: some middle row is (nearly) full width, top/bottom rows empty.
        let cov = font.coverage(dash);
        assert!(cov > 0.03 && cov < 0.4, "─ is a thin line ({cov})");
    }
}
