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

/// Download a video **in place** to `dir` as `<id>.<ext>` and return the produced file path — so
/// it plays via the normal local `VideoPlayer` (streaming via `-g` is unreliable under YouTube
/// SABR). `max_height` caps the resolution (0 = best available; video+audio are merged via ffmpeg
/// so 1080p+ works). Cache-first: an existing `<id>.*` is reused with no network. `None` if yt-dlp
/// can't produce a file (absent / too old / SABR-blocked → caller shows a hint).
pub fn download(id: &str, dir: &std::path::Path, max_height: u32) -> Option<std::path::PathBuf> {
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
    let ok = Command::new("yt-dlp")
        .args(["-f", &fmt, "--no-warnings", "-o"])
        .arg(&out_tmpl)
        .arg(&watch)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
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
}
