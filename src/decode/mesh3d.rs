//! 3D model loading + a CPU software rasterizer (the "3d" format plugin).
//!
//! We load *geometry* from OBJ / STL / COLLADA (via `mesh-loader`, which also resolves an
//! OBJ's `.mtl` relative to the file path), PLY (a hand-rolled parser — `mesh-loader` 0.1
//! has none), and glTF 2.0 / GLB (via `gltf`, embedded or external `.bin` buffers).
//! Everything is flattened into one [`Mesh3D`] (positions + triangle indices + UVs +
//! optional diffuse texture + a bounding sphere).
//!
//! The **thumbnail** is drawn here on the CPU — a z-buffered software render — because the
//! thumbnailer runs off the UI thread where no GPU context is available (the same reason
//! `thumb.rs` workers only ever produce CPU RGBA). The interactive viewport reuses the same
//! [`Mesh3D`] and [`render`] so the two always agree; `render` handles both the orbit (ortho)
//! and free-fly (perspective) cameras, a flat or textured base with an optional wireframe
//! overlay, and view-space lighting.
//!
//! This is the codebase's usual "our own renderer" pattern (see RIP / ANSI): lean and testable
//! headless. Not full PBR — geometry + one diffuse map + a view-space key light.

use super::{DecodeError, Decoder};
use crate::image_types::PixImage;
use std::path::Path;

/// Extensions handled by the 3D plugin. Routed by extension in `decode_bytes` (like
/// `code`/`audio`) because the loaders need the *path*, not just the bytes.
pub const MESH_EXTS: &[&str] = &["obj", "stl", "ply", "gltf", "glb", "dae"];

/// A decoded diffuse texture (the material's `map_Kd` / glTF base-colour image), sampled
/// per-pixel in `ShadeMode::Textured`.
#[derive(Clone)]
pub struct Texture {
    pub px: Vec<[u8; 4]>,
    pub w: usize,
    pub h: usize,
}

impl Texture {
    /// Nearest-neighbour sample at UV (wraps; V flipped — OBJ/glTF UV origin is bottom-left).
    fn sample(&self, u: f32, v: f32) -> [u8; 4] {
        let uu = u - u.floor();
        let vv = 1.0 - (v - v.floor());
        let x = ((uu * self.w as f32) as usize).min(self.w.saturating_sub(1));
        let y = ((vv * self.h as f32) as usize).min(self.h.saturating_sub(1));
        self.px[y * self.w + x]
    }
}

/// A flattened, triangulated mesh + its bounding sphere (for framing the camera). Carries
/// per-vertex UVs + an optional diffuse texture so `ShadeMode::Textured` can map the atlas.
#[derive(Clone, Default)]
pub struct Mesh3D {
    pub positions: Vec<[f32; 3]>,
    pub indices: Vec<u32>,        // triangle list (3 per face)
    pub texcoords: Vec<[f32; 2]>, // parallel to positions ([] = untextured → falls back to solid)
    pub texture: Option<Texture>, // the material's diffuse map (map_Kd / base colour)
    pub base_rgb: [u8; 3],        // material diffuse colour (Kd) for solid shading
    pub tri_rgb: Vec<[u8; 3]>,    // optional per-TRIANGLE colour (len = tri_count); [] = use base_rgb.
    // Lets one mesh carry a face colour vs. an extruded-side colour (the 3D font maker).
    pub center: [f32; 3],
    pub radius: f32,
}

impl Mesh3D {
    pub fn tri_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Recompute `center` + `radius` from the current positions (bounding-sphere-ish:
    /// AABB centre, then the farthest vertex distance — a safe over-estimate for framing).
    pub fn recompute_bounds(&mut self) {
        if self.positions.is_empty() {
            self.center = [0.0; 3];
            self.radius = 1.0;
            return;
        }
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for p in &self.positions {
            for i in 0..3 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
        }
        self.center = [
            (lo[0] + hi[0]) * 0.5,
            (lo[1] + hi[1]) * 0.5,
            (lo[2] + hi[2]) * 0.5,
        ];
        let mut r2 = 0.0f32;
        for p in &self.positions {
            let d = [
                p[0] - self.center[0],
                p[1] - self.center[1],
                p[2] - self.center[2],
            ];
            r2 = r2.max(d[0] * d[0] + d[1] * d[1] + d[2] * d[2]);
        }
        self.radius = r2.sqrt().max(1e-4);
    }
}

/// Load a 3D model from disk. `None` on any failure (unreadable / unsupported / empty),
/// so the caller falls back to a placeholder. Dispatch is by extension.
pub fn load(path: &Path) -> Option<Mesh3D> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut mesh = match ext.as_str() {
        "gltf" | "glb" => load_gltf(path)?,
        "ply" => load_ply(path)?, // hand-rolled — mesh-loader 0.1 has no PLY parser
        _ => load_via_mesh_loader(path)?, // obj / stl / dae
    };
    if mesh.positions.is_empty() || mesh.indices.len() < 3 {
        return None;
    }
    if mesh.base_rgb == [0, 0, 0] {
        mesh.base_rgb = [150, 170, 200]; // default cool-grey when the file has no material colour
    }
    mesh.recompute_bounds();
    Some(mesh)
}

/// OBJ / STL / COLLADA via `mesh-loader` (PLY is `load_ply`). Sub-meshes are merged into one
/// index buffer; per-vertex UVs kept parallel to positions; the first material's diffuse
/// colour + `map_Kd` texture (resolved relative to the model file) captured for shading.
fn load_via_mesh_loader(path: &Path) -> Option<Mesh3D> {
    let scene = mesh_loader::Loader::default().load(path).ok()?;
    let mut out = Mesh3D::default();
    for m in &scene.meshes {
        let base = out.positions.len() as u32;
        out.positions
            .extend(m.vertices.iter().map(|v| [v[0], v[1], v[2]]));
        // Keep texcoords aligned with positions (placeholder for a sub-mesh without UVs).
        if m.texcoords[0].len() == m.vertices.len() {
            out.texcoords.extend(m.texcoords[0].iter().copied());
        } else {
            out.texcoords
                .extend(std::iter::repeat_n([0.0, 0.0], m.vertices.len()));
        }
        for f in &m.faces {
            out.indices.push(base + f[0]);
            out.indices.push(base + f[1]);
            out.indices.push(base + f[2]);
        }
    }
    // First material's diffuse colour (Kd) + texture (map_Kd), resolved against the dir.
    let dir = path.parent().unwrap_or(Path::new("."));
    for mat in &scene.materials {
        if out.base_rgb == [0, 0, 0] {
            if let Some(c) = mat.color.diffuse {
                out.base_rgb = [
                    (c[0].clamp(0.0, 1.0) * 255.0) as u8,
                    (c[1].clamp(0.0, 1.0) * 255.0) as u8,
                    (c[2].clamp(0.0, 1.0) * 255.0) as u8,
                ];
            }
        }
        if out.texture.is_none() {
            if let Some(tp) = &mat.texture.diffuse {
                let full = if tp.is_absolute() {
                    tp.clone()
                } else {
                    dir.join(tp)
                };
                out.texture = load_texture(&full);
            }
        }
    }
    Some(out)
}

/// Hand-rolled PLY loader (Stanford Polygon) — `mesh-loader` 0.1 has no PLY parser, and
/// Blender exports PLY by default (3D scanning/printing too). Handles ASCII + binary
/// little/big-endian, arbitrary vertex properties (reads x/y/z + s,t/u,v texcoords, skips
/// the rest), and a triangulated face list (`property list <c> <i> vertex_indices`).
fn load_ply(path: &Path) -> Option<Mesh3D> {
    let bytes = std::fs::read(path).ok()?;
    if !bytes.starts_with(b"ply") {
        return None;
    }
    // Header ends at "end_header\n"; parse the ASCII header lines.
    let hdr_end = find_subslice(&bytes, b"end_header")? + "end_header".len();
    // Skip the trailing newline(s) after end_header.
    let mut body = hdr_end;
    while body < bytes.len() && (bytes[body] == b'\n' || bytes[body] == b'\r') {
        body += 1;
    }
    let header = std::str::from_utf8(&bytes[..hdr_end]).ok()?;

    #[derive(Clone)]
    struct Prop {
        name: String,
        ty: String,                     // scalar type, or "list" for a list property
        list: Option<(String, String)>, // (count type, item type)
    }
    struct Elem {
        name: String,
        count: usize,
        props: Vec<Prop>,
    }
    let mut format = String::new();
    let mut elems: Vec<Elem> = Vec::new();
    for line in header.lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        match t.as_slice() {
            ["format", f, ..] => format = f.to_string(),
            ["element", name, n] => elems.push(Elem {
                name: name.to_string(),
                count: n.parse().ok()?,
                props: Vec::new(),
            }),
            ["property", "list", ct, it, name] => {
                if let Some(e) = elems.last_mut() {
                    e.props.push(Prop {
                        name: name.to_string(),
                        ty: "list".into(),
                        list: Some((ct.to_string(), it.to_string())),
                    });
                }
            }
            ["property", ty, name] => {
                if let Some(e) = elems.last_mut() {
                    e.props.push(Prop {
                        name: name.to_string(),
                        ty: ty.to_string(),
                        list: None,
                    });
                }
            }
            _ => {}
        }
    }
    let ascii = format.starts_with("ascii");
    let big_endian = format.contains("big_endian");
    let mut out = Mesh3D::default();

    if ascii {
        // Whitespace-tokenise the whole body; consume per element/property.
        let text = std::str::from_utf8(&bytes[body..]).ok()?;
        let mut it = text.split_whitespace();
        for e in &elems {
            let is_vertex = e.name == "vertex";
            for _ in 0..e.count {
                let mut xyz = [0.0f32; 3];
                let mut uv = [0.0f32; 2];
                let mut have_uv = false;
                for p in &e.props {
                    if p.ty == "list" {
                        let n: usize = it.next()?.parse().ok()?;
                        let idx: Vec<u32> = (0..n)
                            .map(|_| it.next().unwrap_or("0").parse().unwrap_or(0))
                            .collect();
                        for k in 1..idx.len().saturating_sub(1) {
                            out.indices.extend([idx[0], idx[k], idx[k + 1]]); // fan-triangulate
                        }
                    } else {
                        let v: f64 = it.next()?.parse().ok()?;
                        match p.name.as_str() {
                            "x" => xyz[0] = v as f32,
                            "y" => xyz[1] = v as f32,
                            "z" => xyz[2] = v as f32,
                            "s" | "u" | "texture_u" => {
                                uv[0] = v as f32;
                                have_uv = true;
                            }
                            "t" | "v" | "texture_v" => {
                                uv[1] = v as f32;
                                have_uv = true;
                            }
                            _ => {}
                        }
                    }
                }
                if is_vertex {
                    out.positions.push(xyz);
                    out.texcoords.push(if have_uv { uv } else { [0.0, 0.0] });
                }
            }
        }
    } else {
        // Binary: walk the byte stream honouring each property's size.
        let mut pos = body;
        let read = |b: &[u8], p: &mut usize, ty: &str| -> Option<f64> {
            let sz = ply_type_size(ty)?;
            if *p + sz > b.len() {
                return None;
            }
            let s = &b[*p..*p + sz];
            *p += sz;
            Some(ply_read_scalar(s, ty, big_endian))
        };
        for e in &elems {
            let is_vertex = e.name == "vertex";
            for _ in 0..e.count {
                let mut xyz = [0.0f32; 3];
                let mut uv = [0.0f32; 2];
                let mut have_uv = false;
                for p in &e.props {
                    if let Some((ct, it)) = &p.list {
                        let n = read(&bytes, &mut pos, ct)? as usize;
                        let mut idx = Vec::with_capacity(n);
                        for _ in 0..n {
                            idx.push(read(&bytes, &mut pos, it)? as u32);
                        }
                        for k in 1..idx.len().saturating_sub(1) {
                            out.indices.extend([idx[0], idx[k], idx[k + 1]]);
                        }
                    } else {
                        let v = read(&bytes, &mut pos, &p.ty)?;
                        match p.name.as_str() {
                            "x" => xyz[0] = v as f32,
                            "y" => xyz[1] = v as f32,
                            "z" => xyz[2] = v as f32,
                            "s" | "u" | "texture_u" => {
                                uv[0] = v as f32;
                                have_uv = true;
                            }
                            "t" | "v" | "texture_v" => {
                                uv[1] = v as f32;
                                have_uv = true;
                            }
                            _ => {}
                        }
                    }
                }
                if is_vertex {
                    out.positions.push(xyz);
                    out.texcoords.push(if have_uv { uv } else { [0.0, 0.0] });
                }
            }
        }
    }
    (!out.positions.is_empty() && out.indices.len() >= 3).then_some(out)
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}
fn ply_type_size(ty: &str) -> Option<usize> {
    Some(match ty {
        "char" | "uchar" | "int8" | "uint8" => 1,
        "short" | "ushort" | "int16" | "uint16" => 2,
        "int" | "uint" | "int32" | "uint32" | "float" | "float32" => 4,
        "double" | "float64" => 8,
        _ => return None,
    })
}
fn ply_read_scalar(s: &[u8], ty: &str, be: bool) -> f64 {
    macro_rules! rd {
        ($t:ty) => {{
            const N: usize = std::mem::size_of::<$t>();
            let mut b = [0u8; N];
            b.copy_from_slice(&s[..N]);
            (if be {
                <$t>::from_be_bytes(b)
            } else {
                <$t>::from_le_bytes(b)
            }) as f64
        }};
    }
    match ty {
        "char" | "int8" => (s[0] as i8) as f64,
        "uchar" | "uint8" => s[0] as f64,
        "short" | "int16" => rd!(i16),
        "ushort" | "uint16" => rd!(u16),
        "int" | "int32" => rd!(i32),
        "uint" | "uint32" => rd!(u32),
        "float" | "float32" => rd!(f32),
        "double" | "float64" => rd!(f64),
        _ => 0.0,
    }
}

/// Load an image file into a [`Texture`] (RGBA). `None` on any failure.
fn load_texture(path: &Path) -> Option<Texture> {
    let img = image::open(path).ok()?.to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let px = img.pixels().map(|p| p.0).collect();
    Some(Texture { px, w, h })
}

/// glTF 2.0 / GLB via the `gltf` crate. Walks every primitive of every mesh, reading
/// positions + (triangulated) indices through the primitive reader. Non-triangle
/// primitives are skipped. Node transforms are not applied (single-object models — the
/// common case for a viewer — render correctly; a rare multi-node scene may be unposed).
fn load_gltf(path: &Path) -> Option<Mesh3D> {
    let (doc, buffers, images) = gltf::import(path).ok()?;
    let mut out = Mesh3D::default();
    for mesh in doc.meshes() {
        for prim in mesh.primitives() {
            if prim.mode() != gltf::mesh::Mode::Triangles {
                continue;
            }
            let reader = prim.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
            let Some(pos) = reader.read_positions() else {
                continue;
            };
            let base = out.positions.len() as u32;
            let before = out.positions.len();
            out.positions.extend(pos);
            let added = out.positions.len() - before;
            // UVs (set 0), kept parallel to positions.
            match reader.read_tex_coords(0) {
                Some(uv) => out.texcoords.extend(uv.into_f32()),
                None => out.texcoords.extend(std::iter::repeat_n([0.0, 0.0], added)),
            }
            match reader.read_indices() {
                Some(idx) => out.indices.extend(idx.into_u32().map(|i| base + i)),
                None => {
                    let n = out.positions.len() as u32;
                    out.indices.extend(base..n);
                }
            }
            // Material base colour (factor) + base-colour texture (first primitive wins).
            let pbr = prim.material().pbr_metallic_roughness();
            if out.base_rgb == [0, 0, 0] {
                let c = pbr.base_color_factor();
                out.base_rgb = [
                    (c[0].clamp(0.0, 1.0) * 255.0) as u8,
                    (c[1].clamp(0.0, 1.0) * 255.0) as u8,
                    (c[2].clamp(0.0, 1.0) * 255.0) as u8,
                ];
            }
            if out.texture.is_none() {
                if let Some(info) = pbr.base_color_texture() {
                    let src = info.texture().source().index();
                    out.texture = images.get(src).and_then(gltf_image_to_texture);
                }
            }
        }
    }
    Some(out)
}

/// Convert a glTF-decoded image to our RGBA [`Texture`] (handles the common RGB8/RGBA8).
fn gltf_image_to_texture(img: &gltf::image::Data) -> Option<Texture> {
    use gltf::image::Format;
    let (w, h) = (img.width as usize, img.height as usize);
    let px: Vec<[u8; 4]> = match img.format {
        Format::R8G8B8A8 => img
            .pixels
            .chunks_exact(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect(),
        Format::R8G8B8 => img
            .pixels
            .chunks_exact(3)
            .map(|c| [c[0], c[1], c[2], 255])
            .collect(),
        _ => return None, // uncommon formats (indexed / 16-bit) — skip, fall back to solid
    };
    (px.len() == w * h).then_some(Texture { px, w, h })
}

/// Orbit camera + framing for a render. `yaw`/`pitch` are radians; `zoom` scales the
/// fit (1 = whole model fits); `pan` shifts the image in fractions of its size.
#[derive(Clone, Copy)]
pub struct Camera {
    pub yaw: f32,
    pub pitch: f32,
    pub zoom: f32,
    pub pan: [f32; 2],
}

impl Default for Camera {
    fn default() -> Self {
        // A gentle 3/4 view — the standard "hero" angle for a model thumbnail.
        Camera {
            yaw: 0.6,
            pitch: -0.5,
            zoom: 1.0,
            pan: [0.0, 0.0],
        }
    }
}

/// A free-fly first-person camera (Blender walk/fly mode): a position `eye` + a look
/// direction (`yaw` around world-Y, `pitch` up/down). Perspective. WASD/QE move `eye`.
#[derive(Clone, Copy)]
pub struct FlyCam {
    pub eye: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
}

impl FlyCam {
    /// Unit look direction from yaw/pitch (yaw=0,pitch=0 → +Z).
    pub fn forward(&self) -> [f32; 3] {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        [cp * sy, sp, cp * cy]
    }
    /// Right vector (world-up cross forward), for strafing.
    pub fn right(&self) -> [f32; 3] {
        normalize(cross([0.0, 1.0, 0.0], self.forward()))
    }
    /// Seed a fly camera from the current orbit view so entering FPS mode doesn't jump:
    /// place `eye` back along the orbit look direction, framing the whole model.
    pub fn from_orbit(cam: &Camera, center: [f32; 3], radius: f32) -> Self {
        // Orbit rotates the mesh by R = Rx(pitch)Ry(yaw) and looks down -Z, so the camera
        // sits at R⁻¹·[0,0,dist] from centre; the look dir is toward the centre.
        let dist = (radius * 3.0 / cam.zoom.max(0.15)).max(radius * 1.2);
        let (sy, cy) = cam.yaw.sin_cos();
        let (sp, cp) = cam.pitch.sin_cos();
        // R⁻¹ = Ry(-yaw)·Rx(-pitch) applied to [0,0,dist].
        let v = [0.0, 0.0, dist];
        let (x1, y1, z1) = (v[0], cp * v[1] + sp * v[2], -sp * v[1] + cp * v[2]); // Rx(-pitch)
        let off = [cy * x1 + sy * z1, y1, -sy * x1 + cy * z1]; // Ry(-yaw)
        let eye = [center[0] + off[0], center[1] + off[1], center[2] + off[2]];
        let fwd = normalize([-off[0], -off[1], -off[2]]); // look toward centre
        let pitch = fwd[1].clamp(-0.999, 0.999).asin();
        let yaw = fwd[0].atan2(fwd[2]);
        FlyCam { eye, yaw, pitch }
    }
}

/// Which camera drives a render.
pub enum View {
    Orbit(Camera),
    Fly(FlyCam),
}

/// Scene/look options for a render — the "scene setup" controls. `textured` picks the base
/// surface (diffuse map vs flat material colour); `wireframe` is an independent **overlay**
/// (hidden-line edges drawn ON TOP), so it composes with either base. Light direction is in
/// **view space** (screen-relative), so it reads like a studio key light however the model
/// is turned.
#[derive(Clone, Copy)]
pub struct RenderOpts {
    pub textured: bool, // sample the diffuse map (falls back to flat if none / no UVs)
    pub wireframe: bool, // overlay hidden-line edges on top of the shaded surface
    pub wire_color: [u8; 3], // wireframe line colour
    pub light_yaw: f32, // light azimuth (view space)
    pub light_pitch: f32, // light elevation
    pub light_rgb: [u8; 3], // key-light colour (tints the diffuse term; white = neutral)
    pub bg: [u8; 4],
}

impl Default for RenderOpts {
    fn default() -> Self {
        RenderOpts {
            textured: false,
            wireframe: false,
            wire_color: [30, 32, 38],
            light_yaw: 0.4,
            light_pitch: 0.6,
            light_rgb: [255, 255, 255],
            bg: [24, 24, 28, 255],
        }
    }
}

/// Flat two-sided diffuse shade of a triangle: `base` × (ambient + diffuse·light_colour). Shared by
/// the rasterizer and the SVG exporter so they agree. `nv` = view-space face normal (pre-flip).
fn shade_tri(base: [u8; 3], nv: [f32; 3], light: [f32; 3], light_rgb: [u8; 3]) -> [u8; 3] {
    let mut n = nv;
    if n[2] < 0.0 {
        n = [-n[0], -n[1], -n[2]];
    }
    let ndl = dot(n, light).max(0.0);
    let (amb, dif) = (0.28f32, 0.72 * ndl);
    let mut out = [0u8; 3];
    for c in 0..3 {
        let lc = light_rgb[c] as f32 / 255.0;
        out[c] = (base[c] as f32 * (amb + dif * lc)).clamp(0.0, 255.0) as u8;
    }
    out
}

// Per-vertex projected data shared by the rasterizer (screen pos, depth, perspective w, u/w, v/w).
#[derive(Clone, Copy)]
struct SVert {
    x: f32,
    y: f32,
    depth: f32,  // larger = nearer (z-test)
    wp: f32,     // perspective weight (1 for ortho, 1/z for perspective)
    uz: f32,     // u * wp
    vz: f32,     // v * wp
    front: bool, // in front of the near plane (perspective cull)
}

/// Software-render `mesh` to an RGBA buffer (`w`×`h`) from `view` with `opts`. Z-buffered,
/// two-sided view-space diffuse lighting; flat or textured base + an optional hidden-line
/// wireframe overlay. Pure + device-free ⇒ testable headless. Used by both the thumbnail
/// and the interactive viewport.
pub fn render(mesh: &Mesh3D, w: usize, h: usize, view: &View, opts: &RenderOpts) -> Vec<[u8; 4]> {
    let mut color = vec![opts.bg; w * h];
    if w == 0 || h == 0 || mesh.positions.is_empty() {
        return color;
    }
    let mut depth = vec![f32::MIN; w * h];
    let (cx, cyc) = (w as f32 * 0.5, h as f32 * 0.5);
    let r = mesh.radius.max(1e-4);
    let uv_of = |i: usize| mesh.texcoords.get(i).copied().unwrap_or([0.0, 0.0]);

    // View-space light + a per-view "world normal → view normal" closure (for lighting).
    let light = {
        let (sy, cy) = opts.light_yaw.sin_cos();
        let (sp, cp) = opts.light_pitch.sin_cos();
        normalize([cp * sy, sp, cp * cy])
    };

    // Project every vertex to screen once (positions are shared by the index buffer).
    let sv: Vec<SVert> = match view {
        View::Orbit(cam) => {
            let (sy, cy) = cam.yaw.sin_cos();
            let (sp, cp) = cam.pitch.sin_cos();
            let scale = (w.min(h) as f32 * 0.5 / r) * 0.9 * cam.zoom;
            let pan = [cam.pan[0] * w as f32, cam.pan[1] * h as f32];
            (0..mesh.positions.len())
                .map(|i| {
                    let p = mesh.positions[i];
                    let v = [
                        p[0] - mesh.center[0],
                        p[1] - mesh.center[1],
                        p[2] - mesh.center[2],
                    ];
                    let rv = orbit_rotate(v, sy, cy, sp, cp);
                    let uv = uv_of(i);
                    SVert {
                        x: cx + rv[0] * scale + pan[0],
                        y: cyc - rv[1] * scale + pan[1],
                        depth: rv[2], // ortho: larger z = nearer
                        wp: 1.0,      // affine
                        uz: uv[0],
                        vz: uv[1],
                        front: true,
                    }
                })
                .collect()
        }
        View::Fly(fc) => {
            let fwd = fc.forward();
            let right = fc.right();
            let up = cross(fwd, right); // orthonormal
            let focal = (h as f32 * 0.5) / (0.5f32).tan(); // ~53° vertical FOV
            let near = r * 0.02;
            (0..mesh.positions.len())
                .map(|i| {
                    let p = mesh.positions[i];
                    let rel = [p[0] - fc.eye[0], p[1] - fc.eye[1], p[2] - fc.eye[2]];
                    let vz = dot(rel, fwd);
                    let uv = uv_of(i);
                    if vz <= near {
                        return SVert {
                            x: 0.0,
                            y: 0.0,
                            depth: f32::MIN,
                            wp: 0.0,
                            uz: 0.0,
                            vz: 0.0,
                            front: false,
                        };
                    }
                    let inv = 1.0 / vz;
                    SVert {
                        x: cx + dot(rel, right) * inv * focal,
                        y: cyc - dot(rel, up) * inv * focal,
                        depth: inv, // 1/z: larger = nearer
                        wp: inv,
                        uz: uv[0] * inv,
                        vz: uv[1] * inv,
                        front: true,
                    }
                })
                .collect()
        }
    };
    // How to take a world-space face normal into view space (for the flip-toward-viewer test).
    let to_view_normal = |n: [f32; 3]| -> [f32; 3] {
        match view {
            View::Orbit(cam) => {
                let (sy, cy) = cam.yaw.sin_cos();
                let (sp, cp) = cam.pitch.sin_cos();
                orbit_rotate(n, sy, cy, sp, cp)
            }
            View::Fly(fc) => {
                let fwd = fc.forward();
                let right = fc.right();
                let up = cross(fwd, right);
                [dot(n, right), dot(n, up), dot(n, fwd)]
            }
        }
    };

    let textured = opts.textured && mesh.texture.is_some();
    let wireframe = opts.wireframe;

    for (tri_idx, tri) in mesh.indices.chunks_exact(3).enumerate() {
        let (ia, ib, ic) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        let (a, b, c) = (sv[ia], sv[ib], sv[ic]);
        if !(a.front && b.front && c.front) {
            continue; // any vertex behind the near plane → skip (simple near cull)
        }
        // Lighting: world face normal → view space, flipped toward the viewer (two-sided).
        let wn = normalize(cross(
            sub(mesh.positions[ib], mesh.positions[ia]),
            sub(mesh.positions[ic], mesh.positions[ia]),
        ));
        let mut nv = to_view_normal(wn);
        if nv[2] < 0.0 {
            nv = [-nv[0], -nv[1], -nv[2]];
        }
        // Per-channel light factor: ambient + diffuse·light_colour (tints highlights by the light).
        let ndl = dot(nv, light).max(0.0);
        let lf = [
            (0.28 + 0.72 * ndl * opts.light_rgb[0] as f32 / 255.0).clamp(0.0, 1.0),
            (0.28 + 0.72 * ndl * opts.light_rgb[1] as f32 / 255.0).clamp(0.0, 1.0),
            (0.28 + 0.72 * ndl * opts.light_rgb[2] as f32 / 255.0).clamp(0.0, 1.0),
        ];

        let area = edge(a.x, a.y, b.x, b.y, c.x, c.y);
        if area.abs() < 1e-6 {
            continue;
        }
        let inv_area = 1.0 / area;
        // Screen edge lengths (for constant-width wireframe).
        let (lbc, lca, lab) = if wireframe {
            (dist2d(b, c), dist2d(c, a), dist2d(a, b))
        } else {
            (1.0, 1.0, 1.0)
        };
        let minx = a.x.min(b.x).min(c.x).floor().max(0.0) as usize;
        let maxx = a.x.max(b.x).max(c.x).ceil().min(w as f32 - 1.0) as usize;
        let miny = a.y.min(b.y).min(c.y).floor().max(0.0) as usize;
        let maxy = a.y.max(b.y).max(c.y).ceil().min(h as f32 - 1.0) as usize;
        if minx > maxx || miny > maxy {
            continue;
        }
        for y in miny..=maxy {
            let fy = y as f32 + 0.5;
            for x in minx..=maxx {
                let fx = x as f32 + 0.5;
                let e0 = edge(b.x, b.y, c.x, c.y, fx, fy);
                let e1 = edge(c.x, c.y, a.x, a.y, fx, fy);
                let e2 = edge(a.x, a.y, b.x, b.y, fx, fy);
                let (w0, w1, w2) = (e0 * inv_area, e1 * inv_area, e2 * inv_area);
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let d = w0 * a.depth + w1 * b.depth + w2 * c.depth;
                let idx = y * w + x;
                if d <= depth[idx] {
                    continue;
                }
                depth[idx] = d;
                // Base surface: textured or flat, shaded.
                let base = if textured {
                    let wp = w0 * a.wp + w1 * b.wp + w2 * c.wp;
                    let u = (w0 * a.uz + w1 * b.uz + w2 * c.uz) / wp;
                    let v = (w0 * a.vz + w1 * b.vz + w2 * c.vz) / wp;
                    let t = mesh.texture.as_ref().unwrap().sample(u, v);
                    [t[0], t[1], t[2]]
                } else {
                    mesh.tri_rgb.get(tri_idx).copied().unwrap_or(mesh.base_rgb)
                };
                let mut out = [
                    (base[0] as f32 * lf[0]) as u8,
                    (base[1] as f32 * lf[1]) as u8,
                    (base[2] as f32 * lf[2]) as u8,
                    255,
                ];
                // Wireframe OVERLAY: draw the edge on top of the shaded surface (still
                // depth-tested above ⇒ hidden-line). Composes with solid AND textured.
                if wireframe {
                    let dbc = (w0 * area).abs() / lbc.max(1e-4);
                    let dca = (w1 * area).abs() / lca.max(1e-4);
                    let dab = (w2 * area).abs() / lab.max(1e-4);
                    if dbc.min(dca).min(dab) < 1.1 {
                        out = [
                            opts.wire_color[0],
                            opts.wire_color[1],
                            opts.wire_color[2],
                            255,
                        ];
                    }
                }
                color[idx] = out;
            }
        }
    }
    color
}

/// Export `mesh` as a flat-shaded **SVG** — a crisp *vector snapshot* of the 3D view (painter's-
/// algorithm depth sort → one filled `<polygon>` per triangle; curved surfaces become facets). Uses
/// the SAME projection + shading as [`render`], so it matches the on-screen image. Ideal for a logo
/// you then edit in Inkscape (the vector is baked at the current angle — no post-export rotation).
pub fn to_svg(mesh: &Mesh3D, w: usize, h: usize, view: &View, opts: &RenderOpts) -> String {
    let (cx, cyc) = (w as f32 * 0.5, h as f32 * 0.5);
    let r = mesh.radius.max(1e-4);
    let light = {
        let (sy, cy) = opts.light_yaw.sin_cos();
        let (sp, cp) = opts.light_pitch.sin_cos();
        normalize([cp * sy, sp, cp * cy])
    };
    // Project every vertex to (x, y, depth, in_front). Mirrors `render`'s vertex stage.
    let proj: Vec<(f32, f32, f32, bool)> = match view {
        View::Orbit(cam) => {
            let (sy, cy) = cam.yaw.sin_cos();
            let (sp, cp) = cam.pitch.sin_cos();
            let scale = (w.min(h) as f32 * 0.5 / r) * 0.9 * cam.zoom;
            let pan = [cam.pan[0] * w as f32, cam.pan[1] * h as f32];
            mesh.positions
                .iter()
                .map(|p| {
                    let v = [p[0] - mesh.center[0], p[1] - mesh.center[1], p[2] - mesh.center[2]];
                    let rv = orbit_rotate(v, sy, cy, sp, cp);
                    (cx + rv[0] * scale + pan[0], cyc - rv[1] * scale + pan[1], rv[2], true)
                })
                .collect()
        }
        View::Fly(fc) => {
            let fwd = fc.forward();
            let right = fc.right();
            let up = cross(fwd, right);
            let focal = (h as f32 * 0.5) / (0.5f32).tan();
            let near = r * 0.02;
            mesh.positions
                .iter()
                .map(|p| {
                    let rel = [p[0] - fc.eye[0], p[1] - fc.eye[1], p[2] - fc.eye[2]];
                    let vz = dot(rel, fwd);
                    if vz <= near {
                        return (0.0, 0.0, f32::MIN, false);
                    }
                    let inv = 1.0 / vz;
                    (cx + dot(rel, right) * inv * focal, cyc - dot(rel, up) * inv * focal, inv, true)
                })
                .collect()
        }
    };
    let to_view_normal = |n: [f32; 3]| -> [f32; 3] {
        match view {
            View::Orbit(cam) => {
                let (sy, cy) = cam.yaw.sin_cos();
                let (sp, cp) = cam.pitch.sin_cos();
                orbit_rotate(n, sy, cy, sp, cp)
            }
            View::Fly(fc) => {
                let fwd = fc.forward();
                let right = fc.right();
                let up = cross(fwd, right);
                [dot(n, right), dot(n, up), dot(n, fwd)]
            }
        }
    };

    // One entry per triangle: (avg depth, polygon points, shaded colour).
    type SvgTri = (f32, [(f32, f32); 3], [u8; 3]);
    let mut tris: Vec<SvgTri> = Vec::with_capacity(mesh.tri_count());
    for (i, t) in mesh.indices.chunks_exact(3).enumerate() {
        let (ia, ib, ic) = (t[0] as usize, t[1] as usize, t[2] as usize);
        let (a, b, c) = (proj[ia], proj[ib], proj[ic]);
        if !(a.3 && b.3 && c.3) {
            continue;
        }
        let wn = normalize(cross(
            sub(mesh.positions[ib], mesh.positions[ia]),
            sub(mesh.positions[ic], mesh.positions[ia]),
        ));
        let nv = to_view_normal(wn);
        let base = mesh.tri_rgb.get(i).copied().unwrap_or(mesh.base_rgb);
        let col = shade_tri(base, nv, light, opts.light_rgb);
        tris.push(((a.2 + b.2 + c.2) / 3.0, [(a.0, a.1), (b.0, b.1), (c.0, c.1)], col));
    }
    // Painter's algorithm: far (smaller depth) first, near last (on top).
    tris.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {w} {h}\" width=\"{w}\" height=\"{h}\" shape-rendering=\"geometricPrecision\">"
    );
    if opts.bg[3] == 255 {
        svg.push_str(&format!(
            "<rect width=\"{w}\" height=\"{h}\" fill=\"#{:02X}{:02X}{:02X}\"/>",
            opts.bg[0], opts.bg[1], opts.bg[2]
        ));
    }
    for (_, p, c) in &tris {
        let hexc = format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]);
        // A thin same-colour stroke hides the 1px seams between adjacent flat facets.
        svg.push_str(&format!(
            "<polygon points=\"{:.1},{:.1} {:.1},{:.1} {:.1},{:.1}\" fill=\"{hexc}\" stroke=\"{hexc}\" stroke-width=\"0.6\" stroke-linejoin=\"round\"/>",
            p[0].0, p[0].1, p[1].0, p[1].1, p[2].0, p[2].1
        ));
    }
    svg.push_str("</svg>");
    svg
}

/// Apply the orbit rotation R = Rx(pitch)·Ry(yaw) to a vector (sin/cos precomputed).
fn orbit_rotate(v: [f32; 3], sy: f32, cy: f32, sp: f32, cp: f32) -> [f32; 3] {
    let x1 = cy * v[0] + sy * v[2];
    let y1 = v[1];
    let z1 = -sy * v[0] + cy * v[2];
    [x1, cp * y1 - sp * z1, sp * y1 + cp * z1]
}

fn dist2d(a: SVert, b: SVert) -> f32 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

// --- tiny vec helpers (no glam dependency for this handful of ops) ---
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn normalize(v: [f32; 3]) -> [f32; 3] {
    let l = dot(v, v).sqrt();
    if l < 1e-8 {
        [0.0, 0.0, 1.0]
    } else {
        [v[0] / l, v[1] / l, v[2] / l]
    }
}
/// Twice the signed area of triangle (a,b,p) — the standard rasterizer edge function.
fn edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
    (bx - ax) * (py - ay) - (by - ay) * (px - ax)
}

/// The 3D-model decoder: registered in the registry, routed by extension. Its tile is a
/// CPU-rendered shaded preview of the geometry (or `Unsupported` if it won't load).
pub struct MeshDecoder;

/// Render a model file at `path` to a `size`×`size` thumbnail (default hero angle, solid,
/// transparent background so the tile backdrop shows through).
pub fn decode_thumb(path: &Path, size: u32) -> Result<PixImage, DecodeError> {
    let mesh = load(path).ok_or(DecodeError::Unsupported)?;
    let s = size.max(16) as usize;
    let opts = RenderOpts {
        bg: [0, 0, 0, 0], // transparent thumbnail
        ..RenderOpts::default()
    };
    let px = render(&mesh, s, s, &View::Orbit(Camera::default()), &opts);
    Ok(PixImage::from_rgba(s as u32, s as u32, px))
}

impl Decoder for MeshDecoder {
    fn name(&self) -> &'static str {
        "mesh3d"
    }
    fn extensions(&self) -> &'static [&'static str] {
        MESH_EXTS
    }
    fn sniff(&self, _: &[u8]) -> bool {
        false // routed by extension (needs the path); no reliable magic across all 6
    }
    fn decode(&self, _: &[u8]) -> Result<PixImage, DecodeError> {
        // Never reached via bytes — the registry routes MESH_EXTS to `decode_thumb(path)`.
        Err(DecodeError::Unsupported)
    }
}

// ── Companion 3D files: .mtl material previews + .blend placeholders ─────────────────
// These aren't geometry (they don't enter the interactive viewer), but they belong to the
// 3D story, so they're gated by the same "3d" plugin and shown as informative tiles.

/// Companion 3D file extensions: routed to [`decode_aux`], gated by the 3D plugin, but
/// NOT `is_mesh_path` (they show as a static tile, they don't open the 3D viewer).
pub const AUX_EXTS: &[&str] = &["mtl", "blend", "blend1"];

/// A "basic preview" of a Wavefront `.mtl`: one colour swatch per material (its diffuse
/// `Kd`), captioned with the material name. Parses just `newmtl` + `Kd` (ignores maps /
/// other opcodes). This is what the user asked for — a glanceable palette of the file.
fn render_mtl_swatches(bytes: &[u8]) -> PixImage {
    let text = String::from_utf8_lossy(bytes);
    let mut mats: Vec<(String, [u8; 3])> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix("newmtl ") {
            mats.push((name.trim().to_string(), [200, 200, 200])); // default grey until Kd seen
        } else if let Some(kd) = line.strip_prefix("Kd ") {
            let c: Vec<f32> = kd
                .split_whitespace()
                .filter_map(|t| t.parse().ok())
                .collect();
            if c.len() >= 3 {
                if let Some(last) = mats.last_mut() {
                    last.1 = [
                        (c[0].clamp(0.0, 1.0) * 255.0) as u8,
                        (c[1].clamp(0.0, 1.0) * 255.0) as u8,
                        (c[2].clamp(0.0, 1.0) * 255.0) as u8,
                    ];
                }
            }
        }
    }
    // Grid of swatches. Empty file → a single "MTL / no materials" note.
    let cols = (mats.len() as f32).sqrt().ceil().max(1.0) as usize;
    let rows = mats.len().div_ceil(cols).max(1);
    let cell = 96usize;
    let (w, h) = ((cols * cell).max(cell), (rows * cell).max(cell));
    let mut px = vec![[30u8, 30, 34, 255]; w * h];
    for (i, (name, rgb)) in mats.iter().enumerate() {
        let (cx, cy) = ((i % cols) * cell, (i / cols) * cell);
        let (x0, y0, x1, y1) = (cx + 8, cy + 8, cx + cell - 8, cy + cell - 26);
        fill_rect(&mut px, w, x0, y0, x1, y1, [rgb[0], rgb[1], rgb[2], 255]);
        stroke_rect(&mut px, w, x0, y0, x1, y1, [0, 0, 0, 255]);
        // Name centered under the swatch (clipped to the cell width).
        let label: String = name.chars().take(cell / 8).collect();
        let tw = label.chars().count() * 8;
        let tx = cx + (cell.saturating_sub(tw)) / 2;
        blit_text(
            &mut px,
            w,
            tx,
            cy + cell - 20,
            &label,
            [210, 210, 210, 255],
            1,
        );
    }
    if mats.is_empty() {
        blit_text(
            &mut px,
            w,
            10,
            h / 2 - 8,
            "MTL: no Kd materials",
            [180, 180, 180, 255],
            1,
        );
    }
    PixImage::from_rgba(w as u32, h as u32, px)
}

/// A unit UV sphere (centred at origin, radius 1) with per-vertex UVs — the "material ball"
/// for the `.mtl` preview. `render` computes face normals, so no vertex normals are needed.
fn uv_sphere(segments: usize, rings: usize) -> Mesh3D {
    use std::f32::consts::PI;
    let mut m = Mesh3D::default();
    let stride = segments + 1;
    for r in 0..=rings {
        let v = r as f32 / rings as f32; // 0 (top) .. 1 (bottom)
        let (sp, cp) = (v * PI).sin_cos();
        for s in 0..=segments {
            let u = s as f32 / segments as f32;
            let (st, ct) = (u * 2.0 * PI).sin_cos();
            m.positions.push([sp * ct, cp, sp * st]);
            m.texcoords.push([u, 1.0 - v]);
        }
    }
    for r in 0..rings {
        for s in 0..segments {
            let a = (r * stride + s) as u32;
            let (b, c, d) = (a + 1, a + stride as u32, a + stride as u32 + 1);
            m.indices.extend([a, c, b, b, c, d]); // two outward-facing triangles
        }
    }
    m.center = [0.0, 0.0, 0.0];
    m.radius = 1.0;
    m
}

/// The path-aware `.mtl` **material previewer**: one lit "material ball" per `newmtl`,
/// shaded with its `Kd` diffuse colour and (if present) its `map_Kd` texture resolved
/// relative to the file — a proper glanceable preview + thumbnail. Falls back to the flat
/// swatch grid ([`render_mtl_swatches`]) if nothing parses.
pub fn decode_mtl_path(path: &Path) -> Result<PixImage, DecodeError> {
    let bytes = std::fs::read(path).map_err(|e| DecodeError::Io(e.to_string()))?;
    let text = String::from_utf8_lossy(&bytes);
    let dir = path.parent().unwrap_or(Path::new("."));
    // (name, Kd, diffuse texture)
    let mut mats: Vec<(String, [u8; 3], Option<Texture>)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix("newmtl ") {
            mats.push((name.trim().to_string(), [200, 200, 200], None));
        } else if let Some(kd) = line.strip_prefix("Kd ") {
            let c: Vec<f32> = kd
                .split_whitespace()
                .filter_map(|t| t.parse().ok())
                .collect();
            if c.len() >= 3 {
                if let Some(m) = mats.last_mut() {
                    m.1 = [
                        (c[0].clamp(0.0, 1.0) * 255.0) as u8,
                        (c[1].clamp(0.0, 1.0) * 255.0) as u8,
                        (c[2].clamp(0.0, 1.0) * 255.0) as u8,
                    ];
                }
            }
        } else if let Some(mp) = line.strip_prefix("map_Kd ") {
            // The path is the last whitespace token (skip any `-o`/`-s` map options).
            if let Some(file) = mp.split_whitespace().last() {
                let full = dir.join(file);
                if let Some(m) = mats.last_mut() {
                    m.2 = load_texture(&full);
                }
            }
        }
    }
    if mats.is_empty() {
        return Ok(render_mtl_swatches(&bytes)); // nothing to preview → the old swatch grid
    }

    // Render each material as a lit ball into a grid cell + caption its name.
    let sphere = uv_sphere(48, 24);
    let cell = 128usize;
    let cols = (mats.len() as f32).sqrt().ceil().max(1.0) as usize;
    let rows = mats.len().div_ceil(cols).max(1);
    let (w, h) = (cols * cell, rows * cell);
    let mut px = vec![[30u8, 30, 34, 255]; w * h];
    for (i, (name, kd, tex)) in mats.iter().enumerate() {
        let (cx, cy) = ((i % cols) * cell, (i / cols) * cell);
        let mut ball = sphere.clone();
        ball.base_rgb = *kd;
        ball.texture = tex.clone();
        let opts = RenderOpts {
            textured: tex.is_some(),
            bg: [0, 0, 0, 0], // transparent → composited over the cell
            ..RenderOpts::default()
        };
        let ballpx = render(&ball, cell, cell, &View::Orbit(Camera::default()), &opts);
        // Composite the ball (alpha over the cell bg).
        for y in 0..cell {
            for x in 0..cell {
                let s = ballpx[y * cell + x];
                if s[3] > 0 {
                    px[(cy + y) * w + (cx + x)] = [s[0], s[1], s[2], 255];
                }
            }
        }
        let label: String = name.chars().take(cell / 8).collect();
        let tw = label.chars().count() * 8;
        blit_text(
            &mut px,
            w,
            cx + (cell.saturating_sub(tw)) / 2,
            cy + cell - 16,
            &label,
            [225, 225, 230, 255],
            1,
        );
    }
    Ok(PixImage::from_rgba(w as u32, h as u32, px))
}

/// A `.blend` placeholder tile. The Rust ecosystem can't read a modern (4.x) `.blend`'s
/// geometry, so we show a branded, honest tile — the file is *visible* + openable in
/// Blender via "Open in default app" / Enter — rather than hiding it from the listing.
fn render_blend_placeholder() -> PixImage {
    let (w, h) = (360usize, 360usize);
    let mut px = vec![[28u8, 28, 32, 255]; w * h];
    // A Blender-orange disc-ish square as the mark.
    let orange = [234u8, 120, 38, 255];
    fill_rect(&mut px, w, 120, 96, 240, 216, orange);
    stroke_rect(&mut px, w, 120, 96, 240, 216, [0, 0, 0, 255]);
    let scale = 3;
    let word = "BLEND";
    let tw = word.len() * 8 * scale;
    blit_text(
        &mut px,
        w,
        (w - tw) / 2,
        250,
        word,
        [235, 235, 235, 255],
        scale,
    );
    let hint = "right-click Render";
    let hw = hint.chars().count() * 8;
    blit_text(&mut px, w, (w - hw) / 2, 300, hint, [150, 150, 150, 255], 1);
    PixImage::from_rgba(w as u32, h as u32, px)
}

/// The directory holding cached headless-Blender `.blend` renders (`<cache>/blend/`). Kept
/// SEPARATE from the 16colo.rs blob/SQLite cache — plain PNGs keyed by the model's path.
pub fn blend_cache_dir() -> Option<std::path::PathBuf> {
    crate::cache::dir().map(|d| d.join("blend"))
}

/// Where a headless-Blender render of `blend` is cached — `<cache>/blend/<hash>.png`, keyed
/// by the file's absolute path. `None` if the cache dir isn't available. Once a render lands
/// here, [`decode_blend`] shows it as the `.blend`'s tile (persisted across restarts).
pub fn blend_render_path(blend: &Path) -> Option<std::path::PathBuf> {
    use std::hash::{Hash, Hasher};
    let abs = std::fs::canonicalize(blend).unwrap_or_else(|_| blend.to_path_buf());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    abs.to_string_lossy().hash(&mut h);
    blend_cache_dir().map(|d| d.join(format!("{:016x}.png", h.finish())))
}

/// Total size (bytes) + file count of the cached `.blend` renders — for the Preferences readout.
pub fn blend_cache_stats() -> (u64, usize) {
    let Some(dir) = blend_cache_dir() else {
        return (0, 0);
    };
    let mut bytes = 0u64;
    let mut count = 0usize;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if let Ok(m) = e.metadata() {
                if m.is_file() {
                    bytes += m.len();
                    count += 1;
                }
            }
        }
    }
    (bytes, count)
}

/// Delete every cached `.blend` render.
pub fn clear_blend_cache() {
    if let Some(dir) = blend_cache_dir() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// The path-aware `.blend` tile: a cached Blender render if one exists (see
/// [`blend_render_path`]), else the branded placeholder. Routed by extension in
/// `decode_bytes` (which has the path; `decode(bytes)` alone can't find the cache).
pub fn decode_blend(path: &Path) -> Result<PixImage, DecodeError> {
    if let Some(render) = blend_render_path(path) {
        if render.is_file() {
            if let Some(tex) = load_texture(&render) {
                return Ok(PixImage::from_rgba(tex.w as u32, tex.h as u32, tex.px));
            }
        }
    }
    Ok(render_blend_placeholder())
}

// Minimal raster helpers (mirrors the ones in `pdf.rs`; kept local so the two decoders
// stay decoupled). CP437 8×16 glyphs for the labels.
fn set_px(px: &mut [[u8; 4]], w: usize, x: usize, y: usize, c: [u8; 4]) {
    if x < w {
        let i = y * w + x;
        if i < px.len() {
            px[i] = c;
        }
    }
}
fn fill_rect(px: &mut [[u8; 4]], w: usize, x0: usize, y0: usize, x1: usize, y1: usize, c: [u8; 4]) {
    for y in y0..y1 {
        for x in x0..x1 {
            set_px(px, w, x, y, c);
        }
    }
}
fn stroke_rect(
    px: &mut [[u8; 4]],
    w: usize,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    c: [u8; 4],
) {
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    for x in x0..x1 {
        set_px(px, w, x, y0, c);
        set_px(px, w, x, y1 - 1, c);
    }
    for y in y0..y1 {
        set_px(px, w, x0, y, c);
        set_px(px, w, x1 - 1, y, c);
    }
}
fn blit_text(
    px: &mut [[u8; 4]],
    w: usize,
    x0: usize,
    y0: usize,
    s: &str,
    c: [u8; 4],
    scale: usize,
) {
    use super::cp437_font::CP437_8X16;
    for (i, ch) in s.chars().enumerate() {
        let byte = if (0x20..0x7f).contains(&(ch as u32)) {
            ch as u8
        } else {
            b'?'
        };
        let glyph = &CP437_8X16[byte as usize];
        let gx = x0 + i * 8 * scale;
        for (ry, &bits) in glyph.iter().enumerate() {
            for rx in 0..8 {
                if (bits >> (7 - rx)) & 1 == 1 {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            set_px(px, w, gx + rx * scale + sx, y0 + ry * scale + sy, c);
                        }
                    }
                }
            }
        }
    }
}

/// Decoder for Wavefront `.mtl` material files → a swatch preview (see [`render_mtl_swatches`]).
pub struct MtlDecoder;
impl Decoder for MtlDecoder {
    fn name(&self) -> &'static str {
        "mtl"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["mtl"]
    }
    fn sniff(&self, _: &[u8]) -> bool {
        false
    }
    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        Ok(render_mtl_swatches(bytes))
    }
}

/// Decoder for Blender `.blend`/`.blend1` → a placeholder tile (see [`render_blend_placeholder`]).
pub struct BlendDecoder;
impl Decoder for BlendDecoder {
    fn name(&self) -> &'static str {
        "blend"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["blend", "blend1"]
    }
    fn sniff(&self, _: &[u8]) -> bool {
        false
    }
    fn decode(&self, _: &[u8]) -> Result<PixImage, DecodeError> {
        Ok(render_blend_placeholder())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A minimal ASCII-OBJ unit tetrahedron, written to a temp file and loaded.
    const TETRA_OBJ: &str = "\
v 0 0 0
v 1 0 0
v 0 1 0
v 0 0 1
f 1 3 2
f 1 2 4
f 1 4 3
f 2 3 4
";

    fn write_temp(name: &str, body: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("pv_mesh_{}_{name}", std::process::id()));
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn loads_obj_and_computes_bounds() {
        let p = write_temp("tetra.obj", TETRA_OBJ);
        let mesh = load(&p).expect("tetra.obj loads");
        // mesh-loader de-indexes OBJ faces (a vertex per face-corner), so it's 4 tris ×
        // 3 corners = 12 positions here — we only rely on the triangle count + bounds.
        assert!(mesh.positions.len() >= 4);
        assert_eq!(mesh.tri_count(), 4);
        // AABB centre of the unit tetra corners is (0.5, 0.5, 0.5)/... actually mid of
        // [0,1] per axis = 0.5, but z spans 0..1 too → each centre component 0.5.
        for c in mesh.center {
            assert!((c - 0.5).abs() < 1e-6, "centre {c}");
        }
        assert!(mesh.radius > 0.4 && mesh.radius < 1.0);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn loads_ascii_ply_with_uint_indices() {
        // A tiny ASCII PLY: a quad (fan-triangulated to 2 tris), `uint` index type + s/t UVs
        // — the exact shape/variant Blender emits that `mesh-loader` can't read.
        let ply = "\
ply
format ascii 1.0
element vertex 4
property float x
property float y
property float z
property float s
property float t
element face 1
property list uchar uint vertex_indices
end_header
0 0 0 0 0
1 0 0 1 0
1 1 0 1 1
0 1 0 0 1
4 0 1 2 3
";
        let p = write_temp("quad.ply", ply);
        let mesh = load(&p).expect("ascii ply loads");
        assert_eq!(mesh.positions.len(), 4);
        assert_eq!(mesh.tri_count(), 2); // the quad fan-triangulates to two triangles
        assert_eq!(mesh.texcoords.len(), 4);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn renders_nonempty_shaded_thumbnail() {
        let p = write_temp("tetra2.obj", TETRA_OBJ);
        let img = decode_thumb(&p, 64).expect("renders");
        assert_eq!((img.width, img.height), (64, 64));
        // At least some pixels are opaque (the model drew) and shaded (not pure base).
        let opaque = img.pixels.iter().filter(|p| p[3] == 255).count();
        assert!(
            opaque > 50,
            "expected the tetra to fill some pixels, got {opaque}"
        );
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn garbage_file_fails_gracefully() {
        let p = write_temp("bad.obj", "not a real obj \x00\x01\x02");
        // Either None from load or Unsupported from decode — never a panic.
        assert!(load(&p).is_none() || decode_thumb(&p, 32).is_ok());
        std::fs::remove_file(&p).ok();
    }
}
