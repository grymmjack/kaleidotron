//! Archive browsing: treat `.zip`/`.lha`/`.arj`/… as **virtual folders** by
//! extracting them once into a per-archive temp cache directory, then letting the
//! rest of the app browse that directory like any other folder. This reuses the
//! whole existing pipeline (thumbnailer, decoder, selection) — only navigation
//! (enter the archive, remap breadcrumbs/up) needs to know archives exist.
//!
//! Extraction is read-only and best-effort; per-entry failures are skipped rather
//! than aborting the whole archive. Names are sanitized so a hostile archive can't
//! escape the cache directory (zip-slip).

use std::io;
use std::path::{Path, PathBuf};

/// Browsable (multi-entry) archive extensions, lower-case. Single-file
/// compressions (`.gz`/`.bz2`/`.Z`/`.ice`/`.pi9`/`.sq*`) are intentionally
/// excluded — they decompress to one file, not a folder.
const ARCHIVE_EXTS: &[&str] = &[
    "zip", "7z", "rar", "lha", "lzh", "tar", "ace", "arj", "arc", "zoo", "ha", "uc2", "sqz", "hyp",
    "tgz", "tbz", // Renoise song / instrument are ZIP containers (browse their SampleData/):
    "xrns", "xrni",
];

/// True if `path` looks like a browsable archive (by extension, case-insensitive).
pub fn is_archive(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ARCHIVE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Extract every entry of `archive` into a per-archive temp directory and return
/// that directory. Cached: keyed by the archive's path + size + mtime, so an
/// unchanged archive is extracted only once. A `.pv_extracted` marker signals a
/// complete prior extraction (so a half-written cache is redone).
pub fn extract_to_cache(archive: &Path) -> io::Result<PathBuf> {
    let dest = cache_dir(archive)?;
    let marker = dest.join(".pv_extracted");
    if marker.exists() {
        return Ok(dest);
    }
    // Stale/partial cache → start clean.
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(&dest)?;
    extract_all(archive, &dest)?;
    let _ = std::fs::write(&marker, b"");
    Ok(dest)
}

/// The deterministic cache directory for `archive`: `<temp>/kaleidotron-archives/
/// <stem>-<hash>` where the hash folds in size + mtime so edits invalidate it.
fn cache_dir(archive: &Path) -> io::Result<PathBuf> {
    let meta = std::fs::metadata(archive)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let key = format!("{}|{}|{}", archive.display(), meta.len(), mtime);
    let stem = archive
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("archive");
    let safe: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(std::env::temp_dir()
        .join("kaleidotron-archives")
        .join(format!("{safe}-{:016x}", hash_str(&key))))
}

fn hash_str(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Walk the archive sequentially, writing each file entry under `dest`.
fn extract_all(archive: &Path, dest: &Path) -> io::Result<()> {
    // Renoise `.xrns`/`.xrni` are ZIP containers, but unarc's format detection is by extension,
    // so open them explicitly as ZIP rather than letting `open_path` reject the unknown suffix.
    let ext = archive
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(ext.as_str(), "xrns" | "xrni") {
        return extract_zip(archive, dest);
    }
    use unarc_rs::unified::ArchiveFormat;
    let mut a = ArchiveFormat::open_path(archive).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("open {}: {e}", archive.display()),
        )
    })?;
    while let Some(entry) = a
        .next_entry()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("read entry: {e}")))?
    {
        let name = entry.name().to_string();
        let is_dir = name.ends_with('/') || name.ends_with('\\');
        let Some(out) = safe_join(dest, &name) else {
            continue; // unsafe (zip-slip) or empty → skip
        };
        if is_dir {
            let _ = std::fs::create_dir_all(&out);
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Best-effort per entry: a single bad member shouldn't kill the browse.
        match a.read(&entry) {
            Ok(data) => {
                let _ = std::fs::write(&out, data);
            }
            Err(_) => continue,
        }
    }
    Ok(())
}

/// Extract a file explicitly as a ZIP (via unarc's `ZipArchive`, bypassing extension-based
/// format detection) — for containers like Renoise `.xrns`/`.xrni` that are zips under a
/// different suffix. Same zip-slip guard + best-effort-per-entry policy as `extract_all`.
fn extract_zip(archive: &Path, dest: &Path) -> io::Result<()> {
    use unarc_rs::zip::zip_archive::ZipArchive;
    let file = std::fs::File::open(archive)?;
    let mut zip = ZipArchive::new(file)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("zip: {e}")))?;
    while let Some(header) = zip
        .get_next_entry()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("zip entry: {e}")))?
    {
        let Some(out) = safe_join(dest, &header.name) else {
            continue;
        };
        if header.is_directory {
            let _ = std::fs::create_dir_all(&out);
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Ok(data) = zip.read(&header) {
            let _ = std::fs::write(&out, data);
        }
    }
    Ok(())
}

/// Join a (possibly hostile) archive-internal `name` under `base`, rejecting any
/// `..` that would escape `base`. Backslash separators (DOS archives) are
/// normalized, leading slashes are stripped (absolute paths get reparented under
/// `base`, which is safe). Returns `None` for an empty/escaping path.
fn safe_join(base: &Path, name: &str) -> Option<PathBuf> {
    use std::path::Component;
    let normalized = name.replace('\\', "/");
    let rel = Path::new(normalized.trim_start_matches('/'));
    let mut out = base.to_path_buf();
    let mut depth = 0i32;
    for comp in rel.components() {
        match comp {
            Component::Normal(c) => {
                out.push(c);
                depth += 1;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return None; // would escape `base`
                }
                out.pop();
                depth -= 1;
            }
            // RootDir / Prefix can't appear after trimming on Unix; reject defensively.
            _ => return None,
        }
    }
    (out != base).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_archive_classifies_by_extension() {
        for ok in ["a.zip", "B.LHA", "pack.arj", "x.7z", "art.zoo", "f.tgz"] {
            assert!(is_archive(Path::new(ok)), "{ok} should be an archive");
        }
        for no in ["a.png", "b.ans", "c.gz", "d.ice", "noext", "e.txt"] {
            assert!(!is_archive(Path::new(no)), "{no} should NOT be an archive");
        }
    }

    #[test]
    fn safe_join_blocks_zip_slip() {
        let base = Path::new("/cache/x");
        assert_eq!(
            safe_join(base, "sub/art.ans"),
            Some(PathBuf::from("/cache/x/sub/art.ans"))
        );
        // DOS backslashes are normalized to nested dirs.
        assert_eq!(
            safe_join(base, "DIR\\PIC.PCX"),
            Some(PathBuf::from("/cache/x/DIR/PIC.PCX"))
        );
        // Leading slash is stripped → reparented safely under base.
        assert_eq!(
            safe_join(base, "/etc/passwd"),
            Some(PathBuf::from("/cache/x/etc/passwd"))
        );
        // A descent then escape that nets back inside is allowed.
        assert_eq!(
            safe_join(base, "a/../b.png"),
            Some(PathBuf::from("/cache/x/b.png"))
        );
        // Escapes are rejected.
        assert_eq!(safe_join(base, "../../etc/passwd"), None);
        assert_eq!(safe_join(base, "a/../../escape"), None);
        assert_eq!(safe_join(base, ""), None);
    }
}
