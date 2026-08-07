//! `themes/*.json` — UI skins, and an importer for **VS Code** theme files.
//!
//! The third text config file, after [`crate::settings`] and [`crate::keybindings`], and the reason
//! the other two established their conventions first: same tolerant `//`-comment parsing, same
//! bundled-plus-user merge, same "a bad file is never fatal".
//!
//! **A curated subset, not `egui::Visuals` serialized.** `Visuals` is large and its fields move
//! between egui releases — persisting it directly would break every saved theme on each upgrade.
//! Instead a theme is ~16 named colours that [`Theme::to_visuals`] maps onto whatever `Visuals`
//! currently looks like. Adding an egui version bump costs one mapping edit, not a migration.
//!
//! **VS Code import** ([`from_vscode`]) reads a theme's `colors` block. The vocabularies don't line
//! up one-to-one, so this is a curated mapping of the keys that matter with sensible fallbacks —
//! `editor.background` → the viewer field, `sideBar.background` → panels, and so on. A VS Code
//! theme also carries `tokenColors`, which is what a future syntax-highlighting theme would bind
//! to; it's parsed into [`Theme::tokens`] now so that work doesn't need to re-read the file format.

use eframe::egui;
use std::path::{Path, PathBuf};

/// Where user themes live.
pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("themes")
}

/// `#rgb`, `#rrggbb` or `#rrggbbaa` → RGBA. Returns `None` for anything else, so a mistyped colour
/// falls back to the base theme's value rather than rendering as black.
pub fn parse_hex(s: &str) -> Option<[u8; 4]> {
    let h = s.trim().trim_start_matches('#');
    let b = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    match h.len() {
        3 => {
            let d = |i: usize| {
                u8::from_str_radix(&h[i..i + 1], 16).ok().map(|v| v * 17)
            };
            Some([d(0)?, d(1)?, d(2)?, 255])
        }
        6 => Some([b(0)?, b(2)?, b(4)?, 255]),
        8 => Some([b(0)?, b(2)?, b(4)?, b(6)?]),
        _ => None,
    }
}

/// One UI skin. Every colour is optional: a theme sets only what it wants to change, and the rest
/// comes from egui's dark or light base.
#[derive(Clone, Debug, Default)]
pub struct Theme {
    pub name: String,
    /// Build on egui's dark base (the default) or its light one.
    pub dark: bool,
    pub window_bg: Option<[u8; 4]>,
    pub panel_bg: Option<[u8; 4]>,
    pub faint_bg: Option<[u8; 4]>,
    pub extreme_bg: Option<[u8; 4]>,
    pub text: Option<[u8; 4]>,
    pub weak_text: Option<[u8; 4]>,
    pub strong_text: Option<[u8; 4]>,
    pub accent: Option<[u8; 4]>,
    pub accent_text: Option<[u8; 4]>,
    pub hover_bg: Option<[u8; 4]>,
    pub active_bg: Option<[u8; 4]>,
    pub widget_bg: Option<[u8; 4]>,
    pub border: Option<[u8; 4]>,
    pub hyperlink: Option<[u8; 4]>,
    pub warn: Option<[u8; 4]>,
    pub error: Option<[u8; 4]>,
    /// Syntax colours from a VS Code theme's `tokenColors`, keyed by scope (`comment`, `string`,
    /// `keyword`, …). Not read yet — the code viewer still rasterises text with its own palette.
    /// Parsed and tested now so the syntax-theming work binds to a format that's already proven,
    /// rather than re-deriving it from VS Code's files later.
    #[allow(dead_code)]
    pub tokens: std::collections::HashMap<String, [u8; 4]>,
}

fn col(v: &serde_json::Value, key: &str) -> Option<[u8; 4]> {
    v.get(key)?.as_str().and_then(parse_hex)
}

impl Theme {
    /// Turn this theme into egui `Visuals`, starting from the dark or light base so anything the
    /// theme doesn't specify still looks deliberate.
    pub fn to_visuals(&self) -> egui::Visuals {
        let mut v = if self.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        let c = |x: [u8; 4]| egui::Color32::from_rgba_unmultiplied(x[0], x[1], x[2], x[3]);
        if let Some(x) = self.window_bg {
            v.window_fill = c(x);
        }
        if let Some(x) = self.panel_bg {
            v.panel_fill = c(x);
        }
        if let Some(x) = self.faint_bg {
            v.faint_bg_color = c(x);
        }
        if let Some(x) = self.extreme_bg {
            v.extreme_bg_color = c(x);
        }
        if let Some(x) = self.text {
            v.override_text_color = Some(c(x));
        }
        if let Some(x) = self.accent {
            v.selection.bg_fill = c(x);
        }
        if let Some(x) = self.accent_text {
            v.selection.stroke.color = c(x);
        }
        if let Some(x) = self.hyperlink {
            v.hyperlink_color = c(x);
        }
        if let Some(x) = self.warn {
            v.warn_fg_color = c(x);
        }
        if let Some(x) = self.error {
            v.error_fg_color = c(x);
        }
        if let Some(x) = self.widget_bg {
            v.widgets.inactive.bg_fill = c(x);
            v.widgets.inactive.weak_bg_fill = c(x);
        }
        if let Some(x) = self.hover_bg {
            v.widgets.hovered.bg_fill = c(x);
            v.widgets.hovered.weak_bg_fill = c(x);
        }
        if let Some(x) = self.active_bg {
            v.widgets.active.bg_fill = c(x);
            v.widgets.active.weak_bg_fill = c(x);
        }
        if let Some(x) = self.border {
            for w in [
                &mut v.widgets.noninteractive,
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
            ] {
                w.bg_stroke.color = c(x);
            }
        }
        if let Some(x) = self.weak_text {
            v.widgets.noninteractive.fg_stroke.color = c(x);
        }
        if let Some(x) = self.strong_text {
            v.widgets.active.fg_stroke.color = c(x);
        }
        v
    }

    /// Parse one of our own theme files.
    pub fn from_json(text: &str, fallback_name: &str) -> Option<Theme> {
        let cleaned = crate::keybindings::strip_comments(text);
        let v: serde_json::Value = serde_json::from_str(&cleaned).ok()?;
        // A VS Code theme is recognised by its `colors` block — accept either format from any file
        // in the themes folder, so dropping a downloaded .json in just works.
        if v.get("colors").is_some() || v.get("tokenColors").is_some() {
            return from_vscode(&v, fallback_name);
        }
        Some(Theme {
            name: v["name"].as_str().unwrap_or(fallback_name).to_string(),
            dark: v["dark"].as_bool().unwrap_or(true),
            window_bg: col(&v, "window_bg"),
            panel_bg: col(&v, "panel_bg"),
            faint_bg: col(&v, "faint_bg"),
            extreme_bg: col(&v, "extreme_bg"),
            text: col(&v, "text"),
            weak_text: col(&v, "weak_text"),
            strong_text: col(&v, "strong_text"),
            accent: col(&v, "accent"),
            accent_text: col(&v, "accent_text"),
            hover_bg: col(&v, "hover_bg"),
            active_bg: col(&v, "active_bg"),
            widget_bg: col(&v, "widget_bg"),
            border: col(&v, "border"),
            hyperlink: col(&v, "hyperlink"),
            warn: col(&v, "warn"),
            error: col(&v, "error"),
            tokens: Default::default(),
        })
    }
}

impl Theme {
    /// The colour a theme gives a TextMate `scope`, following TextMate's **most-specific-wins**
    /// rule: a theme that only defines `comment` still colours `comment.line.double-slash`, so the
    /// lookup drops trailing segments until something matches.
    pub fn scope_color(&self, scope: &str) -> Option<[u8; 4]> {
        let mut s = scope;
        loop {
            if let Some(c) = self.tokens.get(s) {
                return Some(*c);
            }
            match s.rfind('.') {
                Some(i) => s = &s[..i],
                None => return None,
            }
        }
    }

    /// The colour for one of our lexer's token kinds, via the TextMate scope it corresponds to.
    ///
    /// This is the bridge that lets an imported VS Code theme colour source text: our highlighter
    /// produces kinds, VS Code themes colour scopes, and these are the standard scope names for
    /// each kind. Falls back to `None` so the built-in palette still applies when a theme is
    /// absent or says nothing about that scope.
    pub fn kind_color(&self, kind: crate::decode::Tok) -> Option<[u8; 3]> {
        use crate::decode::Tok;
        let scopes: &[&str] = match kind {
            Tok::Comment => &["comment"],
            Tok::Keyword => &["keyword", "keyword.control"],
            Tok::Type => &["storage.type", "entity.name.type", "support.type"],
            Tok::Str => &["string", "string.quoted"],
            Tok::Number => &["constant.numeric", "constant"],
            Tok::Preproc => &["keyword.control.directive", "meta.preprocessor", "keyword"],
            Tok::Punct => &["punctuation"],
            Tok::Default => &["source", "variable"],
        };
        scopes
            .iter()
            .find_map(|s| self.scope_color(s))
            .map(|c| [c[0], c[1], c[2]])
    }
}

/// Map a VS Code theme onto ours. Curated, not mechanical: the two vocabularies don't correspond
/// one-to-one, so each field takes the most apt key with fallbacks.
pub fn from_vscode(v: &serde_json::Value, fallback_name: &str) -> Option<Theme> {
    let c = v.get("colors")?;
    let pick = |keys: &[&str]| -> Option<[u8; 4]> { keys.iter().find_map(|k| col(c, k)) };
    // `type` is "dark"/"light"; some themes only hint it in the name.
    let dark = match v["type"].as_str() {
        Some("light") => false,
        Some(_) => true,
        None => !v["name"].as_str().unwrap_or("").to_lowercase().contains("light"),
    };
    let mut tokens = std::collections::HashMap::new();
    if let Some(list) = v["tokenColors"].as_array() {
        for t in list {
            let Some(fg) = t["settings"]["foreground"].as_str().and_then(parse_hex) else {
                continue;
            };
            // `scope` is a string or a list of them.
            match &t["scope"] {
                serde_json::Value::String(s) => {
                    tokens.entry(s.clone()).or_insert(fg);
                }
                serde_json::Value::Array(a) => {
                    for s in a.iter().filter_map(|x| x.as_str()) {
                        tokens.entry(s.to_string()).or_insert(fg);
                    }
                }
                _ => {}
            }
        }
    }
    Some(Theme {
        name: v["name"].as_str().unwrap_or(fallback_name).to_string(),
        dark,
        window_bg: pick(&["editorWidget.background", "sideBar.background", "editor.background"]),
        panel_bg: pick(&["sideBar.background", "panel.background", "editor.background"]),
        faint_bg: pick(&["list.hoverBackground", "editorGroupHeader.tabsBackground"]),
        extreme_bg: pick(&["editor.background", "input.background"]),
        text: pick(&["foreground", "editor.foreground", "sideBar.foreground"]),
        weak_text: pick(&["descriptionForeground", "disabledForeground"]),
        strong_text: pick(&["editor.foreground", "foreground"]),
        accent: pick(&["list.activeSelectionBackground", "focusBorder", "button.background"]),
        accent_text: pick(&["list.activeSelectionForeground", "button.foreground"]),
        hover_bg: pick(&["list.hoverBackground", "toolbar.hoverBackground"]),
        active_bg: pick(&["list.activeSelectionBackground", "button.hoverBackground"]),
        widget_bg: pick(&["button.secondaryBackground", "input.background", "dropdown.background"]),
        border: pick(&["panel.border", "editorGroup.border", "contrastBorder"]),
        hyperlink: pick(&["textLink.foreground", "editorLink.activeForeground"]),
        warn: pick(&["editorWarning.foreground", "list.warningForeground"]),
        error: pick(&["editorError.foreground", "errorForeground"]),
        tokens,
    })
}

/// Every theme in `dir`, sorted by name. A file that won't parse is skipped, not fatal.
pub fn load_all(dir: &Path) -> Vec<Theme> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("theme");
        if let Some(t) = std::fs::read_to_string(&p).ok().and_then(|s| Theme::from_json(&s, stem)) {
            out.push(t);
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

/// A small bundled set, written into the themes folder on first run so there's something to look
/// at (and a worked example to copy). Mirrors how the PixelFX presets ship.
pub fn builtin() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "midnight",
            r##"{ "name": "Midnight", "dark": true,
  "panel_bg": "#12141a", "window_bg": "#171a21", "extreme_bg": "#0d0f14",
  "accent": "#2f6fed", "accent_text": "#ffffff", "hover_bg": "#232833",
  "border": "#2a2f3a", "hyperlink": "#6fb3ff", "weak_text": "#8a93a6" }"##,
        ),
        (
            "amber-crt",
            r##"{ "name": "Amber CRT", "dark": true,
  "panel_bg": "#140f06", "window_bg": "#1b1408", "extreme_bg": "#0b0803",
  "text": "#ffb547", "weak_text": "#a3761f", "accent": "#5a3a06",
  "accent_text": "#ffd28a", "hover_bg": "#2a1e0a", "border": "#3a2a0c",
  "hyperlink": "#ffd28a" }"##,
        ),
        (
            "paper",
            r##"{ "name": "Paper", "dark": false,
  "panel_bg": "#f4f2ec", "window_bg": "#fbfaf6", "extreme_bg": "#ffffff",
  "accent": "#2f6fed", "accent_text": "#ffffff", "hover_bg": "#e6e3da",
  "border": "#d5d1c6", "weak_text": "#6b6659" }"##,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_forms() {
        assert_eq!(parse_hex("#ff8800"), Some([255, 136, 0, 255]));
        assert_eq!(parse_hex("ff8800"), Some([255, 136, 0, 255]));
        assert_eq!(parse_hex("#f80"), Some([255, 136, 0, 255]));
        assert_eq!(parse_hex("#ff880080"), Some([255, 136, 0, 128]));
        // A mistyped colour must be None so the base theme's value survives, not black.
        assert_eq!(parse_hex("#xyz"), None);
        assert_eq!(parse_hex("#ff88"), None);
        assert_eq!(parse_hex(""), None);
    }

    #[test]
    fn reads_our_own_format() {
        let t = Theme::from_json(
            r##"{ "name": "T", "dark": false, "accent": "#112233", "border": "#445566" }"##,
            "fallback",
        )
        .unwrap();
        assert_eq!(t.name, "T");
        assert!(!t.dark);
        assert_eq!(t.accent, Some([0x11, 0x22, 0x33, 255]));
        // Unset fields stay None so the base theme fills them in.
        assert_eq!(t.text, None);
    }

    #[test]
    fn imports_a_vscode_theme() {
        // The shape a real VS Code theme file has.
        let src = r##"{
          "name": "Nightly", "type": "dark",
          "colors": {
            "editor.background": "#1e1e1e",
            "sideBar.background": "#252526",
            "foreground": "#cccccc",
            "list.activeSelectionBackground": "#094771",
            "list.activeSelectionForeground": "#ffffff",
            "list.hoverBackground": "#2a2d2e",
            "panel.border": "#3c3c3c",
            "textLink.foreground": "#3794ff",
            "errorForeground": "#f48771"
          },
          "tokenColors": [
            { "scope": "comment", "settings": { "foreground": "#6a9955" } },
            { "scope": ["string", "constant.other.symbol"], "settings": { "foreground": "#ce9178" } }
          ]
        }"##;
        let t = Theme::from_json(src, "x").unwrap();
        assert_eq!(t.name, "Nightly");
        assert!(t.dark);
        assert_eq!(t.panel_bg, parse_hex("#252526"));
        assert_eq!(t.extreme_bg, parse_hex("#1e1e1e"));
        assert_eq!(t.accent, parse_hex("#094771"));
        assert_eq!(t.hyperlink, parse_hex("#3794ff"));
        assert_eq!(t.error, parse_hex("#f48771"));
        // tokenColors: a scope list contributes every scope it names.
        assert_eq!(t.tokens.get("comment"), parse_hex("#6a9955").as_ref());
        assert_eq!(t.tokens.get("string"), parse_hex("#ce9178").as_ref());
        assert_eq!(t.tokens.get("constant.other.symbol"), parse_hex("#ce9178").as_ref());
    }

    #[test]
    fn a_light_vscode_theme_is_detected() {
        let t = Theme::from_json(
            r##"{ "name": "Day", "type": "light", "colors": { "foreground": "#333333" } }"##,
            "x",
        )
        .unwrap();
        assert!(!t.dark);
        // …and inferred from the name when `type` is absent.
        let t = Theme::from_json(
            r##"{ "name": "Solarized Light", "colors": { "foreground": "#333333" } }"##,
            "x",
        )
        .unwrap();
        assert!(!t.dark);
    }

    #[test]
    fn scope_lookup_falls_back_to_less_specific() {
        // TextMate's most-specific-wins rule: a theme defining only `comment` must still colour
        // `comment.line.double-slash`, or most real themes would leave our tokens uncoloured.
        let t = Theme::from_json(
            r##"{ "name":"T","colors":{"foreground":"#ffffff"},
                  "tokenColors":[
                    {"scope":"comment","settings":{"foreground":"#118811"}},
                    {"scope":"string.quoted.double","settings":{"foreground":"#cc4444"}}
                  ]}"##,
            "x",
        )
        .unwrap();
        assert_eq!(t.scope_color("comment"), parse_hex("#118811"));
        assert_eq!(t.scope_color("comment.line.double-slash"), parse_hex("#118811"));
        // A more specific rule is found by its own exact name.
        assert_eq!(t.scope_color("string.quoted.double"), parse_hex("#cc4444"));
        // …but a sibling the theme never mentions stays unset, so the built-in palette applies.
        assert_eq!(t.scope_color("entity.name.function"), None);
    }

    #[test]
    fn token_kinds_bind_to_theme_colours() {
        use crate::decode::Tok;
        let t = Theme::from_json(
            r##"{ "name":"T","colors":{"foreground":"#ffffff"},
                  "tokenColors":[
                    {"scope":"comment","settings":{"foreground":"#118811"}},
                    {"scope":"keyword","settings":{"foreground":"#2222ee"}},
                    {"scope":"string","settings":{"foreground":"#cc4444"}}
                  ]}"##,
            "x",
        )
        .unwrap();
        assert_eq!(t.kind_color(Tok::Comment), Some([0x11, 0x88, 0x11]));
        assert_eq!(t.kind_color(Tok::Keyword), Some([0x22, 0x22, 0xee]));
        assert_eq!(t.kind_color(Tok::Str), Some([0xcc, 0x44, 0x44]));
        // A kind the theme says nothing about returns None so the caller keeps its default.
        assert_eq!(t.kind_color(Tok::Number), None);
    }

    #[test]
    fn bad_input_is_never_fatal() {
        assert!(Theme::from_json("{ not json", "x").is_none());
        assert!(Theme::from_json("", "x").is_none());
        // Comments are allowed, like the other config files.
        assert!(Theme::from_json("// hi\n{ \"name\": \"C\" }", "x").is_some());
    }

    #[test]
    fn builtins_all_parse() {
        for (slug, body) in builtin() {
            let t = Theme::from_json(body, slug)
                .unwrap_or_else(|| panic!("builtin {slug} failed to parse"));
            assert!(!t.name.is_empty());
        }
    }
}

#[cfg(test)]
mod real_vscode {
    use super::*;
    /// Import a genuine published VS Code theme, to prove the mapping works on real files (which
    /// are JSONC — VS Code ships them with comments) and not just on hand-written fixtures.
    #[test]
    #[ignore = "reads a file fetched into /tmp"]
    fn imports_a_real_theme() {
        // Fetch with:
        //   curl -o /tmp/vsdark.json https://raw.githubusercontent.com/microsoft/vscode/main/\
        //     extensions/theme-defaults/themes/dark_modern.json
        let Ok(text) = std::fs::read_to_string("/tmp/vsdark.json") else {
            eprintln!("skipped: /tmp/vsdark.json not present");
            return;
        };
        let t = Theme::from_json(&text, "fallback").expect("parsed");
        let fields = [
            ("window_bg", t.window_bg), ("panel_bg", t.panel_bg), ("faint_bg", t.faint_bg),
            ("extreme_bg", t.extreme_bg), ("text", t.text), ("weak_text", t.weak_text),
            ("strong_text", t.strong_text), ("accent", t.accent), ("accent_text", t.accent_text),
            ("hover_bg", t.hover_bg), ("active_bg", t.active_bg), ("widget_bg", t.widget_bg),
            ("border", t.border), ("hyperlink", t.hyperlink), ("warn", t.warn), ("error", t.error),
        ];
        let got = fields.iter().filter(|(_, v)| v.is_some()).count();
        eprintln!("theme: {} | dark={} | mapped {}/{} fields | {} token scopes",
                  t.name, t.dark, got, fields.len(), t.tokens.len());
        for (n, v) in fields.iter() {
            if v.is_none() { eprintln!("   unmapped: {n}"); }
        }
        assert!(got >= 10, "most fields should map from a real theme");
        let _ = t.to_visuals();
    }
}

#[cfg(test)]
mod real_theme_binding {
    use super::*;
    /// Bind our token kinds against a real published theme, to prove the scope names we chose
    /// actually exist in themes people ship — not just in the fixtures above.
    #[test]
    #[ignore = "reads a file fetched into /tmp"]
    fn binds_kinds_against_a_real_theme() {
        let Ok(text) = std::fs::read_to_string("/tmp/vsdark.json") else {
            eprintln!("skipped: /tmp/vsdark.json not present");
            return;
        };
        let t = Theme::from_json(&text, "x").expect("parsed");
        use crate::decode::Tok::*;
        let mut bound = 0;
        for (name, k) in [("Comment", Comment), ("Keyword", Keyword), ("Type", Type),
                          ("Str", Str), ("Number", Number), ("Preproc", Preproc),
                          ("Punct", Punct), ("Default", Default)] {
            match t.kind_color(k) {
                Some(c) => { bound += 1; eprintln!("  {name:8} -> #{:02x}{:02x}{:02x}", c[0], c[1], c[2]); }
                None => eprintln!("  {name:8} -> (unset, keeps built-in)"),
            }
        }
        eprintln!("{} / 8 kinds bound from '{}'", bound, t.name);
        assert!(bound >= 4, "a real theme should colour most of our kinds");
    }
}
