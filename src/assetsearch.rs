//! Free **icon** + **vector-art** search, two more keyless virtual sources (mirrors
//! [`crate::imgsearch`]). Both return **SVG** assets, so opening one downloads the SVG and views it
//! locally through the existing SVG decoder — recolor / palette / Save all work like any tile.
//!
//! - **Icons** — [Iconify](https://iconify.design) (`api.iconify.design`): ~200k open-source icons
//!   across 150+ sets (Material, MDI, Tabler, Lucide, Simple Icons, …). Keyless. Each icon is
//!   fetched as an SVG (tinted light-grey so it reads on the dark grid *and* stays recolourable).
//! - **Vectors** — [Wikimedia Commons](https://commons.wikimedia.org) via the MediaWiki API,
//!   filtered to `image/svg+xml`: a huge CC / public-domain SVG library. Needs a User-Agent (the
//!   shared HTTP client sends one). Its grid thumbnail is Commons' pre-rendered PNG.
//!
//! Pure + unit-tested here; the egui/threading wiring is in `app.rs` (the `asset_*` machinery).

use std::path::Path;

/// Virtual roots. The path root selects the backend.
pub const ICONS_ROOT: &str = "<icons>";
pub const VECTORS_ROOT: &str = "<vectors>";
/// The search facet: `<icons>/search/<query>` / `<vectors>/search/<query>`.
pub const SEARCH: &str = "search";

const ICONIFY_API: &str = "https://api.iconify.design";
const COMMONS_API: &str = "https://commons.wikimedia.org/w/api.php";

/// Which backend a path/search targets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Icons,
    Vectors,
}

impl Source {
    pub fn root(self) -> &'static str {
        match self {
            Source::Icons => ICONS_ROOT,
            Source::Vectors => VECTORS_ROOT,
        }
    }
}

/// The backend a path belongs to (or `None` if it's neither root).
pub fn source_of(path: &Path) -> Option<Source> {
    if path.starts_with(ICONS_ROOT) {
        Some(Source::Icons)
    } else if path.starts_with(VECTORS_ROOT) {
        Some(Source::Vectors)
    } else {
        None
    }
}

/// Is `path` under either virtual root?
pub fn is_remote(path: &Path) -> bool {
    source_of(path).is_some()
}

/// Path components below the root (e.g. `["search", "skull"]`).
pub fn rel_parts(path: &Path) -> Vec<String> {
    let root = match source_of(path) {
        Some(s) => s.root(),
        None => return Vec::new(),
    };
    path.strip_prefix(root)
        .ok()
        .map(|p| {
            p.components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// One search result — an SVG asset from either backend.
#[derive(Clone, Debug, Default)]
pub struct AssetResult {
    pub id: String,
    pub title: String,
    pub thumb_url: String,        // grid-tile preview
    pub thumb_via_registry: bool, // true = the thumb is an SVG (decode via the registry, not `image`)
    pub download_url: String,     // the SVG to fetch + view on open
    pub page_url: String,         // source/attribution landing page
    pub license: String,          // human licence label (may be empty)
    pub attribution: String,      // creator / icon-set credit
}

impl AssetResult {
    /// A safe, readable `.svg` filename: `<title-slug> [<id8>].svg`.
    pub fn filename(&self) -> String {
        let slug: String = self
            .title
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>()
            .trim_matches('_')
            .chars()
            .take(48)
            .collect();
        let slug = if slug.is_empty() { "asset".into() } else { slug };
        let id8: String = self.id.chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect();
        format!("{slug} [{id8}].svg")
    }
}

/// Percent-encode a query for a URL (dependency-free; keeps unreserved chars).
fn enc(q: &str) -> String {
    let mut out = String::with_capacity(q.len());
    for b in q.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Strip HTML tags + collapse whitespace (Commons `extmetadata` Artist is HTML).
fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Search a backend for `query`, up to `n` results (cached 1 day).
pub fn search(source: Source, query: &str, n: usize) -> Result<Vec<AssetResult>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    match source {
        Source::Icons => search_icons(q, n),
        Source::Vectors => search_vectors(q, n),
    }
}

/// The Iconify search request URL (kept separate for the test).
fn iconify_url(query: &str, n: usize) -> String {
    format!("{ICONIFY_API}/search?query={}&limit={}", enc(query), n.clamp(1, 120))
}

/// A tinted SVG URL for an `prefix:name` icon (light grey → visible on dark, still recolourable).
fn iconify_svg_url(icon: &str) -> String {
    let path = icon.replacen(':', "/", 1); // prefix:name → prefix/name
    format!("{ICONIFY_API}/{path}.svg?height=240&color=%23dcdcdc")
}

fn search_icons(query: &str, n: usize) -> Result<Vec<AssetResult>, String> {
    let body = crate::cache::get_bytes(&iconify_url(query, n), Some(86_400))?;
    Ok(parse_iconify(&body))
}

/// Parse an Iconify `/search` body (`{"icons":["prefix:name",…]}`) into results.
pub fn parse_iconify(bytes: &[u8]) -> Vec<AssetResult> {
    let v: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(arr) = v["icons"].as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|i| {
            let icon = i.as_str()?; // "prefix:name"
            let (prefix, name) = icon.split_once(':')?;
            let url = iconify_svg_url(icon);
            Some(AssetResult {
                id: icon.to_string(),
                title: name.replace('-', " "),
                thumb_url: url.clone(),
                thumb_via_registry: true, // SVG thumbnail
                download_url: url,
                page_url: format!("https://icon-sets.iconify.design/{prefix}/{name}/"),
                license: String::new(), // per-set; varies (all open-source)
                attribution: prefix.to_string(),
            })
        })
        .collect()
}

/// The Wikimedia Commons SVG-search request URL.
fn commons_url(query: &str, n: usize) -> String {
    // generator=search over the File namespace (6), filtered to `drawing` (vector) files; imageinfo
    // gives the raw URL + a rendered PNG thumbnail + licence metadata.
    format!(
        "{COMMONS_API}?action=query&format=json&generator=search\
         &gsrsearch={}%20filetype:drawing&gsrnamespace=6&gsrlimit={}\
         &prop=imageinfo&iiprop=url%7Cmime%7Cextmetadata&iiurlwidth=320",
        enc(query),
        n.clamp(1, 120)
    )
}

fn search_vectors(query: &str, n: usize) -> Result<Vec<AssetResult>, String> {
    let body = crate::cache::get_bytes(&commons_url(query, n), Some(86_400))?;
    Ok(parse_commons(&body))
}

/// Parse a Commons `query.pages` body into SVG results (ordered by the search `index`).
pub fn parse_commons(bytes: &[u8]) -> Vec<AssetResult> {
    let v: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(pages) = v["query"]["pages"].as_object() else {
        return Vec::new();
    };
    let mut out: Vec<(i64, AssetResult)> = Vec::new();
    for (_, p) in pages {
        let ii = &p["imageinfo"][0];
        if ii["mime"].as_str() != Some("image/svg+xml") {
            continue; // only vectors
        }
        let Some(url) = ii["url"].as_str() else { continue };
        let title = p["title"].as_str().unwrap_or("Untitled");
        let clean = title.strip_prefix("File:").unwrap_or(title);
        let clean = clean.strip_suffix(".svg").unwrap_or(clean);
        let em = &ii["extmetadata"];
        out.push((
            p["index"].as_i64().unwrap_or(i64::MAX),
            AssetResult {
                id: title.to_string(),
                title: clean.to_string(),
                // Render the RAW SVG ourselves (resvg) for the tile — Commons generates PNG
                // thumbnails lazily and some are slow/time out (a stuck spinner). Decoding the SVG
                // directly is self-contained + consistent with the icon tiles.
                thumb_url: url.to_string(),
                thumb_via_registry: true,
                download_url: url.to_string(),
                page_url: ii["descriptionurl"].as_str().unwrap_or_default().to_string(),
                license: em["LicenseShortName"]["value"].as_str().unwrap_or_default().to_string(),
                attribution: strip_html(em["Artist"]["value"].as_str().unwrap_or_default()),
            },
        ));
    }
    out.sort_by_key(|(i, _)| *i);
    out.into_iter().map(|(_, r)| r).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn roots_and_paths() {
        let ip = PathBuf::from(ICONS_ROOT).join(SEARCH).join("skull");
        let vp = PathBuf::from(VECTORS_ROOT).join(SEARCH).join("cat");
        assert_eq!(source_of(&ip), Some(Source::Icons));
        assert_eq!(source_of(&vp), Some(Source::Vectors));
        assert!(is_remote(&ip) && is_remote(&vp));
        assert!(!is_remote(Path::new("/home/x")));
        assert_eq!(rel_parts(&ip), vec!["search", "skull"]);
    }

    #[test]
    fn iconify_parse_and_urls() {
        let body = br#"{"icons":["mdi:skull","material-symbols:skull-outline"],"total":2}"#;
        let r = parse_iconify(body);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].title, "skull");
        assert!(r[0].thumb_via_registry, "icons are SVG thumbnails");
        assert!(r[0].download_url.contains("/mdi/skull.svg"));
        assert!(r[0].download_url.contains("color=")); // tinted for visibility
        assert!(r[0].filename().ends_with(".svg"));
        assert_eq!(r[1].title, "skull outline"); // '-' → ' '
        assert!(iconify_url("blue sky", 999).contains("limit=120")); // clamped
    }

    #[test]
    fn commons_parse_filters_svg_and_orders() {
        let body = br#"{"query":{"pages":{
          "2":{"index":2,"title":"File:Zeta.png","imageinfo":[{"mime":"image/png","url":"x"}]},
          "1":{"index":1,"title":"File:Alpha Cat.svg","imageinfo":[{"mime":"image/svg+xml",
               "url":"https://upload/alpha.svg","thumburl":"https://upload/thumb.png",
               "descriptionurl":"https://commons/File:Alpha","extmetadata":{
                 "LicenseShortName":{"value":"CC BY-SA 3.0"},"Artist":{"value":"<a href=x>Jane</a>"}}}]}
        }}}"#;
        let r = parse_commons(body);
        assert_eq!(r.len(), 1, "PNG filtered out, only the SVG kept");
        assert_eq!(r[0].title, "Alpha Cat");
        assert!(r[0].thumb_via_registry, "we render the raw SVG ourselves (resvg)");
        assert_eq!(r[0].thumb_url, "https://upload/alpha.svg");
        assert_eq!(r[0].license, "CC BY-SA 3.0");
        assert_eq!(r[0].attribution, "Jane"); // HTML stripped
        assert_eq!(r[0].download_url, "https://upload/alpha.svg");
    }
}

#[cfg(test)]
mod live {
    use super::*;
    #[test]
    #[ignore]
    fn live() {
        for (s, q) in [(Source::Icons, "skull"), (Source::Vectors, "cat")] {
            match search(s, q, 8) {
                Ok(v) => {
                    eprintln!("{s:?} {q}: {} results", v.len());
                    for r in v.iter().take(3) {
                        eprintln!("  {} | {} | {}", r.title, r.thumb_url, r.download_url);
                    }
                }
                Err(e) => eprintln!("{s:?} ERROR: {e}"),
            }
        }
    }
}
