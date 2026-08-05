//! Font preview — `.ttf` / `.otf` / `.ttc` (TrueType / OpenType / collections). `ab_glyph`
//! rasterizes the glyphs (the same crate egui uses) and `ttf-parser` reads the name / metadata
//! tables. The thumbnail is a rendered sample string; the interactive viewer (see `draw_font_ui`
//! in `app.rs`) adds a type-to-sample box, a glyph grid, and rich copy-out.

use super::{DecodeError, Decoder};
use crate::image_types::PixImage;
use ab_glyph::{point, Font, FontRef, GlyphId, ScaleFont};

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
            | Some(b"ttcf") // TrueType Collection
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

/// Render the whole `text` composition (spacing / line-height / colours / z-order) as a **vector
/// SVG** — the logo, not a bitmap — so it exports crisp at any size. Glyph outlines via ttf-parser,
/// laid out to match [`render_text`]. `None` if the font can't be parsed.
pub fn text_svg(bytes: &[u8], text: &str, opts: &TextOpts) -> Option<String> {
    let face = ttf_parser::Face::parse(bytes, 0).ok()?;
    let upem = face.units_per_em() as f32;
    let scale = opts.px.clamp(6.0, 512.0) / upem;
    let asc = face.ascender() as f32 * scale;
    let desc = face.descender() as f32 * scale; // negative
    let line_pitch = (face.height() as f32 * scale) + opts.line_gap;
    const PAD: f32 = 4.0;

    struct PB {
        d: String,
    }
    impl ttf_parser::OutlineBuilder for PB {
        fn move_to(&mut self, x: f32, y: f32) {
            self.d.push_str(&format!("M{x:.1} {y:.1} "));
        }
        fn line_to(&mut self, x: f32, y: f32) {
            self.d.push_str(&format!("L{x:.1} {y:.1} "));
        }
        fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
            self.d.push_str(&format!("Q{x1:.1} {y1:.1} {x:.1} {y:.1} "));
        }
        fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
            self.d
                .push_str(&format!("C{x1:.1} {y1:.1} {x2:.1} {y2:.1} {x:.1} {y:.1} "));
        }
        fn close(&mut self) {
            self.d.push_str("Z ");
        }
    }

    // Lay out each glyph → (line, x, baseline_y, path-in-font-units). One <path> per glyph, placed
    // by a transform (translate to the pen, scale + flip Y from font units to px).
    let mut items: Vec<(usize, f32, f32, String)> = Vec::new();
    let (mut x, mut base, mut line) = (PAD, PAD + asc, 0usize);
    let (mut max_x, mut prev) = (PAD, None::<ttf_parser::GlyphId>);
    for c in text.chars() {
        if c == '\n' {
            x = PAD;
            base += line_pitch;
            line += 1;
            prev = None;
            continue;
        }
        if c == '\r' {
            continue;
        }
        let Some(gid) = face.glyph_index(c) else {
            continue;
        };
        let _ = prev; // kerning omitted in the SVG path (the raster preview keeps it)
        let mut pb = PB { d: String::new() };
        let _ = face.outline_glyph(gid, &mut pb);
        if !pb.d.trim().is_empty() {
            items.push((line, x, base, pb.d.trim().to_string()));
        }
        x += face.glyph_hor_advance(gid).unwrap_or(0) as f32 * scale + opts.letter_spacing;
        max_x = max_x.max(x);
        prev = Some(gid);
    }
    if items.is_empty() {
        return None;
    }
    let total_w = (max_x + PAD).max(1.0);
    let total_h = (PAD * 2.0 + asc + line as f32 * line_pitch + desc.abs()).max(1.0);
    // z-order: top_down draws the upper lines last (on top).
    if opts.top_down {
        items.sort_by(|a, b| b.0.cmp(&a.0));
    } else {
        items.sort_by_key(|i| i.0);
    }
    let hex = |c: [u8; 3]| format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]);
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {total_w:.0} {total_h:.0}\" width=\"{total_w:.0}\" height=\"{total_h:.0}\">"
    );
    if let Some(bg) = opts.bg {
        svg.push_str(&format!(
            "<rect width=\"{total_w:.0}\" height=\"{total_h:.0}\" fill=\"{}\"/>",
            hex(bg)
        ));
    }
    let ink = hex(opts.ink);
    // Stroke: SVG strokes are centred on the path, so we approximate the raster modes by picking a
    // width + paint-order. Stroke-width is in *glyph units* (pre-scale), so divide by `scale`.
    // Outer: paint stroke behind the fill at 2×width (only the outer half shows) → an outline OUTSIDE.
    // Center: a symmetric stroke straddling the edge. Inner: draw the stroke ON TOP so it eats inward.
    let stroke_attr = if opts.stroke_w > 0.5 && scale > 0.0 {
        let sc = hex(opts.stroke_color);
        match opts.stroke_mode {
            StrokeMode::Outer => format!(
                " stroke=\"{sc}\" stroke-width=\"{:.3}\" paint-order=\"stroke\" stroke-linejoin=\"round\"",
                (opts.stroke_w * 2.0) / scale
            ),
            StrokeMode::Center => format!(
                " stroke=\"{sc}\" stroke-width=\"{:.3}\" paint-order=\"stroke\" stroke-linejoin=\"round\"",
                opts.stroke_w / scale
            ),
            StrokeMode::Inner => format!(
                " stroke=\"{sc}\" stroke-width=\"{:.3}\" stroke-linejoin=\"round\"",
                opts.stroke_w / scale
            ),
        }
    } else {
        String::new()
    };
    for (_, gx, gy, d) in items {
        // translate to the pen, then scale glyph units→px with a Y flip.
        svg.push_str(&format!(
            "<path transform=\"translate({gx:.1} {gy:.1}) scale({scale:.5} {:.5})\" d=\"{d}\" fill=\"{ink}\"{stroke_attr}/>",
            -scale
        ));
    }
    svg.push_str("</svg>");
    Some(svg)
}

/// Layout + colour options for [`render_text`] — a mini logo maker (parity with the TDF viewer).
#[derive(Clone, Copy)]
pub struct TextOpts {
    pub px: f32,               // em size
    pub ink: [u8; 3],          // glyph colour
    pub bg: Option<[u8; 3]>,   // background fill (None = transparent)
    pub letter_spacing: f32,   // extra px between glyphs (may be negative → overlap)
    pub line_gap: f32,         // extra px added to the line pitch (may be negative → overlap)
    pub top_down: bool,        // multi-line overlap order: true = upper lines drawn on top
    pub stroke_w: f32,         // stroke width in px (0 = no stroke)
    pub stroke_color: [u8; 3], // stroke colour
    pub stroke_mode: StrokeMode,
}

impl Default for TextOpts {
    fn default() -> Self {
        Self {
            px: 48.0,
            ink: [235, 235, 235],
            bg: None,
            letter_spacing: 0.0,
            line_gap: 0.0,
            top_down: true,
            stroke_w: 0.0,
            stroke_color: [0, 0, 0],
            stroke_mode: StrokeMode::Outer,
        }
    }
}

/// Where a stroke sits relative to the glyph edge.
#[derive(Clone, Copy, PartialEq)]
pub enum StrokeMode {
    Outer,  // entirely outside the fill
    Center, // straddles the edge (half out, half in)
    Inner,  // entirely inside the fill
}

impl StrokeMode {
    pub const ALL: [StrokeMode; 3] = [StrokeMode::Outer, StrokeMode::Center, StrokeMode::Inner];
    pub fn label(self) -> &'static str {
        match self {
            StrokeMode::Outer => "Outer",
            StrokeMode::Center => "Center",
            StrokeMode::Inner => "Inner",
        }
    }
    pub fn to_u8(self) -> u8 {
        match self {
            StrokeMode::Outer => 0,
            StrokeMode::Center => 1,
            StrokeMode::Inner => 2,
        }
    }
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => StrokeMode::Center,
            2 => StrokeMode::Inner,
            _ => StrokeMode::Outer,
        }
    }
}

/// Separable max (`dilate`, grows the shape) / min (`erode`, shrinks it) filter over a `±r` square,
/// on a coverage buffer. Fast (two O(w·h·r) passes); square corners are fine for a font stroke.
fn morph(src: &[f32], w: usize, h: usize, r: usize, dilate: bool) -> Vec<f32> {
    if r == 0 {
        return src.to_vec();
    }
    let pick = |a: f32, b: f32| if dilate { a.max(b) } else { a.min(b) };
    let mut tmp = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut v = if dilate { 0.0 } else { 1.0 };
            let x0 = x.saturating_sub(r);
            let x1 = (x + r).min(w - 1);
            for xx in x0..=x1 {
                v = pick(v, src[y * w + xx]);
            }
            tmp[y * w + x] = v;
        }
    }
    let mut out = vec![0f32; w * h];
    for x in 0..w {
        for y in 0..h {
            let mut v = if dilate { 0.0 } else { 1.0 };
            let y0 = y.saturating_sub(r);
            let y1 = (y + r).min(h - 1);
            for yy in y0..=y1 {
                v = pick(v, tmp[yy * w + x]);
            }
            out[y * w + x] = v;
        }
    }
    out
}

/// Add a stroke to a coverage-bearing image (RGB = ink, alpha = fill coverage — e.g. a bitmap-font
/// render on transparent). Grows the canvas so an outer/center stroke isn't clipped, composites
/// `bg → stroke → fill`, and returns the new image. Used for the FON viewer (TTF strokes inline in
/// `render_text`). `stroke_w ≤ 0.5` returns the input unchanged (optionally over `bg`).
pub fn stroke_image(
    img: &PixImage,
    stroke_w: f32,
    stroke_color: [u8; 3],
    mode: StrokeMode,
    ink: [u8; 3],
    bg: Option<[u8; 3]>,
) -> PixImage {
    let (iw, ih) = (img.width as usize, img.height as usize);
    let src = img.rgba_bytes();
    let sw = stroke_w.clamp(0.0, 200.0);
    let grow = match mode {
        StrokeMode::Outer => sw,
        StrokeMode::Center => sw * 0.5,
        StrokeMode::Inner => 0.0,
    };
    let g = grow.ceil() as usize;
    let (w, h) = (iw + 2 * g, ih + 2 * g);
    if w == 0 || h == 0 {
        return img.clone();
    }
    // Fill coverage lifted from the source alpha, offset by the grown margin.
    let mut cov = vec![0f32; w * h];
    for y in 0..ih {
        for x in 0..iw {
            cov[(y + g) * w + (x + g)] = src[(y * iw + x) * 4 + 3] as f32 / 255.0;
        }
    }
    let mut buf = match bg {
        Some(b) => vec![[b[0], b[1], b[2], 255]; w * h],
        None => vec![[ink[0], ink[1], ink[2], 0]; w * h],
    };
    let composite = |buf: &mut [[u8; 4]], i: usize, color: [u8; 3], a: f32| {
        if a <= 0.0 {
            return;
        }
        let dst = buf[i];
        let da = dst[3] as f32 / 255.0;
        let oa = a + da * (1.0 - a);
        if oa > 0.0 {
            let mix = |s: u8, d: u8| -> u8 {
                (((s as f32 * a + d as f32 * da * (1.0 - a)) / oa).round()).clamp(0.0, 255.0) as u8
            };
            buf[i] = [
                mix(color[0], dst[0]),
                mix(color[1], dst[1]),
                mix(color[2], dst[2]),
                (oa * 255.0) as u8,
            ];
        }
    };
    let (stroke_cov, fill_cov) = if sw > 0.5 {
        match mode {
            StrokeMode::Outer => (morph(&cov, w, h, sw.round() as usize, true), cov.clone()),
            StrokeMode::Inner => (cov.clone(), morph(&cov, w, h, sw.round() as usize, false)),
            StrokeMode::Center => {
                let half = (sw * 0.5).round() as usize;
                (
                    morph(&cov, w, h, half, true),
                    morph(&cov, w, h, half, false),
                )
            }
        }
    } else {
        (Vec::new(), cov.clone())
    };
    for (i, &fa) in fill_cov.iter().enumerate() {
        if let Some(&sa) = stroke_cov.get(i) {
            composite(&mut buf, i, stroke_color, sa);
        }
        composite(&mut buf, i, ink, fa);
    }
    PixImage::from_rgba(w as u32, h as u32, buf)
}

/// Rasterize `text` in the font per `opts` into an RGBA `PixImage`. Honors `\n`, advance widths +
/// kerning, letter-spacing, line-height, an optional background, and the multi-line z-order. `None`
/// if the font can't be parsed. Output is bounded so a giant paste can't allocate wildly.
pub fn render_text(bytes: &[u8], text: &str, opts: &TextOpts) -> Option<PixImage> {
    let font = FontRef::try_from_slice(bytes).ok()?;
    let px = opts.px.clamp(6.0, 512.0);
    let scaled = font.as_scaled(px);
    let ascent = scaled.ascent();
    let line_pitch = scaled.height() + scaled.line_gap() + opts.line_gap;
    const PAD: f32 = 4.0;

    // Layout + outline pass. Outline every glyph up front so the canvas can be sized from the
    // *actual ink bounds* — decorative fonts overhang their advance width (and negative letter-
    // spacing pulls glyphs left/over each other), which a pen-position width would clip.
    let mut outlined: Vec<(usize, ab_glyph::OutlinedGlyph)> = Vec::new();
    let mut caret = point(0.0, ascent);
    let mut prev: Option<GlyphId> = None;
    let mut line_idx = 0usize;
    for c in text.chars() {
        if c == '\n' {
            caret.x = 0.0;
            caret.y += line_pitch;
            prev = None;
            line_idx += 1;
            continue;
        }
        if c == '\r' {
            continue;
        }
        let gid = font.glyph_id(c);
        if let Some(p) = prev {
            caret.x += scaled.kern(p, gid);
        }
        if let Some(o) = font.outline_glyph(gid.with_scale_and_position(px, caret)) {
            outlined.push((line_idx, o));
        }
        caret.x += scaled.h_advance(gid) + opts.letter_spacing;
        prev = Some(gid);
    }

    // True bounding box across all glyph ink (handles overhang + negative positions).
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for (_, o) in &outlined {
        let bb = o.px_bounds();
        min_x = min_x.min(bb.min.x);
        min_y = min_y.min(bb.min.y);
        max_x = max_x.max(bb.max.x);
        max_y = max_y.max(bb.max.y);
    }
    if outlined.is_empty() {
        return Some(PixImage::from_rgba(1, 1, vec![[0u8; 4]]));
    }
    // The canvas grows by however far the stroke extends OUTSIDE the ink (outer = full, center = half).
    let sw = opts.stroke_w.clamp(0.0, 200.0);
    let grow = match opts.stroke_mode {
        StrokeMode::Outer => sw,
        StrokeMode::Center => sw * 0.5,
        StrokeMode::Inner => 0.0,
    };
    let margin = PAD + grow;
    let w = ((max_x - min_x) + 2.0 * margin).ceil().clamp(1.0, 8192.0) as usize;
    let h = ((max_y - min_y) + 2.0 * margin).ceil().clamp(1.0, 8192.0) as usize;
    let (ox, oy) = (margin - min_x, margin - min_y);
    let ink = opts.ink;

    // Fill coverage: the union (max) of every glyph's coverage — one ink colour, so z-order doesn't
    // affect the shape; overlapping lines just merge.
    let mut cov = vec![0f32; w * h];
    let _ = opts.top_down; // z-order is moot for a single-colour union
    for (_, o) in &outlined {
        let bb = o.px_bounds();
        o.draw(|dx, dy, c| {
            let x = (bb.min.x + ox).round() as i32 + dx as i32;
            let y = (bb.min.y + oy).round() as i32 + dy as i32;
            if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
                let i = y as usize * w + x as usize;
                cov[i] = cov[i].max(c.clamp(0.0, 1.0));
            }
        });
    }

    let mut buf = match opts.bg {
        Some(bg) => vec![[bg[0], bg[1], bg[2], 255]; w * h],
        None => vec![[ink[0], ink[1], ink[2], 0]; w * h],
    };
    // Alpha-over a solid `color` at coverage `a` onto pixel `i`.
    let composite = |buf: &mut [[u8; 4]], i: usize, color: [u8; 3], a: f32| {
        if a <= 0.0 {
            return;
        }
        let dst = buf[i];
        let da = dst[3] as f32 / 255.0;
        let oa = a + da * (1.0 - a);
        if oa > 0.0 {
            let mix = |s: u8, d: u8| -> u8 {
                (((s as f32 * a + d as f32 * da * (1.0 - a)) / oa).round()).clamp(0.0, 255.0) as u8
            };
            buf[i] = [
                mix(color[0], dst[0]),
                mix(color[1], dst[1]),
                mix(color[2], dst[2]),
                (oa * 255.0) as u8,
            ];
        }
    };

    // Stroke: paint the stroke shape (dilated/eroded coverage) first, then the fill on top.
    let (stroke_cov, fill_cov) = if sw > 0.5 {
        match opts.stroke_mode {
            StrokeMode::Outer => (morph(&cov, w, h, sw.round() as usize, true), cov.clone()),
            StrokeMode::Inner => (cov.clone(), morph(&cov, w, h, sw.round() as usize, false)),
            StrokeMode::Center => {
                let half = (sw * 0.5).round() as usize;
                (
                    morph(&cov, w, h, half, true),
                    morph(&cov, w, h, half, false),
                )
            }
        }
    } else {
        (Vec::new(), cov.clone())
    };
    for (i, &sa) in stroke_cov.iter().enumerate() {
        composite(&mut buf, i, opts.stroke_color, sa);
    }
    for (i, &fa) in fill_cov.iter().enumerate() {
        composite(&mut buf, i, ink, fa);
    }
    Some(PixImage::from_rgba(w as u32, h as u32, buf))
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
        render_text(
            bytes,
            &thumb_sample(),
            &TextOpts {
                px: 44.0,
                ..Default::default()
            },
        )
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
        let img = render_text(
            &bytes,
            "Ag",
            &TextOpts {
                px: 48.0,
                ink: [255, 255, 255],
                ..Default::default()
            },
        )
        .unwrap();
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
        for p in [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
        ] {
            let Ok(bytes) = std::fs::read(p) else {
                continue;
            };
            let svg = glyph_svg(&bytes, 'g', "#000000").expect("g has an outline");
            eprintln!("svg head: {}", &svg[..svg.len().min(120)]);
            assert!(svg.contains("<path d=\"M"));
            // re-parse via usvg to confirm it's valid SVG
            let tree =
                resvg::usvg::Tree::from_data(svg.as_bytes(), &resvg::usvg::Options::default());
            assert!(tree.is_ok(), "svg should parse");
            // a space typically has no outline → None
            assert!(glyph_svg(&bytes, ' ', "#000").is_none());
            return;
        }
    }
}

#[cfg(test)]
mod logo_test {
    use super::*;
    #[test]
    fn text_opts_and_svg() {
        for p in [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
        ] {
            let Ok(bytes) = std::fs::read(p) else {
                continue;
            };
            // letter-spacing widens; bg fills opaque
            let a = render_text(
                &bytes,
                "AB",
                &TextOpts {
                    px: 48.0,
                    letter_spacing: 0.0,
                    ..Default::default()
                },
            )
            .unwrap()
            .width;
            let b = render_text(
                &bytes,
                "AB",
                &TextOpts {
                    px: 48.0,
                    letter_spacing: 20.0,
                    ..Default::default()
                },
            )
            .unwrap()
            .width;
            assert!(b > a, "letter spacing should widen");
            let bg = render_text(
                &bytes,
                "A",
                &TextOpts {
                    px: 48.0,
                    bg: Some([10, 20, 30]),
                    ..Default::default()
                },
            )
            .unwrap();
            assert!(
                bg.rgba_bytes().chunks(4).any(|px| px == [10, 20, 30, 255]),
                "bg fill present"
            );
            // composition SVG valid
            // transparent pixels carry the ink RGB (alpha 0) so LINEAR filtering has no black halo
            let ti = render_text(
                &bytes,
                "i",
                &TextOpts {
                    px: 48.0,
                    ink: [200, 50, 50],
                    ..Default::default()
                },
            )
            .unwrap();
            let corner = &ti.rgba_bytes()[0..4];
            assert_eq!(corner, [200, 50, 50, 0], "transparent bg keeps ink RGB");
            let svg = text_svg(
                &bytes,
                "Hi",
                &TextOpts {
                    px: 64.0,
                    ink: [255, 0, 0],
                    ..Default::default()
                },
            )
            .unwrap();
            eprintln!("svg len {} head {}", svg.len(), &svg[..svg.len().min(90)]);
            assert!(
                resvg::usvg::Tree::from_data(svg.as_bytes(), &resvg::usvg::Options::default())
                    .is_ok()
            );
            return;
        }
    }

    #[test]
    #[ignore]
    fn dump_two_line_band() {
        for p in ["/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf","/usr/share/fonts/TTF/DejaVuSans.ttf"] {
            let Ok(bytes) = std::fs::read(p) else { continue };
            // Two overlapping lines (negative line_gap), white bg, white outer stroke — the "YOU DIED" case.
            let img = render_text(&bytes, "YOU\nDIED", &TextOpts {
                px: 160.0, ink: [230,0,0], bg: Some([255,255,255]),
                letter_spacing: 4.0, line_gap: -80.0,
                stroke_w: 12.0, stroke_color: [255,255,255], stroke_mode: StrokeMode::Outer,
                ..Default::default()
            }).unwrap();
            // Scan every row for any grey pixel (r≈g≈b, not near white/red) — the "band".
            let b = img.rgba_bytes();
            let (w, h) = (img.width as usize, img.height as usize);
            let mut band_rows = 0;
            for y in 0..h {
                let mut grey = 0;
                for x in 0..w {
                    let px = &b[(y*w+x)*4..(y*w+x)*4+4];
                    let (r,g,bl) = (px[0] as i32, px[1] as i32, px[2] as i32);
                    let near = (r-g).abs() < 12 && (g-bl).abs() < 12 && (r-bl).abs() < 12;
                    if near && r > 40 && r < 230 { grey += 1; }
                }
                if grey > w/10 { band_rows += 1; }
            }
            eprintln!("size {w}x{h}, rows with a grey band: {band_rows}");
            image::save_buffer("/tmp/band_raw.png", &b, img.width, img.height, image::ColorType::Rgba8).unwrap();
            eprintln!("wrote /tmp/band_raw.png");
            return;
        }
    }

    #[test]
    fn outer_stroke_grows_canvas_and_paints_outline() {
        for p in [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
        ] {
            let Ok(bytes) = std::fs::read(p) else {
                continue;
            };
            let plain = render_text(
                &bytes,
                "O",
                &TextOpts {
                    px: 64.0,
                    ink: [255, 255, 255],
                    ..Default::default()
                },
            )
            .unwrap();
            let stroked = render_text(
                &bytes,
                "O",
                &TextOpts {
                    px: 64.0,
                    ink: [255, 255, 255],
                    stroke_w: 4.0,
                    stroke_color: [0, 255, 0],
                    stroke_mode: StrokeMode::Outer,
                    ..Default::default()
                },
            )
            .unwrap();
            // Outer stroke enlarges the canvas (grows outside the ink).
            assert!(
                stroked.width > plain.width && stroked.height > plain.height,
                "outer stroke grows the canvas"
            );
            // The stroke colour is present somewhere.
            assert!(
                stroked
                    .rgba_bytes()
                    .chunks(4)
                    .any(|px| px[0] < 40 && px[1] > 200 && px[2] < 40 && px[3] > 200),
                "green outline pixels present"
            );
            // Inner stroke does NOT grow the canvas.
            let inner = render_text(
                &bytes,
                "O",
                &TextOpts {
                    px: 64.0,
                    ink: [255, 255, 255],
                    stroke_w: 4.0,
                    stroke_color: [0, 255, 0],
                    stroke_mode: StrokeMode::Inner,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(
                (inner.width, inner.height),
                (plain.width, plain.height),
                "inner stroke keeps canvas size"
            );
            return;
        }
    }
}
