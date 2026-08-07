//! Persistent "view history" for browsed items — visited state, view counts, and
//! first/last-viewed timestamps — backed by a small SQLite database alongside eframe's
//! storage (e.g. `~/.local/share/kaleidotron/views.db`). Keyed by the *stable display
//! path* (the same identity ratings use; see [`crate::app::Kaleidotron::to_display`]), so a
//! piece inside a zip or on 16colo.rs is tracked across re-extraction / re-download.
//!
//! All rows are mirrored into an in-memory map on open, so the grid/table can ask "is
//! this visited?" for many tiles every frame without touching SQLite. Writes go through
//! to disk immediately — the data is small and writes are user-paced (open an image,
//! open a folder), so there's no need to batch.
//!
//! SQLite is heavier than the project's usual JSON-sidecar pattern (`ratings.rs`), but
//! it was a deliberate choice for richer view-history queries down the road.

use std::collections::HashMap;
use std::path::Path;

/// One item's view history. Timestamps are unix seconds (`0` = unknown).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ViewRecord {
    pub count: u32,
    pub first: i64,
    pub last: i64,
}

/// A SQLite-backed store of which items have been viewed, how often, and when, with an
/// in-memory mirror for cheap per-frame lookups.
pub struct ViewDb {
    conn: rusqlite::Connection,
    cache: HashMap<String, ViewRecord>,
}

impl ViewDb {
    /// Open (creating if needed) `views.db` in `dir`. Returns `None` if SQLite can't be
    /// opened or the schema can't be ensured — the app then runs with view-tracking
    /// silently disabled rather than crashing.
    pub fn open(dir: &Path) -> Option<ViewDb> {
        let conn = rusqlite::Connection::open(dir.join("views.db")).ok()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS views (
                 path  TEXT PRIMARY KEY,
                 count INTEGER NOT NULL DEFAULT 0,
                 first INTEGER NOT NULL DEFAULT 0,
                 last  INTEGER NOT NULL DEFAULT 0
             );",
        )
        .ok()?;
        let mut cache = HashMap::new();
        {
            let mut stmt = conn
                .prepare("SELECT path, count, first, last FROM views")
                .ok()?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        ViewRecord {
                            count: r.get::<_, i64>(1)?.max(0) as u32,
                            first: r.get::<_, i64>(2)?,
                            last: r.get::<_, i64>(3)?,
                        },
                    ))
                })
                .ok()?;
            for (path, rec) in rows.flatten() {
                cache.insert(path, rec);
            }
        }
        Some(ViewDb { conn, cache })
    }

    /// In-memory only (no SQLite file) — for tests.
    #[cfg(test)]
    pub fn in_memory() -> ViewDb {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE views (path TEXT PRIMARY KEY, count INTEGER NOT NULL DEFAULT 0,
             first INTEGER NOT NULL DEFAULT 0, last INTEGER NOT NULL DEFAULT 0);",
        )
        .unwrap();
        ViewDb {
            conn,
            cache: HashMap::new(),
        }
    }

    /// True if `key` has been viewed at least once.
    pub fn is_viewed(&self, key: &str) -> bool {
        self.cache.get(key).is_some_and(|r| r.count > 0)
    }

    /// The full record (counts + timestamps) for `key`, if any.
    pub fn get(&self, key: &str) -> Option<ViewRecord> {
        self.cache.get(key).copied()
    }

    /// Record one view of `key` at unix time `now`: bump count, set `last`, and stamp
    /// `first` on the first view. Writes through to both the cache and SQLite.
    pub fn record(&mut self, key: &str, now: i64) {
        let rec = self.cache.entry(key.to_string()).or_default();
        rec.count += 1;
        rec.last = now;
        if rec.first == 0 {
            rec.first = now;
        }
        let rec = *rec;
        self.persist(key, &rec);
    }

    /// Manually set visited state. `true` ensures a row (one view stamped via `now` if
    /// the item was unvisited); `false` clears the history entirely.
    pub fn set_viewed(&mut self, key: &str, viewed: bool, now: i64) {
        if viewed {
            if !self.is_viewed(key) {
                self.record(key, now);
            }
        } else if self.cache.remove(key).is_some() {
            let _ = self
                .conn
                .execute("DELETE FROM views WHERE path = ?1", rusqlite::params![key]);
        }
    }

    fn persist(&self, key: &str, rec: &ViewRecord) {
        let _ = self.conn.execute(
            "INSERT INTO views(path, count, first, last) VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET count = ?2, first = ?3, last = ?4",
            rusqlite::params![key, rec.count as i64, rec.first, rec.last],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_bump_count_and_stamp_times() {
        let mut db = ViewDb::in_memory();
        assert!(!db.is_viewed("a"));
        db.record("a", 100);
        db.record("a", 200);
        let r = db.get("a").unwrap();
        assert_eq!(r.count, 2);
        assert_eq!(r.first, 100); // stamped once, on the first view
        assert_eq!(r.last, 200); // moves forward each view
        assert!(db.is_viewed("a"));
    }

    #[test]
    fn set_viewed_toggles_cleanly() {
        let mut db = ViewDb::in_memory();
        db.set_viewed("x", true, 50);
        assert!(db.is_viewed("x"));
        assert_eq!(db.get("x").unwrap().count, 1);
        db.set_viewed("x", false, 60);
        assert!(!db.is_viewed("x"));
        assert!(db.get("x").is_none());
    }

    #[test]
    fn persists_across_reopen() {
        let dir =
            std::env::temp_dir().join(format!("kaleidotron_viewdb_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        {
            let mut db = ViewDb::open(&dir).expect("open");
            db.record("/p/art.ans", 1234);
        }
        let db = ViewDb::open(&dir).expect("reopen");
        assert_eq!(db.get("/p/art.ans").unwrap().count, 1);
        assert_eq!(db.get("/p/art.ans").unwrap().last, 1234);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
