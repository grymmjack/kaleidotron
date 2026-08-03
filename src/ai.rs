//! AI generation plugin — shell out to a user-configured external generator (pixelmon / soundmon /
//! ansimon / a test echo), the same model as grymmjack's DRAW editor.
//!
//! pixelview never talks to a model directly. A **tool** is an external command whose argument
//! string is a **template** of `{macros}` (`{prompt}` `{seed}` `{outdir}` `{sw}` …) expanded per
//! run; the tool writes its output into a fresh `{outdir}`, and pixelview imports whatever new
//! files land there. A **style** wraps the prompt (prefix/suffix) + adds CLI flags + can lock a
//! seed; a **prompt** is a saved text preset. Everything is pure here (no egui) so it's testable.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A configured external generator (pixelmon, soundmon, ansimon, a test script…).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(default)]
pub struct AiTool {
    pub name: String, // display name, e.g. "pixelmon"
    pub exe: String,  // executable (absolute or on PATH)
    pub dir: String,  // working directory ("" = pixelview's own)
    pub args: String, // argument template, expanded by `expand`
    pub audio: bool,  // true = a sound generator (soundmon) — offered on audio pads
}

/// A style guide: text wrapped around the prompt + extra CLI flags + an optional seed lock. May be
/// scoped to one tool (a pixelmon `--style ega` means nothing to another generator).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(default)]
pub struct AiStyle {
    pub name: String,
    pub tool: String,       // "" = any tool
    pub prefix: String,     // prepended to the prompt text
    pub suffix: String,     // appended to the prompt text
    pub args_extra: String, // extra CLI args (NOT prompt text) appended to the template
    pub seed: i64,          // 0 = don't force a seed
    pub desc: String,       // shown in the picker
}

/// A saved prompt preset.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(default)]
pub struct AiPrompt {
    pub name: String,
    pub text: String,
}

/// The macro values for one run (mirrors DRAW's context). Only the practical subset the
/// bundled generators use; unknown `{macros}` expand to "".
#[derive(Clone, Debug, Default)]
pub struct AiCtx {
    pub prompt: String, // the final prompt (style prefix + user prompt + suffix)
    pub style: String,  // selected style name
    pub seed: i64,
    pub outdir: String,
    pub outname: String,
    pub iw: u32, // image / output width
    pub ih: u32, // image / output height
    pub sw: u32, // "selection" size — here the requested generation size
    pub sh: u32,
    pub gw: u32, // grid
    pub gh: u32,
    pub bw: u32, // brush
    pub bh: u32,
    pub pal: String, // palette name
    pub fg: String,  // foreground colour hex
    pub bg: String,  // background colour hex
}

impl AiCtx {
    /// Resolve one macro name (already stripped of `{}`) to its value. Handles the `:slug` /
    /// `:lower` suffixes and the all-UPPERCASE → uppercase-value rule.
    fn value(&self, raw: &str) -> String {
        // `{pal:slug}` / `{pal:lower}` modifiers.
        let (base, modifier) = match raw.split_once(':') {
            Some((b, m)) => (b, Some(m)),
            None => (raw, None),
        };
        let upper = base.chars().all(|c| !c.is_alphabetic() || c.is_uppercase())
            && base.chars().any(|c| c.is_uppercase());
        let key = base.to_ascii_lowercase();
        let mut v = match key.as_str() {
            "prompt" => self.prompt.clone(),
            "style" => self.style.clone(),
            "seed" => self.seed.to_string(),
            "outdir" => self.outdir.clone(),
            "outname" => self.outname.clone(),
            "iw" => self.iw.to_string(),
            "ih" => self.ih.to_string(),
            "sw" => self.sw.to_string(),
            "sh" => self.sh.to_string(),
            "gw" => self.gw.to_string(),
            "gh" => self.gh.to_string(),
            "bw" => self.bw.to_string(),
            "bh" => self.bh.to_string(),
            "pal" => self.pal.clone(),
            "fg" => self.fg.clone(),
            "bg" => self.bg.clone(),
            _ => String::new(),
        };
        v = v.trim().to_string();
        match modifier {
            Some("slug") => v = slug(&v),
            Some("lower") => v = v.to_ascii_lowercase(),
            _ if upper => v = v.to_ascii_uppercase(),
            _ => {}
        }
        v
    }
}

/// Lower-case, non-alphanumeric → `-`, collapsed — CLI-safe.
fn slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// Expand every `{macro}` in `template` using `ctx`. Unknown macros → "".
pub fn expand(template: &str, ctx: &AiCtx) -> String {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            let mut name = String::new();
            let mut closed = false;
            for c2 in chars.by_ref() {
                if c2 == '}' {
                    closed = true;
                    break;
                }
                name.push(c2);
            }
            if closed {
                out.push_str(&ctx.value(&name));
            } else {
                out.push('{');
                out.push_str(&name);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Split an expanded argument string into argv, honouring double quotes (so a `"{prompt}"` that
/// expanded to text-with-spaces stays one argument). Minimal shell-like tokenizer — no escapes
/// beyond quotes, which is all the generators need.
pub fn tokenize(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut has = false;
    for c in args.chars() {
        match c {
            '"' => {
                in_q = !in_q;
                has = true;
            }
            c if c.is_whitespace() && !in_q => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}

/// Run `tool` for `ctx`: create `ctx.outdir`, expand `tool.args` (+ `extra_args` from the style),
/// spawn `tool.exe` in `tool.dir`, wait, and return the NEW files that landed in `outdir` (newest
/// first, skipping `_`-prefixed and hidden files). `Err` on spawn/exit failure with the captured
/// stderr tail. Blocking — call on a worker thread.
pub fn run(
    tool: &AiTool,
    extra_args: &str,
    ctx: &AiCtx,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Vec<PathBuf>, String> {
    use std::io::Read;
    use std::sync::atomic::Ordering::Relaxed;
    std::fs::create_dir_all(&ctx.outdir)
        .map_err(|e| format!("Can't create output dir: {e}"))?;
    let before: std::collections::HashSet<PathBuf> = std::fs::read_dir(&ctx.outdir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();

    let tmpl = if extra_args.trim().is_empty() {
        tool.args.clone()
    } else {
        format!("{} {}", tool.args, extra_args)
    };
    let argv = tokenize(&expand(&tmpl, ctx));
    let mut cmd = Command::new(&tool.exe);
    cmd.args(&argv);
    if !tool.dir.trim().is_empty() {
        cmd.current_dir(tool.dir.trim());
    }
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Can't launch “{}”: {e}", tool.exe))?;
    // Poll so Ctrl+Alt+K (cancel) can kill the generator mid-run instead of waiting.
    let status = loop {
        if cancel.load(Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("cancelled".into());
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(e) => return Err(format!("Run failed: {e}")),
        }
    };
    let mut errbuf = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut errbuf);
    }
    if !status.success() {
        let tail: String = errbuf.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
        return Err(if tail.is_empty() {
            format!("Generator exited with an error (code {:?})", status.code())
        } else {
            format!("Generator error: {tail}")
        });
    }
    // New files, skipping pixelview's own `_`-prefixed / hidden ones. Newest first.
    let mut fresh: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&ctx.outdir)
        .map_err(|e| format!("Can't read output: {e}"))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && !before.contains(p))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| !n.starts_with('_') && !n.starts_with('.'))
                .unwrap_or(false)
        })
        .map(|p| {
            let t = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (t, p)
        })
        .collect();
    fresh.sort_by(|a, b| b.0.cmp(&a.0));
    if fresh.is_empty() {
        return Err("The generator produced no new files".into());
    }
    Ok(fresh.into_iter().map(|(_, p)| p).collect())
}

/// Is `exe` runnable (on PATH or an existing absolute path)?
pub fn tool_available(tool: &AiTool) -> bool {
    let exe = tool.exe.trim();
    if exe.is_empty() {
        return false;
    }
    if Path::new(exe).is_absolute() {
        return Path::new(exe).exists();
    }
    // On PATH: try `which`-like resolution.
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|d| d.join(exe).exists())
        })
        .unwrap_or(false)
}

/// Starter tools seeded on first enable: a test echo + pixelmon (images) + soundmon (audio) +
/// ansimon (ANSI), pointed at the sibling repos grymmjack keeps next to pixel-viewer. Paths that
/// don't exist are harmless — the user edits them in the AI tab.
pub fn starter_tools(home: &Path) -> Vec<AiTool> {
    let img_args =
        "\"{prompt}\" --size {sw}x{sh} --seed {seed} --output-to {outdir} --name {outname}"
            .to_string();
    vec![
        AiTool {
            name: "echo (test)".into(),
            exe: "bash".into(),
            dir: String::new(),
            args: format!(
                "{} \"{{prompt}}\" --size {{sw}}x{{sh}} --seed {{seed}} --output-to {{outdir}} --name {{outname}} --delay 1",
                home.join("git/DRAW/DEV/ai-echo.sh").display()
            ),
            audio: false,
        },
        AiTool {
            name: "pixelmon".into(),
            exe: home.join("git/pixelmon/bin/pixelmon").to_string_lossy().into_owned(),
            dir: home.join("git/pixelmon").to_string_lossy().into_owned(),
            args: img_args.clone(),
            audio: false,
        },
        AiTool {
            name: "soundmon".into(),
            exe: home.join("git/soundmon/bin/soundmon").to_string_lossy().into_owned(),
            dir: home.join("git/soundmon").to_string_lossy().into_owned(),
            args: img_args.clone(),
            audio: true,
        },
        AiTool {
            name: "ansimon".into(),
            exe: home.join("git/ansimon/bin/ansimon").to_string_lossy().into_owned(),
            dir: home.join("git/ansimon").to_string_lossy().into_owned(),
            args: img_args,
            audio: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> AiCtx {
        AiCtx {
            prompt: "a skull".into(),
            style: "DOS EGA".into(),
            seed: 42,
            outdir: "/tmp/out".into(),
            outname: "draw-ai".into(),
            sw: 320,
            sh: 200,
            pal: "ANSI 32".into(),
            ..Default::default()
        }
    }

    #[test]
    fn expands_macros_and_modifiers() {
        let c = ctx();
        assert_eq!(
            expand("\"{prompt}\" --size {sw}x{sh} --seed {seed}", &c),
            "\"a skull\" --size 320x200 --seed 42"
        );
        assert_eq!(expand("{PAL}", &c), "ANSI 32"); // uppercase name → uppercase value
        assert_eq!(expand("{pal:slug}", &c), "ansi-32");
        assert_eq!(expand("{unknown}", &c), ""); // unknown → empty
        assert_eq!(expand("{outdir}/{outname}", &c), "/tmp/out/draw-ai");
    }

    #[test]
    fn tokenize_honours_quotes() {
        assert_eq!(
            tokenize("\"a skull\" --size 320x200 --seed 42"),
            vec!["a skull", "--size", "320x200", "--seed", "42"]
        );
        assert_eq!(tokenize("  --a   --b  "), vec!["--a", "--b"]);
    }
}
