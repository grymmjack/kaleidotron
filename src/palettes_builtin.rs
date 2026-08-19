//! Built-in GPL palettes, embedded into the binary at compile time from
//! `assets/palettes/`. This is what makes the palette library work on a fresh
//! checkout with no external palette directory. Each entry is `(filename,
//! contents)`; the app exposes them as virtual paths under [`BUILTIN_ROOT`].
//!
//! To add a palette: drop a `.GPL` into `assets/palettes/` and add one
//! `include_str!` line below (kept explicit so embedding stays dependency-free).

/// Virtual parent "directory" for built-in palettes. Their `PathBuf`s are
/// `BUILTIN_ROOT.join(filename)` — never touched on disk; [`crate::app`] loads
/// their contents from [`BUILTIN_PALETTES`] instead.
pub const BUILTIN_ROOT: &str = "<built-in palettes>";

pub const BUILTIN_PALETTES: &[(&str, &str)] = &[
    (
        "1BIT (2).GPL",
        include_str!("../assets/palettes/1BIT (2).GPL"),
    ),
    (
        "2BIT (4).GPL",
        include_str!("../assets/palettes/2BIT (4).GPL"),
    ),
    (
        "6BIT (64).GPL",
        include_str!("../assets/palettes/6BIT (64).GPL"),
    ),
    (
        "AMSTRADCPC (26).GPL",
        include_str!("../assets/palettes/AMSTRADCPC (26).GPL"),
    ),
    (
        "ANSI32 (32).GPL",
        include_str!("../assets/palettes/ANSI32 (32).GPL"),
    ),
    (
        "APPLE2-HIRES (6).GPL",
        include_str!("../assets/palettes/APPLE2-HIRES (6).GPL"),
    ),
    (
        "APPLE2-LORES (16).GPL",
        include_str!("../assets/palettes/APPLE2-LORES (16).GPL"),
    ),
    (
        "ATARI-8BIT (256).GPL",
        include_str!("../assets/palettes/ATARI-8BIT (256).GPL"),
    ),
    (
        "ATARI2600 (128).GPL",
        include_str!("../assets/palettes/ATARI2600 (128).GPL"),
    ),
    (
        "BBCMICRO (16).GPL",
        include_str!("../assets/palettes/BBCMICRO (16).GPL"),
    ),
    (
        "BLOODMOON21 (9).GPL",
        include_str!("../assets/palettes/BLOODMOON21 (9).GPL"),
    ),
    (
        "C=64 (16).GPL",
        include_str!("../assets/palettes/C=64 (16).GPL"),
    ),
    (
        "CGA0-HIGH (4).GPL",
        include_str!("../assets/palettes/CGA0-HIGH (4).GPL"),
    ),
    (
        "CGA0-LOW (4).GPL",
        include_str!("../assets/palettes/CGA0-LOW (4).GPL"),
    ),
    (
        "CGA1-HIGH (4).GPL",
        include_str!("../assets/palettes/CGA1-HIGH (4).GPL"),
    ),
    (
        "CGA1-LOW (4).GPL",
        include_str!("../assets/palettes/CGA1-LOW (4).GPL"),
    ),
    (
        "CGA2-HIGH (4).GPL",
        include_str!("../assets/palettes/CGA2-HIGH (4).GPL"),
    ),
    (
        "CGA2-LOW (4).GPL",
        include_str!("../assets/palettes/CGA2-LOW (4).GPL"),
    ),
    (
        "CGA32 (32).GPL",
        include_str!("../assets/palettes/CGA32 (32).GPL"),
    ),
    (
        "COLODORE (16).GPL",
        include_str!("../assets/palettes/COLODORE (16).GPL"),
    ),
    (
        "CYBERPUNK-NEONS (11).GPL",
        include_str!("../assets/palettes/CYBERPUNK-NEONS (11).GPL"),
    ),
    (
        "DAWNBRINGER-16 (16).GPL",
        include_str!("../assets/palettes/DAWNBRINGER-16 (16).GPL"),
    ),
    (
        "DAWNBRINGER-32 (32).GPL",
        include_str!("../assets/palettes/DAWNBRINGER-32 (32).GPL"),
    ),
    (
        "DAWNBRINGERS-8-COLOR (8).GPL",
        include_str!("../assets/palettes/DAWNBRINGERS-8-COLOR (8).GPL"),
    ),
    (
        "EGA (16).GPL",
        include_str!("../assets/palettes/EGA (16).GPL"),
    ),
    (
        "ENDESGA-16 (16).GPL",
        include_str!("../assets/palettes/ENDESGA-16 (16).GPL"),
    ),
    (
        "ENDESGA-32 (32).GPL",
        include_str!("../assets/palettes/ENDESGA-32 (32).GPL"),
    ),
    (
        "ENDESGA-36 (36).GPL",
        include_str!("../assets/palettes/ENDESGA-36 (36).GPL"),
    ),
    (
        "ENDESGA-64 (64).GPL",
        include_str!("../assets/palettes/ENDESGA-64 (64).GPL"),
    ),
    (
        "FAIRCHILD (8).GPL",
        include_str!("../assets/palettes/FAIRCHILD (8).GPL"),
    ),
    (
        "FUNKYFUTURE (8).GPL",
        include_str!("../assets/palettes/FUNKYFUTURE (8).GPL"),
    ),
    (
        "GAMEBOY (4).GPL",
        include_str!("../assets/palettes/GAMEBOY (4).GPL"),
    ),
    (
        "GAMEBOY-BGB (4).GPL",
        include_str!("../assets/palettes/GAMEBOY-BGB (4).GPL"),
    ),
    (
        "HALLOWPUMPKIN (4).GPL",
        include_str!("../assets/palettes/HALLOWPUMPKIN (4).GPL"),
    ),
    (
        "INK (5).GPL",
        include_str!("../assets/palettes/INK (5).GPL"),
    ),
    (
        "INK-CRIMSON (10).GPL",
        include_str!("../assets/palettes/INK-CRIMSON (10).GPL"),
    ),
    (
        "INTELLIVISION (16).GPL",
        include_str!("../assets/palettes/INTELLIVISION (16).GPL"),
    ),
    (
        "JUNGLE-8 (8).GPL",
        include_str!("../assets/palettes/JUNGLE-8 (8).GPL"),
    ),
    (
        "MS-WINDOWS (16).GPL",
        include_str!("../assets/palettes/MS-WINDOWS (16).GPL"),
    ),
    (
        "MSX (16).GPL",
        include_str!("../assets/palettes/MSX (16).GPL"),
    ),
    (
        "NES (55).GPL",
        include_str!("../assets/palettes/NES (55).GPL"),
    ),
    (
        "PEPTO (16).GPL",
        include_str!("../assets/palettes/PEPTO (16).GPL"),
    ),
    (
        "PETMATE (16).GPL",
        include_str!("../assets/palettes/PETMATE (16).GPL"),
    ),
    (
        "PICO-8 (16).GPL",
        include_str!("../assets/palettes/PICO-8 (16).GPL"),
    ),
    (
        "PICO-8-SECRET (32).GPL",
        include_str!("../assets/palettes/PICO-8-SECRET (32).GPL"),
    ),
    (
        "PINEAPPLE-32 (32).GPL",
        include_str!("../assets/palettes/PINEAPPLE-32 (32).GPL"),
    ),
    (
        "QUAKE (244).GPL",
        include_str!("../assets/palettes/QUAKE (244).GPL"),
    ),
    (
        "SECAM (8).GPL",
        include_str!("../assets/palettes/SECAM (8).GPL"),
    ),
    (
        "SEGA (64).GPL",
        include_str!("../assets/palettes/SEGA (64).GPL"),
    ),
    (
        "SHOVEL-KNIGHT-NES (59).GPL",
        include_str!("../assets/palettes/SHOVEL-KNIGHT-NES (59).GPL"),
    ),
    (
        "SODA-CAP (4).GPL",
        include_str!("../assets/palettes/SODA-CAP (4).GPL"),
    ),
    (
        "SYNTHEWAVE-CITY (8).GPL",
        include_str!("../assets/palettes/SYNTHEWAVE-CITY (8).GPL"),
    ),
    (
        "TELETEXT (8).GPL",
        include_str!("../assets/palettes/TELETEXT (8).GPL"),
    ),
    (
        "VGA (256).GPL",
        include_str!("../assets/palettes/VGA (256).GPL"),
    ),
    (
        "VICE (16).GPL",
        include_str!("../assets/palettes/VICE (16).GPL"),
    ),
    (
        "VINES-FLEXIBLE-LINEAR-RAMPS (38).GPL",
        include_str!("../assets/palettes/VINES-FLEXIBLE-LINEAR-RAMPS (38).GPL"),
    ),
    (
        "VIVIDMEMORY (8).GPL",
        include_str!("../assets/palettes/VIVIDMEMORY (8).GPL"),
    ),
    (
        "ZXSPECTRUM (16).GPL",
        include_str!("../assets/palettes/ZXSPECTRUM (16).GPL"),
    ),
];
