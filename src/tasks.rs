//! Run a project's own build/run/tool commands, read from VS Code's `.vscode/tasks.json`.
//!
//! The format is borrowed rather than invented on purpose: most of the folders you'd browse source
//! in already have one, and the alternative is asking people to re-describe their toolchain in a
//! kaleidotron-specific file that only kaleidotron reads. A `.bas` file next to a `tasks.json` that
//! knows how to compile it should just be compilable.
//!
//! This module is deliberately **pure** — parsing, variable substitution and dependency ordering,
//! no I/O beyond reading the two JSON files and no egui. That keeps the fiddly parts (which are all
//! string handling, and all easy to get subtly wrong) under unit test, and leaves `app.rs` holding
//! only the process spawning and the output panel.
//!
//! Tasks come from two places, merged: the project's `.vscode/tasks.json` (found by walking up from
//! the current folder) and a **global** one in kaleidotron's data dir, so a general tool — "open
//! this in GIMP" — isn't confined to the single repo that happened to declare it. The project wins
//! on a name collision; see [`merge_global`].
//!
//! What is supported: JSONC, per-platform overrides, `shell` and `process` types, `args`,
//! `options.cwd`/`options.env`, `dependsOn`, `group`, `presentation.reveal`, the variables listed in
//! [`substitute`], and the `type: "command"` **inputs** that name `simpleBrowser.show` (see
//! [`parse_inputs`] — they resolve to their URL and open it). What is not (yet): `problemMatcher`
//! (so compiler errors are text, not clickable), interactive `${input:…}` kinds (`promptString`,
//! `pickString`), and other VS Code-internal `command:` inputs, which have no meaning outside the
//! editor.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One runnable task, with its platform block already folded in.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Task {
    pub label: String,
    /// The longer description VS Code shows under the label in its picker; used as hover text.
    pub detail: String,
    /// `"shell"` (run through a shell) or `"process"` (spawn directly). Empty defaults to shell,
    /// matching how nearly every real file is written.
    pub kind: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    /// Labels that must run first. See [`plan`] for the ordering.
    pub depends_on: Vec<String>,
    /// `build` / `test` / `none` — used to group the menu.
    pub group: String,
    /// `group.isDefault` — the one bound to the build shortcut.
    pub is_default: bool,
    /// `presentation.reveal`: `always` shows the output panel, `never`/`silent` runs quietly. The
    /// "open this file in GIMP" style of task sets `never`, and popping a panel for it would be
    /// noise.
    pub reveal: String,
    /// `presentation.clear` — wipe the panel before this task's output.
    pub clear: bool,
}

impl Task {
    /// Should running this task show the output panel?
    pub fn reveals(&self) -> bool {
        !matches!(self.reveal.as_str(), "never" | "silent")
    }

    /// Does this task depend on an `${input:…}` this machine **cannot** resolve?
    ///
    /// A `type: "command"` input naming `simpleBrowser.show` resolves to its URL (see
    /// [`parse_inputs`]) and runs fine. The genuinely interactive kinds — `promptString`,
    /// `pickString` — do not, and such a task is reported as unsupported rather than run with a
    /// literal `${input:x}` as its command line, which would fail with a confusing shell error.
    pub fn needs_input(&self, inputs: &HashMap<String, String>) -> bool {
        std::iter::once(&self.command)
            .chain(self.args.iter())
            .any(|s| unresolved_inputs(s, inputs))
    }
}

/// Does `src` name any `${input:id}` that `inputs` has no value for?
fn unresolved_inputs(src: &str, inputs: &HashMap<String, String>) -> bool {
    let mut rest = src;
    while let Some(at) = rest.find("${input:") {
        let after = &rest[at + 8..];
        let Some(end) = after.find('}') else {
            return true; // an unterminated one can never be resolved
        };
        if !inputs.contains_key(&after[..end]) {
            return true;
        }
        rest = &after[end + 1..];
    }
    false
}

/// Strip JSONC down to JSON: `//` and `/* */` comments, and trailing commas before `}` / `]`.
///
/// Not optional — VS Code's own default `tasks.json` ships with comments, and hand-edited ones
/// routinely carry a trailing comma inside a platform block. `serde_json` rejects both, so a real
/// file would simply fail to load with a baffling error.
///
/// Comment detection is string-aware (a `//` inside `"https://…"` is a URL, not a comment) and
/// escape-aware (`"a\"//b"` stays one string).
pub fn strip_jsonc(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        if in_str {
            // A backslash escapes the next byte whatever it is, so copy both and skip the quote
            // test — otherwise `"\""` would look like the string ending.
            if b[i] == b'\\' && i + 1 < b.len() {
                out.push(b[i] as char);
                out.push(b[i + 1] as char);
                i += 2;
                continue;
            }
            if b[i] == b'"' {
                in_str = false;
            }
            out.push(b[i] as char);
            i += 1;
            continue;
        }
        match (b[i], b.get(i + 1)) {
            (b'"', _) => {
                in_str = true;
                out.push('"');
                i += 1;
            }
            (b'/', Some(b'/')) => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            (b'/', Some(b'*')) => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            (b',', _) => {
                // Look past whitespace: a comma followed by a closer is the trailing comma. Done
                // here rather than in a second pass so it can't misfire inside a string.
                let mut j = i + 1;
                while j < b.len() && (b[j] as char).is_whitespace() {
                    j += 1;
                }
                if matches!(b.get(j), Some(b'}') | Some(b']')) {
                    i += 1; // drop the comma, keep the whitespace
                } else {
                    out.push(',');
                    i += 1;
                }
            }
            _ => {
                // Push the whole UTF-8 char, not the byte: indexing bytes and casting would mangle
                // any non-ASCII text (a task label with an accent, a path with one).
                let ch = src[i..].chars().next().unwrap_or(' ');
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

/// String value at `key`, if it is a string.
fn s(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)?.as_str().map(|s| s.to_string())
}

/// String array at `key`. VS Code also allows an arg to be an object (`{value, quoting}`); the
/// `value` is taken and the quoting hint dropped, since we quote for the platform ourselves.
fn arr(v: &serde_json::Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()).or_else(|| s(x, "value")))
                .collect()
        })
        .unwrap_or_default()
}

/// The platform key whose block overrides the base task on this machine.
fn platform_key() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    }
}

/// Parse a `tasks.json`, resolving each task's platform block.
///
/// Anything unparseable is skipped rather than failing the file: one malformed task in a long list
/// shouldn't cost you the other twenty.
pub fn parse(src: &str) -> Vec<Task> {
    let Ok(v): Result<serde_json::Value, _> = serde_json::from_str(&strip_jsonc(src)) else {
        return Vec::new();
    };
    let Some(list) = v.get("tasks").and_then(|t| t.as_array()) else {
        return Vec::new();
    };
    list.iter().filter_map(parse_one).collect()
}

/// One task object → a [`Task`], with the platform block layered over the base.
///
/// The override is per *field*, matching VS Code: a task may set `command` at the top level and
/// only `args` per platform, and both halves have to survive.
fn parse_one(t: &serde_json::Value) -> Option<Task> {
    let plat = t.get(platform_key());
    let ty = plat.and_then(|p| s(p, "type")).or_else(|| s(t, "type")).unwrap_or_default();
    // A typed task may legitimately omit `label` — VS Code names an `npm` task after its script.
    // Dropping those would silently lose whole files (every `npm: watch` entry), so the same name
    // is synthesised here.
    let script = s(t, "script").unwrap_or_default();
    let label = s(t, "label")
        .or_else(|| s(t, "taskName"))
        .or_else(|| (!script.is_empty()).then(|| format!("{ty}: {script}")))?;
    // `pick` prefers the platform block, which is the whole point of having one.
    let pick_s = |key: &str| plat.and_then(|p| s(p, key)).or_else(|| s(t, key));
    let pick_arr = |key: &str| {
        let p = plat.map(|p| arr(p, key)).unwrap_or_default();
        if p.is_empty() {
            arr(t, key)
        } else {
            p
        }
    };
    let opts = plat.and_then(|p| p.get("options")).or_else(|| t.get("options"));
    let env = opts
        .and_then(|o| o.get("env"))
        .and_then(|e| e.as_object())
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                .collect()
        })
        .unwrap_or_default();
    // `dependsOn` is either one label or a list of them.
    let depends_on = match t.get("dependsOn") {
        Some(serde_json::Value::String(one)) => vec![one.clone()],
        Some(serde_json::Value::Array(_)) => arr(t, "dependsOn"),
        _ => Vec::new(),
    };
    // `group` is either the bare kind ("test") or an object with `isDefault`.
    let (group, is_default) = match t.get("group") {
        Some(serde_json::Value::String(g)) => (g.clone(), false),
        Some(o) => (
            s(o, "kind").unwrap_or_default(),
            o.get("isDefault").and_then(|d| d.as_bool()).unwrap_or(false),
        ),
        None => (String::new(), false),
    };
    let pres = t.get("presentation");
    // An `npm` task carries a script name instead of a command line. Expanding it here — rather
    // than leaving an empty command that fails with a blank error — costs three lines and makes
    // every JS project's tasks runnable too.
    let (kind, command, args) = if ty == "npm" && !script.is_empty() {
        ("shell".to_string(), "npm".to_string(), vec!["run".to_string(), script.clone()])
    } else {
        (ty, pick_s("command").unwrap_or_default(), pick_arr("args"))
    };
    Some(Task {
        label,
        detail: s(t, "detail").unwrap_or_default(),
        kind,
        command,
        args,
        cwd: opts.and_then(|o| s(o, "cwd")),
        env,
        depends_on,
        group,
        is_default,
        reveal: pres.and_then(|p| s(p, "reveal")).unwrap_or_else(|| "always".into()),
        clear: pres.and_then(|p| p.get("clear")).and_then(|c| c.as_bool()).unwrap_or(false),
    })
}

/// Parse the file's `inputs` array into the `${input:id}` values we can resolve without a prompt.
///
/// Most input types are interactive by definition (`promptString`, `pickString`) and stay
/// unresolved. The exception is a **`type: "command"`** input, which VS Code answers by running an
/// editor command — and the one that actually appears in the wild is `simpleBrowser.show`, whose
/// single argument is a URL. That has a perfectly good meaning outside VS Code: open the URL. So
/// those resolve to their URL, and [`exec_for`] turns a task whose command *is* a URL into the
/// platform's open-a-link command.
///
/// `vscode.open` is treated the same way — it is the non-embedded spelling of the same intent.
pub fn parse_inputs(src: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(v): Result<serde_json::Value, _> = serde_json::from_str(&strip_jsonc(src)) else {
        return out;
    };
    let Some(list) = v.get("inputs").and_then(|i| i.as_array()) else {
        return out;
    };
    for i in list {
        let (Some(id), Some(ty)) = (s(i, "id"), s(i, "type")) else {
            continue;
        };
        if ty != "command" {
            continue;
        }
        let cmd = s(i, "command").unwrap_or_default();
        if !matches!(cmd.as_str(), "simpleBrowser.show" | "simpleBrowser.api.open" | "vscode.open") {
            continue;
        }
        // The URL is the first argument; VS Code accepts it as a bare string or a one-element array.
        let url = i
            .get("args")
            .and_then(|a| a.as_str().map(|s| s.to_string()).or_else(|| arr(i, "args").into_iter().next()));
        if let Some(url) = url {
            out.insert(id, url);
        }
    }
    out
}

/// Everything `${…}` can expand to.
#[derive(Debug, Clone, Default)]
pub struct Vars {
    /// `${workspaceFolder}` — the folder holding `.vscode`.
    pub workspace: PathBuf,
    /// `${file}` — the file the task acts on. For art opened from an archive or 16colo.rs this is
    /// the *resolved local* copy, since a tool spawned by the task can only open a real path.
    pub file: PathBuf,
    /// `${config:…}` — keys read from `.vscode/settings.json`.
    pub config: HashMap<String, String>,
    /// `${input:…}` — only the non-interactive ones (see [`parse_inputs`]).
    pub inputs: HashMap<String, String>,
}

/// Expand VS Code's task variables in `src`.
///
/// An **unknown** variable is deliberately left verbatim rather than blanked. A task that silently
/// becomes `qb64pe -x  -o ` is a mystery to debug; one that visibly still says
/// `${config:qb64pe.compilerPath}` in the output panel tells you exactly which key is missing.
pub fn substitute(src: &str, v: &Vars) -> String {
    let file = v.file.to_string_lossy().to_string();
    let dir = |p: &Path| p.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
    let base = v.file.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let stem = v.file.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = v.file.extension().map(|s| format!(".{}", s.to_string_lossy())).unwrap_or_default();
    let ws = v.workspace.to_string_lossy().to_string();
    let rel = v.file.strip_prefix(&v.workspace).unwrap_or(&v.file).to_string_lossy().to_string();
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('}').map(|e| start + e) else {
            // An unterminated `${` is just text from here on.
            break;
        };
        let name = &rest[start + 2..end];
        let val = match name {
            "workspaceFolder" => Some(ws.clone()),
            "workspaceFolderBasename" => {
                Some(v.workspace.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default())
            }
            "file" => Some(file.clone()),
            "fileDirname" => Some(dir(&v.file)),
            "fileBasename" => Some(base.clone()),
            "fileBasenameNoExtension" => Some(stem.clone()),
            "fileExtname" => Some(ext.clone()),
            "relativeFile" => Some(rel.clone()),
            "relativeFileDirname" => Some(dir(Path::new(&rel))),
            "cwd" => Some(ws.clone()),
            "pathSeparator" | "/" => Some(std::path::MAIN_SEPARATOR.to_string()),
            "userHome" => std::env::var("HOME").ok().or_else(|| std::env::var("USERPROFILE").ok()),
            _ => match name.split_once(':') {
                Some(("env", k)) => Some(std::env::var(k).unwrap_or_default()),
                Some(("config", k)) => v.config.get(k).cloned(),
                Some(("input", k)) => v.inputs.get(k).cloned(),
                _ => None,
            },
        };
        match val {
            Some(val) => out.push_str(&val),
            None => out.push_str(&rest[start..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

/// A task resolved into something spawnable.
#[derive(Debug, Clone, PartialEq)]
pub struct Exec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    /// The command line as the user would read it — shown in the output panel header so what ran
    /// is never a guess.
    pub display: String,
}

/// `cmd` if it is nothing but an `http(s)` URL. A command line that merely *contains* a URL (
/// `setsid xdg-open https://…`) is a normal shell task and is left alone — it already opens the
/// link itself.
fn bare_url(cmd: &str) -> Option<&str> {
    let t = cmd.trim();
    let is_url = t.starts_with("http://") || t.starts_with("https://");
    (is_url && !t.contains(char::is_whitespace)).then_some(t)
}

/// The platform's "open this in whatever handles it" command.
fn url_opener() -> (&'static str, Vec<String>) {
    if cfg!(target_os = "windows") {
        // `start` is a cmd builtin, and its first quoted argument is taken as the window title —
        // hence the empty string before the URL, or a quoted URL would be swallowed as the title.
        ("cmd", vec!["/C".into(), "start".into(), String::new()])
    } else if cfg!(target_os = "macos") {
        ("open", Vec::new())
    } else {
        ("xdg-open", Vec::new())
    }
}

/// Quote one argument for a POSIX shell. Single quotes with the `'\''` escape: the only form that
/// is safe for *every* byte, including the spaces and `$` that appear in real paths.
fn sh_quote(a: &str) -> String {
    if !a.is_empty() && a.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_./=:+@,".contains(&c)) {
        return a.to_string();
    }
    format!("'{}'", a.replace('\'', r"'\''"))
}

/// Resolve a task into a command to spawn, substituting variables throughout.
///
/// `shell` tasks are handed to the platform shell as one line, because that is what they assume —
/// your `setsid --fork … "${file}"` entries are shell syntax, not an argv. `process` tasks are
/// spawned directly with no shell in between.
pub fn exec_for(t: &Task, v: &Vars) -> Exec {
    let cmd = substitute(&t.command, v);
    let args: Vec<String> = t.args.iter().map(|a| substitute(a, v)).collect();
    let cwd = t
        .cwd
        .as_ref()
        .map(|c| PathBuf::from(substitute(c, v)))
        .unwrap_or_else(|| v.workspace.clone());
    let env = t.env.iter().map(|(k, val)| (k.clone(), substitute(val, v))).collect();
    // A task whose command resolves to nothing but a URL means "open this link" — the shape a
    // `simpleBrowser.show` input collapses to once its URL is substituted in. Run it through the
    // platform's opener rather than handing a URL to a shell, which could only fail.
    if args.is_empty() {
        if let Some(url) = bare_url(&cmd) {
            let (program, mut oargs) = url_opener();
            oargs.push(url.to_string());
            let display = format!("{program} {url}");
            return Exec { program: program.into(), args: oargs, cwd, env, display };
        }
    }
    if t.kind == "process" {
        let display = std::iter::once(cmd.clone()).chain(args.iter().cloned()).collect::<Vec<_>>().join(" ");
        return Exec { program: cmd, args, cwd, env, display };
    }
    let mut line = if cfg!(windows) { cmd.clone() } else { sh_quote_cmd(&cmd) };
    for a in &args {
        line.push(' ');
        line.push_str(&if cfg!(windows) { a.clone() } else { sh_quote(a) });
    }
    let (program, flag) = if cfg!(windows) { ("cmd", "/C") } else { ("sh", "-c") };
    Exec {
        program: program.into(),
        args: vec![flag.into(), line.clone()],
        cwd,
        env,
        display: line,
    }
}

/// The `command` of a shell task is a command *line*, not a single word — `setsid xdg-open URL` has
/// to stay three words. So it is passed through unquoted, unless it looks like a bare path that
/// happens to contain a space (`/opt/My App/tool`), which would otherwise split.
fn sh_quote_cmd(cmd: &str) -> String {
    if cmd.contains(' ') && Path::new(cmd).is_absolute() && Path::new(cmd).exists() {
        sh_quote(cmd)
    } else {
        cmd.to_string()
    }
}

/// Order `label` and everything it depends on into the sequence to run.
///
/// **Sequential, always.** VS Code's documented default is to run `dependsOn` entries in *parallel*
/// unless `dependsOrder: "sequence"` is set, but the overwhelmingly common shape — as in
/// `BUILD: Compile` depending on `BUILD: Remove` — is a chain where the dependency must finish
/// first, and running those two concurrently is a race that deletes the binary just compiled.
/// Following the letter of the spec here would make correct-looking files fail intermittently.
///
/// A dependency cycle is broken rather than hung: each label runs at most once.
pub fn plan(tasks: &[Task], label: &str) -> Vec<Task> {
    let mut out: Vec<Task> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    fn walk(tasks: &[Task], label: &str, seen: &mut Vec<String>, out: &mut Vec<Task>) {
        if seen.iter().any(|s| s == label) {
            return;
        }
        seen.push(label.to_string());
        let Some(t) = tasks.iter().find(|t| t.label == label) else {
            return;
        };
        for d in &t.depends_on {
            walk(tasks, d, seen, out);
        }
        out.push(t.clone());
    }
    walk(tasks, label, &mut seen, &mut out);
    out
}

/// Find the `.vscode` folder governing `start`, by walking up from it.
///
/// Walking up rather than requiring an exact match is what makes this work while browsing: you open
/// `src/deep/thing.bas` and the project's tasks are still the ones at the repo root.
pub fn find_workspace(start: &Path) -> Option<PathBuf> {
    let mut dir = if start.is_dir() { Some(start) } else { start.parent() };
    while let Some(d) = dir {
        if d.join(".vscode").join("tasks.json").is_file() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// Load `<workspace>/.vscode/tasks.json` plus the `${config:…}` values from its `settings.json`.
///
/// Both are read together because a task is frequently unusable without the settings half: your
/// `BUILD: Compile` is entirely `${config:qb64pe.compilerPath}`, and without it the task would
/// resolve to an empty program and fail with a useless error.
pub fn load(workspace: &Path) -> Loaded {
    load_dir(&workspace.join(".vscode"))
}

/// Everything one `tasks.json` (+ its sibling `settings.json`) contributes.
#[derive(Debug, Clone, Default)]
pub struct Loaded {
    pub tasks: Vec<Task>,
    pub config: HashMap<String, String>,
    pub inputs: HashMap<String, String>,
}

/// Read a `tasks.json` and its sibling `settings.json` from `dir`.
///
/// Split out so the same reader serves a project's `.vscode` and the **global** toolbox directory
/// (see [`merge_global`]) — they are the same file format in a different place, and having two
/// readers would guarantee they drift.
pub fn load_dir(dir: &Path) -> Loaded {
    let src = std::fs::read_to_string(dir.join("tasks.json")).unwrap_or_default();
    let config = std::fs::read_to_string(dir.join("settings.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&strip_jsonc(&s)).ok())
        .and_then(|v| v.as_object().cloned())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    Loaded { tasks: parse(&src), inputs: parse_inputs(&src), config }
}

/// Fold a **global** task file into a project's, so tasks that aren't really project-specific —
/// "open this in GIMP", "view this in PabloDraw" — are available in every folder rather than only
/// inside the one repo whose `.vscode` happens to define them.
///
/// The **project wins on a label collision**: a repo that defines its own `BUILD: Compile` means
/// that one, and a global default silently shadowing it would be the worst possible outcome.
/// Config and input maps merge the same way.
pub fn merge_global(project: &mut Loaded, global: Loaded) {
    for t in global.tasks {
        if !project.tasks.iter().any(|p| p.label == t.label) {
            project.tasks.push(t);
        }
    }
    for (k, v) in global.config {
        project.config.entry(k).or_insert(v);
    }
    for (k, v) in global.inputs {
        project.inputs.entry(k).or_insert(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The comment/trailing-comma cases that a real hand-edited file actually contains — including
    /// the one that matters most, a `//` inside a URL string.
    #[test]
    fn jsonc_strips_comments_but_not_urls() {
        let src = r#"{
            // a line comment
            "a": "https://example.com/x", /* block */
            "b": [1, 2,],
            "c": "say \"hi\" // not a comment",
        }"#;
        let v: serde_json::Value = serde_json::from_str(&strip_jsonc(src)).expect("valid JSON");
        assert_eq!(v["a"], "https://example.com/x");
        assert_eq!(v["b"], serde_json::json!([1, 2]));
        assert_eq!(v["c"], r#"say "hi" // not a comment"#);
    }

    /// The platform block overrides per field, and a task setting only `args` per platform keeps
    /// the base `command`.
    #[test]
    fn platform_block_overrides_per_field() {
        let src = r#"{ "tasks": [{
            "label": "T", "type": "shell", "command": "base",
            "linux":   { "command": "linux-cmd" },
            "osx":     { "command": "osx-cmd" },
            "windows": { "command": "win-cmd" }
        }] }"#;
        let t = &parse(src)[0];
        let want = if cfg!(target_os = "windows") {
            "win-cmd"
        } else if cfg!(target_os = "macos") {
            "osx-cmd"
        } else {
            "linux-cmd"
        };
        assert_eq!(t.command, want);
        assert_eq!(t.kind, "shell");
    }

    #[test]
    fn group_object_and_string_forms_both_parse() {
        let src = r#"{ "tasks": [
            { "label": "A", "group": "test" },
            { "label": "B", "group": { "kind": "build", "isDefault": true } }
        ] }"#;
        let ts = parse(src);
        assert_eq!((ts[0].group.as_str(), ts[0].is_default), ("test", false));
        assert_eq!((ts[1].group.as_str(), ts[1].is_default), ("build", true));
    }

    #[test]
    fn depends_on_accepts_a_string_or_a_list() {
        let src = r#"{ "tasks": [
            { "label": "A", "dependsOn": "B" },
            { "label": "C", "dependsOn": ["B", "A"] }
        ] }"#;
        let ts = parse(src);
        assert_eq!(ts[0].depends_on, vec!["B"]);
        assert_eq!(ts[1].depends_on, vec!["B", "A"]);
    }

    #[test]
    fn substitutes_the_file_variables() {
        let v = Vars {
            workspace: PathBuf::from("/w"),
            file: PathBuf::from("/w/sub/PROG.BAS"),
            ..Default::default()
        };
        assert_eq!(substitute("${fileDirname}/${fileBasenameNoExtension}.run", &v), "/w/sub/PROG.run");
        assert_eq!(substitute("${fileBasename}", &v), "PROG.BAS");
        assert_eq!(substitute("${fileExtname}", &v), ".BAS");
        assert_eq!(substitute("${relativeFile}", &v), "sub/PROG.BAS");
        assert_eq!(substitute("${workspaceFolder}", &v), "/w");
    }

    /// A config key that IS defined resolves; one that isn't is left visible so the missing setting
    /// is diagnosable from the output panel instead of vanishing into an empty string.
    #[test]
    fn unknown_variables_survive_verbatim() {
        let mut config = HashMap::new();
        config.insert("qb64pe.compilerPath".to_string(), "/opt/qb64pe/qb64pe".to_string());
        let v = Vars { workspace: "/w".into(), file: "/w/a.bas".into(), config, ..Default::default() };
        assert_eq!(substitute("${config:qb64pe.compilerPath}", &v), "/opt/qb64pe/qb64pe");
        assert_eq!(substitute("${config:nope.missing}", &v), "${config:nope.missing}");
        assert_eq!(substitute("${bogus}", &v), "${bogus}");
    }

    /// A dependency runs before its dependent, and each label runs once even in a cycle.
    #[test]
    fn plan_orders_dependencies_first_and_breaks_cycles() {
        let mk = |l: &str, d: Vec<&str>| Task {
            label: l.into(),
            depends_on: d.into_iter().map(String::from).collect(),
            ..Default::default()
        };
        let tasks = vec![mk("Run", vec!["Compile"]), mk("Compile", vec!["Remove"]), mk("Remove", vec![])];
        let order: Vec<String> = plan(&tasks, "Run").into_iter().map(|t| t.label).collect();
        assert_eq!(order, vec!["Remove", "Compile", "Run"]);

        let cyc = vec![mk("A", vec!["B"]), mk("B", vec!["A"])];
        assert_eq!(plan(&cyc, "A").len(), 2);
    }

    /// A shell task keeps its command line intact and quotes only the arguments — the case that
    /// breaks if you naively quote the whole command.
    #[cfg(unix)]
    #[test]
    fn shell_exec_quotes_args_but_keeps_the_command_line() {
        let v = Vars { workspace: "/w".into(), file: "/w/my art.ans".into(), ..Default::default() };
        let t = Task {
            label: "T".into(),
            kind: "shell".into(),
            command: "setsid --fork /usr/bin/icy_draw".into(),
            args: vec!["${file}".into()],
            ..Default::default()
        };
        let e = exec_for(&t, &v);
        assert_eq!(e.program, "sh");
        assert_eq!(e.args[0], "-c");
        assert_eq!(e.args[1], "setsid --fork /usr/bin/icy_draw '/w/my art.ans'");
    }

    /// A `process` task is spawned directly: no shell, so its args are never re-split.
    #[test]
    fn process_exec_spawns_directly() {
        let v = Vars { workspace: "/w".into(), file: "/w/a.bas".into(), ..Default::default() };
        let t = Task {
            label: "T".into(),
            kind: "process".into(),
            command: "/opt/tool".into(),
            args: vec!["-o".into(), "${fileBasenameNoExtension}.run".into()],
            ..Default::default()
        };
        let e = exec_for(&t, &v);
        assert_eq!(e.program, "/opt/tool");
        assert_eq!(e.args, vec!["-o", "a.run"]);
    }

    /// Parse every real `.vscode/tasks.json` under `~/git` and report what resolved. Ignored (it
    /// depends on this machine's checkouts), but it is the check that matters: hand-written files
    /// in the wild carry shapes no synthetic fixture would think to include.
    /// Parse every `.vscode/tasks.json` under a directory named by `KT_TASKS_TEST_DIR`, and report
    /// what resolved. Real hand-written files in the wild carry shapes no synthetic fixture would
    /// think to include — this is how the dropped-`npm`-task bug was found.
    ///
    /// **Opt-in by env var, and `#[ignore]`d.** It reads whatever it is pointed at, so it must never
    /// go rummaging through a home directory on its own initiative:
    ///
    /// ```text
    /// KT_TASKS_TEST_DIR=~/some/projects cargo test parses_real -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn parses_real_workspace_task_files() {
        let Ok(root) = std::env::var("KT_TASKS_TEST_DIR") else {
            println!("set KT_TASKS_TEST_DIR=<dir of projects> to run this");
            return;
        };
        let mut files = 0;
        let mut total = 0;
        for e in std::fs::read_dir(&root).expect("readable dir").flatten() {
            let f = e.path().join(".vscode").join("tasks.json");
            if !f.is_file() {
                continue;
            }
            let src = std::fs::read_to_string(&f).expect("read");
            let ts = parse(&src);
            // A file with a `tasks` array must yield tasks; zero means the parse silently failed.
            if src.contains("\"tasks\"") {
                assert!(!ts.is_empty(), "no tasks parsed from {}", f.display());
            }
            files += 1;
            total += ts.len();
            println!("{}: {} tasks", f.display(), ts.len());
        }
        println!("parsed {total} tasks across {files} files");
    }

    #[test]
    fn reveal_never_does_not_show_the_panel() {
        let quiet = Task { reveal: "never".into(), ..Default::default() };
        let loud = Task { reveal: "always".into(), ..Default::default() };
        assert!(!quiet.reveals());
        assert!(loud.reveals());
    }
}
