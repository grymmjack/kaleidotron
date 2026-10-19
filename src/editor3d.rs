//! "Open in 3D editor": hand a model to Blender (found automatically) or to the 3D editor set in
//! Preferences.
//!
//! Blender only *opens* `.blend` files; anything else has to be imported. So for Blender we start it
//! with a short Python script that clears the default cube, imports the model by its extension,
//! frames it and switches to Material Preview. Any other editor just gets the file path.
//!
//! The 3D logo maker and the extruded-SVG view have no file: their mesh is written to an `.obj`
//! (+ `.mtl`, + the texture) in the cache folder first. Files under the temp folder (archive
//! members) are copied there too when Blender is the Flatpak, whose sandbox can't see the host's
//! `/tmp`.

use crate::decode::mesh3d::Mesh3D;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// The editor to use: its argv prefix, a name for the UI, and whether it's Blender.
#[derive(Clone, Debug)]
pub struct Editor {
    pub argv: Vec<String>,
    pub name: String,
    pub blender: bool,
}

impl Editor {
    fn flatpak(&self) -> bool {
        self.argv.first().is_some_and(|a| a == "flatpak")
    }
}

/// The editor for `setting` (a program path from Preferences), else Blender, found automatically.
pub fn resolve(setting: Option<&Path>) -> Option<Editor> {
    if let Some(p) = setting.filter(|p| !p.as_os_str().is_empty()) {
        let file = p.file_name().map(|f| f.to_string_lossy().to_lowercase()).unwrap_or_default();
        return Some(Editor {
            argv: vec![p.display().to_string()],
            name: p.file_stem().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string()),
            // a Blender build anywhere (blender, blender.exe, Blender.app/Contents/MacOS/Blender)
            blender: file.starts_with("blender"),
        });
    }
    detect_blender().clone()
}

/// Blender on this machine, looked for once: on the PATH, the Flatpak, the Snap, then the usual
/// install folders (Linux tarballs in ~/ and /opt, Steam, macOS /Applications, Windows Program Files).
pub fn detect_blender() -> &'static Option<Editor> {
    static FOUND: OnceLock<Option<Editor>> = OnceLock::new();
    FOUND.get_or_init(|| {
        let native = |p: PathBuf| Editor { name: format!("Blender ({})", p.display()), argv: vec![p.display().to_string()], blender: true };
        if let Some(p) = on_path(if cfg!(windows) { "blender.exe" } else { "blender" }) {
            return Some(native(p));
        }
        if cfg!(target_os = "linux") {
            let ok = Command::new("flatpak")
                .args(["info", "org.blender.Blender"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if ok {
                return Some(Editor {
                    argv: vec!["flatpak".into(), "run".into(), "org.blender.Blender".into()],
                    name: "Blender (Flatpak)".into(),
                    blender: true,
                });
            }
        }
        candidates().into_iter().find(|p| p.is_file()).map(native)
    })
}

fn on_path(exe: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(exe)).find(|p| p.is_file())
}

/// Install locations to try, newest version first where a folder holds several.
fn candidates() -> Vec<PathBuf> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    let mut out = Vec::new();
    // every `<dir>/blender*/<exe>`, highest version first (blender-4.2-linux-x64 < blender-5.2-…)
    let versions = |dir: &Path, exe: &str, out: &mut Vec<PathBuf>| {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut found: Vec<PathBuf> = rd
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().to_lowercase().starts_with("blender"))
            .map(|e| e.path().join(exe))
            .collect();
        found.sort_by(|a, b| natord(&b.to_string_lossy(), &a.to_string_lossy()));
        out.extend(found);
    };
    if cfg!(target_os = "macos") {
        out.push("/Applications/Blender.app/Contents/MacOS/Blender".into());
        if let Some(h) = &home {
            out.push(h.join("Applications/Blender.app/Contents/MacOS/Blender"));
        }
    } else if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
            if let Some(pf) = std::env::var_os(var).map(PathBuf::from) {
                versions(&pf.join("Blender Foundation"), "blender.exe", &mut out);
                out.push(pf.join("Steam/steamapps/common/Blender/blender.exe"));
            }
        }
    } else {
        out.push("/snap/bin/blender".into());
        out.push("/usr/bin/blender".into());
        out.push("/usr/local/bin/blender".into());
        versions(Path::new("/opt"), "blender", &mut out);
        if let Some(h) = &home {
            versions(h, "blender", &mut out);
            versions(&h.join("Applications"), "blender", &mut out);
            versions(&h.join("apps"), "blender", &mut out);
            out.push(h.join(".steam/steam/steamapps/common/Blender/blender"));
            out.push(h.join(".local/share/Steam/steamapps/common/Blender/blender"));
        }
    }
    out
}

/// Natural order ("blender-10" after "blender-9").
fn natord(a: &str, b: &str) -> std::cmp::Ordering {
    let key = |s: &str| -> Vec<(String, u64)> {
        let mut v = Vec::new();
        let mut text = String::new();
        let mut num = String::new();
        for c in s.chars() {
            if c.is_ascii_digit() {
                num.push(c);
            } else {
                if !num.is_empty() {
                    v.push((std::mem::take(&mut text), num.parse().unwrap_or(0)));
                    num.clear();
                }
                text.push(c);
            }
        }
        v.push((text, num.parse().unwrap_or(0)));
        v
    };
    key(a).cmp(&key(b))
}

/// Blender's start-up script: clear the default scene's cube, import the model after `--`, frame it.
const IMPORT_PY: &str = r#"
import bpy, os, sys
p = sys.argv[sys.argv.index("--") + 1]
ext = os.path.splitext(p)[1].lower()
cube = bpy.data.objects.get("Cube")
if cube:
    bpy.data.objects.remove(cube)
imp = {".glb": lambda: bpy.ops.import_scene.gltf(filepath=p), ".gltf": lambda: bpy.ops.import_scene.gltf(filepath=p),
       ".obj": lambda: bpy.ops.wm.obj_import(filepath=p), ".stl": lambda: bpy.ops.wm.stl_import(filepath=p),
       ".ply": lambda: bpy.ops.wm.ply_import(filepath=p), ".fbx": lambda: bpy.ops.import_scene.fbx(filepath=p),
       ".dae": lambda: bpy.ops.wm.collada_import(filepath=p)}
try:
    imp[ext]()
except Exception as e:
    print("kaleidotron: couldn't import", p, e)
def frame():
    for w in bpy.context.window_manager.windows:
        for a in w.screen.areas:
            if a.type == "VIEW_3D":
                a.spaces.active.shading.type = "MATERIAL"
                r = next(r for r in a.regions if r.type == "WINDOW")
                with bpy.context.temp_override(window=w, area=a, region=r):
                    bpy.ops.view3d.view_all()
bpy.app.timers.register(frame, first_interval=0.5)
"#;

/// Open `model` in `editor`, detached. Returns a status line.
pub fn open(editor: &Editor, model: &Path) -> Result<String, String> {
    let mut model = model.to_path_buf();
    if editor.flatpak() && model.starts_with(std::env::temp_dir()) {
        model = copy_out_of_tmp(&model).map_err(|e| format!("couldn't copy the model for Blender: {e}"))?;
    }
    let mut cmd = Command::new(&editor.argv[0]);
    cmd.args(&editor.argv[1..]);
    let ext = model.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if editor.blender && ext != "blend" && ext != "blend1" {
        cmd.arg("--python-expr").arg(IMPORT_PY).arg("--");
    }
    cmd.arg(&model).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| format!("couldn't start {}: {e}", editor.name))?;
    std::thread::spawn(move || child.wait()); // reap it when it closes
    Ok(format!(
        "Opening {} in {}",
        model.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(),
        editor.name
    ))
}

/// Where exported / copied models go: the persistent cache, which a Flatpak can see.
fn work_dir() -> std::io::Result<PathBuf> {
    let base = crate::cache::dir().unwrap_or_else(|| {
        let home = std::env::var_os(if cfg!(windows) { "LOCALAPPDATA" } else { "HOME" }).map(PathBuf::from);
        home.map(|h| if cfg!(windows) { h.join("kaleidotron") } else { h.join(".cache/kaleidotron") })
            .unwrap_or_else(std::env::temp_dir)
    });
    let d = base.join("3d-editor");
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

/// Copy `model` and the files beside it (a .gltf's .bin and textures, an .obj's .mtl) to the cache.
fn copy_out_of_tmp(model: &Path) -> std::io::Result<PathBuf> {
    let dir = model.parent().unwrap_or(Path::new("."));
    let stem = model.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let dest = work_dir()?.join(&stem);
    std::fs::create_dir_all(&dest)?;
    for e in std::fs::read_dir(dir)?.flatten() {
        if e.file_type().is_ok_and(|t| t.is_file()) && e.metadata().is_ok_and(|m| m.len() < 512 << 20) {
            std::fs::copy(e.path(), dest.join(e.file_name()))?;
        }
    }
    Ok(dest.join(model.file_name().unwrap_or_default()))
}

/// Write an in-memory mesh (the 3D logo maker, an extruded SVG) to `<cache>/3d-editor/<name>.obj`
/// with an `.mtl`: one material per triangle colour, or the texture when it has one.
pub fn export_obj(mesh: &Mesh3D, name: &str) -> std::io::Result<PathBuf> {
    use std::fmt::Write as _;
    let safe: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(60)
        .collect();
    let safe = if safe.is_empty() { "model".to_string() } else { safe };
    let dir = work_dir()?;
    let (obj, mtl) = (dir.join(format!("{safe}.obj")), dir.join(format!("{safe}.mtl")));
    let textured = mesh.texture.is_some() && mesh.texcoords.len() == mesh.positions.len();
    let mut o = String::with_capacity(mesh.positions.len() * 40 + mesh.indices.len() * 12);
    let mut m = String::new();
    let _ = writeln!(o, "# kaleidotron\nmtllib {safe}.mtl");
    for p in &mesh.positions {
        let _ = writeln!(o, "v {} {} {}", p[0], p[1], p[2]);
    }
    if textured {
        for t in &mesh.texcoords {
            let _ = writeln!(o, "vt {} {}", t[0], t[1]);
        }
        let tex = mesh.texture.as_ref().unwrap();
        let png = dir.join(format!("{safe}_texture.png"));
        let flat: Vec<u8> = tex.px.iter().flat_map(|c| *c).collect();
        image::save_buffer(&png, &flat, tex.w as u32, tex.h as u32, image::ColorType::Rgba8)
            .map_err(std::io::Error::other)?;
        let _ = writeln!(m, "newmtl tex\nKd 1 1 1\nmap_Kd {safe}_texture.png");
        let _ = writeln!(o, "usemtl tex");
        for t in mesh.indices.chunks_exact(3) {
            let _ = writeln!(o, "f {a}/{a} {b}/{b} {c}/{c}", a = t[0] + 1, b = t[1] + 1, c = t[2] + 1);
        }
    } else {
        // group the triangles by colour: one material each
        let colour = |i: usize| mesh.tri_rgb.get(i).copied().unwrap_or(mesh.base_rgb);
        let mut groups: Vec<([u8; 3], Vec<usize>)> = Vec::new();
        for i in 0..mesh.tri_count() {
            let c = colour(i);
            match groups.iter_mut().find(|g| g.0 == c) {
                Some(g) => g.1.push(i),
                None => groups.push((c, vec![i])),
            }
        }
        for (k, (c, tris)) in groups.iter().enumerate() {
            let _ = writeln!(m, "newmtl c{k}\nKd {:.4} {:.4} {:.4}", c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0);
            let _ = writeln!(o, "usemtl c{k}");
            for &i in tris {
                let t = &mesh.indices[i * 3..i * 3 + 3];
                let _ = writeln!(o, "f {} {} {}", t[0] + 1, t[1] + 1, t[2] + 1);
            }
        }
    }
    std::fs::write(&mtl, m)?;
    std::fs::write(&obj, o)?;
    Ok(obj)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order_picks_the_newest_blender() {
        let mut v = vec!["blender-4.2-linux", "blender-10.0-linux", "blender-5.2-linux"];
        v.sort_by(|a, b| natord(b, a));
        assert_eq!(v, ["blender-10.0-linux", "blender-5.2-linux", "blender-4.2-linux"]);
    }

    #[test]
    fn a_set_program_wins_and_is_known_as_blender_by_name() {
        let e = resolve(Some(Path::new("/opt/blender-5.2/blender"))).unwrap();
        assert!(e.blender);
        let e = resolve(Some(Path::new("/usr/bin/f3d"))).unwrap();
        assert!(!e.blender);
        assert_eq!(e.argv, ["/usr/bin/f3d"]);
    }

    #[test]
    fn obj_export_round_trips_counts_and_colours() {
        let mesh = Mesh3D {
            positions: vec![[0., 0., 0.], [1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
            indices: vec![0, 2, 1, 0, 1, 3, 0, 3, 2, 1, 2, 3],
            tri_rgb: vec![[255, 0, 0], [255, 0, 0], [0, 0, 255], [0, 0, 255]],
            ..Default::default()
        };
        let p = export_obj(&mesh, &format!("ktest_{}", std::process::id())).unwrap();
        let back = crate::decode::mesh3d::load(&p).expect("exported obj loads");
        assert_eq!(back.tri_count(), 4);
        let mtl = std::fs::read_to_string(p.with_extension("mtl")).unwrap();
        assert_eq!(mtl.matches("newmtl").count(), 2);
        std::fs::remove_file(p.with_extension("mtl")).ok();
        std::fs::remove_file(&p).ok();
    }
}
