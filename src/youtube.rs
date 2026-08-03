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
    pub duration: f32, // seconds; 0 = unknown (e.g. a live stream)
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
pub fn fetch_video_meta(id: &str) -> Option<YtMeta> {
    let watch = format!("https://www.youtube.com/watch?v={id}");
    let out = Command::new("yt-dlp")
        .args(["--dump-json", "--no-warnings", "--skip-download", "--"])
        .arg(&watch)
        .stderr(Stdio::null())
        .output()
        .ok()?;
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
        channel: str_of(&["channel", "uploader", "channel_id"]),
        duration: d.get("duration").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
        views: d.get("view_count").and_then(|v| v.as_u64()).unwrap_or(0),
        thumb_url,
    })
}

/// Run a YouTube search (`ytsearch<n>:<query>`) via yt-dlp's flat/fast mode. Empty vec if yt-dlp
/// is absent or errors — the caller shows a "not installed / no results" status.
pub fn search(query: &str, n: usize) -> Vec<YtVideo> {
    let spec = format!("ytsearch{}:{}", n.max(1), query);
    let out = Command::new("yt-dlp")
        .args(["--dump-json", "--flat-playlist", "--no-warnings", &spec])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    let Ok(out) = out else { return Vec::new() };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(parse_entry)
        .collect()
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
    cancel: &std::sync::atomic::AtomicBool,
    on_progress: &mut dyn FnMut(DlProgress),
) -> Option<std::path::PathBuf> {
    use std::io::{BufRead, BufReader};
    use std::sync::atomic::Ordering::Relaxed;
    // Cache hit: any already-downloaded `<id>.<ext>` in `dir`.
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.file_stem().and_then(|s| s.to_str()) == Some(id)
                && p.extension().is_some_and(|x| x != "part")
            {
                return Some(p);
            }
        }
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
    let mut child = Command::new("yt-dlp")
        .args([
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
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines() {
            if cancel.load(Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                remove_partials(dir, id);
                return None;
            }
            let Ok(line) = line else { break };
            if let Some(p) = parse_progress(&line) {
                on_progress(p);
            }
        }
    }
    let status = child.wait().ok()?;
    if cancel.load(Relaxed) {
        remove_partials(dir, id);
        return None;
    }
    if !status.success() {
        return None;
    }
    // Find the produced `<id>.<ext>`.
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let p = e.path();
        (p.file_stem().and_then(|s| s.to_str()) == Some(id)
            && p.extension().is_some_and(|x| x != "part"))
        .then_some(p)
    })
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
