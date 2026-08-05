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
    pub back_rgb: [u8; 3], // back-face colour (the reverse side; = face_rgb for a uniform look)
    pub side_rgb: [u8; 3],
    pub letter_spacing: f32, // em-normalized
    pub line_gap: f32,       // em-normalized
    pub steps: u32,          // Bézier flattening steps per curve (higher = smoother, heavier)
    pub bevel: f32,          // chamfer size (em-normalized; 0 = a flat block, no bevel)
}

impl Default for Extrude3d {
    fn default() -> Self {
        Extrude3d {
            depth: 0.2,
            face_rgb: [220, 40, 40],
            back_rgb: [220, 40, 40],
            side_rgb: [120, 20, 20],
            letter_spacing: 0.0,
            line_gap: 0.0,
            steps: 10,
            bevel: 0.0,
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

    let (mut pen_x, mut baseline_y, mut line) = (0.0f32, 0.0f32, 0u32);
    for ch in text.chars() {
        if ch == '\n' {
            pen_x = 0.0;
            baseline_y -= line_pitch; // y-up: next line sits below
            line += 1;
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
                // Push each successive line very slightly back in z so overlapping lines (negative
                // line-height) don't z-fight on their coplanar caps.
                let z_off = -(line as f32) * d2 * 0.04;
                let bevel = opts.bevel.max(0.0).min(d2 * 0.9);
                append_glyph(&mut mesh, &fl.contours, &mut tess, d2, z_off, bevel, opts.face_rgb, opts.back_rgb, opts.side_rgb);
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

/// Extrude arbitrary flattened 2D `contours` (already closed polylines, y-up) into a 3D mesh — the
/// same caps + walls + bevel machinery as the font extruder, but for an SVG icon/vector. The whole
/// set is filled as ONE shape (non-zero fill → holes handled across all contours). `None` if empty.
pub fn extrude_contours(contours: &[Vec<[f32; 2]>], opts: &Extrude3d) -> Option<Mesh3D> {
    if contours.is_empty() {
        return None;
    }
    let d2 = opts.depth.max(0.0) * 0.5;
    let bevel = opts.bevel.max(0.0).min(d2 * 0.9);
    let mut mesh = Mesh3D {
        base_rgb: opts.face_rgb,
        ..Default::default()
    };
    let mut tess = FillTessellator::new();
    append_glyph(&mut mesh, contours, &mut tess, d2, 0.0, bevel, opts.face_rgb, opts.back_rgb, opts.side_rgb);
    if mesh.positions.is_empty() || mesh.indices.len() < 3 {
        return None;
    }
    mesh.recompute_bounds();
    Some(mesh)
}

/// Parse an SVG's filled paths into flattened, closed polyline contours in a normalized, y-up space
/// (fit to ~1 unit tall, centred), ready for [`extrude_contours`]. Uses usvg (already in the tree
/// via resvg); Béziers are flattened to `steps` segments. `None` if the SVG has no fillable geometry.
pub fn svg_to_contours(bytes: &[u8], steps: u32) -> Option<Vec<Vec<[f32; 2]>>> {
    use resvg::tiny_skia::PathSegment;
    use resvg::usvg;
    let tree = usvg::Tree::from_data(bytes, &usvg::Options::default()).ok()?;
    let steps = steps.clamp(1, 60);

    // Recursively collect filled paths, transformed to absolute coords (SVG y-down for now).
    fn collect(group: &usvg::Group, steps: u32, out: &mut Vec<Vec<[f32; 2]>>) {
        for node in group.children() {
            match node {
                usvg::Node::Group(g) => collect(g, steps, out),
                usvg::Node::Path(p) => {
                    if p.fill().is_none() {
                        continue; // stroke-only paths have no fillable interior
                    }
                    let t = p.abs_transform();
                    let map = |x: f32, y: f32| [t.sx * x + t.kx * y + t.tx, t.ky * x + t.sy * y + t.ty];
                    let (mut cur, mut last): (Vec<[f32; 2]>, [f32; 2]) = (Vec::new(), [0.0, 0.0]);
                    let mut push = |cur: &mut Vec<[f32; 2]>, out: &mut Vec<Vec<[f32; 2]>>| {
                        if cur.len() >= 3 {
                            out.push(std::mem::take(cur));
                        } else {
                            cur.clear();
                        }
                    };
                    for seg in p.data().segments() {
                        match seg {
                            PathSegment::MoveTo(pt) => {
                                push(&mut cur, out);
                                last = [pt.x, pt.y];
                                cur.push(map(pt.x, pt.y));
                            }
                            PathSegment::LineTo(pt) => {
                                last = [pt.x, pt.y];
                                cur.push(map(pt.x, pt.y));
                            }
                            PathSegment::QuadTo(c, pt) => {
                                for i in 1..=steps {
                                    let u = i as f32 / steps as f32;
                                    let m = 1.0 - u;
                                    let x = m * m * last[0] + 2.0 * m * u * c.x + u * u * pt.x;
                                    let y = m * m * last[1] + 2.0 * m * u * c.y + u * u * pt.y;
                                    cur.push(map(x, y));
                                }
                                last = [pt.x, pt.y];
                            }
                            PathSegment::CubicTo(c1, c2, pt) => {
                                for i in 1..=steps {
                                    let u = i as f32 / steps as f32;
                                    let m = 1.0 - u;
                                    let (m2, u2) = (m * m, u * u);
                                    let (w0, w1, w2, w3) = (m2 * m, 3.0 * m2 * u, 3.0 * m * u2, u2 * u);
                                    let x = w0 * last[0] + w1 * c1.x + w2 * c2.x + w3 * pt.x;
                                    let y = w0 * last[1] + w1 * c1.y + w2 * c2.y + w3 * pt.y;
                                    cur.push(map(x, y));
                                }
                                last = [pt.x, pt.y];
                            }
                            PathSegment::Close => push(&mut cur, out),
                        }
                    }
                    push(&mut cur, out);
                }
                _ => {}
            }
        }
    }

    let mut raw: Vec<Vec<[f32; 2]>> = Vec::new();
    collect(tree.root(), steps, &mut raw);
    if raw.is_empty() {
        return None;
    }
    // Normalize: fit to ~1 unit tall, centre at the origin, flip Y (SVG y-down → mesh y-up).
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for c in &raw {
        for p in c {
            for k in 0..2 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
    }
    let (w, h) = (hi[0] - lo[0], hi[1] - lo[1]);
    let s = 1.0 / h.max(1e-4);
    let (cx, cy) = ((lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5);
    let _ = w;
    for c in &mut raw {
        for p in c {
            p[0] = (p[0] - cx) * s;
            p[1] = -(p[1] - cy) * s; // flip Y
        }
    }
    Some(raw)
}

/// Fill the (already flattened) contours into 2D cap triangles via lyon (non-zero → holes cut).
fn fill_caps(contours: &[Vec<[f32; 2]>], tess: &mut FillTessellator) -> VertexBuffers<[f32; 2], u32> {
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
    let _ = tess.tessellate_path(
        &path,
        &FillOptions::default().with_fill_rule(FillRule::NonZero),
        &mut BuffersBuilder::new(&mut buf, |v: FillVertex| {
            let p = v.position();
            [p.x, p.y]
        }),
    );
    buf
}

/// Signed area of a polygon (shoelace); >0 = CCW.
fn signed_area(c: &[[f32; 2]]) -> f32 {
    let n = c.len();
    let mut a = 0.0;
    for i in 0..n {
        let p = c[i];
        let q = c[(i + 1) % n];
        a += p[0] * q[1] - q[0] * p[1];
    }
    a * 0.5
}

/// Inset every contour toward the FILLED side by `amount` (an approximate inward polygon offset via
/// the per-vertex angle bisector of the adjacent edges' left-normals). Vertex count is preserved so
/// the bevel chamfer can connect inset↔outline 1:1. Small `amount` only — a large inset self-
/// intersects on thin glyph features (the caller clamps it).
fn inset_contours(contours: &[Vec<[f32; 2]>], amount: f32) -> Vec<Vec<[f32; 2]>> {
    // The filled side is consistent with the outer contour's winding: offset toward the left when the
    // outer is CCW, toward the right when CW (holes are wound oppositely, so the same signed step
    // shrinks the solid for both).
    let outer = contours.iter().map(|c| signed_area(c)).fold(0.0f32, |m, a| if a.abs() > m.abs() { a } else { m });
    let side = if outer >= 0.0 { 1.0 } else { -1.0 };
    let left_normal = |a: [f32; 2], b: [f32; 2]| -> [f32; 2] {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = (dx * dx + dy * dy).sqrt().max(1e-6);
        [-dy / len, dx / len] // rotate edge +90°
    };
    contours
        .iter()
        .map(|c| {
            let n = c.len();
            (0..n)
                .map(|i| {
                    let prev = c[(i + n - 1) % n];
                    let cur = c[i];
                    let next = c[(i + 1) % n];
                    let n1 = left_normal(prev, cur);
                    let n2 = left_normal(cur, next);
                    let mut d = [n1[0] + n2[0], n1[1] + n2[1]];
                    let len = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1e-6);
                    d = [d[0] / len, d[1] / len];
                    [cur[0] + side * d[0] * amount, cur[1] + side * d[1] * amount]
                })
                .collect()
        })
        .collect()
}

/// Push cap triangles (from `fill_caps`) at plane `z` into the mesh; `flip` reverses winding.
fn push_cap(mesh: &mut Mesh3D, buf: &VertexBuffers<[f32; 2], u32>, z: f32, flip: bool, rgb: [u8; 3]) {
    let base = mesh.positions.len() as u32;
    for v in &buf.vertices {
        mesh.positions.push([v[0], v[1], z]);
    }
    for t in buf.indices.chunks_exact(3) {
        if flip {
            mesh.indices.extend([base + t[2], base + t[1], base + t[0]]);
        } else {
            mesh.indices.extend([base + t[0], base + t[1], base + t[2]]);
        }
        mesh.tri_rgb.push(rgb);
    }
}

/// A quad strip between two matching contours `lo`/`hi` (same vertex count) at planes `z_lo`/`z_hi`.
fn push_wall(mesh: &mut Mesh3D, lo: &[[f32; 2]], z_lo: f32, hi: &[[f32; 2]], z_hi: f32, rgb: [u8; 3]) {
    let n = lo.len().min(hi.len());
    if n < 2 {
        return;
    }
    for i in 0..n {
        let j = (i + 1) % n;
        let base = mesh.positions.len() as u32;
        mesh.positions.push([hi[i][0], hi[i][1], z_hi]);
        mesh.positions.push([hi[j][0], hi[j][1], z_hi]);
        mesh.positions.push([lo[j][0], lo[j][1], z_lo]);
        mesh.positions.push([lo[i][0], lo[i][1], z_lo]);
        mesh.indices.extend([base, base + 1, base + 2]);
        mesh.tri_rgb.push(rgb);
        mesh.indices.extend([base, base + 2, base + 3]);
        mesh.tri_rgb.push(rgb);
    }
}

/// Tessellate one glyph's contours into caps + extruded walls (with an optional chamfer bevel).
#[allow(clippy::too_many_arguments)]
fn append_glyph(
    mesh: &mut Mesh3D,
    contours: &[Vec<[f32; 2]>],
    tess: &mut FillTessellator,
    d2: f32,
    z_off: f32,
    bevel: f32,
    face_rgb: [u8; 3],
    back_rgb: [u8; 3],
    side_rgb: [u8; 3],
) {
    let (zf, zb) = (d2 + z_off, -d2 + z_off);
    if bevel <= 1e-4 {
        // Flat block: full-outline caps + straight side walls.
        let buf = fill_caps(contours, tess);
        if !buf.indices.is_empty() {
            push_cap(mesh, &buf, zf, false, face_rgb); // front
            push_cap(mesh, &buf, zb, true, back_rgb); // back (reversed)
        }
        for c in contours {
            push_wall(mesh, c, zb, c, zf, side_rgb);
        }
        return;
    }
    // Beveled: the flat top/bottom faces sit on the INSET outline; a chamfer ramps out to the full
    // outline, then the straight side wall, then a chamfer back in. Face/back-coloured chamfers catch
    // the light for the classic beveled look; side walls take the body colour.
    let inner = inset_contours(contours, bevel);
    let buf = fill_caps(&inner, tess);
    if !buf.indices.is_empty() {
        push_cap(mesh, &buf, zf, false, face_rgb); // front face (inset)
        push_cap(mesh, &buf, zb, true, back_rgb); // back face (inset)
    }
    let (zfc, zbc) = (zf - bevel, zb + bevel); // chamfer bottoms
    for (o, i) in contours.iter().zip(inner.iter()) {
        push_wall(mesh, o, zfc, i, zf, face_rgb); // front chamfer (outline@zfc → inset@zf)
        push_wall(mesh, o, zbc, o, zfc, side_rgb); // straight side wall
        push_wall(mesh, i, zb, o, zbc, back_rgb); // back chamfer (inset@zb → outline@zbc)
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

    #[test]
    fn svg_snapshot_is_valid_and_depth_sorted() {
        use crate::decode::mesh3d::{to_svg, Camera, RenderOpts, View};
        for p in [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
        ] {
            let Ok(bytes) = std::fs::read(p) else { continue };
            let m = extrude_text(&bytes, "A", &Extrude3d::default()).unwrap();
            let cam = Camera { yaw: 0.5, pitch: -0.45, zoom: 1.0, pan: [0.0, 0.0] };
            let svg = to_svg(&m, 400, 300, &View::Orbit(cam), &RenderOpts::default());
            assert!(svg.contains("<polygon"), "emits polygons");
            // One polygon per front-facing-projected triangle (all front in orbit) → many.
            assert!(svg.matches("<polygon").count() > 20, "depth-sorted facets");
            assert!(
                resvg::usvg::Tree::from_data(svg.as_bytes(), &resvg::usvg::Options::default()).is_ok(),
                "valid SVG"
            );
            return;
        }
    }

    #[test]
    fn bevel_adds_chamfer_geometry() {
        for p in [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
        ] {
            let Ok(bytes) = std::fs::read(p) else { continue };
            let flat = extrude_text(&bytes, "R", &Extrude3d { depth: 0.2, bevel: 0.0, ..Default::default() }).unwrap();
            let beveled = extrude_text(&bytes, "R", &Extrude3d { depth: 0.2, bevel: 0.03, ..Default::default() }).unwrap();
            // The chamfer adds two extra wall rings per contour → materially more triangles.
            assert!(beveled.tri_count() > flat.tri_count() + 10, "bevel adds chamfer strips");
            assert_eq!(beveled.tri_rgb.len(), beveled.tri_count());
            // Signed area sanity: a CCW and a CW ring give opposite signs.
            let sq_ccw = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
            assert!(signed_area(&sq_ccw) > 0.0);
            return;
        }
    }
}


#[cfg(test)]
mod svg3d {
    use super::*;
    use crate::decode::mesh3d::{render, Camera, RenderOpts, View};
    #[test]
    fn extrudes_an_svg_with_holes() {
        // A filled square with a circular hole (even-odd) → the hole must survive tessellation.
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><path fill="#000" fill-rule="evenodd" d="M10 10 H90 V90 H10 Z M50 30 A20 20 0 1 0 50 70 A20 20 0 1 0 50 30 Z"/></svg>"##;
        let contours = svg_to_contours(svg, 8).expect("svg parses to contours");
        assert!(contours.len() >= 2, "outer square + hole circle");
        let m = extrude_contours(&contours, &Extrude3d { depth: 0.25, ..Default::default() }).unwrap();
        assert!(m.tri_count() > 8);
        // Y was flipped + normalized to ~1 unit tall, centred.
        let (mut ymin, mut ymax) = (f32::MAX, f32::MIN);
        for v in &m.positions { ymin = ymin.min(v[1]); ymax = ymax.max(v[1]); }
        assert!((ymax - ymin - 0.8).abs() < 0.3, "normalized height ~1 (square is 80/100)");
    }
    #[test]
    #[ignore]
    fn dump_svg_3d() {
        let Ok(svg) = std::fs::read("/tmp/heart.svg") else { return };
        let contours = svg_to_contours(&svg, 12).expect("heart contours");
        eprintln!("heart: {} contours", contours.len());
        let opts = Extrude3d { depth: 0.3, face_rgb: [230,60,70], back_rgb: [180,40,50], side_rgb: [120,20,30], bevel: 0.02, ..Default::default() };
        let m = extrude_contours(&contours, &opts).unwrap();
        let cam = Camera { yaw: 0.5, pitch: -0.45, zoom: 1.0, pan: [0.0,0.0] };
        let ro = RenderOpts { bg: [28,28,32,255], light_yaw: 0.5, light_pitch: 0.7, ..Default::default() };
        let (w,h) = (600usize, 500usize);
        let px = render(&m, w, h, &View::Orbit(cam), &ro);
        let flat: Vec<u8> = px.iter().flat_map(|c| *c).collect();
        image::save_buffer("/tmp/svg3d.png", &flat, w as u32, h as u32, image::ColorType::Rgba8).unwrap();
        eprintln!("{} tris; wrote /tmp/svg3d.png", m.tri_count());
    }
}
