mod amiga_fonts;
mod anim;
mod ai;
mod app;
mod archive;
mod audiosearch;
mod cache;
mod colo_thumb;
mod decode;
mod dls;
mod format_color;
mod gfonts;
mod git;
mod httpfs;
mod image_types;
mod keybindings;
mod secrets;
mod settings;
mod libxmp;
mod modarchive;
mod netpolicy;
mod palettes_builtin;
mod polyhaven;
mod rating;
mod ratings;
mod sauce;
mod scale;
mod sfz;
mod sixteen;
mod soundfont;
mod steam;
mod tasks;
mod theme;
mod thumb;
mod video;
mod imgsearch;
mod assetsearch;
mod lospec;
mod viewdb;
mod xi;
mod youtube;

use eframe::egui;

fn main() -> Result<(), eframe::Error> {
    let cli = app::CliArgs::parse();

    // Headless TEXTMODE batch (`--batch`): apply a PixelFX preset to images and dump
    // .ans/.xb/.tnd, then exit — no window (runs over SSH / in a script).
    if cli.is_batch() {
        std::process::exit(app::run_batch(&cli));
    }

    // Headless render-to-file mode (`--render`): convert art to image files and exit,
    // without ever opening a window. Runs fine over SSH / in a batch script.
    if cli.is_render() {
        std::process::exit(app::run_render(&cli));
    }

    // The effective profile directory: `--data-dir DIR` overrides the default location
    // (~/.local/share/kaleidotron), so a build can be tested from a clean slate.
    let data_dir = cli
        .data_dir
        .clone()
        .or_else(|| eframe::storage_dir("kaleidotron"));

    // `--restore`: move the backup back over the current profile, then exit (maintenance op).
    if cli.restore {
        match &data_dir {
            Some(dir) => restore_profile(dir),
            None => eprintln!("kaleidotron: --restore: could not resolve the data dir"),
        }
        return Ok(());
    }
    // `--reset`: back the current profile up once (never clobbering an existing backup) and
    // start fresh. Restore later with `--restore`.
    if cli.reset {
        if let Some(dir) = &data_dir {
            reset_profile(dir);
        }
    }

    // Carry a pixelview install's data over before eframe reads its storage — but NOT when a
    // custom/reset profile is in play, whose whole point is a clean slate.
    if cli.data_dir.is_none() && !cli.reset {
        migrate_from_pixelview();
    }

    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([640.0, 420.0])
            .with_title("kaleidotron")
            // Wayland compositors key the task-switcher icon off the app_id (matched
            // against a kaleidotron.desktop). `with_icon` covers X11 / other backends.
            .with_app_id("kaleidotron")
            .with_icon(app_icon()),
        ..Default::default()
    };
    // Point eframe's own persistence (app.ron) at the custom dir too, so it agrees with the
    // app's data_dir (which reads `cli.data_dir`). Default = eframe's own storage location.
    if let Some(dir) = &cli.data_dir {
        let _ = std::fs::create_dir_all(dir);
        options.persistence_path = Some(dir.join("app.ron"));
    }

    eframe::run_native(
        "kaleidotron",
        options,
        Box::new(move |cc| Ok(Box::new(app::Kaleidotron::new(cc, cli)))),
    )
}

/// Backup path for a profile dir: the same path with `.bak` appended (a sibling, so the
/// cache/youtube symlinks inside move with it on a rename).
fn profile_backup(dir: &std::path::Path) -> std::path::PathBuf {
    let mut s = dir.as_os_str().to_owned();
    s.push(".bak");
    std::path::PathBuf::from(s)
}

/// `--reset`: preserve the current profile as `<dir>.bak` — but only if no backup exists yet,
/// so a *second* reset can never overwrite your real settings with throwaway test data — then
/// start fresh. Removing the dir drops the `cache`/`youtube` symlinks, not their shared targets.
fn reset_profile(dir: &std::path::Path) {
    let bak = profile_backup(dir);
    if !dir.exists() {
        eprintln!(
            "kaleidotron: --reset: {} doesn't exist yet — already a clean slate",
            dir.display()
        );
        return;
    }
    if bak.exists() {
        // Real settings are already safely backed up; just clear the (test) profile.
        if std::fs::remove_dir_all(dir).is_ok() {
            eprintln!(
                "kaleidotron: --reset: cleared {} for a fresh run (backup at {} kept — restore with --restore)",
                dir.display(),
                bak.display()
            );
        } else {
            eprintln!("kaleidotron: --reset: could not clear {}", dir.display());
        }
    } else if std::fs::rename(dir, &bak).is_ok() {
        eprintln!(
            "kaleidotron: --reset: backed up {} → {} — starting fresh (restore with --restore)",
            dir.display(),
            bak.display()
        );
    } else {
        eprintln!(
            "kaleidotron: --reset: could not back up {}; leaving it in place",
            dir.display()
        );
    }
}

/// `--restore`: discard the current (test) profile and move `<dir>.bak` back into place.
fn restore_profile(dir: &std::path::Path) {
    let bak = profile_backup(dir);
    if !bak.exists() {
        eprintln!("kaleidotron: --restore: no backup at {}", bak.display());
        return;
    }
    if dir.exists() {
        let _ = std::fs::remove_dir_all(dir); // discard the throwaway test profile
    }
    match std::fs::rename(&bak, dir) {
        Ok(()) => eprintln!(
            "kaleidotron: --restore: restored {} from {}",
            dir.display(),
            bak.display()
        ),
        Err(e) => eprintln!("kaleidotron: --restore failed ({e})"),
    }
}

/// Bring a previous `pixelview` install's data across, once, on first run under the new name.
///
/// Ratings, view history, kits, pads, themes and settings all live in the app's data dir, which is
/// derived from the application name — so renaming the app would otherwise look exactly like a
/// fresh install with everything lost.
///
/// Deliberately **copies** rather than moves: the old directory is left completely untouched, so if
/// anything here is wrong the original is still sitting there. Nothing is ever overwritten either —
/// the migration only runs when the new directory does not yet exist.
///
/// Two directories are **linked instead of copied**: `cache/` (the evictable HTTP cache) and
/// `youtube/` (downloaded videos) are the only large things here — gigabytes each — and duplicating
/// them would stall startup and double the disk cost for data that is either regenerable or already
/// on disk once. A symlink gives both installs the same store. Where symlinks aren't available the
/// new install simply starts with an empty cache, which costs nothing but re-downloading.
fn migrate_from_pixelview() {
    let (Some(old), Some(new)) = (
        eframe::storage_dir("pixelview"),
        eframe::storage_dir("kaleidotron"),
    ) else {
        return;
    };
    // Gate on an explicit marker, not on the new directory's existence. Anything at all can
    // create that directory first — a build that predates this function did exactly that here —
    // and then the migration silently never runs and the install looks empty. The marker says
    // "this has been done", which is the actual question.
    let marker = new.join(".migrated-from-pixelview");
    if marker.exists() || !old.exists() {
        return; // already migrated, or nothing to migrate
    }
    // Big, regenerable, or already-once-on-disk: shared rather than duplicated.
    const LINK: [&str; 2] = ["cache", "youtube"];
    if let Err(e) = std::fs::create_dir_all(&new) {
        eprintln!("could not create {}: {e}", new.display());
        return;
    }
    let Ok(entries) = std::fs::read_dir(&old) else { return };
    let mut copied = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let src = entry.path();
        let dst = new.join(&name);
        // Never overwrite. Whatever is already on the new side was put there by the new install
        // and is by definition more current than the copy being migrated from.
        if dst.exists() {
            continue;
        }
        if LINK.contains(&name.to_string_lossy().as_ref()) {
            link_dir(&src, &dst);
            continue;
        }
        let ok = match entry.file_type() {
            Ok(t) if t.is_dir() => copy_tree(&src, &dst).is_ok(),
            Ok(_) => std::fs::copy(&src, &dst).is_ok(),
            Err(_) => false,
        };
        copied += usize::from(ok);
    }
    let _ = std::fs::write(&marker, "");
    eprintln!(
        "migrated {copied} item(s) from {} to {} — the old directory was left untouched",
        old.display(),
        new.display()
    );
}

#[cfg(unix)]
fn link_dir(src: &std::path::Path, dst: &std::path::Path) {
    let _ = std::os::unix::fs::symlink(src, dst);
}
#[cfg(not(unix))]
fn link_dir(src: &std::path::Path, dst: &std::path::Path) {
    let _ = std::os::windows::fs::symlink_dir(src, dst);
}

/// Recursively copy `src` into `dst`. Best-effort per entry: one unreadable file must not abort a
/// migration that has already moved everything else across.
fn copy_tree(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)?.flatten() {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        match entry.file_type() {
            Ok(t) if t.is_dir() => {
                let _ = copy_tree(&from, &to);
            }
            Ok(_) => {
                let _ = std::fs::copy(&from, &to);
            }
            Err(_) => {}
        }
    }
    Ok(())
}

/// A generated window icon: a 4×4 grid of bright "thumbnails" on a dark field —
/// a nod to the thumbnail grid this whole app is built around.
fn app_icon() -> egui::IconData {
    const S: usize = 64;
    const PAL: [[u8; 3]; 16] = [
        [231, 76, 60],
        [46, 204, 113],
        [52, 152, 219],
        [241, 196, 15],
        [155, 89, 182],
        [26, 188, 156],
        [230, 126, 34],
        [236, 240, 241],
        [52, 73, 94],
        [243, 156, 18],
        [142, 68, 173],
        [22, 160, 133],
        [192, 57, 43],
        [41, 128, 185],
        [39, 174, 96],
        [211, 84, 0],
    ];
    let mut rgba = vec![0u8; S * S * 4];
    for px in rgba.chunks_exact_mut(4) {
        px.copy_from_slice(&[24, 26, 32, 255]); // dark background
    }
    let (margin, gap, cells) = (6usize, 3usize, 4usize);
    let cell = (S - 2 * margin - (cells - 1) * gap) / cells;
    for cy in 0..cells {
        for cx in 0..cells {
            let color = PAL[cy * cells + cx];
            let (x0, y0) = (margin + cx * (cell + gap), margin + cy * (cell + gap));
            for y in y0..y0 + cell {
                for x in x0..x0 + cell {
                    let o = (y * S + x) * 4;
                    rgba[o..o + 3].copy_from_slice(&color);
                    rgba[o + 3] = 255;
                }
            }
        }
    }
    egui::IconData {
        rgba,
        width: S as u32,
        height: S as u32,
    }
}
