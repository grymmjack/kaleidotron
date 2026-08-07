//! Star ratings via the KDE Baloo extended attribute `user.baloo.rating`.
//!
//! That's the same scheme Gwenview writes when you press 1–5, so ratings made
//! here show up in Gwenview (and feed the user's `sort-by-rating.sh`). The value
//! is an ASCII integer on a 0..10 scale — 2 points per star — so `stars = v / 2`.

use std::path::Path;

#[cfg(unix)]
const ATTR: &str = "user.baloo.rating";

/// Read the star rating (0..=5). 0 means unrated / absent / unreadable.
///
/// Extended attributes only exist on Unix; on Windows there's no xattr backend, so
/// this always returns 0 (unrated) and the caller falls back to the ratings.json sidecar.
#[cfg(unix)]
pub fn read(path: &Path) -> u8 {
    match xattr::get(path, ATTR) {
        Ok(Some(bytes)) => parse_value(&bytes),
        _ => 0,
    }
}

#[cfg(not(unix))]
pub fn read(_path: &Path) -> u8 {
    0
}

/// Write a star rating (1..=5), or clear it (0 removes the attribute, like Baloo).
///
/// No-op on non-Unix (no xattr backend) — ratings persist via the ratings.json sidecar.
#[cfg(unix)]
pub fn write(path: &Path, stars: u8) -> std::io::Result<()> {
    if stars == 0 {
        // Removing a non-existent attr returns an error (ENODATA) — that's fine.
        let _ = xattr::remove(path, ATTR);
        Ok(())
    } else {
        xattr::set(path, ATTR, encode_stars(stars).as_bytes())
    }
}

#[cfg(not(unix))]
pub fn write(_path: &Path, _stars: u8) -> std::io::Result<()> {
    Ok(())
}

/// Parse a Baloo rating value (ASCII `0..10`) into stars (0..=5).
// Only the cfg(unix) `read` path (and the tests) use this — gate it so a non-unix
// build doesn't warn it dead.
#[cfg(any(unix, test))]
fn parse_value(bytes: &[u8]) -> u8 {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .map(|v| (v / 2).min(5) as u8)
        .unwrap_or(0)
}

/// Encode stars (clamped 0..=5) as the Baloo ASCII value (`stars * 2`).
#[cfg(any(unix, test))]
fn encode_stars(stars: u8) -> String {
    (u32::from(stars.min(5)) * 2).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_parses_to_stars() {
        assert_eq!(parse_value(b"10"), 5);
        assert_eq!(parse_value(b"8"), 4);
        assert_eq!(parse_value(b"6"), 3);
        assert_eq!(parse_value(b"2"), 1);
        assert_eq!(parse_value(b"0"), 0);
        assert_eq!(parse_value(b"11"), 5); // clamps to 5 stars
        assert_eq!(parse_value(b" 6 "), 3); // trims whitespace
        assert_eq!(parse_value(b"xyz"), 0); // garbage -> unrated
        assert_eq!(parse_value(b""), 0);
    }

    #[test]
    fn stars_encode_to_baloo_value() {
        assert_eq!(encode_stars(5), "10");
        assert_eq!(encode_stars(4), "8");
        assert_eq!(encode_stars(3), "6");
        assert_eq!(encode_stars(1), "2");
        assert_eq!(encode_stars(9), "10"); // clamps
    }

    #[cfg(unix)]
    #[test]
    fn xattr_round_trip_on_real_file() {
        // Best-effort: some temp filesystems don't support user xattrs, so we
        // only assert when the write actually succeeds.
        let path =
            std::env::temp_dir().join(format!("kaleidotron_rating_{}.bin", std::process::id()));
        std::fs::write(&path, b"x").unwrap();
        if write(&path, 4).is_ok() {
            assert_eq!(read(&path), 4);
            write(&path, 0).unwrap();
            assert_eq!(read(&path), 0);
        }
        let _ = std::fs::remove_file(&path);
    }
}
