//! Pixel-art **upscaling** algorithms for the Recolor pane's Resize → *Upscale* mode.
//!
//! Integer scalers that enlarge low-res sprites with edge-aware interpolation instead
//! of blocky nearest / blurry linear — the family charted at
//! <https://en.wikipedia.org/wiki/Pixel-art_scaling_algorithms>. They run as a
//! pre-pipeline step (upscale the source, then the rest of the recolor stack + Save
//! operate on the enlarged art). Everything works on straight (un-premultiplied) RGBA
//! byte buffers, comparing whole `[u8; 4]` pixels for equality, so it's self-contained
//! (no image-crate/version coupling) and unit-testable.
//!
//! Implemented: the hard-edge family (Scale2x/EPX, Scale3x, Eagle 2×/3×), **xBR
//! 2×/3×/4×** (edge-directed blending — 2×/3× ported from libxbr-standalone's
//! FILT2/FILT3, 4× = 2× chained), **HQ2x/3x/4x** (lookup-table family, via the `hqx`
//! crate through `hqx_scale`), and the **2xSaI / Super2xSaI / SuperEagle** trio (ported
//! verbatim from snes9x's `2xsai.cpp`, RGBA per-channel averaging in place of the C's
//! 16-bit colour-mask tricks). `Scaler` is the enum the UI + persistence key off; a new
//! algorithm is one variant + one fn.

/// Which pixel-art upscaler to apply (or `None` = leave the source untouched).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Scaler {
    #[default]
    None,
    Scale2x,
    Scale3x,
    Eagle2x,
    Eagle3x,
    Xbr2x,
    Xbr3x,
    Xbr4x,
    Hq2x,
    Hq3x,
    Hq4x,
    Sai2x,
    Super2xSai,
    SuperEagle,
}

impl Scaler {
    /// Every scaler, in menu order. `ALL[i] as u8 == i`, so the index persists.
    pub const ALL: [Scaler; 14] = [
        Scaler::None,
        Scaler::Scale2x,
        Scaler::Scale3x,
        Scaler::Eagle2x,
        Scaler::Eagle3x,
        Scaler::Xbr2x,
        Scaler::Xbr3x,
        Scaler::Xbr4x,
        Scaler::Hq2x,
        Scaler::Hq3x,
        Scaler::Hq4x,
        Scaler::Sai2x,
        Scaler::Super2xSai,
        Scaler::SuperEagle,
    ];

    pub fn from_u8(b: u8) -> Scaler {
        Self::ALL.get(b as usize).copied().unwrap_or(Scaler::None)
    }

    pub fn label(self) -> &'static str {
        match self {
            Scaler::None => "None",
            Scaler::Scale2x => "Scale2x (EPX)",
            Scaler::Scale3x => "Scale3x",
            Scaler::Eagle2x => "Eagle 2×",
            Scaler::Eagle3x => "Eagle 3×",
            Scaler::Xbr2x => "xBR 2×",
            Scaler::Xbr3x => "xBR 3×",
            Scaler::Xbr4x => "xBR 4×",
            Scaler::Hq2x => "HQ2x",
            Scaler::Hq3x => "HQ3x",
            Scaler::Hq4x => "HQ4x",
            Scaler::Sai2x => "2xSaI",
            Scaler::Super2xSai => "Super2xSaI",
            Scaler::SuperEagle => "SuperEagle",
        }
    }

    /// The integer enlargement factor (1 for `None`).
    pub fn factor(self) -> usize {
        match self {
            Scaler::None => 1,
            Scaler::Scale3x | Scaler::Eagle3x | Scaler::Xbr3x | Scaler::Hq3x => 3,
            Scaler::Xbr4x | Scaler::Hq4x => 4,
            _ => 2, // Scale2x, Eagle2x, xBR2x, HQ2x, 2xSaI, Super2xSaI, SuperEagle
        }
    }

    /// Upscale `rgba` (`w*h*4` bytes) → `(out, w*factor, h*factor)`. `None` (or a
    /// degenerate size) returns the input unchanged so callers can apply it blindly.
    pub fn apply(self, rgba: &[u8], w: usize, h: usize) -> (Vec<u8>, usize, usize) {
        if self == Scaler::None || w == 0 || h == 0 || rgba.len() < w * h * 4 {
            return (rgba.to_vec(), w, h);
        }
        let src = Grid { rgba, w, h };
        match self {
            Scaler::None => unreachable!(),
            Scaler::Scale2x => scale2x(&src),
            Scaler::Scale3x => scale3x(&src),
            Scaler::Eagle2x => eagle2x(&src),
            Scaler::Eagle3x => eagle3x(&src),
            Scaler::Xbr2x => xbr2x(&src),
            Scaler::Xbr3x => xbr3x(&src),
            Scaler::Xbr4x => xbr4x(&src),
            Scaler::Hq2x => hqx_scale(&src, 2),
            Scaler::Hq3x => hqx_scale(&src, 3),
            Scaler::Hq4x => hqx_scale(&src, 4),
            Scaler::Sai2x => sai2x(&src),
            Scaler::Super2xSai => super_2xsai(&src),
            Scaler::SuperEagle => super_eagle(&src),
        }
    }
}

/// A borrowed RGBA source with edge-clamped pixel access.
struct Grid<'a> {
    rgba: &'a [u8],
    w: usize,
    h: usize,
}

impl Grid<'_> {
    /// Pixel at `(x, y)`, clamped to the edges (so a 3×3 neighbourhood never reads
    /// out of bounds and the border replicates rather than wraps).
    #[inline]
    fn at(&self, x: isize, y: isize) -> [u8; 4] {
        let cx = x.clamp(0, self.w as isize - 1) as usize;
        let cy = y.clamp(0, self.h as isize - 1) as usize;
        let i = (cy * self.w + cx) * 4;
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }
}

/// Write one `factor×factor` output cell for source pixel `(x, y)` into `out`.
#[inline]
fn put_cell(out: &mut [u8], ow: usize, x: usize, y: usize, factor: usize, cell: &[[u8; 4]]) {
    for dy in 0..factor {
        for dx in 0..factor {
            let px = cell[dy * factor + dx];
            let o = ((y * factor + dy) * ow + (x * factor + dx)) * 4;
            out[o..o + 4].copy_from_slice(&px);
        }
    }
}

/// **Scale2x / EPX** — the classic 2× expander (Andrea Mazzoleni / Eric Johnston). Each
/// pixel becomes 2×2: a corner takes an orthogonal neighbour's colour only when that
/// neighbour matches the adjacent one *and* the opposing pair differs (an edge), which
/// rounds jaggies without inventing colours. Uses the 3×3 neighbourhood A B C / D E F /
/// G H I (E = centre).
fn scale2x(s: &Grid) -> (Vec<u8>, usize, usize) {
    let (ow, oh) = (s.w * 2, s.h * 2);
    let mut out = vec![0u8; ow * oh * 4];
    for y in 0..s.h as isize {
        for x in 0..s.w as isize {
            let (b, d, e, f, hh) = (
                s.at(x, y - 1),
                s.at(x - 1, y),
                s.at(x, y),
                s.at(x + 1, y),
                s.at(x, y + 1),
            );
            let (mut e0, mut e1, mut e2, mut e3) = (e, e, e, e);
            if b != hh && d != f {
                if d == b {
                    e0 = d;
                }
                if b == f {
                    e1 = f;
                }
                if d == hh {
                    e2 = d;
                }
                if hh == f {
                    e3 = f;
                }
            }
            put_cell(&mut out, ow, x as usize, y as usize, 2, &[e0, e1, e2, e3]);
        }
    }
    (out, ow, oh)
}

/// **Scale3x** — the 3× sibling of Scale2x (same edge test, nine output pixels). The
/// centre stays the source; corners mirror Scale2x; edges fill on the two-neighbour
/// rules. 3×3 neighbourhood A B C / D E F / G H I (E = centre).
fn scale3x(s: &Grid) -> (Vec<u8>, usize, usize) {
    let (ow, oh) = (s.w * 3, s.h * 3);
    let mut out = vec![0u8; ow * oh * 4];
    for y in 0..s.h as isize {
        for x in 0..s.w as isize {
            let a = s.at(x - 1, y - 1);
            let b = s.at(x, y - 1);
            let c = s.at(x + 1, y - 1);
            let d = s.at(x - 1, y);
            let e = s.at(x, y);
            let f = s.at(x + 1, y);
            let g = s.at(x - 1, y + 1);
            let hh = s.at(x, y + 1);
            let i = s.at(x + 1, y + 1);
            let cell = if b != hh && d != f {
                [
                    if d == b { d } else { e },
                    if (d == b && e != c) || (b == f && e != a) {
                        b
                    } else {
                        e
                    },
                    if b == f { f } else { e },
                    if (d == b && e != g) || (d == hh && e != a) {
                        d
                    } else {
                        e
                    },
                    e,
                    if (b == f && e != i) || (hh == f && e != c) {
                        f
                    } else {
                        e
                    },
                    if d == hh { d } else { e },
                    if (d == hh && e != i) || (hh == f && e != g) {
                        hh
                    } else {
                        e
                    },
                    if hh == f { f } else { e },
                ]
            } else {
                [e; 9]
            };
            put_cell(&mut out, ow, x as usize, y as usize, 3, &cell);
        }
    }
    (out, ow, oh)
}

/// **Eagle 2×** — each output corner takes the corner colour when the three source
/// neighbours forming it (two orthogonal + the diagonal) all match, else the centre.
/// Fatter than Scale2x on solid diagonals. 3×3 A B C / D E F / G H I (E = centre).
fn eagle2x(s: &Grid) -> (Vec<u8>, usize, usize) {
    let (ow, oh) = (s.w * 2, s.h * 2);
    let mut out = vec![0u8; ow * oh * 4];
    for y in 0..s.h as isize {
        for x in 0..s.w as isize {
            let a = s.at(x - 1, y - 1);
            let b = s.at(x, y - 1);
            let c = s.at(x + 1, y - 1);
            let d = s.at(x - 1, y);
            let e = s.at(x, y);
            let f = s.at(x + 1, y);
            let g = s.at(x - 1, y + 1);
            let hh = s.at(x, y + 1);
            let i = s.at(x + 1, y + 1);
            let cell = [
                if a == b && b == d { a } else { e },
                if b == c && c == f { c } else { e },
                if d == g && g == hh { g } else { e },
                if f == hh && hh == i { i } else { e },
            ];
            put_cell(&mut out, ow, x as usize, y as usize, 2, &cell);
        }
    }
    (out, ow, oh)
}

/// **Eagle 3×** — Eagle's corner rule at 3× (the four corners get the Eagle treatment;
/// the centre cross stays the source pixel). 3×3 A B C / D E F / G H I (E = centre).
fn eagle3x(s: &Grid) -> (Vec<u8>, usize, usize) {
    let (ow, oh) = (s.w * 3, s.h * 3);
    let mut out = vec![0u8; ow * oh * 4];
    for y in 0..s.h as isize {
        for x in 0..s.w as isize {
            let a = s.at(x - 1, y - 1);
            let b = s.at(x, y - 1);
            let c = s.at(x + 1, y - 1);
            let d = s.at(x - 1, y);
            let e = s.at(x, y);
            let f = s.at(x + 1, y);
            let g = s.at(x - 1, y + 1);
            let hh = s.at(x, y + 1);
            let i = s.at(x + 1, y + 1);
            let tl = if a == b && b == d { a } else { e };
            let tr = if b == c && c == f { c } else { e };
            let bl = if d == g && g == hh { g } else { e };
            let br = if f == hh && hh == i { i } else { e };
            // Only the four corners get the Eagle treatment; the cross (edges + centre)
            // stays the source pixel — filling the edges with neighbours fringed colour.
            let cell = [tl, e, tr, e, e, e, bl, e, br];
            put_cell(&mut out, ow, x as usize, y as usize, 3, &cell);
        }
    }
    (out, ow, oh)
}

// ---------------------------------------------------------------------------------
// xBR (Hyllian) — an edge-directed *blending* scaler (the smooth family). Ported
// faithfully from libxbr-standalone's FILT2 macro (Treeki/Hyllian). Unlike the hard
// scalers above it produces intermediate colours, so it needs YUV-distance edge
// detection + weighted blends. The 4 output sub-pixels are handled by rotating the
// neighbourhood 90° four times and applying the SAME filter — the rotation is derived
// from the 5×5 grid geometry (below), so there's no hand-transcribed permutation.
// ---------------------------------------------------------------------------------

/// BT.601 Y/U/V (0..255-ish) for a pixel — the space xBR measures colour distance in.
#[inline]
fn yuv(p: [u8; 4]) -> (i32, i32, i32) {
    let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
    let y = (299 * r + 587 * g + 114 * b) / 1000;
    let u = (-169 * r - 331 * g + 500 * b) / 1000 + 128;
    let v = (500 * r - 419 * g - 81 * b) / 1000 + 128;
    (y, u, v)
}

/// xBR pixel difference: |Δα| + |ΔY| + |ΔU| + |ΔV| (the libxbr `pixel_diff`).
#[inline]
fn df(a: [u8; 4], b: [u8; 4]) -> u32 {
    let (ay, au, av) = yuv(a);
    let (by, bu, bv) = yuv(b);
    ((a[3] as i32 - b[3] as i32).abs() + (ay - by).abs() + (au - bu).abs() + (av - bv).abs()) as u32
}

/// "Equal enough" — the libxbr threshold (`df < 155`).
#[inline]
fn eq(a: [u8; 4], b: [u8; 4]) -> bool {
    df(a, b) < 155
}

/// Blend `a` toward `b` by `n`/256 per channel (the `ALPHA_BLEND_n_W` weights: 128 =
/// 50%, 192 = ¾, 224 = ⅞, 64 = ¼).
#[inline]
fn blend(a: [u8; 4], b: [u8; 4], n: i32) -> [u8; 4] {
    std::array::from_fn(|k| {
        let (av, bv) = (a[k] as i32, b[k] as i32);
        (av + (bv - av) * n / 256).clamp(0, 255) as u8
    })
}

/// The xBR neighbourhood — the inner 3×3 (pa..pi, pe = centre) plus the 12 outer
/// pixels, on the 5×5-minus-corners grid:
/// ```text
///        a1 b1 c1
///     a0 pa pb pc c4
///     d0 pd pe pf f4
///     g0 pg ph pi i4
///        g5 h5 i5
/// ```
#[derive(Clone, Copy)]
#[rustfmt::skip]
struct Nb {
    pa: [u8;4], pb: [u8;4], pc: [u8;4],
    pd: [u8;4], pe: [u8;4], pf: [u8;4],
    pg: [u8;4], ph: [u8;4], pi: [u8;4],
    a0: [u8;4], a1: [u8;4], b1: [u8;4], c1: [u8;4], c4: [u8;4],
    d0: [u8;4], f4: [u8;4], g0: [u8;4], g5: [u8;4], h5: [u8;4], i4: [u8;4], i5: [u8;4],
}

impl Nb {
    /// Rotate the whole neighbourhood 90° clockwise (new(r,c) = old(4−c, r)). Applying
    /// the filter to each of the 4 rotations covers the 4 output quadrants.
    #[rustfmt::skip]
    fn rot_cw(&self) -> Nb {
        Nb {
            pa: self.pg, pb: self.pd, pc: self.pa,
            pd: self.ph, pe: self.pe, pf: self.pb,
            pg: self.pi, ph: self.pf, pi: self.pc,
            a1: self.g0, b1: self.d0, c1: self.a0, a0: self.g5, c4: self.a1,
            d0: self.h5, f4: self.b1, g0: self.i5, i4: self.c1, g5: self.i4, h5: self.f4, i5: self.c4,
        }
    }
}

/// One xBR-2× filter pass for the current orientation, blending into the 2×2 output
/// block `out` (indices 0=TL 1=TR 2=BL 3=BR) via `n` = the rotated (n0,n1,n2,n3). Only
/// the "main" corner `n3` (+ n1/n2 on a strong edge) are touched. Verbatim FILT2.
#[allow(clippy::nonminimal_bool)]
fn xbr_filt2(nb: &Nb, out: &mut [[u8; 4]; 4], n: [usize; 4]) {
    let (pe, pf, ph) = (nb.pe, nb.pf, nb.ph);
    if pe == ph || pe == pf {
        return;
    }
    let (pb, pc, pd, pg, pi) = (nb.pb, nb.pc, nb.pd, nb.pg, nb.pi);
    let (f4, h5, i4, i5) = (nb.f4, nb.h5, nb.i4, nb.i5);
    let e = df(pe, pc) + df(pe, pg) + df(pi, h5) + df(pi, f4) + (df(ph, pf) << 2);
    let i = df(ph, pd) + df(ph, i5) + df(pf, i4) + df(pf, pb) + (df(pe, pi) << 2);
    if e > i {
        return;
    }
    let px = if df(pe, pf) <= df(pe, ph) { pf } else { ph };
    let (n1, n2, n3) = (n[1], n[2], n[3]);
    let strong = e < i
        && ((!eq(pf, pb) && !eq(ph, pd))
            || (eq(pe, pi) && !eq(pf, i4) && !eq(ph, i5))
            || eq(pe, pg)
            || eq(pe, pc));
    if strong {
        let ke = df(pf, pg);
        let ki = df(ph, pc);
        let left = ke * 2 <= ki && pe != pg && pd != pg;
        let up = ke >= ki * 2 && pe != pc && pb != pc;
        if left && up {
            out[n3] = blend(out[n3], px, 224);
            out[n2] = blend(out[n2], px, 64);
            out[n1] = out[n2];
        } else if left {
            out[n3] = blend(out[n3], px, 192);
            out[n2] = blend(out[n2], px, 64);
        } else if up {
            out[n3] = blend(out[n3], px, 192);
            out[n1] = blend(out[n1], px, 64);
        } else {
            out[n3] = blend(out[n3], px, 128);
        }
    } else {
        out[n3] = blend(out[n3], px, 128);
    }
}

/// Read the 21-pixel xBR neighbourhood centred on `(x, y)` (edge-clamped).
#[rustfmt::skip]
fn read_nb(s: &Grid, x: isize, y: isize) -> Nb {
    Nb {
        pa: s.at(x-1, y-1), pb: s.at(x, y-1), pc: s.at(x+1, y-1),
        pd: s.at(x-1, y),   pe: s.at(x, y),   pf: s.at(x+1, y),
        pg: s.at(x-1, y+1), ph: s.at(x, y+1), pi: s.at(x+1, y+1),
        a1: s.at(x-1, y-2), b1: s.at(x, y-2), c1: s.at(x+1, y-2),
        a0: s.at(x-2, y-1), c4: s.at(x+2, y-1),
        d0: s.at(x-2, y),   f4: s.at(x+2, y),
        g0: s.at(x-2, y+1), i4: s.at(x+2, y+1),
        g5: s.at(x-1, y+2), h5: s.at(x, y+2), i5: s.at(x+1, y+2),
    }
}

fn xbr2x(s: &Grid) -> (Vec<u8>, usize, usize) {
    let (ow, oh) = (s.w * 2, s.h * 2);
    let mut out = vec![0u8; ow * oh * 4];
    // Output-index maps per 90° rotation (0=TL 1=TR 2=BL 3=BR); the "main" corner n3
    // rotates BR→TR→TL→BL, matching the libxbr call order.
    const MAPS: [[usize; 4]; 4] = [[0, 1, 2, 3], [2, 0, 3, 1], [3, 2, 1, 0], [1, 3, 0, 2]];
    for y in 0..s.h as isize {
        for x in 0..s.w as isize {
            let mut cell = [s.at(x, y); 4]; // TL, TR, BL, BR
            let mut cur = read_nb(s, x, y);
            for m in MAPS {
                xbr_filt2(&cur, &mut cell, m);
                cur = cur.rot_cw();
            }
            put_cell(&mut out, ow, x as usize, y as usize, 2, &cell);
        }
    }
    (out, ow, oh)
}

/// One xBR-3× filter pass, blending into the 3×3 output block `out` (row-major 0..8,
/// 8 = bottom-right = the "main" corner) via the rotated index map `n`. Verbatim FILT3.
#[allow(clippy::nonminimal_bool)]
fn xbr_filt3(nb: &Nb, out: &mut [[u8; 4]; 9], n: [usize; 9]) {
    let (pe, pf, ph) = (nb.pe, nb.pf, nb.ph);
    if pe == ph || pe == pf {
        return;
    }
    let (pb, pc, pd, pg, pi) = (nb.pb, nb.pc, nb.pd, nb.pg, nb.pi);
    let (f4, h5, i4, i5) = (nb.f4, nb.h5, nb.i4, nb.i5);
    let e = df(pe, pc) + df(pe, pg) + df(pi, h5) + df(pi, f4) + (df(ph, pf) << 2);
    let i = df(ph, pd) + df(ph, i5) + df(pf, i4) + df(pf, pb) + (df(pe, pi) << 2);
    if e > i {
        return;
    }
    let px = if df(pe, pf) <= df(pe, ph) { pf } else { ph };
    let strong = e < i
        && ((!eq(pf, pb) && !eq(pf, pc))
            || (!eq(ph, pd) && !eq(ph, pg))
            || (eq(pe, pi) && ((!eq(pf, f4) && !eq(pf, i4)) || (!eq(ph, h5) && !eq(ph, i5))))
            || eq(pe, pg)
            || eq(pe, pc));
    let (n2, n5, n6, n7, n8) = (n[2], n[5], n[6], n[7], n[8]);
    if strong {
        let ke = df(pf, pg);
        let ki = df(ph, pc);
        let left = ke * 2 <= ki && pe != pg && pd != pg;
        let up = ke >= ki * 2 && pe != pc && pb != pc;
        if left && up {
            out[n7] = blend(out[n7], px, 192);
            out[n6] = blend(out[n6], px, 64);
            out[n5] = out[n7];
            out[n2] = out[n6];
            out[n8] = px;
        } else if left {
            out[n7] = blend(out[n7], px, 192);
            out[n5] = blend(out[n5], px, 64);
            out[n6] = blend(out[n6], px, 64);
            out[n8] = px;
        } else if up {
            out[n5] = blend(out[n5], px, 192);
            out[n7] = blend(out[n7], px, 64);
            out[n2] = blend(out[n2], px, 64);
            out[n8] = px;
        } else {
            out[n8] = blend(out[n8], px, 224);
            out[n5] = blend(out[n5], px, 32);
            out[n7] = blend(out[n7], px, 32);
        }
    } else {
        out[n8] = blend(out[n8], px, 128);
    }
}

fn xbr3x(s: &Grid) -> (Vec<u8>, usize, usize) {
    let (ow, oh) = (s.w * 3, s.h * 3);
    let mut out = vec![0u8; ow * oh * 4];
    // 3×3 output-index maps per 90° rotation (row-major 0..8); main corner 8 → 2 → 0 → 6.
    const MAPS: [[usize; 9]; 4] = [
        [0, 1, 2, 3, 4, 5, 6, 7, 8],
        [6, 3, 0, 7, 4, 1, 8, 5, 2],
        [8, 7, 6, 5, 4, 3, 2, 1, 0],
        [2, 5, 8, 1, 4, 7, 0, 3, 6],
    ];
    for y in 0..s.h as isize {
        for x in 0..s.w as isize {
            let mut cell = [s.at(x, y); 9];
            let mut cur = read_nb(s, x, y);
            for m in MAPS {
                xbr_filt3(&cur, &mut cell, m);
                cur = cur.rot_cw();
            }
            put_cell(&mut out, ow, x as usize, y as usize, 3, &cell);
        }
    }
    (out, ow, oh)
}

/// xBR 4× — chain xBR 2× twice (a genuinely smooth 4×; libxbr's native FILT4 isn't
/// transcribed here). Each pass re-detects edges on the already-doubled image.
fn xbr4x(s: &Grid) -> (Vec<u8>, usize, usize) {
    let (two, w2, h2) = xbr2x(s);
    xbr2x(&Grid {
        rgba: &two,
        w: w2,
        h: h2,
    })
}

/// **HQx** (HQ2x/HQ3x/HQ4x) via the `hqx` crate — the high-quality lookup-table family.
/// Bridges our straight-RGBA bytes to/from the crate's packed `0xAARRGGBB` u32 slices.
fn hqx_scale(s: &Grid, factor: usize) -> (Vec<u8>, usize, usize) {
    let (ow, oh) = (s.w * factor, s.h * factor);
    // RGBA bytes → 0xAARRGGBB u32 (exactly w*h pixels; the buffer may be longer).
    let src: Vec<u32> = s.rgba[..s.w * s.h * 4]
        .chunks_exact(4)
        .map(|p| (p[3] as u32) << 24 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | (p[2] as u32))
        .collect();
    let mut dst = vec![0u32; ow * oh];
    let (w, h) = (s.w as u32, s.h as u32);
    match factor {
        2 => hqx::hq2x(&src, &mut dst, w, h),
        3 => hqx::hq3x(&src, &mut dst, w, h),
        _ => hqx::hq4x(&src, &mut dst, w, h),
    }
    // 0xAARRGGBB u32 → RGBA bytes.
    let out = dst
        .iter()
        .flat_map(|&v| [(v >> 16) as u8, (v >> 8) as u8, v as u8, (v >> 24) as u8])
        .collect();
    (out, ow, oh)
}

// ---------------------------------------------------------------------------------
// 2xSaI family (Derek Liauw Kie Fa / "Kreed") — ported verbatim from the snes9x
// `2xsai.cpp` (2xSaI / Super2xSaI / SuperEagle). `INTERPOLATE` = 2-pixel average,
// `Q_INTERPOLATE` = 4-pixel average (here per-channel RGBA; the C's colour-mask
// bit-tricks are just that). `GetResult*` vote on pixel equality and port directly.
// ---------------------------------------------------------------------------------

/// 2-pixel average (INTERPOLATE), per channel.
#[inline]
fn avg2(a: [u8; 4], b: [u8; 4]) -> [u8; 4] {
    std::array::from_fn(|k| ((a[k] as u16 + b[k] as u16) >> 1) as u8)
}

/// 4-pixel average (Q_INTERPOLATE), per channel.
#[inline]
fn avg4(a: [u8; 4], b: [u8; 4], c: [u8; 4], d: [u8; 4]) -> [u8; 4] {
    std::array::from_fn(|k| ((a[k] as u16 + b[k] as u16 + c[k] as u16 + d[k] as u16) >> 2) as u8)
}

/// `GetResult` / `GetResult1` (GetResult1's 5th arg is unused in the C).
#[inline]
fn gr(a: [u8; 4], b: [u8; 4], c: [u8; 4], d: [u8; 4]) -> i32 {
    let (mut x, mut y, mut r) = (0, 0, 0);
    if a == c {
        x += 1;
    } else if b == c {
        y += 1;
    }
    if a == d {
        x += 1;
    } else if b == d {
        y += 1;
    }
    if x <= 1 {
        r += 1;
    }
    if y <= 1 {
        r -= 1;
    }
    r
}

/// `GetResult2` — same as `gr` but the two increments are sign-swapped.
#[inline]
fn gr2(a: [u8; 4], b: [u8; 4], c: [u8; 4], d: [u8; 4]) -> i32 {
    -gr(a, b, c, d)
}

/// **2xSaI** — the original. 4×4 neighbourhood colorA..P (A = centre). Output 2×2 is
/// [A (TL), product (TR), product1 (BL), product2 (BR)].
#[allow(clippy::nonminimal_bool)]
fn sai2x(s: &Grid) -> (Vec<u8>, usize, usize) {
    let (ow, oh) = (s.w * 2, s.h * 2);
    let mut out = vec![0u8; ow * oh * 4];
    for y in 0..s.h as isize {
        for x in 0..s.w as isize {
            let (ci, ce, cf, cj) = (
                s.at(x - 1, y - 1),
                s.at(x, y - 1),
                s.at(x + 1, y - 1),
                s.at(x + 2, y - 1),
            );
            let (cg, ca, cb, ck) = (s.at(x - 1, y), s.at(x, y), s.at(x + 1, y), s.at(x + 2, y));
            let (ch, cc, cd, cl) = (
                s.at(x - 1, y + 1),
                s.at(x, y + 1),
                s.at(x + 1, y + 1),
                s.at(x + 2, y + 1),
            );
            let (cm, cn, co, cp) = (
                s.at(x - 1, y + 2),
                s.at(x, y + 2),
                s.at(x + 1, y + 2),
                s.at(x + 2, y + 2),
            );
            let _ = (ck, cp); // read for completeness; unused by 2xSaI's rules
            let (product, product1, product2) = if ca == cd && cb != cc {
                let p = if (ca == ce && cb == cl) || (ca == cc && ca == cf && cb != ce && cb == cj)
                {
                    ca
                } else {
                    avg2(ca, cb)
                };
                let p1 = if (ca == cg && cc == co) || (ca == cb && ca == ch && cg != cc && cc == cm)
                {
                    ca
                } else {
                    avg2(ca, cc)
                };
                (p, p1, ca)
            } else if cb == cc && ca != cd {
                let p = if (cb == cf && ca == ch) || (cb == ce && cb == cd && ca != cf && ca == ci)
                {
                    cb
                } else {
                    avg2(ca, cb)
                };
                let p1 = if (cc == ch && ca == cf) || (cc == cg && cc == cd && ca != ch && ca == ci)
                {
                    cc
                } else {
                    avg2(ca, cc)
                };
                (p, p1, cb)
            } else if ca == cd && cb == cc {
                if ca == cb {
                    (ca, ca, ca)
                } else {
                    let p1 = avg2(ca, cc);
                    let p = avg2(ca, cb);
                    let r = gr(ca, cb, cg, ce)
                        + gr2(cb, ca, ck, cf)
                        + gr2(cb, ca, ch, cn)
                        + gr(ca, cb, cl, co);
                    let p2 = if r > 0 {
                        ca
                    } else if r < 0 {
                        cb
                    } else {
                        avg4(ca, cb, cc, cd)
                    };
                    (p, p1, p2)
                }
            } else {
                let p = if ca == cc && ca == cf && cb != ce && cb == cj {
                    ca
                } else if cb == ce && cb == cd && ca != cf && ca == ci {
                    cb
                } else {
                    avg2(ca, cb)
                };
                let p1 = if ca == cb && ca == ch && cg != cc && cc == cm {
                    ca
                } else if cc == cg && cc == cd && ca != ch && ca == ci {
                    cc
                } else {
                    avg2(ca, cc)
                };
                (p, p1, avg4(ca, cb, cc, cd))
            };
            put_cell(
                &mut out,
                ow,
                x as usize,
                y as usize,
                2,
                &[ca, product, product1, product2],
            );
        }
    }
    (out, ow, oh)
}

/// The Super2xSaI / SuperEagle 4×4 neighbourhood (centre = c5; c2 is the pixel below).
#[rustfmt::skip]
struct Sai {
    b0: [u8;4], b1: [u8;4], b2: [u8;4], b3: [u8;4],
    c4: [u8;4], c5: [u8;4], c6: [u8;4], s2: [u8;4],
    c1: [u8;4], c2: [u8;4], c3: [u8;4], s1: [u8;4],
    a0: [u8;4], a1: [u8;4], a2: [u8;4], a3: [u8;4],
}

#[rustfmt::skip]
fn read_sai(s: &Grid, x: isize, y: isize) -> Sai {
    Sai {
        b0: s.at(x-1,y-1), b1: s.at(x,y-1), b2: s.at(x+1,y-1), b3: s.at(x+2,y-1),
        c4: s.at(x-1,y),   c5: s.at(x,y),   c6: s.at(x+1,y),   s2: s.at(x+2,y),
        c1: s.at(x-1,y+1), c2: s.at(x,y+1), c3: s.at(x+1,y+1), s1: s.at(x+2,y+1),
        a0: s.at(x-1,y+2), a1: s.at(x,y+2), a2: s.at(x+1,y+2), a3: s.at(x+2,y+2),
    }
}

/// **Super2xSaI**. Output 2×2 = [p1a (TL), p1b (TR), p2a (BL), p2b (BR)].
#[allow(clippy::nonminimal_bool)]
fn super_2xsai(s: &Grid) -> (Vec<u8>, usize, usize) {
    let (ow, oh) = (s.w * 2, s.h * 2);
    let mut out = vec![0u8; ow * oh * 4];
    for y in 0..s.h as isize {
        for x in 0..s.w as isize {
            let n = read_sai(s, x, y);
            let (c1, c2, c3, c4, c5, c6) = (n.c1, n.c2, n.c3, n.c4, n.c5, n.c6);
            let (a0, a1, a2, a3, b0, b1, b2, b3, s1, s2) =
                (n.a0, n.a1, n.a2, n.a3, n.b0, n.b1, n.b2, n.b3, n.s1, n.s2);
            // Right column (p1b top-right, p2b bottom-right).
            let (p1b, p2b) = if c2 == c6 && c5 != c3 {
                (c2, c2)
            } else if c5 == c3 && c2 != c6 {
                (c5, c5)
            } else if c5 == c3 && c2 == c6 && c5 != c6 {
                let r = gr(c6, c5, c1, a1)
                    + gr(c6, c5, c4, b1)
                    + gr(c6, c5, a2, s1)
                    + gr(c6, c5, b2, s2);
                let v = if r > 0 {
                    c6
                } else if r < 0 {
                    c5
                } else {
                    avg2(c5, c6)
                };
                (v, v)
            } else {
                let p2b = if c6 == c3 && c3 == a1 && c2 != a2 && c3 != a0 {
                    avg4(c3, c3, c3, c2)
                } else if c5 == c2 && c2 == a2 && a1 != c3 && c2 != a3 {
                    avg4(c2, c2, c2, c3)
                } else {
                    avg2(c2, c3)
                };
                let p1b = if c6 == c3 && c6 == b1 && c5 != b2 && c6 != b0 {
                    avg4(c6, c6, c6, c5)
                } else if c5 == c2 && c5 == b2 && b1 != c6 && c5 != b3 {
                    avg4(c6, c5, c5, c5)
                } else {
                    avg2(c5, c6)
                };
                (p1b, p2b)
            };
            // Left column.
            let p2a = if (c5 == c3 && c2 != c6 && c4 == c5 && c5 != a2)
                || (c5 == c1 && c6 == c5 && c4 != c2 && c5 != a0)
            {
                avg2(c2, c5)
            } else {
                c2
            };
            let p1a = if (c2 == c6 && c5 != c3 && c1 == c2 && c2 != b2)
                || (c4 == c2 && c3 == c2 && c1 != c5 && c2 != b0)
            {
                avg2(c2, c5)
            } else {
                c5
            };
            put_cell(
                &mut out,
                ow,
                x as usize,
                y as usize,
                2,
                &[p1a, p1b, p2a, p2b],
            );
        }
    }
    (out, ow, oh)
}

/// **SuperEagle**. Output 2×2 = [p1a (TL), p1b (TR), p2a (BL), p2b (BR)].
fn super_eagle(s: &Grid) -> (Vec<u8>, usize, usize) {
    let (ow, oh) = (s.w * 2, s.h * 2);
    let mut out = vec![0u8; ow * oh * 4];
    for y in 0..s.h as isize {
        for x in 0..s.w as isize {
            let n = read_sai(s, x, y);
            let (c1, c2, c3, c4, c5, c6) = (n.c1, n.c2, n.c3, n.c4, n.c5, n.c6);
            let (a1, a2, b1, b2, s1, s2) = (n.a1, n.a2, n.b1, n.b2, n.s1, n.s2);
            let (p1a, p1b, p2a, p2b);
            if c2 == c6 && c5 != c3 {
                p1b = c2;
                p2a = c2;
                if (c1 == c2 && c6 == s2) || (c2 == a1 && c6 == b2) {
                    p1a = avg2(c2, avg2(c2, c5));
                    p2b = avg2(c2, avg2(c2, c3));
                } else {
                    p1a = avg2(c5, c6);
                    p2b = avg2(c2, c3);
                }
            } else if c5 == c3 && c2 != c6 {
                p2b = c5;
                p1a = c5;
                if (b1 == c5 && c3 == a2) || (c4 == c5 && c3 == s1) {
                    p1b = avg2(c5, avg2(c5, c6));
                    p2a = avg2(c5, avg2(c5, c2));
                } else {
                    p1b = avg2(c5, c6);
                    p2a = avg2(c2, c3);
                }
            } else if c5 == c3 && c2 == c6 && c5 != c6 {
                let r = gr(c6, c5, c1, a1)
                    + gr(c6, c5, c4, b1)
                    + gr(c6, c5, a2, s1)
                    + gr(c6, c5, b2, s2);
                if r > 0 {
                    p1b = c2;
                    p2a = c2;
                    p1a = avg2(c5, c6);
                    p2b = avg2(c5, c6);
                } else if r < 0 {
                    p2b = c5;
                    p1a = c5;
                    p1b = avg2(c5, c6);
                    p2a = avg2(c5, c6);
                } else {
                    p2b = c5;
                    p1a = c5;
                    p1b = c2;
                    p2a = c2;
                }
            } else if c2 == c5 || c3 == c6 {
                p1a = c5;
                p2a = c2;
                p1b = c6;
                p2b = c3;
            } else {
                p1a = avg2(c5, avg2(c5, c6));
                p1b = avg2(c6, avg2(c5, c6));
                p2a = avg2(c2, avg2(c2, c3));
                p2b = avg2(c3, avg2(c2, c3));
            }
            put_cell(
                &mut out,
                ow,
                x as usize,
                y as usize,
                2,
                &[p1a, p1b, p2a, p2b],
            );
        }
    }
    (out, ow, oh)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A tiny helper: build a w×h RGBA buffer from a grayscale grid (0/255).
    fn buf(vals: &[u8], _w: usize) -> Vec<u8> {
        vals.iter().flat_map(|&v| [v, v, v, 255]).collect()
    }
    fn px(out: &[u8], ow: usize, x: usize, y: usize) -> u8 {
        out[(y * ow + x) * 4]
    }

    #[test]
    fn factor_and_output_size() {
        // Every non-None scaler enlarges a 4×4 by its factor and keeps a flat field flat.
        let flat = buf(&[128; 16], 4);
        for sc in Scaler::ALL {
            if sc == Scaler::None {
                continue;
            }
            let f = sc.factor();
            let (out, ow, oh) = sc.apply(&flat, 4, 4);
            assert_eq!((ow, oh), (4 * f, 4 * f), "{:?} size", sc);
            assert_eq!(out.len(), ow * oh * 4, "{:?} buffer len", sc);
            assert!(
                out.chunks_exact(4).all(|p| p[0] == 128),
                "{:?} keeps a flat field flat",
                sc
            );
        }
        // None is a passthrough.
        let src = buf(&[0, 255, 255, 0], 2);
        let (out, ow, oh) = Scaler::None.apply(&src, 2, 2);
        assert_eq!((ow, oh, out), (2, 2, src));
    }

    #[test]
    fn scale2x_rounds_a_diagonal_step() {
        // A 3×3 with a diagonal edge: centre is white, up+left are white, the opposing
        // down/right are black → Scale2x fills the top-left output corner white.
        //   B W .        (W=255, .=0)
        //   W W .
        //   . . .
        let src = buf(&[255, 255, 0, 255, 255, 0, 0, 0, 0], 3);
        let (out, ow, _) = Scaler::Scale2x.apply(&src, 3, 3);
        // Centre pixel (1,1) expands to out (2,2)..(3,3): its top-left corner rounds to
        // white because up(=255)==left(=255) and up!=down / left!=right.
        assert_eq!(px(&out, ow, 2, 2), 255, "top-left corner rounded to white");
    }

    #[test]
    fn eagle3x_cross_stays_center() {
        // Distinct centre + neighbours → the 3×3 block's cross (edges + centre) must all
        // be the centre; only corners may differ. Regression for the cross leaking
        // neighbour colours (the edge-fringe bug).
        let src = buf(&[1, 2, 3, 4, 5, 6, 7, 8, 9], 3); // all distinct grays
        let (out, ow, _) = Scaler::Eagle3x.apply(&src, 3, 3);
        let at = |x: usize, y: usize| out[(y * ow + x) * 4];
        // Source centre (1,1)=5 → output block rows 3..6, cols 3..6.
        assert_eq!(at(4, 3), 5, "top-middle = centre");
        assert_eq!(at(3, 4), 5, "middle-left = centre");
        assert_eq!(at(4, 4), 5, "centre = centre");
        assert_eq!(at(5, 4), 5, "middle-right = centre");
        assert_eq!(at(4, 5), 5, "bottom-middle = centre");
    }

    #[test]
    fn xbr2x_size_flat_and_blends() {
        // Correct 2× size, and a flat field must stay perfectly flat (the filter early-
        // outs when the centre equals its neighbours — no phantom edges).
        let (out, ow, oh) = Scaler::Xbr2x.apply(&buf(&[128; 9], 3), 3, 3);
        assert_eq!((ow, oh), (6, 6));
        assert!(out.chunks_exact(4).all(|p| p[0] == 128), "flat stays flat");
        // A hard diagonal edge should yield at least one *blended* (intermediate) pixel,
        // which is what makes xBR smooth rather than blocky.
        //   W W W . .
        //   W W . . .
        //   W . . . .   (5×5 upper-left white triangle, centre is on the edge)
        let mut v = vec![0u8; 25];
        for y in 0..5 {
            for x in 0..5 {
                if x + y < 3 {
                    v[y * 5 + x] = 255;
                }
            }
        }
        let (out, _, _) = Scaler::Xbr2x.apply(&buf(&v, 5), 5, 5);
        assert!(
            out.chunks_exact(4).any(|p| p[0] > 0 && p[0] < 255),
            "xBR produced a blended edge pixel"
        );
    }

    #[test]
    fn eagle2x_fills_a_solid_corner() {
        // up-left, up and left all white around a black centre → Eagle paints the whole
        // top-left output corner white (fatter than Scale2x).
        //   W W .
        //   W . .
        //   . . .
        let src = buf(&[255, 255, 0, 255, 0, 0, 0, 0, 0], 3);
        let (out, ow, _) = Scaler::Eagle2x.apply(&src, 3, 3);
        assert_eq!(px(&out, ow, 2, 2), 255, "eagle filled the corner white");
    }
}
