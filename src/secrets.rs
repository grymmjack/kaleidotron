//! `secrets.json` — API keys, kept deliberately **out** of [`crate::settings`].
//!
//! `settings.json` is designed to be shared: committed to a dotfile repo, synced between machines,
//! pasted into an issue. That makes it exactly the wrong place for a credential — the failure mode
//! isn't hypothetical, it's the ordinary result of tracking a config file in git.
//!
//! So keys live here instead, in a separate file the user can exclude from sync. `settings.json`
//! carries only a pointer comment saying where they went.
//!
//! Deliberately minimal: no encryption, because a local file the app must read at startup without a
//! prompt cannot be meaningfully protected by a key stored beside it — that only looks secure. On a
//! shared machine, an OS keyring is the real answer; this is honest file permissions instead
//! (0600 on Unix, so other local users can't read it).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Where the file lives, given the app's data dir.
pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("secrets.json")
}

/// Read `key -> value` pairs. Missing file or unparseable content yields nothing, never an error —
/// every key here is optional and the app must start fine without them.
pub fn load(file: &Path) -> HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return HashMap::new();
    };
    let cleaned = crate::keybindings::strip_comments(&text);
    match serde_json::from_str::<serde_json::Value>(&cleaned) {
        Ok(serde_json::Value::Object(m)) => m
            .into_iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
            .collect(),
        _ => HashMap::new(),
    }
}

/// Write the file, restricting it to the owner on Unix.
///
/// Writes nothing (and removes an existing file) when every value is blank, so clearing your keys
/// in Preferences doesn't leave an empty husk behind that looks like it still holds something.
pub fn save(file: &Path, entries: &[(&str, &str, &str)]) -> Result<(), String> {
    let live: Vec<_> = entries.iter().filter(|(_, v, _)| !v.trim().is_empty()).collect();
    if live.is_empty() {
        let _ = std::fs::remove_file(file);
        return Ok(());
    }
    if let Some(d) = file.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let mut s = String::from(
        "// kaleidotron secrets — API keys.\n\
         // Kept out of settings.json so that file stays safe to sync or share.\n\
         // EXCLUDE THIS FILE from dotfile managers and version control.\n{\n",
    );
    for (i, (k, v, doc)) in live.iter().enumerate() {
        if !doc.is_empty() {
            s.push_str(&format!("  // {doc}\n"));
        }
        let comma = if i + 1 == live.len() { "" } else { "," };
        // serde_json handles escaping, so a key containing a quote can't break the file.
        let val = serde_json::to_string(v).unwrap_or_else(|_| "\"\"".into());
        s.push_str(&format!("  \"{k}\": {val}{comma}\n"));
    }
    s.push_str("}\n");
    std::fs::write(file, s).map_err(|e| e.to_string())?;
    restrict(file);
    Ok(())
}

/// Owner-only permissions (0600). No-op off Unix, where this has no direct equivalent.
#[cfg(unix)]
fn restrict(file: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn restrict(_file: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_escapes() {
        let dir = std::env::temp_dir().join(format!("pv_secrets_{}", std::process::id()));
        let f = path(&dir);
        let entries = [
            ("steam_api_key", "ABC\"123", "Steam Web API key"),
            ("ma_key", "modkey", "ModArchive API key"),
        ];
        save(&f, &entries).unwrap();
        let m = load(&f);
        assert_eq!(m.get("steam_api_key").map(String::as_str), Some("ABC\"123"));
        assert_eq!(m.get("ma_key").map(String::as_str), Some("modkey"));
        // The warning has to survive in the emitted file — it's the whole point.
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.contains("EXCLUDE THIS FILE"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn blank_values_remove_the_file_rather_than_leaving_a_husk() {
        let dir = std::env::temp_dir().join(format!("pv_secrets_blank_{}", std::process::id()));
        let f = path(&dir);
        save(&f, &[("steam_api_key", "x", "")]).unwrap();
        assert!(f.exists());
        save(&f, &[("steam_api_key", "   ", "")]).unwrap();
        assert!(!f.exists(), "clearing every key removes the file");
        // Loading a missing file is not an error.
        assert!(load(&f).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_file_is_not_fatal() {
        let dir = std::env::temp_dir().join(format!("pv_secrets_bad_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = path(&dir);
        std::fs::write(&f, "{ not json").unwrap();
        assert!(load(&f).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
