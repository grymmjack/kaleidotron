//! Free **audio** search via [Openverse](https://api.openverse.org) — the same keyless CC API that
//! backs [`crate::imgsearch`], but its `/v1/audio/` half. Most results proxy Freesound, so a query
//! like "drum loop" returns CC0 one-shots and loops that drop straight onto kaleidotron's sample pads.
//!
//! The point of this source is the **sampler pipeline**: a result downloads to a real local file, so
//! it opens in the waveform editor, gets trimmed/looped/enveloped, lands on a pad, and exports to
//! SFZ — every existing audio feature applies with no special-casing.
//!
//! Pure + unit-tested here; the egui/threading wiring lives in `app.rs` (the `snd_*` machinery,
//! parallel to `img_*`).

use std::path::Path;

/// Virtual root for audio-search browsing.
pub const ROOT: &str = "<audio>";
/// The search facet: `<audio>/search/<query>`.
pub const SEARCH: &str = "search";

const API: &str = "https://api.openverse.org/v1/audio/";
/// Openverse caps **anonymous** requests at `page_size ≤ 20` — a larger value 401s and silently
/// yields zero results (the exact bug that once made image search look broken). Do not raise this
/// without an API key. See `imgsearch::PAGE_MAX`.
const PAGE_MAX: usize = 20;

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

/// One Openverse audio result.
#[derive(Clone, Debug, Default)]
pub struct SndResult {
    pub id: String,
    pub title: String,
    pub creator: String,
    pub license: String,
    pub provider: String,
    pub url: String,      // the audio file itself
    pub page_url: String, // source landing page (attribution)
    pub genres: Vec<String>,
    pub duration_ms: u64,
    pub sample_rate: u32,
    pub bit_rate: u32,
    pub ext: String, // mp3 / wav / flac / ogg …
}

impl SndResult {
    /// "CC BY" / "CC0" style label.
    pub fn license_label(&self) -> String {
        if self.license.is_empty() {
            return "—".into();
        }
        let up = self.license.to_uppercase();
        if up == "PDM" || up == "CC0" {
            up
        } else {
            format!("CC {up}")
        }
    }

    /// `m:ss` (empty when the API omits a duration).
    pub fn duration_label(&self) -> String {
        if self.duration_ms == 0 {
            return String::new();
        }
        let secs = self.duration_ms / 1000;
        format!("{}:{:02}", secs / 60, secs % 60)
    }

    /// `44.1 kHz` (empty when unknown).
    pub fn rate_label(&self) -> String {
        if self.sample_rate == 0 {
            String::new()
        } else {
            format!("{:.1} kHz", self.sample_rate as f32 / 1000.0)
        }
    }

    /// A safe local filename: `<title-slug> [<id8>].<ext>`. Keeping the real extension matters —
    /// the decoder registry dispatches audio by extension.
    pub fn filename(&self) -> String {
        let slug: String = self
            .title
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>()
            .trim_matches('_')
            .chars()
            .take(48)
            .collect();
        let slug = if slug.is_empty() { "audio".into() } else { slug };
        let id8: String = self.id.chars().take(8).collect();
        format!("{slug} [{id8}].{}", self.ext)
    }
}

/// Infer the extension from the API `filetype`, else the URL, else `mp3`.
fn infer_ext(filetype: Option<&str>, url: &str) -> String {
    if let Some(ft) = filetype {
        let ft = ft.trim().to_ascii_lowercase();
        if !ft.is_empty() {
            return ft;
        }
    }
    let tail = url.split(['?', '#']).next().unwrap_or(url);
    let ext = tail.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "mp3" | "wav" | "ogg" | "flac" | "oga" | "m4a" | "aiff" | "aif" => ext,
        _ => "mp3".into(),
    }
}

/// Parse an Openverse `/v1/audio/` body into results.
pub fn parse_results(bytes: &[u8]) -> Vec<SndResult> {
    let v: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(arr) = v["results"].as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|r| {
            let id = r["id"].as_str()?.to_string();
            let url = r["url"].as_str().unwrap_or_default().to_string();
            if url.is_empty() {
                return None;
            }
            let ext = infer_ext(r["filetype"].as_str(), &url);
            Some(SndResult {
                id,
                title: r["title"].as_str().unwrap_or("Untitled").to_string(),
                creator: r["creator"].as_str().unwrap_or_default().to_string(),
                license: r["license"].as_str().unwrap_or_default().to_string(),
                provider: r["provider"].as_str().unwrap_or_default().to_string(),
                page_url: r["foreign_landing_url"].as_str().unwrap_or_default().to_string(),
                genres: r["genres"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|g| g.as_str().map(String::from)).collect())
                    .unwrap_or_default(),
                duration_ms: r["duration"].as_u64().unwrap_or(0),
                sample_rate: r["sample_rate"].as_u64().unwrap_or(0) as u32,
                bit_rate: r["bit_rate"].as_u64().unwrap_or(0) as u32,
                url,
                ext,
            })
        })
        .collect()
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

fn page_url(query: &str, page: usize) -> String {
    format!("{API}?q={}&page_size={PAGE_MAX}&page={page}", enc(query))
}

/// Search Openverse audio for `query`, up to `n` results (paged in 20s, cached 1 day).
pub fn search(query: &str, n: usize) -> Result<Vec<SndResult>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let want = n.clamp(1, 240);
    let pages = want.div_ceil(PAGE_MAX);
    let mut out: Vec<SndResult> = Vec::new();
    for page in 1..=pages {
        let body = crate::cache::get_bytes(&page_url(q, page), Some(86_400))?;
        let batch = parse_results(&body);
        if batch.is_empty() {
            break;
        }
        out.extend(batch);
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

    const SAMPLE: &[u8] = br#"{"result_count":2,"results":[
      {"id":"aaaa1111-2222","title":"Tribe Drum Loop","creator":"someone","license":"cc0",
       "provider":"freesound","url":"https://cdn.freesound.org/previews/342/342465_3906011-hq.mp3",
       "foreign_landing_url":"https://freesound.org/s/342465/","filetype":"mp3","duration":8275,
       "sample_rate":44100,"bit_rate":128000,"genres":["percussion"]},
      {"id":"bbbb","title":"Pad","creator":"x","license":"by","provider":"jamendo",
       "url":"https://example.org/track?trackid=1","foreign_landing_url":"https://x","duration":0}
    ]}"#;

    #[test]
    fn parses_audio_results() {
        let r = parse_results(SAMPLE);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].ext, "mp3");
        assert_eq!(r[0].duration_label(), "0:08");
        assert_eq!(r[0].rate_label(), "44.1 kHz");
        assert_eq!(r[0].license_label(), "CC0");
        assert_eq!(r[0].genres, vec!["percussion"]);
        assert!(r[0].filename().starts_with("Tribe_Drum_Loop ["));
        assert!(r[0].filename().ends_with(".mp3"));
        // No filetype + a query-string URL → falls back to mp3, and no duration reads as blank.
        assert_eq!(r[1].ext, "mp3");
        assert_eq!(r[1].duration_label(), "");
        assert_eq!(r[1].license_label(), "CC BY");
    }

    #[test]
    fn infers_extension_from_url_when_filetype_missing() {
        assert_eq!(infer_ext(None, "https://x/a.wav"), "wav");
        assert_eq!(infer_ext(None, "https://x/a.flac?k=1"), "flac");
        assert_eq!(infer_ext(Some("OGG"), "https://x/a.mp3"), "ogg");
        assert_eq!(infer_ext(None, "https://x/nothing"), "mp3");
    }

    #[test]
    fn paths_and_anonymous_page_cap() {
        let p = PathBuf::from(ROOT).join(SEARCH).join("drum loop");
        assert!(is_remote(&p));
        assert_eq!(rel_parts(&p), vec!["search", "drum loop"]);
        // Guard the cap that silently empties results when exceeded.
        assert!(PAGE_MAX <= 20, "anonymous page_size cap is 20");
        assert!(page_url("drum loop", 2).contains("page_size=20"));
        assert!(page_url("drum loop", 2).contains("q=drum%20loop"));
    }
}

#[cfg(test)]
mod live {
    use super::*;
    #[test]
    #[ignore = "hits the live network"]
    fn live_search() {
        match search("drum loop", 6) {
            Ok(v) => {
                eprintln!("got {} audio results", v.len());
                for r in v.iter().take(4) {
                    eprintln!(
                        "  {} | {} | {} | {} | {}",
                        r.title,
                        r.duration_label(),
                        r.license_label(),
                        r.rate_label(),
                        r.filename()
                    );
                }
            }
            Err(e) => eprintln!("ERROR: {e}"),
        }
    }
}
