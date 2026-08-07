//! Browse <https://16colo.rs> — the ANSI/ASCII art archive — as a virtual disk:
//! Years → Packs → the art files. A pack is just a ZIP, so once downloaded it hands
//! off to the normal archive-browsing path. Only the year/pack *listing* and the
//! pack *download* are new; both use the public JSON API + the static archive.

use std::io::Read;
use std::path::{Path, PathBuf};

/// Virtual root for the 16colo.rs hierarchy. Borrows the `<…>` sentinel style used
/// for built-in palettes — it can never collide with a real on-disk path.
pub const ROOT: &str = "<16colo.rs>";

const API: &str = "https://api.16colo.rs/v1";
const SITE: &str = "https://16colo.rs";

/// Is `path` somewhere inside the virtual 16colo.rs tree?
pub fn is_remote(path: &Path) -> bool {
    path.starts_with(ROOT)
}

/// The path components below [`ROOT`] (e.g. `["2023", "blocktronics-pack"]`).
pub fn rel_parts(path: &Path) -> Vec<String> {
    path.strip_prefix(ROOT)
        .ok()
        .map(|r| {
            r.components()
                .filter_map(|c| c.as_os_str().to_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The years the archive spans (newest first). 16colo.rs runs 1990 → present.
pub fn years() -> Vec<u32> {
    (1990..=2026).rev().collect()
}

/// A pack listing entry: its name and the full URL of its `.zip`.
pub struct Pack {
    pub name: String,
    pub url: String,
}

/// The newest year the archive holds (also the max in [`years`]). Past years are
/// immutable, so their listings can be cached forever; this one refreshes hourly.
const CURRENT_YEAR: u32 = 2026;

fn cache_dir() -> PathBuf {
    // The persistent cache dir (set at startup); falls back to temp before init / in tests.
    crate::cache::dir().unwrap_or_else(|| std::env::temp_dir().join("kaleidotron-16colo"))
}

/// A disk-cached pack listing for `year`, if present and still fresh.
fn read_packs_cache(year: u32) -> Option<Vec<Pack>> {
    let path = cache_dir().join(format!("packs-{year}.json"));
    let meta = std::fs::metadata(&path).ok()?;
    if year >= CURRENT_YEAR {
        // Recent year: refresh after an hour (new packs may have been uploaded).
        if meta.modified().ok()?.elapsed().ok()? > std::time::Duration::from_secs(3600) {
            return None;
        }
    }
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
    Some(
        v.as_array()?
            .iter()
            .filter_map(|p| {
                Some(Pack {
                    name: p["n"].as_str()?.to_owned(),
                    url: p["u"].as_str()?.to_owned(),
                })
            })
            .collect(),
    )
}

fn write_packs_cache(year: u32, packs: &[Pack]) {
    if packs.is_empty() || std::fs::create_dir_all(cache_dir()).is_err() {
        return;
    }
    let arr: Vec<serde_json::Value> = packs
        .iter()
        .map(|p| serde_json::json!({ "n": p.name, "u": p.url }))
        .collect();
    let _ = std::fs::write(
        cache_dir().join(format!("packs-{year}.json")),
        serde_json::Value::Array(arr).to_string(),
    );
}

/// Fetch the packs released in `year` from the JSON API (cached on disk per year;
/// see [`read_packs_cache`]). The endpoint caps a page at 50, so we follow
/// `page.pages` to collect them all.
pub fn fetch_packs(year: u32) -> Result<Vec<Pack>, String> {
    if let Some(cached) = read_packs_cache(year) {
        return Ok(cached);
    }
    let mut packs = Vec::new();
    let mut page = 1u32;
    loop {
        let url = format!("{API}/year/{year}?pagesize=100&page={page}");
        let body = ureq::get(&url)
            .call()
            .map_err(|e| e.to_string())?
            .into_string()
            .map_err(|e| e.to_string())?;
        let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        let Some(results) = v["results"].as_array() else {
            break;
        };
        if results.is_empty() {
            break;
        }
        for p in results {
            // The pack name is usually a string, but a JSON *number* when the pack is
            // literally named after one (e.g. "1990").
            let name = p["name"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| p["name"].as_i64().map(|n| n.to_string()));
            let (Some(name), Some(dl)) = (name, p["download"].as_str()) else {
                continue;
            };
            // `download` is absolute on the year endpoint, site-relative elsewhere.
            let url = if dl.starts_with("http") {
                dl.to_string()
            } else {
                format!("{SITE}{dl}")
            };
            packs.push(Pack { name, url });
        }
        let pages = v["page"]["pages"].as_u64().unwrap_or(1) as u32;
        page += 1;
        if page > pages || page > 60 {
            break;
        }
    }
    packs.sort_by_key(|a| a.name.to_lowercase());
    write_packs_cache(year, &packs);
    Ok(packs)
}

/// The conventional zip URL for a pack, used as a fallback when its listing entry
/// isn't cached (e.g. a path typed/navigated directly).
pub fn pack_url(year: u32, pack: &str) -> String {
    format!("{SITE}/archive/{year}/{pack}.zip")
}

/// Download a pack `url` to the persistent cache and return its path. Cached (by the
/// shared HTTP cache) so an already-downloaded pack zip is reused across sessions.
pub fn download(url: &str) -> Result<PathBuf, String> {
    let name = url
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("pack.zip");
    crate::cache::get_file(url, name)
}

/// Download `url` straight to `dest` (the "Download file / pack" action — the user
/// already picked the destination). Streams to a temp sibling then renames, so a
/// partial download never leaves a truncated file at `dest`.
pub fn download_to(url: &str, dest: &Path) -> Result<(), String> {
    let resp = ureq::get(url).call().map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    resp.into_reader()
        .take(256 * 1024 * 1024)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    let tmp = dest.with_extension("part");
    std::fs::write(&tmp, &buf).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dest).map_err(|e| e.to_string())?;
    Ok(())
}

/// Download a single piece's `raw` file into a per-URL cache subdir, preserving its
/// real `filename` (so the decoder's extension dispatch still works), and return the
/// local path. Cached: an already-downloaded piece is reused.
pub fn download_file(url: &str, filename: &str) -> Result<PathBuf, String> {
    crate::cache::get_file(url, filename)
}

/// The virtual sub-roots the nav bar exposes besides the year list.
pub const GROUPS: &str = "groups";
pub const ARTISTS: &str = "artists";
pub const LATEST: &str = "latest";
pub const SEARCH: &str = "search";

/// Minimal percent-encoding for an API path segment (group/artist names can have
/// spaces and punctuation).
fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Cache lifetime for a JSON endpoint: a *pack*'s contents are immutable once published
/// (never expire); the "latest" feed turns over fast (1 h); artist/group/search lists
/// grow slowly (1 day). Force a refresh any time by clearing the cache (Preferences).
fn json_ttl(url: &str) -> Option<i64> {
    if url.contains("/pack/") {
        None
    } else if url.contains("/latest") {
        Some(3600)
    } else {
        Some(86_400)
    }
}

fn get_json(url: &str) -> Result<serde_json::Value, String> {
    let body = crate::cache::get_bytes(url, json_ttl(url))?;
    serde_json::from_slice(&body).map_err(|e| e.to_string())
}

/// Walk a paginated endpoint (`base` already carries its query, sans `page`), calling
/// `f` on every item across all pages (`page.pages`), capped so a runaway can't spin.
fn paginate<F: FnMut(&serde_json::Value)>(base: &str, mut f: F) -> Result<(), String> {
    let sep = if base.contains('?') { '&' } else { '?' };
    let mut page = 1u32;
    loop {
        let v = get_json(&format!("{base}{sep}page={page}"))?;
        match v["results"].as_array() {
            Some(r) if !r.is_empty() => r.iter().for_each(&mut f),
            _ => break,
        }
        let pages = v["page"]["pages"].as_u64().unwrap_or(1) as u32;
        page += 1;
        if page > pages || page > 80 {
            break;
        }
    }
    Ok(())
}

/// Every group name on 16colo.rs (sorted, case-insensitive). The `/v1/group` list is
/// an array of single-key objects `{ "<group>": { releases, … } }`.
pub fn fetch_groups() -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    paginate(&format!("{API}/group?pagesize=100"), |item| {
        if let Some(obj) = item.as_object() {
            names.extend(obj.keys().cloned());
        }
    })?;
    names.sort_by_key(|s| s.to_lowercase());
    names.dedup();
    Ok(names)
}

/// Every artist name on 16colo.rs (sorted). `/v1/artist` items are `{ artist: { name } }`.
pub fn fetch_artists() -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    paginate(&format!("{API}/artist?pagesize=100"), |item| {
        if let Some(n) = item["artist"]["name"].as_str() {
            names.push(n.to_string());
        }
    })?;
    names.sort_by_key(|s| s.to_lowercase());
    names.dedup();
    Ok(names)
}

/// The most recent releases (packs), newest first. `/v1/latest/releases` items carry
/// `pack` + an absolute `download` URL.
pub fn fetch_latest() -> Result<Vec<Pack>, String> {
    let v = get_json(&format!("{API}/latest/releases?pagesize=50"))?;
    let mut packs = Vec::new();
    if let Some(results) = v["results"].as_array() {
        for p in results {
            if let (Some(name), Some(dl)) = (p["pack"].as_str(), p["download"].as_str()) {
                let url = if dl.starts_with("http") {
                    dl.to_string()
                } else {
                    format!("{SITE}{dl}")
                };
                packs.push(Pack {
                    name: name.to_string(),
                    url,
                });
            }
        }
    }
    Ok(packs)
}

/// Server-side **substring** search for artist names matching `query` — the API's
/// `?filter=` does the matching (9k+ artists, so we never fetch the full list). One
/// page (100) of hits is plenty for a picker.
pub fn search_artists(query: &str) -> Result<Vec<String>, String> {
    let v = get_json(&format!("{API}/artist?pagesize=100&filter={}", enc(query)))?;
    let mut names = Vec::new();
    if let Some(results) = v["results"].as_array() {
        for x in results {
            if let Some(n) = x["artist"]["name"].as_str() {
                names.push(n.to_string());
            }
        }
    }
    Ok(names)
}

/// Server-side substring search for group names matching `query` (each result is a
/// single-key `{ "<group>": {…} }` object, like the group list).
pub fn search_groups(query: &str) -> Result<Vec<String>, String> {
    let v = get_json(&format!("{API}/group?pagesize=100&filter={}", enc(query)))?;
    let mut names = Vec::new();
    if let Some(results) = v["results"].as_array() {
        for x in results {
            if let Some(obj) = x.as_object() {
                names.extend(obj.keys().cloned());
            }
        }
    }
    Ok(names)
}

/// One individual art *piece* on 16colo.rs (not a pack). This is what the flat table
/// view shows: the file plus the metadata needed for its columns + actions. Built
/// from the artist/pack JSON endpoints (no pack download). `raw_url` is the single
/// file (for "open" / "download file"); `tn_url` is its pre-rendered thumbnail PNG.
#[derive(Clone, Debug, PartialEq)]
pub struct Piece {
    pub filename: String,
    pub artist: String,
    pub group: String,
    pub year: u32,
    pub pack: String,
    pub raw_url: String,
    /// The pre-rendered preview PNG we fetch for the grid/table thumbnail. Points at
    /// 16colo's larger `/x1/` render (≈768px), not the tiny `/tn/`, so big grid tiles
    /// stay crisp (see [`x1_url`]).
    pub tn_url: String,
    /// SAUCE record from the API (`?sauce=true`, pack endpoint only) — `None` when the
    /// file has no SAUCE or the endpoint doesn't carry it (e.g. the artist view).
    pub sauce: Option<crate::sauce::Sauce>,
    /// File size in bytes from the SAUCE record (`0` when unknown).
    pub filesize: u64,
}

/// Map a 16colo.rs API `sauce` object → our `crate::sauce::Sauce`. The API uses
/// PascalCase keys (`Title`/`Author`/`Tinfo1`…), `Date` is a number *or* a string,
/// and iCE colour lives in `f.ice` (or `Tflags` bit 0). An all-blank record → `None`.
fn sauce_from_json(s: &serde_json::Value) -> Option<crate::sauce::Sauce> {
    if !s.is_object() {
        return None;
    }
    let txt = |k: &str| s[k].as_str().unwrap_or("").trim().to_string();
    let date = match &s["Date"] {
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(st) => st.trim().to_string(),
        _ => String::new(),
    };
    let (title, author, group) = (txt("Title"), txt("Author"), txt("Group"));
    if title.is_empty() && author.is_empty() && group.is_empty() && date.is_empty() {
        return None;
    }
    Some(crate::sauce::Sauce {
        title,
        author,
        group,
        date,
        data_type: s["Datatype"].as_u64().unwrap_or(0) as u8,
        file_type: s["Filetype"].as_u64().unwrap_or(0) as u8,
        tinfo1: s["Tinfo1"].as_u64().unwrap_or(0) as u16,
        tinfo2: s["Tinfo2"].as_u64().unwrap_or(0) as u16,
        ice: s["f"]["ice"].as_u64().unwrap_or(0) != 0
            || (s["Tflags"].as_u64().unwrap_or(0) & 1) != 0,
        font: txt("Tinfos"),
        comment: txt("Comments"),
        ..Default::default()
    })
}

/// Percent-encode a site path, **preserving `/`** so the structure survives. The API
/// returns *literal* paths (e.g. `/pack/fire/raw/#44_FIRE.ANS`), so a filename with `#`
/// (or a space, `?`, `&`, …) would otherwise truncate the URL at the fragment/query and
/// the fetch fails — the bug that left `#`-named pieces un-downloadable + their thumbnails
/// spinning forever.
fn enc_path(p: &str) -> String {
    p.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Make a site-relative API path (`/pack/…`) absolute (percent-encoding it via
/// [`enc_path`]); pass through an already-absolute URL unchanged; empty stays empty.
fn abs_url(p: &str) -> String {
    if p.is_empty() || p.starts_with("http") {
        p.to_string()
    } else {
        format!("{SITE}{}", enc_path(p))
    }
}

/// 16colo renders each piece at two parallel sizes: `/tn/` (a 160px thumbnail) and
/// `/x1/` (≈768px). We fetch the larger `x1` for the grid/table so a big tile is a
/// crisp box-downscale instead of a blurry upscale of the tiny `tn`. The artist
/// endpoint only lists `tn`, but the paths are parallel, so derive `x1` by swapping
/// the one path segment. Empty stays empty (`abs_url` then yields "").
fn x1_url(tn_uri: &str) -> String {
    tn_uri.replacen("/tn/", "/x1/", 1)
}

/// Flatten an artist's `/v1/artist/:name` body into pieces. Shape:
/// `results → "<year>" → "<pack>" → { group, files: [{ file, raw, tn }] }`.
/// Pure (no network) so it's unit-testable; [`fetch_artist_pieces`] just feeds it.
fn pieces_from_artist_json(artist: &str, v: &serde_json::Value) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let Some(by_year) = v["results"].as_object() else {
        return pieces;
    };
    for (y, packs) in by_year {
        let year: u32 = y.parse().unwrap_or(0);
        let Some(packs) = packs.as_object() else {
            continue;
        };
        for (pack, info) in packs {
            let group = info["group"].as_str().unwrap_or("").to_string();
            let Some(files) = info["files"].as_array() else {
                continue;
            };
            for f in files {
                let Some(filename) = f["file"].as_str() else {
                    continue;
                };
                pieces.push(Piece {
                    filename: filename.to_string(),
                    artist: artist.to_string(),
                    group: group.clone(),
                    year,
                    pack: pack.clone(),
                    raw_url: abs_url(f["raw"].as_str().unwrap_or("")),
                    tn_url: abs_url(&x1_url(f["tn"].as_str().unwrap_or(""))),
                    // The artist view carries no SAUCE; extract it anyway in case a
                    // future response adds it (harmless `None` today).
                    sauce: sauce_from_json(&f["sauce"]),
                    filesize: f["sauce"]["Filesize"].as_u64().unwrap_or(0),
                });
            }
        }
    }
    pieces
}

/// Flatten a pack's `/v1/pack/:pack` body into pieces. Shape:
/// `results[] → { year, files: { "<FILE>": { file: { raw, tn: { uri } }, artists: [] } } }`.
/// `group` isn't reliably per-file here, so the caller stamps it (it's listing one group).
fn pieces_from_pack_json(
    pack: &str,
    group: &str,
    year_hint: u32,
    v: &serde_json::Value,
) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let Some(results) = v["results"].as_array() else {
        return pieces;
    };
    for r in results {
        let year = r["year"].as_u64().map(|y| y as u32).unwrap_or(year_hint);
        let Some(files) = r["files"].as_object() else {
            continue;
        };
        for (filename, fobj) in files {
            let tn = fobj["file"]["tn"]["uri"].as_str().unwrap_or("");
            let artist = fobj["artists"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            pieces.push(Piece {
                filename: filename.clone(),
                artist,
                group: group.to_string(),
                year,
                pack: pack.to_string(),
                raw_url: format!("{SITE}/pack/{}/raw/{}", enc(pack), enc(filename)),
                tn_url: abs_url(&x1_url(tn)),
                sauce: sauce_from_json(&fobj["sauce"]),
                filesize: fobj["sauce"]["Filesize"].as_u64().unwrap_or(0),
            });
        }
    }
    pieces
}

/// Every piece by `artist` (one API call — the artist endpoint carries files inline).
/// NB: `/v1/artist/{name}` returns an empty body for names containing a space (a server
/// quirk), so a *multi-word* artist yields nothing here — the caller falls back to
/// [`fetch_artist_packs`] + per-pack fetch (see `emit_artist` in app.rs).
pub fn fetch_artist_pieces(artist: &str) -> Result<Vec<Piece>, String> {
    let v = get_json(&format!("{API}/artist/{}?pagesize=100", enc(artist)))?;
    Ok(pieces_from_artist_json(artist, &v))
}

/// The pack names an artist appears in, from the search endpoint's `details=true` view
/// (`results[].artist.packs`). The artist *detail* endpoint 404s/empties on a space in
/// the path, but search matches the display name fine — so this is how we reach a
/// multi-word artist's work (fetch each pack, then filter to this artist).
pub fn fetch_artist_packs(artist: &str) -> Result<Vec<String>, String> {
    let v = get_json(&format!(
        "{API}/artist?pagesize=1&details=true&filter={}",
        enc(artist)
    ))?;
    let want = artist.to_lowercase();
    let packs = v["results"]
        .as_array()
        .and_then(|rs| {
            rs.iter()
                .map(|r| &r["artist"])
                .find(|a| a["name"].as_str().map(str::to_lowercase) == Some(want.clone()))
        })
        .and_then(|a| a["packs"].as_array())
        .map(|ps| {
            ps.iter()
                .filter_map(|p| p.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Ok(packs)
}

/// A group's packs as `(year, pack)` refs (newest year first), so the caller can fetch
/// each pack's pieces. `/v1/group/:name` → `{ results: { packs: { "<year>": [pack…] } } }`.
pub fn fetch_group_pack_refs(group: &str) -> Result<Vec<(u32, String)>, String> {
    let v = get_json(&format!("{API}/group/{}?pagesize=100", enc(group)))?;
    let mut refs = Vec::new();
    if let Some(by_year) = v["results"]["packs"].as_object() {
        let mut years: Vec<&String> = by_year.keys().collect();
        years.sort_by(|a, b| b.cmp(a)); // newest first
        for y in years {
            let year: u32 = y.parse().unwrap_or(0);
            if let Some(list) = by_year[y].as_array() {
                for p in list.iter().filter_map(|p| p.as_str()) {
                    refs.push((year, p.to_string()));
                }
            }
        }
    }
    Ok(refs)
}

/// Every piece in `pack` (via `/v1/pack/:pack`), stamped with `group` (the listing
/// context). `year_hint` is used only if a result omits its year.
pub fn fetch_pack_pieces(group: &str, year_hint: u32, pack: &str) -> Result<Vec<Piece>, String> {
    // `?sauce=true` makes the pack endpoint include each file's SAUCE record (Title,
    // Author, dimensions, Filesize) — populated into the Details panel without download.
    let v = get_json(&format!("{API}/pack/{}?sauce=true", enc(pack)))?;
    Ok(pieces_from_pack_json(pack, group, year_hint, &v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "hits the live 16colo.rs API + network"]
    fn fetch_artist_pieces_live() {
        let pieces = fetch_artist_pieces("jed").expect("fetch jed");
        assert!(!pieces.is_empty(), "jed should have pieces");
        let p = &pieces[0];
        assert!(
            p.tn_url.starts_with("https://16colo.rs/"),
            "tn: {}",
            p.tn_url
        );
        assert!(
            p.raw_url.starts_with("https://16colo.rs/"),
            "raw: {}",
            p.raw_url
        );
        assert!(!p.pack.is_empty() && p.year >= 1990);
        // The thumbnail URL serves a decodable image (what RemoteThumbs relies on).
        let resp = ureq::get(&p.tn_url).call().expect("tn GET");
        let mut buf = Vec::new();
        resp.into_reader().read_to_end(&mut buf).expect("tn body");
        image::load_from_memory(&buf).expect("tn is a valid image");
    }

    #[test]
    fn artist_json_flattens_to_pieces() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{ "results": { "1992": { "acdu0892": {
                "group": "acid",
                "files": [
                    { "file": "MIDNACD3.ANS", "raw": "/pack/acdu0892/raw/MIDNACD3.ANS",
                      "tn": "/pack/acdu0892/tn/MIDNACD3.ANS.png" }
                ]
            } } } }"#,
        )
        .unwrap();
        let pieces = pieces_from_artist_json("jed", &v);
        assert_eq!(pieces.len(), 1);
        let p = &pieces[0];
        assert_eq!(p.filename, "MIDNACD3.ANS");
        assert_eq!(p.artist, "jed");
        assert_eq!(p.group, "acid");
        assert_eq!(p.year, 1992);
        assert_eq!(p.pack, "acdu0892");
        assert_eq!(
            p.raw_url,
            "https://16colo.rs/pack/acdu0892/raw/MIDNACD3.ANS"
        );
        // We fetch the larger `x1` render (derived from the `tn` path) for crisp tiles.
        assert_eq!(
            p.tn_url,
            "https://16colo.rs/pack/acdu0892/x1/MIDNACD3.ANS.png"
        );
    }

    #[test]
    fn pack_json_flattens_and_stamps_group() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{ "results": [ { "year": 1992, "files": {
                "ACID-BR.ANS": {
                    "file": { "raw": "ACID-BR.ANS",
                              "tn": { "uri": "/pack/acdu0892/tn/ACID-BR.ANS.png" } },
                    "artists": ["blade runner"]
                }
            } } ] }"#,
        )
        .unwrap();
        let pieces = pieces_from_pack_json("acdu0892", "acid", 0, &v);
        assert_eq!(pieces.len(), 1);
        let p = &pieces[0];
        assert_eq!(p.filename, "ACID-BR.ANS");
        assert_eq!(p.artist, "blade runner");
        assert_eq!(p.group, "acid"); // stamped from listing context
        assert_eq!(p.year, 1992);
        assert_eq!(p.raw_url, "https://16colo.rs/pack/acdu0892/raw/ACID-BR.ANS");
        assert_eq!(
            p.tn_url,
            "https://16colo.rs/pack/acdu0892/x1/ACID-BR.ANS.png"
        );
    }

    #[test]
    fn hash_in_filename_is_percent_encoded_in_urls() {
        // A literal `#` in a filename would truncate the URL at the fragment, leaving the
        // piece un-downloadable and its thumbnail spinning forever. Both the pack-view
        // (built filename) and artist-view (API path via abs_url) must encode it as %23.
        let pv: serde_json::Value = serde_json::from_str(
            r##"{ "results": [ { "year": 2025, "files": {
                "#44_FIRE.ANS": {
                    "file": { "raw": "#44_FIRE.ANS",
                              "tn": { "uri": "/pack/fire/tn/#44_FIRE.ANS.png" } },
                    "artists": ["tainted"]
                }
            } } ] }"##,
        )
        .unwrap();
        let p = &pieces_from_pack_json("fire", "fire", 0, &pv)[0];
        assert_eq!(p.filename, "#44_FIRE.ANS"); // identity keeps the real name
        assert_eq!(p.raw_url, "https://16colo.rs/pack/fire/raw/%2344_FIRE.ANS");
        assert_eq!(
            p.tn_url,
            "https://16colo.rs/pack/fire/x1/%2344_FIRE.ANS.png"
        );

        let av: serde_json::Value = serde_json::from_str(
            r##"{ "results": { "2025": { "fire": {
                "group": "fire",
                "files": [ { "file": "#44_FIRE.ANS", "raw": "/pack/fire/raw/#44_FIRE.ANS",
                             "tn": "/pack/fire/tn/#44_FIRE.ANS.png" } ]
            } } } }"##,
        )
        .unwrap();
        let p = &pieces_from_artist_json("tainted", &av)[0];
        assert_eq!(p.raw_url, "https://16colo.rs/pack/fire/raw/%2344_FIRE.ANS");
        assert_eq!(
            p.tn_url,
            "https://16colo.rs/pack/fire/x1/%2344_FIRE.ANS.png"
        );
    }

    #[test]
    fn pack_json_extracts_sauce_and_filesize() {
        // `?sauce=true` adds a per-file SAUCE record (PascalCase keys; Date may be a
        // number; iCE in `f.ice`). We map it into our `Sauce` + pull Filesize.
        let v: serde_json::Value = serde_json::from_str(
            r#"{ "results": [ { "year": 1997, "files": {
                "CG-MALP.ANS": {
                    "file": { "raw": "CG-MALP.ANS", "tn": { "uri": "/pack/tw/tn/CG-MALP.ANS.png" } },
                    "artists": ["coug"],
                    "sauce": { "Title": "Malpractice", "Author": "Coug", "Group": "Twilight",
                               "Date": 19970309, "Filesize": 4315, "Datatype": 1, "Filetype": 1,
                               "Tinfo1": 80, "Tinfo2": 25, "Tinfos": "", "Tflags": 1,
                               "f": { "ice": 1 } } }
            } } ] }"#,
        )
        .unwrap();
        let p = &pieces_from_pack_json("tw-pack", "twilight", 0, &v)[0];
        let s = p.sauce.as_ref().expect("sauce extracted");
        assert_eq!(s.title, "Malpractice");
        assert_eq!(s.author, "Coug");
        assert_eq!(s.group, "Twilight");
        assert_eq!(s.date, "19970309");
        assert_eq!((s.tinfo1, s.tinfo2), (80, 25));
        assert_eq!(s.data_type, 1);
        assert!(s.ice);
        assert_eq!(p.filesize, 4315);

        // No sauce key → None, size 0 (hidden in the table).
        let v2: serde_json::Value = serde_json::from_str(
            r#"{ "results": [ { "year": 1992, "files": {
                "X.ANS": { "file": { "raw": "X.ANS", "tn": { "uri": "/t.png" } }, "artists": [] }
            } } ] }"#,
        )
        .unwrap();
        let p2 = &pieces_from_pack_json("p", "g", 0, &v2)[0];
        assert!(p2.sauce.is_none());
        assert_eq!(p2.filesize, 0);
    }

    #[test]
    fn rel_parts_splits_the_virtual_path() {
        assert!(is_remote(Path::new(ROOT)));
        assert_eq!(rel_parts(Path::new(ROOT)), Vec::<String>::new());
        let p = Path::new(ROOT).join("2023").join("some-pack");
        assert_eq!(rel_parts(&p), vec!["2023", "some-pack"]);
        assert!(!is_remote(Path::new("/home/x")));
    }

    #[test]
    #[ignore = "hits the live 16colo.rs API + network"]
    fn fetch_and_cache_packs_live() {
        let _ = std::fs::remove_file(cache_dir().join("packs-1990.json"));
        let packs = fetch_packs(1990).expect("fetch 1990");
        assert!(
            packs.iter().any(|p| p.name == "acdu1190"),
            "1990 should contain acdu1190"
        );
        assert!(
            cache_dir().join("packs-1990.json").exists(),
            "disk cache written"
        );
        // Second call is served from the cache (past year → no network) and matches.
        let again = fetch_packs(1990).expect("cached 1990");
        assert_eq!(packs.len(), again.len());
    }

    #[test]
    fn years_are_descending() {
        let y = years();
        assert_eq!(y.first().copied(), Some(2026));
        assert!(y.windows(2).all(|w| w[0] > w[1]));
    }
}
