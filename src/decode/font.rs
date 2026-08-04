//! Font preview — `.ttf` / `.otf` / `.ttc` (TrueType / OpenType / collections). `ab_glyph`
//! rasterizes the glyphs (the same crate egui uses) and `ttf-parser` reads the name / metadata
//! tables. The thumbnail is a rendered sample string; the interactive viewer (see `draw_font_ui`
//! in `app.rs`) adds a type-to-sample box, a glyph grid, and rich copy-out.

use super::{DecodeError, Decoder};
use crate::image_types::PixImage;
use ab_glyph::{point, Font, FontRef, Glyph, GlyphId, ScaleFont};

/// Extensions handled by the font viewer. Sniff-detected too (magic bytes), so extension is only
/// a fallback.
pub const FONT_EXTS: &[&str] = &["ttf", "otf", "ttc", "otc"];

/// The default sample shown on a font grid thumbnail (compact, recognizable across a folder of
/// fonts). User-overridable via [`set_thumb_sample`] (Preferences → "Font preview sample").
pub const DEFAULT_THUMB_SAMPLE: &str = "AaBbCcDdEe\n0123456789";

/// The active thumbnail sample text — a process-wide rendering preference (set from the UI, primed
/// from storage on launch), read at decode time. Same pattern as `ansi::set_font_9px`, since a
/// `Decoder` only receives bytes, not app state. Newlines split lines on the tile.
static THUMB_SAMPLE: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

/// Override the font-thumbnail sample text (empty ⇒ falls back to the default).
pub fn set_thumb_sample(text: &str) {
    let t = text.trim_end_matches(['\n', '\r']);
    *THUMB_SAMPLE.write().unwrap() = (!t.is_empty()).then(|| t.to_string());
}

/// The current thumbnail sample (the user's override, else the default). Shared with the `.fon`
/// bitmap-font decoder so both tile types honour the same Preferences setting.
pub fn thumb_sample() -> String {
    THUMB_SAMPLE
        .read()
        .unwrap()
        .clone()
        .unwrap_or_else(|| DEFAULT_THUMB_SAMPLE.to_string())
}

/// The user's explicit override, or `None` when unset. The TDF decoder uses this so its tile shows
/// the font's own *name* by default (more useful than "AaBb…") but switches to the custom text once
/// the user sets one — so a single preview string spans TTF / FON / TDF tiles.
pub fn thumb_sample_override() -> Option<String> {
    THUMB_SAMPLE.read().unwrap().clone()
}

/// Parsed font metadata for the Details / viewer header.
#[derive(Clone, Debug, Default)]
pub struct FontInfo {
    pub family: String,
    pub style: String,
    pub glyphs: u16,
    pub monospace: bool,
    pub units_per_em: u16,
}

/// True if `bytes` looks like a TrueType/OpenType font (magic in the first 4 bytes).
pub fn is_font(bytes: &[u8]) -> bool {
    matches!(
        bytes.get(0..4),
        Some(b"\x00\x01\x00\x00") // TrueType
            | Some(b"OTTO")        // OpenType/CFF
            | Some(b"true")        // Apple TrueType
            | Some(b"typ1")        // Type 1 wrapped
            | Some(b"ttcf")        // TrueType Collection
    )
}

/// Read the font's names + metadata via `ttf-parser`. `None` if it can't be parsed.
pub fn font_info(bytes: &[u8]) -> Option<FontInfo> {
    let face = ttf_parser::Face::parse(bytes, 0).ok()?;
    let name = |id: u16| {
        face.names()
            .into_iter()
            .find(|n| n.name_id == id && n.is_unicode())
            .and_then(|n| n.to_string())
    };
    let family = name(ttf_parser::name_id::FAMILY)
        .or_else(|| name(ttf_parser::name_id::TYPOGRAPHIC_FAMILY))
        .unwrap_or_default();
    let style = name(ttf_parser::name_id::SUBFAMILY)
        .or_else(|| name(ttf_parser::name_id::TYPOGRAPHIC_SUBFAMILY))
        .unwrap_or_default();
    Some(FontInfo {
        family,
        style,
        glyphs: face.number_of_glyphs(),
        monospace: face.is_monospaced(),
        units_per_em: face.units_per_em(),
    })
}

/// Every character the font maps (Unicode cmap subtables), sorted + de-duped. Empty on parse error.
pub fn glyph_chars(bytes: &[u8]) -> Vec<char> {
    let Ok(face) = ttf_parser::Face::parse(bytes, 0) else {
        return Vec::new();
    };
    let mut chars = Vec::new();
    if let Some(cmap) = face.tables().cmap {
        for sub in cmap.subtables {
            if sub.is_unicode() {
                sub.codepoints(|cp| {
                    if sub.glyph_index(cp).is_some() {
                        if let Some(c) = char::from_u32(cp) {
                            chars.push(c);
                        }
                    }
                });
            }
        }
    }
    chars.sort_unstable();
    chars.dedup();
    chars
}

/// Extract glyph `ch`'s outline from the font as a standalone **SVG** string (fill=`hex`, e.g.
/// "#000000"). Font Y (up) is flipped to SVG Y (down); the viewBox spans the advance width × the
/// em height. `None` if the font/glyph has no outline (e.g. a bitmap-only or missing glyph).
pub fn glyph_svg(bytes: &[u8], ch: char, fill: &str) -> Option<String> {
    let face = ttf_parser::Face::parse(bytes, 0).ok()?;
    let gid = face.glyph_index(ch)?;

    struct PathBuilder {
        d: String,
    }
    impl ttf_parser::OutlineBuilder for PathBuilder {
        fn move_to(&mut self, x: f32, y: f32) {
            self.d.push_str(&format!("M{x} {y} "));
        }
        fn line_to(&mut self, x: f32, y: f32) {
            self.d.push_str(&format!("L{x} {y} "));
        }
        fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
            self.d.push_str(&format!("Q{x1} {y1} {x} {y} "));
        }
        fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
            self.d.push_str(&format!("C{x1} {y1} {x2} {y2} {x} {y} "));
        }
        fn close(&mut self) {
            self.d.push_str("Z ");
        }
    }

    let mut b = PathBuilder { d: String::new() };
    face.outline_glyph(gid, &mut b)?; // None for glyphs with no contours (e.g. space)
    if b.d.trim().is_empty() {
        return None;
    }
    let asc = face.ascender() as f32;
    let desc = face.descender() as f32;
    let height = (asc - desc).max(1.0);
    let upem = face.units_per_em() as f32;
    let adv = face.glyph_hor_advance(gid).unwrap_or(upem as u16).max(1) as f32;
    // `scale(1,-1)` flips font-Y (up) to SVG-Y (down); the viewBox then spans [−asc, −desc].
    Some(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 {:.0} {:.0} {:.0}\" width=\"{:.0}\" height=\"{:.0}\">\
         <g transform=\"scale(1,-1)\"><path d=\"{}\" fill=\"{fill}\"/></g></svg>",
        -asc, adv, height, adv, height, b.d.trim()
    ))
}

/// Rasterize `text` in the font at `px` em-height into an RGBA `PixImage` (glyphs in `color` over
/// transparent). Honors `\n`, advance widths + kerning. `None` if the font can't be parsed. The
/// output is bounded to a sane max so a giant paste can't allocate wildly.
pub fn render_text(bytes: &[u8], text: &str, px: f32, color: [u8; 3]) -> Option<PixImage> {
    let font = FontRef::try_from_slice(bytes).ok()?;
    let px = px.clamp(6.0, 512.0);
    let scaled = font.as_scaled(px);
    let ascent = scaled.ascent();
    let line_h = scaled.height() + scaled.line_gap();
    const PAD: f32 = 4.0;

    // Layout pass: place each glyph, track width + line count.
    let mut glyphs: Vec<Glyph> = Vec::new();
    let mut caret = point(PAD, PAD + ascent);
    let mut max_x = PAD;
    let mut prev: Option<GlyphId> = None;
    let mut lines = 1usize;
    for c in text.chars() {
        if c == '\n' {
            caret.x = PAD;
            caret.y += line_h;
            prev = None;
            lines += 1;
            continue;
        }
        if c == '\r' {
            continue;
        }
        let gid = font.glyph_id(c);
        if let Some(p) = prev {
            caret.x += scaled.kern(p, gid);
        }
        glyphs.push(gid.with_scale_and_position(px, caret));
        caret.x += scaled.h_advance(gid);
        max_x = max_x.max(caret.x);
        prev = Some(gid);
    }

    let w = ((max_x + PAD).ceil() as usize).clamp(1, 8192);
    let h = ((PAD * 2.0 + lines as f32 * line_h).ceil() as usize).clamp(1, 8192);
    let mut px = vec![[0u8; 4]; w * h];
    for g in glyphs {
        if let Some(outlined) = font.outline_glyph(g) {
            let bb = outlined.px_bounds();
            outlined.draw(|dx, dy, cov| {
                let x = bb.min.x as i32 + dx as i32;
                let y = bb.min.y as i32 + dy as i32;
                if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
                    return;
                }
                let i = y as usize * w + x as usize;
                let a = (cov * 255.0) as u8;
                if a > px[i][3] {
                    px[i] = [color[0], color[1], color[2], a];
                }
            });
        }
    }
    Some(PixImage::from_rgba(w as u32, h as u32, px))
}

/// Render `chars` as a fixed grid of `cols` columns, each glyph centred in a `cell`×`cell` box,
/// into one RGBA image (efficient: one render per page). Returns `(image, rows)`. The viewer
/// overlays a click grid on top to copy a glyph. `None` if the font can't be parsed.
pub fn render_glyph_grid(
    bytes: &[u8],
    chars: &[char],
    cols: usize,
    cell: usize,
    color: [u8; 3],
) -> Option<(PixImage, usize)> {
    let font = FontRef::try_from_slice(bytes).ok()?;
    let cols = cols.max(1);
    let cell = cell.clamp(8, 256);
    let rows = chars.len().div_ceil(cols).max(1);
    let (w, h) = (cols * cell, rows * cell);
    let mut px = vec![[0u8; 4]; w * h];
    let em = cell as f32 * 0.68;
    let scaled = font.as_scaled(em);
    for (idx, &c) in chars.iter().enumerate() {
        let cx = (idx % cols) * cell;
        let cy = (idx / cols) * cell;
        let gid = font.glyph_id(c);
        let adv = scaled.h_advance(gid);
        let gx = cx as f32 + (cell as f32 - adv) * 0.5;
        let gy = cy as f32 + cell as f32 * 0.72; // baseline
        if let Some(outlined) = font.outline_glyph(gid.with_scale_and_position(em, point(gx, gy))) {
            let bb = outlined.px_bounds();
            outlined.draw(|dx, dy, cov| {
                let x = bb.min.x as i32 + dx as i32;
                let y = bb.min.y as i32 + dy as i32;
                if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
                    return;
                }
                let i = y as usize * w + x as usize;
                let a = (cov * 255.0) as u8;
                if a > px[i][3] {
                    px[i] = [color[0], color[1], color[2], a];
                }
            });
        }
    }
    Some((PixImage::from_rgba(w as u32, h as u32, px), rows))
}

/// Registry decoder: the thumbnail is a rendered sample string in the font.
pub struct FontDecoder;

impl Decoder for FontDecoder {
    fn name(&self) -> &'static str {
        "font"
    }
    fn extensions(&self) -> &'static [&'static str] {
        FONT_EXTS
    }
    fn sniff(&self, bytes: &[u8]) -> bool {
        is_font(bytes)
    }
    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        render_text(bytes, &thumb_sample(), 44.0, [235, 235, 235])
            .ok_or(DecodeError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A minimal system font to exercise the parser without bundling one.
    fn a_font() -> Option<Vec<u8>> {
        for p in [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        ] {
            if let Ok(b) = std::fs::read(p) {
                return Some(b);
            }
        }
        None
    }

    #[test]
    fn parses_info_and_renders() {
        let Some(bytes) = a_font() else {
            return; // no system font in CI — skip
        };
        assert!(is_font(&bytes));
        let info = font_info(&bytes).unwrap();
        assert!(!info.family.is_empty());
        assert!(info.glyphs > 100);
        let chars = glyph_chars(&bytes);
        assert!(chars.contains(&'A') && chars.contains(&'g'));
        let img = render_text(&bytes, "Ag", 48.0, [255, 255, 255]).unwrap();
        assert!(img.width > 4 && img.height > 4);
        // Some pixel got drawn.
        assert!(img.rgba_bytes().chunks(4).any(|p| p[3] > 0));
    }

    #[test]
    fn renders_glyph_grid() {
        let Some(bytes) = a_font() else {
            return;
        };
        let chars: Vec<char> = "ABCDEFGH".chars().collect();
        let (img, rows) = render_glyph_grid(&bytes, &chars, 4, 40, [255, 255, 255]).unwrap();
        assert_eq!(rows, 2); // 8 chars / 4 cols
        assert_eq!((img.width, img.height), (160, 80)); // 4·40 × 2·40
        assert!(img.rgba_bytes().chunks(4).any(|p| p[3] > 0));
    }
}


#[cfg(test)]
mod svg_test {
    use super::*;
    #[test]
    fn glyph_svg_is_valid() {
        for p in ["/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf","/usr/share/fonts/TTF/DejaVuSans.ttf"] {
            let Ok(bytes) = std::fs::read(p) else { continue };
            let svg = glyph_svg(&bytes, 'g', "#000000").expect("g has an outline");
            eprintln!("svg head: {}", &svg[..svg.len().min(120)]);
            assert!(svg.contains("<path d=\"M"));
            // re-parse via usvg to confirm it's valid SVG
            let tree = resvg::usvg::Tree::from_data(svg.as_bytes(), &resvg::usvg::Options::default());
            assert!(tree.is_ok(), "svg should parse");
            // a space typically has no outline → None
            assert!(glyph_svg(&bytes, ' ', "#000").is_none());
            return;
        }
    }
}
