//! Video support: the grid tile is a REAL frame grabbed from the file by **ffmpeg**
//! (`grab_frame`: ffmpeg opens the path itself → one PNG on stdout, no bundled lib), and
//! metadata (duration / fps / dimensions / codec / audio-track presence) comes from
//! **ffprobe** as JSON. Both degrade gracefully to `None` / a labeled placeholder when the
//! tools aren't installed — the same ethos as `pdf.rs` (poppler) and `mesh3d.rs` (blender).
//!
//! Decoding is path-routed (registered in `decode_bytes` *before* the sniff loop) because
//! ffmpeg needs the real file path, not a byte slice — mirroring the mesh3d/blend/mtl route.
//! The interactive player (streaming frame pipe + soundtrack) lives in `crate::video`; this
//! module is only the still-frame + metadata side used by the thumbnailer and Details pane.

use super::{DecodeError, Decoder};
use crate::image_types::PixImage;
use std::path::Path;
use std::process::{Command, Stdio};

/// Container extensions we treat as video. Routed to the [`VideoDecoder`] plugin.
pub const VIDEO_EXTS: &[&str] = &[
    "mp4", "m4v", "mkv", "webm", "mov", "avi", "wmv", "flv", "mpg", "mpeg", "mts", "m2ts", "ts",
    "ogv",
    "3gp", // HLS/DASH manifests — Steam trailers etc. (ffmpeg reads them like a file):
    "m3u8", "mpd",
];

/// Parsed video metadata for the Details pane and the player. `duration` is seconds, `fps`
/// is frames-per-second (already reduced from ffprobe's `num/den` string).
#[derive(Clone, Default, Debug)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub duration: f32,
    pub fps: f32,
    pub vcodec: String,
    pub has_audio: bool,
}

/// Best-effort probe via `ffprobe -print_format json`. `None` if ffprobe is absent, errors,
/// or the file has no video stream. Never panics.
pub fn probe(path: &Path) -> Option<VideoInfo> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_streams",
            "-show_format",
        ])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let streams = json.get("streams")?.as_array()?;

    let mut info = VideoInfo::default();
    let mut found_video = false;
    for s in streams {
        match s.get("codec_type").and_then(|v| v.as_str()) {
            Some("video") if !found_video => {
                found_video = true;
                info.width = s.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                info.height = s.get("height").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                info.vcodec = s
                    .get("codec_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // fps arrives as a fraction string, e.g. "30000/1001" or "25/1".
                if let Some(r) = s.get("avg_frame_rate").and_then(|v| v.as_str()) {
                    info.fps = parse_fraction(r);
                }
                if info.fps <= 0.0 {
                    if let Some(r) = s.get("r_frame_rate").and_then(|v| v.as_str()) {
                        info.fps = parse_fraction(r);
                    }
                }
                // Per-stream duration if present (else the container's, below).
                if let Some(d) = s.get("duration").and_then(|v| v.as_str()) {
                    info.duration = d.parse().unwrap_or(0.0);
                }
            }
            Some("audio") => info.has_audio = true,
            _ => {}
        }
    }
    if !found_video {
        return None;
    }
    // Container duration is usually the most reliable — prefer it when the stream lacked one.
    if info.duration <= 0.0 {
        if let Some(d) = json
            .get("format")
            .and_then(|f| f.get("duration"))
            .and_then(|v| v.as_str())
        {
            info.duration = d.parse().unwrap_or(0.0);
        }
    }
    Some(info)
}

/// Parse ffprobe's `"num/den"` frame-rate string into fps. `"0/0"` / malformed → 0.0.
fn parse_fraction(s: &str) -> f32 {
    let mut it = s.split('/');
    let num: f32 = it.next().and_then(|n| n.parse().ok()).unwrap_or(0.0);
    let den: f32 = it.next().and_then(|d| d.parse().ok()).unwrap_or(1.0);
    if den.abs() < f32::EPSILON {
        0.0
    } else {
        num / den
    }
}

/// The timestamp (seconds) to grab a representative thumbnail from a clip of length
/// `duration`. Seeking a little in avoids black intro/fade frames, but must stay well
/// inside a short clip's length or the grab returns zero frames.
///
/// `duration <= 0.0` means "unknown" (probe failed) — pick a small safe default.
fn thumb_seek_secs(duration: f32) -> f32 {
    if duration <= 0.0 {
        // Unknown length: a small offset that's safe for almost any real clip.
        1.0
    } else {
        // 10% in, but never within the last second (guards very short clips).
        (duration * 0.10).clamp(0.0, (duration - 1.0).max(0.0))
    }
}

/// Grab a single frame to a `PixImage`. `at_secs` seeks before decoding (keyframe-accurate
/// fast seek via `-ss` before `-i`); `longest` fits the frame inside a `longest × longest`
/// box (preserving aspect). `None` on any failure (ffmpeg absent, seek past EOF, decode fail).
pub fn grab_frame(path: &Path, at_secs: f32, longest: Option<u32>) -> Option<PixImage> {
    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-v").arg("quiet").arg("-nostdin");
    // `-ss` BEFORE `-i` = fast (keyframe) seek. Skip it at (near) zero to grab the first frame.
    if at_secs > 0.05 {
        cmd.arg("-ss").arg(format!("{at_secs:.3}"));
    }
    cmd.arg("-i").arg(path);
    cmd.arg("-frames:v").arg("1");
    if let Some(px) = longest {
        let px = px.max(16);
        // Fit inside px×px, preserving aspect. `decrease` never enlarges past the box.
        cmd.arg("-vf").arg(format!(
            "scale={px}:{px}:force_original_aspect_ratio=decrease"
        ));
    }
    cmd.arg("-f")
        .arg("image2pipe")
        .arg("-vcodec")
        .arg("png")
        .arg("-");

    let out = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    let img = image::load_from_memory(&out.stdout).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let px: Vec<[u8; 4]> = img
        .into_raw()
        .chunks_exact(4)
        .map(|c| [c[0], c[1], c[2], c[3]])
        .collect();
    if px.len() != (w * h) as usize {
        return None;
    }
    Some(PixImage::from_rgba(w, h, px))
}

/// The grid-tile decode: a representative frame, fit inside `size`. Probes for duration to
/// pick the seek point, then grabs — falling back to frame 0 if the seek yields nothing
/// (very short clip) and to a labeled placeholder if ffmpeg is unavailable entirely.
pub fn decode_thumb(path: &Path, size: u32) -> Result<PixImage, DecodeError> {
    let info = probe(path);
    let seek = thumb_seek_secs(info.as_ref().map(|i| i.duration).unwrap_or(0.0));
    if let Some(img) = grab_frame(path, seek, Some(size)) {
        return Ok(img);
    }
    // Seek overshot (short clip) or the codec dislikes fast-seek → try the very first frame.
    if let Some(img) = grab_frame(path, 0.0, Some(size)) {
        return Ok(img);
    }
    // ffmpeg absent / undecodable: a recognizable placeholder so the tile isn't blank.
    Ok(render_placeholder(path))
}

// Placeholder colors (mirrors pdf.rs's palette).
const CANVAS: [u8; 4] = [24, 24, 30, 255];
const FILM: [u8; 4] = [40, 40, 52, 255];
const ACCENT: [u8; 4] = [86, 140, 220, 255];
const LIGHT: [u8; 4] = [220, 220, 228, 255];

/// A recognizable "video" tile when ffmpeg can't produce a frame: a dark card with a play
/// triangle and the file's extension. Used only on the failure path.
fn render_placeholder(path: &Path) -> PixImage {
    let (w, h) = (320usize, 240usize);
    let mut px = vec![CANVAS; w * h];
    // Inset film panel.
    for y in 16..h - 16 {
        for x in 16..w - 16 {
            px[y * w + x] = FILM;
        }
    }
    // A centered play triangle.
    let (cx, cy) = (w as i32 / 2, h as i32 / 2);
    let r = 44i32;
    for y in -r..r {
        // Triangle half-width shrinks toward the tip on the right.
        let frac = 1.0 - (y.abs() as f32 / r as f32);
        let half = (r as f32 * frac) as i32;
        for x in -r / 2..half {
            let (ix, iy) = (cx + x - r / 6, cy + y);
            if ix >= 0 && iy >= 0 && (ix as usize) < w && (iy as usize) < h {
                px[iy as usize * w + ix as usize] = ACCENT;
            }
        }
    }
    // Extension label, bottom-center.
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("video")
        .to_uppercase();
    blit_label(&mut px, w, &ext, h - 40, LIGHT);
    PixImage::from_rgba(w as u32, h as u32, px)
}

/// Draw an uppercase ASCII label centered horizontally via the CP437 8×16 font at 2× scale.
fn blit_label(px: &mut [[u8; 4]], w: usize, s: &str, y: usize, c: [u8; 4]) {
    use super::cp437_font::CP437_8X16;
    let scale = 2usize;
    let tw = s.chars().count() * 8 * scale;
    let x0 = (w.saturating_sub(tw)) / 2;
    for (i, ch) in s.chars().enumerate() {
        let byte = if (0x20..0x7f).contains(&(ch as u32)) {
            ch as u8
        } else {
            b'?'
        };
        let glyph = &CP437_8X16[byte as usize];
        let gx = x0 + i * 8 * scale;
        for (ry, &bits) in glyph.iter().enumerate() {
            for rx in 0..8 {
                if (bits >> (7 - rx)) & 1 == 1 {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            let (xx, yy) = (gx + rx * scale + sx, y + ry * scale + sy);
                            if xx < w {
                                let idx = yy * w + xx;
                                if idx < px.len() {
                                    px[idx] = c;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Path-routed decoder (registered so its extensions appear in `known_extension`). The real
/// work is [`decode_thumb`], called from `decode_bytes` with the PATH — `decode(bytes)` is
/// never reached for video (ffmpeg needs the file), mirroring `MeshDecoder`.
pub struct VideoDecoder;

impl Decoder for VideoDecoder {
    fn name(&self) -> &'static str {
        "video"
    }
    fn extensions(&self) -> &'static [&'static str] {
        VIDEO_EXTS
    }
    fn sniff(&self, _: &[u8]) -> bool {
        // Routed by extension (needs the path); no byte sniff (containers overlap, and
        // ffmpeg reads the file directly anyway).
        false
    }
    fn decode(&self, _: &[u8]) -> Result<PixImage, DecodeError> {
        // Never reached via bytes — the registry routes VIDEO_EXTS to decode_thumb(path).
        Err(DecodeError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_parsing() {
        assert!((parse_fraction("30000/1001") - 29.97).abs() < 0.01);
        assert!((parse_fraction("25/1") - 25.0).abs() < 0.001);
        assert_eq!(parse_fraction("0/0"), 0.0);
        assert_eq!(parse_fraction("garbage"), 0.0);
    }

    #[test]
    fn thumb_seek_heuristic() {
        // Unknown length → small safe default.
        assert_eq!(thumb_seek_secs(0.0), 1.0);
        // 10% in for a normal clip.
        assert!((thumb_seek_secs(100.0) - 10.0).abs() < 0.001);
        // Very short clip never seeks past (duration - 1s).
        assert_eq!(thumb_seek_secs(0.5), 0.0);
        assert!(thumb_seek_secs(2.0) <= 1.0);
    }

    #[test]
    fn extensions_registered() {
        assert!(VIDEO_EXTS.contains(&"mp4"));
        assert!(VIDEO_EXTS.contains(&"webm"));
        assert!(!VideoDecoder.sniff(b"\x00\x00\x00\x20ftypmp42"));
    }

    #[test]
    fn placeholder_renders() {
        let img = render_placeholder(Path::new("clip.mp4"));
        assert!(img.width > 100 && img.height > 100);
    }
}
