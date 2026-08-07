//! `keybindings.json` — hand-editable key bindings, VSCode-style.
//!
//! The first of three text config files (the others being `settings.json` and `themes/*.json`).
//! Keeping config as plain text rather than an opaque persisted blob means it can be diffed,
//! commented, shared, and — the motivating case here — carried between machines by a dotfile
//! manager like chezmoi.
//!
//! Format: a JSON array, `//` line comments allowed (we emit them, so we must accept them).
//!
//! ```jsonc
//! [
//!   // Previous image
//!   { "action": "prev_image", "key": "ArrowLeft" }
//! ]
//! ```
//!
//! **Actions are named, not numbered.** The previous persisted form was `Vec<(u8, String)>` keyed
//! by the action's index in `Action::ALL` — which is why that array carries a "new actions are
//! appended so persisted indices stay valid" warning. A string id removes that constraint
//! entirely: actions can be reordered or removed without silently rebinding someone's keys.
//!
//! This module is deliberately egui-free (it moves `(action_id, key_name)` string pairs), so the
//! file format is unit-testable without a UI. `app.rs` maps those to `Action` / `egui::Key`.

use std::path::{Path, PathBuf};

/// Where the file lives, given the app's data dir.
pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("keybindings.json")
}

/// Make a **JSONC** document parseable by `serde_json`: strip `//` line comments and `/* … */`
/// block comments, and remove trailing commas.
///
/// All three matter for real files. VS Code ships its own themes with `//` comments *and* trailing
/// commas — `dark_modern.json` fails `serde_json` (and Python's `json`) on a trailing comma alone —
/// so a cleaner that only handled comments would reject the very files this is meant to import.
///
/// Quote-aware throughout: a `//`, a `/*` or a `,` inside a string literal must survive, and
/// escapes are tracked so `"a\"//b"` isn't mistaken for the end of a string.
pub(crate) fn strip_comments(text: &str) -> String {
    let b: Vec<char> = text.chars().collect();
    let mut out: Vec<char> = Vec::with_capacity(b.len());
    let (mut instr, mut esc) = (false, false);
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if instr {
            out.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                instr = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => {
                instr = true;
                out.push(c);
                i += 1;
            }
            '/' if i + 1 < b.len() && b[i + 1] == '/' => {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
            }
            '/' if i + 1 < b.len() && b[i + 1] == '*' => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    // Trailing commas: drop any `,` whose next non-whitespace character closes the object/array.
    let mut s: Vec<char> = Vec::with_capacity(out.len());
    let (mut instr, mut esc) = (false, false);
    for (i, &c) in out.iter().enumerate() {
        if instr {
            s.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                instr = false;
            }
            continue;
        }
        if c == '"' {
            instr = true;
            s.push(c);
            continue;
        }
        if c == ',' {
            if let Some(&nxt) = out[i + 1..].iter().find(|ch| !ch.is_whitespace()) {
                if nxt == '}' || nxt == ']' {
                    continue; // trailing comma — drop it
                }
            }
        }
        s.push(c);
    }
    s.into_iter().collect()
}

/// Parse a `keybindings.json` body into `(action_id, key_name)` pairs.
///
/// Tolerant by design: unknown ids, missing fields and malformed entries are skipped rather than
/// failing the whole file — one bad line shouldn't cost the user every other binding.
pub fn parse(text: &str) -> Vec<(String, String)> {
    let cleaned = strip_comments(text);
    let v: serde_json::Value = match serde_json::from_str(&cleaned) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|e| {
            let a = e["action"].as_str()?.trim();
            let k = e["key"].as_str()?.trim();
            (!a.is_empty() && !k.is_empty()).then(|| (a.to_string(), k.to_string()))
        })
        .collect()
}

/// Render bindings as a documented JSON file. `labels` supplies each action's human description,
/// emitted as a `//` comment above its entry so the file explains itself.
pub fn to_json(entries: &[(String, String)], labels: &dyn Fn(&str) -> Option<String>) -> String {
    let mut s = String::from(
        "// kaleidotron key bindings.\n\
         // Edit and save — changes apply without restarting.\n\
         // \"key\" uses egui key names: ArrowLeft, PageDown, F5, A, Num1, …\n\
         // Delete this file to restore the defaults.\n[\n",
    );
    for (i, (a, k)) in entries.iter().enumerate() {
        if let Some(l) = labels(a) {
            s.push_str(&format!("  // {l}\n"));
        }
        let comma = if i + 1 == entries.len() { "" } else { "," };
        s.push_str(&format!("  {{ \"action\": \"{a}\", \"key\": \"{k}\" }}{comma}\n"));
    }
    s.push_str("]\n");
    s
}

/// Read the file, if present and parseable.
pub fn load(file: &Path) -> Option<Vec<(String, String)>> {
    let text = std::fs::read_to_string(file).ok()?;
    let v = parse(&text);
    (!v.is_empty()).then_some(v)
}

/// Write the file (creating the directory as needed).
pub fn save(
    file: &Path,
    entries: &[(String, String)],
    labels: &dyn Fn(&str) -> Option<String>,
) -> Result<(), String> {
    if let Some(d) = file.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(file, to_json(entries, labels)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_commented_file() {
        let text = "// header\n[\n  // Previous image\n  { \"action\": \"prev_image\", \"key\": \"ArrowLeft\" },\n  { \"action\": \"next_image\", \"key\": \"ArrowRight\" }\n]\n";
        let v = parse(text);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], ("prev_image".into(), "ArrowLeft".into()));
        assert_eq!(v[1].1, "ArrowRight");
    }

    #[test]
    fn a_slash_inside_a_string_is_not_a_comment() {
        // The quote-aware strip is what keeps this from truncating mid-value.
        let text = r#"[{ "action": "http://x//y", "key": "A" }]"#;
        assert_eq!(parse(text), vec![("http://x//y".to_string(), "A".to_string())]);
        // …including when the string contains an escaped quote before the slashes.
        let text = r#"[{ "action": "a\"//b", "key": "A" }]"#;
        assert_eq!(parse(text).len(), 1);
    }

    #[test]
    fn bad_entries_are_skipped_not_fatal() {
        let text = r#"[
            { "action": "ok", "key": "A" },
            { "action": "", "key": "B" },
            { "action": "missing_key" },
            { "key": "no_action" },
            "not an object"
        ]"#;
        assert_eq!(parse(text), vec![("ok".to_string(), "A".to_string())]);
        // A file that isn't valid JSON at all yields nothing rather than panicking.
        assert!(parse("{ not json").is_empty());
        assert!(parse("").is_empty());
    }

    #[test]
    fn round_trips_through_json() {
        let entries = vec![
            ("prev_image".to_string(), "ArrowLeft".to_string()),
            ("next_image".to_string(), "ArrowRight".to_string()),
        ];
        let labels = |a: &str| Some(format!("label for {a}"));
        let text = to_json(&entries, &labels);
        assert!(text.contains("// label for prev_image"));
        // Trailing commas would break serde_json — the last entry must not have one.
        assert!(!text.contains("\"ArrowRight\" },"));
        assert_eq!(parse(&text), entries, "emitted file re-parses to the same bindings");
    }
}
