//! "SteamTube": introspect the local Steam library (installed games) so pixelview can list them,
//! search them, and — clicking a game — find related YouTube videos (routes to the YouTube source).
//!
//! Pure (no egui): reads Steam's VDF/ACF KeyValues config files off disk + a tiny field parser.
//! Detects native, `.steam`, Flatpak and Snap installs. Empty if Steam isn't installed.

use std::path::{Path, PathBuf};

/// Virtual root for the Steam library (mirrors `sixteen::ROOT` / `youtube::ROOT`).
pub const ROOT: &str = "<steam>";

pub fn is_remote(path: &Path) -> bool {
    path.starts_with(ROOT)
}

/// Path components below [`ROOT`].
pub fn rel_parts(path: &Path) -> Vec<String> {
    path.strip_prefix(ROOT)
        .ok()
        .map(|rest| {
            rest.components()
                .filter_map(|c| c.as_os_str().to_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// One installed Steam game.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct SteamGame {
    pub appid: u32,
    pub name: String,
    pub last_played: u64, // unix seconds (0 = never)
    pub size: u64,        // bytes on disk
}

impl SteamGame {
    /// Steam CDN header image (460×215) — the grid-tile thumbnail. Universal across games.
    pub fn header_url(&self) -> String {
        format!(
            "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/header.jpg",
            self.appid
        )
    }
    /// The store page (for a right-click "Open store page").
    pub fn store_url(&self) -> String {
        format!("https://store.steampowered.com/app/{}/", self.appid)
    }
    /// `steam://` deep-link that launches the game through the Steam client (via xdg-open).
    pub fn run_url(&self) -> String {
        format!("steam://rungameid/{}", self.appid)
    }
    /// The community hub web page.
    pub fn hub_url(&self) -> String {
        format!("https://steamcommunity.com/app/{}", self.appid)
    }
    /// The community discussions web page.
    pub fn discussions_url(&self) -> String {
        format!("https://steamcommunity.com/app/{}/discussions/", self.appid)
    }
}

/// A screenshot or trailer from a game's Steam store page (see [`fetch_app_media`]).
#[derive(Clone, Debug, PartialEq)]
pub struct MediaItem {
    pub is_video: bool,    // trailer (streamed) vs screenshot (image)
    pub name: String,      // trailer name; "" for screenshots
    pub thumb_url: String, // tile thumbnail
    pub open_url: String,  // full image (jpg) or trailer stream (HLS .m3u8)
}

/// Fetched game media: display name, short description, and the screenshot/trailer list.
#[derive(Clone, Default, Debug)]
pub struct AppMedia {
    pub name: String,
    pub description: String,
    pub genres: Vec<String>,
    pub media: Vec<MediaItem>, // trailers first, then screenshots
}

/// Fetch a game's store media (screenshots + trailers) via the **public** `store/appdetails` API
/// (no key), through the HTTP cache (1-day TTL). Trailers use the HLS stream URL (ffmpeg reads it).
/// `None` if the request/parse fails or the app has no store page.
pub fn fetch_app_media(appid: u32) -> Option<AppMedia> {
    let url = format!(
        "https://store.steampowered.com/api/appdetails?appids={appid}&filters=basic,screenshots,movies,genres"
    );
    let bytes = crate::cache::get_bytes(&url, Some(86_400)).ok()?;
    parse_app_media(appid, &bytes)
}

/// Parse an `appdetails` JSON blob for `appid` into [`AppMedia`]. Split out for unit testing.
pub fn parse_app_media(appid: u32, bytes: &[u8]) -> Option<AppMedia> {
    let json: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let data = json.get(appid.to_string())?.get("data")?;
    let mut out = AppMedia {
        name: data
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        description: data
            .get("short_description")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        genres: data
            .get("genres")
            .and_then(|g| g.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|g| {
                        g.get("description")
                            .and_then(|v| v.as_str())
                            .map(String::from)
                    })
                    .collect()
            })
            .unwrap_or_default(),
        media: Vec::new(),
    };
    // Trailers first (HLS stream so ffmpeg can play them directly).
    if let Some(movies) = data.get("movies").and_then(|m| m.as_array()) {
        for m in movies {
            let stream = m
                .get("hls_h264")
                .and_then(|v| v.as_str())
                .or_else(|| m.get("dash_h264").and_then(|v| v.as_str()));
            if let Some(stream) = stream {
                out.media.push(MediaItem {
                    is_video: true,
                    name: m
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Trailer")
                        .to_string(),
                    thumb_url: m
                        .get("thumbnail")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    open_url: stream.to_string(),
                });
            }
        }
    }
    // Then screenshots.
    if let Some(shots) = data.get("screenshots").and_then(|s| s.as_array()) {
        for s in shots {
            if let Some(full) = s.get("path_full").and_then(|v| v.as_str()) {
                out.media.push(MediaItem {
                    is_video: false,
                    name: String::new(),
                    thumb_url: s
                        .get("path_thumbnail")
                        .and_then(|v| v.as_str())
                        .unwrap_or(full)
                        .to_string(),
                    open_url: full.to_string(),
                });
            }
        }
    }
    Some(out)
}

/// One game from the Steam Web API's owned-games list (installed OR not).
#[derive(Clone, Default, Debug, PartialEq)]
pub struct OwnedGame {
    pub appid: u32,
    pub name: String,
    pub playtime_min: u64, // total minutes played (0 = never played)
    pub last_played: u64,  // unix seconds (0 = never)
}

/// The signed-in account's SteamID64 — read from `loginusers.vdf` (the first `7656…` key), else
/// computed from the `userdata/<accountid>` dir (`accountid + 76561197960265728`). `None` if absent.
pub fn steam_id64() -> Option<u64> {
    let root = steam_root()?;
    // 1) loginusers.vdf holds the full SteamID64 as a section key.
    if let Ok(text) = std::fs::read_to_string(root.join("config/loginusers.vdf")) {
        for line in text.lines() {
            if let Some(id) = line
                .trim()
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
            {
                if id.len() == 17 && id.starts_with("7656") {
                    if let Ok(n) = id.parse::<u64>() {
                        return Some(n);
                    }
                }
            }
        }
    }
    // 2) Fallback: the userdata dir name is the 32-bit accountid.
    std::fs::read_dir(root.join("userdata"))
        .ok()?
        .flatten()
        .find_map(|e| {
            e.file_name()
                .to_str()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|acc| acc + 76_561_197_960_265_728)
        })
}

/// Fetch the account's **full owned-games list** (installed + not) via the Steam Web API
/// `GetOwnedGames` — needs a free API key (steamcommunity.com/dev/apikey) + the SteamID64. Cached
/// (1-hour TTL). Empty on any failure (no key / private profile / network).
pub fn owned_games(api_key: &str, steamid: u64) -> Vec<OwnedGame> {
    if api_key.trim().is_empty() {
        return Vec::new();
    }
    let url = format!(
        "https://api.steampowered.com/IPlayerService/GetOwnedGames/v1/?key={}&steamid={}&include_appinfo=1&include_played_free_games=1&format=json",
        api_key.trim(),
        steamid
    );
    let Ok(bytes) = crate::cache::get_bytes(&url, Some(3600)) else {
        return Vec::new();
    };
    parse_owned_games(&bytes)
}

/// Parse a `GetOwnedGames` JSON response. Split out for unit testing (no network).
pub fn parse_owned_games(bytes: &[u8]) -> Vec<OwnedGame> {
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return Vec::new();
    };
    let Some(games) = json
        .get("response")
        .and_then(|r| r.get("games"))
        .and_then(|g| g.as_array())
    else {
        return Vec::new();
    };
    let mut out: Vec<OwnedGame> = games
        .iter()
        .filter_map(|g| {
            let appid = g.get("appid").and_then(|v| v.as_u64())? as u32;
            let name = g
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() || is_nongame(&name) {
                return None;
            }
            Some(OwnedGame {
                appid,
                name,
                playtime_min: g
                    .get("playtime_forever")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                last_played: g
                    .get("rtime_last_played")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

/// The set of installed appids (from appmanifests) — to flag owned games as installed-or-not.
pub fn installed_appids() -> std::collections::HashSet<u32> {
    installed_games().into_iter().map(|g| g.appid).collect()
}

/// Candidate Steam data roots: native, classic `.steam`, Flatpak, Snap.
fn steam_roots() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    [
        ".local/share/Steam",
        ".steam/steam",
        ".steam/root",
        ".var/app/com.valvesoftware.Steam/.local/share/Steam",
        "snap/steam/common/.local/share/Steam",
    ]
    .iter()
    .map(|r| home.join(r))
    .collect()
}

/// The first Steam root that actually has a `steamapps` dir, or `None` (Steam not installed).
pub fn steam_root() -> Option<PathBuf> {
    steam_roots()
        .into_iter()
        .find(|p| p.join("steamapps").is_dir())
}

/// Every `steamapps` dir across libraries (follows `libraryfolders.vdf` to other drives).
fn steamapps_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![root.join("steamapps")];
    let lf = root.join("steamapps/libraryfolders.vdf");
    if let Ok(text) = std::fs::read_to_string(&lf) {
        for path in vdf_values(&text, "path") {
            let d = Path::new(&path).join("steamapps");
            if d.is_dir() && !dirs.contains(&d) {
                dirs.push(d);
            }
        }
    }
    dirs
}

/// Names that are tools/runtimes, not games — skipped from the listing.
fn is_nongame(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("proton")
        || n.starts_with("steam linux runtime")
        || n.starts_with("steamworks common")
        || n == "steamvr"
}

/// All installed games across every library, de-duped, sorted by name. Empty if Steam is absent.
pub fn installed_games() -> Vec<SteamGame> {
    let mut games: Vec<SteamGame> = Vec::new();
    let Some(root) = steam_root() else {
        return games;
    };
    for sa in steamapps_dirs(&root) {
        let Ok(rd) = std::fs::read_dir(&sa) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let manifest = p
                .file_name()
                .and_then(|f| f.to_str())
                .is_some_and(|n| n.starts_with("appmanifest_") && n.ends_with(".acf"));
            if !manifest {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&p) {
                if let Some(g) = parse_manifest(&text) {
                    if !is_nongame(&g.name) {
                        games.push(g);
                    }
                }
            }
        }
    }
    games.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    games.dedup_by_key(|g| g.appid);
    games
}

/// Parse one `appmanifest_*.acf` into a [`SteamGame`]. `None` without a valid appid + name.
pub fn parse_manifest(text: &str) -> Option<SteamGame> {
    let appid: u32 = vdf_value(text, "appid")?.parse().ok()?;
    let name = vdf_value(text, "name").unwrap_or_default();
    if name.is_empty() {
        return None;
    }
    Some(SteamGame {
        appid,
        name,
        last_played: vdf_value(text, "LastPlayed")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        size: vdf_value(text, "SizeOnDisk")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    })
}

/// The value of the first `"<key>"  "<value>"` line in a VDF/ACF blob (the line must *start* with
/// the quoted key, so `"name"` won't match a nested `"gamename"`).
fn vdf_value(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    text.lines().find_map(|line| {
        line.trim()
            .strip_prefix(&needle)
            .and_then(|rest| quoted(rest.trim_start()))
    })
}

/// Every value for `"<key>"` (e.g. all `"path"` entries in `libraryfolders.vdf`).
fn vdf_values(text: &str, key: &str) -> Vec<String> {
    let needle = format!("\"{key}\"");
    text.lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix(&needle)
                .and_then(|rest| quoted(rest.trim_start()))
        })
        .collect()
}

/// The contents of the first `"…"` quoted token at the start of `s`.
fn quoted(s: &str) -> Option<String> {
    let s = s.strip_prefix('"')?;
    let end = s.find('"')?;
    Some(s[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACF: &str = r#"
"AppState"
{
	"appid"		"339400"
	"name"		"Runestone Keeper"
	"StateFlags"		"4"
	"installdir"		"Runestone Keeper"
	"LastPlayed"		"1700000000"
	"SizeOnDisk"		"227208092"
}
"#;

    #[test]
    fn parses_a_manifest() {
        let g = parse_manifest(ACF).expect("parses");
        assert_eq!(g.appid, 339400);
        assert_eq!(g.name, "Runestone Keeper");
        assert_eq!(g.last_played, 1700000000);
        assert_eq!(g.size, 227208092);
        assert_eq!(
            g.header_url(),
            "https://cdn.cloudflare.steamstatic.com/steam/apps/339400/header.jpg"
        );
    }

    #[test]
    fn nongames_are_flagged() {
        assert!(is_nongame("Proton 9.0"));
        assert!(is_nongame("Steam Linux Runtime 3.0 (sniper)"));
        assert!(is_nongame("Steamworks Common Redistributables"));
        assert!(!is_nongame("Elden Ring"));
    }

    #[test]
    fn libraryfolders_paths_extracted() {
        let vdf = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"/home/u/.local/share/Steam"
	}
	"1"
	{
		"path"		"/mnt/games/SteamLibrary"
	}
}
"#;
        let paths = vdf_values(vdf, "path");
        assert_eq!(
            paths,
            vec!["/home/u/.local/share/Steam", "/mnt/games/SteamLibrary"]
        );
    }

    /// Reads the machine's REAL Steam library (native/flatpak/…). `#[ignore]` — machine-specific,
    /// like the network tests. Run with `cargo test steam -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn lists_real_library() {
        match steam_root() {
            Some(root) => {
                let games = installed_games();
                eprintln!("steam root: {}\n{} games:", root.display(), games.len());
                for g in games.iter().take(8) {
                    eprintln!("  [{}] {}  → {}", g.appid, g.name, g.header_url());
                }
                assert!(!games.is_empty(), "found a Steam root but no games");
            }
            None => eprintln!("no Steam install on this machine — skipping"),
        }
    }

    #[test]
    fn parses_owned_games() {
        let json = r#"{"response":{"game_count":3,"games":[
            {"appid":1245620,"name":"ELDEN RING","playtime_forever":8520,"rtime_last_played":1700000000},
            {"appid":367520,"name":"Hollow Knight","playtime_forever":0,"rtime_last_played":0},
            {"appid":228980,"name":"Steamworks Common Redistributables","playtime_forever":0}
        ]}}"#;
        let g = parse_owned_games(json.as_bytes());
        assert_eq!(g.len(), 2); // redistributables filtered out
                                // sorted by name: ELDEN RING, Hollow Knight
        assert_eq!(g[0].name, "ELDEN RING");
        assert_eq!(g[0].playtime_min, 8520);
        assert_eq!(g[1].name, "Hollow Knight");
        assert_eq!(g[1].playtime_min, 0); // never played
    }

    #[test]
    fn parses_app_media() {
        let json = r#"{"620":{"success":true,"data":{
            "name":"Portal 2","short_description":"co-op puzzler",
            "genres":[{"description":"Action"},{"description":"Puzzle"}],
            "movies":[{"id":1,"name":"Trailer","thumbnail":"http://t/mv.jpg",
                       "hls_h264":"http://v/master.m3u8","dash_h264":"http://v/x.mpd"}],
            "screenshots":[{"id":0,"path_thumbnail":"http://s/1t.jpg","path_full":"http://s/1.jpg"}]
        }}}"#;
        let m = parse_app_media(620, json.as_bytes()).expect("parses");
        assert_eq!(m.name, "Portal 2");
        assert_eq!(m.genres, vec!["Action", "Puzzle"]);
        assert_eq!(m.media.len(), 2);
        // Trailer first (HLS), streamed.
        assert!(m.media[0].is_video);
        assert_eq!(m.media[0].open_url, "http://v/master.m3u8");
        // Then the screenshot (full jpg).
        assert!(!m.media[1].is_video);
        assert_eq!(m.media[1].open_url, "http://s/1.jpg");
    }

    #[test]
    fn missing_fields_reject() {
        assert!(parse_manifest("\"appid\" \"1\"").is_none()); // no name
        assert!(parse_manifest("\"name\" \"x\"").is_none()); // no appid
    }
}
