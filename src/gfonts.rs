//! [Google Fonts](https://fonts.google.com) as a keyless virtual source — the missing half of
//! kaleidotron's font story. The viewer already renders TTF/OTF (metadata, glyph grid, logo maker, 3D
//! extrusion, COLR colour fonts); this is how you *find* a font in the first place.
//!
//! Two undocumented-but-stable keyless endpoints, both verified against the live service:
//!
//! * **`fonts.google.com/metadata/fonts`** — the whole catalogue (~1900 families) as plain JSON
//!   (no `)]}'` XSSI prefix). One cached call, so **search is a local filter** — instant, like
//!   [`crate::polyhaven`].
//! * **`fonts.googleapis.com/css2?family=…`** — returns a CSS `@font-face` whose `src: url(…)`
//!   is the real font binary. Google content-negotiates the format on **User-Agent**: a modern
//!   browser UA gets `woff2` (which kaleidotron can't parse), while kaleidotron's own UA gets a plain
//!   **`.ttf`**. That's why [`crate::cache`]'s User-Agent must not be spoofed to a browser here.
//!
//! Pure + unit-tested; the egui/threading wiring lives in `app.rs` (the `gf_*` machinery).

use std::path::Path;

/// Virtual root for font browsing.
pub const ROOT: &str = "<fonts>";
/// The search facet: `<fonts>/search/<query>`.
pub const SEARCH: &str = "search";

const META: &str = "https://fonts.google.com/metadata/fonts";
const CSS: &str = "https://fonts.googleapis.com/css2";
const TTL: i64 = 86_400;

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

/// One Google Fonts family.
#[derive(Clone, Debug, Default)]
pub struct GFont {
    pub family: String,
    pub category: String,      // "Sans Serif" / "Display" / "Monospace" …
    pub designers: Vec<String>,
    pub weights: Vec<String>,  // "400", "700", "400i" …
    pub subsets: Vec<String>,  // "latin", "cyrillic" …
    pub popularity: u64,       // 1 = most popular
    pub is_variable: bool,     // has variable axes
}

impl GFont {
    /// The leaf filename. The family name *is* the stem (it round-trips via [`parse_family`]),
    /// keeping the tile caption readable — "Roboto Slab.ttf", not "Roboto+Slab [id].ttf".
    pub fn filename(&self) -> String {
        let safe: String = self
            .family
            .chars()
            .map(|c| if c == '/' || c == '\\' { '_' } else { c })
            .collect();
        format!("{}.ttf", safe.trim())
    }

    pub fn page_url(&self) -> String {
        format!("https://fonts.google.com/specimen/{}", self.family.replace(' ', "+"))
    }

    /// The weight we download: regular (400) when offered, else the first listed.
    pub fn best_weight(&self) -> String {
        if self.weights.iter().any(|w| w == "400") {
            "400".into()
        } else {
            self.weights.first().cloned().unwrap_or_else(|| "400".into())
        }
    }

    pub fn designer_label(&self) -> String {
        if self.designers.is_empty() {
            "—".into()
        } else {
            self.designers.join(", ")
        }
    }
}

/// Recover the family name from a leaf filename made by [`GFont::filename`].
pub fn parse_family(filename: &str) -> Option<String> {
    let stem = filename.strip_suffix(".ttf").unwrap_or(filename).trim();
    (!stem.is_empty()).then(|| stem.to_string())
}

/// Parse the `metadata/fonts` catalogue.
pub fn parse_catalogue(bytes: &[u8]) -> Vec<GFont> {
    let v: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(arr) = v["familyMetadataList"].as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|f| {
            let family = f["family"].as_str()?.to_string();
            Some(GFont {
                family,
                category: f["category"].as_str().unwrap_or_default().to_string(),
                designers: f["designers"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|d| d.as_str().map(String::from)).collect())
                    .unwrap_or_default(),
                // `fonts` is an object keyed by weight ("400", "400i", …).
                weights: f["fonts"]
                    .as_object()
                    .map(|o| {
                        let mut w: Vec<String> = o.keys().cloned().collect();
                        w.sort();
                        w
                    })
                    .unwrap_or_default(),
                subsets: f["subsets"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
                    .unwrap_or_default(),
                popularity: f["popularity"].as_u64().unwrap_or(u64::MAX),
                is_variable: f["axes"].as_array().is_some_and(|a| !a.is_empty()),
            })
        })
        .collect()
}

/// Match every whitespace-separated word of `query` (case-insensitive) against family / category /
/// designer. Empty query matches all.
pub fn matches(f: &GFont, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    let hay = format!(
        "{} {} {}",
        f.family.to_lowercase(),
        f.category.to_lowercase(),
        f.designers.join(" ").to_lowercase()
    );
    q.split_whitespace().all(|w| hay.contains(w))
}

/// The whole catalogue, most-popular first (`popularity` is a 1-based rank).
pub fn list() -> Result<Vec<GFont>, String> {
    let body = crate::cache::get_bytes(META, Some(TTL))?;
    let mut v = parse_catalogue(&body);
    v.sort_by(|a, b| a.popularity.cmp(&b.popularity).then_with(|| a.family.cmp(&b.family)));
    Ok(v)
}

/// Search the catalogue locally for `query`, capped at `want`.
pub fn search(query: &str, want: usize) -> Result<Vec<GFont>, String> {
    let mut v = list()?;
    v.retain(|f| matches(f, query));
    v.truncate(want.clamp(1, 2000));
    Ok(v)
}

/// The css2 request URL for a family + weight.
fn css_url(family: &str, weight: &str) -> String {
    // css2 wants `+` for spaces; italic weights are e.g. "400i" → `ital,wght@1,400`.
    let fam = family.replace(' ', "+");
    if let Some(w) = weight.strip_suffix('i') {
        format!("{CSS}?family={fam}:ital,wght@1,{w}")
    } else {
        format!("{CSS}?family={fam}:wght@{weight}")
    }
}

/// Pull the first `src: url(...)` out of a css2 response. Pure so the (UA-dependent) contract is
/// testable without the network.
pub fn parse_font_url(css: &str) -> Option<String> {
    let start = css.find("url(")? + 4;
    let end = css[start..].find(')')? + start;
    let url = css[start..end].trim().trim_matches(['"', '\'']);
    url.starts_with("http").then(|| url.to_string())
}

/// Resolve a family's downloadable **TTF** URL via the css2 endpoint.
pub fn font_url(family: &str, weight: &str) -> Result<String, String> {
    let body = crate::cache::get_bytes(&css_url(family, weight), Some(TTL))?;
    let css = String::from_utf8_lossy(&body);
    let url = parse_font_url(&css).ok_or_else(|| format!("no font URL in css2 for {family}"))?;
    if !url.ends_with(".ttf") && !url.ends_with(".otf") {
        // Only happens if the User-Agent is spoofed to a browser (→ woff2), which kaleidotron
        // doesn't do — surface it clearly rather than downloading an unparseable file.
        return Err(format!("css2 returned a non-TTF font ({url}) — check the User-Agent"));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const CATALOGUE: &[u8] = br#"{"familyMetadataList":[
      {"family":"Roboto","category":"Sans Serif","designers":["Christian Robertson"],
       "fonts":{"400":{},"700":{},"400i":{}},"subsets":["latin"],"popularity":1,
       "axes":[{"tag":"wght"}]},
      {"family":"Bebas Neue","category":"Display","designers":["Ryoichi Tsunekawa"],
       "fonts":{"400":{}},"subsets":["latin"],"popularity":40,"axes":[]}
    ]}"#;

    #[test]
    fn parses_catalogue() {
        let f = parse_catalogue(CATALOGUE);
        assert_eq!(f.len(), 2);
        let r = &f[0];
        assert_eq!(r.family, "Roboto");
        assert_eq!(r.category, "Sans Serif");
        assert_eq!(r.weights, vec!["400", "400i", "700"]);
        assert!(r.is_variable);
        assert_eq!(r.best_weight(), "400");
        assert!(!f[1].is_variable);
    }

    #[test]
    fn filename_roundtrips_the_family() {
        let f = &parse_catalogue(CATALOGUE)[1];
        assert_eq!(f.filename(), "Bebas Neue.ttf");
        assert_eq!(parse_family("Bebas Neue.ttf").as_deref(), Some("Bebas Neue"));
        assert_eq!(parse_family(""), None);
    }

    #[test]
    fn local_search_matches_family_category_designer() {
        let f = parse_catalogue(CATALOGUE);
        let hit = |q: &str| f.iter().filter(|x| matches(x, q)).count();
        assert_eq!(hit(""), 2);
        assert_eq!(hit("robo"), 1);
        assert_eq!(hit("display"), 1, "category");
        assert_eq!(hit("ryoichi"), 1, "designer, case-insensitive");
        assert_eq!(hit("sans roboto"), 1, "AND across fields");
        assert_eq!(hit("roboto display"), 0);
    }

    #[test]
    fn css_urls_and_extraction() {
        assert!(css_url("Roboto Slab", "400").contains("family=Roboto+Slab:wght@400"));
        // Italic weights use the ital axis.
        assert!(css_url("Roboto", "400i").contains("ital,wght@1,400"));
        let css = "@font-face { font-family: 'Roboto';\n  src: url(https://fonts.gstatic.com/s/roboto/v51/abc.ttf) format('truetype');\n}";
        assert_eq!(
            parse_font_url(css).as_deref(),
            Some("https://fonts.gstatic.com/s/roboto/v51/abc.ttf")
        );
        assert_eq!(parse_font_url("no url here"), None);
    }

    #[test]
    fn paths() {
        let p = PathBuf::from(ROOT).join(SEARCH).join("mono");
        assert!(is_remote(&p));
        assert_eq!(rel_parts(&p), vec!["search", "mono"]);
    }
}

#[cfg(test)]
mod live {
    use super::*;
    #[test]
    #[ignore = "hits the live network"]
    fn live_catalogue_and_download_url() {
        match search("mono", 5) {
            Ok(v) => {
                eprintln!("got {} families", v.len());
                for f in v.iter().take(4) {
                    eprintln!("  {} | {} | {}", f.family, f.category, f.designer_label());
                }
            }
            Err(e) => eprintln!("ERROR: {e}"),
        }
        match font_url("Roboto", "400") {
            Ok(u) => eprintln!("ttf: {u}"),
            Err(e) => eprintln!("font_url ERROR: {e}"),
        }
    }
}
