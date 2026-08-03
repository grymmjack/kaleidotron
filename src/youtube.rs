//! YouTube browsing + playback via **yt-dlp** (the "API") feeding the same ffmpeg player pipe
//! as local video. Mirrors the 16colo.rs online browser: a search returns a table of results;
//! opening one resolves a direct **stream URL** (`yt-dlp -g`) which — because ffmpeg reads URLs
//! like files — is handed to the existing `VideoPlayer` unchanged (probe / frame pipe / audio /
//! seek / PNG export all work over HTTP).
//!
//! Pure (no egui): just `Command` + `serde_json`, so JSON parsing is unit-testable with no
//! network. Everything shells out to `yt-dlp`; absent ⇒ empty results / a graceful status, the
//! same ethos as ffmpeg/poppler/blender elsewhere in the project.
//!
//! NOTE: the browser UI wiring lands in a follow-up (see the playback-strategy decision in the
//! session notes), so the public fns are `allow(dead_code)` until then.
#![allow(dead_code)]

use std::path::Path;
use std::process::{Command, Stdio};

/// Virtual root for YouTube browsing (mirrors `sixteen::ROOT`). A path under it is "remote"
/// (never touched on disk until a video is downloaded in place).
pub const ROOT: &str = "<youtube>";
/// Sub-root: search results live at `<youtube>/search/<query>`.
pub const SEARCH: &str = "search";
/// Sub-root: a channel's videos at `<youtube>/channel/<id>`, its playlists at
/// `<youtube>/channel/<id>/playlists`.
pub const CHANNEL: &str = "channel";
/// Sub-leaf under a channel path: the channel's playlists listing.
pub const PLAYLISTS: &str = "playlists";
/// Sub-root: a playlist's videos at `<youtube>/playlist/<id>`.
pub const PLAYLIST: &str = "playlist";

/// The UC… channel id from a channel URL (`…/channel/UC…`) or a bare id. `""` if not found.
pub fn channel_id_from_url(url: &str) -> String {
    if let Some(i) = url.find("/channel/") {
        url[i + 9..]
            .split(['/', '?'])
            .next()
            .unwrap_or("")
            .to_string()
    } else if url.starts_with("UC") {
        url.to_string()
    } else {
        String::new()
    }
}

/// Is `path` a YouTube virtual path?
pub fn is_remote(path: &Path) -> bool {
    path.starts_with(ROOT)
}

/// The path components below [`ROOT`] (e.g. `["search", "lofi"]`).
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

/// One search result (or a video being opened).
#[derive(Clone, Default, Debug, PartialEq)]
pub struct YtVideo {
    pub id: String,
    pub title: String,
    pub channel: String,
    pub channel_id: String, // the UC… id (for "Go to channel"); "" when yt-dlp omits it
    pub duration: f32,      // seconds; 0 = unknown (e.g. a live stream)
    pub views: u64,
    pub thumb_url: String,
}

impl YtVideo {
    /// The canonical watch URL (fed to `yt-dlp -g` and used as the piece's virtual identity).
    pub fn watch_url(&self) -> String {
        format!("https://www.youtube.com/watch?v={}", self.id)
    }

    /// A compact human view-count ("1.2M", "56K", "812").
    pub fn views_short(&self) -> String {
        human_count(self.views)
    }

    /// `h:mm:ss` / `m:ss` duration, or "LIVE" when unknown/zero.
    pub fn duration_str(&self) -> String {
        if self.duration <= 0.0 {
            return "LIVE".into();
        }
        let t = self.duration as u64;
        let (h, m, s) = (t / 3600, (t % 3600) / 60, t % 60);
        if h > 0 {
            format!("{h}:{m:02}:{s:02}")
        } else {
            format!("{m}:{s:02}")
        }
    }
}

/// Rich per-video metadata — a full `yt-dlp --dump-json` of ONE video (slower than the flat
/// search, so it's fetched lazily when a video's Details pane is shown). Absent JSON fields stay
/// 0 / "". NB YouTube removed public **dislikes** in 2021, so there's no dislike count to show.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct YtMeta {
    pub id: String,
    pub title: String,
    pub channel: String,
    pub channel_url: String,
    pub upload_date: String, // raw "YYYYMMDD"
    pub views: u64,
    pub likes: u64,
    pub comments: u64,
    pub width: u32,
    pub height: u32,
    pub fps: f32,
    pub ext: String,
    pub filesize: u64, // bytes (exact or approx)
    pub description: String,
}

impl YtMeta {
    pub fn watch_url(&self) -> String {
        format!("https://www.youtube.com/watch?v={}", self.id)
    }
    pub fn views_short(&self) -> String {
        human_count(self.views)
    }
    pub fn likes_short(&self) -> String {
        human_count(self.likes)
    }
    pub fn comments_short(&self) -> String {
        human_count(self.comments)
    }
    /// "YYYY-MM-DD" from the raw "YYYYMMDD" (else the raw value).
    pub fn upload_date_fmt(&self) -> String {
        let d = &self.upload_date;
        if d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()) {
            format!("{}-{}-{}", &d[0..4], &d[4..6], &d[6..8])
        } else {
            d.clone()
        }
    }
}

/// Fetch one video's full metadata via `yt-dlp --dump-json` (no download). `None` if yt-dlp
/// fails / is absent — the caller degrades to whatever the flat search already had.
pub fn fetch_video_meta(id: &str, cookies: Option<&str>) -> Option<YtMeta> {
    let watch = format!("https://www.youtube.com/watch?v={id}");
    let mut cmd = Command::new("yt-dlp");
    cmd.args(["--dump-json", "--no-warnings", "--skip-download", "--"])
        .arg(&watch)
        .stderr(Stdio::null());
    push_cookie_args(&mut cmd, cookies);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_video_meta(&out.stdout)
}

/// Parse a `yt-dlp --dump-json` blob into [`YtMeta`]. Split out so it's unit-testable offline.
pub fn parse_video_meta(bytes: &[u8]) -> Option<YtMeta> {
    let d: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let s = |k: &str| {
        d.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let u = |k: &str| d.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    let first_nonempty = |a: &str, b: &str| {
        let x = s(a);
        if x.is_empty() {
            s(b)
        } else {
            x
        }
    };
    Some(YtMeta {
        id: s("id"),
        title: s("title"),
        channel: first_nonempty("channel", "uploader"),
        channel_url: first_nonempty("channel_url", "uploader_url"),
        upload_date: s("upload_date"),
        views: u("view_count"),
        likes: u("like_count"),
        comments: u("comment_count"),
        width: u("width") as u32,
        height: u("height") as u32,
        fps: d.get("fps").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
        ext: s("ext"),
        filesize: d
            .get("filesize")
            .and_then(|v| v.as_u64())
            .or_else(|| d.get("filesize_approx").and_then(|v| v.as_u64()))
            .unwrap_or(0),
        description: s("description"),
    })
}

/// One playlist from a channel's Playlists tab.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct YtPlaylist {
    pub id: String,
    pub title: String,
    pub count: u64, // video count (0 = unknown in flat mode)
    pub thumb_url: String,
}

/// Parse one `--flat-playlist --dump-json` line from a channel's *playlists* listing into a
/// [`YtPlaylist`]. `None` for non-playlist lines.
pub fn parse_playlist_entry(line: &str) -> Option<YtPlaylist> {
    let d: serde_json::Value = serde_json::from_str(line).ok()?;
    let id = d.get("id").and_then(|v| v.as_str()).filter(|s| !s.is_empty())?;
    let thumb_url = d
        .get("thumbnails")
        .and_then(|t| t.as_array())
        .and_then(|a| a.iter().rev().find_map(|t| t.get("url").and_then(|v| v.as_str())))
        .map(|s| s.to_string())
        .unwrap_or_default();
    Some(YtPlaylist {
        id: id.to_string(),
        title: d
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("(playlist)")
            .to_string(),
        count: d
            .get("playlist_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        thumb_url,
    })
}

/// Abbreviate a count the YouTube way: 1_234_567 → "1.2M".
fn human_count(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1e9)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

/// Parse one `yt-dlp --dump-json --flat-playlist` line into a [`YtVideo`]. `None` if there's no
/// id (playlist headers / malformed lines). Picks the largest thumbnail available.
pub fn parse_entry(line: &str) -> Option<YtVideo> {
    let d: serde_json::Value = serde_json::from_str(line).ok()?;
    let id = d.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return None;
    }
    let str_of = |keys: &[&str]| -> String {
        for k in keys {
            if let Some(s) = d.get(*k).and_then(|v| v.as_str()) {
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
        String::new()
    };
    // thumbnails: an array of {url, width, …}; the last is usually the largest. Fall back to
    // the flat `thumbnail` field, else a deterministic i.ytimg.com URL from the id.
    let thumb_url = d
        .get("thumbnails")
        .and_then(|t| t.as_array())
        .and_then(|a| {
            a.iter()
                .rev()
                .find_map(|t| t.get("url").and_then(|v| v.as_str()))
        })
        .map(|s| s.to_string())
        .or_else(|| {
            d.get("thumbnail")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg"));
    Some(YtVideo {
        id: id.to_string(),
        title: str_of(&["title"]),
        channel: str_of(&["channel", "uploader"]),
        channel_id: str_of(&["channel_id", "uploader_id"]),
        duration: d.get("duration").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
        views: d.get("view_count").and_then(|v| v.as_u64()).unwrap_or(0),
        thumb_url,
    })
}

/// Run `yt-dlp --flat-playlist --dump-json` over a `target` (a `ytsearchN:q` spec OR a channel/
/// playlist URL), capped to `n` entries, and map each JSON line with `f`. The shared engine behind
/// search / channel-videos / playlist-videos / channel-playlists. Empty vec on absence/error.
fn flat_list<T>(target: &str, n: usize, cookies: Option<&str>, f: impl Fn(&str) -> Option<T>) -> Vec<T> {
    let mut cmd = Command::new("yt-dlp");
    cmd.args([
        "--flat-playlist",
        "--dump-json",
        "--no-warnings",
        "-I",
        &format!("1:{}", n.max(1)),
        target,
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    push_cookie_args(&mut cmd, cookies);
    let Ok(out) = cmd.output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout).lines().filter_map(f).collect()
}

/// Run a YouTube search (`ytsearch<n>:<query>`). Empty vec if yt-dlp is absent or errors.
pub fn search(query: &str, n: usize, cookies: Option<&str>) -> Vec<YtVideo> {
    flat_list(&format!("ytsearch{}:{}", n.max(1), query), n, cookies, parse_entry)
}

/// A channel's uploaded videos (newest first), by UC… id.
pub fn channel_videos(channel_id: &str, n: usize, cookies: Option<&str>) -> Vec<YtVideo> {
    let url = format!("https://www.youtube.com/channel/{channel_id}/videos");
    flat_list(&url, n, cookies, parse_entry)
}

/// A playlist's videos (in order), by playlist id.
pub fn playlist_videos(playlist_id: &str, n: usize, cookies: Option<&str>) -> Vec<YtVideo> {
    let url = format!("https://www.youtube.com/playlist?list={playlist_id}");
    flat_list(&url, n, cookies, parse_entry)
}

/// A channel's playlists, by UC… id.
pub fn channel_playlists(channel_id: &str, n: usize, cookies: Option<&str>) -> Vec<YtPlaylist> {
    let url = format!("https://www.youtube.com/channel/{channel_id}/playlists");
    flat_list(&url, n, cookies, parse_playlist_entry)
}

/// Append `--cookies-from-browser <b>` when a browser is configured (Preferences → YouTube
/// cookies) — authenticates yt-dlp as the signed-in user, which clears YouTube's "confirm you're
/// not a bot" gate + reaches age-restricted / members' videos. Empty ⇒ no cookies (anonymous).
fn push_cookie_args(cmd: &mut Command, cookies: Option<&str>) {
    if let Some(b) = cookies {
        if !b.trim().is_empty() {
            cmd.args(["--cookies-from-browser", b.trim()]);
        }
    }
}

/// Resolve a **single** direct stream URL for `id` — a progressive/muxed format ≤720p so it's one
/// URL ffmpeg (and thus our `VideoPlayer`) can read like a file. `None` if yt-dlp fails / is absent.
pub fn stream_url(id: &str) -> Option<String> {
    let watch = format!("https://www.youtube.com/watch?v={id}");
    let out = Command::new("yt-dlp")
        .args([
            "-f",
            "best[height<=720][ext=mp4]/best[height<=720]/best",
            "--no-warnings",
            "-g",
            &watch,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|l| l.to_string())
}

/// A live download-progress update parsed from yt-dlp's `--progress-template` output.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct DlProgress {
    pub pct: f32,      // 0..100 (resets per merged stream — video then audio)
    pub eta: String,   // "mm:ss" (or "" when unknown)
    pub speed: String, // e.g. "1.23MiB/s" (or "" when unknown)
}

impl DlProgress {
    pub fn pct_str(&self) -> String {
        format!("{:.0}%", self.pct)
    }
}

/// Parse one `PV|<pct>|<eta>|<speed>` line from our `--progress-template`. `None` for other lines.
fn parse_progress(line: &str) -> Option<DlProgress> {
    let rest = line.strip_prefix("PV|")?;
    let mut it = rest.split('|');
    let pct = it.next()?.trim().trim_end_matches('%').trim().parse::<f32>().ok()?;
    let clean = |s: &str| {
        let s = s.trim();
        if s.is_empty() || s.contains("Unknown") || s == "N/A" {
            String::new()
        } else {
            s.to_string()
        }
    };
    Some(DlProgress {
        pct,
        eta: clean(it.next().unwrap_or("")),
        speed: clean(it.next().unwrap_or("")),
    })
}

/// Remove a video's partial download artifacts (`<id>.*.part`, fragments, `.ytdl`) so an aborted
/// download isn't mistaken for a finished file on the next cache-hit scan.
fn remove_partials(dir: &std::path::Path, id: &str) {
    let prefix = format!("{id}.");
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with(&prefix)
                && (name.ends_with(".part")
                    || name.contains(".part-")
                    || name.ends_with(".ytdl")
                    || name.contains(".f")) // per-stream temp files (e.g. id.f137.mp4)
            {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Download a video **in place** to `dir` as `<id>.<ext>` and return the produced file path — so
/// it plays via the normal local `VideoPlayer` (streaming via `-g` is unreliable under YouTube
/// SABR). `max_height` caps the resolution (0 = best available; video+audio are merged via ffmpeg
/// so 1080p+ works). Cache-first: an existing `<id>.*` is reused with no network.
///
/// `cancel` is polled between yt-dlp progress lines — setting it kills yt-dlp and cleans up the
/// partial download (returns `None`). `on_progress` is called for each progress update (percent /
/// ETA / speed) so the UI can show a live readout. `None` if yt-dlp can't produce a file
/// (absent / too old / SABR-blocked / aborted → caller shows a hint).
pub fn download(
    id: &str,
    dir: &std::path::Path,
    max_height: u32,
    cookies: Option<&str>,
    cancel: &std::sync::atomic::AtomicBool,
    on_progress: &mut dyn FnMut(DlProgress),
) -> Result<std::path::PathBuf, String> {
    use std::io::{BufRead, BufReader, Read};
    use std::sync::atomic::Ordering::Relaxed;
    let found = |dir: &std::path::Path| -> Option<std::path::PathBuf> {
        std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
            let p = e.path();
            (p.file_stem().and_then(|s| s.to_str()) == Some(id)
                && p.extension().is_some_and(|x| x != "part"))
            .then_some(p)
        })
    };
    // Cache hit: any already-downloaded `<id>.<ext>` in `dir`.
    if let Some(p) = found(dir) {
        return Ok(p);
    }
    let _ = std::fs::create_dir_all(dir);
    let watch = format!("https://www.youtube.com/watch?v={id}");
    let out_tmpl = dir.join(format!("{id}.%(ext)s"));
    // Merge the best video+audio at/under the cap (falls back to a progressive stream, then best).
    let fmt = if max_height == 0 {
        "bestvideo+bestaudio/best".to_string()
    } else {
        format!(
            "bestvideo[height<={h}]+bestaudio/best[height<={h}]/best",
            h = max_height
        )
    };
    // `--newline` + a machine-readable progress template on stdout → parse per-line for the ETA.
    let mut cmd = Command::new("yt-dlp");
    cmd.args([
        "-f",
        &fmt,
        "--no-warnings",
        "--newline",
        "--progress-template",
        "PV|%(progress._percent_str)s|%(progress._eta_str)s|%(progress._speed_str)s",
        "-o",
    ])
    .arg(&out_tmpl)
    .arg(&watch)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    push_cookie_args(&mut cmd, cookies);
    let mut child = cmd
        .spawn()
        .map_err(|_| "yt-dlp not found on PATH — install it (see the README).".to_string())?;
    // Drain stderr on a thread (so a full pipe can't deadlock the download) and keep it for a
    // precise failure message — the "confirm you're not a bot" gate needs a very different hint
    // than "update yt-dlp".
    let stderr = child.stderr.take();
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut e) = stderr {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines() {
            if cancel.load(Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                let _ = err_thread.join();
                remove_partials(dir, id);
                return Err("aborted".to_string());
            }
            let Ok(line) = line else { break };
            if let Some(p) = parse_progress(&line) {
                on_progress(p);
            }
        }
    }
    let status = child.wait().ok();
    let stderr_text = err_thread.join().unwrap_or_default();
    if cancel.load(Relaxed) {
        remove_partials(dir, id);
        return Err("aborted".to_string());
    }
    if status.map(|s| s.success()).unwrap_or(false) {
        return found(dir).ok_or_else(|| "yt-dlp finished but produced no file".to_string());
    }
    // Failure: map the stderr to an actionable hint.
    let low = stderr_text.to_lowercase();
    if low.contains("not a bot")
        || low.contains("sign in to confirm")
        || low.contains("http error 429")
        || low.contains("rate limit")
        || low.contains("too many requests")
    {
        Err("YouTube is rate-limiting this IP (bot check). Set “YouTube cookies from browser” \
             in Preferences, or wait a while."
            .to_string())
    } else if low.contains("age") && low.contains("confirm") {
        Err("Age-restricted — set “YouTube cookies from browser” in Preferences.".to_string())
    } else {
        Err("YouTube download failed — update yt-dlp (`yt-dlp -U` / pip install -U yt-dlp).".to_string())
    }
}

/// Is `yt-dlp` on PATH? (Gates the YouTube UI / shows a "install yt-dlp" hint.)
pub fn available() -> bool {
    Command::new("yt-dlp")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_flat_entry() {
        let line = r#"{"id":"abc123","title":"Cool Video","channel":"Some Chan","duration":135.0,"view_count":1234567,"thumbnails":[{"url":"http://a/1.jpg","width":90},{"url":"http://a/2.jpg","width":720}]}"#;
        let v = parse_entry(line).expect("parses");
        assert_eq!(v.id, "abc123");
        assert_eq!(v.title, "Cool Video");
        assert_eq!(v.channel, "Some Chan");
        assert_eq!(v.views, 1234567);
        assert_eq!(v.thumb_url, "http://a/2.jpg"); // largest (last)
        assert_eq!(v.duration_str(), "2:15");
        assert_eq!(v.views_short(), "1.2M");
        assert_eq!(v.watch_url(), "https://www.youtube.com/watch?v=abc123");
    }

    #[test]
    fn uploader_falls_back_to_channel_and_derives_thumb() {
        let line = r#"{"id":"xyz","title":"T","uploader":"Chan2","duration":0}"#;
        let v = parse_entry(line).expect("parses");
        assert_eq!(v.channel, "Chan2");
        assert_eq!(v.duration_str(), "LIVE"); // 0 duration
        assert_eq!(v.thumb_url, "https://i.ytimg.com/vi/xyz/hqdefault.jpg");
    }

    #[test]
    fn no_id_is_rejected() {
        assert!(parse_entry(r#"{"title":"no id"}"#).is_none());
        assert!(parse_entry("not json").is_none());
    }

    #[test]
    fn long_duration_and_counts() {
        let line = r#"{"id":"a","title":"t","duration":3725,"view_count":56269256}"#;
        let v = parse_entry(line).unwrap();
        assert_eq!(v.duration_str(), "1:02:05");
        assert_eq!(v.views_short(), "56.3M");
    }

    #[test]
    fn parses_full_video_meta() {
        let json = r#"{"id":"abc","title":"Cool","channel":"Chan","channel_url":"https://youtube.com/@chan",
            "upload_date":"20240115","view_count":1234567,"like_count":45000,"comment_count":890,
            "width":1920,"height":1080,"fps":30.0,"ext":"mp4","filesize_approx":123456789}"#;
        let m = parse_video_meta(json.as_bytes()).unwrap();
        assert_eq!(m.channel, "Chan");
        assert_eq!(m.channel_url, "https://youtube.com/@chan");
        assert_eq!(m.upload_date_fmt(), "2024-01-15");
        assert_eq!(m.views_short(), "1.2M");
        assert_eq!(m.likes_short(), "45.0K");
        assert_eq!((m.width, m.height), (1920, 1080));
        assert_eq!(m.filesize, 123456789);
    }

    #[test]
    fn parses_download_progress() {
        let p = parse_progress("PV|  0.0%|00:11|   4.24MiB/s").unwrap();
        assert_eq!(p.pct, 0.0);
        assert_eq!(p.eta, "00:11");
        assert_eq!(p.speed, "4.24MiB/s");
        assert_eq!(p.pct_str(), "0%");
        // "Unknown" fields collapse to empty.
        let u = parse_progress("PV| 12.5%|Unknown| Unknown B/s").unwrap();
        assert_eq!(u.pct, 12.5);
        assert_eq!(u.eta, "");
        assert_eq!(u.speed, "");
        // Non-progress lines are ignored.
        assert!(parse_progress("[download] Destination: foo.mp4").is_none());
    }

    #[test]
    fn video_meta_falls_back_to_uploader() {
        let json = r#"{"id":"x","title":"t","uploader":"UpChan","uploader_url":"https://u/url"}"#;
        let m = parse_video_meta(json.as_bytes()).unwrap();
        assert_eq!(m.channel, "UpChan");
        assert_eq!(m.channel_url, "https://u/url");
        assert_eq!(m.upload_date_fmt(), ""); // no date → empty
    }
}
