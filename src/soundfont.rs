//! Read-only SoundFont (`.sf2`) support: browse a soundfont **as a virtual folder** of its
//! samples. We parse with `rustysynth` (pure Rust, std-only) and extract every sample to a
//! 16-bit WAV in a per-file temp dir, which the app mounts exactly like an archive — so each
//! sample then opens in the in-app audio player, plays, and can be rated/exported like any
//! file, and the whole `.sf2` reads as an enterable "folder" in the grid/table.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Recognize a SoundFont by extension (`.sf2`).
pub fn is_soundfont(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("sf2"))
}

/// Preset / instrument / sample counts for the Details pane ("how many sounds are in here").
#[derive(Clone)]
pub struct SoundFontInfo {
    pub name: String,
    pub presets: usize,
    pub instruments: usize,
    pub samples: usize,
}

/// Parse just the directory of `sf2` to report its contents. `None` on any parse error.
pub fn info(sf2: &Path) -> Option<SoundFontInfo> {
    let mut f = std::fs::File::open(sf2).ok()?;
    let sf = rustysynth::SoundFont::new(&mut f).ok()?;
    Some(SoundFontInfo {
        name: sf.get_info().get_bank_name().trim().to_string(),
        presets: sf.get_presets().len(),
        instruments: sf.get_instruments().len(),
        samples: sf.get_sample_headers().len(),
    })
}

/// Extract every SoundFont sample to a 16-bit WAV in a per-file temp dir, and return that
/// dir (mounted like an archive). Cached by the file's path+size+mtime, so an unchanged
/// soundfont is unpacked only once (a `.pv_extracted` marker signals completion).
pub fn extract_to_cache(sf2: &Path) -> io::Result<PathBuf> {
    let dest = cache_dir(sf2)?;
    let marker = dest.join(".pv_extracted");
    if marker.exists() {
        return Ok(dest);
    }
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(&dest)?;
    let mut f = std::fs::File::open(sf2)?;
    let sf = rustysynth::SoundFont::new(&mut f)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad sf2: {e:?}")))?;
    // One shared i16 buffer holds every sample's PCM back to back; each header slices it.
    let wave = sf.get_wave_data();
    let mut written = 0usize;
    for (i, h) in sf.get_sample_headers().iter().enumerate() {
        let (start, end) = (h.get_start(), h.get_end());
        if start < 0 || end <= start {
            continue;
        }
        let (s, e) = (start as usize, end as usize);
        if e > wave.len() {
            continue;
        }
        let pcm = &wave[s..e];
        if pcm.is_empty() {
            continue;
        }
        let raw = h.get_name();
        // SF2 lists end in an "EOS" terminal record — skip it (and any empty name).
        if raw.trim().eq_ignore_ascii_case("eos") {
            continue;
        }
        let name = sanitize(raw);
        let rate = h.get_sample_rate().max(1) as u32;
        // Number the files so they sort in soundfont order and never collide on name.
        let fname = format!("{:03}_{name}.wav", i + 1);
        write_wav_i16(&dest.join(fname), pcm, 1, rate)?;
        written += 1;
    }
    if written == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "no samples in soundfont",
        ));
    }
    let _ = std::fs::write(&marker, b"");
    Ok(dest)
}

/// The deterministic cache directory for `sf2`: `<temp>/kaleidotron-soundfonts/<stem>-<hash>`,
/// the hash folding in size + mtime so an edited file re-extracts.
fn cache_dir(sf2: &Path) -> io::Result<PathBuf> {
    let meta = std::fs::metadata(sf2)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let key = format!("{}|{}|{}", sf2.display(), meta.len(), mtime);
    let stem = sf2.file_name().and_then(|s| s.to_str()).unwrap_or("sf2");
    let safe = sanitize(stem);
    Ok(std::env::temp_dir()
        .join("kaleidotron-soundfonts")
        .join(format!("{safe}-{:016x}", hash_str(&key))))
}

fn hash_str(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Keep alphanumerics / `-_.`, collapse the rest to `_`; fall back to "sample".
fn sanitize(s: &str) -> String {
    let out: String = s
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        "sample".to_string()
    } else {
        out
    }
}

/// Write mono/interleaved `i16` PCM to a 16-bit WAV file (no external crate).
fn write_wav_i16(path: &Path, samples: &[i16], channels: u16, sample_rate: u32) -> io::Result<()> {
    let bits = 16u16;
    let block_align = channels * bits / 8;
    let byte_rate = sample_rate * block_align as u32;
    let data_bytes = samples.len() * 2;
    let mut buf = Vec::with_capacity(44 + data_bytes);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&((36 + data_bytes) as u32).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
    buf.extend_from_slice(&channels.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    for &s in samples {
        buf.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::File::create(path)?.write_all(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_sf2_by_extension() {
        assert!(is_soundfont(Path::new("piano.sf2")));
        assert!(is_soundfont(Path::new("PIANO.SF2")));
        assert!(!is_soundfont(Path::new("song.it")));
        assert!(!is_soundfont(Path::new("noext")));
    }

    #[test]
    fn sanitize_makes_safe_stems() {
        assert_eq!(sanitize("Grand Piano!"), "Grand_Piano_");
        assert_eq!(sanitize("  "), "sample");
        assert_eq!(sanitize("kick-01.raw"), "kick-01.raw");
    }
}
