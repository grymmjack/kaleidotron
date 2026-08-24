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

/// Lospec's own filter vocabularies (slug, label). Slugs are the site's URL PATH tokens
/// (`gallery/medium:pixel-art/sorting:top/…`) — they are NOT query params (those are ignored).
/// `all` / `latest` are the defaults and emit no path segment.
pub const MEDIUMS: &[(&str, &str)] = &[
    ("all", "All"),
    ("pixel-art", "Pixel Art"),
    ("voxel-art", "Voxel Art"),
    ("low-poly", "Low-Poly"),
    ("textmode-art", "Textmode"),
];
/// Categories are **per-medium** (the site's Category dropdown changes with Medium). `all` = none.
const PIXEL_CATEGORIES: &[(&str, &str)] = &[
    ("all", "All"),
    ("hand-pixelled", "Hand-pixelled"),
    ("enhanced-pixel-art", "Enhanced"),
    ("pixel-painting", "Pixel Painting"),
    ("low-res-render", "Low-res Render"),
    ("reduction", "Reduction"),
    ("computer-generated", "Computer-generated"),
];
const TEXTMODE_CATEGORIES: &[(&str, &str)] = &[
    ("all", "All"),
    ("ascii", "ASCII"),
    ("petscii", "PETSCII"),
    ("ansi", "ANSI"),
    ("miscii", "MISCII"),
    ("enhanced-textmode", "Enhanced"),
];
const NO_CATEGORIES: &[(&str, &str)] = &[("all", "All")];

/// The category options for a given medium slug (empty-ish for mediums with none).
pub fn categories_for(medium: &str) -> &'static [(&'static str, &'static str)] {
    match medium {
        "pixel-art" => PIXEL_CATEGORIES,
        "textmode-art" => TEXTMODE_CATEGORIES,
        _ => NO_CATEGORIES,
    }
}
pub const SORTINGS: &[(&str, &str)] = &[("latest", "Latest"), ("top", "Top"), ("likes", "Likes")];
pub const TIMES: &[(&str, &str)] = &[
    ("all", "All"),
    ("daily", "Daily"),
    ("weekly", "Weekly"),
    ("monthly", "Monthly"),
    ("yearly", "Yearly"),
];

const GALLERY: &str = "https://lospec.com/gallery";
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

/// The virtual browse path for a filter set — fixed arity so it round-trips through `rel_parts` and
/// stays a pinnable identity:
/// `<lospec-gallery>/browse/<medium>/<category>/<sorting>/<time>/<tag|->/<masterpiece 0|1>`.
pub fn browse_path(
    medium: &str,
    category: &str,
    sorting: &str,
    time: &str,
    tag: &str,
    masterpiece: bool,
) -> PathBuf {
    let tag = if tag.trim().is_empty() { "-" } else { tag.trim() };
    Path::new(ROOT)
        .join(BROWSE)
        .join(dash(medium))
        .join(dash(category))
        .join(dash(sorting))
        .join(dash(time))
        .join(tag)
        .join(if masterpiece { "1" } else { "0" })
}

/// A blank filter value becomes `-` so no path segment is empty.
fn dash(s: &str) -> &str {
    if s.trim().is_empty() { "-" } else { s.trim() }
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

/// The `Referer` we send with gallery requests — a plausible gallery URL, so Lospec's own AJAX
/// endpoint sees a same-site request (per the app author's "masquerade as the browser" ask).
const REFERER: &str = "https://lospec.com/gallery";

/// Normalise a filter value for the POST form: a blank/`-` medium/category/time becomes `all`, a
/// blank sorting becomes `latest` (the site's own defaults, sent explicitly in the form).
fn norm<'a>(v: &'a str, default: &'a str) -> &'a str {
    let v = v.trim();
    if v.is_empty() || v == "-" { default } else { v }
}

/// Scrape a gallery HTML fragment into pieces. Each tile is
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

/// Browse the gallery for a filter set, up to `want` pieces.
///
/// **Paged via POST**, because Lospec's gallery only pages that way: `GET …/load?page=N` always
/// returns page 1, but a `POST /gallery` with a `skip` offset returns the next batch. Each POST
/// returns `{"success":true,"html":"<tiles…>"}`; the `html` is scraped by [`parse`]. Filters go in
/// the form (`medium`/`category`/`sorting`/`time`/`tags`/`masterpiece`), not the URL. Deduped across
/// pages; a page that adds nothing new stops the loop.
#[allow(clippy::too_many_arguments)]
pub fn browse(
    medium: &str,
    category: &str,
    sorting: &str,
    time: &str,
    tag: &str,
    masterpiece: bool,
    want: usize,
) -> Result<Vec<GalleryPiece>, String> {
    let want = want.clamp(1, 640);
    let batches = want.div_ceil(PER_PAGE);
    let (medium, category) = (norm(medium, "all"), norm(category, "all"));
    let (sorting, time) = (norm(sorting, "latest"), norm(time, "all"));
    let tag = { let t = tag.trim(); if t == "-" { "" } else { t } };
    let mp = if masterpiece { "true" } else { "" };
    let mut out: Vec<GalleryPiece> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut first_err: Option<String> = None;
    for batch in 0..batches {
        let (page, skip) = ((batch + 1).to_string(), (batch * PER_PAGE).to_string());
        let fields: &[(&str, &str)] = &[
            ("page", &page),
            ("skip", &skip),
            ("medium", medium),
            ("category", category),
            ("sorting", sorting),
            ("time", time),
            ("artist", ""),
            ("liked-by", ""),
            ("tags", tag),
            ("masterpiece", mp),
        ];
        let body = match crate::cache::post_form(GALLERY, REFERER, fields, Some(86_400)) {
            Ok(b) => b,
            Err(e) => {
                if batch == 0 {
                    first_err = Some(e);
                }
                break;
            }
        };
        let html = extract_html(&body);
        let pieces = parse(&html);
        if pieces.is_empty() {
            break; // ran past the last batch
        }
        let before = out.len();
        for p in pieces {
            if seen.insert((p.artist.clone(), p.slug.clone())) {
                out.push(p);
            }
        }
        if out.len() == before || out.len() >= want {
            break; // nothing new (server clamped) or enough
        }
    }
    out.truncate(want);
    match first_err {
        Some(e) if out.is_empty() => Err(e),
        _ => Ok(out),
    }
}

/// The `html` field out of a `{"success":true,"html":"…"}` gallery POST response (serde unescapes it).
fn extract_html(body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["html"].as_str().map(String::from))
        .unwrap_or_default()
}

/// Fetch a piece's page and resolve its **full-resolution** image URL
/// (`cdn.lospec.com/gallery/<slug>-<id>.<ext>` — NOT the `/thumbnails/…-default` preview). The
/// gallery listing only exposes thumbnails, and the full URL carries a numeric id that isn't
/// derivable from the artist/slug, so opening a piece resolves it from the page here.
pub fn full_image_url(page_url: &str) -> Result<String, String> {
    let body = crate::cache::get_bytes_ua(page_url, crate::cache::BROWSER_UA, REFERER, Some(86_400))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_defaults_filter_values() {
        assert_eq!(norm("-", "all"), "all");
        assert_eq!(norm("", "latest"), "latest");
        assert_eq!(norm("pixel-art", "all"), "pixel-art");
        assert_eq!(norm("  top ", "latest"), "top");
    }

    #[test]
    fn extracts_html_from_post_json() {
        let body = br#"{"success":true,"html":"<a class=\"gallery-thumbnail\"><img class=\"thumbnail\" src=\"https://cdn.lospec.com/thumbnails/gallery/wynat/lake-guardians-default.png\"></a>"}"#;
        let pieces = parse(&extract_html(body));
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].slug, "lake-guardians");
        assert_eq!(pieces[0].artist, "wynat");
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
        let p = browse_path("textmode-art", "ansi", "top", "all", "", true);
        let parts = rel_parts(&p);
        assert_eq!(parts, vec!["browse", "textmode-art", "ansi", "top", "all", "-", "1"]);
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
