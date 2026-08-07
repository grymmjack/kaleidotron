//! Read-only SFZ (`.sfz`) support: browse an SFZ instrument **as a folder** of the samples it
//! maps. Unlike SF2/DLS (which embed their PCM), an `.sfz` is a *text* file that references
//! external audio files (`sample=…`, resolved against an optional `default_path` and the SFZ's
//! own directory). Those samples are already real files on disk, so we mount a temp dir of
//! **symlinks** to them (a copy fallback where symlinks aren't available) — no data duplication —
//! and each opens in the audio player / rates / exports like any file. See <https://sfzformat.com>.

use std::io;
use std::path::{Path, PathBuf};

/// Recognize an SFZ instrument by extension (`.sfz`).
pub fn is_sfz(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("sfz"))
}

/// Region / sample counts + key range for the Details pane.
#[derive(Clone)]
pub struct SfzInfo {
    pub regions: usize,
    pub samples: usize, // unique, existing-on-disk
    pub key_lo: Option<u8>,
    pub key_hi: Option<u8>,
}

/// Parse an `.sfz` and report what it maps (regions, unique samples, key range). `None` if the
/// file can't be read.
pub fn info(sfz: &Path) -> Option<SfzInfo> {
    let text = std::fs::read_to_string(sfz).ok()?;
    let ops = parse_opcodes(&text);
    let regions = ops
        .iter()
        .filter(|(k, v)| k == "<header>" && v == "region")
        .count();
    let base = sfz.parent().unwrap_or(Path::new("."));
    let samples = resolve_samples(&ops, base).len();
    let mut lo: Option<u8> = None;
    let mut hi: Option<u8> = None;
    for (k, v) in &ops {
        // `key` sets both lokey and hikey; `lokey`/`hikey` set one bound.
        let note = |v: &str| parse_note(v);
        match k.as_str() {
            "lokey" | "key" => {
                if let Some(n) = note(v) {
                    lo = Some(lo.map_or(n, |x| x.min(n)));
                    if k == "key" {
                        hi = Some(hi.map_or(n, |x| x.max(n)));
                    }
                }
            }
            "hikey" => {
                if let Some(n) = note(v) {
                    hi = Some(hi.map_or(n, |x| x.max(n)));
                }
            }
            _ => {}
        }
    }
    Some(SfzInfo {
        regions,
        samples,
        key_lo: lo,
        key_hi: hi,
    })
}

/// Extract (mount) an SFZ's samples into a per-file temp dir of symlinks and return that dir
/// (mounted like an archive). Cached by the SFZ's path+size+mtime.
pub fn mount_to_cache(sfz: &Path) -> io::Result<PathBuf> {
    let dest = cache_dir(sfz)?;
    let marker = dest.join(".pv_extracted");
    if marker.exists() {
        return Ok(dest);
    }
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(&dest)?;
    let text = std::fs::read_to_string(sfz)?;
    let ops = parse_opcodes(&text);
    let base = sfz.parent().unwrap_or(Path::new("."));
    let samples = resolve_samples(&ops, base);
    let mut linked = 0usize;
    for (i, src) in samples.iter().enumerate() {
        let stem = src
            .file_name()
            .and_then(|s| s.to_str())
            .map(sanitize)
            .unwrap_or_else(|| format!("sample{i}"));
        // Number so it sorts in map order and never collides on a shared basename.
        let dst = dest.join(format!("{:03}_{stem}", i + 1));
        if link_or_copy(src, &dst).is_ok() {
            linked += 1;
        }
    }
    if linked == 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no reachable samples referenced by this .sfz",
        ));
    }
    let _ = std::fs::write(&marker, b"");
    Ok(dest)
}

/// Resolve every `sample=` opcode to a unique, existing absolute path, honoring the most recent
/// `default_path` (an `<control>` opcode). Backslashes are normalized to `/`.
fn resolve_samples(ops: &[(String, String)], base: &Path) -> Vec<PathBuf> {
    let mut default_path = String::new();
    let mut out: Vec<PathBuf> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (k, v) in ops {
        match k.as_str() {
            "default_path" => default_path = v.replace('\\', "/"),
            "sample" => {
                let rel = v.replace('\\', "/");
                let mut p = base.to_path_buf();
                if !default_path.is_empty() {
                    p.push(&default_path);
                }
                p.push(&rel);
                // Cheap normalization of `..`/`.` without touching the filesystem.
                let norm = normalize(&p);
                if norm.is_file() && seen.insert(norm.clone()) {
                    out.push(norm);
                }
            }
            _ => {}
        }
    }
    out
}

/// Tokenize SFZ text into `(opcode, value)` pairs plus `("<header>", name)` markers. Values may
/// contain spaces (e.g. a sample path), so a value runs until the next `opcode=` or `<header>`.
fn parse_opcodes(text: &str) -> Vec<(String, String)> {
    let clean = strip_comments(text);
    let mut out: Vec<(String, String)> = Vec::new();
    let mut cur: Option<(String, String)> = None;
    for tok in clean.split_whitespace() {
        if tok.starts_with('<') && tok.ends_with('>') {
            if let Some(kv) = cur.take() {
                out.push(kv);
            }
            out.push((
                "<header>".to_string(),
                tok.trim_matches(['<', '>']).to_string(),
            ));
            continue;
        }
        // A token that starts a new opcode looks like `name=` with an identifier before `=`.
        if let Some(eq) = tok.find('=') {
            let key = &tok[..eq];
            if !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                if let Some(kv) = cur.take() {
                    out.push(kv);
                }
                cur = Some((key.to_ascii_lowercase(), tok[eq + 1..].to_string()));
                continue;
            }
        }
        // Otherwise it's a continuation of the current value (a space inside a path).
        if let Some((_, val)) = &mut cur {
            val.push(' ');
            val.push_str(tok);
        }
    }
    if let Some(kv) = cur.take() {
        out.push(kv);
    }
    out
}

/// Remove `//` line comments and `/* … */` block comments.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else {
            out.push(b[i] as char);
            i += 1;
        }
    }
    out
}

/// A note value is either a MIDI number (`0..127`) or a note name like `c4`, `f#3`, `Db5`.
fn parse_note(v: &str) -> Option<u8> {
    if let Ok(n) = v.trim().parse::<i32>() {
        return u8::try_from(n.clamp(0, 127)).ok();
    }
    let s = v.trim().to_ascii_lowercase();
    let bytes = s.as_bytes();
    let (base, mut idx) = match bytes.first()? {
        b'c' => (0i32, 1),
        b'd' => (2, 1),
        b'e' => (4, 1),
        b'f' => (5, 1),
        b'g' => (7, 1),
        b'a' => (9, 1),
        b'b' => (11, 1),
        _ => return None,
    };
    let mut semis = base;
    if idx < bytes.len() && (bytes[idx] == b'#' || bytes[idx] == b's') {
        semis += 1;
        idx += 1;
    } else if idx < bytes.len() && bytes[idx] == b'b' {
        semis -= 1;
        idx += 1;
    }
    let oct: i32 = s.get(idx..)?.parse().ok()?;
    // sfz convention: c4 = MIDI 60 → (oct+1)*12 + semis
    let midi = (oct + 1) * 12 + semis;
    u8::try_from(midi.clamp(0, 127)).ok()
}

/// Collapse `.`/`..` components without hitting the filesystem (symlinks aside).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

#[cfg(unix)]
fn link_or_copy(src: &Path, dst: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(src, dst).or_else(|_| std::fs::copy(src, dst).map(|_| ()))
}

#[cfg(not(unix))]
fn link_or_copy(src: &Path, dst: &Path) -> io::Result<()> {
    std::fs::copy(src, dst).map(|_| ())
}

fn cache_dir(sfz: &Path) -> io::Result<PathBuf> {
    let meta = std::fs::metadata(sfz)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let key = format!("{}|{}|{}", sfz.display(), meta.len(), mtime);
    let stem = sfz.file_name().and_then(|s| s.to_str()).unwrap_or("sfz");
    Ok(std::env::temp_dir().join("kaleidotron-sfz").join(format!(
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
    fn recognizes_sfz() {
        assert!(is_sfz(Path::new("piano.sfz")));
        assert!(is_sfz(Path::new("PIANO.SFZ")));
        assert!(!is_sfz(Path::new("piano.sf2")));
    }

    #[test]
    fn parses_opcodes_with_spaces_in_paths() {
        let sfz = "// a comment\n<region> sample=Grand Piano/C4 note.wav lokey=60 hikey=64\n\
                   <region> sample=kick.wav key=c4";
        let ops = parse_opcodes(sfz);
        let samples: Vec<&String> = ops
            .iter()
            .filter(|(k, _)| k == "sample")
            .map(|(_, v)| v)
            .collect();
        assert_eq!(samples, vec!["Grand Piano/C4 note.wav", "kick.wav"]);
        assert_eq!(
            ops.iter()
                .filter(|(k, v)| k == "<header>" && v == "region")
                .count(),
            2
        );
    }

    #[test]
    fn parses_note_names_and_numbers() {
        assert_eq!(parse_note("60"), Some(60));
        assert_eq!(parse_note("c4"), Some(60));
        assert_eq!(parse_note("C#4"), Some(61));
        assert_eq!(parse_note("db4"), Some(61));
        assert_eq!(parse_note("a4"), Some(69));
    }

    #[test]
    fn strips_block_and_line_comments() {
        let s = strip_comments("a /* b\nc */ d // e\nf");
        assert!(s.contains('a') && s.contains('d') && s.contains('f'));
        assert!(!s.contains('b') && !s.contains('e'));
    }
}
