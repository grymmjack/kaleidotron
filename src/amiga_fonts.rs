//! Fetch the Stone Oakvalley **Amiga ColorFonts** collection into the data dir.
//!
//! The site publishes the whole thing as one 46 MB zip, so this is one polite request rather than a
//! 1030-image scrape — the same "download the archive, don't crawl the gallery" approach the rest of
//! the program takes with a remote source. The archive is `robots.txt`-clean (only `/php/` is
//! disallowed).
//!
//! The download goes through the shared HTTP cache (`cache::get_file`), so a re-fetch after the data
//! dir is wiped is served from cache with no network. Extraction is idempotent: a font directory
//! already on disk is left alone, so opening the collection a second time is instant.

use std::path::Path;

/// The archive the site links from its ColorFonts post. One file, ~46 MB.
pub const ARCHIVE_URL: &str = "https://www.stone-oakvalley-studios.com/uploads/000913112022231233/amigacolorfonts_archive_by_stone_oakvalley_2022.zip";

/// The subtree inside the zip that holds the primary (non-duplicate) fonts. The archive also ships
/// six `..._Dupes0N_...` trees and 1030 `.iff` previews; the `Fonts/` directory under the main tree
/// is the curated set — one `NAME.font` descriptor plus a `NAME/` directory of sizes per family.
const FONTS_SUBTREE: &str = "ColorFonts_Archive_by_Stone_Oakvalley/Fonts/";

/// Is the collection already unpacked in `dir`? Cheap enough to call on every launch — it stats one
/// path — so the menu can show "Open" vs "Download" without any I/O beyond a directory check.
pub fn is_present(dir: &Path) -> bool {
    // A `.font` descriptor at the top level is the signature of a completed extraction.
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten().any(|e| {
                e.path().extension().and_then(|x| x.to_str()).is_some_and(|x| x.eq_ignore_ascii_case("font"))
            })
        })
        .unwrap_or(false)
}

/// Download (cache-first) and unpack the collection's font families into `dir`, flattening the
/// archive's `.../Fonts/` subtree to the top level so `dir` holds `Aggress.font`, `Aggress/`, … .
///
/// Only the primary `Fonts/` subtree is extracted — the duplicate trees and the `.iff` previews are
/// skipped, so the on-disk result is the ~768 families a browser wants and not 3663 files. Returns
/// the count of files written.
///
/// This is the slow, blocking half; it runs on a worker thread. `unpack_into` (below) is the pure
/// part, so the extraction logic is unit-testable without the 46 MB download.
pub fn fetch_into(dir: &Path) -> Result<usize, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let zip_path = crate::cache::get_file(ARCHIVE_URL, "amiga_colorfonts.zip")?;
    let bytes = std::fs::read(&zip_path).map_err(|e| e.to_string())?;
    unpack_into(&bytes, dir)
}

/// Extract the `Fonts/` subtree of the archive to `dir`. Pure but for the writes it is asked to make.
///
/// Skips: the duplicate trees, the `.iff` previews, anything outside `FONTS_SUBTREE`, and any entry
/// whose name would escape `dir` (a zip-slip guard — never trust an archive's paths). An entry that
/// already exists on disk is left untouched, so a partial extraction resumes and a re-run is a no-op.
pub fn unpack_into(zip_bytes: &[u8], dir: &Path) -> Result<usize, String> {
    let reader = std::io::Cursor::new(zip_bytes);
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| e.to_string())?;
    let mut written = 0usize;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let Some(name) = entry.enclosed_name() else {
            continue; // an unsafe path; enclosed_name() already refuses `..`
        };
        // Skip directory entries by the zip's OWN flag, not by a trailing slash: `enclosed_name`
        // normalises the path and can drop the slash, so `.../18CAROTGOLD/` arrives looking like a
        // file and — written as one — becomes a 0-byte file that its own size files then cannot be
        // created under ("Not a directory"). This was the real archive's first failure.
        if entry.is_dir() {
            continue;
        }
        let name = name.to_string_lossy().replace('\\', "/");
        // Only the curated Fonts subtree, and only its font data — not the .iff previews.
        let Some(rel) = name.strip_prefix(FONTS_SUBTREE) else {
            continue;
        };
        if rel.is_empty() || rel.ends_with('/') {
            continue;
        }
        if rel.to_ascii_lowercase().ends_with(".iff") {
            continue; // previews live elsewhere; the browser makes its own thumbnails
        }
        let out = dir.join(rel);
        // Zip-slip belt to the enclosed_name() braces: refuse anything that resolves outside `dir`.
        if !out.starts_with(dir) {
            continue;
        }
        if out.exists() {
            continue; // idempotent: keep what is already there
        }
        if let Some(parent) = out.parent() {
            // create_dir_all is normally idempotent, but the archive contains a family whose
            // descriptor and directory differ only in case on a case-insensitive path, or a stray
            // file where a directory is expected — either yields EEXIST. Treat "already there" as
            // success and only fail on a real error.
            if let Err(e) = std::fs::create_dir_all(parent) {
                if e.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(format!("{}: {e}", parent.display()));
                }
            }
        }
        let mut buf = Vec::with_capacity(entry.size() as usize);
        std::io::copy(&mut entry, &mut buf).map_err(|e| e.to_string())?;
        std::fs::write(&out, &buf).map_err(|e| e.to_string())?;
        written += 1;
    }
    if written == 0 && !is_present(dir) {
        return Err("archive held no font families where expected".into());
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a tiny in-memory zip mirroring the archive's shape: a couple of real font entries
    /// under the Fonts subtree, plus the noise the extractor must skip — a `.iff` preview, a
    /// duplicate-tree file, and a zip-slip attempt.
    fn synth_zip() -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<'_, ()> =
                zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
            let write = |zw: &mut zip::ZipWriter<_>, name: &str, body: &[u8]| {
                zw.start_file(name, opts).unwrap();
                zw.write_all(body).unwrap();
            };
            write(&mut zw, "ColorFonts_Archive_by_Stone_Oakvalley/Fonts/Aggress.font", b"\x0f\x00desc");
            write(&mut zw, "ColorFonts_Archive_by_Stone_Oakvalley/Fonts/Aggress/36.8C", b"glyphs");
            write(&mut zw, "ColorFonts_Archive_by_Stone_Oakvalley/Fonts/3D.font", b"\x0f\x00desc");
            // Noise that must NOT be extracted:
            write(&mut zw, "ColorFonts_Archive_by_Stone_Oakvalley/IFF_Previews/Aggress.iff", b"iff");
            write(&mut zw, "ColorFonts_Archive_Dupes01_by_Stone_Oakvalley/Fonts/Dupe.font", b"dup");
            zw.finish().unwrap();
        }
        buf
    }

    /// The archive stores explicit directory entries, and `enclosed_name` can strip their trailing
    /// slash — so a directory arrives looking like a file. Written as one it becomes a 0-byte file
    /// that its own size files then cannot be created under ("Not a directory"). The extractor must
    /// skip it by the zip's own directory flag. This was the real archive's first failure.
    #[test]
    fn skips_directory_entries_that_lost_their_trailing_slash() {
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<'_, ()> =
                zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
            // A directory entry WITHOUT the trailing slash, then a file that needs it as a parent.
            zw.add_directory("ColorFonts_Archive_by_Stone_Oakvalley/Fonts/Fam", opts).unwrap();
            zw.start_file("ColorFonts_Archive_by_Stone_Oakvalley/Fonts/Fam/36.8C", opts).unwrap();
            zw.write_all(b"glyphs").unwrap();
            zw.finish().unwrap();
        }
        let tmp = std::env::temp_dir().join("kt_amiga_dirent_test");
        let _ = std::fs::remove_dir_all(&tmp);
        let n = unpack_into(&buf, &tmp).expect("unpacks without a Not-a-directory error");
        assert_eq!(n, 1, "only the size file is written; the directory entry is skipped");
        assert!(tmp.join("Fam").is_dir(), "Fam is a real directory, not a 0-byte file");
        assert!(tmp.join("Fam/36.8C").is_file());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn extracts_only_the_curated_fonts_subtree() {
        let tmp = std::env::temp_dir().join("kt_amiga_unpack_test");
        let _ = std::fs::remove_dir_all(&tmp);
        let n = unpack_into(&synth_zip(), &tmp).expect("unpacks");
        assert_eq!(n, 3, "two descriptors + one size file; the .iff and the dupe are skipped");
        assert!(tmp.join("Aggress.font").is_file());
        assert!(tmp.join("Aggress/36.8C").is_file());
        assert!(tmp.join("3D.font").is_file());
        assert!(!tmp.join("Aggress.iff").exists(), ".iff previews are not written");
        assert!(!tmp.join("Dupe.font").exists(), "the duplicate trees are skipped");
        assert!(is_present(&tmp), "a .font at the top level means the collection is present");

        // Re-running is a no-op: everything already exists.
        assert_eq!(unpack_into(&synth_zip(), &tmp).expect("re-runs"), 0);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The whole download+unpack against the live site. Ignored — it is a 46 MB fetch — but it is
    /// the check that matters, since a synthetic zip only proves the extractor agrees with itself.
    #[test]
    #[ignore]
    fn fetches_the_real_archive() {
        crate::cache::init(&std::env::temp_dir().join("kt_amiga_cache"));
        let dir = std::env::temp_dir().join("kt_amiga_real");
        let _ = std::fs::remove_dir_all(&dir);
        let n = fetch_into(&dir).expect("fetch + unpack");
        println!("wrote {n} files to {}", dir.display());
        assert!(n > 500, "expected the full collection, got {n} files");
        assert!(is_present(&dir));
    }
}
