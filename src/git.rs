//! Lightweight git status for the file browser.
//!
//! We shell out to the `git` CLI once per opened folder (`git status --porcelain`),
//! parse the result into a per-file status map, and cache it on the app. Surfaced in
//! the grid (a corner badge), the table (a "Git" column), the Details pane, and the
//! filename tint. Everything degrades gracefully: not a repo / no `git` on PATH /
//! any error ⇒ `None`, and the whole feature is simply inert (no badges, no column
//! content) — never an error to the user.
//!
//! We deliberately do NOT use a libgit2 binding: it's a heavy dependency for what is a
//! single `git status` read, and the CLI is what every developer already has installed
//! and trusts. The porcelain v1 format is stable and explicitly meant for scripting.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The status of one file, in rough order of "how much it wants your attention".
/// `Clean` (tracked, unmodified) is never stored in the map — an absent path that's
/// under the repo root is treated as clean — so the map only holds the interesting ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitStatus {
    Conflict, // a merge conflict (UU/AA/DD/AU/UA/DU/UD)
    Modified, // tracked + changed (staged or unstaged), or renamed/added-to-index
    New,      // untracked (??) — not yet added to git
    Ignored,  // matched by .gitignore (!!)
    Clean,    // tracked, unmodified (returned by lookup, never stored)
}

impl GitStatus {
    /// A one-word label for the Details pane / table cell.
    pub fn label(self) -> &'static str {
        match self {
            GitStatus::Conflict => "conflict",
            GitStatus::Modified => "modified",
            GitStatus::New => "new",
            GitStatus::Ignored => "ignored",
            GitStatus::Clean => "clean",
        }
    }

    /// The accent RGB for a badge / filename tint (None = clean, draw nothing special).
    pub fn color(self) -> Option<[u8; 3]> {
        match self {
            GitStatus::Conflict => Some([230, 70, 70]),  // red
            GitStatus::Modified => Some([222, 150, 40]), // orange
            GitStatus::New => Some([80, 190, 90]),       // green
            GitStatus::Ignored => Some([130, 130, 130]), // grey
            GitStatus::Clean => None,
        }
    }
}

/// The git status of one directory's repository, keyed by absolute path.
pub struct GitInfo {
    root: PathBuf,                    // repo toplevel (absolute)
    map: HashMap<PathBuf, GitStatus>, // only non-clean files
}

impl GitInfo {
    /// Look up a path's status. `None` when the path isn't inside this repo; otherwise
    /// the stored status, or `Clean` for a tracked-but-unremarkable file.
    pub fn status(&self, path: &Path) -> Option<GitStatus> {
        // Compare on absolute paths — the browser hands us absolute entry paths.
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };
        if !abs.starts_with(&self.root) {
            return None;
        }
        Some(self.map.get(&abs).copied().unwrap_or(GitStatus::Clean))
    }
}

/// Compute the git status for the repo containing `dir`. Returns `None` if `dir` isn't
/// in a git work tree, `git` isn't available, or the command fails — the caller then
/// simply shows no git information.
pub fn status_for_dir(dir: &Path) -> Option<GitInfo> {
    // 1) Find the work-tree root (also proves we're in a repo). `-C dir` runs git as if
    //    launched there, so a subfolder resolves to the same toplevel.
    let out = Command::new("git")
        .args(["-C"])
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let root = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    if root.as_os_str().is_empty() {
        return None;
    }

    // 2) One porcelain read of the whole tree. `-z` = NUL-separated (filenames with
    //    spaces/newlines survive), `--ignored=matching` lists .gitignore'd files,
    //    `--untracked-files=all` lists files inside untracked dirs individually.
    let out = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args([
            "status",
            "--porcelain=v1",
            "-z",
            "--ignored=matching",
            "--untracked-files=all",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let map = parse_porcelain_z(&out.stdout, &root);
    Some(GitInfo { root, map })
}

/// Parse `git status --porcelain=v1 -z` output into an absolute-path → status map.
///
/// Each record is `XY<space>PATH\0`, where `XY` is the two status letters (index +
/// worktree). A rename record is `XY<space>NEW\0OLD\0` (two NUL-terminated paths), so
/// after a rename code (`R`/`C`) we must consume an extra field. Everything is resolved
/// to an absolute path under `root`.
fn parse_porcelain_z(bytes: &[u8], root: &Path) -> HashMap<PathBuf, GitStatus> {
    let mut map = HashMap::new();
    let text = String::from_utf8_lossy(bytes);
    let mut fields = text.split('\0');
    while let Some(rec) = fields.next() {
        if rec.len() < 3 {
            continue; // trailing empty field after the final NUL
        }
        let code = &rec[..2];
        let path = &rec[3..]; // skip "XY "
                              // A rename/copy record carries the ORIGIN path as the next NUL field — consume it
                              // so it isn't mis-parsed as its own record.
        if code.starts_with('R') || code.starts_with('C') {
            let _origin = fields.next();
        }
        let status = classify(code);
        map.insert(root.join(path), status);
    }
    map
}

/// Map a porcelain two-letter code to our coarse status. Conflict codes win, then
/// ignored, then untracked, else anything with a non-space letter is "modified".
fn classify(code: &str) -> GitStatus {
    let b = code.as_bytes();
    let (x, y) = (b[0], b[1]);
    // Unmerged (conflict): any side is U, or the symmetric AA / DD pairs.
    if x == b'U' || y == b'U' || (x == b'A' && y == b'A') || (x == b'D' && y == b'D') {
        return GitStatus::Conflict;
    }
    if code == "!!" {
        return GitStatus::Ignored;
    }
    if code == "??" {
        return GitStatus::New;
    }
    GitStatus::Modified
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(records: &[&str]) -> HashMap<PathBuf, GitStatus> {
        // Join records with NUL and a trailing NUL, mimicking `-z` output.
        let mut buf = Vec::new();
        for r in records {
            buf.extend_from_slice(r.as_bytes());
            buf.push(0);
        }
        parse_porcelain_z(&buf, Path::new("/repo"))
    }

    #[test]
    fn classifies_the_common_codes() {
        assert_eq!(classify(" M"), GitStatus::Modified);
        assert_eq!(classify("M "), GitStatus::Modified);
        assert_eq!(classify("MM"), GitStatus::Modified);
        assert_eq!(classify("A "), GitStatus::Modified); // added to index = tracked change
        assert_eq!(classify("??"), GitStatus::New);
        assert_eq!(classify("!!"), GitStatus::Ignored);
        assert_eq!(classify("UU"), GitStatus::Conflict);
        assert_eq!(classify("AA"), GitStatus::Conflict);
        assert_eq!(classify("DD"), GitStatus::Conflict);
        assert_eq!(classify("AU"), GitStatus::Conflict);
    }

    #[test]
    fn parses_z_records_to_absolute_paths() {
        let m = build(&[" M src/app.rs", "?? new file.txt", "!! target"]);
        assert_eq!(
            m.get(Path::new("/repo/src/app.rs")),
            Some(&GitStatus::Modified)
        );
        // A filename with a space survives NUL-splitting intact.
        assert_eq!(
            m.get(Path::new("/repo/new file.txt")),
            Some(&GitStatus::New)
        );
        assert_eq!(m.get(Path::new("/repo/target")), Some(&GitStatus::Ignored));
    }

    #[test]
    fn rename_record_consumes_its_origin_field() {
        // `R  new\0old\0` — the origin `old` must NOT become its own bogus entry.
        let m = build(&["R  after.rs", "before.rs", " M other.rs"]);
        assert_eq!(
            m.get(Path::new("/repo/after.rs")),
            Some(&GitStatus::Modified)
        );
        assert_eq!(
            m.get(Path::new("/repo/other.rs")),
            Some(&GitStatus::Modified)
        );
        assert!(
            m.get(Path::new("/repo/before.rs")).is_none(),
            "origin isn't a record"
        );
        assert_eq!(m.len(), 2);
    }
}
