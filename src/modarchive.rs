//! [The Mod Archive](https://modarchive.org) — ~170,000 tracker modules as a virtual source.
//!
//! This one is almost pure upside for pixelview, because the *playback* half already exists:
//! `xmrs` handles MOD/XM/S3M/IT, bundled libxmp covers 669/FAR/OKT/MED/AMF/ULT/MTM/STM, and the
//! sample explorer rips a module's instrument bank to individual WAVs that drop onto the sample
//! pads. So a downloaded module isn't just something to listen to — it's a **sample source**.
//!
//! **Two search paths, because ModArchive splits access:**
//!
//! * **Downloads are keyless** — `downloads.php?moduleid=N` hands back the module bytes with no
//!   authentication at all. (Verified against the live service.)
//! * **The XML API needs a key** — a keyless request returns `<error>Invalid Key</error>`. Keys are
//!   free but must be requested from the ModArchive admins, so requiring one would break the
//!   zero-setup pattern every other source here follows.
//!
//! Hence [`search`] takes an `Option<&str>` key: with a key it uses the XML API (richer metadata —
//! artist, genre, size, rating); without one it parses the public search page, which lists results
//! as `…moduleid=<id>#<filename>` anchors. Both produce the same [`MaModule`].
//!
//! **Be polite.** ModArchive rate-limits aggressively — a handful of rapid requests during
//! development got this IP temporarily refused on *both* hosts. Every call here goes through the
//! shared HTTP cache with a long TTL, results are paged rather than bulk-fetched, and there is
//! deliberately no "download everything" action: unlike Poly Haven's CC0 assets, modules remain
//! their composers' copyright — the archive distributes them for listening and downloading, which
//! is what this source does.

use std::path::Path;

/// Virtual root for module browsing.
pub const ROOT: &str = "<modules>";
/// The search facet: `<modules>/search/<query>`.
pub const SEARCH: &str = "search";

const SITE: &str = "https://modarchive.org";
const API: &str = "https://api.modarchive.org/xml-tools.php";
/// Search results change rarely and we want to minimise load on the archive.
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

/// One module listing. Fields beyond `id`/`filename` are only populated on the XML-API path (the
/// public search page doesn't expose them).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MaModule {
    pub id: u64,
    pub filename: String, // e.g. "cadaver_-_chiptune-powermetal.xm"
    pub title: String,    // song title (XML only; falls back to the filename stem)
    pub artist: String,
    pub genre: String,
    pub bytes: u64,
}

impl MaModule {
    /// The keyless download URL (verified: returns the module bytes, no auth).
    pub fn download_url(&self) -> String {
        format!("https://api.modarchive.org/downloads.php?moduleid={}", self.id)
    }

    /// The module's page on modarchive.org (right-click → open in browser / attribution).
    pub fn page_url(&self) -> String {
        format!("{SITE}/index.php?request=view_by_moduleid&query={}", self.id)
    }

    /// Lower-case tracker extension (`xm`, `mod`, `it`, …), from the filename.
    pub fn ext(&self) -> String {
        self.filename
            .rsplit('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    }

    /// The leaf filename used in the virtual path. The module id is embedded so a path alone
    /// identifies the module (the same `[id]` trick the YouTube + Poly Haven sources use) — that's
    /// what lets a *pinned* result still resolve after a restart, when the session map is empty.
    pub fn leaf(&self) -> String {
        let stem = self
            .filename
            .rsplit_once('.')
            .map(|(s, _)| s)
            .unwrap_or(&self.filename);
        let safe: String = stem
            .chars()
            .map(|c| if c == '/' || c == '\\' || c == '[' || c == ']' { '_' } else { c })
            .collect();
        format!("{} [{}].{}", safe.trim(), self.id, self.ext())
    }

    /// Human size, empty when unknown.
    pub fn size_label(&self) -> String {
        if self.bytes == 0 {
            String::new()
        } else if self.bytes >= 1 << 20 {
            format!("{:.1} MB", self.bytes as f64 / (1u64 << 20) as f64)
        } else {
            format!("{:.0} KB", self.bytes as f64 / 1024.0)
        }
    }

    /// Best display name: the song title when the API gave us one, else the filename stem.
    pub fn display(&self) -> String {
        if !self.title.trim().is_empty() {
            self.title.trim().to_string()
        } else {
            self.filename
                .rsplit_once('.')
                .map(|(s, _)| s)
                .unwrap_or(&self.filename)
                .to_string()
        }
    }
}

/// Recover the module id from a leaf built by [`MaModule::leaf`].
pub fn parse_id(leaf: &str) -> Option<u64> {
    let open = leaf.rfind('[')?;
    let close = leaf[open..].find(']')? + open;
    leaf[open + 1..close].parse().ok()
}

/// Decode the handful of XML/HTML entities that show up in module titles and filenames. Hand-rolled
/// (no new dependency) in the same spirit as the VDF reader in `steam.rs`.
fn unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&apos;", "'")
}

/// Extract the text of the first `<tag>…</tag>` in `xml`.
fn tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let s = xml.find(&open)? + open.len();
    let e = xml[s..].find(&close)? + s;
    Some(&xml[s..e])
}

/// Parse the **public search page**. Results appear as anchors of the form
/// `…moduleid=<id>#<filename>`, which is the one shape the keyless path can rely on, so we scan for
/// exactly that rather than trying to model ModArchive's full page structure (which would be far
/// more brittle). De-duplicated, order preserved.
pub fn parse_search_html(html: &str) -> Vec<MaModule> {
    const PAT: &str = "moduleid=";
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut rest = html;
    while let Some(i) = rest.find(PAT) {
        rest = &rest[i + PAT.len()..];
        // id digits
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        let Ok(id) = digits.parse::<u64>() else { continue };
        // The filename rides along as the anchor fragment: `moduleid=123#name.xm`.
        let after = &rest[digits.len()..];
        let filename = if let Some(f) = after.strip_prefix('#') {
            let end = f
                .find(['"', '\'', '&', '<', ' '])
                .unwrap_or(f.len());
            unescape(&f[..end])
        } else {
            String::new()
        };
        // Only keep entries that carry a plausible tracker filename (the page also links modules
        // from navigation/sidebars without the fragment).
        if filename.is_empty() || !filename.contains('.') {
            continue;
        }
        if !seen.insert(id) {
            continue;
        }
        out.push(MaModule {
            id,
            filename,
            ..Default::default()
        });
    }
    out
}

/// Parse an XML-API search response into modules. Written tolerantly (every field beyond the id is
/// optional) because the exact element set varies by request type.
pub fn parse_search_xml(xml: &str) -> Vec<MaModule> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<module>") {
        rest = &rest[i + "<module>".len()..];
        let end = rest.find("</module>").unwrap_or(rest.len());
        let block = &rest[..end];
        rest = &rest[end..];
        let Some(id) = tag(block, "id").and_then(|s| s.trim().parse::<u64>().ok()) else {
            continue;
        };
        out.push(MaModule {
            id,
            filename: tag(block, "filename").map(unescape).unwrap_or_default(),
            title: tag(block, "songtitle").map(unescape).unwrap_or_default(),
            // The artist lives in a nested <artist_info><artist><alias> block.
            artist: tag(block, "alias").map(unescape).unwrap_or_default(),
            genre: tag(block, "genretext").map(unescape).unwrap_or_default(),
            bytes: tag(block, "bytes").and_then(|s| s.trim().parse().ok()).unwrap_or(0),
        });
    }
    out
}

/// Is this an error response (e.g. a missing/rejected API key)? Returns the message.
pub fn xml_error(xml: &str) -> Option<String> {
    tag(xml, "error").map(|e| unescape(e.trim()))
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

/// The public search-page URL (keyless path), page `page` (1-based).
fn html_url(query: &str, page: usize) -> String {
    format!(
        "{SITE}/index.php?request=search&query={}&submit=Find&search_type=filename_or_songtitle&page={page}",
        enc(query)
    )
}

/// The XML-API search URL (keyed path).
fn api_url(key: &str, query: &str, page: usize) -> String {
    format!(
        "{API}?key={}&request=search&type=filename_or_songtitle&query={}&page={page}",
        enc(key),
        enc(query)
    )
}

/// Search ModArchive for `query`, up to `want` modules.
///
/// With `key` present the XML API is used (richer metadata); otherwise the public search page is
/// parsed. On a keyed request that comes back with an `<error>` (bad/expired key) this
/// **falls back to the keyless path** rather than failing outright — a stale key shouldn't break
/// browsing entirely.
pub fn search(query: &str, key: Option<&str>, want: usize) -> Result<Vec<MaModule>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let want = want.clamp(1, 400);
    let mut out: Vec<MaModule> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // The archive pages results ~40 at a time on both paths.
    for page in 1..=want.div_ceil(40).clamp(1, 10) {
        let batch = match key.map(str::trim).filter(|k| !k.is_empty()) {
            Some(k) => {
                let body = crate::cache::get_bytes(&api_url(k, q, page), Some(TTL))?;
                let text = String::from_utf8_lossy(&body);
                match xml_error(&text) {
                    // Bad key → degrade to the keyless path instead of erroring out.
                    Some(_) => {
                        let body = crate::cache::get_bytes(&html_url(q, page), Some(TTL))?;
                        parse_search_html(&String::from_utf8_lossy(&body))
                    }
                    None => parse_search_xml(&text),
                }
            }
            None => {
                let body = crate::cache::get_bytes(&html_url(q, page), Some(TTL))?;
                parse_search_html(&String::from_utf8_lossy(&body))
            }
        };
        if batch.is_empty() {
            break;
        }
        for m in batch {
            if seen.insert(m.id) {
                out.push(m);
            }
        }
        if out.len() >= want {
            break;
        }
    }
    out.truncate(want);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The exact anchor shape observed on the live search page: the *download* link, which carries
    /// the filename as a URL fragment (so a browser saves it under a readable name) — that fragment
    /// is what gives the keyless path a filename without a second request per result.
    const HTML: &str = r#"
      <div class="module-listing">
        <a href="https://api.modarchive.org/downloads.php?moduleid=182836#cadaver_-_chiptune-powermetal.xm">x</a>
      </div>
      <div class="module-listing">
        <a href="https://api.modarchive.org/downloads.php?moduleid=36685#chip2001.xm">y</a>
      </div>
      <!-- a duplicate + a fragment-less nav link that must both be ignored -->
      <a href="https://api.modarchive.org/downloads.php?moduleid=36685#chip2001.xm">dup</a>
      <a href="index.php?request=view_random&amp;moduleid=999">nav</a>
    "#;

    #[test]
    fn parses_search_page_anchors() {
        let m = parse_search_html(HTML);
        assert_eq!(m.len(), 2, "deduped, fragment-less nav link skipped");
        assert_eq!(m[0].id, 182836);
        assert_eq!(m[0].filename, "cadaver_-_chiptune-powermetal.xm");
        assert_eq!(m[0].ext(), "xm");
        assert_eq!(m[1].id, 36685);
    }

    #[test]
    fn leaf_roundtrips_the_id() {
        let m = MaModule {
            id: 182836,
            filename: "cadaver_-_chiptune-powermetal.xm".into(),
            ..Default::default()
        };
        assert_eq!(m.leaf(), "cadaver_-_chiptune-powermetal [182836].xm");
        assert_eq!(parse_id(&m.leaf()), Some(182836));
        assert_eq!(parse_id("no-brackets.xm"), None);
        // Download URL is the keyless endpoint.
        assert!(m.download_url().contains("downloads.php?moduleid=182836"));
    }

    #[test]
    fn parses_xml_and_detects_the_key_error() {
        let xml = r#"<modarchive><module>
             <id>60395</id><filename>a.xm</filename><songtitle>Song &amp; Dance</songtitle>
             <artist_info><artist><alias>Cadaver</alias></artist></artist_info>
             <bytes>278986</bytes><genretext>Chiptune</genretext>
           </module><module><id>2</id><filename>b.mod</filename></module></modarchive>"#;
        let m = parse_search_xml(xml);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].title, "Song & Dance", "entities decoded");
        assert_eq!(m[0].artist, "Cadaver");
        assert_eq!(m[0].genre, "Chiptune");
        assert_eq!(m[0].size_label(), "272 KB");
        // Missing fields are tolerated.
        assert_eq!(m[1].artist, "");
        assert_eq!(m[1].display(), "b", "falls back to the filename stem");
        // The real keyless-API response.
        assert_eq!(
            xml_error("<modarchive><error>Invalid Key</error></modarchive>").as_deref(),
            Some("Invalid Key")
        );
        assert_eq!(xml_error(xml), None);
    }

    #[test]
    fn paths_and_urls() {
        let p = PathBuf::from(ROOT).join(SEARCH).join("chiptune");
        assert!(is_remote(&p));
        assert_eq!(rel_parts(&p), vec!["search", "chiptune"]);
        assert!(html_url("drum solo", 2).contains("query=drum%20solo"));
        assert!(html_url("x", 3).contains("page=3"));
        assert!(api_url("K", "x", 1).contains("key=K"));
    }
}

#[cfg(test)]
mod live {
    use super::*;
    /// NB ModArchive rate-limits hard — run this sparingly, never in a loop.
    #[test]
    #[ignore = "hits the live network (rate-limited service)"]
    fn live_search_and_download() {
        match search("chiptune", None, 8) {
            Ok(v) => {
                eprintln!("got {} modules", v.len());
                for m in v.iter().take(5) {
                    eprintln!("  [{}] {} ({})", m.id, m.filename, m.ext());
                }
                if let Some(m) = v.first() {
                    match crate::cache::get_bytes(&m.download_url(), None) {
                        Ok(b) => eprintln!("downloaded {} bytes of {}", b.len(), m.filename),
                        Err(e) => eprintln!("download ERROR: {e}"),
                    }
                }
            }
            Err(e) => eprintln!("ERROR: {e}"),
        }
    }
}
