//! **Lospec Gallery** — the community's pixel / voxel / low-poly / **textmode** art, browsed as a
//! keyless virtual source. There is no JSON API, so the `gallery/load?…` HTML is scraped for each
//! piece's thumbnail; a piece opens to its `-default.png` preview + a link back to its Lospec page
//! (the full-resolution original is access-denied — Lospec protects it).
//!
//! **Browse-only, link-back, respectful.** Lospec's gallery carries `noai/noimageai` and an explicit
//! anti-view-stealing stance. This source is a *specialised viewer*: it loads previews from Lospec's
//! own CDN (giving them the views), links every piece back to its page, and honours the shared
//! HTTP politeness layer (rate limit + Crawl-delay). It is **not** a bulk downloader or a scraper
//! for anything other than on-screen browsing.
//!
//! Mirrors the other web sources' shape (`is_remote` / `rel_parts` / a `parse` + a paged `browse`).

use std::path::{Path, PathBuf};

/// Virtual root for gallery browsing.
pub const ROOT: &str = "<lospec-gallery>";
/// The single browse facet: `<lospec-gallery>/browse/<medium>/<sorting>/<time>/<tag>`.
pub const BROWSE: &str = "browse";

/// Lospec's own filter vocabularies (slug, label). `medium` "all" and `tag` empty mean "no filter".
pub const MEDIUMS: &[(&str, &str)] = &[
    ("all", "All"),
    ("pixelart", "Pixel Art"),
    ("voxelart", "Voxel Art"),
    ("lowpoly", "Low-Poly"),
    ("textmode", "Textmode"),
];
pub const SORTINGS: &[(&str, &str)] = &[("latest", "Latest"), ("top", "Top"), ("likes", "Likes")];
pub const TIMES: &[(&str, &str)] = &[
    ("all", "All"),
    ("daily", "Daily"),
    ("weekly", "Weekly"),
    ("monthly", "Monthly"),
    ("yearly", "Yearly"),
];

const API: &str = "https://lospec.com/gallery/load";
/// Pieces per page in the gallery/load response.
pub const PER_PAGE: usize = 32;

/// Is `path` under the gallery virtual root?
pub fn is_remote(path: &Path) -> bool {
    path.starts_with(ROOT)
}

/// Path components below [`ROOT`] (e.g. `["browse", "textmode", "top", "all", ""]`).
pub fn rel_parts(path: &Path) -> Vec<String> {
    path.strip_prefix(ROOT)
        .ok()
        .map(|rest| {
            rest.iter()
                .filter_map(|s| s.to_str())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// The virtual browse path for a filter set. A blank `tag` becomes `-` so the path keeps a fixed
/// arity (and stays a valid, pinnable identity that round-trips through `rel_parts`).
pub fn browse_path(medium: &str, sorting: &str, time: &str, tag: &str) -> PathBuf {
    let tag = if tag.trim().is_empty() { "-" } else { tag.trim() };
    Path::new(ROOT)
        .join(BROWSE)
        .join(medium)
        .join(sorting)
        .join(time)
        .join(tag)
}

/// One gallery piece (a scraped thumbnail + its identity).
#[derive(Clone, Debug, PartialEq)]
pub struct GalleryPiece {
    pub title: String,
    pub artist: String,
    pub slug: String,
    /// `cdn.lospec.com/thumbnails/gallery/<artist>/<slug>-default.png` — the only fetchable image.
    pub thumb_url: String,
    /// `lospec.com/gallery/<artist>/<slug>` — the piece's page (the link-back).
    pub page_url: String,
}

impl GalleryPiece {
    /// A safe, readable local filename for the previewed image.
    pub fn filename(&self) -> String {
        let ext = self
            .thumb_url
            .rsplit('.')
            .next()
            .filter(|e| (1..=4).contains(&e.len()) && e.chars().all(|c| c.is_ascii_alphanumeric()))
            .unwrap_or("png");
        let slug: String = self
            .slug
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') { c } else { '_' })
            .collect();
        let stem = if slug.is_empty() { "piece".to_string() } else { slug };
        format!("{stem}.{ext}")
    }
}

/// Build the `gallery/load` URL for a filter set + page. `medium == "all"` and an empty `tag` are
/// omitted (Lospec treats their absence as "no filter").
pub fn browse_url(medium: &str, sorting: &str, time: &str, tag: &str, page: usize) -> String {
    let mut u = format!("{API}?page={}", page.max(1));
    if !medium.is_empty() && medium != "all" {
        u.push_str(&format!("&medium={}", enc(medium)));
    }
    if !sorting.is_empty() {
        u.push_str(&format!("&sorting={}", enc(sorting)));
    }
    if !time.is_empty() {
        u.push_str(&format!("&time={}", enc(time)));
    }
    let tag = tag.trim();
    if !tag.is_empty() && tag != "-" {
        u.push_str(&format!("&tag={}", enc(tag)));
    }
    u
}

/// Scrape a `gallery/load` HTML body into pieces. Each tile is
/// `<img class="thumbnail" src="…/thumbnails/gallery/<artist>/<slug>-default.<ext>">`; artist + slug
/// come from that path, and the piece's page URL is derived from them. Deduped by (artist, slug).
pub fn parse(html: &str) -> Vec<GalleryPiece> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for chunk in html.split("class=\"thumbnail\"").skip(1) {
        // The `src="…"` sits right after the class on the <img>. Cap the search window so a
        // malformed tag can't scan the rest of the document.
        let window = &chunk[..chunk.len().min(600)];
        let Some(src) = extract_attr(window, "src") else {
            continue;
        };
        let Some((artist, slug)) = split_thumb_url(&src) else {
            continue;
        };
        if !seen.insert((artist.clone(), slug.clone())) {
            continue;
        }
        out.push(GalleryPiece {
            title: prettify(&slug),
            page_url: format!("https://lospec.com/gallery/{artist}/{slug}"),
            thumb_url: src,
            artist,
            slug,
        });
    }
    out
}

/// Browse the gallery for a filter set, up to `want` pieces (paged in [`PER_PAGE`]s; cached 1 day).
/// A later page failing keeps the pages already collected (mirrors the palette browser).
pub fn browse(
    medium: &str,
    sorting: &str,
    time: &str,
    tag: &str,
    want: usize,
) -> Result<Vec<GalleryPiece>, String> {
    let want = want.clamp(1, 320);
    let pages = want.div_ceil(PER_PAGE);
    let mut out: Vec<GalleryPiece> = Vec::new();
    let mut first_err: Option<String> = None;
    for page in 1..=pages {
        let url = browse_url(medium, sorting, time, tag, page);
        let body = match crate::cache::get_bytes(&url, Some(86_400)) {
            Ok(b) => b,
            Err(e) => {
                if page == 1 {
                    first_err = Some(e);
                }
                break;
            }
        };
        let batch = parse(&String::from_utf8_lossy(&body));
        if batch.is_empty() {
            break;
        }
        out.extend(batch);
        if out.len() >= want {
            break;
        }
    }
    out.truncate(want);
    match first_err {
        Some(e) if out.is_empty() => Err(e),
        _ => Ok(out),
    }
}

/// Fetch a piece's page and resolve its **full-resolution** image URL
/// (`cdn.lospec.com/gallery/<slug>-<id>.<ext>` — NOT the `/thumbnails/…-default` preview). The
/// gallery listing only exposes thumbnails, and the full URL carries a numeric id that isn't
/// derivable from the artist/slug, so opening a piece resolves it from the page here.
pub fn full_image_url(page_url: &str) -> Result<String, String> {
    let body = crate::cache::get_bytes(page_url, Some(86_400))?;
    find_full_url(&String::from_utf8_lossy(&body))
        .ok_or_else(|| "no full-resolution image found on the piece page".to_string())
}

/// The first `cdn.lospec.com/gallery/<file>.<ext>` in `html` that is NOT a thumbnail / -default
/// preview — i.e. the full-resolution original.
fn find_full_url(html: &str) -> Option<String> {
    const MARK: &str = "https://cdn.lospec.com/gallery/";
    let mut rest = html;
    while let Some(i) = rest.find(MARK) {
        let after = &rest[i..];
        // A URL ends at the first quote / whitespace / angle bracket.
        let end = after
            .find(['"', '\'', ' ', '\t', '\n', '\r', '<', ')'])
            .unwrap_or(after.len());
        let url = &after[..end];
        let lower = url.to_ascii_lowercase();
        if (lower.ends_with(".png") || lower.ends_with(".gif") || lower.ends_with(".jpg") || lower.ends_with(".jpeg"))
            && !url.contains("/thumbnails/")
            && !url.contains("-default.")
        {
            return Some(url.to_string());
        }
        rest = &rest[i + MARK.len()..];
    }
    None
}

/// Pull `<name>="value"` out of a tag fragment (first match). Returns the unescaped-enough value.
fn extract_attr(fragment: &str, name: &str) -> Option<String> {
    let key = format!("{name}=\"");
    let start = fragment.find(&key)? + key.len();
    let rest = &fragment[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Parse `…/thumbnails/gallery/<artist>/<slug>-default.<ext>` → `(artist, slug)`.
fn split_thumb_url(url: &str) -> Option<(String, String)> {
    const MARK: &str = "/thumbnails/gallery/";
    let idx = url.find(MARK)? + MARK.len();
    let tail = &url[idx..];
    let (artist, rest) = tail.split_once('/')?;
    if artist.is_empty() {
        return None;
    }
    // rest = `<slug>-default.<ext>` (strip the query/hash if any, then the extension).
    let file = rest.split(['?', '#']).next().unwrap_or(rest);
    let stem = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file);
    let slug = stem.strip_suffix("-default").unwrap_or(stem);
    if slug.is_empty() {
        return None;
    }
    Some((artist.to_string(), slug.to_string()))
}

/// Turn a slug (`fuel-showcase-street`) into a readable title (`Fuel Showcase Street`).
fn prettify(slug: &str) -> String {
    let mut out = String::with_capacity(slug.len());
    let mut cap = true;
    for c in slug.chars() {
        if c == '-' || c == '_' {
            out.push(' ');
            cap = true;
        } else if cap {
            out.extend(c.to_uppercase());
            cap = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// Percent-encode a query value (unreserved chars pass through).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browse_url_omits_defaults() {
        assert_eq!(
            browse_url("all", "latest", "all", "", 1),
            "https://lospec.com/gallery/load?page=1&sorting=latest&time=all"
        );
        assert_eq!(
            browse_url("textmode", "top", "all", "skull", 2),
            "https://lospec.com/gallery/load?page=2&medium=textmode&sorting=top&time=all&tag=skull"
        );
        // A `-` placeholder tag is treated as empty.
        assert!(!browse_url("all", "latest", "all", "-", 1).contains("tag="));
    }

    #[test]
    fn parses_thumbnails_into_pieces() {
        let html = r#"
          <a href="/gallery/stewthepoo/mike"><img class="thumbnail" src="https://cdn.lospec.com/thumbnails/gallery/stewthepoo/mike-default.png"></a>
          <a href="/gallery/ruikaj/fuel-showcase-street"><img class="thumbnail" src="https://cdn.lospec.com/thumbnails/gallery/ruikaj/fuel-showcase-street-default.png"></a>
          <img class="thumbnail" src="https://cdn.lospec.com/thumbnails/gallery/stewthepoo/mike-default.png">
        "#;
        let p = parse(html);
        assert_eq!(p.len(), 2, "deduped by (artist, slug)");
        assert_eq!(p[0].artist, "stewthepoo");
        assert_eq!(p[0].slug, "mike");
        assert_eq!(p[0].title, "Mike");
        assert_eq!(p[0].page_url, "https://lospec.com/gallery/stewthepoo/mike");
        assert_eq!(p[1].slug, "fuel-showcase-street");
        assert_eq!(p[1].title, "Fuel Showcase Street");
        assert_eq!(p[1].filename(), "fuel-showcase-street.png");
    }

    #[test]
    fn rel_parts_and_browse_path_roundtrip() {
        let p = browse_path("textmode", "top", "all", "");
        let parts = rel_parts(&p);
        assert_eq!(parts, vec!["browse", "textmode", "top", "all", "-"]);
        assert!(is_remote(&p));
    }

    #[test]
    fn finds_full_image_url_skipping_thumbnails() {
        let html = r#"
          <meta property="og:image" content="https://cdn.lospec.com/gallery/dragons-keep-616129.png">
          <img class="thumbnail" src="https://cdn.lospec.com/thumbnails/gallery/namatnieks/dragons-keep-default.png">
          <img class="main" src="https://cdn.lospec.com/gallery/dragons-keep-616129.png">
        "#;
        assert_eq!(
            find_full_url(html).as_deref(),
            Some("https://cdn.lospec.com/gallery/dragons-keep-616129.png")
        );
        // Nothing but a thumbnail → no full URL.
        assert_eq!(
            find_full_url(r#"<img src="https://cdn.lospec.com/thumbnails/gallery/a/b-default.png">"#),
            None
        );
    }

    #[test]
    fn split_thumb_url_extracts_artist_and_slug() {
        assert_eq!(
            split_thumb_url("https://cdn.lospec.com/thumbnails/gallery/sabi/thermal-space-default.png"),
            Some(("sabi".into(), "thermal-space".into()))
        );
        assert_eq!(split_thumb_url("https://cdn.lospec.com/static/og.png"), None);
    }
}
