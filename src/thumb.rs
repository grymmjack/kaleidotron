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
];

/// `DITHER_NAMES` index for the user-editable custom matrix.
pub const DITHER_CUSTOM: u8 = 6;

/// `DITHER_NAMES` index for the textmode/ANSI shade-block renderer. Unlike the
/// other modes this one paints CP437 glyphs (space ░▒▓█ + half-blocks) drawn in a
/// two-colour palette per cell — a hard-quantized, blocky "ANSI art" look. Needs a
/// palette (like error-diffusion), and it already outputs palette colours so a
/// following Palette snap is a no-op.
pub const DITHER_ANSI: u8 = 7;

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
    shade_amount: f32,
    ice: bool,
    smooth: f32,
) -> AnsiGrid {
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
    // (1.0); the ░▒▓ mid levels only when their per-shade toggle is on.
    let mut coverages: Vec<(f32, u8)> = Vec::with_capacity(5);
    coverages.push((0.0, SHADE_GLYPHS[0])); // space
    if f1_on {
        coverages.push((f1.clamp(0.0, 1.0), SHADE_GLYPHS[1])); // ░
    }
    if f2_on {
        coverages.push((f2.clamp(0.0, 1.0), SHADE_GLYPHS[2])); // ▒
    }
    if f3_on {
        coverages.push((f3.clamp(0.0, 1.0), SHADE_GLYPHS[3])); // ▓
    }
    coverages.push((1.0, SHADE_GLYPHS[4])); // █
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
    let threshold = (1.0 - shade_amount.clamp(0.0, 1.0)) * max_threshold;
    // "Smoothness": a contrast penalty on the mid-shade glyphs (░▒▓) so the greedy
    // pair search doesn't dither wildly-different colours (white▒black, yellow▒purple)
    // that AVERAGE to the target but look garish. At smooth=1.0 a full-contrast pair
    // (dist²≈195075) pays that whole cost, so it's only picked on a near-perfect match;
    // at smooth=0.0 the penalty vanishes (the old greedy behaviour). Solids and the
    // half-blocks are exempt — see the loop below.
    let smooth_w = smooth.clamp(0.0, 1.0);
    // Perf: a cell this small can't show a visible shade pattern, so it always
    // renders as a flat colour — a constant of the effective cell size, hoisted out.
    let tiny_cell = cw < 3 || ch_ < 3;
    // Candidate fg/bg set for the pair search. Small palettes (EGA is 16) search
    // ALL indices — no per-cell sort, no allocation. Big palettes fall back to the
    // ~6 nearest, refilling a SINGLE scratch buffer each cell (never a fresh Vec).
    let small_pal = pf.len() <= 32;
    let all_idx: Vec<usize> = (0..pf.len()).collect();
    let mut cand_buf: Vec<usize> = Vec::with_capacity(pf.len());
    // Which palette entries are legal as a BACKGROUND? Standard textmode has only 8
    // backgrounds (the non-bright ANSI slots 0–7); iCE-color mode unlocks all 16. So
    // a palette colour that maps to a bright ANSI slot (≥8) can't be a bg unless iCE —
    // gate the search on this so the on-screen preview matches a real non-iCE .ans.
    let bg_ok: Vec<bool> = pf
        .iter()
        .map(|c| ice || nearest_ansi16([c[0] as u8, c[1] as u8, c[2] as u8]) < 8)
        .collect();
    for cy in 0..rows {
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
                cells.push(AnsiCell { fg: 0, bg: 0, ch: 32 }); // wholly transparent
                continue;
            }
            let avg = [sum[0] / n as f32, sum[1] / n as f32, sum[2] / n as f32];
            let solid_idx = nearest_index([avg[0] as u8, avg[1] as u8, avg[2] as u8], palette);
            // Tiny-cell short-circuit: a <3px cell can't render a shade pattern, so
            // it's always a flat colour — skip the whole search (makes 1×1 instant).
            if tiny_cell {
                cells.push(AnsiCell { fg: solid_idx, bg: solid_idx, ch: 219 });
                continue;
            }
            // Shading-amount gate: if `avg` is already close to a solid palette colour
            // (within `threshold`), keep the cell a flat █ and skip the shade search —
            // so large flat regions stay solid instead of being needlessly dithered.
            let solid_err = dist2(avg, pf[solid_idx as usize]);
            if solid_err <= threshold {
                cells.push(AnsiCell { fg: solid_idx, bg: solid_idx, ch: 219 });
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
                    for &(cov, glyph) in &coverages {
                        let pred = [
                            pf[fg][0] * cov + pf[bg][0] * (1.0 - cov),
                            pf[fg][1] * cov + pf[bg][1] * (1.0 - cov),
                            pf[fg][2] * cov + pf[bg][2] * (1.0 - cov),
                        ];
                        // Mid-shade glyphs (░▒▓, 0 < cov < 1) mix BOTH colours, so add a
                        // contrast penalty to avoid garish complementary dithers. Solids
                        // (space cov 0 → only bg; █ cov 1 → only fg) show one colour → no
                        // penalty; they always win a flat/near-flat cell.
                        let err_eff = if cov != 0.0 && cov != 1.0 {
                            dist2(avg, pred) + smooth_w * dist2(pf[fg], pf[bg])
                        } else {
                            dist2(avg, pred)
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

            // --- HALF-BLOCK candidates (▀ upper-half, ▌ left-half). The bg half must
            // also land in a legal background slot (non-iCE), else skip the candidate. ---
            if half_blocks {
                // ▀ 223: top = fg, bottom = bg.
                let fg = nearest_index([top_avg[0] as u8, top_avg[1] as u8, top_avg[2] as u8], palette);
                let bg = nearest_index([bot_avg[0] as u8, bot_avg[1] as u8, bot_avg[2] as u8], palette);
                let err = 0.5 * dist2(top_avg, pf[fg as usize]) + 0.5 * dist2(bot_avg, pf[bg as usize]);
                if bg_ok[bg as usize] && err < best_err {
                    best_err = err;
                    best_fg = fg;
                    best_bg = bg;
                    best_ch = 223;
                }
                // ▌ 221: left = fg, right = bg.
                let fg = nearest_index([left_avg[0] as u8, left_avg[1] as u8, left_avg[2] as u8], palette);
                let bg = nearest_index([right_avg[0] as u8, right_avg[1] as u8, right_avg[2] as u8], palette);
                let err = 0.5 * dist2(left_avg, pf[fg as usize]) + 0.5 * dist2(right_avg, pf[bg as usize]);
                if bg_ok[bg as usize] && err < best_err {
                    best_fg = fg;
                    best_bg = bg;
                    best_ch = 221;
                }
            }
            cells.push(AnsiCell { fg: best_fg, bg: best_bg, ch: best_ch });
        }
    }
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
fn ansi_render_grid(grid: &AnsiGrid, rgba: &mut [u8], w: usize, h: usize, font_8x8: bool) {
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
    shade_amount: f32,
    font_8x8: bool,
    ice: bool,
    smooth: f32,
) {
    if palette.is_empty() || w == 0 || h == 0 {
        return;
    }
    let grid = ansi_shade_grid(
        rgba, w, h, palette, cell_w, cell_h, f1, f2, f3, half_blocks, f1_on, f2_on, f3_on,
        shade_amount, ice, smooth,
    );
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
    for r in 0..6 {
        for g in 0..6 {
            for b in 0..6 {
                let d = d2([XTERM_CUBE[r], XTERM_CUBE[g], XTERM_CUBE[b]]);
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
                        // 16-colour: nearest ANSI, with the classic bold/aixterm rules.
                        let f = nearest_ansi16(fg);
                        let b = nearest_ansi16(bg);
                        if f >= 8 {
                            sgr.push_str(";1"); // bold → bright fg
                        }
                        sgr.push_str(&format!(";{}", 30 + (f % 8)));
                        if b >= 8 && ice {
                            sgr.push_str(&format!(";{}", 100 + (b % 8))); // aixterm bright bg
                        } else {
                            sgr.push_str(&format!(";{}", 40 + (b % 8))); // clamp to 0-7
                        }
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

/// Serialize an [`AnsiGrid`] to an **XBIN** file (embeds the exact palette + font, so
/// non-EGA colours survive). Header `XBIN\x1A`, `width`/`height` (u16 LE cells),
/// `fontsize` (8/16), `flags`; then a 16-colour 6-bit-DAC palette block, the CP437
/// font bitmap, and `width*height` `(char, attribute)` cell pairs where
/// `attribute = bg<<4 | fg` (palette indices mapped into 0..15). No SAUCE trailer —
/// the caller appends that (shared with the `.ans` path).
pub fn ansi_grid_to_xbin(grid: &AnsiGrid, font_8x8: bool, ice: bool) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(b"XBIN");
    out.push(0x1A); // EOF marker inside the header
    out.extend_from_slice(&(grid.cols as u16).to_le_bytes());
    out.extend_from_slice(&(grid.rows as u16).to_le_bytes());
    let fontsize: u8 = if font_8x8 { 8 } else { 16 };
    out.push(fontsize);
    // flags: bit0 palette present, bit1 font present, bit3 non-blink / iCE.
    let mut flags = 0b0000_0011u8;
    if ice {
        flags |= 0b0000_1000;
    }
    out.push(flags);
    // Palette block: 16 colours × RGB, each channel down to the 6-bit VGA DAC (v>>2).
    for i in 0..16 {
        let c = grid.palette.get(i).copied().unwrap_or([0, 0, 0, 255]);
        out.push(c[0] >> 2);
        out.push(c[1] >> 2);
        out.push(c[2] >> 2);
    }
    // Font block: `fontsize` bytes per glyph × 256 glyphs (MSB-left, row-major).
    for ch in 0..256usize {
        if font_8x8 {
            out.extend_from_slice(&CP437_8X8[ch]);
        } else {
            out.extend_from_slice(&CP437_8X16[ch]);
        }
    }
    // Image data: (char, attribute) per cell. XBin attributes are 16-colour, so map
    // any index ≥16 (palette larger than 16) to the nearest of the first 16 entries.
    let first16 = &grid.palette[..grid.palette.len().min(16)];
    let map16 = |idx: u8| -> u8 {
        let i = idx as usize;
        if i < 16 {
            idx & 0x0F
        } else if !first16.is_empty() {
            let c = grid.palette[i.min(grid.palette.len().saturating_sub(1))];
            nearest_index([c[0], c[1], c[2]], first16) & 0x0F
        } else {
            0
        }
    };
    for cell in &grid.cells {
        out.push(cell.ch);
        let fg = map16(cell.fg);
        let bg = map16(cell.bg);
        out.push((bg << 4) | fg);
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;
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
