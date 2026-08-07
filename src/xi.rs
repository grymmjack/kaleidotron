//! Read-only XI (FastTracker II instrument, `.xi`) support: browse an instrument **as a folder**
//! of its samples. `xmrs` parses the XI (`XiInstrument::load` → a core `Instrument`); we pull each
//! sample's PCM out (like a tracker module's samples) and write it to a WAV in a temp dir, mounted
//! like an archive — so each sample opens in the audio player, auditions on the keyboard/MIDI,
//! rates, and exports.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Recognize a FastTracker II instrument by extension (`.xi`).
pub fn is_xi(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("xi"))
}

/// Instrument name + sample count for the Details pane.
#[derive(Clone)]
pub struct XiInfo {
    pub name: String,
    pub samples: usize,
}

/// Parse `xi` via `xmrs` into a core instrument. `None` if it isn't a readable Extended Instrument.
fn load(xi_bytes: &[u8]) -> Option<xmrs::core::instrument::Instrument> {
    use xmrs::tracker::import::xm::xi_instrument::XiInstrument;
    Some(XiInstrument::load(xi_bytes).ok()?.to_instrument())
}

/// The instrument's samples (the non-empty ones), as `(name, mono f32, native rate)`.
fn samples(instr: &xmrs::core::instrument::Instrument) -> Vec<(String, Vec<f32>, u32)> {
    use xmrs::core::instrument::InstrumentType;
    let InstrumentType::Default(def) = &instr.instr_type else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (si, opt) in def.sample.iter().enumerate() {
        let Some(smp) = opt else { continue };
        if smp.is_empty() {
            continue;
        }
        let pcm = sample_pcm_mono(smp);
        if pcm.is_empty() {
            continue;
        }
        // Tracker/XI pitch is relative to C-4, whose canonical rate is 8363 Hz.
        let base = 8363.0 * 2f32.powf(smp.relative_pitch as f32 / 12.0);
        let rate = (base.round() as u32).clamp(1000, 192_000);
        let name = if smp.name.trim().is_empty() {
            format!("sample {}", si + 1)
        } else {
            smp.name.trim().to_string()
        };
        out.push((name, pcm, rate));
    }
    out
}

/// Convert one xmrs `Sample`'s PCM to mono `f32` in `[-1, 1]` (stereo averaged).
fn sample_pcm_mono(smp: &xmrs::core::sample::Sample) -> Vec<f32> {
    use xmrs::core::sample::SampleDataType as D;
    match &smp.data {
        Some(D::Mono8(v)) => v.iter().map(|&x| x as f32 / 128.0).collect(),
        Some(D::Mono16(v)) => v.iter().map(|&x| x as f32 / 32768.0).collect(),
        Some(D::Stereo8(v)) => v
            .chunks_exact(2)
            .map(|p| (p[0] as f32 + p[1] as f32) / 256.0)
            .collect(),
        Some(D::Stereo16(v)) => v
            .chunks_exact(2)
            .map(|p| (p[0] as f32 + p[1] as f32) / 65536.0)
            .collect(),
        Some(D::StereoFloat(v)) => v.chunks_exact(2).map(|p| (p[0] + p[1]) * 0.5).collect(),
        None => Vec::new(),
    }
}

/// Report the instrument name + sample count for Details. `None` if it isn't a readable XI.
pub fn info(xi: &Path) -> Option<XiInfo> {
    let bytes = std::fs::read(xi).ok()?;
    let instr = load(&bytes)?;
    let n = samples(&instr).len();
    Some(XiInfo {
        name: instr.name.trim().to_string(),
        samples: n,
    })
}

/// Extract every sample to a 16-bit WAV in a per-file temp dir (cached), and return that dir —
/// mounted like an archive.
pub fn extract_to_cache(xi: &Path) -> io::Result<PathBuf> {
    let dest = cache_dir(xi)?;
    let marker = dest.join(".pv_extracted");
    if marker.exists() {
        return Ok(dest);
    }
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(&dest)?;
    let bytes = std::fs::read(xi)?;
    let instr =
        load(&bytes).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not an XI"))?;
    let mut written = 0usize;
    for (i, (name, pcm, rate)) in samples(&instr).into_iter().enumerate() {
        let fname = format!("{:03}_{}.wav", i + 1, sanitize(&name));
        write_wav_f32(&dest.join(fname), &pcm, rate)?;
        written += 1;
    }
    if written == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "no samples in this XI instrument",
        ));
    }
    let _ = std::fs::write(&marker, b"");
    Ok(dest)
}

/// Write mono `f32` samples to a 16-bit PCM WAV file.
fn write_wav_f32(path: &Path, samples: &[f32], sample_rate: u32) -> io::Result<()> {
    let bits = 16u16;
    let block_align = bits / 8; // mono
    let byte_rate = sample_rate * block_align as u32;
    let data_bytes = samples.len() * 2;
    let mut buf = Vec::with_capacity(44 + data_bytes);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&((36 + data_bytes) as u32).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
    buf.extend_from_slice(&1u16.to_le_bytes()); // channels
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    for &s in samples {
        buf.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    std::fs::File::create(path)?.write_all(&buf)
}

fn cache_dir(xi: &Path) -> io::Result<PathBuf> {
    let meta = std::fs::metadata(xi)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let key = format!("{}|{}|{}", xi.display(), meta.len(), mtime);
    let stem = xi.file_name().and_then(|s| s.to_str()).unwrap_or("xi");
    Ok(std::env::temp_dir().join("kaleidotron-xi").join(format!(
        "{}-{:016x}",
        sanitize(stem),
        hash_str(&key)
    )))
}

fn hash_str(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_xi() {
        assert!(is_xi(Path::new("piano.xi")));
        assert!(is_xi(Path::new("PIANO.XI")));
        assert!(!is_xi(Path::new("piano.sf2")));
    }
}
