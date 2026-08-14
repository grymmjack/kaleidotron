//! The bundled **Amiga ColorFonts** collection — embedded in the binary, seeded on first use.
//!
//! Same pattern as the TheDraw font library (`bundle_zip_path`): a zip is `include_bytes!`d into the
//! app, written to `<data>/bundled/` on first run, and browsed as an archive. No download, no
//! network — the fonts are part of the program.
//!
//! The bundle is **flat**: one file per font family, all in a single directory. An Amiga size file
//! (`36.8C`) is self-contained — it carries the name, palette and every glyph — so it decodes with
//! no `.font` descriptor, which means the whole collection can live as siblings in one folder. That
//! is what makes Left/Right step between fonts in the viewer, and it is what the user asked for over
//! the original per-family-directory layout. Each file is named `<FontName>.<colours>c` (e.g.
//! `Aggress.8c`), so the grid caption is the font's name and the extension routes it to the Amiga
//! decoder.

use std::path::{Path, PathBuf};

/// The flat collection: 564 font families, one size file each (the largest), ~12.5 MB zipped.
/// Sourced from the Stone Oakvalley public archive.
const BUNDLE: &[u8] = include_bytes!("../assets/amiga/amiga_colorfonts.zip");

/// The name the seeded zip is written under. Shows up in the breadcrumb as the mount root, so it is
/// a human label, not a slug.
pub const BUNDLE_NAME: &str = "Amiga ColorFonts.zip";

/// Write the embedded bundle to `<data>/bundled/` if it is missing or a different size, and return
/// its path. Mirrors `Kaleidotron::bundle_zip_path` exactly — the seed is idempotent (size check),
/// so it costs one `stat` on every call after the first run.
pub fn bundle_zip_path(data_dir: &Path) -> Option<PathBuf> {
    let dir = data_dir.join("bundled");
    let path = dir.join(BUNDLE_NAME);
    let need = std::fs::metadata(&path).map(|m| m.len() != BUNDLE.len() as u64).unwrap_or(true);
    if need {
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::write(&path, BUNDLE).ok()?;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The embedded bundle is a real zip of real Amiga fonts — a size check, plus a spot-decode of
    /// the first font entry so a corrupt or wrong asset is caught at build time rather than in the
    /// field.
    #[test]
    fn the_bundle_is_a_zip_of_decodable_fonts() {
        assert!(BUNDLE.len() > 1_000_000, "the bundle should be the full ~12 MB collection");
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(BUNDLE)).expect("a valid zip");
        assert!(zip.len() > 100, "expected hundreds of fonts, got {}", zip.len());

        let mut decoded = 0;
        for i in 0..zip.len().min(20) {
            let mut e = zip.by_index(i).expect("entry");
            if e.is_dir() {
                continue;
            }
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut e, &mut bytes).expect("read");
            if crate::decode::amiga_font::parse(&bytes).is_ok() {
                decoded += 1;
            }
        }
        assert!(decoded > 0, "no bundled entry decoded as an Amiga font");
    }

    /// Seeding writes the zip once and is idempotent on a second call.
    #[test]
    fn seeds_the_bundle_into_the_data_dir() {
        let tmp = std::env::temp_dir().join("kt_amiga_seed_test");
        let _ = std::fs::remove_dir_all(&tmp);
        let p = bundle_zip_path(&tmp).expect("seeds");
        assert!(p.is_file());
        assert_eq!(std::fs::metadata(&p).unwrap().len(), BUNDLE.len() as u64);
        assert_eq!(bundle_zip_path(&tmp).as_deref(), Some(p.as_path()));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
