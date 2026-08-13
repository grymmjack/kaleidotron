//! Amiga bitmap fonts, including **ColorFonts** — the palette-preserving path.
//!
//! An Amiga font family is two things on disk: a `NAME.font` *descriptor* naming one or more sizes,
//! and a sibling `NAME/` directory holding a file per size (`36.8C`, `144.16C`, …). The size file is
//! the interesting one — it is a complete **AmigaOS hunk executable** whose one code hunk is a
//! `DiskFontHeader` followed by a `TextFont` struct and the glyph bitmaps. The executable stub is a
//! real `moveq #0,d0 / rts`, because on a real Amiga you could run a font and have it do nothing.
//!
//! A **ColorFont** is that same `TextFont` with `FSF_COLORFONT` (0x40) in `tf_Style`, which
//! reinterprets the struct as a `ColorTextFont`: instead of one bitplane there are up to eight, plus
//! a palette. That is why these fonts land here rather than in `font.rs` (TTF/OTF): they are
//! *indexed pixel art*, and the whole point of this program is not to throw the palette away.
//! [`decode`] therefore builds a [`PixImage::from_indexed`], so the swatches, `.GPL` export and
//! recolor pipeline all work on a font sheet exactly as they do on a PCX.
//!
//! ## Reading the layout
//!
//! Everything is big-endian, and **every `APTR` in the file is an offset from the segment base**,
//! not a file offset — on a real load the hunk relocation records would add the segment address to
//! each, so what is stored is the relative value. Getting that wrong reads glyphs from nowhere.
//!
//! ```text
//! file   0  HUNK_HEADER 0x3F3, resident name, table size, first, last, size[]
//!           HUNK_CODE   0x3E9, longword count
//! seg    0  moveq #0,d0 / rts        the runnable stub
//!        4  struct Node              (14 bytes)
//!       18  dfh_FileID = 0x0F80      DFH_ID — the check that this is really a font
//!       20  dfh_Revision
//!       22  dfh_Segment
//!       26  dfh_Name[32]
//!       58  struct TextFont          (52 bytes; see the offsets in `parse`)
//!      110  ColorTextFont extras     depth, palette pointer, ctf_CharData[8]
//! ```
//!
//! Verified byte-for-byte against the Stone Oakvalley ColorFonts archive before this was written:
//! `Aggress/36.8C` reports `tf_YSize` 36 (matching its filename), `tf_Style` 0x40, `tf_XSize` 35,
//! chars 0x20..0x5F, depth 3, and renders 64 glyphs of shaded 3D lettering.

use super::{DecodeError, Decoder};
use crate::image_types::PixImage;
use std::path::Path;

/// Size-file extensions: `<pointsize>.<colours>C`. The descriptor names them, but they are also
/// perfectly decodable on their own — a size file carries the name, the palette and every glyph —
/// so a folder of them browses without the `.font` files being present at all.
pub const AMIGA_FONT_EXTS: &[&str] =
    &["font", "2c", "4c", "8c", "16c", "32c", "64c", "128c", "256c"];

const HUNK_HEADER: u32 = 0x3F3;
const HUNK_CODE: u32 = 0x3E9;
const DFH_ID: u16 = 0x0F80;
/// `FCH_ID` — the two-byte magic beginning a `.font` descriptor.
const FCH_ID: u16 = 0x0F00;
/// `FSF_COLORFONT` in `tf_Style`: the TextFont is really a ColorTextFont.
const FSF_COLORFONT: u8 = 0x40;

fn u16be(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(o)?, *b.get(o + 1)?]))
}
fn u32be(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes([*b.get(o)?, *b.get(o + 1)?, *b.get(o + 2)?, *b.get(o + 3)?]))
}

/// One glyph: its width in pixels and one palette index per pixel, row-major.
#[derive(Debug, Clone)]
pub struct Glyph {
    /// The character this glyph draws. Amiga fonts declare a contiguous `tf_LoChar..=tf_HiChar`
    /// range, so this is derived rather than stored per glyph.
    pub code: u8,
    pub width: u32,
    /// `width * height` palette indices. Index 0 is the background and is drawn transparent.
    pub indices: Vec<u8>,
}

/// A parsed Amiga font size.
#[derive(Debug, Clone)]
pub struct ColorFont {
    pub name: String,
    /// `tf_YSize` — the design size, and what the size file is named after.
    pub height: u32,
    /// Bitplane count. 1 for a plain mono Amiga font, up to 8 for a ColorFont.
    pub depth: u8,
    pub is_color: bool,
    /// RGBA, index 0 forced transparent. A mono font gets a synthetic two-entry palette.
    pub palette: Vec<[u8; 4]>,
    pub glyphs: Vec<Glyph>,
}

impl ColorFont {
    /// Widest glyph — the cell width of the rendered sheet, and the natural export cell.
    pub fn max_width(&self) -> u32 {
        self.glyphs.iter().map(|g| g.width).max().unwrap_or(1).max(1)
    }
}

/// Parse a size file (the hunk executable). This is the whole format.
pub fn parse(bytes: &[u8]) -> Result<ColorFont, DecodeError> {
    let bad = |m: &str| DecodeError::Malformed(m.to_string());

    if u32be(bytes, 0) != Some(HUNK_HEADER) {
        return Err(bad("not an Amiga hunk file"));
    }
    // Header: magic, resident-library names (a 0-terminated list — always empty for a font),
    // table size, first, last, then one size longword per hunk.
    let mut o = 4usize;
    while u32be(bytes, o).ok_or_else(|| bad("truncated resident list"))? != 0 {
        let n = u32be(bytes, o).unwrap() as usize;
        o += 4 + n * 4; // a name is a length in longwords followed by that many
    }
    o += 4;
    let _table = u32be(bytes, o).ok_or_else(|| bad("truncated header"))?;
    let first = u32be(bytes, o + 4).ok_or_else(|| bad("truncated header"))?;
    let last = u32be(bytes, o + 8).ok_or_else(|| bad("truncated header"))?;
    if last < first || last - first > 64 {
        return Err(bad("implausible hunk count"));
    }
    o += 12 + (last - first + 1) as usize * 4;

    if u32be(bytes, o) != Some(HUNK_CODE) {
        return Err(bad("first hunk is not HUNK_CODE"));
    }
    // `seg` is the base every APTR in this file is relative to.
    let seg = o + 8;

    if u16be(bytes, seg + 18) != Some(DFH_ID) {
        return Err(bad("no DiskFontHeader — not a font"));
    }
    let name = bytes
        .get(seg + 26..seg + 58)
        .map(|s| {
            let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
            String::from_utf8_lossy(&s[..end]).trim().to_string()
        })
        .unwrap_or_default();

    // ── TextFont ────────────────────────────────────────────────────────────
    let tf = seg + 58;
    let height = u16be(bytes, tf + 20).ok_or_else(|| bad("truncated TextFont"))? as u32;
    let style = *bytes.get(tf + 22).ok_or_else(|| bad("truncated TextFont"))?;
    let modulo = u16be(bytes, tf + 38).ok_or_else(|| bad("truncated TextFont"))? as usize;
    let lo = *bytes.get(tf + 32).ok_or_else(|| bad("truncated TextFont"))?;
    let hi = *bytes.get(tf + 33).ok_or_else(|| bad("truncated TextFont"))?;
    let char_data = u32be(bytes, tf + 34).ok_or_else(|| bad("truncated TextFont"))? as usize;
    let char_loc = u32be(bytes, tf + 40).ok_or_else(|| bad("truncated TextFont"))? as usize;
    if hi < lo || height == 0 || height > 512 || modulo == 0 {
        return Err(bad("implausible font metrics"));
    }

    let is_color = style & FSF_COLORFONT != 0;

    // ── Planes and palette ──────────────────────────────────────────────────
    // A mono font has its single plane at tf_CharData and no palette; a ColorFont has up to eight
    // planes and a real one. Treating mono as "depth 1 with a synthetic palette" means the renderer
    // below has exactly one path.
    let (depth, planes, palette) = if is_color {
        let ctf = tf + 52;
        let depth = *bytes.get(ctf + 2).ok_or_else(|| bad("truncated ColorTextFont"))?;
        if depth == 0 || depth > 8 {
            return Err(bad("implausible ColorFont depth"));
        }
        let cfc = u32be(bytes, ctf + 8).ok_or_else(|| bad("truncated ColorTextFont"))? as usize;
        let mut planes = Vec::with_capacity(depth as usize);
        for i in 0..depth as usize {
            planes.push(
                u32be(bytes, ctf + 12 + 4 * i).ok_or_else(|| bad("truncated plane table"))? as usize,
            );
        }
        // ColorFontColors: reserved, count, then a pointer to `count` 0x0RGB words — 4 bits per
        // channel, which is the Amiga's actual colour resolution. Scaling by 17 (not <<4) maps
        // 0xF to 0xFF so white is white rather than 0xF0.
        let mut palette: Vec<[u8; 4]> = Vec::new();
        if cfc != 0 {
            let count = u16be(bytes, seg + cfc + 2).unwrap_or(0) as usize;
            let table = u32be(bytes, seg + cfc + 4).unwrap_or(0) as usize;
            for i in 0..count.min(256) {
                let w = u16be(bytes, seg + table + 2 * i).unwrap_or(0);
                let (r, g, b) = ((w >> 8) & 0xF, (w >> 4) & 0xF, w & 0xF);
                palette.push([(r * 17) as u8, (g * 17) as u8, (b * 17) as u8, 255]);
            }
        }
        // A ColorFont with no usable table still has to draw: fall back to a grey ramp rather than
        // refusing the file, so a damaged palette costs colour and not the glyphs.
        if palette.len() < 1 << depth {
            let n = 1usize << depth;
            palette = (0..n)
                .map(|i| {
                    let v = (i * 255 / (n - 1).max(1)) as u8;
                    [v, v, v, 255]
                })
                .collect();
        }
        (depth, planes, palette)
    } else {
        (1u8, vec![char_data], vec![[0, 0, 0, 255], [255, 255, 255, 255]])
    };

    let mut palette = palette;
    palette[0][3] = 0; // Amiga colour 0 is the background — transparent, so tiles composite

    // ── Glyph table ─────────────────────────────────────────────────────────
    // tf_CharLoc is (bit offset, bit width) per character, with ONE extra entry for the
    // undefined-character glyph. All glyphs live side by side in one wide bitmap per plane, so a
    // glyph is a column slice — there is no per-glyph bitmap to find.
    let n = hi as usize - lo as usize + 1;
    let mut glyphs = Vec::with_capacity(n);
    for i in 0..n {
        let off = u16be(bytes, seg + char_loc + 4 * i).ok_or_else(|| bad("truncated CharLoc"))? as usize;
        let w = u16be(bytes, seg + char_loc + 4 * i + 2).ok_or_else(|| bad("truncated CharLoc"))? as usize;
        let w = w.min(1024);
        let mut indices = vec![0u8; w * height as usize];
        for y in 0..height as usize {
            for x in 0..w {
                let bit = off + x;
                let mut v = 0u8;
                for (p, &plane) in planes.iter().enumerate() {
                    let byte = match bytes.get(seg + plane + y * modulo + (bit >> 3)) {
                        Some(b) => *b,
                        None => continue,
                    };
                    if byte & (0x80 >> (bit & 7)) != 0 {
                        v |= 1 << p;
                    }
                }
                indices[y * w + x] = v;
            }
        }
        glyphs.push(Glyph { code: lo.saturating_add(i as u8), width: w as u32, indices });
    }

    Ok(ColorFont { name, height, depth, is_color, palette, glyphs })
}

/// The `.font` descriptor: which size files this family has.
///
/// `FCH_ID`, an entry count, then fixed 260-byte `FontContents` records — a 256-byte
/// `fc_FileName` (a path *relative to the descriptor's own directory*, e.g. `Aggress/36.8C`),
/// then `fc_YSize`, `fc_Style`, `fc_Flags`.
pub fn parse_descriptor(bytes: &[u8]) -> Result<Vec<(String, u16)>, DecodeError> {
    if u16be(bytes, 0) != Some(FCH_ID) {
        return Err(DecodeError::Malformed("not a .font descriptor".into()));
    }
    let n = u16be(bytes, 2).unwrap_or(0) as usize;
    let mut out = Vec::new();
    for i in 0..n.min(64) {
        let rec = 4 + i * 260;
        let Some(raw) = bytes.get(rec..rec + 256) else { break };
        let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
        let path = String::from_utf8_lossy(&raw[..end]).to_string();
        if path.is_empty() {
            continue;
        }
        out.push((path, u16be(bytes, rec + 256).unwrap_or(0)));
    }
    Ok(out)
}

/// Render a font as a glyph sheet: a 16-column grid, one cell per character.
///
/// A grid rather than the single wide strip the font stores, because a 96-glyph font is a
/// 3000px-wide sliver that reads as nothing at thumbnail size — and because the grid is how these
/// fonts are shown in every Amiga font tool, so it is the familiar picture.
pub fn render_sheet(f: &ColorFont) -> PixImage {
    const COLS: u32 = 16;
    let cell_w = f.max_width();
    let rows = (f.glyphs.len() as u32).div_ceil(COLS).max(1);
    let (w, h) = (COLS * cell_w, rows * f.height);
    let mut indices = vec![0u8; (w * h) as usize];
    for (i, g) in f.glyphs.iter().enumerate() {
        let (ox, oy) = ((i as u32 % COLS) * cell_w, (i as u32 / COLS) * f.height);
        for y in 0..f.height {
            for x in 0..g.width.min(cell_w) {
                let v = g.indices[(y * g.width + x) as usize];
                indices[((oy + y) * w + ox + x) as usize] = v;
            }
        }
    }
    PixImage::from_indexed(w, h, indices, f.palette.clone())
}

/// Render sample text as a logo — the point of the whole feature.
///
/// Amiga fonts are proportional (`tf_CharLoc` gives each glyph its own width) and often overlap by
/// design, so `spacing` is a per-glyph delta added after each character: 0 abuts glyphs, a small
/// negative kerns a 3D font into itself the way it was drawn to sit. Newlines start a new row,
/// `line_gap` added to the font height between rows.
///
/// A character the font does not define is skipped rather than boxed — a logo maker wants the word,
/// not a row of tofu. Space advances by the average glyph width, because a ColorFont has no space
/// glyph (`tf_LoChar` is usually `!`).
///
/// Palette-preserving: the result is `from_indexed`, so the recolor pane can retint a whole logo
/// and the swatches read the font's colours.
pub fn render_text(f: &ColorFont, text: &str, spacing: i32, line_gap: i32) -> PixImage {
    let avg = (f.glyphs.iter().map(|g| g.width).sum::<u32>() / f.glyphs.len().max(1) as u32).max(1);
    let line_h = f.height as i32 + line_gap.max(-(f.height as i32) + 1);

    // Lay out into (x, y, glyph) placements first so the canvas can be sized to the real extent —
    // overlapping/negative spacing means a line's width is not just the sum of widths.
    let mut placed: Vec<(i32, i32, &Glyph)> = Vec::new();
    let (mut x, mut y, mut max_x) = (0i32, 0i32, 0i32);
    for ch in text.chars() {
        if ch == '\n' {
            x = 0;
            y += line_h;
            continue;
        }
        if ch == ' ' {
            x += avg as i32 + spacing;
            continue;
        }
        let code = ch as u32;
        match f.glyphs.iter().find(|g| g.code as u32 == code) {
            Some(g) if g.width > 0 => {
                placed.push((x, y, g));
                x += g.width as i32 + spacing;
                max_x = max_x.max(x - spacing); // the glyph's right edge, not the post-advance cursor
            }
            _ => x += avg as i32 + spacing, // undefined char / lowercase a font lacks: advance, no box
        }
    }
    let w = max_x.max(1) as u32;
    let h = (y + f.height as i32).max(1) as u32;
    let mut indices = vec![0u8; (w * h) as usize];
    for (gx, gy, g) in placed {
        for row in 0..f.height as i32 {
            let py = gy + row;
            if py < 0 || py as u32 >= h {
                continue;
            }
            for col in 0..g.width as i32 {
                let px = gx + col;
                if px < 0 || px as u32 >= w {
                    continue;
                }
                let idx = g.indices[(row as u32 * g.width + col as u32) as usize];
                if idx != 0 {
                    // Last writer wins where glyphs overlap — which is how these fonts were drawn
                    // to layer, so a negative `spacing` looks right rather than showing seams.
                    indices[(py as u32 * w + px as u32) as usize] = idx;
                }
            }
        }
    }
    PixImage::from_indexed(w, h, indices, f.palette.clone())
}

/// Decode a size file's bytes straight to a sheet.
pub fn decode(bytes: &[u8]) -> Result<PixImage, DecodeError> {
    Ok(render_sheet(&parse(bytes)?))
}

/// Decode by path, following a `.font` descriptor to its first size when given one.
///
/// The descriptor names a path relative to its OWN directory, so this needs the path and not just
/// the bytes — the same reason 3D models and video are routed by path in `decode_bytes`.
pub fn decode_path(path: &Path, bytes: &[u8]) -> Result<PixImage, DecodeError> {
    let is_descriptor = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("font"));
    if !is_descriptor {
        return decode(bytes);
    }
    let entries = parse_descriptor(bytes)?;
    let dir = path.parent().unwrap_or(Path::new("."));
    // Largest size first: it is the one worth looking at, and a family's small sizes are usually
    // the same art at less detail.
    let mut entries = entries;
    entries.sort_by_key(|(_, y)| std::cmp::Reverse(*y));
    for (rel, _) in &entries {
        // Amiga paths use '/', which is already right on unix and is what `Path` wants anyway.
        let candidate = dir.join(rel.replace('\\', "/"));
        if let Ok(sz) = std::fs::read(&candidate) {
            if let Ok(img) = decode(&sz) {
                return Ok(img);
            }
        }
    }
    Err(DecodeError::Malformed(
        "descriptor names no size file that could be read next to it".into(),
    ))
}

/// Convert to a **DRAW Color Bitmap Font** sheet.
///
/// DRAW's CBF is one bitmap strip with a marker row on top:
///
/// ```text
/// row 0    background everywhere, except ONE pixel of another colour at each glyph's start x
/// rows 1+  the glyphs, left to right, in their real colours
/// ```
///
/// and its loader (`CBF_detect%` / `CBF_render_glyph` in DRAW's `GUI/FONT-LIST.BM`) infers
/// everything else: the **background is elected**, not declared — whatever colour is most frequent
/// in row 0 wins and becomes transparent throughout — and glyph **widths are implied** by the gaps
/// between markers, so proportional fonts come for free.
///
/// Two mappings have to be got right, and they are where an Amiga font and DRAW disagree:
///
/// * **DRAW maps glyphs positionally from ASCII 33**, while an Amiga font declares an arbitrary
///   `tf_LoChar..=tf_HiChar`. A gap anywhere shifts every later character, so gaps inside the
///   exported range are filled with blank spacers rather than skipped.
/// * **Space is never a glyph** in a CBF (DRAW advances by the average width), so code 32 is
///   dropped even though most Amiga fonts start there.
///
/// The range deliberately stops at the font's own last character rather than padding out to 126.
/// These are display fonts — `Aggress` ends at 0x5F, with no lowercase — and DRAW aliases missing
/// lowercase onto the uppercase glyphs, which is exactly what you want for a logo font. Padding to
/// 126 with blanks would instead give you a font that types nothing in lowercase.
pub fn to_draw_cbf(f: &ColorFont) -> Result<PixImage, DecodeError> {
    let refuse = |m: &str| DecodeError::Malformed(m.to_string());

    // The exported range: from '!' to the font's last glyph, capped at DRAW's addressable end.
    let last = f.glyphs.iter().map(|g| g.code).max().unwrap_or(0).min(126);
    if last < b'!' {
        return Err(refuse("font has no glyphs at or above '!' — nothing DRAW could address"));
    }
    let avg = (f.glyphs.iter().map(|g| g.width).sum::<u32>()
        / f.glyphs.len().max(1) as u32)
        .max(1);

    // Pick each exported cell: the real glyph, or a blank spacer holding its place.
    let mut cells: Vec<(u32, Option<&Glyph>)> = Vec::new();
    for code in b'!'..=last {
        match f.glyphs.iter().find(|g| g.code == code) {
            Some(g) if g.width > 0 => cells.push((g.width, Some(g))),
            _ => cells.push((avg, None)),
        }
    }
    if cells.len() < 2 {
        return Err(refuse("DRAW needs at least two glyphs"));
    }

    let width: u32 = cells.iter().map(|(w, _)| *w).sum();
    let height = f.height + 1; // + the marker row
    if width < 10 || height < 3 {
        return Err(refuse("sheet is below DRAW's 10x3 minimum"));
    }
    // DRAW elects the most frequent row-0 colour as the background. One marker pixel per glyph
    // means markers lose that vote as long as glyphs are wider than a pixel or two — below ~3px
    // average they can outnumber the background and the loader elects the MARKER, turning the
    // glyphs inside out. Refuse rather than emit a sheet that loads wrong.
    if avg < 3 {
        return Err(refuse("glyphs average under 3px — markers would outvote the background"));
    }

    let bg = f.palette.first().copied().unwrap_or([0, 0, 0, 255]);
    let bg_rgb = [bg[0], bg[1], bg[2], 255u8];
    // A marker must differ from the background AND from nothing else in particular — but choosing
    // a colour the font never uses keeps row 0 unambiguous.
    let marker = pick_marker(&f.palette, bg_rgb);

    let mut px = vec![bg_rgb; (width * height) as usize];
    let mut x0 = 0u32;
    for (w, glyph) in &cells {
        px[x0 as usize] = marker; // row 0: the glyph's start
        if let Some(g) = glyph {
            for y in 0..f.height {
                for x in 0..g.width {
                    let idx = g.indices[(y * g.width + x) as usize] as usize;
                    // Index 0 is the Amiga background: leave it as the CBF background so DRAW
                    // elects it and renders it transparent.
                    if idx != 0 {
                        let c = f.palette.get(idx).copied().unwrap_or(bg);
                        px[((y + 1) * width + x0 + x) as usize] = [c[0], c[1], c[2], 255];
                    }
                }
            }
        }
        x0 += w;
    }
    Ok(PixImage::from_rgba(width, height, px))
}

/// A marker colour the font itself never uses, so row 0 has exactly two colours.
fn pick_marker(palette: &[[u8; 4]], bg: [u8; 4]) -> [u8; 4] {
    const CANDIDATES: [[u8; 4]; 4] = [
        [255, 0, 255, 255],
        [0, 255, 0, 255],
        [255, 255, 0, 255],
        [0, 255, 255, 255],
    ];
    let used = |c: [u8; 4]| {
        c == bg || palette.iter().any(|p| p[0] == c[0] && p[1] == c[1] && p[2] == c[2])
    };
    CANDIDATES
        .into_iter()
        .find(|&c| !used(c))
        // Every candidate taken is possible for a rich palette; step through the whole cube
        // rather than give up, since only ONE unused colour is needed.
        .unwrap_or_else(|| {
            for r in (0..=255u8).step_by(17) {
                for g in (0..=255u8).step_by(17) {
                    for b in (0..=255u8).step_by(17) {
                        let c = [r, g, b, 255];
                        if !used(c) {
                            return c;
                        }
                    }
                }
            }
            [255, 0, 255, 255]
        })
}

/// Registry entry. Sniffing is by hunk magic; `.font` descriptors are path-routed in `decode_bytes`
/// because they need their sibling directory.
pub struct AmigaFontDecoder;

impl Decoder for AmigaFontDecoder {
    fn name(&self) -> &'static str {
        "amiga-font"
    }
    fn extensions(&self) -> &'static [&'static str] {
        AMIGA_FONT_EXTS
    }
    fn sniff(&self, header: &[u8]) -> bool {
        // Only the size file is sniffable. A `.font` descriptor's 0x0F00 magic is two bytes and far
        // too weak to claim on — it would swallow unrelated files — so it is matched by extension.
        u32be(header, 0) == Some(HUNK_HEADER)
    }
    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        decode(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal but structurally real ColorFont, built here so the test does not depend on having
    /// the archive on disk. Two 2x2 glyphs, depth 2, four palette entries.
    fn synth() -> Vec<u8> {
        let mut b = Vec::new();
        let mut push32 = |v: u32, b: &mut Vec<u8>| b.extend_from_slice(&v.to_be_bytes());
        push32(HUNK_HEADER, &mut b);
        push32(0, &mut b); // no resident names
        push32(1, &mut b); // table size
        push32(0, &mut b); // first
        push32(0, &mut b); // last
        push32(64, &mut b); // hunk 0 size (longwords)
        push32(HUNK_CODE, &mut b);
        push32(64, &mut b);
        let seg = b.len();
        b.resize(seg + 512, 0);

        b[seg..seg + 4].copy_from_slice(&[0x70, 0x00, 0x4E, 0x75]); // moveq/rts
        b[seg + 18..seg + 20].copy_from_slice(&DFH_ID.to_be_bytes());
        b[seg + 26..seg + 33].copy_from_slice(b"Synthed");

        let tf = seg + 58;
        b[tf + 20..tf + 22].copy_from_slice(&2u16.to_be_bytes()); // tf_YSize = 2
        b[tf + 22] = FSF_COLORFONT;
        b[tf + 32] = b'A'; // lo
        b[tf + 33] = b'B'; // hi
        b[tf + 38..tf + 40].copy_from_slice(&1u16.to_be_bytes()); // tf_Modulo = 1 byte/row
        b[tf + 40..tf + 44].copy_from_slice(&300u32.to_be_bytes()); // tf_CharLoc -> seg+300

        let ctf = tf + 52;
        b[ctf + 2] = 2; // depth
        b[ctf + 8..ctf + 12].copy_from_slice(&200u32.to_be_bytes()); // ColorFontColors -> seg+200
        b[ctf + 12..ctf + 16].copy_from_slice(&400u32.to_be_bytes()); // plane 0
        b[ctf + 16..ctf + 20].copy_from_slice(&420u32.to_be_bytes()); // plane 1

        // ColorFontColors { reserved, count=4, table -> seg+220 }
        b[seg + 202..seg + 204].copy_from_slice(&4u16.to_be_bytes());
        b[seg + 204..seg + 208].copy_from_slice(&220u32.to_be_bytes());
        for (i, w) in [0x0000u16, 0x0F00, 0x00F0, 0x000F].iter().enumerate() {
            b[seg + 220 + i * 2..seg + 222 + i * 2].copy_from_slice(&w.to_be_bytes());
        }

        // CharLoc: glyph 'A' at bit 0 width 2, 'B' at bit 2 width 2, then the undefined glyph.
        for (i, (off, w)) in [(0u16, 2u16), (2, 2), (4, 0)].iter().enumerate() {
            let r = seg + 300 + i * 4;
            b[r..r + 2].copy_from_slice(&off.to_be_bytes());
            b[r + 2..r + 4].copy_from_slice(&w.to_be_bytes());
        }

        // Plane 0, 2 rows of 1 byte: bits 0..3 = 1,0,1,1 / 0,1,1,0
        b[seg + 400] = 0b1011_0000;
        b[seg + 401] = 0b0110_0000;
        // Plane 1: 0,1,1,0 / 1,1,0,0  → combined indices row0: 0,2,3,1  row1: 2,3,1,0
        b[seg + 420] = 0b0110_0000;
        b[seg + 421] = 0b1100_0000;
        b
    }

    #[test]
    fn parses_a_colorfont_and_combines_bitplanes() {
        let f = parse(&synth()).expect("parses");
        assert_eq!(f.name, "Synthed");
        assert_eq!((f.height, f.depth, f.is_color), (2, 2, true));
        assert_eq!(f.glyphs.len(), 2, "lo..=hi inclusive, WITHOUT the undefined glyph");
        assert_eq!((f.glyphs[0].code, f.glyphs[1].code), (b'A', b'B'));

        // The whole point of a ColorFont: a pixel's index is assembled from one bit per PLANE,
        // low plane = low bit. Plane0 row0 = 1,0 and plane1 row0 = 0,1 for glyph 'A', so the
        // indices are 1 and 2 — not 1 and 1, which is what reading a single plane would give.
        assert_eq!(f.glyphs[0].indices, vec![1, 2, 2, 3], "glyph A, 2x2");
        assert_eq!(f.glyphs[1].indices, vec![3, 1, 1, 0], "glyph B, 2x2");
    }

    /// The palette is 4 bits per channel, and 0xF must reach 0xFF — scaling by 16 instead of 17
    /// would make white 0xF0 and quietly darken every font in the collection.
    #[test]
    fn palette_scales_four_bit_channels_to_full_range() {
        let f = parse(&synth()).expect("parses");
        assert_eq!(f.palette[1], [255, 0, 0, 255], "0x0F00 -> full red");
        assert_eq!(f.palette[2], [0, 255, 0, 255]);
        assert_eq!(f.palette[3], [0, 0, 255, 255]);
        assert_eq!(f.palette[0][3], 0, "index 0 is the background and must be transparent");
    }

    /// The sheet keeps its palette, which is what makes a font behave like the rest of the
    /// program's indexed art — swatches, .GPL export and the recolor pipeline all key off it.
    #[test]
    fn renders_an_indexed_sheet() {
        let img = decode(&synth()).expect("renders");
        assert_eq!((img.width, img.height), (16 * 2, 2), "16-column grid of 2px cells");
        let idx = img.indexed.as_ref().expect("indexed, not flattened to RGBA");
        assert_eq!(idx.palette.len(), 4);
        assert_eq!(idx.indices[0], 1, "glyph A's first pixel lands at the sheet origin");
    }

    #[test]
    fn rejects_files_that_are_not_fonts() {
        assert!(parse(b"not a hunk file at all").is_err());
        // DFH_ID lives at seg+18, and the segment starts after the hunk header — NOT at file
        // offset 18. Zeroing the file's byte 18 (as a first draft of this test did) hits the
        // header's hunk table and proves nothing.
        let mut b = synth();
        let seg = 32; // HUNK_HEADER(4) + resident(4) + table(4) + first(4) + last(4) + size(4)
                      // + HUNK_CODE(4) + code size(4)
        assert_eq!(u16be(&b, seg + 18), Some(DFH_ID), "fixture really has the ID here");
        b[seg + 18] = 0;
        b[seg + 19] = 0;
        assert!(parse(&b).is_err(), "a hunk file without a DiskFontHeader is not a font");
    }

    #[test]
    fn parses_a_font_descriptor() {
        let mut b = vec![0u8; 4 + 260];
        b[0..2].copy_from_slice(&FCH_ID.to_be_bytes());
        b[2..4].copy_from_slice(&1u16.to_be_bytes());
        b[4..17].copy_from_slice(b"Aggress/36.8C");
        b[260..262].copy_from_slice(&36u16.to_be_bytes());
        let e = parse_descriptor(&b).expect("parses");
        assert_eq!(e, vec![("Aggress/36.8C".to_string(), 36)]);
    }

    /// Re-implements DRAW's `CBF_detect%` so the export is checked against the LOADER's rules
    /// rather than against my reading of them: scan row 0, elect the most frequent colour as the
    /// background, and count background→non-background transitions as glyphs.
    fn draw_would_see(img: &PixImage) -> (usize, [u8; 4], usize) {
        let w = img.width as usize;
        let row0 = &img.pixels[..w];
        let mut counts: Vec<([u8; 4], usize)> = Vec::new();
        for &p in row0 {
            match counts.iter_mut().find(|(c, _)| *c == p) {
                Some((_, n)) => *n += 1,
                None => counts.push((p, 1)),
            }
        }
        let distinct = counts.len();
        let bg = counts.iter().max_by_key(|(_, n)| *n).map(|(c, _)| *c).unwrap();
        let mut glyphs = 0usize;
        let mut prev_bg = true;
        for &p in row0 {
            if p != bg {
                if prev_bg {
                    glyphs += 1;
                }
                prev_bg = false;
            } else {
                prev_bg = true;
            }
        }
        (distinct, bg, glyphs)
    }

    /// The export must satisfy every gate DRAW's loader applies, or it silently refuses the font.
    #[test]
    fn cbf_export_satisfies_draws_loader() {
        // A font wide enough to clear the 3px-average guard: 8px glyphs for '!'..'0'.
        let mut f = parse(&synth()).expect("parses");
        f.height = 8;
        f.glyphs = (b'!'..=b'0')
            .map(|code| Glyph { code, width: 8, indices: vec![1u8; 64] })
            .collect();

        let sheet = to_draw_cbf(&f).expect("exports");
        assert!(sheet.width >= 10 && sheet.height >= 3, "DRAW's minimum size");
        assert_eq!(sheet.height, f.height + 1, "one marker row on top of the glyphs");

        let (distinct, bg, glyphs) = draw_would_see(&sheet);
        assert!(distinct >= 2, "row 0 needs a background AND a marker colour");
        assert_eq!(bg, [0, 0, 0, 255], "palette index 0 is the elected background");
        assert_eq!(glyphs, f.glyphs.len(), "one marker per exported glyph, and DRAW finds them all");
    }

    /// Space is never a glyph in a CBF, and DRAW maps positionally from '!' — so an Amiga font
    /// starting at 0x20 must have its space DROPPED, not exported. Getting this wrong shifts every
    /// character in the font by one.
    #[test]
    fn cbf_export_drops_space_and_starts_at_bang() {
        let mut f = parse(&synth()).expect("parses");
        f.height = 8;
        f.glyphs = (b' '..=b'*')
            .map(|code| Glyph { code, width: 8, indices: vec![1u8; 64] })
            .collect();

        let sheet = to_draw_cbf(&f).expect("exports");
        let (_, _, glyphs) = draw_would_see(&sheet);
        assert_eq!(
            glyphs,
            (b'!'..=b'*').count(),
            "the space glyph is dropped; the sheet begins at '!'"
        );
    }

    /// A gap in the middle of the range has to be PADDED, not skipped: DRAW counts positions, so a
    /// missing character would shift every later one onto the wrong code.
    #[test]
    fn cbf_export_pads_gaps_so_later_glyphs_stay_aligned() {
        let mut f = parse(&synth()).expect("parses");
        f.height = 8;
        // '!' and '#' present, '"' missing.
        f.glyphs = [b'!', b'#']
            .into_iter()
            .map(|code| Glyph { code, width: 8, indices: vec![1u8; 64] })
            .collect();

        let sheet = to_draw_cbf(&f).expect("exports");
        let (_, _, glyphs) = draw_would_see(&sheet);
        assert_eq!(glyphs, 3, "'!', a blank spacer for '\"', then '#'");
    }

    /// The refusals are as important as the export: a sheet DRAW would misread should never be
    /// written. Below ~3px average, the one-pixel markers outnumber the background in row 0 and
    /// DRAW elects the MARKER as background — rendering every glyph inside out.
    #[test]
    fn cbf_export_refuses_sheets_draw_would_misread() {
        let mut f = parse(&synth()).expect("parses");
        f.height = 8;
        f.glyphs = (b'!'..=b'0')
            .map(|code| Glyph { code, width: 2, indices: vec![1u8; 16] })
            .collect();
        assert!(to_draw_cbf(&f).is_err(), "2px glyphs would invert the background election");

        let mut only_one = parse(&synth()).expect("parses");
        only_one.glyphs = vec![Glyph { code: b'!', width: 20, indices: vec![1u8; 40] }];
        assert!(to_draw_cbf(&only_one).is_err(), "DRAW needs at least two glyphs");
    }

    /// Sample text lays out into a logo whose canvas fits the real extent, undefined characters
    /// are skipped rather than boxed, and the palette is kept.
    #[test]
    fn renders_sample_text_as_a_logo() {
        let mut f = parse(&synth()).expect("parses");
        f.height = 4;
        f.glyphs = [(b'A', 6u32), (b'B', 4)]
            .into_iter()
            .map(|(code, w)| Glyph { code, width: w, indices: vec![1u8; (w * 4) as usize] })
            .collect();

        // "AB" with zero spacing is 6 + 4 = 10 wide, 4 tall, indexed and palette-preserving.
        let img = render_text(&f, "AB", 0, 0);
        assert_eq!((img.width, img.height), (10, 4));
        assert!(img.indexed.is_some(), "a logo keeps the font palette");

        // A character the font lacks ('Z') advances by the average width but paints nothing, so
        // the canvas is wider than "AB" yet only "AB" is drawn.
        let with_gap = render_text(&f, "AZB", 0, 0);
        assert!(with_gap.width > img.width, "the missing glyph still advances the cursor");

        // Two lines stack; negative spacing overlaps rather than overflowing the canvas.
        let two = render_text(&f, "A\nB", 0, 0);
        assert_eq!(two.height, 8, "two rows of a 4px font");
        let kerned = render_text(&f, "AB", -2, 0);
        assert_eq!(kerned.width, 8, "-2 spacing pulls B two px into A");
    }

    /// Export a real font to a DRAW CBF `.bmp` for a look, and for dropping into
    /// `DRAW/ASSETS/FONTS/COLOR_BITMAP/`. Ignored — needs the archive.
    ///
    /// ```text
    /// AMIGA_FONT=<path to a size file> CBF_OUT=/tmp/out.bmp \
    ///   cargo test dump_cbf -- --ignored --nocapture
    /// ```
    /// Render a word from a real font to a PNG, for a look. Ignored — needs the archive.
    ///   AMIGA_FONT=<size file> LOGO_TEXT=HELLO LOGO_OUT=/tmp/logo.png \\
    ///     cargo test dump_logo -- --ignored --nocapture
    #[test]
    #[ignore]
    fn dump_logo() {
        let (Ok(src), Ok(out)) = (std::env::var("AMIGA_FONT"), std::env::var("LOGO_OUT")) else {
            println!("set AMIGA_FONT and LOGO_OUT");
            return;
        };
        let text = std::env::var("LOGO_TEXT").unwrap_or_else(|_| "AMIGA".into());
        let f = parse(&std::fs::read(&src).expect("read")).expect("parse");
        let img = render_text(&f, &text, 0, 0);
        image::save_buffer(&out, &img.rgba_bytes(), img.width, img.height, image::ColorType::Rgba8)
            .expect("write");
        println!("wrote {out}: {}x{} for {text:?}", img.width, img.height);
    }

    #[test]
    #[ignore]
    fn dump_cbf_for_draw() {
        let (Ok(src), Ok(out)) = (std::env::var("AMIGA_FONT"), std::env::var("CBF_OUT")) else {
            println!("set AMIGA_FONT=<size file> and CBF_OUT=<out.bmp>");
            return;
        };
        let f = parse(&std::fs::read(&src).expect("read")).expect("parse");
        let sheet = to_draw_cbf(&f).expect("export");
        let (dis, bg, glyphs) = draw_would_see(&sheet);
        println!(
            "{} — {}x{}, {} glyphs; DRAW sees bg {:?}, {} row-0 colours, {} glyphs",
            f.name, sheet.width, sheet.height, f.glyphs.len(), bg, dis, glyphs
        );
        image::save_buffer(
            &out,
            &sheet.rgba_bytes(),
            sheet.width,
            sheet.height,
            image::ColorType::Rgba8,
        )
        .expect("write bmp");
        println!("wrote {out}");
    }

    /// The real archive, when it happens to be on this machine. Ignored — it is a 46 MB download —
    /// but it is the check that matters: a synthetic fixture only proves the parser agrees with
    /// itself.
    #[test]
    #[ignore]
    fn parses_the_real_archive() {
        let Ok(root) = std::env::var("AMIGA_FONTS_DIR") else {
            println!("set AMIGA_FONTS_DIR=<extracted archive>/Fonts to run this");
            return;
        };
        let (mut ok, mut failed, mut color) = (0, 0, 0);
        for e in std::fs::read_dir(&root).expect("dir").flatten() {
            if !e.path().is_dir() {
                continue;
            }
            for sz in std::fs::read_dir(e.path()).expect("size dir").flatten() {
                let bytes = std::fs::read(sz.path()).unwrap_or_default();
                match parse(&bytes) {
                    Ok(f) => {
                        ok += 1;
                        if f.is_color {
                            color += 1;
                        }
                        assert!(!f.glyphs.is_empty(), "{:?} parsed with no glyphs", sz.path());
                    }
                    Err(err) => {
                        failed += 1;
                        println!("FAILED {:?}: {err:?}", sz.path());
                    }
                }
            }
        }
        println!("parsed {ok} size files ({color} colour), {failed} failed");
        assert!(ok > 100, "expected a real collection, got {ok}");
        assert_eq!(failed, 0, "every size file in the archive should parse");
    }
}
