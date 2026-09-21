//! RIPscript (`.rip`) — vector BBS graphics (the EGA-era 640×350 drawing language).
//!
//! `icy_parser_core::RipParser` does the parsing; it emits structured `RipCommand`s
//! into our `CommandSink::emit_rip`. We rasterize them onto a 640×350 EGA canvas with
//! **hand-rolled integer (BGI-style) primitives** — no anti-aliasing — so the result
//! keeps RIP's pixel-exact EGA look (the whole point: matching the original).
//!
//! Static-viewer scope: all the drawing commands (lines/rects/circles/ovals/polys/
//! beziers/arcs/fills/flood-fill/text). The interactive bits — mouse fields, buttons,
//! queries, host/file commands, embedded icons/images — are parsed but not drawn.

use super::cp437_font_8x8::CP437_8X8;
use super::{DecodeError, Decoder};
use crate::image_types::{PixImage, Rgba};
// Aliased: the `RipCommand` variants `Color`/`WriteMode`/`LineStyle`/`FillStyle`
// collide with the type names, so we glob the variants and refer to the *types* by
// alias (WMode/LStyle/FStyle).
use icy_parser_core::{
    CommandParser, CommandSink, FillStyle as FStyle, ImagePasteMode, LineStyle as LStyle,
    RipCommand, RipParser, WriteMode as WMode,
};

const W: i32 = 640;
const H: i32 = 350;

/// Standard 16-colour EGA palette (the RIP default).
const EGA: [Rgba; 16] = [
    [0x00, 0x00, 0x00, 255], // 0  black
    [0x00, 0x00, 0xaa, 255], // 1  blue
    [0x00, 0xaa, 0x00, 255], // 2  green
    [0x00, 0xaa, 0xaa, 255], // 3  cyan
    [0xaa, 0x00, 0x00, 255], // 4  red
    [0xaa, 0x00, 0xaa, 255], // 5  magenta
    [0xaa, 0x55, 0x00, 255], // 6  brown
    [0xaa, 0xaa, 0xaa, 255], // 7  light grey
    [0x55, 0x55, 0x55, 255], // 8  dark grey
    [0x55, 0x55, 0xff, 255], // 9  light blue
    [0x55, 0xff, 0x55, 255], // 10 light green
    [0x55, 0xff, 0xff, 255], // 11 light cyan
    [0xff, 0x55, 0x55, 255], // 12 light red
    [0xff, 0x55, 0xff, 255], // 13 light magenta
    [0xff, 0xff, 0x55, 255], // 14 yellow
    [0xff, 0xff, 0xff, 255], // 15 white
];

/// An EGA 64-palette value (0..=63, `00rgbRGB`) → RGB, for RIP `SetPalette`.
fn ega64(v: u16) -> Rgba {
    let bit = |n: u16| ((v >> n) & 1u16) as u8;
    let ch = |hi: u16, lo: u16| bit(hi) * 0xaa + bit(lo) * 0x55;
    [ch(2, 5), ch(1, 4), ch(0, 3), 255]
}

/// EGA pixel aspect — a "circle" on a 640×350 EGA screen is drawn as a slightly
/// flattened ellipse so it *looks* round on a 4:3 monitor. icy_engine's exact value,
/// so our circles/arcs match its reference renders (was a rough 0.83 before).
const ASPECT: f64 = 350.0 / 480.0 * 1.06;

/// Midpoint-ellipse boundary offsets `(dx,dy)` from the centre (ported verbatim from
/// icy_engine's `ellipse`/`scan_ellipse`), restricted to the angular range `[a0,a1]`
/// in degrees (0°=east, CCW; `a1 < a0` selects the wrap-around arc). This is the
/// pixel-exact, *closed* curve — a polygon approximation (what we used before) can
/// leave a sub-pixel seam that a flood fill then leaks through.
fn ellipse_offsets(mut rx: i32, mut ry: i32, a0: i32, a1: i32) -> Vec<(i32, i32)> {
    let mut out = Vec::new();
    if ry == 0 {
        ry = 1;
        rx -= 1;
    }
    if rx <= 0 {
        rx = 1;
    }
    let (mut ex, mut ey) = (0i64, ry as i64);
    let a2 = (rx as i64) * (rx as i64);
    let b2 = (ry as i64) * (ry as i64);
    let crit1 = -(a2 / 4 + (rx % 2) as i64 + b2);
    let crit2 = -(b2 / 4 + (ry % 2) as i64 + a2);
    let crit3 = -(b2 / 4 + (ry % 2) as i64);
    let mut t = -(a2 * ey);
    let (mut dxt, mut dyt) = (2 * b2 * ex, -2 * a2 * ey);
    let (d2xt, d2yt) = (2 * b2, 2 * a2);
    let inv = a1 < a0;
    let in_range = |ang: i32| {
        if inv {
            ang <= a1 || ang >= a0
        } else {
            ang >= a0 && ang <= a1
        }
    };
    let mut skip = false;
    while ey >= 0 && ex <= rx as i64 {
        let angle = if ey == 0 {
            90
        } else {
            (90.0 - (ex as f64 / ey as f64).atan() * (180.0 / std::f64::consts::PI)).round() as i32
        };
        if !skip {
            let (xi, yi) = (ex as i32, ey as i32);
            if (ex != 0 || ey != 0) && in_range(180 - angle) {
                out.push((-xi, -yi));
            }
            if ex != 0 && ey != 0 {
                if in_range(angle) {
                    out.push((xi, -yi));
                }
                if in_range(180 + angle) {
                    out.push((-xi, yi));
                }
            }
            if in_range(360 - angle) {
                out.push((xi, yi));
            }
        }
        skip = false;
        if (t + b2 * ex <= crit1) || (t + a2 * ey <= crit3) {
            ex += 1;
            dxt += d2xt;
            t += dxt;
            if !((t + b2 * ex <= crit1) || (t + a2 * ey <= crit3)) && (t - a2 * ey > crit2) {
                skip = true;
            }
        } else if t - a2 * ey > crit2 {
            ey -= 1;
            dyt += d2yt;
            t += dyt;
            if (t + b2 * ex <= crit1) || (t + a2 * ey <= crit3) {
                skip = true;
            }
        } else {
            ex += 1;
            dxt += d2xt;
            t += dxt;
            ey -= 1;
            dyt += d2yt;
            t += dyt;
        }
    }
    out
}

struct Rip {
    px: Vec<u8>,     // W*H palette indices
    pal: [Rgba; 16], // current 16-colour palette
    color: u8,       // current drawing colour
    x: i32,          // current position
    y: i32,
    line_pat: u16,                          // 16-bit line dash pattern (solid = 0xFFFF)
    thick: i32,                             // line thickness (1 or 3 in RIP)
    fill_pat: [u8; 8],                      // 8×8 fill pattern
    fill_color: u8,                         // fill colour
    bkcolor: u8,                // background colour painted on pattern OFF-bits (BGI: 0)
    xor: bool,                  // XOR write mode
    clip: (i32, i32, i32, i32), // viewport (x0,y0,x1,y1 inclusive)
    fsize: i32,                 // text size (font 0 = ×N pixels; fonts 1-10 = BGI scale)
    fdir: u16,                  // text direction (0 = →, 1 = ↑)
    font: u16,                  // RIP font number: 0 = 8×8 bitmap, 1-10 = BGI stroke
    btn: Btn,                   // current RIP_BUTTON_STYLE (beveled menu panels)
    rip_image: Option<(i32, i32, Vec<u8>)>, // GetImage clipboard: (w, h, palette indices)
}

/// RIP button style — the beveled "panel" look BBS menus are built from. Colours and
/// flags map 1:1 from RIP_BUTTON_STYLE; rendering ported from icy_engine's `add_button`.
#[derive(Clone, Copy, Default)]
struct Btn {
    w: i32,
    h: i32,
    orient: u16,
    bevel: i32,
    label: u8,   // text colour
    bright: u8,  // top-left bevel highlight
    dark: u8,    // bottom-right bevel shadow
    surface: u8, // button face
    uline: u8,   // hotkey underline/highlight
    corner: u8,  // bevel corner pixel
    flags: i32,  // bevel/recess/chisel/sunken/dropshadow bits
    flags2: i32, // justify / highlight-hotkey bits
}

impl Rip {
    fn new() -> Self {
        Self {
            px: vec![0u8; (W * H) as usize],
            pal: EGA,
            color: 15,
            x: 0,
            y: 0,
            line_pat: 0xFFFF,
            thick: 1,
            fill_pat: [0xFF; 8],
            fill_color: 15,
            bkcolor: 0,
            xor: false,
            clip: (0, 0, W - 1, H - 1),
            fsize: 1,
            fdir: 0,
            font: 0,
            btn: Btn::default(),
            rip_image: None,
        }
    }

    /// RIP_GET_IMAGE: capture the screen rectangle into the clipboard. Bounds match
    /// icy_engine's `image()` exactly — `x0..x1`/`y0..y1` are **upper-exclusive**, so
    /// the stored tile is `(x1-x0) × (y1-y0)`; reproducing that off-by-one is what keeps
    /// the tiled stamps (paleo's textured background) pixel-aligned with the reference.
    fn get_image(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        let (w, h) = (x1 - x0, y1 - y0);
        if w <= 0 || h <= 0 {
            self.rip_image = None;
            return;
        }
        let mut data = Vec::with_capacity((w * h) as usize);
        for y in y0..y1 {
            for x in x0..x1 {
                let v = if (0..W).contains(&x) && (0..H).contains(&y) {
                    self.px[(y * W + x) as usize]
                } else {
                    0
                };
                data.push(v);
            }
        }
        self.rip_image = Some((w, h, data));
    }

    /// RIP_PUT_IMAGE: paste the clipboard at (x,y) under a paste mode. The source comes
    /// from the frozen clipboard while the logical ops read the *live* screen — that's
    /// what lets paleo XOR-stamp shifted copies of a grab onto itself to synthesize a
    /// dither texture (ported from icy_engine's `put_image`/`put_pixel`).
    fn put_image(&mut self, x: i32, y: i32, mode: ImagePasteMode) {
        let Some((w, h, data)) = self.rip_image.take() else {
            return;
        };
        let (cx0, cy0, cx1, cy1) = self.clip;
        let (lo_x, lo_y) = (cx0.max(0), cy0.max(0));
        let (hi_x, hi_y) = (cx1.min(W - 1), cy1.min(H - 1));
        let mut pos = 0usize;
        for iy in 0..h {
            for ix in 0..w {
                let src = data[pos];
                pos += 1;
                let (px, py) = (x + ix, y + iy);
                if px < lo_x || py < lo_y || px > hi_x || py > hi_y {
                    continue;
                }
                let i = (py * W + px) as usize;
                let cur = self.px[i];
                self.px[i] = match mode {
                    ImagePasteMode::Copy => src,
                    ImagePasteMode::Xor => cur ^ src,
                    ImagePasteMode::Or => cur | src,
                    ImagePasteMode::And => cur & src,
                    ImagePasteMode::Not => !src,
                } & 0x0f;
            }
        }
        self.rip_image = Some((w, h, data));
    }

    #[inline]
    fn put(&mut self, x: i32, y: i32, c: u8) {
        let (cx0, cy0, cx1, cy1) = self.clip;
        if x < cx0.max(0) || y < cy0.max(0) || x > cx1.min(W - 1) || y > cy1.min(H - 1) {
            return;
        }
        let i = (y * W + x) as usize;
        self.px[i] = if self.xor { self.px[i] ^ c } else { c };
    }

    /// A horizontal span honouring the current fill pattern. BGI fills are *opaque*:
    /// a set bit gets the fill colour, a clear bit gets the **background colour** (not
    /// left transparent) — matching icy_engine's `bar_rect` / PabloDraw's `Bar`, so a
    /// dithered band drawn over an earlier solid fill doesn't let it bleed through.
    fn fill_span(&mut self, x0: i32, x1: i32, y: i32) {
        let (a, b) = if x0 <= x1 { (x0, x1) } else { (x1, x0) };
        let (fc, bk) = (self.fill_color, self.bkcolor);
        for x in a..=b {
            let row = self.fill_pat[(y.rem_euclid(8)) as usize];
            let on = (row >> (7 - x.rem_euclid(8))) & 1 == 1;
            self.put(x, y, if on { fc } else { bk });
        }
    }

    /// The **BGI run-slice line** (ported from icy_engine), not plain Bresenham — it
    /// lays the line as whole horizontal/vertical *runs*. Matching it pixel-for-pixel
    /// is what makes adjacent outlines meet flush, so flood fills don't leak through a
    /// 1-px seam (the cause of the old fill leaks). Honors line pattern + thickness.
    fn line(&mut self, x1: i32, y1: i32, x2: i32, y2: i32) {
        let dy = (y2 - y1).abs();
        let dx = (x2 - x1).abs();
        let mut off = 0i32;
        if dx == 0 {
            self.fill_y(x1, y1.min(y2), dy + 1, &mut off);
        } else if dy == 0 {
            self.fill_x(y1, x1.min(x2), dx + 1, &mut off);
        } else if dx >= dy {
            let (mut px, mut py, step) = if y1 < y2 {
                (x1, y1, if x1 > x2 { -1 } else { 1 })
            } else {
                (x2, y2, if x2 > x1 { -1 } else { 1 })
            };
            let whole = (dx / dy) * step;
            let mut adj_up = dx % dy;
            let adj_down = dy * 2;
            let mut err = adj_up - adj_down;
            adj_up *= 2;
            let mut start = (whole / 2) + step;
            let end = start;
            if adj_up == 0 && whole & 1 == 0 {
                start -= step;
            }
            if whole & 1 != 0 {
                err += dy;
            }
            self.fill_x(py, px, start, &mut off);
            px += start;
            py += 1;
            for _ in 0..dy - 1 {
                let mut run = whole;
                err += adj_up;
                if err > 0 {
                    run += step;
                    err -= adj_down;
                }
                self.fill_x(py, px, run, &mut off);
                px += run;
                py += 1;
            }
            self.fill_x(py, px, end, &mut off);
        } else {
            let (mut px, mut py, adv) = if y1 < y2 {
                (x1, y1, if x1 > x2 { -1 } else { 1 })
            } else {
                (x2, y2, if x2 > x1 { -1 } else { 1 })
            };
            let whole = dy / dx;
            let mut adj_up = dy % dx;
            let adj_down = dx * 2;
            let mut err = adj_up - adj_down;
            adj_up *= 2;
            let mut start = (whole / 2) + 1;
            let end = start;
            if adj_up == 0 && whole & 1 == 0 {
                start -= 1;
            }
            if whole & 1 != 0 {
                err += dx;
            }
            self.fill_y(px, py, start, &mut off);
            py += start;
            px += adv;
            for _ in 0..dx - 1 {
                let mut run = whole;
                err += adj_up;
                if err > 0 {
                    run += 1;
                    err -= adj_down;
                }
                self.fill_y(px, py, run, &mut off);
                py += run;
                px += adv;
            }
            self.fill_y(px, py, end, &mut off);
        }
    }

    #[inline]
    fn pat_on(&self, off: i32) -> bool {
        (self.line_pat >> (15 - off.rem_euclid(16))) & 1 == 1
    }

    /// A horizontal run of `count` pixels (signed) from `startx` at `y`, `thick` rows
    /// tall, in the draw colour, following the line pattern via `off`.
    fn fill_x(&mut self, y: i32, startx: i32, count: i32, off: &mut i32) {
        let (sy, ey) = (y - self.thick / 2, y - self.thick / 2 + self.thick - 1);
        let mut end_x = startx + count;
        if count > 0 {
            end_x -= 1;
        } else {
            end_x += 1;
            *off -= count;
        }
        let inc = if count >= 0 { 1 } else { -1 };
        let (mut a, mut b) = (startx, end_x);
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        let c = self.color;
        for x in a..=b {
            if self.pat_on(*off) {
                for cy in sy..=ey {
                    self.put(x, cy, c);
                }
            }
            *off += inc;
        }
        if count < 0 {
            *off -= count;
        }
    }

    fn fill_y(&mut self, x: i32, starty: i32, count: i32, off: &mut i32) {
        let (sx, ex) = (x - self.thick / 2, x - self.thick / 2 + self.thick - 1);
        let mut end_y = starty + count;
        if count > 0 {
            end_y -= 1;
        } else {
            end_y += 1;
            *off -= count;
        }
        let inc = if count >= 0 { 1 } else { -1 };
        let (mut a, mut b) = (starty, end_y);
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        let c = self.color;
        for y in a..=b {
            if self.pat_on(*off) {
                for cx in sx..=ex {
                    self.put(cx, y, c);
                }
            }
            *off += inc;
        }
        if count < 0 {
            *off -= count;
        }
    }

    fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        self.line(x0, y0, x1, y0);
        self.line(x1, y0, x1, y1);
        self.line(x1, y1, x0, y1);
        self.line(x0, y1, x0, y0);
    }

    fn bar(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        let (ya, yb) = (y0.min(y1), y0.max(y1));
        for y in ya..=yb {
            self.fill_span(x0, x1, y);
        }
    }

    /// Full midpoint-ellipse outline (radii `rx`,`ry`). A circle is `rx == ry·ASPECT`.
    fn ellipse(&mut self, xc: i32, yc: i32, rx: i32, ry: i32) {
        let c = self.color;
        for (dx, dy) in ellipse_offsets(rx, ry, 0, 360) {
            self.put(xc + dx, yc + dy, c);
        }
    }

    /// Solid ellipse fill via the midpoint scan: each boundary row's min..max x is a
    /// span. Matches icy_engine's `fill_scan`, so the fill stays inside the outline.
    fn fill_ellipse(&mut self, xc: i32, yc: i32, rx: i32, ry: i32) {
        let pts = ellipse_offsets(rx, ry, 0, 360);
        let mut rows: std::collections::BTreeMap<i32, (i32, i32)> =
            std::collections::BTreeMap::new();
        for (dx, dy) in pts {
            let e = rows.entry(dy).or_insert((dx, dx));
            e.0 = e.0.min(dx);
            e.1 = e.1.max(dx);
        }
        for (dy, (mn, mx)) in rows {
            self.fill_span(xc + mn, xc + mx, yc + dy);
        }
    }

    /// Trace an elliptical arc over `a0`..`a1` degrees (0° = east, CCW) using the exact
    /// midpoint boundary. When `pie`, also draw the two radii to the arc endpoints.
    #[allow(clippy::too_many_arguments)]
    fn arc_pts(&mut self, xc: i32, yc: i32, rx: i32, ry: i32, a0: i32, a1: i32, pie: bool) {
        let c = self.color;
        for (dx, dy) in ellipse_offsets(rx, ry, a0, a1) {
            self.put(xc + dx, yc + dy, c);
        }
        if pie {
            // Radii to the exact angular endpoints (0°=east, CCW; screen y is down).
            let end = |a: i32| {
                let r = (a as f64).to_radians();
                (
                    xc + (rx as f64 * r.cos()).round() as i32,
                    yc - (ry as f64 * r.sin()).round() as i32,
                )
            };
            let (sx, sy) = end(a0);
            let (ex, ey) = end(a1);
            self.line(xc, yc, sx, sy);
            self.line(xc, yc, ex, ey);
        }
    }

    fn poly(&mut self, pts: &[u16], close: bool) {
        let v: Vec<(i32, i32)> = pts
            .chunks_exact(2)
            .map(|c| (c[0] as i32, c[1] as i32))
            .collect();
        for w in v.windows(2) {
            self.line(w[0].0, w[0].1, w[1].0, w[1].1);
        }
        if close && v.len() > 2 {
            let (a, b) = (v[v.len() - 1], v[0]);
            self.line(a.0, a.1, b.0, b.1);
        }
    }

    fn fill_poly(&mut self, pts: &[u16]) {
        let v: Vec<(i32, i32)> = pts
            .chunks_exact(2)
            .map(|c| (c[0] as i32, c[1] as i32))
            .collect();
        if v.len() < 3 {
            return;
        }
        let (ymin, ymax) = v
            .iter()
            .fold((H, 0), |(lo, hi), &(_, y)| (lo.min(y), hi.max(y)));
        for y in ymin.max(0)..=ymax.min(H - 1) {
            let mut xs: Vec<i32> = Vec::new();
            for i in 0..v.len() {
                let (x0, y0) = v[i];
                let (x1, y1) = v[(i + 1) % v.len()];
                if (y0 <= y && y1 > y) || (y1 <= y && y0 > y) {
                    xs.push(x0 + (y - y0) * (x1 - x0) / (y1 - y0));
                }
            }
            xs.sort_unstable();
            for pair in xs.chunks_exact(2) {
                self.fill_span(pair[0], pair[1], y);
            }
        }
        // BGI fill_poly borders the shape in the draw colour — but icy_engine /
        // PabloDraw *skip the border when the colour is black* (`if self.color != 0`).
        // Drawing it unconditionally painted a black contour seam around every band of
        // this dithered portrait (the bands are filled with colour 0 as the draw colour);
        // honouring the guard makes adjacent bands merge smoothly like the reference.
        if self.color != 0 {
            self.poly(pts, true);
        }
    }

    /// Cubic Bézier through 4 control points in `cnt` segments — matching icy_engine's
    /// `rip_bezier` *exactly*: the endpoints are the literal control points and the
    /// intermediate samples are **truncated** (`as i32`), not rounded. That one-pixel
    /// difference decides whether a curved outline closes; rounding here was leaving a
    /// sub-pixel seam the dragon's body fill leaked through.
    fn bezier(&mut self, p: [(i32, i32); 4], cnt: i32) {
        let cnt = cnt.max(1);
        let mut pts: Vec<(i32, i32)> = Vec::with_capacity(cnt as usize + 1);
        pts.push(p[0]);
        for step in 1..cnt {
            let tf = step as f64 / cnt as f64;
            let tr = (cnt - step) as f64 / cnt as f64;
            // Use icy_engine's exact float ops (powf, not x*x): `powf(n)` differs from
            // repeated multiplication by ~1 ULP, and that lone ULP flips the truncated
            // (`as i32`) sample at integer boundaries — the 1-px difference that left a
            // gap in garfield's paper outline (the white fill leaked through it).
            let tfs = tf.powf(2.0);
            let tfstr = tfs * tr; // t²(1-t)
            let tf_c = tf.powf(3.0); // t³
            let tr_s = tr.powf(2.0);
            let tftrs = tf * tr_s; // t(1-t)²
            let trc = tr.powf(3.0); // (1-t)³
            let f = |a: i32, b: i32, c: i32, d: i32| {
                (trc * a as f64 + 3.0 * tftrs * b as f64 + 3.0 * tfstr * c as f64 + tf_c * d as f64)
                    as i32
            };
            pts.push((
                f(p[0].0, p[1].0, p[2].0, p[3].0),
                f(p[0].1, p[1].1, p[2].1, p[3].1),
            ));
        }
        pts.push(p[3]);
        for w in pts.windows(2) {
            self.line(w[0].0, w[0].1, w[1].0, w[1].1);
        }
    }

    /// RIP boundary flood fill from (x,y): the 4-connected region whose pixels are NOT
    /// colour `border`, painted with the current fill pattern/colour. Our primitives now
    /// match icy_engine's rasterization (run-slice lines, truncated béziers, midpoint
    /// ellipses), so outlines close and fills stay put — but a residual 1-px gap in some
    /// complex art can still let a fill escape. As a **safety net we abandon any fill
    /// that escapes past half the canvas**: that bounds the worst case to a *missing
    /// region* (line art preserved) instead of a full-screen colour flood — the right
    /// failure mode for a viewer. The cost is that a genuinely huge background fill
    /// (some scenes flood >50% on purpose) is also skipped.
    fn flood(&mut self, x: i32, y: i32, border: u8) {
        if x < 0 || y < 0 || x >= W || y >= H || self.px[(y * W + x) as usize] == border {
            return;
        }
        let cap = (W * H / 2) as usize;
        let mut region: Vec<(i32, i32)> = Vec::new();
        let mut seen = vec![false; (W * H) as usize];
        let mut stack = vec![(x, y)];
        while let Some((sx, sy)) = stack.pop() {
            if seen[(sy * W + sx) as usize] {
                continue;
            }
            let mut lx = sx;
            while lx > 0
                && self.px[(sy * W + lx - 1) as usize] != border
                && !seen[(sy * W + lx - 1) as usize]
            {
                lx -= 1;
            }
            let mut rx = sx;
            while rx < W - 1
                && self.px[(sy * W + rx + 1) as usize] != border
                && !seen[(sy * W + rx + 1) as usize]
            {
                rx += 1;
            }
            for nx in lx..=rx {
                seen[(sy * W + nx) as usize] = true;
                region.push((nx, sy));
                for ny in [sy - 1, sy + 1] {
                    if (0..H).contains(&ny) {
                        let i = (ny * W + nx) as usize;
                        if !seen[i] && self.px[i] != border {
                            stack.push((nx, ny));
                        }
                    }
                }
            }
        }
        // **Leak guard (shape-based).** A residual 1px gap in some art still lets a fill
        // escape. Abandoning *every* big fill (the old `> W*H/2` rule) wrongly blanked
        // legitimate full-screen backgrounds — the common case. Instead abandon only a big
        // fill whose region is *topologically complex*: a leak escapes into a finished
        // scene and must weave around every drawn shape, exploding its perimeter, whereas
        // a real background is one solid blob. `perimeter²/area` measures exactly that — a
        // disk≈12.6, a square 16; the solid full-screen backgrounds sit at 16–20, while
        // leaks run ≈50–2185. The `> 40` cut clears every *solid* background with wide
        // margin and catches the complex leaks (GARFIELD 112, FIERO 95, MSG ≈50, …).
        // Known limits (can't be tuned away — the metric genuinely overlaps here): a
        // *complex but legit* background (HOUND's sky weaving round the dog, ≈65) is over
        // the cut and stays unfilled; a leak that floods an *empty* region stays simple
        // and slips through. Raising the cut to pass HOUND also passes the MSG/SH leaks
        // (≈50) and floods them — so those need the real fix (the outline gap closed,
        // generally, in the rasteriser; see project memory), not a threshold nudge.
        if region.len() > cap {
            let idx = |px: i32, py: i32| (py * W + px) as usize;
            let mut perim = 0usize;
            for &(px, py) in &region {
                let edge = [(px - 1, py), (px + 1, py), (px, py - 1), (px, py + 1)]
                    .iter()
                    .any(|&(a, b)| a < 0 || b < 0 || a >= W || b >= H || !seen[idx(a, b)]);
                if edge {
                    perim += 1;
                }
            }
            let p2_area = (perim as f64) * (perim as f64) / region.len() as f64;
            if p2_area > 40.0 {
                return; // complex shape → a leak; leave the outline untouched
            }
        }
        let (fc, bk) = (self.fill_color, self.bkcolor);
        for (px, py) in region {
            let row = self.fill_pat[py.rem_euclid(8) as usize];
            let on = (row >> (7 - px.rem_euclid(8))) & 1 == 1;
            self.put(px, py, if on { fc } else { bk });
        }
    }

    fn text(&mut self, mut x: i32, mut y: i32, s: &str) {
        let c = self.color;
        let sz = self.fsize.max(1);
        for ch in s.bytes() {
            let glyph = &CP437_8X8[ch as usize];
            for (gy, &bits) in glyph.iter().enumerate() {
                for gx in 0..8 {
                    if (bits >> (7 - gx)) & 1 == 1 {
                        for oy in 0..sz {
                            for ox in 0..sz {
                                let (dx, dy) = (gx * sz + ox, gy as i32 * sz + oy);
                                // direction 1 = bottom-up vertical text
                                let (px, py) = if self.fdir == 1 {
                                    (x + dy, y - dx)
                                } else {
                                    (x + dx, y + dy)
                                };
                                self.put(px, py, c);
                            }
                        }
                    }
                }
            }
            if self.fdir == 1 {
                y -= 8 * sz;
            } else {
                x += 8 * sz;
            }
        }
    }

    /// Draw `s` at (x,y) with the current RIP font — 0 = the 8×8 bitmap above, 1–10 =
    /// the BGI scalable stroke fonts (drawn as lines). Returns the x just past the text.
    fn draw_text(&mut self, x: i32, y: i32, s: &str) -> i32 {
        if let Some(f) = (self.font > 0)
            .then(|| super::rip_chr::font(self.font as usize))
            .flatten()
        {
            // Collect stroke segments first, then draw with our line() — avoids
            // borrowing `self` while `f` (a &'static font) is in use.
            let (dir, size) = (self.fdir, self.fsize as usize);
            let mut segs: Vec<(i32, i32, i32, i32)> = Vec::new();
            let adv = f.draw(s, x, y, dir, size, &mut |a, b, c, d| {
                segs.push((a, b, c, d))
            });
            // BGI stroke-font text is always drawn **thin and solid**, regardless of the
            // current line thickness/style: icy_engine (and PabloDraw) force
            // `line_thickness = 1` + `LineStyle::Solid` around the glyph strokes and
            // restore after. Without this, a preceding `LineStyle thick:3` (common right
            // before a title) renders every stroke 3px wide — the bold "shadow"/doubled
            // text seen on P1-WNC2's "Wind Ninja Chronicles" block.
            let (saved_thick, saved_pat) = (self.thick, self.line_pat);
            self.thick = 1;
            self.line_pat = 0xFFFF;
            for (a, b, c, d) in segs {
                self.line(a, b, c, d);
            }
            self.thick = saved_thick;
            self.line_pat = saved_pat;
            adv
        } else {
            self.text(x, y, s);
            if self.fdir == 1 {
                x
            } else {
                x + 8 * self.fsize.max(1) * s.len() as i32
            }
        }
    }

    /// The pixel (width, height) of `s` in the current font — for button-label layout.
    fn text_size(&self, s: &str) -> (i32, i32) {
        if self.font == 0 {
            let (w, h) = (8 * self.fsize * s.len() as i32, 8 * self.fsize);
            if self.fdir == 1 {
                (8 * self.fsize, 8 * self.fsize * s.len() as i32)
            } else {
                (w, h)
            }
        } else if let Some(f) = super::rip_chr::font(self.font as usize) {
            let w = f.draw(
                s,
                0,
                0,
                self.fdir,
                self.fsize as usize,
                &mut |_, _, _, _| {},
            );
            (w, f.scaled_height(self.fsize as usize))
        } else {
            (8 * self.fsize * s.len() as i32, 8 * self.fsize)
        }
    }

    fn hline_c(&mut self, x0: i32, x1: i32, y: i32, c: u8) {
        for x in x0.min(x1)..=x0.max(x1) {
            self.put(x, y, c);
        }
    }
    fn vline_c(&mut self, x: i32, y0: i32, y1: i32, c: u8) {
        for y in y0.min(y1)..=y0.max(y1) {
            self.put(x, y, c);
        }
    }

    /// Draw a RIP button: a beveled/recessed/chiseled panel in the current button
    /// style, with a centered (or oriented) label whose hotkey can be highlighted.
    /// Ported from icy_engine's `add_button`. Only the *visual* panel — the mouse
    /// region is irrelevant to a static viewer.
    fn draw_button(&mut self, x1: i32, y1: i32, mut x2: i32, mut y2: i32, hotkey: u8, text: &str) {
        let b = self.btn;
        if x2 == 0 {
            x2 = x1 + b.w - 1;
        }
        if y2 == 0 {
            y2 = y1 + b.h - 1;
        }
        let (bg, ch, cs, su, ul, cc, br) =
            (0u8, b.label, b.dark, b.surface, b.uline, b.corner, b.bright);

        if b.flags & 16 != 0 {
            // recessed: an outer frame (top-left dark, bottom-right bright) + inner black
            let (a, t, r, d) = (x1 - 2, y1 - 2, x2 + 2, y2 + 2);
            self.hline_c(a, r, t, cs);
            self.vline_c(a, t, d, cs);
            self.vline_c(r, t, d, br);
            self.hline_c(a, r, d, br);
            for (px, py) in [(a, t), (r, t), (a, d), (r, d)] {
                self.put(px, py, cc);
            }
            self.hline_c(a + 1, r - 1, t + 1, bg);
            self.vline_c(a + 1, t + 1, d - 1, bg);
            self.vline_c(r - 1, t + 1, d - 1, bg);
            self.hline_c(a + 1, r - 1, d - 1, bg);
        }
        if b.flags & 512 != 0 {
            // bevel special effect: `bevel` rings (bright top-left, dark bottom-right)
            for i in 1..=b.bevel {
                self.hline_c(x1 - i, x2 + i, y1 - i, br);
                self.vline_c(x1 - i, y1 - i, y2 + i, br);
                self.hline_c(x1 - i, x2 + i, y2 + i, cs);
                self.vline_c(x2 + i, y1 - i, y2 + i, cs);
                for (px, py) in [
                    (x1 - i, y1 - i),
                    (x2 + i, y1 - i),
                    (x1 - i, y2 + i),
                    (x2 + i, y2 + i),
                ] {
                    self.put(px, py, cc);
                }
            }
        }
        for y in y1..=y2 {
            self.hline_c(x1, x2, y, su); // surface fill
        }
        if b.flags & 32768 != 0 {
            // sunken: a single 3D edge
            self.hline_c(x1, x2, y1, br);
            self.vline_c(x1, y1, y2, br);
            self.hline_c(x1, x2, y2, cs);
            self.vline_c(x2, y1, y2, cs);
        }
        if b.flags & 8 != 0 {
            // chisel: an inset edge
            let (xi, yi) = chisel_inset(y2 - y1 + 1);
            self.hline_c(x1 + xi, x2 - xi, y1 + yi, br);
            self.vline_c(x1 + xi, y1 + yi, y2 - yi, br);
            self.hline_c(x1 + xi + 1, x2 - xi, y2 - yi, cs);
            self.vline_c(x2 - xi, y1 + yi + 1, y2 - yi, cs);
        }
        if !text.is_empty() {
            let (tw, th) = self.text_size(text);
            let (w, h) = (x2 - x1 + 1, y2 - y1 + 1);
            let (tx, ty) = match b.orient {
                0 => (x1 + (w - tw) / 2, y1 - th - 2),       // above
                1 => (x1 - tw - 2, y1 + (h - th) / 2),       // left
                3 => (x1 + w + 2, y1 + (h - th) / 2),        // right
                4 => (x1 + (w - tw) / 2, y1 + h + 2),        // below
                _ => (x1 + (w - tw) / 2, y1 + (h - th) / 2), // center
            };
            self.button_label(text, tx, ty, hotkey, ch, cs, ul);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn button_label(&mut self, text: &str, tx: i32, ty: i32, hotkey: u8, ch: u8, cs: u8, ul: u8) {
        let (drop, hi, under) = (
            self.btn.flags & 32 != 0,
            self.btn.flags2 & 2 != 0,
            self.btn.flags & 2048 != 0,
        );
        let old = self.color;
        if drop {
            self.color = cs;
            self.draw_text(tx + 1, ty + 1, text);
        }
        self.color = ch;
        self.draw_text(tx, ty, text);
        if hotkey != 0 && hotkey != 255 {
            let hk = (hotkey as char).to_ascii_uppercase();
            for (i, c) in text.char_indices() {
                if c.to_ascii_uppercase() == hk {
                    let (pw, _) = self.text_size(&text[..i]);
                    if hi {
                        self.color = ul;
                        self.draw_text(tx + pw, ty, &c.to_string());
                    }
                    if under {
                        let (hw, hh) = self.text_size(&c.to_string());
                        self.hline_c(tx + pw, tx + pw + hw, ty + hh + 2, ul);
                    }
                    break;
                }
            }
        }
        self.color = old;
    }
}

impl CommandSink for Rip {
    fn print(&mut self, _text: &[u8]) {} // RIP content arrives via emit_rip

    fn emit(&mut self, _cmd: icy_parser_core::TerminalCommand) {}

    fn emit_rip(&mut self, cmd: RipCommand) {
        use RipCommand::*;
        if std::env::var_os("RIP_TRACE").is_some() {
            eprintln!("{cmd:?}");
        }
        let f = |a: u16, b: u16, c: u16, d: u16| (a as i32, b as i32, c as i32, d as i32);
        match cmd {
            Color { c } => self.color = (c & 0x0f) as u8,
            WriteMode { mode } => self.xor = matches!(mode, WMode::Xor),
            Move { x, y } => {
                self.x = x as i32;
                self.y = y as i32;
            }
            Pixel { x, y } => {
                let c = self.color;
                self.put(x as i32, y as i32, c);
            }
            Line { x0, y0, x1, y1 } => {
                let (a, b, c, d) = f(x0, y0, x1, y1);
                self.line(a, b, c, d);
            }
            PolyLine { points } => self.poly(&points, false),
            Polygon { points } => self.poly(&points, true),
            FilledPolygon { points } => self.fill_poly(&points),
            Rectangle { x0, y0, x1, y1 } => {
                let (a, b, c, d) = f(x0, y0, x1, y1);
                self.rect(a, b, c, d);
            }
            Bar { x0, y0, x1, y1 } => {
                let (a, b, c, d) = f(x0, y0, x1, y1);
                self.bar(a, b, c, d);
            }
            Circle {
                x_center,
                y_center,
                radius,
            } => {
                self.ellipse(
                    x_center as i32,
                    y_center as i32,
                    radius as i32,
                    // icy's `circle()` *truncates* the aspect (only `arc()` rounds) —
                    // the 1px this saves is what seals the dragon's eye against a stray
                    // black pixel one row below the rounded ellipse's bottom.
                    (radius as f64 * ASPECT) as i32,
                );
            }
            Oval {
                x,
                y,
                st_ang: _,
                end_ang: _,
                x_rad,
                y_rad,
            } => {
                self.ellipse(x as i32, y as i32, x_rad as i32, y_rad as i32);
            }
            FilledOval { x, y, x_rad, y_rad } => {
                self.fill_ellipse(x as i32, y as i32, x_rad as i32, y_rad as i32);
                self.ellipse(x as i32, y as i32, x_rad as i32, y_rad as i32);
            }
            Arc {
                x,
                y,
                st_ang,
                end_ang,
                radius,
            } => {
                self.arc_pts(
                    x as i32,
                    y as i32,
                    radius as i32,
                    (radius as f64 * ASPECT).round() as i32,
                    st_ang as i32,
                    end_ang as i32,
                    false,
                );
            }
            OvalArc {
                x,
                y,
                st_ang,
                end_ang,
                x_rad,
                y_rad,
            } => {
                self.arc_pts(
                    x as i32,
                    y as i32,
                    x_rad as i32,
                    y_rad as i32,
                    st_ang as i32,
                    end_ang as i32,
                    false,
                );
            }
            PieSlice {
                x,
                y,
                st_ang,
                end_ang,
                radius,
            } => {
                self.arc_pts(
                    x as i32,
                    y as i32,
                    radius as i32,
                    (radius as f64 * ASPECT).round() as i32,
                    st_ang as i32,
                    end_ang as i32,
                    true,
                );
            }
            OvalPieSlice {
                x,
                y,
                st_ang,
                end_ang,
                x_rad,
                y_rad,
            } => {
                self.arc_pts(
                    x as i32,
                    y as i32,
                    x_rad as i32,
                    y_rad as i32,
                    st_ang as i32,
                    end_ang as i32,
                    true,
                );
            }
            Bezier {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
                x4,
                y4,
                cnt,
            } => {
                let p = [
                    (x1 as i32, y1 as i32),
                    (x2 as i32, y2 as i32),
                    (x3 as i32, y3 as i32),
                    (x4 as i32, y4 as i32),
                ];
                self.bezier(p, cnt as i32);
            }
            Fill { x, y, border } => self.flood(x as i32, y as i32, (border & 0x0f) as u8),
            LineStyle {
                style,
                user_pat,
                thick,
            } => {
                self.line_pat = match style {
                    LStyle::User => user_pat,
                    s => {
                        // Build the 16-bit pattern from the type's own bool table.
                        s.line_pattern()
                            .iter()
                            .take(16)
                            .fold(0u16, |a, &b| (a << 1) | b as u16)
                    }
                };
                if self.line_pat == 0 {
                    self.line_pat = 0xFFFF;
                }
                self.thick = if thick >= 3 { 3 } else { 1 };
            }
            FillStyle { pattern, color } => {
                self.fill_color = (color & 0x0f) as u8;
                self.fill_pat = pattern_bytes(pattern);
            }
            FillPattern {
                c1,
                c2,
                c3,
                c4,
                c5,
                c6,
                c7,
                c8,
                col,
            } => {
                self.fill_pat = [c1, c2, c3, c4, c5, c6, c7, c8].map(|v| v as u8);
                self.fill_color = (col & 0x0f) as u8;
            }
            SetPalette { colors } => {
                for (i, v) in colors.iter().take(16).enumerate() {
                    self.pal[i] = ega64(*v);
                }
            }
            OnePalette { color, value } => {
                if (color as usize) < 16 {
                    self.pal[color as usize] = ega64(value);
                }
            }
            ViewPort { x0, y0, x1, y1 } => {
                self.clip = (x0 as i32, y0 as i32, x1 as i32, y1 as i32);
            }
            ResetWindows | EraseWindow | EraseView => {
                let bg = self.pal[0];
                self.px.fill(0);
                self.clip = (0, 0, W - 1, H - 1);
                let _ = bg;
            }
            FontStyle {
                font,
                size,
                direction,
                ..
            } => {
                self.font = font;
                self.fsize = (size as i32).clamp(1, 10);
                self.fdir = direction;
            }
            Text { text } => {
                let (x, y) = (self.x, self.y);
                self.x = self.draw_text(x, y, &text);
            }
            TextXY { x, y, text } => {
                let (x, y) = (x as i32, y as i32);
                self.x = self.draw_text(x, y, &text);
                self.y = y;
            }
            ButtonStyle {
                wid,
                hgt,
                orient,
                flags,
                bevsize,
                dfore,
                bright,
                dark,
                surface,
                flags2,
                uline_col,
                corner_col,
                ..
            } => {
                self.btn = Btn {
                    w: wid as i32,
                    h: hgt as i32,
                    orient,
                    bevel: bevsize as i32,
                    label: (dfore & 0x0f) as u8,
                    bright: (bright & 0x0f) as u8,
                    dark: (dark & 0x0f) as u8,
                    surface: (surface & 0x0f) as u8,
                    uline: (uline_col & 0x0f) as u8,
                    corner: (corner_col & 0x0f) as u8,
                    flags: flags as i32,
                    flags2: flags2 as i32,
                };
            }
            Button {
                x0,
                y0,
                x1,
                y1,
                hotkey,
                text,
                ..
            } => {
                // Text is `<icon><>label<>host_command<>`; the label is the 2nd field.
                let parts: Vec<&str> = text.split("<>").collect();
                let label = if parts.len() >= 2 {
                    parts[1]
                } else {
                    text.as_str()
                };
                self.draw_button(
                    x0 as i32,
                    y0 as i32,
                    x1 as i32,
                    y1 as i32,
                    hotkey as u8,
                    label,
                );
            }
            GetImage { x0, y0, x1, y1, .. } => {
                self.get_image(x0 as i32, y0 as i32, x1 as i32, y1 as i32);
            }
            PutImage { x, y, mode, .. } => self.put_image(x as i32, y as i32, mode),
            _ => {} // remaining interactive / host / icon commands — parsed but not drawn
        }
    }
}

/// Button chisel inset (x,y) by button height — Borland's table (via icy_engine).
fn chisel_inset(height: i32) -> (i32, i32) {
    match height {
        ..=11 => (1, 1),
        12..=24 => (3, 2),
        25..=39 => (4, 3),
        40..=74 => (6, 5),
        75..=149 => (7, 5),
        150..=199 => (8, 6),
        200..=249 => (10, 7),
        250..=299 => (11, 8),
        _ => (13, 9),
    }
}

/// The 8-byte fill bitmap for a fill style (icy_parser_core ships the BGI table).
fn pattern_bytes(style: FStyle) -> [u8; 8] {
    let p = style.fill_pattern(&[0xFF; 8]);
    let mut out = [0xFFu8; 8];
    for (o, b) in out.iter_mut().zip(p.iter()) {
        *o = *b;
    }
    out
}

pub struct RipDecoder;

impl Decoder for RipDecoder {
    fn name(&self) -> &'static str {
        "rip"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rip"]
    }

    fn sniff(&self, header: &[u8]) -> bool {
        header.starts_with(b"!|") // RIP scenes open with the RIPscript escape
    }

    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        Ok(RipStream::new(bytes).render(bytes.len()))
    }
}

/// A RIPscript scene ready to render in full or as a byte *prefix* — the latter lets
/// the viewer "watch it draw" at a simulated baud rate, like a modem user in the 90s.
/// The canvas is the fixed 640×350 EGA screen, so prefix frames don't resize.
pub struct RipStream {
    content: Vec<u8>,
}

impl RipStream {
    pub fn new(bytes: &[u8]) -> RipStream {
        RipStream {
            content: crate::sauce::strip(bytes).to_vec(),
        }
    }

    pub fn len(&self) -> usize {
        self.content.len()
    }

    /// Replay the first `limit` bytes of RIP commands onto a fresh canvas. A partial
    /// trailing command is simply not emitted by the parser.
    pub fn render(&self, limit: usize) -> PixImage {
        let lim = limit.min(self.content.len());
        let mut rip = Rip::new();
        RipParser::new().parse(&self.content[..lim], &mut rip);
        PixImage::from_indexed(W as u32, H as u32, rip.px, rip.pal.to_vec())
    }
}

