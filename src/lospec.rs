//! [Lospec](https://lospec.com) palette browser + downloader — another keyless virtual source. The
//! `palette-list/load` endpoint returns palettes **with their colours inline**, so we render the
//! swatch thumbnail ourselves (Lospec's own `.png` endpoint is currently broken) and can apply a
//! palette to the Recolor pane instantly; opening/clicking one also downloads its `.gpl` into the
//! palette library (pixelview already parses `.gpl`), so it persists in the Recolor palette list.
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
}

impl LospecPalette {
    /// The `.gpl` (GIMP palette) download URL — pixelview parses this directly.
    pub fn gpl_url(&self) -> String {
        format!("{DL}/{}.gpl", self.slug)
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

/// The browse/search request URL for `query` (a Lospec tag; empty = all), page `page` (1-based).
fn browse_url(query: &str, page: usize) -> String {
    format!(
        "{API}?colorNumberFilterType=any&page={page}&tag={}&sortingType=default",
        enc(query.trim())
    )
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
            Some(LospecPalette {
                title: p["title"].as_str().unwrap_or("Untitled").to_string(),
                slug,
                colors,
            })
        })
        .collect()
}

/// Browse/search Lospec for `query`, up to `want` palettes (paged in 10s; cached 1 day).
pub fn search(query: &str, want: usize) -> Result<Vec<LospecPalette>, String> {
    let want = want.clamp(1, 120);
    let pages = want.div_ceil(PER_PAGE);
    let mut out = Vec::new();
    for page in 1..=pages {
        let body = crate::cache::get_bytes(&browse_url(query, page), Some(86_400))?;
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
    Ok(out)
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
        assert!(browse_url("dark fantasy", 2).contains("tag=dark%20fantasy"));
        assert!(browse_url("", 1).contains("page=1"));
    }
}

#[cfg(test)]
mod live {
    use super::*;
    #[test]
    #[ignore]
    fn live() {
        match search("", 12) {
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
