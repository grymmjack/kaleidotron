//! Read-only DLS (Downloadable Sounds, `.dls`) support: browse a DLS bank **as a folder** of
//! its samples. DLS is an open MMA RIFF format — its wave pool (`LIST:wvpl`) holds a list of
//! `LIST:wave` chunks, each of which is essentially an embedded WAV (a `fmt ` + `data` pair).
//! So we walk the pool and reconstruct each entry into a standalone `.wav` in a temp dir,
//! mounted like an archive — each sample then opens in the audio player / rates / exports.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Recognize a DLS bank by extension (`.dls`).
pub fn is_dls(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("dls"))
}

/// Instrument / sample counts for the Details pane.
#[derive(Clone)]
pub struct DlsInfo {
    pub instruments: usize,
    pub samples: usize,
}

/// Report a DLS bank's instrument + wave-pool sample counts. `None` if it isn't a readable DLS.
pub fn info(dls: &Path) -> Option<DlsInfo> {
    let bytes = std::fs::read(dls).ok()?;
    let form = riff_form(&bytes)?;
    if &form.0 != b"DLS " {
        return None;
    }
    let mut instruments = 0usize;
    let mut samples = 0usize;
    for (id, body) in chunks(form.1) {
        match &id {
            b"colh" if body.len() >= 4 => {
                instruments = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
            }
            b"LIST" if body.len() >= 4 && &body[0..4] == b"wvpl" => {
                samples = list_items(&body[4..], b"wave").count();
            }
            _ => {}
        }
    }
    Some(DlsInfo {
        instruments,
        samples,
    })
}

/// Extract every wave-pool sample to a standalone WAV in a per-file temp dir (cached), and
/// return that dir — mounted like an archive.
pub fn extract_to_cache(dls: &Path) -> io::Result<PathBuf> {
    let dest = cache_dir(dls)?;
    let marker = dest.join(".pv_extracted");
    if marker.exists() {
        return Ok(dest);
    }
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(&dest)?;
    let bytes = std::fs::read(dls)?;
    let form = riff_form(&bytes)
        .filter(|f| &f.0 == b"DLS ")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not a DLS file"))?;
    let mut written = 0usize;
    for (id, body) in chunks(form.1) {
        if &id == b"LIST" && body.len() >= 4 && &body[0..4] == b"wvpl" {
            for (i, wave) in list_items(&body[4..], b"wave").enumerate() {
                // Each `wave` LIST holds a `fmt ` + `data` (+ optional INFO/INAM name).
                let mut fmt: Option<&[u8]> = None;
                let mut data: Option<&[u8]> = None;
                let mut name: Option<String> = None;
                for (sid, sbody) in chunks(wave) {
                    match &sid {
                        b"fmt " => fmt = Some(sbody),
                        b"data" => data = Some(sbody),
                        b"LIST" if sbody.len() >= 4 && &sbody[0..4] == b"INFO" => {
                            for (iid, ibody) in chunks(&sbody[4..]) {
                                if &iid == b"INAM" {
                                    name = Some(cstr(ibody));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                let (Some(fmt), Some(data)) = (fmt, data) else {
                    continue;
                };
                let stem = name
                    .as_deref()
                    .map(sanitize)
                    .filter(|s| s != "sample")
                    .unwrap_or_else(|| format!("wave{}", i + 1));
                let fname = format!("{:03}_{stem}.wav", i + 1);
                write_wav_from_chunks(&dest.join(fname), fmt, data)?;
                written += 1;
            }
        }
    }
    if written == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "no samples in DLS wave pool",
        ));
    }
    let _ = std::fs::write(&marker, b"");
    Ok(dest)
}

/// Wrap a DLS wave's raw `fmt `+`data` chunks back into a standalone RIFF/WAVE file — since a
/// DLS wave is a WAV minus its outer wrapper, this preserves the exact PCM format bit-for-bit.
fn write_wav_from_chunks(path: &Path, fmt: &[u8], data: &[u8]) -> io::Result<()> {
    let pad = |n: usize| n + (n & 1); // RIFF chunks are word-aligned
    let riff_len = 4 + (8 + pad(fmt.len())) + (8 + pad(data.len()));
    let mut buf = Vec::with_capacity(8 + riff_len);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(riff_len as u32).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    for (id, body) in [(b"fmt ", fmt), (b"data", data)] {
        buf.extend_from_slice(id);
        buf.extend_from_slice(&(body.len() as u32).to_le_bytes());
        buf.extend_from_slice(body);
        if body.len() & 1 == 1 {
            buf.push(0); // pad byte
        }
    }
    std::fs::File::create(path)?.write_all(&buf)
}

/// If `buf` starts with a `RIFF` chunk, return its form type + the chunk body (after the form).
fn riff_form(buf: &[u8]) -> Option<([u8; 4], &[u8])> {
    if buf.len() < 12 || &buf[0..4] != b"RIFF" {
        return None;
    }
    let size = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
    let end = (8 + size).min(buf.len());
    let form = [buf[8], buf[9], buf[10], buf[11]];
    Some((form, &buf[12..end]))
}

/// Iterate the `(id, body)` chunks in a RIFF chunk-list body (handles word-alignment padding).
fn chunks(mut buf: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    while buf.len() >= 8 {
        let id = [buf[0], buf[1], buf[2], buf[3]];
        let size = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
        let body_end = (8 + size).min(buf.len());
        out.push((id, &buf[8..body_end]));
        let advance = 8 + size + (size & 1); // pad to even
        if advance == 0 || advance > buf.len() {
            break;
        }
        buf = &buf[advance..];
    }
    out
}

/// Iterate the bodies of `LIST:<form>` chunks matching `want_form` inside a chunk-list body.
fn list_items<'a>(buf: &'a [u8], want_form: &'static [u8; 4]) -> impl Iterator<Item = &'a [u8]> {
    chunks(buf).into_iter().filter_map(move |(id, body)| {
        if &id == b"LIST" && body.len() >= 4 && &body[0..4] == want_form.as_slice() {
            Some(&body[4..])
        } else {
            None
        }
    })
}

/// A NUL-terminated (or whole) RIFF string.
fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).trim().to_string()
}

fn cache_dir(dls: &Path) -> io::Result<PathBuf> {
    let meta = std::fs::metadata(dls)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let key = format!("{}|{}|{}", dls.display(), meta.len(), mtime);
    let stem = dls.file_name().and_then(|s| s.to_str()).unwrap_or("dls");
    Ok(std::env::temp_dir().join("kaleidotron-dls").join(format!(
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
    fn recognizes_dls() {
        assert!(is_dls(Path::new("bank.dls")));
        assert!(is_dls(Path::new("BANK.DLS")));
        assert!(!is_dls(Path::new("bank.sf2")));
    }

    #[test]
    fn walks_riff_chunks() {
        // RIFF("DLS ") { colh(=2 instruments), LIST("wvpl"){ LIST("wave"){...}, LIST("wave"){...} } }
        let wave = |n: u8| {
            let mut w = Vec::new();
            w.extend_from_slice(b"wave");
            // one dummy 2-byte fmt chunk so the wave is non-empty
            w.extend_from_slice(b"fmt ");
            w.extend_from_slice(&2u32.to_le_bytes());
            w.extend_from_slice(&[n, 0]);
            w
        };
        let mut wvpl = Vec::new();
        wvpl.extend_from_slice(b"wvpl");
        for n in 0..2u8 {
            let w = wave(n);
            wvpl.extend_from_slice(b"LIST");
            wvpl.extend_from_slice(&(w.len() as u32).to_le_bytes());
            wvpl.extend_from_slice(&w);
        }
        let mut body = Vec::new();
        body.extend_from_slice(b"colh");
        body.extend_from_slice(&4u32.to_le_bytes());
        body.extend_from_slice(&2u32.to_le_bytes()); // 2 instruments
        body.extend_from_slice(b"LIST");
        body.extend_from_slice(&(wvpl.len() as u32).to_le_bytes());
        body.extend_from_slice(&wvpl);
        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&((4 + body.len()) as u32).to_le_bytes());
        file.extend_from_slice(b"DLS ");
        file.extend_from_slice(&body);

        let form = riff_form(&file).unwrap();
        assert_eq!(&form.0, b"DLS ");
        let mut colh = 0;
        let mut waves = 0;
        for (id, b) in chunks(form.1) {
            if &id == b"colh" {
                colh = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            }
            if &id == b"LIST" && &b[0..4] == b"wvpl" {
                waves = list_items(&b[4..], b"wave").count();
            }
        }
        assert_eq!(colh, 2);
        assert_eq!(waves, 2);
    }
}
