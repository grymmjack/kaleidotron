//! Free image search via **Openverse** (`api.openverse.org`) — the Creative-Commons image
//! search WordPress runs (~800M CC/public-domain images), keyless + JSON. Mirrors the
//! virtual-source pattern of [`crate::sixteen`] / [`crate::youtube`]: a `<images>` virtual root,
//! `is_remote`/`rel_parts`, a `search()` that hits the JSON API (through the shared HTTP cache),
//! and result records the app turns into grid tiles. Opening a result downloads its image (cache-
//! first) and views it locally — so recolor / palette / Save all work on it like any tile.
//!
//! No API key is required for anonymous use (rate-limited). Everything is pure + unit-testable
//! here; the egui/threading wiring lives in `app.rs` (the `img_*` machinery, parallel to `yt_*`).

use std::path::Path;

/// Virtual root for image-search browsing.
pub const ROOT: &str = "<images>";
/// The search facet: `<images>/search/<query>`.
pub const SEARCH: &str = "search";

const API: &str = "https://api.openverse.org/v1/images/";

/// Is `path` under the image-search virtual root?
pub fn is_remote(path: &Path) -> bool {
    path.starts_with(ROOT)
}

/// The path components below [`ROOT`] (e.g. `["search", "sunset"]`).
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

/// One Openverse image result.
#[derive(Clone, Debug, Default)]
pub struct ImgResult {
    pub id: String,
    pub title: String,
    pub creator: String,
    pub license: String, // e.g. "by-nc-sa"
    pub provider: String, // e.g. "flickr", "wikimedia"
    pub img_url: String,  // the full-resolution image
    pub thumb_url: String, // an Openverse-hosted thumbnail endpoint
    pub page_url: String, // the source landing page (attribution — shown on open / right-click)
    pub width: u32,
    pub height: u32,
    pub ext: String, // "jpg" / "png" / … (inferred if the API omits `filetype`)
}

impl ImgResult {
    /// Human "CC BY-NC-SA" style label.
    pub fn license_label(&self) -> String {
        if self.license.is_empty() {
            return "—".into();
        }
        let up = self.license.to_uppercase();
        if up == "PDM" || up == "CC0" {
            up
        } else {
            format!("CC {up}")
        }
    }

    /// `WxH` (empty if unknown).
    pub fn dims(&self) -> String {
        if self.width == 0 || self.height == 0 {
            String::new()
        } else {
            format!("{}×{}", self.width, self.height)
        }
    }

    /// A safe, readable filename for the downloaded image: `<title-slug> [<id8>].<ext>`.
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
        let slug = if slug.is_empty() { "image".into() } else { slug };
        let id8: String = self.id.chars().take(8).collect();
        format!("{slug} [{id8}].{}", self.ext)
    }
}

/// Infer a file extension from the API `filetype`, else the image URL, else `jpg`.
fn infer_ext(filetype: Option<&str>, url: &str) -> String {
    if let Some(ft) = filetype {
        let ft = ft.trim().to_ascii_lowercase();
        if !ft.is_empty() {
            return if ft == "jpeg" { "jpg".into() } else { ft };
        }
    }
    let tail = url.rsplit('/').next().unwrap_or("");
    let ext = tail.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "jpg".into(),
        "png" | "gif" | "webp" | "bmp" | "tiff" | "svg" => ext,
        _ => "jpg".into(),
    }
}

/// Parse an Openverse `/v1/images/` JSON body into results.
pub fn parse_results(bytes: &[u8]) -> Vec<ImgResult> {
    let v: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(arr) = v["results"].as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|r| {
            let id = r["id"].as_str()?.to_string();
            let img_url = r["url"].as_str().unwrap_or_default().to_string();
            if img_url.is_empty() {
                return None;
            }
            let ext = infer_ext(r["filetype"].as_str(), &img_url);
            Some(ImgResult {
                id,
                title: r["title"].as_str().unwrap_or("Untitled").to_string(),
                creator: r["creator"].as_str().unwrap_or_default().to_string(),
                license: r["license"].as_str().unwrap_or_default().to_string(),
                provider: r["provider"].as_str().unwrap_or_default().to_string(),
                thumb_url: r["thumbnail"].as_str().unwrap_or(&img_url).to_string(),
                page_url: r["foreign_landing_url"].as_str().unwrap_or_default().to_string(),
                width: r["width"].as_u64().unwrap_or(0) as u32,
                height: r["height"].as_u64().unwrap_or(0) as u32,
                img_url,
                ext,
            })
        })
        .collect()
}

/// Percent-encode a query for the URL (space→%20 etc.; keeps it dependency-free).
fn enc(q: &str) -> String {
    let mut out = String::with_capacity(q.len());
    for b in q.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Search Openverse for `query`, up to `n` results (page_size ≤ 60 per the API). Uses the shared
/// HTTP cache (1-day TTL) so repeat searches are instant + offline-friendly.
pub fn search(query: &str, n: usize) -> Result<Vec<ImgResult>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let size = n.clamp(1, 60);
    // `mature=false` (default) keeps results SFW; `license_type=all-cc,commercial` widens coverage.
    let url = format!("{API}?q={}&page_size={size}", enc(q));
    let body = crate::cache::get_bytes(&url, Some(86_400))?;
    Ok(parse_results(&body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const SAMPLE: &[u8] = br#"{
      "result_count": 2, "page_count": 1, "page_size": 2, "page": 1,
      "results": [
        {"id":"1e97a259-4a7c","title":"A Sunset","creator":"krazydad","license":"by-nc-sa",
         "url":"https://live.staticflickr.com/3/4994679_b.jpg","thumbnail":"https://api.openverse.org/v1/images/1e97a259/thumb/",
         "foreign_landing_url":"https://www.flickr.com/photos/x/4994679","width":1024,"height":768,"filetype":null,"provider":"flickr"},
        {"id":"abcd1234","title":"Mountain","creator":"jane","license":"cc0",
         "url":"https://upload.wikimedia.org/pic.png","thumbnail":"https://api.openverse.org/v1/images/abcd1234/thumb/",
         "foreign_landing_url":"https://commons.wikimedia.org/x","width":800,"height":600,"filetype":"png","provider":"wikimedia"}
      ]
    }"#;

    #[test]
    fn parses_and_infers() {
        let r = parse_results(SAMPLE);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].ext, "jpg"); // filetype null → inferred from .jpg URL
        assert_eq!(r[0].dims(), "1024×768");
        assert_eq!(r[0].license_label(), "CC BY-NC-SA");
        assert_eq!(r[1].ext, "png");
        assert_eq!(r[1].license_label(), "CC0");
        assert!(r[0].filename().starts_with("A_Sunset ["));
        assert!(r[0].filename().ends_with(".jpg"));
    }

    #[test]
    fn virtual_paths() {
        let p = PathBuf::from(ROOT).join(SEARCH).join("sunset");
        assert!(is_remote(&p));
        assert_eq!(rel_parts(&p), vec!["search", "sunset"]);
        assert_eq!(enc("blue sky!"), "blue%20sky%21");
    }
}

#[cfg(test)]
mod live {
    use super::*;
    #[test]
    #[ignore]
    fn live_search_skull() {
        match search("skull", 10) {
            Ok(v) => {
                eprintln!("got {} results", v.len());
                for r in v.iter().take(3) {
                    eprintln!("  {} | {} | {}", r.title, r.thumb_url, r.img_url);
                }
            }
            Err(e) => eprintln!("ERROR: {e}"),
        }
    }
}
