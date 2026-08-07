//! Cross-platform star ratings sidecar.
//!
//! [`crate::rating`] stores stars in the `user.baloo.rating` xattr so they
//! interoperate with Gwenview/Baloo — but that only works for a real on-disk file
//! on Linux. Art that lives in a *virtual* folder has no file to attach an xattr to:
//! an archive's contents are extracted to a disposable temp dir, and a 16colo.rs
//! piece is downloaded on demand. For those, this store maps a **stable display
//! path** (e.g. `/home/u/pack.zip/SUB/ART.ANS` or `<16colo.rs>/2023/pack/ART.ANS`)
//! → stars in one JSON file in the app data dir, so the ratings survive across
//! sessions and platforms.
//!
//! On-disk files keep using xattr as their source of truth (so external Gwenview
//! edits show up), but their ratings are *also* mirrored here — making this file a
//! complete, portable record and the fallback on platforms without xattr support.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// An in-memory ratings map backed by a JSON file. `dirty` defers disk writes.
pub struct RatingStore {
    path: PathBuf,
    map: HashMap<String, u8>, // display-path string → stars (1..=5; 0 = absent)
    dirty: bool,
}

impl RatingStore {
    /// Load `ratings.json` from `dir` (missing/corrupt → an empty store).
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("ratings.json");
        let map = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<HashMap<String, u8>>(&s).ok())
            .unwrap_or_default();
        Self {
            path,
            map,
            dirty: false,
        }
    }

    /// Stars (0..=5) recorded for `key` (a display path), or 0 if none.
    pub fn get(&self, key: &Path) -> u8 {
        self.map
            .get(key.to_string_lossy().as_ref())
            .copied()
            .unwrap_or(0)
    }

    /// A snapshot of the path→stars map, for the background search thread (which can't
    /// borrow the live store) to resolve sidecar-only ratings — e.g. files on a mount
    /// (NTFS, etc.) that can't carry a `user.baloo.rating` xattr.
    pub fn snapshot(&self) -> HashMap<String, u8> {
        self.map.clone()
    }

    /// Record (or, for 0, clear) the rating for `key`. Flushes lazily via [`save`].
    pub fn set(&mut self, key: &Path, stars: u8) {
        let k = key.to_string_lossy().into_owned();
        if stars == 0 {
            if self.map.remove(&k).is_none() {
                return; // nothing changed
            }
        } else {
            self.map.insert(k, stars.min(5));
        }
        self.dirty = true;
    }

    /// Write the JSON file if anything changed since the last save.
    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(s) = serde_json::to_string_pretty(&self.map) {
            let _ = std::fs::write(&self.path, s);
        }
        self.dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_a_file() {
        let dir = std::env::temp_dir().join(format!("kaleidotron_ratings_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let virt = Path::new("<16colo.rs>/2023/pack/ART.ANS");
        {
            let mut s = RatingStore::load(&dir);
            assert_eq!(s.get(virt), 0);
            s.set(virt, 4);
            s.save();
        }
        // A fresh load sees the persisted value.
        let mut s = RatingStore::load(&dir);
        assert_eq!(s.get(virt), 4);
        // Clearing removes it.
        s.set(virt, 0);
        s.save();
        assert_eq!(RatingStore::load(&dir).get(virt), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stars_are_clamped() {
        let dir =
            std::env::temp_dir().join(format!("kaleidotron_ratings_clamp_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = RatingStore::load(&dir);
        s.set(Path::new("x"), 9);
        assert_eq!(s.get(Path::new("x")), 5);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
