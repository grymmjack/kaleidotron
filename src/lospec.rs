//! [Lospec](https://lospec.com) palette browser + downloader — another keyless virtual source. The
//! `palette-list/load` endpoint returns palettes **with their colours inline**, so we render the
//! swatch thumbnail ourselves (Lospec's own `.png` endpoint is currently broken) and can apply a
//! palette to the Recolor pane instantly; opening/clicking one also downloads its `.gpl` into the
//! palette library (kaleidotron already parses `.gpl`), so it persists in the Recolor palette list.
//!
//! Pure + unit-tested here; the egui/threading wiring is in `app.rs` (the `lospec_*` machinery).

use std::path::Path;

/// Virtual root for palette browsing.
pub const ROOT: &str = "<palettes>";
/// The search/browse facet: `<palettes>/search/<query>` (empty query = browse all, default sort).
pub const SEARCH: &str = "search";

const API: &str = "https://lospec.com/palette-list/load";
const DL: &str = "https://lospec.com/palette-list";
const PER_PAGE: usize = 10; // the load endpoint's fixed page size
const REFERER: &str = "https://lospec.com/palette-list";

/// Cached GET through the browser UA + a lospec Referer — all Lospec requests masquerade as the
/// site's own browser so they can't be singled out and blocked (the app author's call).
fn get(url: &str) -> Result<Vec<u8>, String> {
    crate::cache::get_bytes_ua(url, crate::cache::BROWSER_UA, REFERER, Some(86_400))
}

pub fn is_remote(path: &Path) -> bool {
    path.starts_with(ROOT)
}

pub fn rel_parts(path: &Path) -> Vec<String> {
    path.strip_prefix(ROOT)
        .ok()
        .map(|p| p.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect())
        .unwrap_or_default()
}

/// One Lospec palette (title + slug + its colours).
#[derive(Clone, Debug, Default)]
pub struct LospecPalette {
    pub title: String,
    pub slug: String,
    pub colors: Vec<[u8; 3]>,
    pub description: String,   // may contain simple HTML (stripped when shown)
    pub downloads: String,     // Lospec formats this as a string ("101,790")
    pub likes: u64,
    pub tags: Vec<String>,
    pub examples: Vec<String>, // full example-art image URLs
}

impl LospecPalette {
    /// The `.gpl` (GIMP palette) download URL — kaleidotron parses this directly.
    pub fn gpl_url(&self) -> String {
        format!("{DL}/{}.gpl", self.slug)
    }
    /// The per-palette JSON (has the `author`, which the list endpoint omits).
    pub fn json_url(&self) -> String {
        format!("{DL}/{}.json", self.slug)
    }
    /// A stable library filename for the downloaded palette.
    pub fn filename(&self) -> String {
        format!("{}.gpl", self.slug)
    }
    /// Colours as RGBA (for `custom_palette` / instant apply).
    pub fn rgba(&self) -> Vec<[u8; 4]> {
        self.colors.iter().map(|c| [c[0], c[1], c[2], 255]).collect()
    }
}

/// Extract the `author` from a per-palette `.json` body (the list endpoint doesn't include it).
pub fn parse_author(bytes: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    v["author"].as_str().filter(|s| !s.is_empty()).map(|s| s.to_string())
}

/// `#rrggbb` / `rrggbb` → RGB.
fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(h, 16).ok()?;
    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
}

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

/// Palette-list filters (the site's Filtering Options: colour-count + sorting).
#[derive(Clone, Copy, Debug)]
pub struct Filters<'a> {
    /// `colorNumberFilterType`: `any` | `max` | `min` | `exact`.
    pub color_filter: &'a str,
    /// The `colorNumber` (ignored when `color_filter == "any"`).
    pub color_n: u32,
    /// `sortingType`: `default` | `alphabetical` | `downloads` | `newest`.
    pub sorting: &'a str,
}

impl Default for Filters<'_> {
    fn default() -> Self {
        Filters { color_filter: "any", color_n: 16, sorting: "default" }
    }
}

/// The browse/search request URL for `query` (a Lospec tag; empty = all), page `page` (1-based).
fn browse_url(query: &str, page: usize, f: &Filters) -> String {
    let cf = if matches!(f.color_filter, "max" | "min" | "exact") { f.color_filter } else { "any" };
    let sort = match f.sorting {
        "alphabetical" | "downloads" | "newest" => f.sorting,
        _ => "default",
    };
    let mut u = format!(
        "{API}?colorNumberFilterType={cf}&page={page}&tag={}&sortingType={sort}",
        enc(query.trim())
    );
    if cf != "any" {
        u.push_str(&format!("&colorNumber={}", f.color_n.clamp(1, 256)));
    }
    u
}

/// Parse a `palette-list/load` body into palettes.
pub fn parse(bytes: &[u8]) -> Vec<LospecPalette> {
    let v: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(arr) = v["palettes"].as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|p| {
            let slug = p["slug"].as_str()?.to_string();
            let colors: Vec<[u8; 3]> = p["colors"].as_array()?.iter().filter_map(|c| parse_hex(c.as_str()?)).collect();
            if colors.is_empty() {
                return None;
            }
            let tags = p["tags"].as_array().map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect()).unwrap_or_default();
            // Example art: each item has an `image` path relative to lospec.com.
            let examples = p["examples"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|e| e["image"].as_str())
                        .map(|img| format!("https://lospec.com/{}", img.trim_start_matches('/')))
                        .collect()
                })
                .unwrap_or_default();
            Some(LospecPalette {
                title: p["title"].as_str().unwrap_or("Untitled").to_string(),
                slug,
                colors,
                description: p["description"].as_str().unwrap_or_default().to_string(),
                downloads: p["downloads"].as_str().map(String::from).unwrap_or_else(|| p["downloads"].as_u64().map(|n| n.to_string()).unwrap_or_default()),
                likes: p["likes"].as_u64().unwrap_or(0),
                tags,
                examples,
            })
        })
        .collect()
}

/// Browse/search Lospec for `query`, up to `want` palettes (cached 1 day).
///
/// **Two paths, because Lospec's `load?tag=` index is incomplete.** A blank query browses the main
/// list via the JSON `load` endpoint (fast, colours inline). A tag query instead scrapes the
/// authoritative `/palette-list/tag/<tag>` PAGE — `load?tag=ansi` silently returns nothing while
/// `/tag/ansi` correctly lists `ansi32` etc. — then fetches each palette's `.json` for its colours
/// (the tag page has none inline).
pub fn search(query: &str, want: usize, f: &Filters) -> Result<Vec<LospecPalette>, String> {
    if query.trim().is_empty() {
        browse_all(want, f)
    } else {
        search_by_tag(query.trim(), want, f)
    }
}

/// Tag search as the **union** of both of Lospec's tag sources, deduped by slug — because neither is
/// complete on its own: the fast JSON `load?tag=` index misses some tags (e.g. "ansi" → nothing),
/// while the `/palette-list/tag/<tag>` page lists them all (incl. `ansi32`) but needs a `.json` per
/// palette for colours. Query both, take load's inline-colour results first, then fill from the tag
/// page whatever load didn't already have — so everything shows up, once.
pub fn search_by_tag(tag: &str, want: usize, f: &Filters) -> Result<Vec<LospecPalette>, String> {
    let want = want.clamp(1, 480);
    let mut out: Vec<LospecPalette> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // 1) `load?tag=` — inline colours, paged, no per-palette fetch. Complete for common tags.
    for page in 1..=want.div_ceil(PER_PAGE) {
        let Ok(body) = get(&browse_url(tag, page, f)) else {
            break;
        };
        let batch = parse(&body);
        if batch.is_empty() {
            break;
        }
        for p in batch {
            if seen.insert(p.slug.clone()) {
                out.push(p);
            }
        }
        if out.len() >= want {
            break;
        }
    }
    // 2) `/palette-list/tag/<tag>` page — the complete index; union in anything (1) missed, fetching
    //    each palette's `.json` for its colours.
    if out.len() < want {
        if let Ok(body) = get(&format!("{DL}/tag/{}", enc(tag))) {
            for slug in scrape_tag_slugs(&String::from_utf8_lossy(&body)) {
                if out.len() >= want {
                    break;
                }
                if !seen.contains(&slug) {
                    if let Ok(p) = fetch_palette(&slug) {
                        seen.insert(slug);
                        out.push(p);
                    }
                }
            }
        }
    }
    out.truncate(want);
    Ok(out)
}

/// Pull every `data-palette="<slug>"` out of a `/tag/<tag>` page (deduped, in order).
fn scrape_tag_slugs(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for chunk in html.split("data-palette=\"").skip(1) {
        if let Some(end) = chunk.find('"') {
            let slug = &chunk[..end];
            if !slug.is_empty() && seen.insert(slug.to_string()) {
                out.push(slug.to_string());
            }
        }
    }
    out
}

/// Fetch one palette's `.json` (`name` + `colors`; the tag page carries no inline colours).
fn fetch_palette(slug: &str) -> Result<LospecPalette, String> {
    let body = get(&format!("{DL}/{slug}.json"))?;
    let v: serde_json::Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    let colors: Vec<[u8; 3]> = v["colors"]
        .as_array()
        .map(|a| a.iter().filter_map(|c| parse_hex(c.as_str()?)).collect())
        .unwrap_or_default();
    if colors.is_empty() {
        return Err("no colours".into());
    }
    Ok(LospecPalette {
        title: v["name"].as_str().unwrap_or("Untitled").to_string(),
        slug: slug.to_string(),
        colors,
        description: String::new(),
        downloads: String::new(),
        likes: 0,
        tags: Vec::new(),
        examples: Vec::new(),
    })
}

/// Browse the main palette list (blank query) via the JSON `load` endpoint, up to `want` (paged in
/// 10s; cached 1 day; partial-tolerant).
fn browse_all(want: usize, f: &Filters) -> Result<Vec<LospecPalette>, String> {
    let want = want.clamp(1, 480);
    let pages = want.div_ceil(PER_PAGE);
    let mut out = Vec::new();
    let mut first_err: Option<String> = None;
    for page in 1..=pages {
        // A later page failing (Lospec 500s on some edge requests) must NOT discard the pages that
        // already succeeded — break with what we have instead of `?`-ing the whole browse away.
        let body = match get(&browse_url("", page, f)) {
            Ok(b) => b,
            Err(e) => {
                if page == 1 {
                    first_err = Some(e);
                }
                break;
            }
        };
        let batch = parse(&body);
        if batch.is_empty() {
            break;
        }
        out.extend(batch);
        if out.len() >= want {
            break;
        }
    }
    out.truncate(want);
    // Only surface an error when we got nothing AND the very first page failed — otherwise a
    // partial page-2+ failure just yields fewer results, not a broken source.
    match first_err {
        Some(e) if out.is_empty() => Err(e),
        _ => Ok(out),
    }
}

/// Render a palette's colours as a swatch-grid RGBA image (`size`×`size`), for the grid thumbnail.
/// Near-square grid; the last row's remainder is left as the background.
pub fn swatch_rgba(colors: &[[u8; 3]], size: usize) -> Vec<[u8; 4]> {
    let bg = [26u8, 26, 30, 255];
    let mut img = vec![bg; size * size];
    if colors.is_empty() {
        return img;
    }
    let cols = (colors.len() as f32).sqrt().ceil() as usize;
    let rows = colors.len().div_ceil(cols);
    let cw = size / cols;
    let ch = size / rows.max(1);
    if cw == 0 || ch == 0 {
        return img;
    }
    for (i, c) in colors.iter().enumerate() {
        let (gx, gy) = (i % cols, i / cols);
        let (x0, y0) = (gx * cw, gy * ch);
        let px = [c[0], c[1], c[2], 255];
        for y in y0..(y0 + ch).min(size) {
            for x in x0..(x0 + cw).min(size) {
                img[y * size + x] = px;
            }
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn paths_and_hex() {
        let p = PathBuf::from(ROOT).join(SEARCH).join("fantasy");
        assert!(is_remote(&p));
        assert_eq!(rel_parts(&p), vec!["search", "fantasy"]);
        assert_eq!(parse_hex("#10121c"), Some([0x10, 0x12, 0x1c]));
        assert_eq!(parse_hex("ffffff"), Some([255, 255, 255]));
        assert_eq!(parse_hex("bad"), None);
    }

    #[test]
    fn parses_load_body() {
        let body = br#"{"totalCount":2,"palettes":[
          {"title":"SLSO8","slug":"slso8","colors":["0d2b45","203c56","544e68"]},
          {"title":"Empty","slug":"empty","colors":[]}
        ]}"#;
        let ps = parse(body);
        assert_eq!(ps.len(), 1, "empty-colour palette dropped");
        assert_eq!(ps[0].title, "SLSO8");
        assert_eq!(ps[0].colors.len(), 3);
        assert_eq!(ps[0].colors[0], [0x0d, 0x2b, 0x45]);
        assert!(ps[0].gpl_url().ends_with("/slso8.gpl"));
        assert_eq!(ps[0].filename(), "slso8.gpl");
        assert_eq!(ps[0].rgba()[0], [0x0d, 0x2b, 0x45, 255]);
    }

    #[test]
    fn swatch_fills_from_colors() {
        let cols = vec![[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0]];
        let img = swatch_rgba(&cols, 64);
        assert_eq!(img.len(), 64 * 64);
        // Top-left cell is the first colour (red).
        assert_eq!(img[0], [255, 0, 0, 255]);
    }

    #[test]
    fn browse_url_encodes_tag() {
        let f = Filters::default();
        assert!(browse_url("dark fantasy", 2, &f).contains("tag=dark%20fantasy"));
        assert!(browse_url("", 1, &f).contains("page=1"));
        // Colour-count filter appends colorNumber only when not "any"; sorting is passed through.
        let g = Filters { color_filter: "max", color_n: 8, sorting: "downloads" };
        let u = browse_url("", 1, &g);
        assert!(u.contains("colorNumberFilterType=max") && u.contains("colorNumber=8"));
        assert!(u.contains("sortingType=downloads"));
        assert!(!browse_url("", 1, &f).contains("colorNumber="));
    }
}

#[cfg(test)]
mod live {
    use super::*;
    #[test]
    #[ignore]
    fn live() {
        match search("", 12, &Filters::default()) {
            Ok(v) => {
                eprintln!("got {} palettes", v.len());
                for p in v.iter().take(4) {
                    eprintln!("  {} ({} colors) {}", p.title, p.colors.len(), p.gpl_url());
                }
            }
            Err(e) => eprintln!("ERROR: {e}"),
        }
    }
}
