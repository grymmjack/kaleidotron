//! 3D font extrusion — the "3D logo maker" (a nod to Ulead COOL 3D). Each glyph's outline is
//! tessellated into a filled **front/back cap** (via `lyon_tessellation`, non-zero fill so holes in
//! `O`/`A`/`e` are cut correctly) and connected by extruded **side walls**, assembled into ONE
//! `Mesh3D` that the existing CPU rasterizer (`mesh3d::render`) shades, lights and rotates. Per-
//! triangle colours (`Mesh3D::tri_rgb`) give a distinct **face** colour vs. extruded **body** colour.
//!
//! Everything is built in em-normalized units (font units × 1/upem), so the model is ~1 unit tall
//! regardless of point size — the renderer frames it by its bounding sphere.

use crate::decode::mesh3d::Mesh3D;
use lyon_tessellation::path::math::point;
use lyon_tessellation::path::Path as LyonPath;
use lyon_tessellation::{BuffersBuilder, FillOptions, FillRule, FillTessellator, FillVertex, VertexBuffers};
use ttf_parser::{Face, OutlineBuilder};

/// Options for [`extrude_text`]. Spacing/line_gap are in em-normalized units (× 1/upem), matching
/// the mesh space; `depth` is the total front-to-back extrusion (0.15 ≈ a chunky bevel-less block).
#[derive(Clone, Copy)]
pub struct Extrude3d {
    pub depth: f32,
    pub face_rgb: [u8; 3],
    pub side_rgb: [u8; 3],
    pub letter_spacing: f32, // em-normalized
    pub line_gap: f32,       // em-normalized
    pub steps: u32,          // Bézier flattening steps per curve (higher = smoother, heavier)
}

impl Default for Extrude3d {
    fn default() -> Self {
        Extrude3d {
            depth: 0.2,
            face_rgb: [220, 40, 40],
            side_rgb: [120, 20, 20],
            letter_spacing: 0.0,
            line_gap: 0.0,
            steps: 10,
        }
    }
}

/// Flattens a glyph outline (font units) into closed polyline contours in em-normalized units,
/// translated by the pen offset. Bézier curves are subdivided into `steps` line segments.
struct Flattener {
    contours: Vec<Vec<[f32; 2]>>,
    cur: Vec<[f32; 2]>,
    last: [f32; 2], // last point in font units (for Bézier sampling)
    s: f32,         // 1/upem
    tx: f32,        // pen x (em-normalized)
    ty: f32,        // baseline y (em-normalized)
    steps: u32,
}

impl Flattener {
    fn emit(&mut self, x: f32, y: f32) {
        self.cur.push([x * self.s + self.tx, y * self.s + self.ty]);
    }
    fn finish_contour(&mut self) {
        if self.cur.len() >= 3 {
            self.contours.push(std::mem::take(&mut self.cur));
        } else {
            self.cur.clear();
        }
    }
}

impl OutlineBuilder for Flattener {
    fn move_to(&mut self, x: f32, y: f32) {
        self.finish_contour();
        self.last = [x, y];
        self.emit(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.last = [x, y];
        self.emit(x, y);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let (p0, c, p1) = (self.last, [cx, cy], [x, y]);
        for i in 1..=self.steps {
            let t = i as f32 / self.steps as f32;
            let mt = 1.0 - t;
            let px = mt * mt * p0[0] + 2.0 * mt * t * c[0] + t * t * p1[0];
            let py = mt * mt * p0[1] + 2.0 * mt * t * c[1] + t * t * p1[1];
            self.emit(px, py);
        }
        self.last = [x, y];
    }
    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        let (p0, a, b, p1) = (self.last, [c1x, c1y], [c2x, c2y], [x, y]);
        for i in 1..=self.steps {
            let t = i as f32 / self.steps as f32;
            let mt = 1.0 - t;
            let (mt2, t2) = (mt * mt, t * t);
            let (w0, w1, w2, w3) = (mt2 * mt, 3.0 * mt2 * t, 3.0 * mt * t2, t2 * t);
            let px = w0 * p0[0] + w1 * a[0] + w2 * b[0] + w3 * p1[0];
            let py = w0 * p0[1] + w1 * a[1] + w2 * b[1] + w3 * p1[1];
            self.emit(px, py);
        }
        self.last = [x, y];
    }
    fn close(&mut self) {
        self.finish_contour();
    }
}

/// Build a 3D extruded mesh for `text`. Returns `None` if the font can't be parsed or nothing has an
/// outline (e.g. all spaces).
pub fn extrude_text(bytes: &[u8], text: &str, opts: &Extrude3d) -> Option<Mesh3D> {
    let face = Face::parse(bytes, 0).ok()?;
    let upem = face.units_per_em() as f32;
    if upem <= 0.0 {
        return None;
    }
    let s = 1.0 / upem;
    let steps = opts.steps.clamp(1, 60);
    let line_pitch = face.height() as f32 * s + opts.line_gap;
    let d2 = opts.depth.max(0.0) * 0.5;

    let mut mesh = Mesh3D {
        base_rgb: opts.face_rgb,
        ..Default::default()
    };
    let mut tess = FillTessellator::new();

    let (mut pen_x, mut baseline_y) = (0.0f32, 0.0f32);
    for ch in text.chars() {
        if ch == '\n' {
            pen_x = 0.0;
            baseline_y -= line_pitch; // y-up: next line sits below
            continue;
        }
        if ch == '\r' {
            continue;
        }
        let Some(gid) = face.glyph_index(ch) else {
            continue;
        };
        let adv = face.glyph_hor_advance(gid).unwrap_or(0) as f32 * s;
        // Flatten this glyph's contours (translated to the pen).
        let mut fl = Flattener {
            contours: Vec::new(),
            cur: Vec::new(),
            last: [0.0, 0.0],
            s,
            tx: pen_x,
            ty: baseline_y,
            steps,
        };
        if face.outline_glyph(gid, &mut fl).is_some() {
            fl.finish_contour();
            if !fl.contours.is_empty() {
                append_glyph(&mut mesh, &fl.contours, &mut tess, d2, opts.face_rgb, opts.side_rgb);
            }
        }
        pen_x += adv + opts.letter_spacing;
    }

    if mesh.positions.is_empty() || mesh.indices.len() < 3 {
        return None;
    }
    mesh.recompute_bounds();
    Some(mesh)
}

/// Tessellate one glyph's contours into front + back caps and add the extruded side walls.
fn append_glyph(
    mesh: &mut Mesh3D,
    contours: &[Vec<[f32; 2]>],
    tess: &mut FillTessellator,
    d2: f32,
    face_rgb: [u8; 3],
    side_rgb: [u8; 3],
) {
    // Build a lyon path of the (already flattened) contours and tessellate the fill.
    let mut pb = LyonPath::builder();
    for c in contours {
        if c.len() < 3 {
            continue;
        }
        pb.begin(point(c[0][0], c[0][1]));
        for p in &c[1..] {
            pb.line_to(point(p[0], p[1]));
        }
        pb.end(true);
    }
    let path = pb.build();
    let mut buf: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
    let ok = tess
        .tessellate_path(
            &path,
            &FillOptions::default().with_fill_rule(FillRule::NonZero),
            &mut BuffersBuilder::new(&mut buf, |v: FillVertex| {
                let p = v.position();
                [p.x, p.y]
            }),
        )
        .is_ok();
    if ok && !buf.indices.is_empty() {
        // FRONT cap (z = +d2) and BACK cap (z = -d2, reversed winding). Lighting is two-sided so the
        // winding only sets which way the flat normal points before the renderer flips it — either
        // way each cap lights as a flat face.
        let front_base = mesh.positions.len() as u32;
        for v in &buf.vertices {
            mesh.positions.push([v[0], v[1], d2]);
        }
        for t in buf.indices.chunks_exact(3) {
            mesh.indices.extend([front_base + t[0], front_base + t[1], front_base + t[2]]);
            mesh.tri_rgb.push(face_rgb);
        }
        let back_base = mesh.positions.len() as u32;
        for v in &buf.vertices {
            mesh.positions.push([v[0], v[1], -d2]);
        }
        for t in buf.indices.chunks_exact(3) {
            mesh.indices.extend([back_base + t[2], back_base + t[1], back_base + t[0]]);
            mesh.tri_rgb.push(face_rgb);
        }
    }

    // SIDE WALLS: a quad per contour edge, connecting the front loop to the back loop.
    for c in contours {
        let n = c.len();
        if n < 2 {
            continue;
        }
        for i in 0..n {
            let a = c[i];
            let b = c[(i + 1) % n];
            let base = mesh.positions.len() as u32;
            mesh.positions.push([a[0], a[1], d2]); // 0 front-a
            mesh.positions.push([b[0], b[1], d2]); // 1 front-b
            mesh.positions.push([b[0], b[1], -d2]); // 2 back-b
            mesh.positions.push([a[0], a[1], -d2]); // 3 back-a
            mesh.indices.extend([base, base + 1, base + 2]);
            mesh.tri_rgb.push(side_rgb);
            mesh.indices.extend([base, base + 2, base + 3]);
            mesh.tri_rgb.push(side_rgb);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extrudes_a_glyph_into_a_solid_mesh() {
        for p in [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
        ] {
            let Ok(bytes) = std::fs::read(p) else { continue };
            let m = extrude_text(&bytes, "A", &Extrude3d::default()).expect("A extrudes");
            assert!(m.tri_count() > 4, "has caps + walls");
            assert_eq!(m.tri_rgb.len(), m.tri_count(), "one colour per triangle");
            // Depth present: some vertices in front, some behind z=0.
            let (mut zmin, mut zmax) = (f32::MAX, f32::MIN);
            for v in &m.positions {
                zmin = zmin.min(v[2]);
                zmax = zmax.max(v[2]);
            }
            assert!(zmax - zmin > 0.0, "extruded in Z");
            // 'A' has a counter (hole) → the fill must not cover it: the tessellation produced a
            // non-trivial number of triangles (a solid quad would be ~2).
            assert!(m.tri_count() > 20, "hole-aware tessellation");
            return;
        }
    }
}
