//! JPEG artifact removal — the *continuous-tone* kind (the complement of `jpeg_clean.rs`).
//!
//! Where `jpeg_clean.rs` **re-quantises** a lossy image toward pixel-art (palette snap + background
//! key + grid snap — great for sprite sheets, destructive on a photo), this module **preserves the
//! image as continuous-tone** and removes the two artifacts JPEG's 8×8 DCT + quantisation leave
//! behind:
//!
//! - **Blocking** — the DCT works on independent 8×8 blocks, so heavy quantisation zeroes the
//!   high-frequency coefficients and the blocks lose their smooth transitions, leaving visible
//!   step-seams on the 8-pixel grid. [`deblock_seams`] finds those seams and smooths **only** the
//!   ones that look like artifacts (a small step flanked by flatter interiors), preserving a genuine
//!   edge that happens to land on the grid — the "conditional thresholding" of the classic spatial
//!   de-blocking filter (detect the 8×8 grid → compare the boundary gradient to the interior
//!   gradient → low-pass only the artifact seams).
//! - **Ringing / mosquito noise** — a sharp edge needs high frequencies to encode; quantising them
//!   away makes the reconstructed edge oscillate (Gibbs). [`diffuse`] runs **edge-preserving
//!   anisotropic diffusion** (Perona–Malik) to melt those ripples in flat areas while leaving true
//!   edges intact.
//!
//! ## Why diffusion and not literal ROF Total-Variation
//! Total-Variation denoising minimises `½∫(u−f)² + λ∫|∇u|`. Solved by *explicit* gradient descent
//! it needs a tiny time-step to stay stable, and a live preview slider that occasionally diverges
//! into noise is worse than useless. **Perona–Malik anisotropic diffusion is the same PDE/TV
//! denoising family** — it evolves `∂u/∂t = div( c(|∇u|)·∇u )` with a conduction coefficient
//! `c` that → 0 across strong edges — but is *unconditionally* well-behaved for `dt ≤ 0.25` with the
//! 4-neighbour stencil. Same goal (kill blocking + ringing, keep edges), robust at any slider value.
//!
//! Pure + headless (no egui, no I/O) so it's unit-testable, and same-size in/out so it drops into
//! the recolor pipeline's `scale_source` step exactly like [`crate::jpeg_clean::cleanup`].

/// Options for [`deblock`]. All stages are individually gated (a zero amount is a no-op), so the UI
/// can expose each as its own toggle+slider. Same-size RGBA in/out; alpha is passed through.
#[derive(Clone, Debug, PartialEq)]
pub struct DeblockOpts {
    /// The DCT block grid to de-seam (JPEG is always 8). Exposed so an image saved at a different
    /// macroblock size, or a 2×-scaled JPEG (16-px seams), can still be cleaned.
    pub block_size: usize,
    /// De-blocking strength, 0..1. 0 = off. Blends each smoothed seam pixel toward its low-passed
    /// value; 1 = full replacement.
    pub strength: f32,
    /// A boundary whose step (per-channel, 0..255) is **at or above** this is treated as a real edge
    /// and left alone; below it (and dominant over the block interiors) it's smoothed as an artifact.
    /// Also seeds the diffusion conduction `K`.
    pub edge_threshold: u32,
    /// Edge-preserving diffusion (ringing / mosquito-noise removal) strength, 0..1. 0 = off. Blends
    /// the diffused result over the original.
    pub tv_amount: f32,
    /// Diffusion iterations. More = smoother flats (and softer, but edges are preserved by `c`).
    pub tv_iters: usize,
}

impl Default for DeblockOpts {
    fn default() -> Self {
        Self {
            block_size: 8,
            strength: 0.6,
            edge_threshold: 24,
            tv_amount: 0.0,
            tv_iters: 4,
        }
    }
}

/// Remove JPEG artifacts (blocking + ringing) while keeping the image continuous-tone. Returns a new
/// RGBA buffer the same size as the input. A short/empty buffer is returned unchanged.
pub fn deblock(rgba: &[u8], w: usize, h: usize, o: &DeblockOpts) -> Vec<u8> {
    let n = w * h;
    if n == 0 || rgba.len() < n * 4 {
        return rgba.to_vec();
    }
    let mut out = rgba.to_vec();
    if o.strength > 0.0 && o.block_size >= 2 {
        deblock_seams(
            &mut out,
            w,
            h,
            o.block_size,
            o.strength.clamp(0.0, 1.0),
            o.edge_threshold,
        );
    }
    if o.tv_amount > 0.0 && o.tv_iters > 0 {
        diffuse(
            &mut out,
            w,
            h,
            o.tv_iters,
            o.edge_threshold.max(1) as f32,
            o.tv_amount.clamp(0.0, 1.0),
        );
    }
    out
}

/// Smooth only the block-boundary seams that look like compression artifacts. Vertical seams
/// (columns that are multiples of `block`) are smoothed horizontally, then horizontal seams
/// vertically. Each pass reads a **snapshot** so seams don't cascade into each other; the two passes
/// run in sequence (the horizontal pass sees the de-vertical-seamed result).
///
/// The artifact test per seam pixel (one channel): let the step across the seam be `d = |a1−b1|`
/// where `a1,b1` straddle it and `a2,b2` are the next pixels into each block. Smooth **only** when
/// `0 < d < edge_threshold` (a small step, not a real edge) **and** `max(|a2−a1|,|b1−b2|) < d`
/// (the interiors are flatter than the seam — the seam is the dominant local feature, i.e. blocking,
/// not texture). Then pull `a1,b1` toward a short [1 2 1]-weighted ramp by `strength`.
fn deblock_seams(
    px: &mut [u8],
    w: usize,
    h: usize,
    block: usize,
    strength: f32,
    edge_threshold: u32,
) {
    let thr = edge_threshold as i32;
    // Vertical seams: smooth across columns for each row.
    let src = px.to_vec();
    let mut bx = block;
    while bx < w {
        for y in 0..h {
            let row = y * w;
            for c in 0..3 {
                let at = |x: usize| src[(row + x) * 4 + c] as i32;
                let (a1, b1) = (at(bx - 1), at(bx));
                let d = (a1 - b1).abs();
                if d == 0 || d >= thr {
                    continue;
                }
                let a2 = if bx >= 2 { at(bx - 2) } else { a1 };
                let b2 = if bx + 1 < w { at(bx + 1) } else { b1 };
                if (a2 - a1).abs().max((b1 - b2).abs()) >= d {
                    continue; // interior gradient dominates → real texture, leave it
                }
                let s_a1 = (a2 + 2 * a1 + b1 + 2) / 4;
                let s_b1 = (a1 + 2 * b1 + b2 + 2) / 4;
                blend_into(px, (row + bx - 1) * 4 + c, a1, s_a1, strength);
                blend_into(px, (row + bx) * 4 + c, b1, s_b1, strength);
            }
        }
        bx += block;
    }
    // Horizontal seams: smooth across rows for each column (snapshot = the post-vertical buffer).
    let src = px.to_vec();
    let mut by = block;
    while by < h {
        for x in 0..w {
            for c in 0..3 {
                let at = |y: usize| src[(y * w + x) * 4 + c] as i32;
                let (a1, b1) = (at(by - 1), at(by));
                let d = (a1 - b1).abs();
                if d == 0 || d >= thr {
                    continue;
                }
                let a2 = if by >= 2 { at(by - 2) } else { a1 };
                let b2 = if by + 1 < h { at(by + 1) } else { b1 };
                if (a2 - a1).abs().max((b1 - b2).abs()) >= d {
                    continue;
                }
                let s_a1 = (a2 + 2 * a1 + b1 + 2) / 4;
                let s_b1 = (a1 + 2 * b1 + b2 + 2) / 4;
                blend_into(px, ((by - 1) * w + x) * 4 + c, a1, s_a1, strength);
                blend_into(px, (by * w + x) * 4 + c, b1, s_b1, strength);
            }
        }
        by += block;
    }
}

/// Write `orig*(1-t) + smoothed*t` (rounded, clamped) into `px[idx]`.
#[inline]
fn blend_into(px: &mut [u8], idx: usize, orig: i32, smoothed: i32, t: f32) {
    let v = orig as f32 * (1.0 - t) + smoothed as f32 * t;
    px[idx] = v.round().clamp(0.0, 255.0) as u8;
}

/// Edge-preserving anisotropic diffusion (Perona–Malik) over the RGB channels, blended over the
/// original by `amount`. `k` is the conduction threshold: neighbour differences ≫ k barely diffuse
/// (edges preserved), differences ≪ k diffuse freely (flats + ringing smoothed). `dt = 0.2` keeps
/// the 4-neighbour explicit scheme stable. Alpha is untouched.
fn diffuse(px: &mut [u8], w: usize, h: usize, iters: usize, k: f32, amount: f32) {
    let n = w * h;
    let dt = 0.2f32;
    let inv_k2 = 1.0 / (k * k);
    // Per-channel f32 working buffers seeded from the source.
    for c in 0..3 {
        let mut u: Vec<f32> = (0..n).map(|i| px[i * 4 + c] as f32).collect();
        let orig = u.clone();
        let mut next = u.clone();
        for _ in 0..iters {
            for y in 0..h {
                for x in 0..w {
                    let i = y * w + x;
                    let cur = u[i];
                    // 4-neighbour fluxes, each damped by the Perona–Malik conductance
                    // c(∇) = 1 / (1 + (∇/k)²) so a big step across an edge contributes ~0.
                    let mut acc = 0.0f32;
                    let mut flux = |ni: usize| {
                        let d = u[ni] - cur;
                        acc += d / (1.0 + d * d * inv_k2);
                    };
                    if x > 0 {
                        flux(i - 1);
                    }
                    if x + 1 < w {
                        flux(i + 1);
                    }
                    if y > 0 {
                        flux(i - w);
                    }
                    if y + 1 < h {
                        flux(i + w);
                    }
                    next[i] = cur + dt * acc;
                }
            }
            std::mem::swap(&mut u, &mut next);
        }
        for i in 0..n {
            let v = orig[i] * (1.0 - amount) + u[i] * amount;
            px[i * 4 + c] = v.round().clamp(0.0, 255.0) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> DeblockOpts {
        DeblockOpts {
            block_size: 8,
            strength: 1.0,
            edge_threshold: 40,
            tv_amount: 0.0,
            tv_iters: 4,
        }
    }

    /// A 16×1 row: a flat left block (value 100) and a flat right block (value 108) with an 8-px
    /// seam between them = an 8-value blocking step. De-blocking should soften the seam (bring the
    /// two straddling pixels closer together) without touching the flat interiors.
    #[test]
    fn smooths_a_small_block_seam() {
        let (w, h) = (16, 1);
        let mut px = vec![0u8; w * h * 4];
        for x in 0..w {
            let v = if x < 8 { 100 } else { 108 };
            let i = x * 4;
            px[i] = v;
            px[i + 1] = v;
            px[i + 2] = v;
            px[i + 3] = 255;
        }
        let out = deblock(&px, w, h, &opts());
        let l = out[7 * 4] as i32; // last px of the left block (a1)
        let r = out[8 * 4] as i32; // first px of the right block (b1)
        assert!(
            (l - r).abs() < 8,
            "seam step {} → should shrink below 8",
            (l - r).abs()
        );
        // Deep interiors are untouched.
        assert_eq!(out[0], 100);
        assert_eq!(out[15 * 4], 108);
    }

    /// A large step on the seam is a *real edge* (or exceeds `edge_threshold`) and must be preserved.
    #[test]
    fn preserves_a_real_edge_on_the_grid() {
        let (w, h) = (16, 1);
        let mut px = vec![0u8; w * h * 4];
        for x in 0..w {
            let v = if x < 8 { 20 } else { 220 }; // a 200-value step ≫ threshold
            let i = x * 4;
            px[i] = v;
            px[i + 1] = v;
            px[i + 2] = v;
            px[i + 3] = 255;
        }
        let out = deblock(&px, w, h, &opts());
        assert_eq!(out[7 * 4], 20, "left of a real edge is preserved");
        assert_eq!(out[8 * 4], 220, "right of a real edge is preserved");
    }

    /// Diffusion melts random speckle in a flat field toward the mean while keeping alpha.
    #[test]
    fn diffusion_flattens_speckle() {
        let (w, h) = (8, 8);
        let mut px = vec![0u8; w * h * 4];
        for i in 0..w * h {
            // ~128 ± a little pseudo-random speckle (deterministic).
            let noise = ((i * 37 + 11) % 17) as i32 - 8;
            let v = (128 + noise).clamp(0, 255) as u8;
            px[i * 4] = v;
            px[i * 4 + 1] = v;
            px[i * 4 + 2] = v;
            px[i * 4 + 3] = 255;
        }
        let before_var = variance(&px, w, h);
        let o = DeblockOpts {
            strength: 0.0, // isolate the diffusion pass
            tv_amount: 1.0,
            tv_iters: 8,
            ..opts()
        };
        let out = deblock(&px, w, h, &o);
        let after_var = variance(&out, w, h);
        assert!(
            after_var < before_var,
            "speckle variance {before_var} → {after_var} should drop"
        );
        assert_eq!(out[3], 255, "alpha preserved");
    }

    fn variance(px: &[u8], w: usize, h: usize) -> f32 {
        let n = (w * h) as f32;
        let mean: f32 = (0..w * h).map(|i| px[i * 4] as f32).sum::<f32>() / n;
        (0..w * h)
            .map(|i| (px[i * 4] as f32 - mean).powi(2))
            .sum::<f32>()
            / n
    }

    #[test]
    fn empty_or_short_buffer_is_returned_unchanged() {
        assert!(deblock(&[], 0, 0, &opts()).is_empty());
        let short = vec![1, 2, 3];
        assert_eq!(deblock(&short, 4, 4, &opts()), short);
    }

    // Visual dump: run deblock + undither on the user's real JPEGs so a human can eyeball the
    // before/after. Run: cargo test --release dump_real_deblock -- --ignored --nocapture
    #[test]
    #[ignore = "reads real JPEGs; run with --ignored to produce visual output"]
    fn dump_real_deblock() {
        let inputs = [
            "/home/grymmjack/Dropbox/wp8737378-atari-2600-wallpapers.jpg",
            "/home/grymmjack/Dropbox/wp8737422-atari-2600-wallpapers.jpg",
            "/home/grymmjack/Dropbox/IMG_4267.JPG",
            "/home/grymmjack/Desktop/MISC-OLDSCHOOL-PIXEL-ART.jpg",
            "/home/grymmjack/Desktop/ATARI-2600-PIXEL-ART.jpg",
        ];
        let dir = "/tmp/kt_deblock";
        std::fs::create_dir_all(dir).unwrap();
        for path in inputs {
            if !std::path::Path::new(path).exists() {
                eprintln!("skip (not found): {path}");
                continue;
            }
            let img = image::open(path).unwrap().to_rgba8();
            let (w, h) = (img.width() as usize, img.height() as usize);
            let rgba = img.into_raw();
            let stem = std::path::Path::new(path)
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .to_string();
            eprintln!("{stem}: {w}x{h}");
            save(dir, &stem, "0_original", &rgba, w, h);
            // De-block only, a couple of strengths.
            for (tag, edge) in [("deblock_e24", 24u32), ("deblock_e48", 48)] {
                let o = DeblockOpts {
                    block_size: 8,
                    strength: 1.0,
                    edge_threshold: edge,
                    tv_amount: 0.0,
                    tv_iters: 4,
                };
                save(dir, &stem, tag, &deblock(&rgba, w, h, &o), w, h);
            }
            // De-block + diffusion (ringing removal).
            let o = DeblockOpts {
                block_size: 8,
                strength: 1.0,
                edge_threshold: 32,
                tv_amount: 0.7,
                tv_iters: 8,
            };
            save(
                dir,
                &stem,
                "deblock_plus_smooth",
                &deblock(&rgba, w, h, &o),
                w,
                h,
            );
            // Undither variants (smooth + snap).
            let uo = crate::undither::UnditherOpts {
                edge_threshold: 220,
                radius: 1,
                strength: 1.0,
                snap: false,
            };
            save(
                dir,
                &stem,
                "undither_smooth",
                &crate::undither::undither(&rgba, w, h, None, &uo),
                w,
                h,
            );
            let mut distinct = crate::thumb::distinct_opaque_colors(&rgba);
            if distinct.len() > 256 {
                distinct = crate::thumb::median_cut(&distinct, 256);
            }
            let uo = crate::undither::UnditherOpts {
                edge_threshold: 220,
                radius: 2,
                strength: 1.0,
                snap: true,
            };
            save(
                dir,
                &stem,
                "undither_snap",
                &crate::undither::undither(&rgba, w, h, Some(&distinct), &uo),
                w,
                h,
            );
        }
        eprintln!("wrote variants to {dir}");
    }

    // Prove deblock RUNS after the scaler, and show why order/block-size matter. Compares:
    //   HQ4x only | deblock(block8)→HQ4x | HQ4x→deblock(block2) | HQ4x→deblock(block32)
    // Run: cargo test --release order_vs_scaler -- --ignored --nocapture
    #[test]
    #[ignore = "reads a real JPEG; run with --ignored to produce visual output"]
    fn order_vs_scaler() {
        let path = "/home/grymmjack/Desktop/ATARI-2600-PIXEL-ART.jpg";
        if !std::path::Path::new(path).exists() {
            eprintln!("skip (not found): {path}");
            return;
        }
        let img = image::open(path).unwrap().to_rgba8();
        let (iw, ih) = (img.width() as usize, img.height() as usize);
        let full = img.into_raw();
        // A small crop so HQ4x is fast.
        let (cw, ch, cx, cy) = (120usize, 90usize, 300usize, 220usize);
        let mut crop = vec![0u8; cw * ch * 4];
        for y in 0..ch {
            for x in 0..cw {
                let s = ((cy + y).min(ih - 1) * iw + (cx + x).min(iw - 1)) * 4;
                let d = (y * cw + x) * 4;
                crop[d..d + 4].copy_from_slice(&full[s..s + 4]);
            }
        }
        let dir = "/tmp/kt_deblock_order";
        std::fs::create_dir_all(dir).unwrap();
        let hq4x = crate::scale::Scaler::Hq4x;
        let db = |block: u32| DeblockOpts {
            block_size: block as usize,
            strength: 0.8,
            edge_threshold: 132,
            tv_amount: 0.86,
            tv_iters: 2,
        };
        // HQ4x only.
        let (a, aw, ah) = hq4x.apply(&crop, cw, ch);
        save(dir, "cmp", "1_hq4x_only", &a, aw, ah);
        // deblock BEFORE upscale (the correct order): clean the JPEG, then enlarge.
        let pre = deblock(&crop, cw, ch, &db(8));
        let (b, bw, bh) = hq4x.apply(&pre, cw, ch);
        save(dir, "cmp", "2_deblock_then_hq4x", &b, bw, bh);
        // deblock AFTER upscale at block=2 (the user's setup) — seams are now 32px, so block=2 misses.
        let c = deblock(&a, aw, ah, &db(2));
        save(dir, "cmp", "3_hq4x_then_deblock_block2", &c, aw, ah);
        // deblock AFTER upscale at block=32 (matches the scaled seam spacing).
        let d = deblock(&a, aw, ah, &db(32));
        save(dir, "cmp", "4_hq4x_then_deblock_block32", &d, aw, ah);
        // Quantify: how many pixels each variant changed vs HQ4x-only (proves it ran).
        let changed = |x: &[u8]| x.chunks(4).zip(a.chunks(4)).filter(|(p, q)| p != q).count();
        eprintln!(
            "vs HQ4x-only — after@block2 changed {} px, after@block32 changed {} px",
            changed(&c),
            changed(&d)
        );
        eprintln!("wrote {dir}");
    }

    // Tune a general "remove artifacts, keep content" preset against the user's real examples.
    // Run: cargo test --release tune_cleanup_preset -- --ignored --nocapture
    #[test]
    #[ignore = "reads real JPEGs; run with --ignored to produce visual output"]
    fn tune_cleanup_preset() {
        let inputs = [
            "/home/grymmjack/Dropbox/wp8737378-atari-2600-wallpapers.jpg",
            "/home/grymmjack/Dropbox/wp8737422-atari-2600-wallpapers.jpg",
            "/home/grymmjack/Dropbox/IMG_4267.JPG",
            "/home/grymmjack/Desktop/MISC-OLDSCHOOL-PIXEL-ART.jpg",
            "/home/grymmjack/Desktop/ATARI-2600-PIXEL-ART.jpg",
        ];
        let dir = "/tmp/kt_preset";
        std::fs::create_dir_all(dir).unwrap();
        // Candidates: (tag, deblock strength, edge-keep, tv, tv_iters).
        let cands = [
            // The shipped "JPEG De-Artifact" preset settings.
            ("cleaned", 0.9f32, 50u32, 0.65f32, 4usize),
        ];
        for path in inputs {
            if !std::path::Path::new(path).exists() {
                eprintln!("skip (not found): {path}");
                continue;
            }
            let img = image::open(path).unwrap().to_rgba8();
            let (w, h) = (img.width() as usize, img.height() as usize);
            let rgba = img.into_raw();
            let stem = std::path::Path::new(path)
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .to_string();
            save(dir, &stem, "0_original", &rgba, w, h);
            for (tag, str_, edge, tv, iters) in cands {
                let o = DeblockOpts {
                    block_size: 8,
                    strength: str_,
                    edge_threshold: edge,
                    tv_amount: tv,
                    tv_iters: iters,
                };
                save(dir, &stem, tag, &deblock(&rgba, w, h, &o), w, h);
            }
        }
        eprintln!("wrote candidates to {dir}");
    }

    #[cfg(test)]
    fn save(dir: &str, stem: &str, tag: &str, buf: &[u8], w: usize, h: usize) {
        image::save_buffer(
            format!("{dir}/{stem}__{tag}.png"),
            buf,
            w as u32,
            h as u32,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
}
