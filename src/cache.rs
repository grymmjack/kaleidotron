//! Persistent on-disk HTTP cache for 16colo.rs — JSON API responses, pre-rendered
//! thumbnails, single piece files, and pack zips — so we don't re-fetch the same bytes
//! over the network every session.
//!
//! Layout: blob *bytes* live as files under `<data_dir>/cache/`, and a small **SQLite**
//! index (`cache.db`) maps each URL → its file, byte size, and fetched/last-used
//! timestamps. The index gives freshness (per-call TTL), LRU-ish eviction once the total
//! exceeds a cap, and queryable stats (size / count / clear) for the UI.
//!
//! The cache is reached from background fetch threads (`colo_walk`, `RemoteThumbs`, the
//! download workers), so the connection lives behind a global `Mutex` — index ops are
//! tiny and serialized; the (larger) blob file I/O happens outside the lock. If the cache
//! can't be opened it's simply disabled: every call falls back to a direct network fetch.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Evict least-recently-used blobs once the cache grows past this.
const MAX_BYTES: i64 = 2 * 1024 * 1024 * 1024; // 2 GiB
/// Per-response sanity cap (a pack zip is a few MB; this guards a runaway).
const FETCH_CAP: u64 = 256 * 1024 * 1024; // 256 MB

struct Cache {
    dir: PathBuf,
    db: Mutex<rusqlite::Connection>,
}

static CACHE: OnceLock<Option<Cache>> = OnceLock::new();

fn cache() -> Option<&'static Cache> {
    CACHE.get().and_then(|o| o.as_ref())
}

/// Initialise the cache under `data_dir` (idempotent; the first call wins). On any
/// failure the cache stays disabled and every fetch goes straight to the network.
pub fn init(data_dir: &Path) {
    CACHE.get_or_init(|| {
        let dir = data_dir.join("cache");
        std::fs::create_dir_all(&dir).ok()?;
        let conn = rusqlite::Connection::open(dir.join("cache.db")).ok()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS cache (
                 url     TEXT PRIMARY KEY,
                 file    TEXT NOT NULL,
                 fetched INTEGER NOT NULL,
                 used    INTEGER NOT NULL,
                 bytes   INTEGER NOT NULL
             );",
        )
        .ok()?;
        Some(Cache {
            dir,
            db: Mutex::new(conn),
        })
    });
}

/// The blob directory, if the cache is initialised (used by the legacy per-year packs
/// cache so it lands in the same persistent spot).
pub fn dir() -> Option<PathBuf> {
    cache().map(|c| c.dir.clone())
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn key(url: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// A descriptive User-Agent — Wikimedia (and good API etiquette generally) require one; a
/// missing/blank UA gets 403'd. Kept generic + contactable per the Wikimedia UA policy.
pub const USER_AGENT: &str = "kaleidotron/0.1 (https://github.com/grymmjack/kaleidotron)";

/// HTTP GET `url` into memory (capped). Errors on a network/HTTP failure (so failures
/// are never cached).
fn http_get(url: &str) -> Result<Vec<u8>, String> {
    // Every outbound request in the app funnels through here, so this is the one place politeness
    // has to live: robots.txt, the per-host rate limit, and any server-requested cooldown. Putting
    // it here means no source can bypass it, now or later.
    crate::netpolicy::before_request(url)?;
    let resp = match ureq::get(url).set("User-Agent", USER_AGENT).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            // The server explicitly telling us to slow down — hold off on this host.
            if code == 429 || code == 503 {
                crate::netpolicy::note_throttled(url, r.header("Retry-After"));
            }
            return Err(format!("HTTP {code}"));
        }
        Err(e) => return Err(e.to_string()),
    };
    let mut buf = Vec::new();
    resp.into_reader()
        .take(FETCH_CAP)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    Ok(buf)
}

/// `Content-Length` for `url` via a **HEAD** request, when the server reports one. Used to skip
/// downloading something huge just to build a thumbnail; `None` means "unknown", and the caller
/// decides whether to risk it. Never cached — it's a cheap header-only probe.
pub fn content_length(url: &str) -> Option<u64> {
    // A HEAD is still a request against someone's server — same rate limit and robots rules.
    crate::netpolicy::before_request(url).ok()?;
    let resp = ureq::head(url)
        .set("User-Agent", USER_AGENT)
        .call()
        .ok()?;
    resp.header("Content-Length")?.trim().parse().ok()
}

/// Cached GET → bytes. `ttl` of `None` means the content is immutable (never expires);
/// `Some(secs)` re-fetches once the entry is older than that. Used for JSON + thumbnails.
pub fn get_bytes(url: &str, ttl: Option<i64>) -> Result<Vec<u8>, String> {
    if let Some(bytes) = read_blob(url, ttl) {
        return Ok(bytes);
    }
    let bytes = http_get(url)?;
    write_blob(url, &bytes);
    Ok(bytes)
}

/// Cached GET → a file path *named* `filename` (so the decoder's extension dispatch
/// still works, and a pack zip keeps its `.zip`). Immutable — once fetched it's reused.
/// Returns a path even when the cache is disabled (a temp file).
pub fn get_file(url: &str, filename: &str) -> Result<PathBuf, String> {
    if let Some(c) = cache() {
        let rel = format!("files/{}/{}", key(url), filename);
        let path = c.dir.join(&rel);
        if path.exists() {
            touch(url);
            return Ok(path);
        }
        let bytes = http_get(url)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let tmp = path.with_extension("part");
        std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        record(url, &rel, bytes.len() as i64);
        return Ok(path);
    }
    // Cache disabled → still hand back a (temp) file so callers keep working.
    let dir = std::env::temp_dir().join("kaleidotron-16colors").join(key(url));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(filename);
    if !path.exists() {
        let bytes = http_get(url)?;
        std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    }
    Ok(path)
}

/// True if `url` is already stored as a file blob (so [`get_file`] would be a local hit,
/// no network). Lets the bulk downloader honestly report cache reuse vs. fresh fetches.
/// Verifies the blob file still exists on disk, not just the index row.
pub fn contains(url: &str) -> bool {
    let Some(c) = cache() else { return false };
    let file: Option<String> = {
        let Ok(db) = c.db.lock() else { return false };
        db.query_row("SELECT file FROM cache WHERE url = ?1", [url], |r| r.get(0))
            .ok()
    };
    file.is_some_and(|f| c.dir.join(f).exists())
}

fn read_blob(url: &str, ttl: Option<i64>) -> Option<Vec<u8>> {
    let c = cache()?;
    let (file, fetched): (String, i64) = {
        let db = c.db.lock().ok()?;
        db.query_row(
            "SELECT file, fetched FROM cache WHERE url = ?1",
            [url],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()?
    };
    if ttl.is_some_and(|t| now() - fetched > t) {
        return None; // stale
    }
    let bytes = std::fs::read(c.dir.join(&file)).ok()?;
    touch(url);
    Some(bytes)
}

fn write_blob(url: &str, bytes: &[u8]) {
    let Some(c) = cache() else { return };
    let rel = format!("{}.bin", key(url));
    if std::fs::write(c.dir.join(&rel), bytes).is_ok() {
        record(url, &rel, bytes.len() as i64);
    }
}

/// Index a stored blob + evict if we're over the cap.
fn record(url: &str, file: &str, bytes: i64) {
    let Some(c) = cache() else { return };
    let Ok(db) = c.db.lock() else { return };
    let _ = db.execute(
        "INSERT INTO cache(url, file, fetched, used, bytes) VALUES(?1, ?2, ?3, ?3, ?4)
         ON CONFLICT(url) DO UPDATE SET file = ?2, fetched = ?3, used = ?3, bytes = ?4",
        rusqlite::params![url, file, now(), bytes],
    );
    evict(&c.dir, &db);
}

/// Mark a cache hit as recently used (best-effort; drives LRU eviction).
fn touch(url: &str) {
    if let Some(c) = cache() {
        if let Ok(db) = c.db.lock() {
            let _ = db.execute(
                "UPDATE cache SET used = ?2 WHERE url = ?1",
                rusqlite::params![url, now()],
            );
        }
    }
}

/// Delete least-recently-used blobs while the total exceeds [`MAX_BYTES`].
fn evict(dir: &Path, db: &rusqlite::Connection) {
    let total: i64 = db
        .query_row("SELECT COALESCE(SUM(bytes), 0) FROM cache", [], |r| {
            r.get(0)
        })
        .unwrap_or(0);
    if total <= MAX_BYTES {
        return;
    }
    // Collect the oldest entries first (drop the statement before deleting).
    let victims: Vec<(String, String, i64)> = {
        let Ok(mut stmt) =
            db.prepare("SELECT url, file, bytes FROM cache ORDER BY used ASC LIMIT 512")
        else {
            return;
        };
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    };
    let mut over = total - MAX_BYTES;
    for (url, file, bytes) in victims {
        if over <= 0 {
            break;
        }
        let _ = std::fs::remove_file(dir.join(&file));
        let _ = std::fs::remove_dir(dir.join(&file).parent().unwrap_or(dir)); // empty `files/<key>`
        let _ = db.execute("DELETE FROM cache WHERE url = ?1", [&url]);
        over -= bytes;
    }
}

/// `(total_bytes, entry_count)` currently cached — for a "clear cache" UI.
pub fn stats() -> (i64, i64) {
    let Some(c) = cache() else { return (0, 0) };
    let Ok(db) = c.db.lock() else { return (0, 0) };
    db.query_row(
        "SELECT COALESCE(SUM(bytes), 0), COUNT(*) FROM cache",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap_or((0, 0))
}

/// Empty the cache (delete every blob + index row).
pub fn clear() {
    let Some(c) = cache() else { return };
    if let Ok(db) = c.db.lock() {
        let _ = db.execute("DELETE FROM cache", []);
    }
    // Wipe the blob files (everything but the db itself).
    if let Ok(rd) = std::fs::read_dir(&c.dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("db") {
                continue;
            }
            if p.is_dir() {
                let _ = std::fs::remove_dir_all(&p);
            } else {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_stable_and_hex() {
        assert_eq!(key("https://x/y"), key("https://x/y"));
        assert_ne!(key("a"), key("b"));
        assert_eq!(key("a").len(), 16);
    }
}

#[cfg(test)]
mod live {
    use super::*;
    #[test]
    #[ignore = "hits the live network"]
    fn fetch_is_cached_and_counted() {
        init(&std::env::temp_dir().join("pv_cache_live_test"));
        let url = "https://api.16colo.rs/v1/year/2019?pagesize=1";
        let a = get_bytes(url, None).expect("first fetch");
        let b = get_bytes(url, None).expect("served from cache");
        assert_eq!(a, b, "cached bytes match");
        assert!(!a.is_empty());
        let (bytes, count) = stats();
        assert!(
            bytes >= a.len() as i64 && count >= 1,
            "stats reflect the entry"
        );
    }
}
