//! **DeviantArt** — browse public art via DeviantArt's official OAuth2 API (the *client-credentials*
//! flow: app-only, no user login). Needs a registered app's `client_id` + `client_secret` (from
//! deviantart.com/developers), stored in `secrets.json` like the Steam key. A short-lived app token
//! is minted from those and sent as `Authorization: Bearer …` on each browse request; the app.rs
//! side caches the token with its expiry and re-mints on demand.
//!
//! Browse facets → the API's real `browse/*` endpoints: **dailydeviations** (today's curated set),
//! **home** (the homepage feed), **tags** (a single tag), **topic** (a named topic). There is no
//! `browse/popular`/`browse/newest` — those were removed. Each returns
//! `{results:[deviation…], has_more, next_offset}`; a
//! deviation gives a thumbnail (`thumbs[]`), a full-view image (`content.src`), the DeviantArt page
//! (`url`, the link-back) and the author.
//!
//! Pure + unit-tested here (parse + URL building + token parse); the egui/threading + token cache
//! live in `app.rs` (the `da_*` machinery).

use std::path::{Path, PathBuf};

/// Virtual root for DeviantArt browsing.
pub const ROOT: &str = "<deviantart>";
/// The browse facet segment: `<deviantart>/browse/<facet>/<query>`.
pub const BROWSE: &str = "browse";

const API: &str = "https://www.deviantart.com/api/v1/oauth2";
const TOKEN_URL: &str = "https://www.deviantart.com/oauth2/token";

/// The browse facets (slug, label) — the **real** DeviantArt `browse/*` endpoints (there is no
/// `popular`/`newest`; those were removed). `dailydeviations`/`home` take no query; `tags`/`topic`
/// search by tag/topic name.
pub const FACETS: &[(&str, &str)] = &[
    ("dailydeviations", "Daily Deviations"),
    ("home", "Home"),
    ("tags", "Tag search"),
    ("topic", "Topic"),
];

pub fn is_remote(path: &Path) -> bool {
    path.starts_with(ROOT)
}

/// Path components below [`ROOT`].
pub fn rel_parts(path: &Path) -> Vec<String> {
    path.strip_prefix(ROOT)
        .ok()
        .map(|rest| rest.iter().filter_map(|s| s.to_str()).map(String::from).collect())
        .unwrap_or_default()
}

/// The virtual browse path for a facet + query (blank query → `-`, keeping a fixed arity).
pub fn browse_path(facet: &str, query: &str) -> PathBuf {
    let query = if query.trim().is_empty() { "-" } else { query.trim() };
    Path::new(ROOT).join(BROWSE).join(facet).join(query)
}

/// One deviation (art piece).
#[derive(Clone, Debug, PartialEq)]
pub struct DaDeviation {
    pub id: String,
    pub title: String,
    pub artist: String,
    /// The deviantart.com page (the link-back).
    pub page_url: String,
    /// A ~medium thumbnail for the grid tile.
    pub thumb_url: String,
    /// The full-view image (`content.src`, up to ~1280px) — what opening the piece shows.
    pub content_url: String,
}

impl DaDeviation {
    /// A safe, readable local filename for the downloaded full-view image.
    pub fn filename(&self) -> String {
        let ext = self
            .content_url
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .rsplit('.')
            .next()
            .filter(|e| (1..=4).contains(&e.len()) && e.chars().all(|c| c.is_ascii_alphanumeric()))
            .unwrap_or("jpg");
        let slug: String = self
            .title
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') { c } else { '_' })
            .take(48)
            .collect();
        let stem = slug.trim_matches('_');
        let stem = if stem.is_empty() { self.id.split('-').next().unwrap_or("deviation") } else { stem };
        format!("{stem}.{ext}")
    }
}

/// Percent-encode a query value.
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

/// Mint an app-only access token via the client-credentials grant. Returns `(token, expires_in_secs)`.
/// The token response is fetched **uncached** (it's a credential).
pub fn fetch_token(client_id: &str, client_secret: &str) -> Result<(String, i64), String> {
    if client_id.trim().is_empty() || client_secret.trim().is_empty() {
        return Err("Set your DeviantArt client_id + client_secret in Preferences".into());
    }
    let url = format!(
        "{TOKEN_URL}?grant_type=client_credentials&client_id={}&client_secret={}",
        enc(client_id.trim()),
        enc(client_secret.trim())
    );
    let body = crate::cache::fetch_uncached(&url)?;
    parse_token(&body)
}

/// Parse the `{access_token, expires_in, status}` token response.
pub fn parse_token(bytes: &[u8]) -> Result<(String, i64), String> {
    let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    match v["access_token"].as_str() {
        Some(t) if !t.is_empty() => Ok((t.to_string(), v["expires_in"].as_i64().unwrap_or(3600))),
        _ => Err(v["error_description"]
            .as_str()
            .unwrap_or("DeviantArt token request failed")
            .to_string()),
    }
}

/// Build a browse request URL for a facet + query + paging window. `mature_content=false` keeps it
/// SFW. `dailydeviations`/`home` take no query; `tags` needs `&tag=`, `topic` needs `&topic=`.
/// (DeviantArt's browse API has **no general keyword search** — searching by tag is the closest.)
pub fn browse_url(facet: &str, query: &str, offset: usize, limit: usize) -> String {
    let limit = limit.clamp(1, 24);
    let q = query.trim();
    match facet {
        "tags" => format!(
            "{API}/browse/tags?tag={}&limit={limit}&offset={offset}&mature_content=false",
            enc(if q.is_empty() || q == "-" { "pixelart" } else { q })
        ),
        "topic" => format!(
            "{API}/browse/topic?topic={}&limit={limit}&offset={offset}&mature_content=false",
            enc(if q.is_empty() || q == "-" { "pixel-art" } else { q })
        ),
        // dailydeviations / home (and any future no-query facet): paged by offset/limit, no query.
        _ => format!("{API}/browse/{facet}?limit={limit}&offset={offset}&mature_content=false"),
    }
}

/// A parsed browse page: the deviations, whether more exist, and the offset for the next page.
pub struct Page {
    pub items: Vec<DaDeviation>,
    pub has_more: bool,
    pub next_offset: usize,
}

/// Parse a `browse/*` JSON response.
pub fn parse(bytes: &[u8]) -> Result<Page, String> {
    let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if let Some(err) = v["error_description"].as_str() {
        return Err(err.to_string());
    }
    let has_more = v["has_more"].as_bool().unwrap_or(false);
    let next_offset = v["next_offset"].as_u64().unwrap_or(0) as usize;
    let mut items = Vec::new();
    if let Some(arr) = v["results"].as_array() {
        for r in arr {
            let Some(id) = r["deviationid"].as_str() else { continue };
            // Skip literature / deleted / journals with no viewable image.
            let content_url = r["content"]["src"].as_str().unwrap_or_default().to_string();
            let thumb_url = pick_thumb(&r["thumbs"], &content_url);
            if thumb_url.is_empty() {
                continue;
            }
            items.push(DaDeviation {
                id: id.to_string(),
                title: r["title"].as_str().unwrap_or("Untitled").to_string(),
                artist: r["author"]["username"].as_str().unwrap_or_default().to_string(),
                page_url: r["url"].as_str().unwrap_or_default().to_string(),
                thumb_url,
                content_url,
            });
        }
    }
    Ok(Page { items, has_more, next_offset })
}

/// Choose a grid thumbnail from a deviation's `thumbs` array: the largest whose width is ≤ ~400px
/// (so tiles aren't a huge download), else the biggest available, else fall back to `content`.
fn pick_thumb(thumbs: &serde_json::Value, content: &str) -> String {
    let Some(arr) = thumbs.as_array() else {
        return content.to_string();
    };
    let mut best: Option<(&str, u64)> = None;
    let mut biggest: Option<(&str, u64)> = None;
    for t in arr {
        let (Some(src), w) = (t["src"].as_str(), t["width"].as_u64().unwrap_or(0)) else {
            continue;
        };
        if biggest.is_none_or(|(_, bw)| w > bw) {
            biggest = Some((src, w));
        }
        if w <= 400 && best.is_none_or(|(_, bw)| w > bw) {
            best = Some((src, w));
        }
    }
    best.or(biggest).map(|(s, _)| s.to_string()).unwrap_or_else(|| content.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browse_url_shapes() {
        // No-query facets page by offset/limit and carry no query param.
        assert_eq!(
            browse_url("dailydeviations", "-", 0, 24),
            "https://www.deviantart.com/api/v1/oauth2/browse/dailydeviations?limit=24&offset=0&mature_content=false"
        );
        assert_eq!(
            browse_url("home", "-", 24, 24),
            "https://www.deviantart.com/api/v1/oauth2/browse/home?limit=24&offset=24&mature_content=false"
        );
        // Tag/topic search carry the query in the facet's own param.
        assert!(browse_url("tags", "pixel art", 24, 24).contains("tag=pixel%20art"));
        assert!(browse_url("topic", "pixel-art", 0, 24).contains("topic=pixel-art"));
        // An empty tag/topic falls back to a sensible default rather than erroring.
        assert!(browse_url("tags", "-", 0, 24).contains("tag=pixelart"));
        assert!(browse_url("topic", "", 0, 24).contains("topic=pixel-art"));
    }

    #[test]
    fn parses_a_browse_response() {
        let json = br#"{
          "has_more": true, "next_offset": 3,
          "results": [
            {"deviationid":"9D80","title":"Sunset Cactus","url":"https://www.deviantart.com/pixeljad/art/Sunset-Cactus-960731021",
             "author":{"username":"PIXELJAD"},
             "content":{"src":"https://img/full-1280.jpg","width":1280,"height":1280},
             "thumbs":[{"src":"https://img/t-150.jpg","width":150,"height":150},
                       {"src":"https://img/t-300.jpg","width":300,"height":300},
                       {"src":"https://img/t-900.jpg","width":900,"height":900}]},
            {"deviationid":"NOIMG","title":"a journal","author":{"username":"x"},"thumbs":[]}
          ]}"#;
        let p = parse(json).unwrap();
        assert!(p.has_more);
        assert_eq!(p.next_offset, 3);
        assert_eq!(p.items.len(), 1, "the imageless entry is skipped");
        let d = &p.items[0];
        assert_eq!(d.title, "Sunset Cactus");
        assert_eq!(d.artist, "PIXELJAD");
        assert_eq!(d.thumb_url, "https://img/t-300.jpg", "largest thumb ≤ 400px");
        assert_eq!(d.content_url, "https://img/full-1280.jpg");
        assert_eq!(d.filename(), "Sunset_Cactus.jpg");
    }

    #[test]
    fn token_parse_ok_and_err() {
        assert_eq!(
            parse_token(br#"{"status":"success","access_token":"abc","expires_in":3600}"#).unwrap(),
            ("abc".to_string(), 3600)
        );
        assert!(parse_token(br#"{"error":"x","error_description":"bad client"}"#).is_err());
    }
}
