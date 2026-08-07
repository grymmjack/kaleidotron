//! Build the vendored **libxmp** (MIT) into a static library, so kaleidotron can play the extra
//! tracker formats `xmrs` doesn't (669/FAR/OKT/MED/AMF/ULT/MTM/STM). libxmp is self-contained
//! (its own depackers; only links libm) and needs no generated config header — `common.h`
//! self-detects the platform and the version lives in `include/xmp.h`, so a plain `cc` compile
//! works with no autotools/cmake step.
//!
//! We compile exactly the files libxmp's own build lists in `cmake/libxmp-sources.cmake` (the
//! full build = everything before the `LITE` variant) rather than globbing `src/**.c` — the tree
//! ships a couple of disabled/leftover ProWizard files (`pm.c`, `pp30.c`) that don't compile and
//! aren't in the real build.

use std::path::Path;

fn main() {
    let base = Path::new("vendor/libxmp");
    let list_path = base.join("cmake/libxmp-sources.cmake");
    let sources = std::fs::read_to_string(&list_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", list_path.display()));

    // The full build is everything before the reduced "LITE" list (whose entries are bare
    // filenames without a `src/` prefix and are ignored by the filter below anyway).
    let full = sources
        .split("LIBXMP_SRC_LIST_LITE")
        .next()
        .unwrap_or(&sources);

    let mut build = cc::Build::new();
    build
        .include(base.join("src"))
        .include(base.join("include"))
        .warnings(false); // third-party C — don't flood the log

    let mut count = 0usize;
    for line in full.lines() {
        let tok = line.trim();
        // Each source entry is a lone `src/…/foo.c` token on its line.
        if tok.starts_with("src/") && tok.ends_with(".c") {
            let file = base.join(tok);
            if file.is_file() {
                build.file(&file);
                count += 1;
            }
        }
    }
    assert!(
        count > 100,
        "libxmp source list parsed to only {count} files — libxmp-sources.cmake format changed?"
    );

    build.compile("xmp");
    // libxmp's oscillators use the math library. On Unix that's a separate `libm`; on
    // Windows/MSVC the math functions live in the C runtime, so there is no `m.lib` to link
    // (and asking for one is LNK1181: cannot open input file 'm.lib').
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        println!("cargo:rustc-link-lib=m");
    }
    println!("cargo:rerun-if-changed=vendor/libxmp/src");
    println!("cargo:rerun-if-changed=vendor/libxmp/cmake/libxmp-sources.cmake");

    copy_pdfium_next_to_exe();
}

/// The Windows build renders PDFs in-process via a bundled `pdfium.dll` (loaded dynamically
/// at runtime — see `decode/pdf.rs`). pdfium looks for its library next to the executable, so
/// copy the vendored DLL from `vendor/pdfium/win-x64/` into the target profile dir (and its
/// `deps/` subdir, where the test binaries live). No-op on other platforms — they fall back to
/// poppler's `pdftoppm`.
fn copy_pdfium_next_to_exe() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let src = Path::new("vendor/pdfium/win-x64/pdfium.dll");
    println!("cargo:rerun-if-changed=vendor/pdfium/win-x64/pdfium.dll");
    if !src.is_file() {
        return;
    }
    // OUT_DIR = target/<profile>/build/<pkg>-<hash>/out → up 3 = target/<profile>.
    let Ok(out) = std::env::var("OUT_DIR") else {
        return;
    };
    let Some(profile_dir) = Path::new(&out).ancestors().nth(3) else {
        return;
    };
    let _ = std::fs::copy(src, profile_dir.join("pdfium.dll"));
    let deps = profile_dir.join("deps");
    if deps.is_dir() {
        let _ = std::fs::copy(src, deps.join("pdfium.dll"));
    }
}
