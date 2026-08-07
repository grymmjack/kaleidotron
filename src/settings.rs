//! `settings.json` — hand-editable preferences, VSCode-style.
//!
//! The second of the three text config files, after [`crate::keybindings`] (whose parsing rules
//! this shares) and before `themes/*.json`. Same motivation: plain text can be diffed, commented,
//! shared, and carried between machines by a dotfile manager, which an opaque persisted blob can't.
//!
//! ```jsonc
//! {
//!   // ── Appearance ──
//!   "appearance": {
//!     // 0 = dark, 1 = light
//!     "theme": 0,
//!     "grid_gap": 8.0
//!   },
//!   "plugins": { "plugin_audio": true }
//! }
//! ```
//!
//! Settings are grouped into a nested object per section rather than one flat blob — 45 keys in a
//! single object is unreadable. [`parse`] flattens one level, and also accepts a **flat** file, so
//! a config written by an earlier build still loads and is simply rewritten nested on the next
//! save. Keys are unique across sections, so flattening is unambiguous.
//!
//! **This file is a curated subset, not a dump of everything persisted.** Three categories are
//! deliberately excluded:
//!
//! * **Machine-local state** — window geometry, the last-opened folder, panel/divider positions.
//!   Two machines with different displays must not share these, so they stay in eframe's storage.
//! * **Secrets** — API keys live in a separate, sync-excluded file. A settings file tracked by a
//!   dotfile manager is exactly how a key gets committed by accident.
//! * **Caches and transient state** — nothing a person would ever hand-edit.
//!
//! Like `keybindings.rs` this module is deliberately egui-free and knows nothing about
//! `Kaleidotron`: it moves `(key, json value)` pairs, so the file format is unit-testable without a
//! UI. `app.rs` owns the table that maps keys onto fields.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Where the file lives, given the app's data dir.
pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("settings.json")
}

/// The JSON object name for a section heading ("Web sources" -> "web_sources").
pub fn section_key(section: &str) -> String {
    section.to_lowercase().replace(' ', "_")
}

/// One setting as it appears in the file: which section it's grouped under, its key, its current
/// value, and a one-line explanation emitted as a `//` comment above it.
pub struct Entry {
    pub section: &'static str,
    pub key: &'static str,
    pub value: serde_json::Value,
    pub doc: &'static str,
}

/// Parse a `settings.json` body into key → value.
///
/// Tolerant like the keybindings parser: `//` comments are stripped quote-aware, and a file that
/// fails to parse yields nothing rather than panicking — the caller keeps its defaults.
pub fn parse(text: &str) -> HashMap<String, serde_json::Value> {
    let cleaned = crate::keybindings::strip_comments(text);
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(&cleaned)
    else {
        return HashMap::new();
    };
    // Flatten one level: a section object contributes its members, anything else is a flat key
    // (which is how a file written by an earlier, un-sectioned build still loads).
    let mut out = HashMap::new();
    for (k, v) in map {
        match v {
            serde_json::Value::Object(inner) => out.extend(inner),
            other => {
                out.insert(k, other);
            }
        }
    }
    out
}

/// Render entries as a documented JSON object, grouped by section in the order given.
pub fn to_json(entries: &[Entry]) -> String {
    let mut s = String::from(
        "// kaleidotron settings.\n\
         // Edit and save. Delete a line to restore its default; delete the file to reset all.\n\
         // Machine-local state (window size, last folder) is deliberately NOT here.\n\
         // API keys live in secrets.json — keep THAT file out of dotfile sync.\n\
         {\n",
    );
    // Group into one nested object per section, preserving the order entries arrive in.
    let mut groups: Vec<(&'static str, Vec<&Entry>)> = Vec::new();
    for e in entries {
        match groups.last_mut() {
            Some((sec, list)) if *sec == e.section => list.push(e),
            _ => groups.push((e.section, vec![e])),
        }
    }
    for (gi, (section, list)) in groups.iter().enumerate() {
        if gi > 0 {
            s.push('\n');
        }
        s.push_str(&format!("  // ── {section} ──\n"));
        s.push_str(&format!("  \"{}\": {{\n", section_key(section)));
        for (i, e) in list.iter().enumerate() {
            if !e.doc.is_empty() {
                s.push_str(&format!("    // {}\n", e.doc));
            }
            let comma = if i + 1 == list.len() { "" } else { "," };
            let v = serde_json::to_string(&e.value).unwrap_or_else(|_| "null".into());
            s.push_str(&format!("    \"{}\": {v}{comma}\n", e.key));
        }
        let comma = if gi + 1 == groups.len() { "" } else { "," };
        s.push_str(&format!("  }}{comma}\n"));
    }
    s.push_str("}\n");
    s
}

/// Read the file, if present and parseable.
pub fn load(file: &Path) -> Option<HashMap<String, serde_json::Value>> {
    let text = std::fs::read_to_string(file).ok()?;
    let map = parse(&text);
    (!map.is_empty()).then_some(map)
}

/// Write the file, creating the directory as needed.
/// Write the file **atomically**: a full temp file, then a rename.
///
/// `fs::write` truncates and then fills, so anything reading during that window sees a partial —
/// or empty — file. This is not hypothetical: launching a second instance while the first is
/// flushing produced exactly that, and since an unreadable file parses to nothing, the reader then
/// wrote its *defaults* back over the user's settings. A rename is atomic on both Unix and Windows,
/// so a reader sees either the old file or the new one, never a half-written one.
pub fn save(file: &Path, entries: &[Entry]) -> Result<(), String> {
    if let Some(d) = file.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, to_json(entries)).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, file).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Does a settings file exist but yield nothing usable?
///
/// The distinction matters: a *missing* file is the normal first run and should be seeded with
/// defaults, whereas a file that exists but won't parse holds settings somebody wrote by hand.
/// Overwriting that with defaults is silent data loss, so the caller preserves it instead.
pub fn exists_but_unreadable(file: &Path) -> bool {
    file.exists() && load(file).is_none()
}

// --- Typed readers. Each returns `None` when the key is absent or the wrong type, so a mangled
// value falls back to the in-app default rather than to zero.

pub fn get_bool(m: &HashMap<String, serde_json::Value>, k: &str) -> Option<bool> {
    m.get(k)?.as_bool()
}
pub fn get_f32(m: &HashMap<String, serde_json::Value>, k: &str) -> Option<f32> {
    m.get(k)?.as_f64().map(|v| v as f32)
}
pub fn get_u64(m: &HashMap<String, serde_json::Value>, k: &str) -> Option<u64> {
    m.get(k)?.as_u64()
}
/// Unused by the Appearance tranche; the Paths/Audio tranches (yt_download_dir, midi
/// soundfont) need it, so it ships with the readers rather than being added piecemeal.
#[allow(dead_code)]
pub fn get_string(m: &HashMap<String, serde_json::Value>, k: &str) -> Option<String> {
    m.get(k)?.as_str().map(String::from)
}
/// An `[r, g, b]` triple. Values outside 0–255 are clamped rather than rejected — a hand-typed
/// `[255, 300, -5]` should still give you something sane.
pub fn get_rgb(m: &HashMap<String, serde_json::Value>, k: &str) -> Option<[u8; 3]> {
    let a = m.get(k)?.as_array()?;
    if a.len() != 3 {
        return None;
    }
    let c = |i: usize| a[i].as_i64().unwrap_or(0).clamp(0, 255) as u8;
    Some([c(0), c(1), c(2)])
}

/// Helper for building an [`Entry`] from anything serde can serialize.
pub fn entry<T: serde::Serialize>(
    section: &'static str,
    key: &'static str,
    value: T,
    doc: &'static str,
) -> Entry {
    Entry {
        section,
        key,
        value: serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
        doc,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Entry> {
        vec![
            entry("Appearance", "theme", 0u8, "0 = dark, 1 = light"),
            entry("Appearance", "grid_gap", 8.0f32, "horizontal gap between tiles"),
            entry("Viewer", "black_bg", true, "black viewer background"),
            entry("Viewer", "transp_color", [255u8, 0, 255], "solid transparency backdrop"),
        ]
    }

    #[test]
    fn emits_nested_documented_json_that_reparses() {
        let text = to_json(&sample());
        assert!(text.contains("// ── Appearance ──"));
        assert!(text.contains("// ── Viewer ──"));
        assert!(text.contains("// 0 = dark, 1 = light"));
        // Each section is its own object, not a flat blob.
        assert!(text.contains("\"appearance\": {"), "sections are nested objects");
        assert!(text.contains("\"viewer\": {"));
        // Must still be valid JSON — a stray trailing comma anywhere would break serde_json.
        let cleaned = crate::keybindings::strip_comments(&text);
        serde_json::from_str::<serde_json::Value>(&cleaned).expect("emitted file is valid JSON");

        let m = parse(&text);
        assert_eq!(get_u64(&m, "theme"), Some(0));
        assert_eq!(get_f32(&m, "grid_gap"), Some(8.0));
        assert_eq!(get_bool(&m, "black_bg"), Some(true));
        assert_eq!(get_rgb(&m, "transp_color"), Some([255, 0, 255]));
    }

    #[test]
    fn a_flat_file_from_an_older_build_still_loads() {
        // Backward compatibility: settings.json used to be one flat object. Such a file must keep
        // working (and is simply rewritten nested on the next save) rather than resetting someone
        // to defaults.
        let flat = r#"{ "theme": 1, "grid_gap": 4.0, "black_bg": true }"#;
        let m = parse(flat);
        assert_eq!(get_u64(&m, "theme"), Some(1));
        assert_eq!(get_f32(&m, "grid_gap"), Some(4.0));
        assert_eq!(get_bool(&m, "black_bg"), Some(true));
    }

    #[test]
    fn section_names_become_object_keys() {
        assert_eq!(section_key("Appearance"), "appearance");
        assert_eq!(section_key("Web sources"), "web_sources");
    }

    #[test]
    fn missing_or_wrong_typed_values_fall_back() {
        let m = parse(r#"{ "theme": "not a number", "grid_gap": true }"#);
        // Wrong type → None, so the caller keeps its default rather than getting 0.
        assert_eq!(get_u64(&m, "theme"), None);
        assert_eq!(get_f32(&m, "grid_gap"), None);
        // Absent key → None.
        assert_eq!(get_bool(&m, "nope"), None);
        // A broken file yields nothing at all, rather than panicking.
        assert!(parse("{ not json").is_empty());
    }

    #[test]
    fn rgb_is_clamped_not_rejected() {
        let m = parse(r#"{ "c": [255, 300, -5], "short": [1, 2] }"#);
        assert_eq!(get_rgb(&m, "c"), Some([255, 255, 0]));
        assert_eq!(get_rgb(&m, "short"), None, "wrong length is rejected");
    }

    #[test]
    fn an_unreadable_file_is_detected_rather_than_replaced() {
        // The bug this guards: a partial or unparseable settings file yields nothing, and a caller
        // that then writes its defaults back has silently destroyed hand-written settings.
        let dir = std::env::temp_dir().join(format!("pv_set_bad_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("settings.json");
        // Missing is NOT unreadable — that's an ordinary first run, which should be seeded.
        assert!(!exists_but_unreadable(&f));
        std::fs::write(&f, "").unwrap();
        assert!(exists_but_unreadable(&f), "an empty file is not usable settings");
        std::fs::write(&f, "{ this is not json").unwrap();
        assert!(exists_but_unreadable(&f));
        std::fs::write(&f, r#"{ "Appearance": { "theme_name": "X" } }"#).unwrap();
        assert!(!exists_but_unreadable(&f));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_leaves_no_partial_file_behind() {
        let dir = std::env::temp_dir().join(format!("pv_set_atomic_{}", std::process::id()));
        let f = dir.join("settings.json");
        let entries = [Entry {
            section: "Appearance",
            key: "theme_name",
            value: serde_json::Value::String("X".into()),
            doc: "",
        }];
        save(&f, &entries).unwrap();
        assert_eq!(load(&f).unwrap()["theme_name"], serde_json::json!("X"));
        // The temp file must not survive a successful write.
        assert!(!f.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn comments_are_stripped_but_strings_survive() {
        let m = parse("{\n // a comment\n \"dir\": \"http://x//y\" // trailing\n}\n");
        assert_eq!(get_string(&m, "dir").as_deref(), Some("http://x//y"));
    }
}
