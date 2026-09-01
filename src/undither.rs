//! Undithering — reverse ordered / Floyd–Steinberg dithering back to smooth tone.
//!
//! Dithering fakes an intermediate colour a small palette can't hold by *alternating* two palette
//! colours in a fine pattern (a 50 % grey = a checker of black and white). Undithering is the
//! inverse: detect those flat dithered fields and **average them back to the single true tone** the
//! pattern was standing in for — while leaving real edges crisp so the image doesn't turn to mush.
//!
//! This is a hand-rolled take on the idea behind Kornelski's `undither` crate (which hard-codes its
//! Prewitt thresholds and exposes no knobs). Here every parameter is a slider:
//!
//! 1. **Edge map** — a Sobel gradient over luminance. A pixel whose gradient is `≥ edge_threshold`
//!    is an *edge*: it's excluded from averaging AND excluded as a neighbour, so a dithered field
//!    right up against a hard edge is smoothed without bleeding the edge into it. (Dither texture
//!    has a *high local* gradient too, but it's high-frequency and cancels over a window, whereas a
//!    real edge is a sustained ridge — the window average is what separates them.)
//! 2. **Windowed average** — each non-edge pixel becomes the mean of the non-edge pixels in its
//!    `radius` box. A checker of two palette colours averages to their midpoint = the intended tone.
//! 3. **Blend** — `out = orig*(1−strength) + averaged*strength`, so the effect is dialable.
//! 4. **Optional palette snap** — with `snap` on (and a source palette), the smoothed region is
//!    re-quantised to the *nearest single* palette entry, i.e. "this dithered field was meant to be
//!    one solid colour" (flattens 50 %-dithered fills to a flat block). Off = keep the smooth,
//!    off-palette intermediate tone (true de-dither). Default off.
//!
//! Pure + headless, same-size RGBA in/out — drops into the recolor pipeline's `scale_source` step
//! next to [`crate::jpeg_clean::cleanup`] and [`crate::deblock::deblock`].

/// Options for [`undither`]. Same-size RGBA in/out; alpha is passed through and transparent pixels
/// (`a < 8`) are ignored by the averaging (so a keyed background can't wash into the sprite).
#[derive(Clone, Debug, PartialEq)]
pub struct UnditherOpts {
    /// Sobel luminance-gradient (0..~1020) at/above which a pixel is a protected edge, excluded from
    /// averaging. Lower = protect more detail (undither only very flat fields); higher = smooth more.
    pub edge_threshold: u32,
    /// Averaging box radius in px (window = `(2r+1)²`). 1–2 suits fine ordered dither; larger
    /// windows recover coarser patterns but soften more.
    pub radius: usize,
    /// Blend of the undithered result over the original, 0..1. 0 = off, 1 = full.
    pub strength: f32,
    /// After smoothing, snap each pixel to the nearest entry of the source palette (flatten a
    /// dithered field to one solid palette colour) instead of keeping the smooth tone.
    pub snap: bool,
}

impl Default for UnditherOpts {
    fn default() -> Self {
        Self {
            edge_threshold: 220,
            radius: 1,
            strength: 1.0,
            snap: false,
        }
    }
}

/// Undither `rgba` (same-size RGBA out). `palette` (the source's indexed palette, if any) is only
/// used when `o.snap` is set. A short/empty buffer, `strength == 0`, or `radius == 0` returns the
/// input unchanged.
pub fn undither(
    rgba: &[u8],
    w: usize,
    h: usize,
    palette: Option<&[[u8; 4]]>,
    o: &UnditherOpts,
) -> Vec<u8> {
    let n = w * h;
    if n == 0 || rgba.len() < n * 4 || o.strength <= 0.0 || o.radius == 0 {
        return rgba.to_vec();
    }
    // 1. Luminance + a Sobel edge mask. Perceptual-ish integer luma (2·G + R + B ≈ ×4).
    let lum: Vec<i32> = (0..n)
        .map(|i| {
            let p = &rgba[i * 4..i * 4 + 4];
            (p[0] as i32 + 2 * p[1] as i32 + p[2] as i32) / 4
        })
        .collect();
    let thr = o.edge_threshold as i32;
    let mut is_edge = vec![false; n];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            // Clamp-at-border sampling for the 3×3 Sobel.
            let l = |dx: isize, dy: isize| -> i32 {
                let sx = (x as isize + dx).clamp(0, w as isize - 1) as usize;
                let sy = (y as isize + dy).clamp(0, h as isize - 1) as usize;
                lum[sy * w + sx]
            };
            let gx = (l(1, -1) + 2 * l(1, 0) + l(1, 1)) - (l(-1, -1) + 2 * l(-1, 0) + l(-1, 1));
            let gy = (l(-1, 1) + 2 * l(0, 1) + l(1, 1)) - (l(-1, -1) + 2 * l(0, -1) + l(1, -1));
            is_edge[i] = gx.abs() + gy.abs() >= thr;
        }
    }

    // 2. Windowed average over non-edge, opaque neighbours.
    let r = o.radius as isize;
    let strength = o.strength.clamp(0.0, 1.0);
    let mut out = rgba.to_vec();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if is_edge[i] || rgba[i * 4 + 3] < 8 {
                continue; // edges + transparent pixels keep their original value
            }
            let (mut sr, mut sg, mut sb, mut cnt) = (0u32, 0u32, 0u32, 0u32);
            for dy in -r..=r {
                let sy = y as isize + dy;
                if sy < 0 || sy >= h as isize {
                    continue;
                }
                for dx in -r..=r {
                    let sx = x as isize + dx;
                    if sx < 0 || sx >= w as isize {
                        continue;
                    }
                    let j = sy as usize * w + sx as usize;
                    if is_edge[j] || rgba[j * 4 + 3] < 8 {
                        continue;
                    }
                    sr += rgba[j * 4] as u32;
                    sg += rgba[j * 4 + 1] as u32;
                    sb += rgba[j * 4 + 2] as u32;
                    cnt += 1;
                }
            }
            if cnt == 0 {
                continue;
            }
            let avg = [(sr / cnt) as f32, (sg / cnt) as f32, (sb / cnt) as f32];
            for c in 0..3 {
                let v = rgba[i * 4 + c] as f32 * (1.0 - strength) + avg[c] * strength;
                out[i * 4 + c] = v.round().clamp(0.0, 255.0) as u8;
            }
        }
    }

    // 3. Optional: snap the smoothed pixels back to the nearest single palette colour.
    if o.snap {
        if let Some(pal) = palette {
            if !pal.is_empty() {
                for i in 0..n {
                    if out[i * 4 + 3] < 8 {
                        continue;
                    }
                    let c = [out[i * 4], out[i * 4 + 1], out[i * 4 + 2]];
                    let best = nearest(&c, pal);
                    out[i * 4] = best[0];
                    out[i * 4 + 1] = best[1];
                    out[i * 4 + 2] = best[2];
                }
            }
        }
    }
    out
}

/// Nearest palette entry to `c` by squared RGB distance (alpha ignored).
fn nearest(c: &[u8; 3], pal: &[[u8; 4]]) -> [u8; 3] {
    let mut best = [c[0], c[1], c[2]];
    let mut bd = i64::MAX;
    for e in pal {
        let dr = c[0] as i64 - e[0] as i64;
        let dg = c[1] as i64 - e[1] as i64;
        let db = c[2] as i64 - e[2] as i64;
        let d = dr * dr + dg * dg + db * db;
        if d < bd {
            bd = d;
            best = [e[0], e[1], e[2]];
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> UnditherOpts {
        UnditherOpts {
            edge_threshold: 1020, // no pixel is an edge → pure averaging (isolate the smoothing)
            radius: 1,
            strength: 1.0,
            snap: false,
        }
    }

    /// A checkerboard of black/white (a 50 % ordered dither) undithers to ~mid grey.
    #[test]
    fn checker_averages_to_mid_grey() {
        let (w, h) = (8, 8);
        let mut px = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let v = if (x + y) % 2 == 0 { 0 } else { 255 };
                let i = (y * w + x) * 4;
                px[i] = v;
                px[i + 1] = v;
                px[i + 2] = v;
                px[i + 3] = 255;
            }
        }
        let out = undither(&px, w, h, None, &opts());
        // An interior pixel (full 3×3 window) should land near 127.
        let i = (3 * w + 3) * 4;
        let g = out[i] as i32;
        assert!((g - 127).abs() <= 40, "checker → grey, got {g}");
    }

    /// A hard edge between two flat fields is preserved (not blurred across the boundary).
    #[test]
    fn preserves_a_hard_edge() {
        let (w, h) = (8, 8);
        let mut px = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let v = if x < 4 { 30 } else { 220 };
                let i = (y * w + x) * 4;
                px[i] = v;
                px[i + 1] = v;
                px[i + 2] = v;
                px[i + 3] = 255;
            }
        }
        // A realistic edge threshold: the flat interiors have zero gradient, the boundary a big one.
        let o = UnditherOpts {
            edge_threshold: 200,
            ..opts()
        };
        let out = undither(&px, w, h, None, &o);
        // Interior columns stay their flat value (window is all same-value non-edge pixels).
        assert_eq!(out[(2 * w + 1) * 4], 30, "left field flat");
        assert_eq!(out[(2 * w + 6) * 4], 220, "right field flat");
    }

    /// With `snap` on, the smoothed tone is re-quantised to the nearest palette entry.
    #[test]
    fn snap_quantises_to_palette() {
        let (w, h) = (8, 8);
        let mut px = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let v = if (x + y) % 2 == 0 { 0 } else { 255 };
                let i = (y * w + x) * 4;
                px[i] = v;
                px[i + 1] = v;
                px[i + 2] = v;
                px[i + 3] = 255;
            }
        }
        let pal = [[0u8, 0, 0, 255], [128, 128, 128, 255], [255, 255, 255, 255]];
        let o = UnditherOpts {
            snap: true,
            ..opts()
        };
        let out = undither(&px, w, h, Some(&pal), &o);
        // The mid-grey average snaps to the [128,128,128] entry for interior pixels.
        let i = (3 * w + 3) * 4;
        assert_eq!(&out[i..i + 3], &[128, 128, 128]);
    }

    #[test]
    fn off_or_short_buffer_is_unchanged() {
        let (w, h) = (4, 4);
        let px = vec![9u8; w * h * 4];
        let off = UnditherOpts {
            strength: 0.0,
            ..opts()
        };
        assert_eq!(undither(&px, w, h, None, &off), px);
        assert!(undither(&[], 0, 0, None, &opts()).is_empty());
    }
}
