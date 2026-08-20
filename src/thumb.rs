//! Thumbnail generation + a small background worker pool.
//!
//! Decoding and scaling happen off the UI thread; only the cheap CPU pixel
//! buffer crosses back, and the UI thread uploads it to a GPU texture lazily.
//! Scaling is split by direction: small sprites are kept at source res and the GPU
//! NEAREST-samples them up crisply, while big images are **area-averaged** down
//! (a box filter) so high-frequency block/shade art shrinks faithfully (a 50% dither
//! reads as 50% grey) instead of aliasing — those downscaled thumbs display LINEAR.

use crate::decode::cp437_font::CP437_8X16;
use crate::decode::cp437_font_8x8::CP437_8X8;
use rayon::prelude::*;
use crate::decode::Registry;
use crate::image_types::PixImage;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

pub struct ThumbResult {
    pub path: PathBuf,
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>, // width * height * 4
    // Source-image metadata, piggybacked on the decode the worker already does.
    pub src_w: u32,
    pub src_h: u32,
    pub colors: Option<usize>, // distinct colors, or None if too large to count
    // The image's palette for the details pane / .GPL export: the authoritative
    // palette for indexed art, else the distinct fully-opaque colors when ≤
    // SWATCH_CAP of them (None above that).
    pub palette: Option<Vec<[u8; 4]>>,
}

struct Job {
    path: PathBuf,
    target: u32,
}

pub struct ThumbBuilder {
    queue: Arc<(Mutex<Vec<Job>>, Condvar)>,
    results: Receiver<ThumbResult>,
    requested: HashSet<PathBuf>,
}

impl ThumbBuilder {
    pub fn new(registry: Arc<Registry>, workers: usize) -> Self {
        let queue: Arc<(Mutex<Vec<Job>>, Condvar)> =
            Arc::new((Mutex::new(Vec::new()), Condvar::new()));
        let (tx, rx): (Sender<ThumbResult>, Receiver<ThumbResult>) = channel();

        for _ in 0..workers.max(1) {
            let queue = Arc::clone(&queue);
            let tx = tx.clone();
            let registry = Arc::clone(&registry);
            std::thread::spawn(move || loop {
                let job = {
                    let (lock, cvar) = &*queue;
                    let mut q = lock.lock().unwrap();
                    while q.is_empty() {
                        q = cvar.wait(q).unwrap();
                    }
                    // LIFO: the most-recently-requested (visible) item first.
                    q.pop().unwrap()
                };
                if let Ok(img) = registry.decode_path(&job.path) {
                    let (w, h, rgba) = make_thumb(&img, job.target);
                    let colors = count_colors(&img);
                    let palette = extract_palette(&img);
                    let _ = tx.send(ThumbResult {
                        path: job.path,
                        width: w,
                        height: h,
                        rgba,
                        src_w: img.width,
                        src_h: img.height,
                        colors,
                        palette,
                    });
                }
            });
        }

        Self {
            queue,
            results: rx,
            requested: HashSet::new(),
        }
    }

    /// Enqueue once per path. Cheap to call every frame for visible items.
    pub fn request(&mut self, path: &Path, target: u32) {
        if self.requested.insert(path.to_path_buf()) {
            let (lock, cvar) = &*self.queue;
            lock.lock().unwrap().push(Job {
                path: path.to_path_buf(),
                target,
            });
            cvar.notify_one();
        }
    }

    pub fn drain(&self) -> Vec<ThumbResult> {
        self.results.try_iter().collect()
    }

    /// Forget that `path` was requested, so a later `request` re-decodes it (e.g. after its
    /// tile color changed). The caller also drops the cached texture.
    pub fn forget(&mut self, path: &Path) {
        self.requested.remove(path);
    }
}

/// Count distinct colors among the **fully-opaque** pixels (alpha 255). This
/// drops both fully-transparent pixels (generators leave RGB noise behind a
/// zeroed alpha) and the semi-transparent anti-aliased *edge* pixels (blended
/// in-between shades), so the total reflects the sprite's solid body colors.
/// Capped so a huge (non-pixel-art) image can't stall a worker — above → `None`.
fn count_colors(img: &PixImage) -> Option<usize> {
    const CAP: usize = 4_000_000;
    if img.pixels.len() > CAP {
        return None;
    }
    let mut seen: HashSet<[u8; 4]> = HashSet::with_capacity(256);
    for &p in &img.pixels {
        if p[3] != 255 {
            continue; // only fully-opaque pixels count
        }
        seen.insert(p);
    }
    Some(seen.len())
}

/// Most distinct colors we'll surface as a swatch palette / `.GPL`. Generous so
/// shaded/anti-aliased pixel art (which is RGBA with no index, often several
/// hundred colors) still gets a dynamic palette — but bounded so a photo doesn't
/// produce tens of thousands of swatches.
pub const SWATCH_CAP: usize = 4096;

/// Extract a palette for the details pane / .GPL export: the source's own
/// palette for indexed art (authoritative order, preserves unused slots), else
/// the distinct colors actually used when there are ≤ `SWATCH_CAP` of them (built
/// dynamically from the pixels), else `None` (too busy to be a useful palette).
fn extract_palette(img: &PixImage) -> Option<Vec<[u8; 4]>> {
    if let Some(idx) = &img.indexed {
        return Some(idx.palette.clone());
    }
    const PIXEL_CAP: usize = 4_000_000; // don't scan absurdly large images
    if img.pixels.len() > PIXEL_CAP {
        return None;
    }
    let mut seen: HashSet<[u8; 4]> = HashSet::with_capacity(512);
    for &p in &img.pixels {
        if p[3] != 255 {
            continue; // only fully-opaque pixels (skip transparent + AA edges)
        }
        seen.insert(p);
        if seen.len() > SWATCH_CAP {
            return None; // too many distinct colors to be a useful swatch palette
        }
    }
    let mut v: Vec<[u8; 4]> = seen.into_iter().collect();
    v.sort();
    Some(v)
}

/// Collect the distinct fully-opaque colors from a raw RGBA byte buffer (4 bytes
/// per pixel). Unlike [`extract_palette`] there's **no** `SWATCH_CAP` — this feeds
/// [`median_cut`], which reduces however many colors it's given, so it's how
/// "Reduce to N" builds a palette for a >`SWATCH_CAP` (photo/gradient) image that
/// has no useful swatch palette of its own. Transparent + partial-alpha pixels are
/// skipped (as in `extract_palette`); the result is sorted for determinism.
pub fn distinct_opaque_colors(rgba: &[u8]) -> Vec<[u8; 4]> {
    let mut seen: HashSet<[u8; 4]> = HashSet::with_capacity(4096);
    for px in rgba.chunks_exact(4) {
        if px[3] != 255 {
            continue; // only fully-opaque pixels (skip transparent + AA edges)
        }
        seen.insert([px[0], px[1], px[2], 255]);
    }
    let mut v: Vec<[u8; 4]> = seen.into_iter().collect();
    v.sort();
    v
}

/// Estimate the **per-axis** integer upscale factor of (pixel-)art: `(sx, sy)`, the
/// largest cell size in 2..=16 on each axis whose colour-change *edges* cluster onto
/// a period grid — i.e. the image looks like native pixels blown up sx× wide, sy×
/// tall (they differ for non-square pixel art). Either axis is 1 when it has no clean
/// grid (detailed / 1× art, or too few edges). Drives the dither "Auto" button.
///
/// How: an *edge* is where a pixel's RGB differs from its left (horizontal) or upper
/// (vertical) neighbour by more than `THRESH` (skips anti-alias gradients). For each
/// candidate period we bucket edge positions by `pos % s` and take the fullest
/// bucket's share — so a grid offset by a crop still scores ~1.0 at its true period.
/// Rows/cols are sampled for speed.
pub fn detect_pixel_scale(rgba: &[u8], w: usize, h: usize) -> (usize, usize) {
    if w < 4 || h < 4 || rgba.len() < w * h * 4 {
        return (1, 1);
    }
    const THRESH: i32 = 24; // min |ΔR|+|ΔG|+|ΔB| to count as an edge (skip AA)
    let opaque = |i: usize| rgba[i * 4 + 3] == 255;
    let diff = |a: usize, b: usize| {
        (rgba[a * 4] as i32 - rgba[b * 4] as i32).abs()
            + (rgba[a * 4 + 1] as i32 - rgba[b * 4 + 1] as i32).abs()
            + (rgba[a * 4 + 2] as i32 - rgba[b * 4 + 2] as i32).abs()
            > THRESH
    };
    // Sample up to ~64 rows / 64 columns evenly so huge images stay cheap.
    let row_step = (h / 64).max(1);
    let col_step = (w / 64).max(1);
    let mut hx: Vec<usize> = Vec::new(); // x of horizontal colour changes → sx
    let mut y = 0;
    while y < h {
        for x in 1..w {
            let (i, j) = (y * w + x, y * w + x - 1);
            if opaque(i) && opaque(j) && diff(i, j) {
                hx.push(x);
            }
        }
        y += row_step;
    }
    let mut vy: Vec<usize> = Vec::new(); // y of vertical colour changes → sy
    let mut x = 0;
    while x < w {
        for y in 1..h {
            let (i, j) = (y * w + x, (y - 1) * w + x);
            if opaque(i) && opaque(j) && diff(i, j) {
                vy.push(y);
            }
        }
        x += col_step;
    }
    // The largest period whose edges concentrate in one `pos % s` phase bucket (≥80%);
    // 1 when there's too little signal or no clean grid.
    let period = |edges: &[usize]| -> usize {
        if edges.len() < 16 {
            return 1;
        }
        for s in (2..=16).rev() {
            let mut buckets = vec![0u32; s];
            for &e in edges {
                buckets[e % s] += 1;
            }
            let best = *buckets.iter().max().unwrap_or(&0) as f32 / edges.len() as f32;
            if best >= 0.80 {
                return s;
            }
        }
        1
    };
    (period(&hx), period(&vy))
}

/// Parse a GIMP `.gpl` palette into opaque RGBA colors. Skips the header lines
/// (`GIMP Palette`, `Name:`, `Columns:`), `#` comments and blanks; each color
/// line is `R G B [name]` with space- or tab-separated 0..255 channels.
pub fn parse_gpl(text: &str) -> Vec<[u8; 4]> {
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let lower = t.to_ascii_lowercase();
        if lower.starts_with("gimp palette")
            || lower.starts_with("name:")
            || lower.starts_with("columns:")
        {
            continue;
        }
        let mut it = t.split_whitespace();
        let r = it.next().and_then(|s| s.parse::<u8>().ok());
        let g = it.next().and_then(|s| s.parse::<u8>().ok());
        let b = it.next().and_then(|s| s.parse::<u8>().ok());
        if let (Some(r), Some(g), Some(b)) = (r, g, b) {
            out.push([r, g, b, 255]);
        }
    }
    out
}

/// Reduce `colors` to at most `target` representatives via **median cut** — a
/// classic, deterministic palette reduction. Repeatedly split the color box with
/// the widest single-channel spread at that channel's median, then average each
/// final box. Alpha is forced opaque (the input is the opaque palette). The
/// result is sorted + deduped, so it may be a hair under `target`.
pub fn median_cut(colors: &[[u8; 4]], target: usize) -> Vec<[u8; 4]> {
    let target = target.max(1);
    if colors.len() <= target {
        let mut v = colors.to_vec();
        v.sort();
        v.dedup();
        return v;
    }
    let mut boxes: Vec<Vec<[u8; 4]>> = vec![colors.to_vec()];
    while boxes.len() < target {
        // Split the splittable box with the largest single-channel range.
        let pick = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.len() > 1)
            .max_by_key(|(_, b)| widest_range(b))
            .map(|(i, _)| i);
        let Some(idx) = pick else {
            break; // every box is a single color
        };
        let b = boxes.swap_remove(idx);
        let (lo, hi) = split_box(b);
        boxes.push(lo);
        boxes.push(hi);
    }
    let mut out: Vec<[u8; 4]> = boxes
        .iter()
        .filter(|b| !b.is_empty())
        .map(|b| average_color(b))
        .collect();
    out.sort();
    out.dedup();
    out
}

fn channel_minmax(colors: &[[u8; 4]]) -> ([u8; 3], [u8; 3]) {
    let mut mn = [255u8; 3];
    let mut mx = [0u8; 3];
    for c in colors {
        for ch in 0..3 {
            mn[ch] = mn[ch].min(c[ch]);
            mx[ch] = mx[ch].max(c[ch]);
        }
    }
    (mn, mx)
}

fn widest_range(colors: &[[u8; 4]]) -> u8 {
    let (mn, mx) = channel_minmax(colors);
    (0..3).map(|ch| mx[ch] - mn[ch]).max().unwrap_or(0)
}

fn split_box(mut b: Vec<[u8; 4]>) -> (Vec<[u8; 4]>, Vec<[u8; 4]>) {
    let (mn, mx) = channel_minmax(&b);
    let ch = (0..3).max_by_key(|&ch| mx[ch] - mn[ch]).unwrap_or(0);
    b.sort_by_key(|c| c[ch]);
    let hi = b.split_off(b.len() / 2);
    (b, hi)
}

fn average_color(colors: &[[u8; 4]]) -> [u8; 4] {
    let n = colors.len().max(1) as u32;
    let mut s = [0u32; 3];
    for c in colors {
        for ch in 0..3 {
            s[ch] += c[ch] as u32;
        }
    }
    [(s[0] / n) as u8, (s[1] / n) as u8, (s[2] / n) as u8, 255]
}

/// Decode `path` synchronously and return its thumbnail-sized RGBA buffer — the
/// source pixels for the details pane's reduced-palette preview (same scaling the
/// worker uses, just on the calling thread for a single inspected image).
pub fn decode_thumb(
    registry: &Registry,
    path: &std::path::Path,
    max: u32,
) -> Option<(usize, usize, Vec<u8>)> {
    let img = registry.decode_path(path).ok()?;
    Some(make_thumb(&img, max))
}

/// Snap each opaque pixel's RGB to the nearest color in `palette` (squared RGB
/// distance) — the live preview of a reduced palette. Fully-transparent pixels
/// are left untouched and alpha is preserved. Memoizes per source color, so the
/// per-pixel cost is a hash lookup once a color has been resolved.
pub fn remap_to_palette(rgba: &mut [u8], palette: &[[u8; 4]]) {
    if palette.is_empty() {
        return;
    }
    let mut cache: std::collections::HashMap<[u8; 3], [u8; 3]> = HashMap::new();
    for px in rgba.chunks_exact_mut(4) {
        if px[3] == 0 {
            continue; // invisible — leave as-is
        }
        let key = [px[0], px[1], px[2]];
        let near = *cache
            .entry(key)
            .or_insert_with(|| nearest_color(key, palette));
        px[0] = near[0];
        px[1] = near[1];
        px[2] = near[2];
    }
}

/// Dither method names (index = id), a small useful subset of IMG2PAL's set.
/// Indices 1–3 are *ordered* (Bayer) and 6 is a user-editable ordered matrix —
/// these are pure pre-quantization biases, so the Dither op can sit anywhere in
/// the pipeline. Indices 4–5 are *error-diffusion* and need a palette target, so
/// they only do something when a palette/Reduce is active at the dither step.
pub const DITHER_NAMES: &[&str] = &[
    "None",
    "Bayer 2×2",
    "Bayer 4×4",
    "Bayer 8×8",
    "Floyd–Steinberg",
    "Atkinson",
    "Custom",
    "ANSI Shade",
    "PETSCII",
    "ASCII",
    "ATASCII",
    "Apple ][",
    "REXPaint font",
    "Unicode",
];

/// `DITHER_NAMES` index for the user-editable custom matrix.
pub const DITHER_CUSTOM: u8 = 6;

/// `DITHER_NAMES` index for the textmode/ANSI shade-block renderer. Unlike the
/// other modes this one paints CP437 glyphs (space ░▒▓█ + half-blocks) drawn in a
/// two-colour palette per cell — a hard-quantized, blocky "ANSI art" look. Needs a
/// palette (like error-diffusion), and it already outputs palette colours so a
/// following Palette snap is a no-op.
pub const DITHER_ANSI: u8 = 7;

/// `DITHER_NAMES` index for the image→PETSCII (C64 hi-res char art) converter. Unlike the
/// others this one has its OWN matcher ([`petscii_grid`]) + fixed VIC-II palette + serializers,
/// so it takes a separate preview/export path (it does not reuse the shade grid or a chosen
/// palette).
pub const DITHER_PETSCII: u8 = 8;

/// `DITHER_NAMES` index for the image→ASCII (character-density) converter. Like ANSI Shade it
/// produces an [`AnsiGrid`] (so it reuses the whole `.ans`/`.xb`/`.tnd` export + preview render),
/// but instead of the two-colour shade matcher it maps each cell's brightness to a glyph on a
/// coverage-sorted ramp built from the enabled character ranges (32–126 always, + control 0–31,
/// + high 128–255). Colour is per-cell from the active palette (or monochrome).
pub const DITHER_ASCII: u8 = 9;

/// `DITHER_NAMES` index for the image→ATASCII (Atari 8-bit character art) converter. A generic
/// bit-font density converter ([`bitfont_pass`]) over the Atari ROM font.
pub const DITHER_ATASCII: u8 = 10;

/// `DITHER_NAMES` index for the image→Apple ][ character-art converter (Apple II text font, with
/// optional MouseText glyphs + inverse video), via the same [`bitfont_pass`].
pub const DITHER_APPLE: u8 = 11;

/// `DITHER_NAMES` index for the image→char-art converter over a **selected REXPaint font**
/// (any cell size), via [`glyphfont_pass`].
pub const DITHER_REXFONT: u8 = 12;

/// `DITHER_NAMES` index for the image→**Unicode** text-art converter (half-block colour cells or
/// Braille dot cells), which also exports real copy-pasteable UTF-8 ([`unicode_pass`]).
pub const DITHER_UNICODE: u8 = 13;

/// Unicode-art style: `▀` upper-half blocks — each character is 1×2 truecolour pixels.
pub const UNI_HALFBLOCK: u8 = 0;
/// Unicode-art style: Braille (U+2800..) — each character is a 2×4 dot cell, hi-res mono-ish.
pub const UNI_BRAILLE: u8 = 1;
/// Unicode-art style: density **ramp** over the enabled Unicode ranges (Box Drawing / Block
/// Elements / Geometric Shapes / Braille / ASCII), rendered via the bundled DejaVu font.
pub const UNI_RAMP: u8 = 2;

/// Build a UTF-8 [`UniGrid`] from a density [`BitGrid`] (glyph indices into a ramp font) + the
/// parallel codepoint list `chars`. Colours come from the grid's palette. Used by the Ramp style
/// so it shares the render ([`glyphfont_render`]) and text serializer ([`unicode_to_text`]).
pub fn bitgrid_to_unigrid(grid: &BitGrid, chars: &[char]) -> UniGrid {
    let rgb = |idx: u8| -> [u8; 3] {
        grid.palette
            .get(idx as usize)
            .map(|p| [p[0], p[1], p[2]])
            .unwrap_or([0, 0, 0])
    };
    let (mut chs, mut fg, mut bg) = (Vec::new(), Vec::new(), Vec::new());
    for c in &grid.cells {
        chs.push(chars.get(c.glyph as usize).copied().unwrap_or(' '));
        fg.push(rgb(c.fg));
        bg.push(rgb(c.bg));
    }
    UniGrid { cols: grid.cols, rows: grid.rows, style: UNI_RAMP, chars: chs, fg, bg }
}

// 0..n²-1 ordered-dither (Bayer) threshold matrices.
const BAYER2: [u32; 4] = [0, 2, 3, 1];
#[rustfmt::skip]
const BAYER4: [u32; 16] = [
     0,  8,  2, 10,
    12,  4, 14,  6,
     3, 11,  1,  9,
    15,  7, 13,  5,
];
#[rustfmt::skip]
const BAYER8: [u32; 64] = [
     0, 32,  8, 40,  2, 34, 10, 42,
    48, 16, 56, 24, 50, 18, 58, 26,
    12, 44,  4, 36, 14, 46,  6, 38,
    60, 28, 52, 20, 62, 30, 54, 22,
     3, 35, 11, 43,  1, 33,  9, 41,
    51, 19, 59, 27, 49, 17, 57, 25,
    15, 47,  7, 39, 13, 45,  5, 37,
    63, 31, 55, 23, 61, 29, 53, 21,
];

// (dx, dy, weight) error-diffusion kernels.
const FLOYD_STEINBERG: &[(i32, i32, f32)] = &[
    (1, 0, 7. / 16.),
    (-1, 1, 3. / 16.),
    (0, 1, 5. / 16.),
    (1, 1, 1. / 16.),
];
const ATKINSON: &[(i32, i32, f32)] = &[
    (1, 0, 0.125),
    (2, 0, 0.125),
    (-1, 1, 0.125),
    (0, 1, 0.125),
    (1, 1, 0.125),
    (0, 2, 0.125),
];

/// The built-in Bayer matrix for an `n×n` size (2/4/8), as the seed for the
/// custom-matrix editor. Falls back to the 4×4 for any other size.
pub fn bayer_values(n: usize) -> Vec<u32> {
    match n {
        2 => BAYER2.to_vec(),
        8 => BAYER8.to_vec(),
        _ => BAYER4.to_vec(),
    }
}

/// Apply the dither step at its slot in the pipeline. Ordered methods (Bayer
/// 2/4/8 and `custom`) lay down a pure threshold bias and leave the snapping to
/// the later Palette step — so they work even with no palette (e.g. dithered
/// posterize banding). Error-diffusion (Floyd–Steinberg/Atkinson) needs a target,
/// so it quantizes toward `palette` here, or no-ops if none is active.
/// `scale_x`/`scale_y` (≥1) enlarge each ordered-dither cell to span
/// `scale_x`×`scale_y` pixels — so on high-resolution art a Bayer pattern reads as a
/// proper crosshatch instead of single-pixel noise, and a non-square cell can match
/// non-square art. Ignored by the error-diffusion methods (no fixed cell). Pass 1,1
/// for the classic 1-px pattern.
#[allow(clippy::too_many_arguments)]
pub fn dither_pass(
    rgba: &mut [u8],
    w: usize,
    h: usize,
    method: u8,
    amount: f32,
    custom: &[u32],
    custom_n: usize,
    scale_x: usize,
    scale_y: usize,
    palette: Option<&[[u8; 4]]>,
) {
    if method == 0 || amount <= 0.0 {
        return;
    }
    let (sx, sy) = (scale_x.max(1), scale_y.max(1));
    match method {
        1 => ordered_bias(rgba, w, h, &BAYER2, 2, sx, sy, amount),
        2 => ordered_bias(rgba, w, h, &BAYER4, 4, sx, sy, amount),
        3 => ordered_bias(rgba, w, h, &BAYER8, 8, sx, sy, amount),
        4 => {
            if let Some(p) = palette {
                diffuse(rgba, w, h, p, amount, FLOYD_STEINBERG);
            }
        }
        5 => {
            if let Some(p) = palette {
                diffuse(rgba, w, h, p, amount, ATKINSON);
            }
        }
        DITHER_CUSTOM if custom_n >= 1 && custom.len() >= custom_n * custom_n => {
            ordered_bias(rgba, w, h, custom, custom_n, sx, sy, amount);
        }
        _ => {}
    }
}

/// Ordered (Bayer/custom) dither *bias*: nudge each opaque pixel up/down by its
/// `matrix` threshold so a later quantize (Palette or Posterize) breaks into a
/// stable crosshatch. No snapping happens here — that's what makes it movable.
/// `scale_x`/`scale_y` (≥1) make each matrix cell span `scale_x`×`scale_y` pixels.
#[allow(clippy::too_many_arguments)]
fn ordered_bias(
    rgba: &mut [u8],
    w: usize,
    h: usize,
    matrix: &[u32],
    n: usize,
    scale_x: usize,
    scale_y: usize,
    amount: f32,
) {
    let strength = amount * 64.0; // bias span in 0..255 space
    let denom = (n * n) as f32;
    let (sx, sy) = (scale_x.max(1), scale_y.max(1));
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            if rgba[i + 3] != 255 {
                continue;
            }
            let m = matrix[((y / sy) % n) * n + ((x / sx) % n)] as f32 / denom - 0.5;
            let bias = (m * strength) as i32;
            rgba[i] = (rgba[i] as i32 + bias).clamp(0, 255) as u8;
            rgba[i + 1] = (rgba[i + 1] as i32 + bias).clamp(0, 255) as u8;
            rgba[i + 2] = (rgba[i + 2] as i32 + bias).clamp(0, 255) as u8;
        }
    }
}

/// Error-diffusion dithering (Floyd–Steinberg / Atkinson): quantize each pixel,
/// then push its (scaled) error into not-yet-visited opaque neighbors.
fn diffuse(
    rgba: &mut [u8],
    w: usize,
    h: usize,
    palette: &[[u8; 4]],
    amount: f32,
    kernel: &[(i32, i32, f32)],
) {
    let mut work: Vec<[f32; 3]> = (0..w * h)
        .map(|p| {
            [
                rgba[p * 4] as f32,
                rgba[p * 4 + 1] as f32,
                rgba[p * 4 + 2] as f32,
            ]
        })
        .collect();
    for y in 0..h {
        for x in 0..w {
            let idx = y * w + x;
            if rgba[idx * 4 + 3] != 255 {
                continue;
            }
            let old = work[idx];
            let c = [
                old[0].clamp(0., 255.) as u8,
                old[1].clamp(0., 255.) as u8,
                old[2].clamp(0., 255.) as u8,
            ];
            let near = nearest_color(c, palette);
            rgba[idx * 4] = near[0];
            rgba[idx * 4 + 1] = near[1];
            rgba[idx * 4 + 2] = near[2];
            let err = [
                (old[0] - near[0] as f32) * amount,
                (old[1] - near[1] as f32) * amount,
                (old[2] - near[2] as f32) * amount,
            ];
            for &(dx, dy, wgt) in kernel {
                let nx = x as i32 + dx;
                let ny = y as i32 + dy;
                if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                    continue;
                }
                let nidx = ny as usize * w + nx as usize;
                if rgba[nidx * 4 + 3] != 255 {
                    continue; // don't leak error into transparent pixels
                }
                work[nidx][0] += err[0] * wgt;
                work[nidx][1] += err[1] * wgt;
                work[nidx][2] += err[2] * wgt;
            }
        }
    }
}

fn nearest_color(c: [u8; 3], palette: &[[u8; 4]]) -> [u8; 3] {
    let mut best = [palette[0][0], palette[0][1], palette[0][2]];
    let mut best_d = u32::MAX;
    for p in palette {
        let dr = c[0] as i32 - p[0] as i32;
        let dg = c[1] as i32 - p[1] as i32;
        let db = c[2] as i32 - p[2] as i32;
        let d = (dr * dr + dg * dg + db * db) as u32;
        if d < best_d {
            best_d = d;
            best = [p[0], p[1], p[2]];
        }
    }
    best
}

/// Index of the palette entry nearest to `c` (squared RGB distance). Companion to
/// [`nearest_color`] for the ANSI shade renderer, which tracks palette *indices*
/// (a cell's fg/bg) rather than RGB triples. Assumes a non-empty palette.
fn nearest_index(c: [u8; 3], palette: &[[u8; 4]]) -> u8 {
    let mut best = 0u8;
    let mut best_d = u32::MAX;
    for (i, p) in palette.iter().enumerate() {
        let dr = c[0] as i32 - p[0] as i32;
        let dg = c[1] as i32 - p[1] as i32;
        let db = c[2] as i32 - p[2] as i32;
        let d = (dr * dr + dg * dg + db * db) as u32;
        if d < best_d {
            best_d = d;
            best = i as u8;
        }
    }
    best
}

// ---------------------------------------------------------------------------
// ANSI shade-block ("textmode") renderer
// ---------------------------------------------------------------------------

/// One text-mode cell: a CP437 glyph `ch` drawn in two palette colours (`fg`/`bg`
/// are PALETTE INDICES, so the grid also round-trips to an `.ans` file).
#[derive(Clone, Copy)]
pub struct AnsiCell {
    pub fg: u8,
    pub bg: u8,
    pub ch: u8,
}

/// A full text-mode screen: `cols`×`rows` cells over a `cell_w`×`cell_h` pixel
/// grid, drawn from `palette`. Produced by [`ansi_shade_grid`]; rendered back into
/// pixels by [`ansi_shade_pass`] or serialized by [`ansi_grid_to_ans`].
pub struct AnsiGrid {
    pub cols: usize,
    pub rows: usize,
    pub cell_w: usize,
    pub cell_h: usize,
    pub palette: Vec<[u8; 4]>,
    pub cells: Vec<AnsiCell>,
}

/// The five shade coverages, paired with their CP437 glyphs: space ░ ▒ ▓ █. The
/// middle three fractions are user-tunable (`f1`/`f2`/`f3`), so a palette pair can
/// hit intermediate tones the way classic ANSI art did.
const SHADE_GLYPHS: [u8; 5] = [32, 176, 177, 178, 219];

/// Squared RGB distance between two f32 triples.
#[inline]
fn dist2(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dr = a[0] - b[0];
    let dg = a[1] - b[1];
    let db = a[2] - b[2];
    dr * dr + dg * dg + db * db
}

/// Squared distance between two colours with each one's own brightness removed — i.e.
/// how differently they're *tinted*, independent of how light/dark they are. Subtract a
/// colour's mean (its grey/luma component) and what remains is its chroma vector; grey
/// has a zero chroma vector, so two greys (or white↔black) are chroma-distance 0, while
/// two different HUES (yellow↔blue) are far apart. Used to penalise dithering two hues
/// together (false colour) while leaving brightness-only shading free.
#[inline]
fn chroma_dist2(a: [f32; 3], b: [f32; 3]) -> f32 {
    let la = (a[0] + a[1] + a[2]) * (1.0 / 3.0);
    let lb = (b[0] + b[1] + b[2]) * (1.0 / 3.0);
    let dr = (a[0] - la) - (b[0] - lb);
    let dg = (a[1] - la) - (b[1] - lb);
    let db = (a[2] - la) - (b[2] - lb);
    dr * dr + dg * dg + db * db
}

/// Characteristic palette spacing: the median nearest-neighbour squared distance among
/// the palette colours. A cell sitting at the midpoint of two adjacent colours has a
/// solid-error of about spacing/4, so this is what scales the shading-amount threshold —
/// the whole 0..1 slider stays useful whether the palette is EGA-16 or a dense 256-set.
fn palette_spacing(pf: &[[f32; 3]]) -> f32 {
    if pf.len() < 2 {
        return 20000.0;
    }
    let mut nn: Vec<f32> = Vec::with_capacity(pf.len());
    for (i, a) in pf.iter().enumerate() {
        let mut best = f32::MAX;
        for (j, b) in pf.iter().enumerate() {
            if i != j {
                best = best.min(dist2(*a, *b));
            }
        }
        nn.push(best);
    }
    nn.sort_by(|a, b| a.partial_cmp(b).unwrap());
    nn[nn.len() / 2]
}

/// Analyze `rgba` into an [`AnsiGrid`]: for each `cell_w`×`cell_h` block pick the
/// two palette colours + glyph (a shade level or a half-block) that best match the
/// block's tone(s). Edge cells are clamped to the image bounds. Fully-transparent
/// cells become a space in palette entry 0.
#[allow(clippy::too_many_arguments)]
pub fn ansi_shade_grid(
    rgba: &[u8],
    w: usize,
    h: usize,
    palette: &[[u8; 4]],
    cell_w: usize,
    cell_h: usize,
    f1: f32,
    f2: f32,
    f3: f32,
    half_blocks: bool,
    f1_on: bool,
    f2_on: bool,
    f3_on: bool,
    half_on: [bool; 4],
    half_use: [f32; 4],
    shade_amount: f32,
    ice: bool,
    smooth: f32,
    detail: f32,
    allowed: Option<&[bool]>,
) -> AnsiGrid {
    // Glyph-picker gate: which of the shade/half candidates the matcher may use (space stays a
    // fallback). None = all. Codes outside the block set are irrelevant to this matcher.
    let ok = |g: u8| -> bool { allowed.is_none_or(|a| a.get(g as usize).copied().unwrap_or(true)) };
    let cw = cell_w.max(1);
    let ch_ = cell_h.max(1);
    let cols = w.div_ceil(cw);
    let rows = h.div_ceil(ch_);
    let mut cells = Vec::with_capacity(cols * rows);
    if palette.is_empty() || w == 0 || h == 0 {
        // Nothing to match against — emit an all-space grid so callers stay simple.
        cells.resize(cols * rows, AnsiCell { fg: 0, bg: 0, ch: 32 });
        return AnsiGrid { cols, rows, cell_w: cw, cell_h: ch_, palette: palette.to_vec(), cells };
    }
    // The active (coverage, glyph) candidates: always the space (0.0) and full block
    // (1.0); the ░▒▓ mid levels only when their per-shade toggle is on. Each mid-shade
    // is clamped to its OWN interior band (light→mid→dark) so it can never reach 0.0 or
    // 1.0 — a coverage of exactly 0/1 turns a shade into a fake penalty-free solid that
    // then out-competes █ and swallows the whole image (the "only F3" bug). The bands
    // also keep the ramp ordered near each glyph's true fill (░≈¼ ▒≈½ ▓≈¾).
    let mut coverages: Vec<(f32, u8)> = Vec::with_capacity(5);
    coverages.push((0.0, SHADE_GLYPHS[0])); // space (always — the fallback glyph)
    if f1_on && ok(SHADE_GLYPHS[1]) {
        coverages.push((f1.clamp(0.10, 0.40), SHADE_GLYPHS[1])); // ░
    }
    if f2_on && ok(SHADE_GLYPHS[2]) {
        coverages.push((f2.clamp(0.40, 0.60), SHADE_GLYPHS[2])); // ▒
    }
    if f3_on && ok(SHADE_GLYPHS[3]) {
        coverages.push((f3.clamp(0.60, 0.90), SHADE_GLYPHS[3])); // ▓
    }
    if ok(SHADE_GLYPHS[4]) {
        coverages.push((1.0, SHADE_GLYPHS[4])); // █
    }
    // Palette as f32 triples for the joint search.
    let pf: Vec<[f32; 3]> = palette
        .iter()
        .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
        .collect();
    // "Shading amount": how far a cell's average must sit from the nearest SOLID
    // palette colour before shade/half-block glyphs are allowed. Low amount → big
    // threshold → flats stay a solid █, so only genuine transitions get shaded.
    // The max threshold is scaled to the palette's spacing (≈ the solid-error of a
    // midpoint cell = spacing/4), so the FULL 0..1 slider is useful on any palette.
    let max_threshold = palette_spacing(&pf) * 0.25;
    // "Shading" now runs 0..2. 0..1 is the flat-cell THRESHOLD (low → big threshold → flats
    // stay a solid █, only real transitions shade). Above 1 it FORCES dithering: the flat
    // gate is off and solid glyphs (space/█) pay a penalty that grows with the amount, so
    // shade pairs win even on near-flat cells — the whole canvas goes textured at 2.0.
    let amt = shade_amount.clamp(0.0, 2.0);
    let threshold = (1.0 - amt).max(0.0) * max_threshold;
    let solid_penalty = (amt - 1.0).max(0.0) * palette_spacing(&pf) * 0.5;
    // "Smoothness" = false-colour avoidance. The greedy pair search would happily dither
    // two DIFFERENT HUES that average to the target (yellow▒blue → grey) — garish false
    // colour the source never had. We penalise mid-shade glyphs (░▒▓) by their CHROMA
    // distance (hue difference with brightness removed), so mixing hues is costly but
    // brightness-only shading (grey▓grey, white▒black) — which has ~zero chroma distance
    // — stays free at ANY Smoothness. A baseline weight is always applied so greys never
    // dither into colour even at Smoothness 0; the slider adds more on top (now 0..3 for a
    // much harder clamp on hue-mixing). Solids and half-blocks are exempt (see the loop).
    let smooth_w = smooth.clamp(0.0, 3.0);
    let chroma_w = 0.10 + 0.40 * smooth_w;
    // Perf: a cell this small can't show a visible shade pattern, so it always
    // renders as a flat colour — a constant of the effective cell size, hoisted out.
    let tiny_cell = cw < 3 || ch_ < 3;
    // Candidate fg/bg set for the pair search. Small palettes (EGA is 16) search
    // ALL indices — no per-cell sort, no allocation. Big palettes fall back to the
    // ~6 nearest, refilling a SINGLE scratch buffer each cell (never a fresh Vec).
    let small_pal = pf.len() <= 32;
    let all_idx: Vec<usize> = (0..pf.len()).collect();
    // Which palette entries are legal as a BACKGROUND? Standard textmode has only 8
    // backgrounds (the non-bright ANSI slots 0–7); iCE-color mode unlocks all 16. So
    // a palette colour that maps to a bright ANSI slot (≥8) can't be a bg unless iCE —
    // gate the search on this so the on-screen preview matches a real non-iCE .ans.
    let bg_ok: Vec<bool> = pf
        .iter()
        .map(|c| ice || nearest_ansi16([c[0] as u8, c[1] as u8, c[2] as u8]) < 8)
        .collect();
    // Compute the grid in PARALLEL, one row per task. Each cell is an independent
    // function of the (read-only) source + palette, so rows fan out across cores while
    // writing into a pre-sized buffer preserves the exact serial order — the grid, and
    // thus preview==export, is byte-identical to the single-threaded result. This is the
    // live-preview hot loop, so the speedup is what keeps slider drags smooth.
    cells.resize(cols * rows, AnsiCell { fg: 0, bg: 0, ch: 32 });
    cells.par_chunks_mut(cols).enumerate().for_each(|(cy, row)| {
        // Per-task scratch for the big-palette nearest-6 candidate list — no shared state.
        let mut cand_buf: Vec<usize> = Vec::with_capacity(pf.len());
        // `cx` is a cell COORDINATE (drives x0 = cx*cw, midx, …), not just the row index.
        #[allow(clippy::needless_range_loop)]
        for cx in 0..cols {
            let x0 = cx * cw;
            let y0 = cy * ch_;
            let x1 = (x0 + cw).min(w);
            let y1 = (y0 + ch_).min(h);
            // Region means, skipping transparent pixels. Halves split by the cell's
            // authored geometry (not the clamped region) so partial edge cells stay sane.
            let (mut sum, mut n) = ([0f32; 3], 0u32);
            let (mut top, mut tn) = ([0f32; 3], 0u32);
            let (mut bot, mut bn) = ([0f32; 3], 0u32);
            let (mut left, mut ln) = ([0f32; 3], 0u32);
            let (mut right, mut rn) = ([0f32; 3], 0u32);
            let midy = y0 + ch_ / 2;
            let midx = x0 + cw / 2;
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) * 4;
                    if rgba[i + 3] == 0 {
                        continue; // skip transparent
                    }
                    let c = [rgba[i] as f32, rgba[i + 1] as f32, rgba[i + 2] as f32];
                    sum[0] += c[0]; sum[1] += c[1]; sum[2] += c[2]; n += 1;
                    if y < midy { top[0]+=c[0]; top[1]+=c[1]; top[2]+=c[2]; tn+=1; }
                    else { bot[0]+=c[0]; bot[1]+=c[1]; bot[2]+=c[2]; bn+=1; }
                    if x < midx { left[0]+=c[0]; left[1]+=c[1]; left[2]+=c[2]; ln+=1; }
                    else { right[0]+=c[0]; right[1]+=c[1]; right[2]+=c[2]; rn+=1; }
                }
            }
            if n == 0 {
                row[cx] = AnsiCell { fg: 0, bg: 0, ch: 32 }; // wholly transparent
                continue;
            }
            let avg = [sum[0] / n as f32, sum[1] / n as f32, sum[2] / n as f32];
            let solid_idx = nearest_index([avg[0] as u8, avg[1] as u8, avg[2] as u8], palette);
            // Tiny-cell short-circuit: a <3px cell can't render a shade pattern, so
            // it's always a flat colour — skip the whole search (makes 1×1 instant).
            if tiny_cell {
                row[cx] = AnsiCell { fg: solid_idx, bg: solid_idx, ch: 219 };
                continue;
            }
            // Shading-amount gate: if `avg` is already close to a solid palette colour
            // (within `threshold`), keep the cell a flat █ and skip the shade search —
            // so large flat regions stay solid instead of being needlessly dithered. When
            // Shading > 1 (`solid_penalty` on), the gate is bypassed so even flats get to
            // dither.
            let solid_err = dist2(avg, pf[solid_idx as usize]);
            if solid_err <= threshold && solid_penalty <= 0.0 {
                row[cx] = AnsiCell { fg: solid_idx, bg: solid_idx, ch: 219 };
                continue;
            }
            let mean = |acc: [f32; 3], cnt: u32| {
                if cnt == 0 { avg } else { [acc[0]/cnt as f32, acc[1]/cnt as f32, acc[2]/cnt as f32] }
            };
            let top_avg = mean(top, tn);
            let bot_avg = mean(bot, bn);
            let left_avg = mean(left, ln);
            let right_avg = mean(right, rn);

            // --- SHADE candidate: joint search over (bg, fg, coverage). Small
            // palettes search all colours; big ones restrict fg/bg to the ~6 nearest
            // `avg`, computed into the reused `cand_buf` (no per-cell allocation). ---
            let cand: &[usize] = if small_pal {
                &all_idx
            } else {
                cand_buf.clear();
                cand_buf.extend_from_slice(&all_idx);
                cand_buf.sort_by(|&a, &b| {
                    dist2(pf[a], avg).partial_cmp(&dist2(pf[b], avg)).unwrap()
                });
                cand_buf.truncate(6);
                &cand_buf
            };
            let mut best_ch = 32u8;
            let mut best_fg = nearest_index([avg[0] as u8, avg[1] as u8, avg[2] as u8], palette);
            let mut best_bg = best_fg;
            let mut best_err = f32::MAX;
            for &bg in cand {
                if !bg_ok[bg] {
                    continue; // bright bg only allowed in iCE mode
                }
                for &fg in cand {
                    // Mid-shade glyphs (░▒▓, 0 < cov < 1) mix BOTH colours, so pay a CHROMA
                    // penalty: mixing two different hues (false colour) is costly, but mixing
                    // brightness (grey▓grey shading) is free. It depends only on the fg/bg
                    // pair, so hoist it out of the coverage loop. Solids (space cov 0 → only
                    // bg; █ cov 1 → only fg) show one colour → no penalty; they always win a
                    // flat/near-flat cell.
                    let chroma_pen = chroma_w * chroma_dist2(pf[fg], pf[bg]);
                    for &(cov, glyph) in &coverages {
                        let pred = [
                            pf[fg][0] * cov + pf[bg][0] * (1.0 - cov),
                            pf[fg][1] * cov + pf[bg][1] * (1.0 - cov),
                            pf[fg][2] * cov + pf[bg][2] * (1.0 - cov),
                        ];
                        let err_eff = if cov != 0.0 && cov != 1.0 {
                            dist2(avg, pred) + chroma_pen
                        } else {
                            // space (cov 0) / █ (cov 1) are the SOLIDS — penalise them when
                            // Shading > 1 so shade pairs win even on flats (forced dither).
                            dist2(avg, pred) + solid_penalty
                        };
                        if err_eff < best_err {
                            best_err = err_eff;
                            best_fg = fg as u8;
                            best_bg = bg as u8;
                            best_ch = glyph;
                        }
                    }
                }
            }

            // --- HALF-BLOCK candidates: all four directions, each its own candidate so
            // the per-glyph toggle + usage slider (F5 ▀ / F6 ▄ / F7 ▌ / F8 ▐) can bias
            // which one the search prefers. ▀/▄ are the same horizontal split with fg/bg
            // swapped, ▌/▐ the same vertical split — but which CHARACTER gets written
            // still matters to the artist. `half_use[i]` in 0..1 scales the candidate's
            // error (0.5 = neutral); higher use → smaller error → the glyph wins more
            // cells, lower → fewer. The bg half must land in a legal background slot
            // (non-iCE), else the candidate is skipped. ---
            if half_blocks {
                // (glyph, fg-half average, bg-half average) in F5..F8 order. ▀/▄ are the
                // same horizontal split with fg/bg swapped (rows 0–6 | 7–15), ▌/▐ the same
                // vertical split — so which of a pair is chosen is a CHARACTER choice, not a
                // visual one. Listed with ▀/▌ first so, at equal usage, the strict `<` below
                // keeps them the default (matching classic art) unless F6/F8 is dialed up.
                let halves: [(u8, [f32; 3], [f32; 3]); 4] = [
                    (223, top_avg, bot_avg),    // ▀ F5: top = fg, bottom = bg
                    (220, bot_avg, top_avg),    // ▄ F6: bottom = fg, top = bg
                    (221, left_avg, right_avg), // ▌ F7: left = fg, right = bg
                    (222, right_avg, left_avg), // ▐ F8: right = fg, left = bg
                ];
                // Pick the best-scoring enabled half-block. `half_use` in 0..1 shifts a
                // candidate's error by up to ±max_threshold (scaled to the palette so it's
                // resolution/palette-invariant): >0.5 favours the glyph, <0.5 suppresses it.
                let mut hb_err = f32::MAX;
                let mut hb: Option<(u8, u8, u8)> = None;
                for (i, &(glyph, fg_avg, bg_avg)) in halves.iter().enumerate() {
                    if !half_on[i] || !ok(glyph) {
                        continue;
                    }
                    let fg = nearest_index([fg_avg[0] as u8, fg_avg[1] as u8, fg_avg[2] as u8], palette);
                    let bg = nearest_index([bg_avg[0] as u8, bg_avg[1] as u8, bg_avg[2] as u8], palette);
                    if !bg_ok[bg as usize] {
                        continue;
                    }
                    let err = 0.5 * dist2(fg_avg, pf[fg as usize]) + 0.5 * dist2(bg_avg, pf[bg as usize]);
                    let use_i = half_use[i].clamp(0.0, 1.0);
                    // DETAIL: reward the half-block in proportion to how DIFFERENT its two
                    // halves are — a big top/bottom (or left/right) contrast is real sub-cell
                    // structure that a shade or solid would blur into one tone. This is what
                    // keeps a shrunk image sharp: cells carrying an edge become a crisp
                    // half-block instead of grey mush. Scaled by the "Detail" weight AND each
                    // glyph's usage, so Detail dials retention globally and F5–F8 per
                    // direction; the flat ±bias still shifts the baseline.
                    // Two NORMALISED (0..1) factors gate the retention reward so no Detail
                    // setting can manufacture the white speckle a raw reward produced:
                    //  • `contrast` — the sub-cell top/bottom (or left/right) spread as a
                    //    fraction of the largest possible RGB spread. A stray-pixel cell has
                    //    little spread; a true edge has a lot.
                    //  • `need` — how badly a flat SOLID represents this cell (its `solid_err`
                    //    relative to the palette spacing). A near-flat cell a solid already
                    //    nails earns ~0 reward, so a few bright pixels can't force a black/white
                    //    half-block; a cell a solid genuinely fails (a real edge) earns the full
                    //    reward and snaps to a crisp half-block.
                    // Their product, scaled by `max_threshold`, keeps the reward bounded to
                    // ~detail·max_threshold — enough to win genuine edges, never enough to swamp
                    // a poor colour match. (The old `detail * raw_dist2` was unbounded — Detail=5
                    // times a ~195 075 distance dwarfed the match error and speckled the flats.)
                    const MAX_DIST2: f32 = 3.0 * 255.0 * 255.0;
                    let contrast = (dist2(fg_avg, bg_avg) / MAX_DIST2).min(1.0);
                    let need = (solid_err / (max_threshold * 4.0)).min(1.0);
                    let bias = (use_i - 0.5) * 2.0 * max_threshold
                        + use_i * detail * contrast * need * max_threshold * 6.0;
                    let err_eff = err - bias;
                    if err_eff < hb_err {
                        hb_err = err_eff;
                        hb = Some((glyph, fg, bg));
                    }
                }
                // Half-blocks win TIES against shades/solids (`<=`): a genuine edge whose
                // average a shade could also hit should render as the crisp half-block, not
                // a grey dither that only matches the average. Usage>0.5 lets them win
                // near-ties too; usage<0.5 makes them yield.
                if let Some((glyph, fg, bg)) = hb {
                    if hb_err <= best_err {
                        // best_err isn't read past this point (cell is pushed next), so no
                        // need to update it — just take the half-block's fg/bg/glyph.
                        best_fg = fg;
                        best_bg = bg;
                        best_ch = glyph;
                    }
                }
            }
            row[cx] = AnsiCell { fg: best_fg, bg: best_bg, ch: best_ch };
        }
    });
    AnsiGrid { cols, rows, cell_w: cw, cell_h: ch_, palette: palette.to_vec(), cells }
}

/// Is dot column `rx` of a 9-wide VGA cell lit, for glyph scanline `bits` of
/// character `ch`? Columns 0..8 read the 8-pixel glyph; column 8 (the 9th VGA dot)
/// is background except in the line-draw range `0xC0..=0xDF`, where it repeats the
/// last glyph column — so box rules (and the full/half blocks 219..223) connect
/// across cells, while the shade blocks 176..178 keep a blank 9th column (authentic).
#[inline]
fn ansi_dot_on(bits: u8, rx: usize, ch: u8) -> bool {
    if rx < 8 {
        (bits >> (7 - rx)) & 1 == 1
    } else {
        (0xC0u8..=0xDFu8).contains(&ch) && (bits & 1 == 1)
    }
}

/// Render an [`AnsiGrid`] back into `rgba` in place: each cell's glyph is painted
/// with its fg palette colour where the glyph mask is on, its bg colour where off.
/// The authentic mask is 9×16 (VGA text cell) — or 8×8 when `font_8x8` (VGA50 mode,
/// which has no 9th column); when the cell size differs it's nearest-sampled to
/// `cell_w`×`cell_h`. Original alpha is preserved.
pub fn ansi_render_grid(grid: &AnsiGrid, rgba: &mut [u8], w: usize, h: usize, font_8x8: bool) {
    if grid.palette.is_empty() {
        return;
    }
    let (cw, ch_) = (grid.cell_w.max(1), grid.cell_h.max(1));
    // Font geometry: 8×8 (VGA50, plain 8-wide dot rule) or 8×16 (9-dot VGA cell).
    let font_h = if font_8x8 { 8 } else { 16 };
    for cy in 0..grid.rows {
        for cx in 0..grid.cols {
            let cell = grid.cells[cy * grid.cols + cx];
            let fg = grid.palette[cell.fg as usize % grid.palette.len()];
            let bg = grid.palette[cell.bg as usize % grid.palette.len()];
            let x0 = cx * cw;
            let y0 = cy * ch_;
            for ry in 0..ch_ {
                let y = y0 + ry;
                if y >= h {
                    break;
                }
                // Nearest-sample the authentic glyph row.
                let frow = if ch_ == font_h { ry } else { ry * font_h / ch_ };
                let bits = if font_8x8 {
                    CP437_8X8[cell.ch as usize][frow.min(7)]
                } else {
                    CP437_8X16[cell.ch as usize][frow.min(15)]
                };
                for rx in 0..cw {
                    let x = x0 + rx;
                    if x >= w {
                        break;
                    }
                    let on = if font_8x8 {
                        // Plain 8-wide dot rule — VGA50 has no 9th column.
                        let fcol = if cw == 8 { rx } else { rx * 8 / cw };
                        (bits >> (7 - fcol.min(7))) & 1 == 1
                    } else {
                        // Nearest-sample the authentic 9-wide cell column.
                        let fcol = if cw == 9 { rx } else { rx * 9 / cw };
                        ansi_dot_on(bits, fcol.min(8), cell.ch)
                    };
                    let col = if on { fg } else { bg };
                    let i = (y * w + x) * 4;
                    rgba[i] = col[0];
                    rgba[i + 1] = col[1];
                    rgba[i + 2] = col[2];
                    // alpha preserved
                }
            }
        }
    }
}

/// The ANSI shade dither pass: build the grid from `rgba` then paint it back in
/// place. A no-op with an empty palette (mirrors error-diffusion, which also needs
/// a target). The output is already palette colours, so a later Palette snap is a
/// harmless no-op.
#[allow(clippy::too_many_arguments)]
pub fn ansi_shade_pass(
    rgba: &mut [u8],
    w: usize,
    h: usize,
    palette: &[[u8; 4]],
    cell_w: usize,
    cell_h: usize,
    f1: f32,
    f2: f32,
    f3: f32,
    half_blocks: bool,
    f1_on: bool,
    f2_on: bool,
    f3_on: bool,
    half_on: [bool; 4],
    half_use: [f32; 4],
    shade_amount: f32,
    font_8x8: bool,
    ice: bool,
    smooth: f32,
    detail: f32,
    invert: bool,
    allowed: Option<&[bool]>,
) {
    if palette.is_empty() || w == 0 || h == 0 {
        return;
    }
    let mut grid = ansi_shade_grid(
        rgba, w, h, palette, cell_w, cell_h, f1, f2, f3, half_blocks, f1_on, f2_on, f3_on,
        half_on, half_use, shade_amount, ice, smooth, detail, allowed,
    );
    if invert {
        for c in &mut grid.cells {
            std::mem::swap(&mut c.fg, &mut c.bg);
        }
    }
    ansi_render_grid(&grid, rgba, w, h, font_8x8);
}

/// The 16 standard CGA/EGA colours (index = ANSI colour number 0..15), in RGB.
const ANSI16: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00], // 0 black
    [0xAA, 0x00, 0x00], // 1 red
    [0x00, 0xAA, 0x00], // 2 green
    [0xAA, 0x55, 0x00], // 3 brown/yellow
    [0x00, 0x00, 0xAA], // 4 blue
    [0xAA, 0x00, 0xAA], // 5 magenta
    [0x00, 0xAA, 0xAA], // 6 cyan
    [0xAA, 0xAA, 0xAA], // 7 light grey
    [0x55, 0x55, 0x55], // 8 dark grey
    [0xFF, 0x55, 0x55], // 9 bright red
    [0x55, 0xFF, 0x55], // 10 bright green
    [0xFF, 0xFF, 0x55], // 11 bright yellow
    [0x55, 0x55, 0xFF], // 12 bright blue
    [0xFF, 0x55, 0xFF], // 13 bright magenta
    [0x55, 0xFF, 0xFF], // 14 bright cyan
    [0xFF, 0xFF, 0xFF], // 15 white
];

/// Nearest of the 16 ANSI/EGA colours (0..15) to an RGB triple.
fn nearest_ansi16(c: [u8; 3]) -> u8 {
    let mut best = 0u8;
    let mut best_d = u32::MAX;
    for (i, p) in ANSI16.iter().enumerate() {
        let dr = c[0] as i32 - p[0] as i32;
        let dg = c[1] as i32 - p[1] as i32;
        let db = c[2] as i32 - p[2] as i32;
        let d = (dr * dr + dg * dg + db * db) as u32;
        if d < best_d {
            best_d = d;
            best = i as u8;
        }
    }
    best
}

/// One channel of the xterm-256 6×6×6 colour cube: indices step 0,95,135,175,215,255.
const XTERM_CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// The full 256-colour xterm palette (16 system + 6×6×6 cube + 24 greys), as RGBA. Used as the
/// Unicode Ramp's default colour set when no Reduce/palette is active, so it colours richly and
/// exports cleanly to xterm-256.
pub fn xterm256_palette() -> Vec<[u8; 4]> {
    let mut v = Vec::with_capacity(256);
    for c in ANSI16 {
        v.push([c[0], c[1], c[2], 255]);
    }
    for &r in &XTERM_CUBE {
        for &g in &XTERM_CUBE {
            for &b in &XTERM_CUBE {
                v.push([r, g, b, 255]);
            }
        }
    }
    for i in 0..24u8 {
        let l = 8 + 10 * i;
        v.push([l, l, l, 255]);
    }
    v
}

/// Nearest xterm-256 palette index (0..255) to an RGB triple, by squared distance.
/// The palette is the 16 system colours ([`ANSI16`]), the 6×6×6 colour cube
/// (16..231), then 24 greys (232..255, level 8+10·i).
fn nearest_xterm256(c: [u8; 3]) -> u8 {
    let mut best = 0u8;
    let mut best_d = u32::MAX;
    let d2 = |p: [u8; 3]| -> u32 {
        let dr = c[0] as i32 - p[0] as i32;
        let dg = c[1] as i32 - p[1] as i32;
        let db = c[2] as i32 - p[2] as i32;
        (dr * dr + dg * dg + db * db) as u32
    };
    // 16 system colours.
    for (i, p) in ANSI16.iter().enumerate() {
        let d = d2(*p);
        if d < best_d {
            best_d = d;
            best = i as u8;
        }
    }
    // 6×6×6 cube (16..231).
    for (r, &cr) in XTERM_CUBE.iter().enumerate() {
        for (g, &cg) in XTERM_CUBE.iter().enumerate() {
            for (b, &cb) in XTERM_CUBE.iter().enumerate() {
                let d = d2([cr, cg, cb]);
                if d < best_d {
                    best_d = d;
                    best = 16 + (36 * r + 6 * g + b) as u8;
                }
            }
        }
    }
    // 24 greys (232..255).
    for i in 0..24u8 {
        let v = 8 + 10 * i;
        let d = d2([v, v, v]);
        if d < best_d {
            best_d = d;
            best = 232 + i;
        }
    }
    best
}

/// Serialize an [`AnsiGrid`] to a CP437 `.ans` file: SGR colour escapes + the raw
/// glyph bytes. `depth` picks the colour encoding — 1 = 16-colour (map RGB→nearest
/// ANSI16; fg 30-37 + `1;` bold for bright, bg 40-47, aixterm 100-107 for a bright bg
/// only when `ice`), 2 = 256-colour (`38;5;`/`48;5;` xterm indices), 3 = 24-bit
/// truecolour (`38;2;r;g;b`/`48;2;…`, the exact palette RGB, no loss). An SGR escape
/// is emitted only when fg/bg changes from the previous cell (reset+restate to avoid
/// stale attrs). The file opens with `ESC[0m` and closes with `ESC[0m`; there are NO
/// per-row newlines — every row is exactly `cols` wide, so the viewer's auto-wrap (from
/// the SAUCE width) breaks lines. A trailing CRLF here would double-space every row.
pub fn ansi_grid_to_ans(grid: &AnsiGrid, ice: bool, depth: u8) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(b"\x1b[0m");
    // Resolve a cell's fg/bg palette indices to their actual RGB (fallback grey/black).
    let rgb = |idx: u8, fallback: [u8; 3]| -> [u8; 3] {
        grid.palette
            .get(idx as usize)
            .map(|p| [p[0], p[1], p[2]])
            .unwrap_or(fallback)
    };
    // Track the previous cell's RGB so we only re-emit an SGR on a change. The colour
    // state flows across rows (no per-row reset) since we emit no row breaks.
    let (mut cur_fg, mut cur_bg) = ([256i16; 3], [256i16; 3]);
    for cy in 0..grid.rows {
        for cx in 0..grid.cols {
            let cell = grid.cells[cy * grid.cols + cx];
            let fg = rgb(cell.fg, [170, 170, 170]);
            let bg = rgb(cell.bg, [0, 0, 0]);
            let fg16 = [fg[0] as i16, fg[1] as i16, fg[2] as i16];
            let bg16 = [bg[0] as i16, bg[1] as i16, bg[2] as i16];
            if fg16 != cur_fg || bg16 != cur_bg {
                // Reset then re-state both attrs so we never carry stale bold/bg.
                let mut sgr = String::from("\x1b[0");
                match depth {
                    2 => {
                        sgr.push_str(&format!(";38;5;{}", nearest_xterm256(fg)));
                        sgr.push_str(&format!(";48;5;{}", nearest_xterm256(bg)));
                    }
                    3 => {
                        sgr.push_str(&format!(";38;2;{};{};{}", fg[0], fg[1], fg[2]));
                        sgr.push_str(&format!(";48;2;{};{};{}", bg[0], bg[1], bg[2]));
                    }
                    _ => {
                        // 16-colour, encoded the way the ANSI-art scene (PabloDraw/Moebius)
                        // actually reads it:
                        //  • bright FG → bold (SGR 1) + base fg 30-37.
                        //  • bright BG → the BLINK bit (SGR 5) + base bg 40-47, and the
                        //    SAUCE iCE flag tells viewers to render blink as a bright
                        //    background (no flashing). This is the whole point of "iCE
                        //    colors". We previously emitted xterm's aixterm 100-107, which
                        //    those tools DON'T honor — so every bright background dropped to
                        //    black (the "black gaps"). Without iCE a bright bg clamps to its
                        //    base color (a real non-iCE screen has only 8 backgrounds).
                        let f = nearest_ansi16(fg);
                        let b = nearest_ansi16(bg);
                        if f >= 8 {
                            sgr.push_str(";1"); // bold → bright fg
                        }
                        if b >= 8 && ice {
                            sgr.push_str(";5"); // blink bit → bright bg under iCE
                        }
                        sgr.push_str(&format!(";{}", 30 + (f % 8)));
                        sgr.push_str(&format!(";{}", 40 + (b % 8)));
                    }
                }
                sgr.push('m');
                out.extend_from_slice(sgr.as_bytes());
                cur_fg = fg16;
                cur_bg = bg16;
            }
            out.push(cell.ch);
        }
        // NO per-row CRLF: each row is exactly `cols` wide, so the viewer's auto-wrap
        // (guided by the SAUCE width) breaks the lines. Emitting CRLF here would
        // double-space every row (auto-wrap + explicit newline) — the stretched output.
    }
    out.extend_from_slice(b"\x1b[0m");
    let _ = (cur_fg, cur_bg);
    out
}

/// Serialize an [`AnsiGrid`] to a **TundraDraw** (`.tnd`) file — the scene-native
/// binary 24-bit-truecolour format, so an RGB export keeps every colour exactly.
/// Serialize an [`AnsiGrid`] to a REXPaint `.xp` file (gzipped). A single layer of the grid's
/// CP437 cells with 24-bit fg/bg resolved from the palette, in the format's **column-major** order
/// (see `crate::decode::rexpaint`). Round-trips through that decoder. No SAUCE (`.xp` is
/// self-contained), so callers must NOT append one.
pub fn ansi_grid_to_xp(grid: &AnsiGrid) -> Vec<u8> {
    use std::io::Write;
    let rgb = |idx: u8, fallback: [u8; 3]| -> [u8; 3] {
        grid.palette
            .get(idx as usize)
            .map(|p| [p[0], p[1], p[2]])
            .unwrap_or(fallback)
    };
    let (w, h) = (grid.cols.max(1), grid.rows.max(1));
    let mut raw = Vec::with_capacity(16 + w * h * 10);
    raw.extend_from_slice(&(-1i32).to_le_bytes()); // version (negative = R9+)
    raw.extend_from_slice(&1i32.to_le_bytes()); // one layer
    raw.extend_from_slice(&(w as i32).to_le_bytes());
    raw.extend_from_slice(&(h as i32).to_le_bytes());
    // Column-major: x outer, y inner.
    for x in 0..w {
        for y in 0..h {
            let cell = grid.cells[y * grid.cols + x];
            let fg = rgb(cell.fg, [170, 170, 170]);
            let mut bg = rgb(cell.bg, [0, 0, 0]);
            // 255,0,255 is REXPaint's transparent marker — nudge an exact match so a real
            // magenta bg stays opaque on re-open.
            if bg == crate::decode::XP_TRANSPARENT {
                bg = [254, 0, 255];
            }
            raw.extend_from_slice(&(cell.ch as u32).to_le_bytes());
            raw.extend_from_slice(&fg);
            raw.extend_from_slice(&bg);
        }
    }
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let _ = enc.write_all(&raw);
    enc.finish().unwrap_or_default()
}

/// Header `0x18 "TUNDRA24"`, then a command stream of cells in row-major order (the
/// decoder auto-increments the column and wraps at the SAUCE width, so no explicit
/// position commands are needed). Per cell we emit the minimal command for whatever
/// changed vs the current fg/bg: cmd 6 (both), cmd 2 (fg), cmd 4 (bg), or a bare
/// literal char when neither changed. Matches `crate::decode::tundra` exactly,
/// including its `[0,0,0]` initial fg/bg. Caller appends the SAUCE trailer.
pub fn ansi_grid_to_tundra(grid: &AnsiGrid) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    out.push(0x18);
    out.extend_from_slice(b"TUNDRA24");
    let rgb = |idx: u8, fallback: [u8; 3]| -> [u8; 3] {
        grid.palette
            .get(idx as usize)
            .map(|p| [p[0], p[1], p[2]])
            .unwrap_or(fallback)
    };
    // The decoder inits current fg/bg to [0,0,0]; match it so the diff logic agrees.
    let (mut cur_fg, mut cur_bg) = ([0u8; 3], [0u8; 3]);
    for cy in 0..grid.rows {
        for cx in 0..grid.cols {
            let cell = grid.cells[cy * grid.cols + cx];
            let fg = rgb(cell.fg, [0, 0, 0]);
            let bg = rgb(cell.bg, [0, 0, 0]);
            let ch = cell.ch;
            let fg_diff = fg != cur_fg;
            let bg_diff = bg != cur_bg;
            if fg_diff && bg_diff {
                // cmd 6: set both + draw char. Filler 0x00 at +2/+6 (decoder skips them).
                out.extend_from_slice(&[6, ch, 0x00, fg[0], fg[1], fg[2], 0x00, bg[0], bg[1], bg[2]]);
                cur_fg = fg;
                cur_bg = bg;
            } else if fg_diff {
                out.extend_from_slice(&[2, ch, 0x00, fg[0], fg[1], fg[2]]);
                cur_fg = fg;
            } else if bg_diff {
                out.extend_from_slice(&[4, ch, 0x00, bg[0], bg[1], bg[2]]);
                cur_bg = bg;
            } else if matches!(ch, 1 | 2 | 4 | 6) {
                // A literal here would be read as a COMMAND byte — re-issue it as a cmd 6
                // (fg/bg already equal cur, so state is unchanged). Our glyphs are never
                // 1/2/4/6, but guard the stream anyway.
                out.extend_from_slice(&[6, ch, 0x00, fg[0], fg[1], fg[2], 0x00, bg[0], bg[1], bg[2]]);
            } else {
                out.push(ch); // plain literal — drawn with the current fg/bg
            }
        }
    }
    out
}

/// Does `palette` equal the standard EGA/CGA-16 set ([`ANSI16`])? Count must be 16
/// and every ANSI colour present (order-independent). Used to choose the export
/// format: EGA-16 → a plain `.ans`, anything else → `.xbin` (embeds its palette).
pub fn palette_is_ega16(palette: &[[u8; 4]]) -> bool {
    palette.len() == 16
        && ANSI16
            .iter()
            .all(|a| palette.iter().any(|p| [p[0], p[1], p[2]] == *a))
}

/// Serialize an [`AnsiGrid`] to an **XBIN** file (embeds the palette so non-EGA
/// colours survive). Header `XBIN\x1A`, `width`/`height` (u16 LE cells), `fontsize`,
/// `flags`; then a 16-colour 6-bit-DAC palette block, an *optional* font, and
/// `width*height` `(char, attribute)` cell pairs where `attribute = bg16<<4 | fg16`.
///
/// The embedded 16-colour palette is built from the colours the grid ACTUALLY USES
/// (not `palette[..16]`, which for e.g. ANSI32 is all cold blues/greys and would
/// wreck warm cells): distinct used colours are embedded verbatim when ≤16, else
/// median-cut to 16 representatives; every cell's fg/bg is remapped to its nearest
/// embedded slot. A font is embedded ONLY for VGA50/8×8 (`font_8x8`); the 9×16 path
/// draws standard CP437, so it omits the font (fontsize 0, flag clear) and lets the
/// decoder fall back to the default 8×16 VGA font. No SAUCE trailer — the caller
/// appends that. Returns `(bytes, reduced)` where `reduced` is true iff >16 distinct
/// colours were used and had to be median-cut.
pub fn ansi_grid_to_xbin(grid: &AnsiGrid, font_8x8: bool, ice: bool) -> (Vec<u8>, bool) {
    // 1) Distinct palette indices actually referenced by the cells (fg + bg), in
    //    first-seen order — the candidates for the embedded 16-colour palette.
    let mut used: Vec<usize> = Vec::new();
    let mut seen: HashSet<usize> = HashSet::new();
    for cell in &grid.cells {
        for idx in [cell.fg as usize, cell.bg as usize] {
            if idx < grid.palette.len() && seen.insert(idx) {
                used.push(idx);
            }
        }
    }
    let reduced = used.len() > 16;
    // Embedded RGB palette (16 slots) + a map from ORIGINAL palette index → slot 0..15.
    let mut embedded: Vec<[u8; 3]> = Vec::with_capacity(16);
    let mut map16 = vec![0u8; grid.palette.len().max(1)];
    if !reduced {
        // ≤16 used → embed exactly those colours; each used index maps to its slot.
        for (slot, &oi) in used.iter().enumerate() {
            let c = grid.palette[oi];
            embedded.push([c[0], c[1], c[2]]);
            map16[oi] = slot as u8;
        }
    } else {
        // >16 used → median-cut the used colours' RGB to 16 reps, then map each used
        // index to its nearest embedded rep.
        let used_rgba: Vec<[u8; 4]> = used
            .iter()
            .map(|&oi| {
                let c = grid.palette[oi];
                [c[0], c[1], c[2], 255]
            })
            .collect();
        for r in median_cut(&used_rgba, 16) {
            embedded.push([r[0], r[1], r[2]]);
        }
        let emb_rgba: Vec<[u8; 4]> = embedded.iter().map(|c| [c[0], c[1], c[2], 255]).collect();
        for &oi in &used {
            let c = grid.palette[oi];
            map16[oi] = nearest_index([c[0], c[1], c[2]], &emb_rgba) & 0x0F;
        }
    }
    while embedded.len() < 16 {
        embedded.push([0, 0, 0]); // pad unused slots with black
    }

    // 2) Header.
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(b"XBIN");
    out.push(0x1A); // EOF marker inside the header
    out.extend_from_slice(&(grid.cols as u16).to_le_bytes());
    out.extend_from_slice(&(grid.rows as u16).to_le_bytes());
    // Only VGA50/8×8 embeds a font; the 9×16 path relies on the default 8×16 VGA font.
    let (fontsize, has_font): (u8, bool) = if font_8x8 { (8, true) } else { (0, false) };
    out.push(fontsize);
    // flags: bit0 palette present; bit1 font present (VGA50 only); bit3 non-blink / iCE.
    let mut flags = 0b0000_0001u8;
    if has_font {
        flags |= 0b0000_0010;
    }
    if ice {
        flags |= 0b0000_1000;
    }
    out.push(flags);
    // 3) Palette block: 16 × RGB, each channel down to the 6-bit VGA DAC (v>>2).
    for c in &embedded {
        out.push(c[0] >> 2);
        out.push(c[1] >> 2);
        out.push(c[2] >> 2);
    }
    // 4) Font block — VGA50 only (256 × 8 rows, MSB-left).
    if has_font {
        for glyph in CP437_8X8.iter() {
            out.extend_from_slice(glyph);
        }
    }
    // 5) Image data: (char, attribute) per cell, indices remapped into the embedded 0..15.
    let emap = |idx: u8| -> u8 {
        let i = idx as usize;
        if i < map16.len() {
            map16[i] & 0x0F
        } else {
            0
        }
    };
    for cell in &grid.cells {
        out.push(cell.ch);
        let fg = emap(cell.fg);
        let bg = emap(cell.bg);
        out.push((bg << 4) | fg);
    }
    (out, reduced)
}

/// Build a thumbnail. Pixel art that already fits `max_dim` is stored at its
/// *source* resolution — the GPU's NEAREST sampling then upscales it crisply at
/// any tile size / grid-zoom, so detail isn't thrown away the way a fixed-size
/// downscaled thumbnail would (a 15×392 sprite must NOT become 10×256). Only
/// images larger than `max_dim` in either axis are scaled down — by **area
/// averaging** (box filter), so dithered block art shrinks to faithful tones.
pub fn make_thumb(img: &PixImage, max_dim: u32) -> (usize, usize, Vec<u8>) {
    let (sw, sh) = (img.width as usize, img.height as usize);
    let max = max_dim.max(1) as usize;

    if sw <= max && sh <= max {
        return (sw, sh, img.rgba_bytes());
    }

    let scale = (max as f32 / sw as f32).min(max as f32 / sh as f32);
    let dw = (sw as f32 * scale).round().max(1.0) as usize;
    let dh = (sh as f32 * scale).round().max(1.0) as usize;
    let mut out = vec![0u8; dw * dh * 4];
    for y in 0..dh {
        let sy0 = y * sh / dh;
        let sy1 = ((y + 1) * sh / dh).max(sy0 + 1).min(sh);
        for x in 0..dw {
            let sx0 = x * sw / dw;
            let sx1 = ((x + 1) * sw / dw).max(sx0 + 1).min(sw);
            // Premultiplied box average over each dest pixel's source footprint. For
            // a downscale this is the *faithful* shrink: a 50% dither (▒) becomes a
            // 50% grey, not the aliased noise a single-sample nearest pick produced —
            // "legit blocks, not fake ones". (Upscales never reach here; small art is
            // returned at source res above and the GPU NEAREST-samples it crisply.)
            let (mut sr, mut sg, mut sb, mut sa, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    let p = img.pixels[sy * sw + sx];
                    let a = p[3] as u64;
                    sr += p[0] as u64 * a;
                    sg += p[1] as u64 * a;
                    sb += p[2] as u64 * a;
                    sa += a;
                    n += 1;
                }
            }
            let o = (y * dw + x) * 4;
            // Guard-then-divide the alpha-weighted sums: clearer than four `checked_div`
            // chains for what is one averaging block over the same divisor.
            if sa > 0 {
                out[o] = (sr / sa) as u8;
                out[o + 1] = (sg / sa) as u8;
                out[o + 2] = (sb / sa) as u8;
                out[o + 3] = (sa / n) as u8;
            } // else fully transparent → leave the zeroed RGBA
        }
    }
    (dw, dh, out)
}

/// Area-average (box filter) a straight-RGBA buffer from `sw×sh` down to `dw×dh`,
/// the same faithful shrink [`make_thumb`] uses (a 50% dither averages to 50% grey
/// instead of aliasing). Operates on raw bytes so callers with CPU pixels but no
/// `PixImage` — e.g. the viewer minimap, built at the strip's device resolution so
/// it stays crisp — can reuse it. Output is straight (un-premultiplied) RGBA.
pub fn box_downscale(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    let (dw, dh) = (dw.max(1), dh.max(1));
    let mut out = vec![0u8; dw * dh * 4];
    if sw == 0 || sh == 0 {
        return out;
    }
    for y in 0..dh {
        let sy0 = y * sh / dh;
        let sy1 = ((y + 1) * sh / dh).max(sy0 + 1).min(sh);
        for x in 0..dw {
            let sx0 = x * sw / dw;
            let sx1 = ((x + 1) * sw / dw).max(sx0 + 1).min(sw);
            let (mut sr, mut sg, mut sb, mut sa, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    let p = (sy * sw + sx) * 4;
                    let a = src[p + 3] as u64;
                    sr += src[p] as u64 * a;
                    sg += src[p + 1] as u64 * a;
                    sb += src[p + 2] as u64 * a;
                    sa += a;
                    n += 1;
                }
            }
            let o = (y * dw + x) * 4;
            // Guard-then-divide the alpha-weighted sums: clearer than four `checked_div`
            // chains for what is one averaging block over the same divisor.
            if sa > 0 {
                out[o] = (sr / sa) as u8;
                out[o + 1] = (sg / sa) as u8;
                out[o + 2] = (sb / sa) as u8;
                out[o + 3] = (sa / n) as u8;
            } // else fully transparent → leave zeroed
        }
    }
    out
}

/// Scale `src` (`sw×sh`) to fit *inside* a `dw×dh` canvas while preserving its aspect
/// ratio, anchored to the **top-left**, with the leftover margin (right and/or bottom)
/// left fully transparent. Unlike a straight [`box_downscale`] (which stretches to the
/// exact target), this keeps circles round — the ANSI "Fit to chars" grid uses it so a
/// square sprite fitting an 80×50 canvas gets blank (space) cells instead of being
/// squashed. Top-left anchoring means column/row 0 is the art's own origin, so the
/// character ruler reads naturally and it's obvious where the art runs past the grid.
/// The transparent padding becomes empty cells downstream (`ansi_shade_grid` maps
/// fully-transparent cells to a space). Degenerate source/target → transparent canvas.
pub fn letterbox(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    let (dw, dh) = (dw.max(1), dh.max(1));
    let mut out = vec![0u8; dw * dh * 4]; // transparent canvas
    if sw == 0 || sh == 0 {
        return out;
    }
    // Largest integer content box that fits dw×dh at the source aspect.
    let scale = (dw as f32 / sw as f32).min(dh as f32 / sh as f32);
    let cw = ((sw as f32 * scale).round() as usize).clamp(1, dw);
    let ch = ((sh as f32 * scale).round() as usize).clamp(1, dh);
    let scaled = box_downscale(src, sw, sh, cw, ch);
    // Anchor top-left (offset 0,0): the padding falls on the right and bottom.
    for y in 0..ch {
        let dst = (y * dw) * 4;
        let s = y * cw * 4;
        out[dst..dst + cw * 4].copy_from_slice(&scaled[s..s + cw * 4]);
    }
    out
}

// ===================== PETSCII converter (image → C64 hi-res char art) =====================
//
// C64 standard hi-res char mode: ONE global background + per-cell (glyph, foreground-from-16).
// We match each 8×8 cell against the C64 ROM font ([`crate::decode::C64_FONT`], reverse glyphs
// baked in at codes 128..256) using the two-region-mean trick, over the VIC-II palette
// ([`crate::decode::VIC2`]). A "purity" knob biases toward clean block glyphs; the background is
// auto-picked (least total error over block glyphs) unless overridden.

/// One PETSCII cell: a C64 screen code (0..255; ≥128 = reverse) + a VIC-II fg index (0..15).
#[derive(Clone, Copy)]
pub struct PetsciiCell {
    pub code: u8,
    pub fg: u8,
}

/// A converted PETSCII screen: `cols`×`rows` cells over one global background `bg`, with `page`
/// selecting the charset (0 = upper/graphics, 1 = lower).
pub struct PetsciiGrid {
    pub cols: usize,
    pub rows: usize,
    pub bg: u8,
    pub page: usize,
    pub cells: Vec<PetsciiCell>,
}

/// Is C64 screen-code `code` (in font `page`) a "block" glyph — each of its four 4×4 quadrants
/// uniformly on/off? Captures space / full / halves / quarters (the classic block set), computed
/// from the ROM so no screen codes are hardcoded.
fn c64_is_block(page: usize, code: u8) -> bool {
    let g = &crate::decode::C64_FONT[page * 256 + code as usize];
    for &(r0, c0) in &[(0usize, 0usize), (0, 4), (4, 0), (4, 4)] {
        let (mut any, mut all) = (false, true);
        for row in g.iter().take(r0 + 4).skip(r0) {
            for rx in c0..c0 + 4 {
                let on = (row >> (7 - rx)) & 1 == 1;
                any |= on;
                all &= on;
            }
        }
        if any && !all {
            return false; // mixed quadrant → not a pure block glyph
        }
    }
    true
}

/// Best (code, fg, error) for one 8×8 `cell` given a fixed background, searching the C64 font.
/// `block_only` restricts to block glyphs (used for the cheap bg search); otherwise non-block
/// glyphs pay `penalty` (the purity bias). Error = Σset(src-fg)² + Σclear(src-bg)².
#[allow(clippy::too_many_arguments)]
fn petscii_cell_match(
    cell: &[[f32; 3]; 64],
    page: usize,
    bg_idx: u8,
    pal: &[[u8; 4]; 16],
    pf: &[[f32; 3]],
    is_block: &[bool],
    penalty: f32,
    block_only: bool,
    allowed: Option<&[bool]>,
) -> (u8, u8, f32) {
    let bg = pf[bg_idx as usize];
    let mut best = (32u8, bg_idx, f32::MAX);
    for code in 0u16..256 {
        if block_only && !is_block[code as usize] {
            continue;
        }
        // Glyph-picker mask: skip codes the user disabled (space stays the fallback).
        if let Some(a) = allowed {
            if !a.get(code as usize).copied().unwrap_or(true) {
                continue;
            }
        }
        let g = &crate::decode::C64_FONT[page * 256 + code as usize];
        let (mut set_sum, mut set_sq, mut set_n, mut clear_err) = ([0f32; 3], 0f32, 0f32, 0f32);
        for (py, &bits) in g.iter().enumerate() {
            for px in 0..8usize {
                let s = cell[py * 8 + px];
                if (bits >> (7 - px)) & 1 == 1 {
                    set_sum[0] += s[0];
                    set_sum[1] += s[1];
                    set_sum[2] += s[2];
                    set_sq += s[0] * s[0] + s[1] * s[1] + s[2] * s[2];
                    set_n += 1.0;
                } else {
                    let (dr, dg, db) = (s[0] - bg[0], s[1] - bg[1], s[2] - bg[2]);
                    clear_err += dr * dr + dg * dg + db * db;
                }
            }
        }
        let (fg_idx, err_set) = if set_n > 0.0 {
            let mean = [
                (set_sum[0] / set_n) as u8,
                (set_sum[1] / set_n) as u8,
                (set_sum[2] / set_n) as u8,
            ];
            let fi = nearest_index(mean, pal.as_slice());
            let fc = pf[fi as usize];
            let e = set_sq
                - 2.0 * (fc[0] * set_sum[0] + fc[1] * set_sum[1] + fc[2] * set_sum[2])
                + set_n * (fc[0] * fc[0] + fc[1] * fc[1] + fc[2] * fc[2]);
            (fi, e)
        } else {
            (bg_idx, 0.0) // all-clear glyph (space): fg irrelevant
        };
        let mut err = err_set + clear_err;
        if !block_only && !is_block[code as usize] {
            err += penalty;
        }
        if err < best.2 {
            best = (code as u8, fg_idx, err);
        }
    }
    best
}

/// Convert `rgba` (`w`×`h`) into a `cols`×`rows` PETSCII grid: one global background + per-cell
/// (C64 glyph, VIC-II fg). `page` selects the charset (0 upper/graphics, 1 lower). `purity` 0..1
/// biases toward clean block glyphs (0) vs the full charset (1). `bg_override` forces the
/// background colour; otherwise it's auto-picked to minimise total error.
#[allow(clippy::too_many_arguments)]
pub fn petscii_grid(
    rgba: &[u8],
    w: usize,
    h: usize,
    cols: usize,
    rows: usize,
    page: usize,
    purity: f32,
    bg_override: Option<u8>,
    pal: &[[u8; 4]; 16],
    allowed: Option<&[bool]>,
) -> PetsciiGrid {
    let cols = cols.max(1);
    let rows = rows.max(1);
    let page = page.min(1);
    let (gw, gh) = (cols * 8, rows * 8);
    let small = box_downscale(rgba, w, h, gw, gh);
    let pf: Vec<[f32; 3]> = pal
        .iter()
        .map(|c| [c[0] as f32, c[1] as f32, c[2] as f32])
        .collect();
    let is_block: Vec<bool> = (0u16..256).map(|c| c64_is_block(page, c as u8)).collect();
    // Non-block penalty: purity 0 strongly prefers blocks, purity 1 = no penalty. Scaled to the
    // cell's error magnitude (64 px × a few palette-steps²).
    let penalty = (1.0 - purity.clamp(0.0, 1.0)) * 64.0 * 3000.0;

    // Per-cell 8×8 pixels (as f32), reused across the bg search + final match.
    let cells_px: Vec<[[f32; 3]; 64]> = (0..cols * rows)
        .map(|ci| {
            let (cy, cx) = (ci / cols, ci % cols);
            let mut buf = [[0f32; 3]; 64];
            for py in 0..8 {
                for px in 0..8 {
                    let o = ((cy * 8 + py) * gw + cx * 8 + px) * 4;
                    buf[py * 8 + px] = [small[o] as f32, small[o + 1] as f32, small[o + 2] as f32];
                }
            }
            buf
        })
        .collect();

    // Background: override, or auto = the VIC-II colour giving least total block-match error.
    let bg = bg_override.map(|b| b & 15).unwrap_or_else(|| {
        (0u8..16)
            .map(|b| {
                let total: f32 = cells_px
                    .par_iter()
                    .map(|cell| {
                        petscii_cell_match(cell, page, b, pal, &pf, &is_block, 0.0, true, allowed).2
                    })
                    .sum();
                (b, total)
            })
            .min_by(|a, c| a.1.partial_cmp(&c.1).unwrap())
            .map(|(b, _)| b)
            .unwrap_or(0)
    });

    // Final match per cell, in parallel.
    let cells: Vec<PetsciiCell> = cells_px
        .par_iter()
        .map(|cell| {
            let (code, fg, _) =
                petscii_cell_match(cell, page, bg, pal, &pf, &is_block, penalty, false, allowed);
            PetsciiCell { code, fg }
        })
        .collect();

    PetsciiGrid { cols, rows, bg, page, cells }
}

/// Render a [`PetsciiGrid`] to an RGBA buffer (`cols*8` × `rows*8`) via the C64 font + VIC-II —
/// the same pixels the decoder produces, so the preview matches a re-opened export.
pub fn petscii_render(grid: &PetsciiGrid, pal: &[[u8; 4]; 16]) -> (usize, usize, Vec<u8>) {
    let (w, h) = (grid.cols * 8, grid.rows * 8);
    let mut rgba = vec![0u8; w * h * 4];
    let bg = pal[grid.bg as usize];
    for cy in 0..grid.rows {
        for cx in 0..grid.cols {
            let cell = grid.cells[cy * grid.cols + cx];
            let g = &crate::decode::C64_FONT[grid.page * 256 + cell.code as usize];
            let fg = pal[cell.fg as usize];
            for (py, &bits) in g.iter().enumerate() {
                for px in 0..8usize {
                    let c = if (bits >> (7 - px)) & 1 == 1 { fg } else { bg };
                    let o = ((cy * 8 + py) * w + cx * 8 + px) * 4;
                    rgba[o..o + 4].copy_from_slice(&c);
                }
            }
        }
    }
    (w, h, rgba)
}

/// The PETSCII **pipeline pass**: convert `rgba` (w×h) to a `cols`×`rows` C64 char grid, render it,
/// then nearest-sample that char art back into the same w×h buffer. This is what lets PETSCII apply
/// everywhere the pipeline runs (grid tiles, the details preview, "Apply to grid") — exactly the way
/// [`ansi_shade_pass`] does for ANSI Shade. The full-view path builds the grid directly for a crisp
/// cell-exact render; here we fit the char art into whatever buffer the pipeline hands us.
#[allow(clippy::too_many_arguments)]
pub fn petscii_pass(
    rgba: &mut [u8],
    w: usize,
    h: usize,
    cols: usize,
    rows: usize,
    page: usize,
    purity: f32,
    bg_override: Option<u8>,
    pal: &[[u8; 4]; 16],
    allowed: Option<&[bool]>,
) {
    if w == 0 || h == 0 {
        return;
    }
    let grid = petscii_grid(rgba, w, h, cols, rows, page, purity, bg_override, pal, allowed);
    let (pw, ph, px) = petscii_render(&grid, pal);
    if pw == 0 || ph == 0 {
        return;
    }
    for y in 0..h {
        let sy = (y * ph / h).min(ph - 1);
        for x in 0..w {
            let sx = (x * pw / w).min(pw - 1);
            let so = (sy * pw + sx) * 4;
            let d = (y * w + x) * 4;
            rgba[d..d + 4].copy_from_slice(&px[so..so + 4]);
        }
    }
}

// ── ASCII (character-density) converter ─────────────────────────────────────────
// Maps image brightness to CP437 glyphs on a coverage-sorted ramp. The glyph pool is
// chosen by the user: either an EXPLICIT set typed into "Use only chars" (e.g. .oOX$),
// or a union of character categories — printable 32–126 is always in, plus optional
// High ASCII 128–255, Control 0–31, Blocks (░▒▓█ + half/quarter blocks) and Box drawing.
// Produces an `AnsiGrid`, so it rides the same render + `.ans`/`.xb`/`.tnd` export as
// ANSI Shade.

/// The CP437 block / shade glyphs (space handled separately): light/medium/dark shades,
/// the full block, and the half/quarter blocks + solid square.
const ASCII_BLOCK_GLYPHS: [u8; 9] = [176, 177, 178, 219, 220, 221, 222, 223, 254];

/// The glyph pool selection for the ASCII converter. When `only` is non-empty it is used
/// verbatim (the typed characters); otherwise the enabled category ranges are unioned.
#[derive(Clone, Default)]
pub struct AsciiCharset {
    pub only: Vec<u8>,   // explicit CP437 glyph pool ("Use only chars"); overrides the ranges
    pub high: bool,      // 128..=255 (CP437 extended)
    pub control: bool,   // 0..=31 (control-code glyphs)
    pub blocks: bool,    // the ░▒▓█ + half/quarter block set (even when High ASCII is off)
    pub box_draw: bool,  // 179..=218 (box-drawing lines)
    pub mask: Vec<bool>, // glyph-picker mask over CP437 (256); intersects the pool. Empty = all.
}

/// Build the light→dark ASCII ramp: `(glyph, coverage)` pairs sorted by ink coverage
/// (fraction of set pixels in the render font). For the category-based pool one
/// representative glyph is kept per distinct coverage level (printable ASCII wins ties);
/// an explicit "only" set keeps every typed glyph. `font_8x8` selects which CP437 font
/// the coverage is measured from, so it matches the renderer.
pub fn ascii_ramp(cs: &AsciiCharset, font_8x8: bool) -> Vec<(u8, f32)> {
    let total = if font_8x8 { 64.0 } else { 8.0 * 16.0 };
    let cov = |code: u8| -> f32 {
        let bits: u32 = if font_8x8 {
            CP437_8X8[code as usize].iter().map(|b| b.count_ones()).sum()
        } else {
            CP437_8X16[code as usize].iter().map(|b| b.count_ones()).sum()
        };
        bits as f32 / total
    };
    // Glyph-picker mask (256): when set, only these CP437 codes are usable. Empty = all.
    let mask_on = cs.mask.len() == 256 && cs.mask.iter().any(|b| !*b);
    let allowed = |code: u8| -> bool {
        !mask_on || cs.mask.get(code as usize).copied().unwrap_or(true)
    };
    // Explicit set: use exactly the typed glyphs, sorted by coverage, no dedup.
    if !cs.only.is_empty() {
        let mut ramp: Vec<(u8, f32)> = cs
            .only
            .iter()
            .copied()
            .filter(|&c| allowed(c))
            .map(|c| (c, cov(c)))
            .collect();
        ramp.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        return ramp;
    }
    // Category union. First glyph seen for a coverage bucket wins, so the EXPLICITLY enabled
    // special categories (Box Drawing, Blocks) go FIRST — otherwise base ASCII would claim every
    // bucket and toggling them on would change almost nothing. Base printable is the fallback,
    // then the broad High/Control ranges.
    let mut order: Vec<u8> = Vec::new();
    if cs.box_draw {
        order.extend(179u8..=218);
    }
    if cs.blocks {
        order.extend(ASCII_BLOCK_GLYPHS);
    }
    order.extend(32u8..=126); // base printable ASCII, always in the pool
    if cs.high {
        order.extend(127u8..=255);
    }
    if cs.control {
        order.extend(0u8..=31);
    }
    let mut seen: std::collections::HashMap<u16, (u8, f32)> = std::collections::HashMap::new();
    for code in order {
        if !allowed(code) {
            continue;
        }
        let c = cov(code);
        let bucket = (c * total).round() as u16; // one slot per set-pixel count
        seen.entry(bucket).or_insert((code, c));
    }
    let mut ramp: Vec<(u8, f32)> = seen.into_values().collect();
    ramp.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    // Guarantee a blank (space) at the light end even if some odd font had no 0-cover glyph.
    if ramp.first().map(|(_, c)| *c > 0.0).unwrap_or(true) {
        ramp.insert(0, (32, 0.0));
    }
    ramp
}

/// Analyse `rgba` (w×h) into an ASCII [`AnsiGrid`]: per `cell_w`×`cell_h` cell, pick the
/// ramp glyph whose coverage matches the cell's darkness, and colour it from `palette`
/// (per-cell nearest when `color`, else a fixed brightest-on-darkest monochrome). The
/// glyph pool follows `cs` (see [`ascii_ramp`]).
#[allow(clippy::too_many_arguments)]
pub fn ascii_grid(
    rgba: &[u8],
    w: usize,
    h: usize,
    palette: &[[u8; 4]],
    cell_w: usize,
    cell_h: usize,
    cs: &AsciiCharset,
    color: bool,
    invert: bool,
    font_8x8: bool,
    mono: Option<([u8; 3], [u8; 3])>,
) -> AnsiGrid {
    let cw = cell_w.max(1);
    let ch_ = cell_h.max(1);
    let cols = w.div_ceil(cw);
    let rows = h.div_ceil(ch_);
    let mut cells = Vec::with_capacity(cols * rows);
    // Working palette + ink/paper (the unified fg/bg chips override when set).
    let (out_pal, ink, paper) = build_mono_palette(palette, mono);
    if palette.is_empty() || w == 0 || h == 0 {
        cells.resize(cols * rows, AnsiCell { fg: 0, bg: 0, ch: 32 });
        return AnsiGrid { cols, rows, cell_w: cw, cell_h: ch_, palette: out_pal, cells };
    }
    let mut ramp = ascii_ramp(cs, font_8x8);
    if ramp.is_empty() {
        ramp.push((32, 0.0)); // a stray empty "only" set → all blank rather than a panic
    }
    for cy in 0..rows {
        for cx in 0..cols {
            // Average the cell block (clamped to the image bounds), tracking alpha so a
            // fully-transparent cell becomes blank paper.
            let (mut sr, mut sg, mut sb, mut sa, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
            for y in cy * ch_..((cy + 1) * ch_).min(h) {
                for x in cx * cw..((cx + 1) * cw).min(w) {
                    let o = (y * w + x) * 4;
                    let a = rgba[o + 3] as u32;
                    sr += rgba[o] as u32 * a;
                    sg += rgba[o + 1] as u32 * a;
                    sb += rgba[o + 2] as u32 * a;
                    sa += a;
                    n += 1;
                }
            }
            if n == 0 || sa == 0 {
                let (fg, bg) = if invert { (paper, ink) } else { (ink, paper) };
                cells.push(AnsiCell { fg, bg, ch: 32 });
                continue;
            }
            let avg = [(sr / sa) as u8, (sg / sa) as u8, (sb / sa) as u8];
            // Coverage darkness = 1 - luminance; low alpha reads as background.
            let lum = (0.299 * avg[0] as f32 + 0.587 * avg[1] as f32 + 0.114 * avg[2] as f32)
                / 255.0;
            let cover = (sa as f32 / (n as f32 * 255.0)) * (1.0 - lum);
            // Nearest ramp glyph by coverage.
            let ch = ramp
                .iter()
                .min_by(|a, b| {
                    (a.1 - cover)
                        .abs()
                        .partial_cmp(&(b.1 - cover).abs())
                        .unwrap()
                })
                .map(|(g, _)| *g)
                .unwrap_or(32);
            let fg = if color { nearest_index(avg, palette) } else { ink };
            // Invert = inverse video: draw the glyph in paper on an fg-coloured cell.
            let (fg, bg) = if invert { (paper, fg) } else { (fg, paper) };
            cells.push(AnsiCell { fg, bg, ch });
        }
    }
    AnsiGrid { cols, rows, cell_w: cw, cell_h: ch_, palette: out_pal, cells }
}

/// The ASCII **pipeline pass**: build the ASCII grid from `rgba` then paint it back in
/// place — the twin of [`ansi_shade_pass`], so ASCII applies everywhere the pipeline
/// runs (grid tiles, details preview, "Apply to grid").
#[allow(clippy::too_many_arguments)]
pub fn ascii_pass(
    rgba: &mut [u8],
    w: usize,
    h: usize,
    palette: &[[u8; 4]],
    cell_w: usize,
    cell_h: usize,
    cs: &AsciiCharset,
    color: bool,
    invert: bool,
    font_8x8: bool,
    mono: Option<([u8; 3], [u8; 3])>,
) {
    if palette.is_empty() || w == 0 || h == 0 {
        return;
    }
    let grid = ascii_grid(rgba, w, h, palette, cell_w, cell_h, cs, color, invert, font_8x8, mono);
    ansi_render_grid(&grid, rgba, w, h, font_8x8);
}

// ── Generic 8×8 bit-font density converter (ATASCII, Apple ][, …) ────────────────
// A platform-agnostic version of the ASCII converter: given any 8×8 bitmap font and a
// pool of glyph indices, map each cell's brightness to the glyph whose ink coverage
// matches, colour it from the palette (or monochrome), optionally inverse-video. Used
// by the Atari + Apple modes; each supplies its own ROM font + glyph pool + toggles.

/// One cell of a generic bit-font grid: a glyph index into the mode's font + fg/bg palette indices.
#[derive(Clone, Copy)]
pub struct BitCell {
    pub glyph: u16,
    pub fg: u8,
    pub bg: u8,
}

/// A converted bit-font screen (`cols`×`rows` 8×8 cells over `palette`).
pub struct BitGrid {
    pub cols: usize,
    pub rows: usize,
    pub cells: Vec<BitCell>,
    pub palette: Vec<[u8; 4]>,
}

/// Ink coverage (0..1) of an 8×8 glyph.
fn bit_cov(g: &[u8; 8]) -> f32 {
    g.iter().map(|b| b.count_ones()).sum::<u32>() as f32 / 64.0
}

/// The light→dark ramp for a bit-font: `(glyph_index, coverage)` sorted by coverage, one
/// representative per distinct coverage bucket (first in `pool` order wins). Always starts with a
/// blank (a zero-coverage glyph if the font has one, else a synthetic space at index 0).
pub fn bitfont_ramp(font: &[[u8; 8]], pool: &[u16]) -> Vec<(u16, f32)> {
    let mut seen: std::collections::HashMap<u16, (u16, f32)> = std::collections::HashMap::new();
    for &gi in pool {
        if let Some(g) = font.get(gi as usize) {
            let c = bit_cov(g);
            let bucket = (c * 64.0).round() as u16;
            seen.entry(bucket).or_insert((gi, c));
        }
    }
    let mut ramp: Vec<(u16, f32)> = seen.into_values().collect();
    ramp.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    if ramp.first().map(|(_, c)| *c > 0.0).unwrap_or(true) {
        // Prefer a real blank glyph from the pool; else fall back to index 0.
        let blank = pool
            .iter()
            .copied()
            .find(|&gi| font.get(gi as usize).map(|g| bit_cov(g) == 0.0).unwrap_or(false))
            .unwrap_or(0);
        ramp.insert(0, (blank, 0.0));
    }
    ramp
}

/// Analyse `rgba` (w×h) into a [`BitGrid`] over `font`/`pool`: per `cell_w`×`cell_h` cell, pick the
/// ramp glyph matching the cell's darkness; colour per-cell from `palette` when `color` (else a
/// fixed ink-on-paper monochrome). `invert` swaps ink/paper (inverse video).
#[allow(clippy::too_many_arguments)]
pub fn bitfont_grid(
    rgba: &[u8],
    w: usize,
    h: usize,
    palette: &[[u8; 4]],
    cell_w: usize,
    cell_h: usize,
    font: &[[u8; 8]],
    pool: &[u16],
    color: bool,
    invert: bool,
    mono: Option<([u8; 3], [u8; 3])>,
) -> BitGrid {
    let cw = cell_w.max(1);
    let ch_ = cell_h.max(1);
    let cols = w.div_ceil(cw);
    let rows = h.div_ceil(ch_);
    let mut cells = Vec::with_capacity(cols * rows);
    // Working palette + ink/paper. When `mono` overrides (the unified fg/bg chips), append the
    // chosen bg + fg and use them; else fall back to the palette's darkest/brightest.
    let (out_pal, ink, paper) = build_mono_palette(palette, mono);
    if palette.is_empty() || w == 0 || h == 0 {
        cells.resize(cols * rows, BitCell { glyph: 0, fg: 0, bg: 0 });
        return BitGrid { cols, rows, cells, palette: out_pal };
    }
    let mut ramp = bitfont_ramp(font, pool);
    if ramp.is_empty() {
        ramp.push((0, 0.0));
    }
    for cy in 0..rows {
        for cx in 0..cols {
            let (mut sr, mut sg, mut sb, mut sa, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
            for y in cy * ch_..((cy + 1) * ch_).min(h) {
                for x in cx * cw..((cx + 1) * cw).min(w) {
                    let o = (y * w + x) * 4;
                    let a = rgba[o + 3] as u32;
                    sr += rgba[o] as u32 * a;
                    sg += rgba[o + 1] as u32 * a;
                    sb += rgba[o + 2] as u32 * a;
                    sa += a;
                    n += 1;
                }
            }
            let (glyph, fg) = if n == 0 || sa == 0 {
                (ramp[0].0, ink)
            } else {
                let avg = [(sr / sa) as u8, (sg / sa) as u8, (sb / sa) as u8];
                let lum = (0.299 * avg[0] as f32 + 0.587 * avg[1] as f32 + 0.114 * avg[2] as f32)
                    / 255.0;
                let cover = (sa as f32 / (n as f32 * 255.0)) * (1.0 - lum);
                let g = ramp
                    .iter()
                    .min_by(|a, b| (a.1 - cover).abs().partial_cmp(&(b.1 - cover).abs()).unwrap())
                    .map(|(g, _)| *g)
                    .unwrap_or(0);
                (g, if color { nearest_index(avg, palette) } else { ink })
            };
            let (fg, bg) = if invert { (paper, fg) } else { (fg, paper) };
            cells.push(BitCell { glyph, fg, bg });
        }
    }
    BitGrid { cols, rows, cells, palette: out_pal }
}

/// The working palette + `(ink, paper)` indices for a mono char converter. `mono` = the unified
/// (fg, bg) chip colours: when set they're appended and used verbatim; else ink/paper are the
/// palette's nearest-to-white / nearest-to-black.
fn build_mono_palette(
    palette: &[[u8; 4]],
    mono: Option<([u8; 3], [u8; 3])>,
) -> (Vec<[u8; 4]>, u8, u8) {
    let mut pal = palette.to_vec();
    if let Some((fg, bg)) = mono {
        pal.push([bg[0], bg[1], bg[2], 255]);
        let paper = (pal.len() - 1) as u8;
        pal.push([fg[0], fg[1], fg[2], 255]);
        let ink = (pal.len() - 1) as u8;
        (pal, ink, paper)
    } else {
        let ink = nearest_index([255, 255, 255], palette);
        let paper = nearest_index([0, 0, 0], palette);
        (pal, ink, paper)
    }
}

/// Render a [`BitGrid`] back into `rgba` in place using `font`'s 8×8 glyphs, each nearest-scaled to
/// `cell_w`×`cell_h`.
pub fn bitfont_render(
    grid: &BitGrid,
    font: &[[u8; 8]],
    rgba: &mut [u8],
    w: usize,
    h: usize,
    cell_w: usize,
    cell_h: usize,
) {
    if grid.palette.is_empty() {
        return;
    }
    let (cw, ch_) = (cell_w.max(1), cell_h.max(1));
    for cy in 0..grid.rows {
        for cx in 0..grid.cols {
            let cell = grid.cells[cy * grid.cols + cx];
            let g = font.get(cell.glyph as usize).copied().unwrap_or([0; 8]);
            let fg = grid.palette[cell.fg as usize % grid.palette.len()];
            let bg = grid.palette[cell.bg as usize % grid.palette.len()];
            for ry in 0..ch_ {
                let y = cy * ch_ + ry;
                if y >= h {
                    break;
                }
                let bits = g[(ry * 8 / ch_).min(7)];
                for rx in 0..cw {
                    let x = cx * cw + rx;
                    if x >= w {
                        break;
                    }
                    let on = (bits >> (7 - (rx * 8 / cw).min(7))) & 1 == 1;
                    let col = if on { fg } else { bg };
                    let o = (y * w + x) * 4;
                    rgba[o..o + 4].copy_from_slice(&col);
                }
            }
        }
    }
}

/// The Apple ][ render font: the text set (`APPLE2_FONT`, indices 0..95) followed by the MouseText
/// glyphs (95..127), so a single font+index space covers both. Built once.
pub fn apple_font() -> &'static [[u8; 8]] {
    static F: std::sync::OnceLock<Vec<[u8; 8]>> = std::sync::OnceLock::new();
    F.get_or_init(|| {
        let mut v = crate::decode::APPLE2_FONT.to_vec();
        v.extend_from_slice(&crate::decode::APPLE2_MOUSETEXT);
        v
    })
}

/// The Apple ][ **80-column** (PR#3) render font: the PRNumber3 text set followed by the same
/// MouseText block, so it shares the text/MouseText index layout with [`apple_font`].
pub fn apple_font_80() -> &'static [[u8; 8]] {
    static F: std::sync::OnceLock<Vec<[u8; 8]>> = std::sync::OnceLock::new();
    F.get_or_init(|| {
        let mut v = crate::decode::APPLE2_80_FONT.to_vec();
        v.extend_from_slice(&crate::decode::APPLE2_MOUSETEXT);
        v
    })
}

/// Number of text glyphs in [`apple_font`] / [`apple_font_80`] before the MouseText block begins
/// (both text sets are the same length, so one value serves both).
pub fn apple_text_len() -> usize {
    crate::decode::APPLE2_FONT.len()
}

/// The bit-font **pipeline pass**: build the grid then paint it back in place — the twin of
/// [`ascii_pass`], so ATASCII / Apple ][ apply everywhere the pipeline runs.
#[allow(clippy::too_many_arguments)]
pub fn bitfont_pass(
    rgba: &mut [u8],
    w: usize,
    h: usize,
    palette: &[[u8; 4]],
    cell_w: usize,
    cell_h: usize,
    font: &[[u8; 8]],
    pool: &[u16],
    color: bool,
    invert: bool,
    mono: Option<([u8; 3], [u8; 3])>,
) {
    if palette.is_empty() || w == 0 || h == 0 {
        return;
    }
    let grid = bitfont_grid(rgba, w, h, palette, cell_w, cell_h, font, pool, color, invert, mono);
    bitfont_render(&grid, font, rgba, w, h, cell_w, cell_h);
}

// ── REXPaint GlyphFont density converter (any cell size) ────────────────────────
// The bit-font converter generalized to a [`crate::decode::rexfont::GlyphFont`] of any
// cell size. Used by the REXPaint-font image→art mode; the render is also reused by the
// textmode viewer to re-draw decoded cells in a chosen font.

use crate::decode::rexfont::GlyphFont;

/// Coverage-sorted ramp over `font`'s glyphs (one representative per coverage bucket, blank first).
/// `pool` restricts the candidate glyphs to those indices; empty = all 256. Same idea as
/// [`bitfont_ramp`] but reading the GlyphFont's own coverage.
pub fn glyphfont_ramp(font: &GlyphFont, pool: &[u16]) -> Vec<(u16, f32)> {
    let total = (font.cell_w * font.cell_h).max(1) as f32;
    // Empty pool = every glyph in the font. Span the font's real length (not a hardcoded 256) so
    // fonts larger than 256 glyphs — e.g. the all-ranges Unicode ramp (~600) plus user codepoints —
    // aren't silently truncated to their first 256 glyphs.
    let all: Vec<u16> = (0..font.glyphs.len().min(u16::MAX as usize) as u16).collect();
    let candidates: &[u16] = if pool.is_empty() { &all } else { pool };
    let mut seen: std::collections::HashMap<u16, (u16, f32)> = std::collections::HashMap::new();
    for &gi in candidates {
        let c = font.coverage(gi as usize);
        let bucket = (c * total).round() as u16;
        seen.entry(bucket).or_insert((gi, c));
    }
    let mut ramp: Vec<(u16, f32)> = seen.into_values().collect();
    ramp.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    if ramp.first().map(|(_, c)| *c > 0.0).unwrap_or(true) {
        // A blank glyph, drawn from the same candidate set so a restricted pool keeps its light end.
        let blank = candidates
            .iter()
            .copied()
            .find(|&g| font.coverage(g as usize) == 0.0)
            .unwrap_or(32);
        ramp.insert(0, (blank, 0.0));
    }
    ramp
}

/// Analyse `rgba` into a [`BitGrid`] over `font` (cell size = the font's), mapping each cell's
/// darkness to the ramp glyph and colouring from `palette` (per-cell when `color`, else mono).
/// `pool` restricts which glyphs may be used (empty = all 256).
#[allow(clippy::too_many_arguments)]
pub fn glyphfont_grid(
    rgba: &[u8],
    w: usize,
    h: usize,
    palette: &[[u8; 4]],
    font: &GlyphFont,
    pool: &[u16],
    color: bool,
    invert: bool,
) -> BitGrid {
    let (cw, ch_) = (font.cell_w.max(1), font.cell_h.max(1));
    let cols = w.div_ceil(cw);
    let rows = h.div_ceil(ch_);
    let mut cells = Vec::with_capacity(cols * rows);
    let paper = nearest_index([0, 0, 0], palette);
    let ink = nearest_index([255, 255, 255], palette);
    if palette.is_empty() || w == 0 || h == 0 {
        cells.resize(cols * rows, BitCell { glyph: 0, fg: 0, bg: 0 });
        return BitGrid { cols, rows, cells, palette: palette.to_vec() };
    }
    let mut ramp = glyphfont_ramp(font, pool);
    if ramp.is_empty() {
        ramp.push((32, 0.0));
    }
    for cy in 0..rows {
        for cx in 0..cols {
            let (mut sr, mut sg, mut sb, mut sa, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
            for y in cy * ch_..((cy + 1) * ch_).min(h) {
                for x in cx * cw..((cx + 1) * cw).min(w) {
                    let o = (y * w + x) * 4;
                    let a = rgba[o + 3] as u32;
                    sr += rgba[o] as u32 * a;
                    sg += rgba[o + 1] as u32 * a;
                    sb += rgba[o + 2] as u32 * a;
                    sa += a;
                    n += 1;
                }
            }
            let (glyph, fg) = if n == 0 || sa == 0 {
                (ramp[0].0, ink)
            } else {
                let avg = [(sr / sa) as u8, (sg / sa) as u8, (sb / sa) as u8];
                let lum = (0.299 * avg[0] as f32 + 0.587 * avg[1] as f32 + 0.114 * avg[2] as f32)
                    / 255.0;
                let cover = (sa as f32 / (n as f32 * 255.0)) * (1.0 - lum);
                let g = ramp
                    .iter()
                    .min_by(|a, b| (a.1 - cover).abs().partial_cmp(&(b.1 - cover).abs()).unwrap())
                    .map(|(g, _)| *g)
                    .unwrap_or(0);
                (g, if color { nearest_index(avg, palette) } else { ink })
            };
            let (fg, bg) = if invert { (paper, fg) } else { (fg, paper) };
            cells.push(BitCell { glyph, fg, bg });
        }
    }
    BitGrid { cols, rows, cells, palette: palette.to_vec() }
}

/// Render a [`BitGrid`] using `font`'s glyphs (cell size = the font's) into `rgba` in place.
/// Shared by the REXPaint-font converter and the textmode viewer's font re-render.
pub fn glyphfont_render(grid: &BitGrid, font: &GlyphFont, rgba: &mut [u8], w: usize, h: usize) {
    if grid.palette.is_empty() {
        return;
    }
    let (cw, ch_) = (font.cell_w.max(1), font.cell_h.max(1));
    for cy in 0..grid.rows {
        for cx in 0..grid.cols {
            let cell = grid.cells[cy * grid.cols + cx];
            let fg = grid.palette[cell.fg as usize % grid.palette.len()];
            let bg = grid.palette[cell.bg as usize % grid.palette.len()];
            for ry in 0..ch_ {
                let y = cy * ch_ + ry;
                if y >= h {
                    break;
                }
                for rx in 0..cw {
                    let x = cx * cw + rx;
                    if x >= w {
                        break;
                    }
                    let col = if font.on(cell.glyph as usize, rx, ry) { fg } else { bg };
                    let o = (y * w + x) * 4;
                    rgba[o..o + 4].copy_from_slice(&col);
                }
            }
        }
    }
}

/// The REXPaint-font **pipeline pass**: build the grid then paint it back in place. `pool`
/// restricts the usable glyphs (empty = all 256).
#[allow(clippy::too_many_arguments)]
pub fn glyphfont_pass(
    rgba: &mut [u8],
    w: usize,
    h: usize,
    palette: &[[u8; 4]],
    font: &GlyphFont,
    pool: &[u16],
    color: bool,
    invert: bool,
) {
    if palette.is_empty() || w == 0 || h == 0 {
        return;
    }
    let grid = glyphfont_grid(rgba, w, h, palette, font, pool, color, invert);
    glyphfont_render(&grid, font, rgba, w, h);
}

// ── Unicode text-art converter (half-block + Braille) ───────────────────────────
// Converts an image to real UTF-8 art with no font needed to render: half-block (▀)
// packs 2 vertical truecolour pixels per character; Braille (U+2800..) packs a 2×4 dot
// cell for hi-res line/tone art. Both render natively (the pass fills the shapes) AND
// serialize to copy-pasteable text ([`unicode_to_text`]).

/// A converted Unicode-art screen: `cols`×`rows` cells, each a `ch` glyph with fg/bg colours.
pub struct UniGrid {
    pub cols: usize,
    pub rows: usize,
    pub style: u8,
    pub chars: Vec<char>,
    pub fg: Vec<[u8; 3]>,
    pub bg: Vec<[u8; 3]>,
}

/// Braille dot bit for sub-cell (`dx` 0..2, `dy` 0..4). Matches Unicode's dot numbering
/// (1-2-3-7 down the left column, 4-5-6-8 down the right).
const BRAILLE_BITS: [[u8; 4]; 2] = [[0, 1, 2, 6], [3, 4, 5, 7]];

/// Convert `rgba` (`w`×`h`) into a [`UniGrid`] of `cols` characters wide (rows follow the image
/// aspect). `style` picks half-block vs Braille; `invert` flips the Braille on/off test.
#[allow(clippy::needless_range_loop)] // dx/dy index BRAILLE_BITS *and* compute the pixel offset
pub fn unicode_convert(
    rgba: &[u8],
    w: usize,
    h: usize,
    style: u8,
    cols: usize,
    invert: bool,
) -> UniGrid {
    let cols = cols.max(1);
    let aspect = h.max(1) as f32 / w.max(1) as f32;
    if style == UNI_BRAILLE {
        let dots_x = cols * 2;
        let dots_y = ((dots_x as f32 * aspect).round() as usize).max(4);
        let rows = dots_y.div_ceil(4);
        let small = box_downscale(rgba, w, h, dots_x, rows * 4);
        let (mut chars, mut fg, mut bg) = (Vec::new(), Vec::new(), Vec::new());
        for cy in 0..rows {
            for cx in 0..cols {
                let mut bits = 0u8;
                let (mut sr, mut sg, mut sb, mut n) = (0u32, 0u32, 0u32, 0u32);
                for dx in 0..2 {
                    for dy in 0..4 {
                        let o = ((cy * 4 + dy) * dots_x + (cx * 2 + dx)) * 4;
                        let (r, g, b) = (small[o] as f32, small[o + 1] as f32, small[o + 2] as f32);
                        let lum = (0.299 * r + 0.587 * g + 0.114 * b) / 255.0;
                        // Dark pixels become dots (ink-on-paper); invert flips it.
                        let on = if invert { lum > 0.5 } else { lum < 0.5 };
                        if on {
                            bits |= 1 << BRAILLE_BITS[dx][dy];
                            sr += r as u32;
                            sg += g as u32;
                            sb += b as u32;
                            n += 1;
                        }
                    }
                }
                chars.push(char::from_u32(0x2800 + bits as u32).unwrap_or('⠀'));
                fg.push(if n > 0 {
                    [(sr / n) as u8, (sg / n) as u8, (sb / n) as u8]
                } else {
                    [200, 200, 200]
                });
                bg.push([0, 0, 0]);
            }
        }
        return UniGrid { cols, rows, style, chars, fg, bg };
    }
    // Half-block: each char = 1 wide × 2 tall truecolour pixels (▀ upper on fg, lower on bg).
    let px_h = ((cols as f32 * aspect).round() as usize).max(2);
    let rows = px_h.div_ceil(2);
    let small = box_downscale(rgba, w, h, cols, rows * 2);
    let (mut chars, mut fg, mut bg) = (Vec::new(), Vec::new(), Vec::new());
    for cy in 0..rows {
        for cx in 0..cols {
            let ot = ((cy * 2) * cols + cx) * 4;
            let ob = ((cy * 2 + 1) * cols + cx) * 4;
            chars.push('▀');
            fg.push([small[ot], small[ot + 1], small[ot + 2]]);
            bg.push([small[ob], small[ob + 1], small[ob + 2]]);
        }
    }
    UniGrid { cols, rows, style, chars, fg, bg }
}

/// Render a [`UniGrid`] into `rgba` in place, drawing each cell's shape directly (no font):
/// half-block fills the top/bottom halves; Braille draws a dot per set bit.
pub fn unicode_render(grid: &UniGrid, rgba: &mut [u8], w: usize, h: usize) {
    if grid.cols == 0 || grid.rows == 0 {
        return;
    }
    let cellw = (w / grid.cols).max(1);
    let cellh = (h / grid.rows).max(1);
    for cy in 0..grid.rows {
        for cx in 0..grid.cols {
            let i = cy * grid.cols + cx;
            let (fg, bg, ch) = (grid.fg[i], grid.bg[i], grid.chars[i]);
            let bits = (ch as u32).wrapping_sub(0x2800);
            for ry in 0..cellh {
                let y = cy * cellh + ry;
                if y >= h {
                    break;
                }
                for rx in 0..cellw {
                    let x = cx * cellw + rx;
                    if x >= w {
                        break;
                    }
                    let on = if grid.style == UNI_BRAILLE {
                        // Which 2×4 dot are we in, and is a dot painted near its centre?
                        let dx = (rx * 2 / cellw).min(1);
                        let dy = (ry * 4 / cellh).min(3);
                        let set = (bits >> BRAILLE_BITS[dx][dy]) & 1 == 1;
                        // Round dot: paint the inner ~60% of the sub-cell.
                        let (sw, sh) = ((cellw / 2).max(1), (cellh / 4).max(1));
                        let (lx, ly) = (rx % sw, ry % sh);
                        let inset = |p: usize, s: usize| p * 5 >= s && p * 5 < s * 4;
                        set && inset(lx, sw) && inset(ly, sh)
                    } else {
                        ry * 2 < cellh // half-block: top half is fg
                    };
                    let c = if on { fg } else { bg };
                    let o = (y * w + x) * 4;
                    rgba[o..o + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
                }
            }
        }
    }
}

/// The Unicode **pipeline pass**: convert then render in place (no palette needed — colours come
/// straight from the image).
pub fn unicode_pass(rgba: &mut [u8], w: usize, h: usize, style: u8, cols: usize, invert: bool) {
    if w == 0 || h == 0 {
        return;
    }
    let grid = unicode_convert(rgba, w, h, style, cols, invert);
    // Rebuild the buffer background so cells that don't fill (Braille gaps) read as black.
    for px in rgba.chunks_exact_mut(4) {
        px.copy_from_slice(&[0, 0, 0, 255]);
    }
    unicode_render(&grid, rgba, w, h);
}

/// Build the density [`BitGrid`] for the Unicode **Ramp** style: exactly `cols` glyphs wide (rows
/// preserve the image aspect), each the ramp glyph matching that region's tone, coloured from
/// `palette`. Shared by the pass (render) and text export (glyph→char).
#[allow(clippy::too_many_arguments)]
pub fn unicode_ramp_grid(
    rgba: &[u8],
    w: usize,
    h: usize,
    palette: &[[u8; 4]],
    font: &GlyphFont,
    pool: &[u16],
    cols: usize,
    color: bool,
    invert: bool,
) -> BitGrid {
    let (cw, ch) = (font.cell_w.max(1), font.cell_h.max(1));
    let gw = cols.max(1);
    let gh = ((((gw * cw) as f32 * h as f32 / w.max(1) as f32).round() as usize) / ch).max(1);
    let small = box_downscale(rgba, w, h, gw * cw, gh * ch);
    // No active Reduce/palette → colour from the full xterm-256 set (rich + exports cleanly).
    let default_pal;
    let pal: &[[u8; 4]] = if palette.is_empty() {
        default_pal = xterm256_palette();
        &default_pal
    } else {
        palette
    };
    glyphfont_grid(&small, gw * cw, gh * ch, pal, font, pool, color, invert)
}

/// The Unicode **Ramp pipeline pass**: build the grid then render it (scaled) into `rgba` in place.
#[allow(clippy::too_many_arguments)]
pub fn unicode_ramp_pass(
    rgba: &mut [u8],
    w: usize,
    h: usize,
    palette: &[[u8; 4]],
    font: &GlyphFont,
    pool: &[u16],
    cols: usize,
    color: bool,
    invert: bool,
) {
    if w == 0 || h == 0 {
        return;
    }
    let (cw, ch) = (font.cell_w.max(1), font.cell_h.max(1));
    let grid = unicode_ramp_grid(rgba, w, h, palette, font, pool, cols, color, invert);
    let (pw, ph) = (grid.cols * cw, grid.rows * ch);
    if pw == 0 || ph == 0 {
        return;
    }
    let mut px = vec![0u8; pw * ph * 4];
    glyphfont_render(&grid, font, &mut px, pw, ph);
    for y in 0..h {
        let sy = (y * ph / h).min(ph - 1);
        for x in 0..w {
            let sx = (x * pw / w).min(pw - 1);
            let so = (sy * pw + sx) * 4;
            let d = (y * w + x) * 4;
            rgba[d..d + 4].copy_from_slice(&px[so..so + 4]);
        }
    }
}

/// Serialize a [`UniGrid`] to text. `ansi_color` emits **xterm-256** SGR fg/bg per cell (the
/// standard terminal-art colour format) — needed for half-block to carry its colours; Braille
/// still reads fine as plain glyphs. An SGR is emitted only when the colour index changes. Rows
/// end in `\n`; the file resets colour at each row end.
pub fn unicode_to_text(grid: &UniGrid, ansi_color: bool) -> String {
    let mut s = String::new();
    for cy in 0..grid.rows {
        let (mut cf, mut cb) = (256i16, 256i16);
        for cx in 0..grid.cols {
            let i = cy * grid.cols + cx;
            if ansi_color {
                let f = nearest_xterm256(grid.fg[i]) as i16;
                let b = nearest_xterm256(grid.bg[i]) as i16;
                if f != cf || b != cb {
                    s.push_str(&format!("\x1b[38;5;{f};48;5;{b}m"));
                    cf = f;
                    cb = b;
                }
            }
            s.push(grid.chars[i]);
        }
        if ansi_color {
            s.push_str("\x1b[0m");
        }
        s.push('\n');
    }
    s
}

/// A C64 **screen code** → **PETSCII** (printable) byte, matching petmate's `convertToSEQ`. The
/// reverse bit is handled by the caller (RVS ON/OFF), so this maps the base glyph.
fn screencode_to_petscii(c: u8) -> u8 {
    match c {
        0x00..=0x1f => c + 0x40,
        0x40..=0x5d => c + 0x80,
        0x5e => 0xff,
        0x5f => 0xdf,
        0x60..=0x7f => c + 0x40,
        0x95 => 0xdf,
        0x80..=0xbf => c - 0x80,
        0xc0..=0xff => c - 0x40,
        _ => c, // 0x20..=0x3f pass through
    }
}

/// petmate's "upper"/"lower" charset name for a font page (0 = upper/graphics, 1 = lower).
fn petscii_charset_name(page: usize) -> &'static str {
    if page == 1 {
        "lower"
    } else {
        "upper"
    }
}

/// Serialize a [`PetsciiGrid`] to a C64 `.seq` stream: PETSCII glyph bytes with colour-control
/// bytes on each colour change, RVS on/off (`0x12`/`0x92`) around reverse glyphs, a charset-set
/// prefix, an optional clear-screen, and a CR (`0x0d`) per row. Displayable on a real C64.
pub fn petscii_grid_to_seq(grid: &PetsciiGrid, clear: bool) -> Vec<u8> {
    // idx = VIC-II colour, value = the PETSCII colour-control byte (petmate's `seq_colors`).
    const COLORS: [u8; 16] = [
        0x90, 0x05, 0x1c, 0x9f, 0x9c, 0x1e, 0x1f, 0x9e, 0x81, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a,
        0x9b,
    ];
    let mut out = Vec::new();
    if clear {
        out.push(0x93); // clear screen
    }
    out.push(if grid.page == 1 { 0x0e } else { 0x8e }); // lower / upper+graphics charset
    let mut cur_fg: Option<u8> = None;
    let mut rvs = false;
    for cy in 0..grid.rows {
        for cx in 0..grid.cols {
            let cell = grid.cells[cy * grid.cols + cx];
            if cur_fg != Some(cell.fg) {
                out.push(COLORS[(cell.fg & 15) as usize]);
                cur_fg = Some(cell.fg);
            }
            let want_rvs = cell.code & 0x80 != 0;
            if want_rvs != rvs {
                out.push(if want_rvs { 0x12 } else { 0x92 });
                rvs = want_rvs;
            }
            out.push(screencode_to_petscii(cell.code));
        }
        out.push(0x0d); // CR
    }
    if rvs {
        out.push(0x92); // leave RVS off
    }
    out
}

/// Serialize to petmate's **native `.petmate`** JSON (opens for editing in petmate): one framebuf
/// whose `framebuf` is a 2-D array (rows) of `{code, color}` cells over a global background.
pub fn petscii_grid_to_petmate(grid: &PetsciiGrid) -> Vec<u8> {
    let mut s = String::from("{\"version\":2,\"screens\":[0],\"framebufs\":[{");
    s.push_str(&format!("\"width\":{},\"height\":{},", grid.cols, grid.rows));
    s.push_str(&format!(
        "\"backgroundColor\":{},\"borderColor\":{},",
        grid.bg, grid.bg
    ));
    s.push_str(&format!(
        "\"charset\":\"{}\",\"name\":\"kaleidotron\",\"framebuf\":[",
        petscii_charset_name(grid.page)
    ));
    for cy in 0..grid.rows {
        if cy > 0 {
            s.push(',');
        }
        s.push('[');
        for cx in 0..grid.cols {
            if cx > 0 {
                s.push(',');
            }
            let c = grid.cells[cy * grid.cols + cx];
            s.push_str(&format!("{{\"code\":{},\"color\":{}}}", c.code, c.fg));
        }
        s.push(']');
    }
    s.push_str("]}]}");
    s.into_bytes()
}

/// Serialize to petmate's flat **`.json`** interchange format: `screencodes[]` + `colors[]` flat
/// arrays (width×height), easiest to script against.
pub fn petscii_grid_to_json(grid: &PetsciiGrid) -> Vec<u8> {
    let codes: Vec<String> = grid.cells.iter().map(|c| c.code.to_string()).collect();
    let colors: Vec<String> = grid.cells.iter().map(|c| c.fg.to_string()).collect();
    let s = format!(
        "{{\"version\":1,\"framebufs\":[{{\"width\":{},\"height\":{},\"backgroundColor\":{},\"borderColor\":{},\"charset\":\"{}\",\"name\":\"kaleidotron\",\"screencodes\":[{}],\"colors\":[{}]}}]}}",
        grid.cols,
        grid.rows,
        grid.bg,
        grid.bg,
        petscii_charset_name(grid.page),
        codes.join(","),
        colors.join(",")
    );
    s.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_40_and_80_fonts_are_distinct() {
        // PR#0 (PrintChar21) and PR#3 (PRNumber3) must be genuinely different glyph sets — an
        // earlier regen accidentally emitted identical bytes, making the toggle a no-op.
        let (f40, f80) = (apple_font(), apple_font_80());
        assert_eq!(f40.len(), f80.len());
        let text = apple_text_len();
        let differing = (0..text).filter(|&i| f40[i] != f80[i]).count();
        assert!(differing > text / 2, "PR#0 vs PR#3 differ on most glyphs ({differing}/{text})");
        // A concrete check: the letter 'A' (code 0x21 in the Apple text set) differs.
        assert_ne!(f40[0x21], f80[0x21], "'A' differs between the two fonts");
        // PR#0 (40-col PrintChar21) is the bold/wide font; PR#3 (80-col PRNumber3) is thin. Guard
        // the roles so a regen can't silently flip them: PR#0 lays down more ink over the letters.
        let ink = |f: &[[u8; 8]]| -> u32 {
            (0x21..0x3B).map(|i| f[i].iter().map(|b| b.count_ones()).sum::<u32>()).sum()
        };
        assert!(ink(f40) > ink(f80), "PR#0 is bolder than PR#3 ({} vs {})", ink(f40), ink(f80));
    }

    #[test]
    fn ascii_ramp_spans_blank_to_full() {
        // The ramp is coverage-sorted: lightest (space, 0.0) first. With High ASCII on it
        // reaches the full block █ (CP437 219, coverage 1.0) at the dark end.
        let hi = AsciiCharset { high: true, ..Default::default() };
        let ramp = ascii_ramp(&hi, true);
        assert_eq!(ramp.first().unwrap().1, 0.0, "lightest is a blank");
        let (dark_glyph, dark_cov) = *ramp.last().unwrap();
        assert!(dark_cov > 0.9, "darkest ramp entry is nearly full ink");
        assert_eq!(dark_glyph, 219, "the full block anchors the dark end");
        // Without High ASCII the block glyphs are gone, so the dark end is lighter.
        let low = ascii_ramp(&AsciiCharset::default(), true);
        assert!(low.last().unwrap().1 < 0.9, "printable-only ramp can't reach solid");
        // Blocks category brings the full block back even with High ASCII off.
        let blk = AsciiCharset { blocks: true, ..Default::default() };
        assert_eq!(ascii_ramp(&blk, true).last().unwrap().0, 219, "Blocks adds the full block");
        // "Use only chars" uses exactly the typed set, coverage-sorted.
        let only = AsciiCharset { only: b" .oOX".to_vec(), ..Default::default() };
        let r = ascii_ramp(&only, true);
        assert_eq!(r.len(), 5, "every typed glyph kept");
        assert_eq!(r.first().unwrap().0, b' ', "space is lightest");
    }

    #[test]
    fn ascii_maps_brightness_to_density() {
        // Two cells: white (left) → blank/space, black (right) → dense glyph.
        let pal = [[0u8, 0, 0, 255], [255, 255, 255, 255]];
        let (w, h) = (16usize, 8usize);
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let c = if x < 8 { 255u8 } else { 0u8 }; // left white, right black
                let o = (y * w + x) * 4;
                rgba[o..o + 4].copy_from_slice(&[c, c, c, 255]);
            }
        }
        let cs = AsciiCharset { high: true, ..Default::default() };
        let grid = ascii_grid(&rgba, w, h, &pal, 8, 8, &cs, false, false, true, None);
        assert_eq!(grid.cols, 2);
        let left = grid.cells[0];
        let right = grid.cells[1];
        assert_eq!(left.ch, 32, "white cell is a space");
        let cov = |c: u8| CP437_8X8[c as usize].iter().map(|b| b.count_ones()).sum::<u32>();
        assert!(cov(right.ch) > cov(left.ch), "black cell is denser than white");
    }

    #[test]
    fn unicode_halfblock_and_braille() {
        // Half-block: a top-red / bottom-blue image → a ▀ cell with fg red, bg blue.
        let (w, h) = (2usize, 2usize);
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let c = if y == 0 { [255u8, 0, 0, 255] } else { [0, 0, 255, 255] };
                rgba[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&c);
            }
        }
        let g = unicode_convert(&rgba, w, h, UNI_HALFBLOCK, 2, false);
        assert_eq!(g.chars[0], '▀');
        assert_eq!(g.fg[0], [255, 0, 0], "top → fg red");
        assert_eq!(g.bg[0], [0, 0, 255], "bottom → bg blue");
        // Braille: an all-black image → all dots set → the full braille cell ⣿ (U+28FF).
        let black = vec![0u8; 8 * 8 * 4];
        let gb = unicode_convert(&black, 8, 8, UNI_BRAILLE, 1, false);
        assert!(gb.chars.iter().all(|&c| c == '\u{28FF}'), "dark → all dots");
        // Text output: braille is plain glyphs; ends each row with newline.
        let txt = unicode_to_text(&gb, false);
        assert!(txt.contains('\u{28FF}') && txt.ends_with('\n'));
    }

    #[test]
    fn bitfont_maps_brightness_and_inverts() {
        // A tiny 2-glyph font: index 0 blank, index 1 full block.
        let font: [[u8; 8]; 2] = [[0; 8], [0xff; 8]];
        let pal = [[0u8, 0, 0, 255], [255, 255, 255, 255]];
        let (w, h) = (16usize, 8usize);
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let c = if x < 8 { 255u8 } else { 0u8 }; // left white, right black
                let o = (y * w + x) * 4;
                rgba[o..o + 4].copy_from_slice(&[c, c, c, 255]);
            }
        }
        let pool = [0u16, 1];
        let g = bitfont_grid(&rgba, w, h, &pal, 8, 8, &font, &pool, false, false, None);
        assert_eq!(g.cols, 2);
        assert_eq!(g.cells[0].glyph, 0, "white cell -> blank glyph");
        assert_eq!(g.cells[1].glyph, 1, "black cell -> full block");
        // Invert swaps ink/paper: the fg becomes paper (nearest-black index).
        let paper = nearest_index([0, 0, 0], &pal);
        let gi = bitfont_grid(&rgba, w, h, &pal, 8, 8, &font, &pool, false, true, None);
        assert_eq!(gi.cells[1].fg, paper, "inverse video swaps ink to paper");
    }

    #[test]
    fn petscii_solid_color_round_trips() {
        // A solid VIC-II colour converts + renders back to that same colour (bg auto-picks it).
        let red = crate::decode::VIC2[2];
        let rgba: Vec<u8> = std::iter::repeat(red).take(64).flatten().collect();
        let grid = petscii_grid(&rgba, 8, 8, 1, 1, 0, 1.0, None, &crate::decode::VIC2, None);
        let (w, h, out) = petscii_render(&grid, &crate::decode::VIC2);
        assert_eq!((w, h), (8, 8));
        for px in out.chunks_exact(4) {
            assert_eq!(&px[0..3], &red[0..3], "solid red stays red");
        }
    }

    #[test]
    fn petscii_captures_two_tone() {
        // Left half red, right half blue over two 8×8 cells → the render keeps each side.
        let red = crate::decode::VIC2[2];
        let blue = crate::decode::VIC2[6];
        let (w, h) = (16usize, 8usize);
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let c = if x < 8 { red } else { blue };
                let o = (y * w + x) * 4;
                rgba[o..o + 4].copy_from_slice(&c);
            }
        }
        let grid = petscii_grid(&rgba, w, h, 2, 1, 0, 1.0, None, &crate::decode::VIC2, None);
        let (rw, _rh, out) = petscii_render(&grid, &crate::decode::VIC2);
        let at = |x: usize, y: usize| {
            let o = (y * rw + x) * 4;
            [out[o], out[o + 1], out[o + 2]]
        };
        assert_eq!(at(2, 2), [red[0], red[1], red[2]], "left cell red");
        assert_eq!(at(10, 2), [blue[0], blue[1], blue[2]], "right cell blue");
    }

    #[test]
    fn petscii_serializers_emit_expected_bytes() {
        // 2×1: space (code 32, white) then reverse-space solid (code 160, red), bg black.
        let grid = PetsciiGrid {
            cols: 2,
            rows: 1,
            bg: 0,
            page: 0,
            cells: vec![
                PetsciiCell { code: 32, fg: 1 },
                PetsciiCell { code: 160, fg: 2 },
            ],
        };
        let seq = petscii_grid_to_seq(&grid, false);
        assert_eq!(seq[0], 0x8e, "upper/graphics charset prefix");
        assert!(seq.contains(&0x05), "white colour code");
        assert!(seq.contains(&0x1c), "red colour code");
        assert!(
            seq.contains(&0x12) && seq.contains(&0x92),
            "RVS on/off around the reverse glyph"
        );
        assert!(seq.contains(&0x0d), "CR at row end");
        assert_eq!(*seq.last().unwrap(), 0x92, "leaves RVS off at the end");

        let json = String::from_utf8(petscii_grid_to_json(&grid)).unwrap();
        assert!(json.contains("\"screencodes\":[32,160]"), "json codes: {json}");
        assert!(json.contains("\"colors\":[1,2]"), "json colors: {json}");
        assert!(json.contains("\"backgroundColor\":0"));

        let pm = String::from_utf8(petscii_grid_to_petmate(&grid)).unwrap();
        assert!(
            pm.contains("\"framebuf\":[[{\"code\":32,\"color\":1},{\"code\":160,\"color\":2}]]"),
            "petmate framebuf: {pm}"
        );
        assert!(pm.contains("\"charset\":\"upper\""));
    }
    use crate::image_types::PixImage;

    #[test]
    fn box_downscale_averages_a_checkerboard_to_grey() {
        // A 2×2 black/white checkerboard shrunk to 1×1 must average to ~50% grey
        // (the faithful shrink), not pick one corner.
        let src = vec![
            255, 255, 255, 255, 0, 0, 0, 255, // row 0: white, black
            0, 0, 0, 255, 255, 255, 255, 255, // row 1: black, white
        ];
        let out = box_downscale(&src, 2, 2, 1, 1);
        assert_eq!(out.len(), 4);
        assert!((120..=135).contains(&out[0]), "≈50% grey, got {}", out[0]);
        assert_eq!(out[3], 255, "opaque");
    }

    #[test]
    fn ansi_shade_grey_source_never_invents_false_colour() {
        // Regression for the "phantom yellow" bug: a purely GREY source, given a palette
        // that also contains saturated yellow + blue, must be shaded with greys only —
        // never yellow▒blue (which averages to grey but is garish false colour). The
        // chroma penalty in `ansi_shade_grid` is what forbids the hue-mix.
        let palette: Vec<[u8; 4]> = vec![
            [0, 0, 0, 255],       // 0 black
            [64, 64, 64, 255],    // 1 dark grey
            [128, 128, 128, 255], // 2 mid grey
            [192, 192, 192, 255], // 3 light grey
            [255, 255, 255, 255], // 4 white
            [255, 255, 0, 255],   // 5 yellow   (must NOT appear)
            [0, 0, 255, 255],     // 6 blue     (must NOT appear)
        ];
        // 18×32 image: left half grey 96, right half grey 168 — tones that sit BETWEEN
        // palette greys, so the shade search is genuinely exercised (not a solid short-
        // circuit). Cell 9×16 → a 2×2 grid.
        let (w, h) = (18usize, 32usize);
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 96u8 } else { 168u8 };
                let i = (y * w + x) * 4;
                rgba[i] = v;
                rgba[i + 1] = v;
                rgba[i + 2] = v;
                rgba[i + 3] = 255;
            }
        }
        let grid = ansi_shade_grid(
            &rgba, w, h, &palette, 9, 16, 0.25, 0.50, 0.75, true, true, true, true,
            [true; 4], [0.5; 4], // all half-blocks on, neutral usage
            1.0,   // full shading amount → always run the shade search
            false, // no iCE
            0.0,   // Smoothness 0 → proves the ALWAYS-ON baseline kills false colour
            0.30,  // Detail weight
            None,
        );
        for (i, cell) in grid.cells.iter().enumerate() {
            assert!(
                cell.fg != 5 && cell.bg != 5,
                "cell {i} used YELLOW (glyph {}) on a grey source",
                cell.ch
            );
            assert!(
                cell.fg != 6 && cell.bg != 6,
                "cell {i} used BLUE (glyph {}) on a grey source",
                cell.ch
            );
        }
    }

    #[test]
    fn ansi_shade_sharp_edge_prefers_half_block_over_shade() {
        // A cell that is top-half white / bottom-half black is a genuine horizontal edge:
        // the crisp answer is a half-block (▀/▄), NOT a 50%-shade ▒ that only matches the
        // grey average. One 9×16 cell.
        let palette: Vec<[u8; 4]> =
            vec![[0, 0, 0, 255], [128, 128, 128, 255], [255, 255, 255, 255]];
        let (w, h) = (9usize, 16usize);
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            let v = if y < 8 { 255u8 } else { 0u8 }; // top white, bottom black
            for x in 0..w {
                let i = (y * w + x) * 4;
                rgba[i] = v;
                rgba[i + 1] = v;
                rgba[i + 2] = v;
                rgba[i + 3] = 255;
            }
        }
        // Half-blocks ON → expect a horizontal half-block (▀ 223 or ▄ 220), not a shade.
        let g_on = ansi_shade_grid(
            &rgba, w, h, &palette, 9, 16, 0.25, 0.50, 0.75, true, true, true, true,
            [true; 4], [0.5; 4], 1.0, true, 0.0, 0.30, None,
        );
        assert!(
            matches!(g_on.cells[0].ch, 220 | 223),
            "sharp edge should be a half-block, got glyph {}",
            g_on.cells[0].ch
        );
        // Horizontal pair OFF (F5 ▀ + F6 ▄), vertical still on → must NOT be ▀/▄.
        let g_off = ansi_shade_grid(
            &rgba, w, h, &palette, 9, 16, 0.25, 0.50, 0.75, true, true, true, true,
            [false, false, true, true], [0.5; 4], 1.0, true, 0.0, 0.30, None,
        );
        assert!(
            !matches!(g_off.cells[0].ch, 220 | 223),
            "horizontal half-blocks disabled, yet got {}",
            g_off.cells[0].ch
        );
    }

    #[test]
    fn ansi_shade_max_detail_retains_true_edges_but_not_near_flat_speckle() {
        // Regression for the white-speckle bug: at MAX Detail (5.0) the retention reward is
        // now bounded and gated by how badly a flat solid represents the cell, so a genuine
        // edge still snaps to a crisp half-block, but a mild near-flat cell (one a solid
        // nails) is NOT forced into a bright half-block. The old `detail * raw_dist2` reward
        // was unbounded — Detail=5 flipped almost any faint imbalance to a black/white
        // half-block, stippling the dark flats.
        let palette: Vec<[u8; 4]> = vec![
            [0, 0, 0, 255],
            [64, 64, 64, 255],
            [128, 128, 128, 255],
            [192, 192, 192, 255],
            [255, 255, 255, 255],
        ];
        let cell = |top: u8, bot: u8| -> AnsiCell {
            let (w, h) = (9usize, 16usize);
            let mut rgba = vec![0u8; w * h * 4];
            for y in 0..h {
                let v = if y < 8 { top } else { bot };
                for x in 0..w {
                    let i = (y * w + x) * 4;
                    rgba[i] = v;
                    rgba[i + 1] = v;
                    rgba[i + 2] = v;
                    rgba[i + 3] = 255;
                }
            }
            // Detail = 5.0 (max), half-blocks on at neutral usage, full shade search.
            let g = ansi_shade_grid(
                &rgba, w, h, &palette, 9, 16, 0.25, 0.50, 0.75, true, true, true, true,
                [true; 4], [0.5; 4], 1.0, true, 0.0, 5.0, None,
            );
            g.cells[0]
        };
        // Strong edge (white over black): a half-block is the crisp answer — still retained.
        let strong = cell(255, 0);
        assert!(
            matches!(strong.ch, 220 | 223),
            "a true white/black edge must stay a half-block even at max Detail, got glyph {}",
            strong.ch
        );
        // Mild near-flat edge (dark 100 over dark 50): a solid dark represents it well, so at
        // max Detail it must NOT be forced into a (brighter) half-block — the speckle case.
        let mild = cell(100, 50);
        assert!(
            !matches!(mild.ch, 220 | 221 | 222 | 223),
            "a near-flat dark cell must not speckle into a half-block at max Detail, got glyph {}",
            mild.ch
        );
    }

    #[test]
    fn ansi16_export_uses_blink_bit_for_bright_bg_under_ice() {
        // A cell with a BRIGHT background (index 12 = bright blue) must be encoded the
        // scene-standard iCE way — the blink bit (SGR 5) + the base bg (44) — not xterm's
        // aixterm 104, which PabloDraw/Moebius ignore (dropping the bg to black).
        let palette: Vec<[u8; 4]> = ANSI16.iter().map(|c| [c[0], c[1], c[2], 255]).collect();
        let grid = AnsiGrid {
            cols: 1,
            rows: 1,
            cell_w: 9,
            cell_h: 16,
            palette,
            cells: vec![AnsiCell { fg: 15, bg: 12, ch: 219 }], // white on bright blue █
        };
        // The output carries raw CP437 glyph bytes (0xDB), so compare on the SGR prefix
        // (everything up to the glyph) as a lossy string.
        let ice_bytes = ansi_grid_to_ans(&grid, true, 1);
        let ice = String::from_utf8_lossy(&ice_bytes);
        assert!(ice.contains(";5;"), "iCE bright bg must set the blink bit: {ice:?}");
        assert!(ice.contains("44"), "bright blue bg encodes as base blue 44: {ice:?}");
        assert!(!ice.contains("104"), "must NOT use aixterm 104: {ice:?}");
        // Without iCE, a bright bg has nowhere to go on a real screen → clamp to base, no blink.
        let noice_bytes = ansi_grid_to_ans(&grid, false, 1);
        let noice = String::from_utf8_lossy(&noice_bytes);
        assert!(!noice.contains(";5;"), "no blink bit without iCE: {noice:?}");
    }

    #[test]
    fn letterbox_preserves_aspect_left_justified() {
        // A 4×4 opaque square fit into an 8×4 canvas → a 4×4 content box anchored top-LEFT
        // (cols 0–3 opaque), with the right margin (cols 4–7) fully transparent.
        let src = vec![255u8; 4 * 4 * 4]; // opaque (all 255 incl alpha)
        let out = letterbox(&src, 4, 4, 8, 4);
        assert_eq!(out.len(), 8 * 4 * 4);
        assert_eq!(out[(0 * 8 + 0) * 4 + 3], 255, "col 0 (art origin) opaque");
        assert_eq!(out[(0 * 8 + 3) * 4 + 3], 255, "col 3 (art edge) opaque");
        assert_eq!(out[(0 * 8 + 4) * 4 + 3], 0, "col 4 (right margin) transparent");
        assert_eq!(out[(0 * 8 + 7) * 4 + 3], 0, "col 7 (right margin) transparent");
    }

    #[test]
    fn small_sprite_kept_at_source_resolution() {
        // A sprite that fits is stored 1:1 (no detail-destroying scaling); the GPU
        // NEAREST-samples it up to the tile size at display time.
        let img = PixImage::from_rgba(4, 4, vec![[1, 2, 3, 255]; 16]);
        let (w, h, buf) = make_thumb(&img, 512);
        assert_eq!((w, h), (4, 4));
        assert_eq!(buf.len(), 4 * 4 * 4);
    }

    #[test]
    fn tall_thin_sprite_keeps_all_columns() {
        // The reported bug: a 15×392 sprite must keep its 15 columns, not squash
        // to ~10 wide to fit a 256 box.
        let img = PixImage::from_rgba(15, 392, vec![[9, 9, 9, 255]; 15 * 392]);
        let (w, h, _) = make_thumb(&img, 512);
        assert_eq!((w, h), (15, 392));
    }

    #[test]
    fn downscales_preserving_aspect() {
        let img = PixImage::from_rgba(32, 16, vec![[0, 0, 0, 255]; 32 * 16]);
        let (w, h, _) = make_thumb(&img, 8);
        assert_eq!((w, h), (8, 4));
    }

    #[test]
    fn extract_palette_collects_distinct_rgba_colors() {
        let pixels = vec![
            [0, 0, 0, 255],
            [255, 0, 0, 255],
            [0, 0, 0, 255],
            [0, 255, 0, 255],
        ];
        let img = PixImage::from_rgba(2, 2, pixels);
        let pal = extract_palette(&img).expect("≤256 colors → Some");
        assert_eq!(pal.len(), 3); // 3 distinct, sorted
        assert_eq!(pal[0], [0, 0, 0, 255]);
    }

    #[test]
    fn only_fully_opaque_pixels_count_toward_colors() {
        // Only alpha==255 pixels contribute: fully-transparent (invisible noise)
        // and semi-transparent (anti-aliased edge) pixels are both excluded.
        let pixels = vec![
            [255, 0, 0, 255],  // opaque red   -> counts
            [0, 255, 0, 255],  // opaque green -> counts
            [10, 20, 30, 0],   // transparent  -> skip
            [40, 50, 60, 128], // AA edge       -> skip
            [70, 80, 90, 254], // not quite opaque -> skip
        ];
        let img = PixImage::from_rgba(5, 1, pixels);
        assert_eq!(count_colors(&img), Some(2));
        assert_eq!(extract_palette(&img).map(|p| p.len()), Some(2));
    }

    #[test]
    fn extract_palette_uses_indexed_palette_verbatim() {
        // Indexed art keeps its authoritative palette (order + unused slots), not
        // just the distinct colors actually drawn.
        let palette = vec![[1, 1, 1, 255], [2, 2, 2, 255], [9, 9, 9, 255]];
        let img = PixImage::from_indexed(2, 1, vec![0, 1], palette.clone());
        assert_eq!(extract_palette(&img), Some(palette));
    }

    #[test]
    fn extract_palette_keeps_several_hundred_colors() {
        // Shaded pixel art with a few hundred distinct colors (e.g. a 707-color
        // sprite) still gets a dynamic palette — it's under SWATCH_CAP.
        let pixels: Vec<[u8; 4]> = (0..700u32)
            .map(|i| [(i % 256) as u8, (i / 256) as u8, (i / 4) as u8, 255])
            .collect();
        let img = PixImage::from_rgba(700, 1, pixels);
        assert_eq!(extract_palette(&img).map(|p| p.len()), Some(700));
    }

    #[test]
    fn extract_palette_none_when_too_many_colors() {
        // Above SWATCH_CAP distinct colors (photo-like) → no swatch palette.
        let n = (SWATCH_CAP + 200) as u32;
        let pixels: Vec<[u8; 4]> = (0..n)
            .map(|i| [(i % 256) as u8, (i / 256) as u8, (i * 7 % 256) as u8, 255])
            .collect();
        let img = PixImage::from_rgba(n, 1, pixels);
        assert_eq!(extract_palette(&img), None);
    }

    #[test]
    fn distinct_opaque_colors_dedupes_and_skips_transparent() {
        // 4 pixels: two identical red, one transparent (skipped), one blue.
        let rgba: Vec<u8> = vec![
            255, 0, 0, 255, // red
            255, 0, 0, 255, // red (dup)
            9, 9, 9, 0, // fully transparent — skipped
            0, 0, 255, 255, // blue
        ];
        let cols = distinct_opaque_colors(&rgba);
        assert_eq!(cols, vec![[0, 0, 255, 255], [255, 0, 0, 255]]);
    }

    #[test]
    fn distinct_opaque_colors_has_no_swatch_cap() {
        // Unlike `extract_palette`, this feeds median_cut and keeps ALL colors —
        // that's what lets a >SWATCH_CAP image be reduced. Reducing it works.
        let n = (SWATCH_CAP + 500) as u32;
        let mut rgba = Vec::with_capacity(n as usize * 4);
        for i in 0..n {
            rgba.extend_from_slice(&[(i % 256) as u8, (i / 256) as u8, (i * 7 % 256) as u8, 255]);
        }
        let cols = distinct_opaque_colors(&rgba);
        assert!(
            cols.len() > SWATCH_CAP,
            "no cap: {} colors kept",
            cols.len()
        );
        let reduced = median_cut(&cols, 16);
        assert!(!reduced.is_empty() && reduced.len() <= 16);
    }

    #[test]
    #[ignore = "decodes a real user sprite if present; run with --ignored"]
    fn real_shaded_sprite_gets_a_dynamic_palette() {
        // The reported case: a 32×48 RGBA sprite with 707 colors and NO indexed
        // palette must still produce a dynamic swatch palette (≤ SWATCH_CAP).
        let Ok(home) = std::env::var("HOME") else {
            return;
        };
        let p = std::path::Path::new(&home).join(
            "git/qb64pe-lab/greywood/sprites/ash_wolf/\
             ash_wolf_32x48_none_s1026343054_sprite_00001_.png",
        );
        if !p.exists() {
            return;
        }
        let reg = crate::decode::Registry::with_builtins();
        let img = reg.decode_path(&p).unwrap();
        assert!(img.indexed.is_none(), "this PNG is RGBA, not indexed");
        let pal = extract_palette(&img).expect("opaque colors ≤ SWATCH_CAP → Some palette");
        // 707 distinct RGBA total, but 92 live only in fully-transparent pixels
        // (invisible grey noise); the opaque palette is 615 colors.
        assert_eq!(pal.len(), 615);
        // The "reduce to N" feature: median-cut the 615 down to a workable palette.
        let reduced = median_cut(&pal, 16);
        assert!(
            !reduced.is_empty() && reduced.len() <= 16,
            "615 -> <=16 reps"
        );
    }

    #[test]
    fn parse_gpl_reads_colors_skipping_headers() {
        let gpl = "GIMP Palette\nName: Test\nColumns: 8\n# a comment\n\
                   \x20\x20 0   0   0\tBLACK\n170   0   0\tRED\n255 255 255\tWHITE\n";
        let pal = parse_gpl(gpl);
        assert_eq!(pal.len(), 3);
        assert_eq!(pal[0], [0, 0, 0, 255]);
        assert_eq!(pal[1], [170, 0, 0, 255]);
        assert_eq!(pal[2], [255, 255, 255, 255]);
    }

    #[test]
    fn median_cut_reduces_to_target_clusters() {
        // Four dark + four light colors, reduced to 2 → one rep per cluster.
        let colors = vec![
            [0, 0, 0, 255],
            [10, 10, 10, 255],
            [20, 20, 20, 255],
            [30, 30, 30, 255],
            [200, 200, 200, 255],
            [210, 210, 210, 255],
            [220, 220, 220, 255],
            [230, 230, 230, 255],
        ];
        let out = median_cut(&colors, 2);
        assert_eq!(out.len(), 2);
        assert!(out[0][0] < 50, "a dark representative");
        assert!(out[1][0] > 180, "a light representative");
        // Alpha stays opaque.
        assert!(out.iter().all(|c| c[3] == 255));
    }

    #[test]
    fn dither_then_snap_only_emits_palette_colors() {
        // Whatever the method, the *final* pixels (after the palette snap that the
        // Palette op performs) must be palette colors only. Ordered methods bias
        // then rely on the snap; diffusion snaps during the dither pass itself.
        let palette = [[0, 0, 0, 255], [255, 255, 255, 255]];
        let custom = bayer_values(4);
        for method in [1u8, 2, 3, 4, 5, DITHER_CUSTOM] {
            // a flat mid-grey field that forces dithering between black and white
            let mut rgba: Vec<u8> = (0..64).flat_map(|_| [128, 128, 128, 255]).collect();
            dither_pass(
                &mut rgba,
                8,
                8,
                method,
                1.0,
                &custom,
                4,
                1,
                1,
                Some(&palette),
            );
            // The Palette op always runs after the Dither op in the pipeline.
            remap_to_palette(&mut rgba, &palette);
            let blacks = rgba.chunks_exact(4).filter(|p| p[0] == 0).count();
            for px in rgba.chunks_exact(4) {
                assert!(
                    px == [0, 0, 0, 255] || px == [255, 255, 255, 255],
                    "method {method} produced a non-palette color {px:?}"
                );
            }
            // A flat grey must actually break into a mix of both colors.
            assert!(
                blacks > 0 && blacks < 64,
                "method {method} did not dither (got {blacks}/64 black)"
            );
        }
    }

    #[test]
    fn ordered_dither_is_pure_bias_without_palette() {
        // Ordered/custom dither with no palette must NOT snap — it only nudges
        // values, leaving them off-palette so a later op can quantize them.
        let mut rgba: Vec<u8> = (0..16).flat_map(|_| [128u8, 128, 128, 255]).collect();
        dither_pass(&mut rgba, 4, 4, 2, 1.0, &[], 0, 1, 1, None);
        // The flat grey is now a checker of nudged values, none forced to 0/255.
        let distinct: std::collections::HashSet<[u8; 3]> =
            rgba.chunks_exact(4).map(|p| [p[0], p[1], p[2]]).collect();
        assert!(distinct.len() > 1, "ordered bias should perturb the field");
        assert!(
            rgba.chunks_exact(4).all(|p| p[0] > 0 && p[0] < 255),
            "ordered bias must not snap to palette endpoints"
        );
    }

    #[test]
    fn ordered_dither_scale_enlarges_cells() {
        // `scale` makes each Bayer cell span scale×scale pixels: within a cell the
        // bias is identical, so a flat field comes out in solid blocks. This is what
        // "zooms" the dither so 1-px noise becomes a readable crosshatch on hi-res art.
        let flat = || -> Vec<u8> { (0..16).flat_map(|_| [128u8, 128, 128, 255]).collect() }; // 4×4
        let at = |b: &[u8], x: usize, y: usize| b[(y * 4 + x) * 4];

        let mut s1 = flat();
        dither_pass(&mut s1, 4, 4, 2, 1.0, &[], 0, 1, 1, None); // Bayer 4×4, scale 1×1
                                                                // Neighbouring pixels use different Bayer entries → they differ.
        assert_ne!(at(&s1, 0, 0), at(&s1, 1, 0));

        let mut s2 = flat();
        dither_pass(&mut s2, 4, 4, 2, 1.0, &[], 0, 2, 2, None); // scale 2 → 2×2 cells
        assert_eq!(at(&s2, 0, 0), at(&s2, 1, 0), "same 2×2 cell → equal");
        assert_eq!(at(&s2, 0, 0), at(&s2, 0, 1), "same 2×2 cell → equal");
        assert_eq!(at(&s2, 0, 0), at(&s2, 1, 1), "same 2×2 cell → equal");
        assert_ne!(at(&s2, 0, 0), at(&s2, 2, 0), "next cell differs");
    }

    #[test]
    fn detect_pixel_scale_finds_the_upscale_factor() {
        // A 4×-upscaled 4×4 checkerboard: each cell is a solid 4×4 px block, so every
        // colour edge sits on a period-4 grid → detected scale is 4.
        let n = 16;
        let mut up = vec![0u8; n * n * 4];
        for y in 0..n {
            for x in 0..n {
                let v = if ((x / 4) + (y / 4)) % 2 == 0 { 255 } else { 0 };
                let i = (y * n + x) * 4;
                up[i] = v;
                up[i + 1] = v;
                up[i + 2] = v;
                up[i + 3] = 255;
            }
        }
        assert_eq!(detect_pixel_scale(&up, n, n), (4, 4));

        // A 1-px checkerboard has a period-1 grid (native detail) → stays 1.
        let mut fine = vec![0u8; n * n * 4];
        for y in 0..n {
            for x in 0..n {
                let v = if (x + y) % 2 == 0 { 255 } else { 0 };
                let i = (y * n + x) * 4;
                fine[i] = v;
                fine[i + 1] = v;
                fine[i + 2] = v;
                fine[i + 3] = 255;
            }
        }
        assert_eq!(detect_pixel_scale(&fine, n, n), (1, 1));

        // A flat field has no edges → 1.
        let flat = vec![128u8; n * n * 4];
        assert_eq!(detect_pixel_scale(&flat, n, n), (1, 1));

        // Non-square: cells 2px wide × 4px tall → detected (2, 4).
        let (cw, ch) = (2usize, 4usize);
        let (iw, ih) = (24usize, 24usize);
        let mut ns = vec![0u8; iw * ih * 4];
        for yy in 0..ih {
            for xx in 0..iw {
                let v = if ((xx / cw) + (yy / ch)) % 2 == 0 {
                    255
                } else {
                    0
                };
                let i = (yy * iw + xx) * 4;
                ns[i] = v;
                ns[i + 1] = v;
                ns[i + 2] = v;
                ns[i + 3] = 255;
            }
        }
        assert_eq!(detect_pixel_scale(&ns, iw, ih), (2, 4));
    }

    #[test]
    fn remap_snaps_pixels_to_nearest_palette_color() {
        let palette = [[0, 0, 0, 255], [255, 255, 255, 255]];
        let mut rgba = vec![
            10, 10, 10, 255, // dark -> black
            240, 240, 240, 255, // light -> white
            99, 99, 99, 0, // transparent -> untouched
        ];
        remap_to_palette(&mut rgba, &palette);
        assert_eq!(&rgba[0..4], &[0, 0, 0, 255]);
        assert_eq!(&rgba[4..8], &[255, 255, 255, 255]);
        assert_eq!(&rgba[8..12], &[99, 99, 99, 0]);
    }

    #[test]
    fn median_cut_passthrough_when_already_small() {
        let colors = vec![[1, 2, 3, 255], [4, 5, 6, 255]];
        assert_eq!(median_cut(&colors, 16), colors);
        // target of 1 collapses everything to a single averaged color.
        assert_eq!(median_cut(&colors, 1).len(), 1);
    }

    #[test]
    fn counts_distinct_colors() {
        let pixels = vec![
            [0, 0, 0, 255],
            [255, 0, 0, 255],
            [0, 0, 0, 255],
            [0, 255, 0, 255],
        ];
        let img = PixImage::from_rgba(2, 2, pixels);
        assert_eq!(count_colors(&img), Some(3));
    }
}
