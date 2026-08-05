//! **HTTP filesystem browser** — point pixelview at any URL and browse it like a folder tree, the
//! way Total Commander's FS plugins do.
//!
//! HTTP has no standard directory listing, so the practical target is the **auto-index page** that
//! Apache / nginx / lighttpd / fancyindex generate — which is what most file archives actually serve.
//! Rather than modelling each server's markup, the parser scans for `<a href="…">` anchors and
//! filters out the non-entries (sort links, parent links, anchors, off-host links); a trailing `/`
//! on the href is the directory signal, and any size/date text after the anchor is picked up
//! best-effort. That one rule covers all three real layouts this was verified against:
//!
//! ```text
//! Apache   <td><a href="=README">=README</a></td><td …>2001-01-25 11:18</td><td …>1.5K</td>
//! fancy    <td class="link"><a href="AHX/" title="AHX">AHX/</a></td><td class="size">-</td>
//! nginx    <a href="nginx-0.1.0.tar.gz">nginx-0.1.0.tar.gz</a>   05-Oct-2004 15:39   220038
//! ```
//!
//! Virtual paths are `<web>/<host>/<segments…>`, which keeps breadcrumbs readable
//! (`<web>/modland.com/pub/modules/AHX`). The scheme isn't in the path: [`url_for`] builds `https://`
//! by default and the caller retries `http://` for plain-HTTP servers.
//!
//! Not implemented (deliberately): **WebDAV** `PROPFIND` and **FTP**. WebDAV needs a live server to
//! verify the XML shape against and FTP needs a new dependency; both are natural follow-ups.

use std::path::Path;

/// Virtual root for HTTP browsing.
pub const ROOT: &str = "<web>";

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

/// One entry in a remote directory listing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WebEntry {
    pub name: String, // decoded (`AM Composer`, not `AM%20Composer`)
    pub is_dir: bool,
    pub size: u64, // 0 when unknown / not shown
}

/// Percent-decode a URL path segment (`AM%20Composer` → `AM Composer`).
pub fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encode one path segment for a URL (keeps unreserved chars; `/` is NOT preserved — this
/// encodes a single segment, so a name containing a slash can't escape its directory).
pub fn pct_encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Decode the few HTML entities that appear in listing markup.
fn unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

/// Strip HTML tags from a fragment, leaving its text.
///
/// A removed tag becomes a **space**, not nothing: adjacent cells like
/// `<td>31038</td><td>2004-Sep-13</td>` would otherwise concatenate into `310382004-Sep-13`, and the
/// size token would stop parsing. (Caught against modland's real markup — nginx has no tags between
/// its columns and Apache happens to follow the size with an `&nbsp;` cell, so both hid this.)
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '<' => {
                depth += 1;
                out.push(' ');
            }
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    unescape(&out)
}

/// Parse a size token: raw bytes (`220038`) or a human suffix (`1.5K`, `12M`). `None` for `-`, `""`
/// or anything unrecognised.
fn parse_size(tok: &str) -> Option<u64> {
    let t = tok.trim();
    if t.is_empty() || t == "-" {
        return None;
    }
    let (num, mult) = match t.chars().last()?.to_ascii_uppercase() {
        'K' => (&t[..t.len() - 1], 1024f64),
        'M' => (&t[..t.len() - 1], 1024f64 * 1024.0),
        'G' => (&t[..t.len() - 1], 1024f64 * 1024.0 * 1024.0),
        'T' => (&t[..t.len() - 1], 1024f64.powi(4)),
        'B' => (&t[..t.len() - 1], 1.0),
        _ => (t, 1.0),
    };
    let v: f64 = num.trim().parse().ok()?;
    (v >= 0.0).then_some((v * mult) as u64)
}

/// Should this href be skipped (not a real child entry)?
fn skip_href(href: &str) -> bool {
    href.is_empty()
        // Sort/query links (Apache's `?C=N;O=D`), fragments, and absolute-root/parent links.
        || href.starts_with('?')
        || href.starts_with('#')
        || href == "/"
        || href == "../"
        || href == ".."
        || href == "./"
        || href == "."
        // Off-site links (banners, "powered by", mailto).
        || href.contains("://")
        || href.starts_with("mailto:")
        // An absolute path can't be resolved as a child of the current dir.
        || href.starts_with('/')
}

/// Parse a server-generated directory index into entries.
///
/// Deliberately markup-agnostic: it scans anchors and uses the **href** (not the link text) for the
/// name, because Apache truncates long names in the visible text (`someverylongname..&gt;`) while
/// the href stays complete.
pub fn parse_listing(html: &str) -> Vec<WebEntry> {
    let mut out: Vec<WebEntry> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut rest = html;
    while let Some(i) = rest.find("<a ") {
        rest = &rest[i..];
        // href="…"
        let Some(hs) = rest.find("href=\"") else { break };
        let after = &rest[hs + 6..];
        let Some(he) = after.find('"') else { break };
        let href = &after[..he];
        // The chunk between this anchor's end and the next anchor holds the size/date columns.
        let tail_start = after[he..].find("</a>").map(|p| he + p + 4).unwrap_or(he);
        let tail = &after[tail_start..];
        let tail = &tail[..tail.find("<a ").unwrap_or(tail.len())];
        rest = &after[he..];

        if skip_href(href) {
            continue;
        }
        let is_dir = href.ends_with('/');
        let name = pct_decode(unescape(href).trim_end_matches('/'));
        if name.is_empty() || name.contains('/') {
            continue; // not a direct child
        }
        // "Parent Directory" rows sometimes use a real href — catch them by name too.
        if name.eq_ignore_ascii_case("parent directory") {
            continue;
        }
        if !seen.insert(name.clone()) {
            continue;
        }
        // Best-effort size: the last token of the post-anchor text that parses as a size.
        let size = strip_tags(tail)
            .split_whitespace()
            .filter_map(parse_size)
            .next_back()
            .unwrap_or(0);
        out.push(WebEntry {
            name,
            is_dir,
            size: if is_dir { 0 } else { size },
        });
    }
    out
}

/// Does this look like a directory index at all (vs. a normal web page)? Used to tell "browse" from
/// "this URL is a file/page" and to give a useful message instead of an empty folder.
pub fn looks_like_listing(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    lower.contains("index of ")
        || lower.contains("parent directory")
        || lower.contains("<title>directory listing")
        || parse_listing(html).iter().filter(|e| e.is_dir).count() >= 2
}

/// Build the URL for a virtual path's parts (`[host, seg, …]`). `http` selects plain HTTP for
/// servers without TLS. A trailing slash is added for directory browsing.
pub fn url_for(parts: &[String], http: bool, dir: bool) -> String {
    let scheme = if http { "http" } else { "https" };
    let mut u = format!("{scheme}://{}", parts.first().cloned().unwrap_or_default());
    for seg in parts.iter().skip(1) {
        u.push('/');
        u.push_str(&pct_encode_segment(seg));
    }
    if dir && !u.ends_with('/') {
        u.push('/');
    }
    u
}

/// Turn a user-typed URL into a virtual path's parts (`["modland.com", "pub", "modules"]`).
/// Accepts input with or without a scheme. Returns `(parts, is_http)`.
pub fn parts_for_url(input: &str) -> Option<(Vec<String>, bool)> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    let (is_http, rest) = if let Some(r) = s.strip_prefix("https://") {
        (false, r)
    } else if let Some(r) = s.strip_prefix("http://") {
        (true, r)
    } else if s.contains("://") {
        return None; // some other protocol (ftp:, file:) — not handled here
    } else {
        (false, s)
    };
    // Drop any query/fragment, then split into segments.
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let parts: Vec<String> = rest
        .split('/')
        .filter(|p| !p.is_empty())
        .map(pct_decode)
        .collect();
    let host = parts.first()?;
    // A bare word isn't a host; require at least a dot or a localhost-ish name.
    if !host.contains('.') && !host.starts_with("localhost") {
        return None;
    }
    Some((parts, is_http))
}

/// Fetch and parse a directory listing, trying HTTPS first then HTTP. Returns the entries plus
/// whether plain HTTP was needed (so the caller can remember it for this host).
pub fn fetch_listing(parts: &[String], prefer_http: bool) -> Result<(Vec<WebEntry>, bool), String> {
    let mut last = String::new();
    // Try the preferred scheme first, then the other one.
    for http in [prefer_http, !prefer_http] {
        let url = url_for(parts, http, true);
        match crate::cache::get_bytes(&url, Some(3600)) {
            Ok(body) => {
                let html = String::from_utf8_lossy(&body);
                let entries = parse_listing(&html);
                if entries.is_empty() && !looks_like_listing(&html) {
                    return Err("Not a browsable directory listing".into());
                }
                return Ok((entries, http));
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// One file found by [`enumerate`], with its path segments relative to the starting directory
/// (empty for a file directly in it) — used to mirror the remote tree locally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundFile {
    pub rel: Vec<String>,
    pub name: String,
    pub size: u64,
}

/// Depth ceiling for a recursive walk — a guard against a mis-configured server that links a
/// directory back into itself (a symlink loop renders as an infinitely deep tree).
pub const MAX_DEPTH: usize = 12;

/// Enumerate the files under `parts`, optionally recursing, keeping only names matching `mask`
/// (a [`glob_any`] mask set; blank = everything). `cancel` is polled between requests so a long
/// crawl stops promptly. Directory *names* are never mask-filtered — otherwise `*.zip` would refuse
/// to descend into any folder and recursion would find nothing.
///
/// Visited URLs are tracked so a self-referential listing can't loop forever.
pub fn enumerate(
    parts: &[String],
    http: bool,
    mask: &str,
    recursive: bool,
    cancel: &std::sync::atomic::AtomicBool,
    mut on_file: impl FnMut(FoundFile),
) -> Result<(), String> {
    use std::sync::atomic::Ordering::Relaxed;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    // (relative segments, depth)
    let mut queue: std::collections::VecDeque<(Vec<String>, usize)> =
        std::collections::VecDeque::new();
    queue.push_back((Vec::new(), 0));
    let mut first_err: Option<String> = None;
    while let Some((rel, depth)) = queue.pop_front() {
        if cancel.load(Relaxed) {
            return Ok(());
        }
        let mut full = parts.to_vec();
        full.extend(rel.iter().cloned());
        if !seen.insert(url_for(&full, http, true)) {
            continue; // already walked (loop guard)
        }
        let items = match fetch_listing(&full, http) {
            Ok((v, _)) => v,
            Err(e) => {
                // A single unreadable sub-directory shouldn't abort the whole crawl; only the
                // very first failure (the starting directory) is fatal.
                if rel.is_empty() {
                    return Err(e);
                }
                first_err.get_or_insert(e);
                continue;
            }
        };
        for it in items {
            if cancel.load(Relaxed) {
                return Ok(());
            }
            if it.is_dir {
                if recursive && depth < MAX_DEPTH {
                    let mut sub = rel.clone();
                    sub.push(it.name);
                    queue.push_back((sub, depth + 1));
                }
            } else if glob_any(mask, &it.name) {
                on_file(FoundFile {
                    rel: rel.clone(),
                    name: it.name,
                    size: it.size,
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real markup captured from the live servers this was built against.
    const APACHE: &str = r#"
      <tr><th><a href="?C=N;O=D">Name</a></th><th><a href="?C=S;O=A">Size</a></th></tr>
      <tr><td><img src="/icons/back.gif" alt="[PARENTDIR]"></td><td><a href="/">Parent Directory</a></td><td align="right">  - </td></tr>
      <tr><td><img src="/icons/folder.gif" alt="[DIR]"></td><td><a href="3dldf/">3dldf/</a></td><td align="right">2013-12-13 14:00  </td><td align="right">  - </td></tr>
      <tr><td><img alt="[   ]"></td><td><a href="=README">=README</a></td><td align="right">2001-01-25 11:18  </td><td align="right">1.5K</td></tr>
    "#;

    const FANCY: &str = r#"
      <tr><td class="link"><a href="AHX/" title="AHX">AHX/</a></td><td class="size">-</td><td class="date">2022-Dec-17 17:49</td></tr>
      <tr><td class="link"><a href="AM%20Composer/" title="AM Composer">AM Composer/</a></td><td class="size">-</td><td class="date">2007-Mar-03 16:55</td></tr>
    "#;

    const NGINX: &str = r#"<h1>Index of /download/</h1><hr><pre><a href="../">../</a>
<a href="nginx-0.1.0.tar.gz">nginx-0.1.0.tar.gz</a>                 05-Oct-2004 15:39              220038
<a href="nginx-0.1.1.tar.gz">nginx-0.1.1.tar.gz</a>                 11-Oct-2004 15:06              224533
</pre>"#;

    #[test]
    fn parses_apache_listing() {
        let e = parse_listing(APACHE);
        assert_eq!(e.len(), 2, "sort links + Parent Directory skipped");
        assert_eq!(e[0], WebEntry { name: "3dldf".into(), is_dir: true, size: 0 });
        assert_eq!(e[1].name, "=README");
        assert!(!e[1].is_dir);
        assert_eq!(e[1].size, 1536, "1.5K → bytes");
    }

    /// Real modland markup for a *file* row: the size cell is immediately followed by a date cell
    /// with no whitespace between the tags. Regression guard for the strip_tags separator bug —
    /// without it the tokens merge (`310382004-Sep-13`) and the size silently reads 0.
    const FANCY_FILE: &str = r#"
      <tr><td class="link"><a href="krymini%20jingle%201.amc" title="krymini jingle 1.amc">krymini jingle 1.amc</a></td><td class="size">              31038</td><td class="date">2004-Sep-13 17:49</td></tr>
    "#;

    #[test]
    fn adjacent_table_cells_do_not_merge_tokens() {
        let e = parse_listing(FANCY_FILE);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].name, "krymini jingle 1.amc");
        assert!(!e[0].is_dir);
        assert_eq!(e[0].size, 31038, "size must survive the adjacent date cell");
    }

    #[test]
    fn parses_fancyindex_and_decodes_names() {
        let e = parse_listing(FANCY);
        assert_eq!(e.len(), 2);
        assert!(e.iter().all(|x| x.is_dir));
        // The percent-encoded href must become a readable name — this is the modland case.
        assert_eq!(e[1].name, "AM Composer");
    }

    #[test]
    fn parses_nginx_pre_listing() {
        let e = parse_listing(NGINX);
        assert_eq!(e.len(), 2, "../ skipped");
        assert_eq!(e[0].name, "nginx-0.1.0.tar.gz");
        assert!(!e[0].is_dir);
        assert_eq!(e[0].size, 220038, "raw byte size");
    }

    #[test]
    fn sizes_and_junk_hrefs() {
        assert_eq!(parse_size("1.5K"), Some(1536));
        assert_eq!(parse_size("220038"), Some(220038));
        assert_eq!(parse_size("12M"), Some(12 * 1024 * 1024));
        assert_eq!(parse_size("-"), None);
        assert_eq!(parse_size(""), None);
        for h in ["?C=N;O=D", "#top", "/", "../", "https://elsewhere.org/x", "mailto:a@b", "/abs/path"] {
            assert!(skip_href(h), "{h} should be skipped");
        }
        assert!(!skip_href("sub/"));
        assert!(!skip_href("file.txt"));
    }

    #[test]
    fn url_and_path_roundtrip() {
        let (parts, http) = parts_for_url("https://modland.com/pub/modules").unwrap();
        assert_eq!(parts, vec!["modland.com", "pub", "modules"]);
        assert!(!http);
        assert_eq!(url_for(&parts, false, true), "https://modland.com/pub/modules/");
        // A name with a space must re-encode when it goes back out as a URL.
        let with_space = vec!["modland.com".to_string(), "pub".into(), "AM Composer".into()];
        assert_eq!(url_for(&with_space, false, true), "https://modland.com/pub/AM%20Composer/");
        // Scheme-less input and http:// input.
        assert_eq!(parts_for_url("example.com/a").unwrap().0, vec!["example.com", "a"]);
        assert!(parts_for_url("http://example.com").unwrap().1);
        // Rejections: no host, unsupported scheme.
        assert!(parts_for_url("notahost").is_none());
        assert!(parts_for_url("ftp://example.com").is_none());
        assert!(parts_for_url("").is_none());
    }

    #[test]
    fn detects_a_listing_page() {
        assert!(looks_like_listing(NGINX));
        assert!(looks_like_listing(APACHE));
        assert!(!looks_like_listing("<html><body><p>Just a page</p></body></html>"));
    }

    #[test]
    fn virtual_paths() {
        let p = std::path::PathBuf::from(ROOT).join("modland.com").join("pub");
        assert!(is_remote(&p));
        assert_eq!(rel_parts(&p), vec!["modland.com", "pub"]);
    }
}

#[cfg(test)]
mod live {
    use super::*;
    #[test]
    #[ignore = "hits the live network"]
    fn live_listings() {
        for url in [
            "https://ftp.gnu.org/gnu",
            "https://nginx.org/download",
            "https://modland.com/pub/modules",
        ] {
            let (parts, http) = parts_for_url(url).unwrap();
            match fetch_listing(&parts, http) {
                Ok((e, _)) => {
                    let dirs = e.iter().filter(|x| x.is_dir).count();
                    eprintln!("{url} → {} entries ({dirs} dirs)", e.len());
                    for x in e.iter().take(3) {
                        eprintln!("   {}{}  {}", x.name, if x.is_dir { "/" } else { "" }, x.size);
                    }
                }
                Err(e) => eprintln!("{url} ERROR: {e}"),
            }
        }
    }
}

/// Match `name` against a shell-style wildcard `pattern` (`*` = any run, `?` = one char),
/// case-insensitively. Total Commander's select-by-mask also accepts several masks separated by
/// `;` (and `|` to exclude), so [`glob_any`] layers that on top.
///
/// Iterative with backtracking rather than recursive, so a pathological pattern like `*a*a*a*…`
/// can't blow the stack on a hostile listing.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    let (mut pi, mut ni) = (0usize, 0usize);
    // Where to resume if the current `*` guess fails.
    let (mut star, mut resume) = (usize::MAX, 0usize);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            resume = ni;
            pi += 1; // try matching zero chars first
        } else if star != usize::MAX {
            pi = star + 1; // backtrack: let the `*` swallow one more char
            resume += 1;
            ni = resume;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Match against a Total-Commander-style mask set: `*.zip;*.rar` (include any), with an optional
/// `|` section listing exclusions — `*.*|*.tmp;*.bak`. An empty/blank mask matches everything.
pub fn glob_any(mask: &str, name: &str) -> bool {
    let mask = mask.trim();
    if mask.is_empty() {
        return true;
    }
    let (inc, exc) = match mask.split_once('|') {
        Some((a, b)) => (a, b),
        None => (mask, ""),
    };
    let listed = |set: &str, name: &str| {
        set.split(';')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .any(|p| glob_match(p, name))
    };
    if !exc.trim().is_empty() && listed(exc, name) {
        return false;
    }
    let inc = inc.trim();
    inc.is_empty() || listed(inc, name)
}

#[cfg(test)]
mod glob_tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(glob_match("*.zip", "pack.zip"));
        assert!(glob_match("*.ZIP", "pack.zip"), "case-insensitive");
        assert!(!glob_match("*.zip", "pack.rar"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("a?c.txt", "abc.txt"));
        assert!(!glob_match("a?c.txt", "ac.txt"));
        assert!(glob_match("ac*dc*", "acldcx"));
        assert!(glob_match("*mod*", "a.mod.bak"));
        assert!(glob_match("exact", "exact"));
        assert!(!glob_match("exact", "exact2"));
        // Backtracking: the first `*` must give ground for the tail to match.
        assert!(glob_match("*a*b", "xxaxxb"));
        assert!(!glob_match("*a*b", "xxaxx"));
        // Trailing stars collapse.
        assert!(glob_match("abc***", "abc"));
    }

    #[test]
    fn mask_sets_and_exclusions() {
        assert!(glob_any("*.zip;*.rar", "x.rar"));
        assert!(!glob_any("*.zip;*.rar", "x.txt"));
        assert!(glob_any("", "anything"), "blank matches all");
        assert!(glob_any("   ", "anything"));
        // `|` excludes.
        assert!(glob_any("*.*|*.tmp", "a.txt"));
        assert!(!glob_any("*.*|*.tmp", "a.tmp"));
        assert!(!glob_any("*|*.bak;*.tmp", "x.bak"));
        // Exclusion-only mask still includes everything else.
        assert!(glob_any("|*.tmp", "keep.me"));
    }
}

#[cfg(test)]
mod live_recursive {
    use super::*;
    /// Recursive crawl + mask filtering against a real server. Deliberately points at a tiny
    /// sub-tree so the test stays polite.
    #[test]
    #[ignore = "hits the live network"]
    fn recursive_enumerate_with_mask() {
        let (parts, http) = parts_for_url("https://modland.com/pub/modules/AM Composer").unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let mut found = Vec::new();
        enumerate(&parts, http, "*", true, &cancel, |f| found.push(f)).unwrap();
        eprintln!("RECURSIVE: {} file(s)", found.len());
        for f in found.iter().take(5) {
            eprintln!("   {}/{}  ({} bytes)", f.rel.join("/"), f.name, f.size);
        }
        assert!(!found.is_empty(), "recursion descended into sub-directories");
        assert!(found.iter().any(|f| !f.rel.is_empty()), "found nested files");

        // Now with a mask that should exclude everything.
        let mut none = Vec::new();
        enumerate(&parts, http, "*.zzz", true, &cancel, |f| none.push(f)).unwrap();
        eprintln!("MASKED(*.zzz): {}", none.len());
        assert!(none.is_empty(), "mask filters files");
    }
}
