//! Thumbnail generation + a small background worker pool.
//!
//! Decoding and scaling happen off the UI thread; only the cheap CPU pixel
//! buffer crosses back, and the UI thread uploads it to a GPU texture lazily.
//! Scaling is split by direction: small sprites are kept at source res and the GPU
//! NEAREST-samples them up crisply, while big images are **area-averaged** down
//! (a box filter) so high-frequency block/shade art shrinks faithfully (a 50% dither
//! reads as 50% grey) instead of aliasing — those downscaled thumbs display LINEAR.

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
];

/// `DITHER_NAMES` index for the user-editable custom matrix.
pub const DITHER_CUSTOM: u8 = 6;

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
