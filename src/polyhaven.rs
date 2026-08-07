//! [Poly Haven](https://polyhaven.com) — **CC0** 3D models, textures and HDRIs, as a keyless virtual
//! source (mirrors [`crate::sixteen`] / [`crate::lospec`]). Three facets in one API:
//!
//! * **models** → a `.gltf` bundle that opens in kaleidotron's existing `Mode::ThreeD` viewer
//! * **textures** → the diffuse map as a normal image tile
//! * **HDRIs** → the `tonemapped` JPG preview as the tile (the real `.hdr` is a download)
//!
//! Two things make this source different from the others:
//!
//! 1. **The whole catalogue arrives in one call** (`/assets?t=models` → all 521), so *search is a
//!    local filter* — instant, no per-keystroke network round-trip.
//! 2. **A model is a bundle, not a file.** The `.gltf` is a ~2.7 KB manifest referencing an external
//!    `.bin` + texture jpgs. The API hands us an `include` map of `relative path → URL`, so
//!    [`model_bundle`] returns the main file plus its siblings and the caller materialises them into
//!    one directory at exactly those relative paths — after which the `gltf` crate loads it as an
//!    ordinary on-disk model.
//!
//! Everything here is pure + unit-tested; the egui/threading wiring lives in `app.rs` (`ph_*`).

use std::path::Path;

/// Virtual root for Poly Haven browsing.
pub const ROOT: &str = "<polyhaven>";
/// The search facet under a kind: `<polyhaven>/models/search/<query>`.
pub const SEARCH: &str = "search";

const API: &str = "https://api.polyhaven.com";
/// Catalogue//files responses change rarely; cache for a day.
const TTL: i64 = 86_400;
/// Resolution we fetch for models/textures — 2k keeps a browse session light.
pub const DEFAULT_RES: &str = "2k";

/// The three Poly Haven asset families.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Models,
    Textures,
    Hdris,
}

impl Kind {
    /// The `?t=` API value — also the path segment under [`ROOT`].
    pub fn slug(self) -> &'static str {
        match self {
            Kind::Models => "models",
            Kind::Textures => "textures",
            Kind::Hdris => "hdris",
        }
    }
    /// Label for the Places tab / breadcrumb.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Models => "3D Models",
            Kind::Textures => "Textures",
            Kind::Hdris => "HDRIs",
        }
    }
    /// The extension a downloaded asset of this kind lands as (what the tile/viewer decodes).
    pub fn ext(self) -> &'static str {
        match self {
            Kind::Models => "gltf",
            // Textures + HDRIs are viewed as ordinary images (diffuse map / tonemapped preview).
            Kind::Textures | Kind::Hdris => "jpg",
        }
    }
    pub fn from_slug(s: &str) -> Option<Kind> {
        match s {
            "models" => Some(Kind::Models),
            "textures" => Some(Kind::Textures),
            "hdris" => Some(Kind::Hdris),
            _ => None,
        }
    }
    /// All three, in display order.
    pub const ALL: [Kind; 3] = [Kind::Models, Kind::Textures, Kind::Hdris];
}

pub fn is_remote(path: &Path) -> bool {
    path.starts_with(ROOT)
}

pub fn rel_parts(path: &Path) -> Vec<String> {
    path.strip_prefix(ROOT)
        .ok()
        .map(|p| {
            p.components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// One catalogue entry.
#[derive(Clone, Debug, Default)]
pub struct PhAsset {
    pub slug: String,
    pub name: String,
    pub categories: Vec<String>,
    pub tags: Vec<String>,
    pub authors: Vec<String>,
    pub polycount: u64,
    pub downloads: u64,
    pub thumb_url: String,
}

impl PhAsset {
    /// The tile/leaf filename: `<Name> [<slug>].<ext>` — the slug round-trips via [`parse_slug`], so
    /// a virtual path alone identifies the asset (same trick as the YouTube `[id]` scheme).
    pub fn filename(&self, kind: Kind) -> String {
        let safe: String = self
            .name
            .chars()
            .map(|c| if c == '/' || c == '\\' || c == '[' || c == ']' { '_' } else { c })
            .collect();
        format!("{} [{}].{}", safe.trim(), self.slug, kind.ext())
    }
    /// `polyhaven.com` landing page (right-click → open in browser / attribution).
    pub fn page_url(&self) -> String {
        format!("https://polyhaven.com/a/{}", self.slug)
    }
    /// "Kirill Sannikov" / "A, B" — for the Details pane.
    pub fn author_label(&self) -> String {
        if self.authors.is_empty() {
            "—".into()
        } else {
            self.authors.join(", ")
        }
    }
}

/// Recover the asset slug from a leaf filename built by [`PhAsset::filename`].
pub fn parse_slug(filename: &str) -> Option<String> {
    let open = filename.rfind('[')?;
    let close = filename[open..].find(']')? + open;
    let slug = &filename[open + 1..close];
    (!slug.is_empty()).then(|| slug.to_string())
}

/// Parse an `/assets` catalogue body (a JSON **object** keyed by slug) into assets, sorted by
/// download count (most popular first — the useful default for a browse grid).
pub fn parse_assets(bytes: &[u8]) -> Vec<PhAsset> {
    let v: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(obj) = v.as_object() else {
        return Vec::new();
    };
    let strs = |x: &serde_json::Value| -> Vec<String> {
        x.as_array()
            .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
            .unwrap_or_default()
    };
    let mut out: Vec<PhAsset> = obj
        .iter()
        .map(|(slug, a)| PhAsset {
            slug: slug.clone(),
            name: a["name"].as_str().unwrap_or(slug).to_string(),
            categories: strs(&a["categories"]),
            tags: strs(&a["tags"]),
            // `authors` is an object {name: role} — we only want the names.
            authors: a["authors"]
                .as_object()
                .map(|o| o.keys().cloned().collect())
                .unwrap_or_default(),
            polycount: a["polycount"].as_u64().unwrap_or(0),
            downloads: a["download_count"].as_u64().unwrap_or(0),
            thumb_url: a["thumbnail_url"].as_str().unwrap_or_default().to_string(),
        })
        .collect();
    out.sort_by(|a, b| b.downloads.cmp(&a.downloads).then_with(|| a.name.cmp(&b.name)));
    out
}

/// Does `a` match every whitespace-separated word of `query` (case-insensitive) across its name,
/// tags, categories and authors? An empty query matches everything.
pub fn matches(a: &PhAsset, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    let hay = format!(
        "{} {} {} {}",
        a.name.to_lowercase(),
        a.tags.join(" ").to_lowercase(),
        a.categories.join(" ").to_lowercase(),
        a.authors.join(" ").to_lowercase()
    );
    q.split_whitespace().all(|w| hay.contains(w))
}

/// The whole catalogue for `kind` (one cached request — the API has no server-side search).
pub fn list(kind: Kind) -> Result<Vec<PhAsset>, String> {
    let body = crate::cache::get_bytes(&format!("{API}/assets?t={}", kind.slug()), Some(TTL))?;
    Ok(parse_assets(&body))
}

/// Browse/search `kind` for `query`, capped at `want`. Filtering is **local** (the catalogue is a
/// single cached response), so repeat searches cost nothing.
pub fn search(kind: Kind, query: &str, want: usize) -> Result<Vec<PhAsset>, String> {
    let mut v = list(kind)?;
    v.retain(|a| matches(a, query));
    v.truncate(want.clamp(1, 2000));
    Ok(v)
}

/// One member of a downloadable bundle: where it goes (relative to the mount dir) and its URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhFile {
    pub rel: String,
    pub url: String,
}

/// Walk a `/files/<slug>` body to the `[section][res][fmt]` leaf and return its `url` + `include`
/// siblings. Split out from the network so the (fiddly) JSON shape is unit-testable.
pub fn parse_bundle(
    bytes: &[u8],
    section: &str,
    res: &str,
    fmt: &str,
    main_name: &str,
) -> Option<(PhFile, Vec<PhFile>)> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let leaf = &v[section][res][fmt];
    let url = leaf["url"].as_str()?.to_string();
    let main = PhFile {
        rel: main_name.to_string(),
        url,
    };
    let includes = leaf["include"]
        .as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(rel, f)| {
                    Some(PhFile {
                        rel: rel.clone(),
                        url: f["url"].as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some((main, includes))
}

/// A model's `.gltf` plus every sibling it references (`.bin` + textures), at `res`. The caller
/// writes each `PhFile` to `<dir>/<rel>` and then opens `<dir>/<main.rel>`.
pub fn model_bundle(slug: &str, res: &str) -> Result<(PhFile, Vec<PhFile>), String> {
    let body = crate::cache::get_bytes(&format!("{API}/files/{slug}"), Some(TTL))?;
    parse_bundle(&body, "gltf", res, "gltf", &format!("{slug}.gltf"))
        .ok_or_else(|| format!("no {res} glTF for {slug}"))
}

/// A texture's diffuse map URL (a plain image — no bundle).
pub fn texture_url(slug: &str, res: &str) -> Result<String, String> {
    let body = crate::cache::get_bytes(&format!("{API}/files/{slug}"), Some(TTL))?;
    let v: serde_json::Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    // Poly Haven capitalises the diffuse key; a few assets only ship `arm`/`AO`.
    for key in ["Diffuse", "diffuse", "AO", "arm"] {
        if let Some(u) = v[key][res]["jpg"]["url"].as_str() {
            return Ok(u.to_string());
        }
    }
    Err(format!("no {res} diffuse map for {slug}"))
}

/// An HDRI's **tonemapped preview** JPG (viewable with the existing decoders).
pub fn hdri_preview_url(slug: &str) -> Result<String, String> {
    let body = crate::cache::get_bytes(&format!("{API}/files/{slug}"), Some(TTL))?;
    let v: serde_json::Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    v["tonemapped"]["url"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| format!("no tonemapped preview for {slug}"))
}

/// The real `.hdr` at `res` (right-click → download, for Blender/etc).
pub fn hdri_hdr_url(slug: &str, res: &str) -> Result<String, String> {
    let body = crate::cache::get_bytes(&format!("{API}/files/{slug}"), Some(TTL))?;
    let v: serde_json::Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    v["hdri"][res]["hdr"]["url"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| format!("no {res} .hdr for {slug}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const CATALOGUE: &[u8] = br#"{
      "ArmChair_01": {"name":"Arm Chair 01","categories":["furniture"],"tags":["chair","wood"],
        "authors":{"Kirill Sannikov":"All"},"polycount":5626,"download_count":100,
        "thumbnail_url":"https://cdn.polyhaven.com/asset_img/thumbs/ArmChair_01.png"},
      "wooden_table_02": {"name":"Wooden Table 02","categories":["furniture"],"tags":["table"],
        "authors":{"Jane Doe":"All"},"polycount":900,"download_count":500,
        "thumbnail_url":"https://cdn.polyhaven.com/asset_img/thumbs/wooden_table_02.png"}
    }"#;

    #[test]
    fn parses_catalogue_sorted_by_downloads() {
        let a = parse_assets(CATALOGUE);
        assert_eq!(a.len(), 2);
        // Most-downloaded first.
        assert_eq!(a[0].slug, "wooden_table_02");
        assert_eq!(a[0].downloads, 500);
        assert_eq!(a[1].name, "Arm Chair 01");
        assert_eq!(a[1].authors, vec!["Kirill Sannikov"]);
        assert_eq!(a[1].polycount, 5626);
    }

    #[test]
    fn filename_roundtrips_the_slug() {
        let a = &parse_assets(CATALOGUE)[1];
        let f = a.filename(Kind::Models);
        assert_eq!(f, "Arm Chair 01 [ArmChair_01].gltf");
        assert_eq!(parse_slug(&f).as_deref(), Some("ArmChair_01"));
        // Textures/HDRIs land as images.
        assert!(a.filename(Kind::Textures).ends_with(".jpg"));
        assert_eq!(parse_slug("no brackets.gltf"), None);
    }

    #[test]
    fn local_search_matches_name_tags_and_author() {
        let a = parse_assets(CATALOGUE);
        let hit = |q: &str| a.iter().filter(|x| matches(x, q)).count();
        assert_eq!(hit(""), 2, "empty query matches all");
        assert_eq!(hit("chair"), 1, "tag + name");
        assert_eq!(hit("furniture"), 2, "category");
        assert_eq!(hit("kirill"), 1, "author, case-insensitive");
        // Multi-word is AND across fields — and each word matches as a substring, so "wood"
        // finds both the "wood" tag AND "Wooden Table" (prefix search for free).
        assert_eq!(hit("wood chair"), 1, "chair: 'wood' tag + name");
        assert_eq!(hit("wood table"), 1, "table: 'wood' matches 'Wooden'");
        assert_eq!(hit("chair table"), 0, "no asset is both");
    }

    #[test]
    fn parses_a_model_bundle_with_its_siblings() {
        // The shape that makes models different: the gltf is a manifest + an `include` map.
        let body = br##"{"gltf":{"2k":{"gltf":{
            "url":"https://x/wooden_table_02_2k.gltf","size":2687,
            "include":{
              "wooden_table_02.bin":{"url":"https://x/wooden_table_02.bin"},
              "textures/diff_2k.jpg":{"url":"https://x/diff_2k.jpg"}
            }}}}}"##;
        let (main, inc) = parse_bundle(body, "gltf", "2k", "gltf", "wooden_table_02.gltf").unwrap();
        assert_eq!(main.rel, "wooden_table_02.gltf");
        assert_eq!(main.url, "https://x/wooden_table_02_2k.gltf");
        assert_eq!(inc.len(), 2);
        assert!(inc.iter().any(|f| f.rel == "textures/diff_2k.jpg"));
        // A missing resolution is an error, not a panic.
        assert!(parse_bundle(body, "gltf", "8k", "gltf", "x.gltf").is_none());
    }

    #[test]
    fn kinds_and_paths() {
        let p = PathBuf::from(ROOT).join("models").join(SEARCH).join("chair");
        assert!(is_remote(&p));
        assert_eq!(rel_parts(&p), vec!["models", "search", "chair"]);
        assert_eq!(Kind::from_slug("hdris"), Some(Kind::Hdris));
        assert_eq!(Kind::from_slug("nope"), None);
        assert_eq!(Kind::Models.ext(), "gltf");
        for k in Kind::ALL {
            assert_eq!(Kind::from_slug(k.slug()), Some(k));
        }
    }
}

#[cfg(test)]
mod live {
    use super::*;
    #[test]
    #[ignore = "hits the live network"]
    fn live_browse_and_bundle() {
        for k in Kind::ALL {
            match search(k, "", 5) {
                Ok(v) => {
                    eprintln!("{} → {} assets", k.label(), v.len());
                    for a in v.iter().take(2) {
                        eprintln!("   {} [{}] by {}", a.name, a.slug, a.author_label());
                    }
                }
                Err(e) => eprintln!("{} ERROR: {e}", k.label()),
            }
        }
        match model_bundle("wooden_table_02", DEFAULT_RES) {
            Ok((m, inc)) => eprintln!("bundle: {} + {} includes", m.rel, inc.len()),
            Err(e) => eprintln!("bundle ERROR: {e}"),
        }
    }
}

#[cfg(test)]
mod e2e {
    use super::*;

    /// The end-to-end claim this whole source rests on: a model's glTF is only a manifest, so
    /// materialising it plus its `include` siblings at the right relative paths must yield a file
    /// the existing `mesh3d` loader opens with real geometry.
    #[test]
    #[ignore = "hits the live network"]
    fn downloaded_model_bundle_loads_as_a_mesh() {
        let dir = std::env::temp_dir().join("pv_ph_e2e");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let (main, includes) = model_bundle("wooden_table_02", DEFAULT_RES).expect("bundle");
        eprintln!("main={} includes={}", main.rel, includes.len());
        // Materialise exactly the way `ph_fetch` does.
        for f in &includes {
            let dest = dir.join(&f.rel);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            let bytes = crate::cache::get_bytes(&f.url, None).expect("include");
            std::fs::write(&dest, &bytes).unwrap();
            eprintln!("  wrote {} ({} bytes)", f.rel, bytes.len());
        }
        let gltf = dir.join(&main.rel);
        let bytes = crate::cache::get_bytes(&main.url, None).expect("gltf");
        std::fs::write(&gltf, &bytes).unwrap();

        let mesh = crate::decode::mesh3d::load(&gltf).expect("mesh3d loads the bundle");
        eprintln!(
            "MESH: {} verts, {} tris, radius {:.3}, texture={}",
            mesh.positions.len(),
            mesh.indices.len() / 3,
            mesh.radius,
            mesh.texture.is_some()
        );
        assert!(!mesh.positions.is_empty(), "geometry loaded");
        assert!(mesh.indices.len() >= 3, "has triangles");
        assert!(mesh.radius > 0.0, "bounding sphere computed");
    }
}
