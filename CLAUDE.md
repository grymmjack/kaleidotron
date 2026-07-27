# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`pixelview` — a pixel-art-first image **browser** in Rust + eframe/egui, grown
from a scaffold into a Gwenview-like tool. Single binary crate named `pixelview`.
Features: folder navigation (breadcrumbs, drag-reorderable favorites, a left
activity rail, an explorer dock with an expandable folder tree + filter, and a
live details dock with a fit thumbnail + palette swatches + `.GPL` export), a
virtualized thumbnail grid with independent Ctrl+wheel sizing and configurable
per-tile captions **or a sortable table view** (Grid/Table toggle in the sortbar +
View menu, persisted; click a header to sort), a nearest-neighbor zoom view (persistent zoom), per-image
metadata + hover details, multi-select, star ratings stored as KDE Baloo xattrs,
recursive folder montages + count badges, sort/filter, filename search (`/`),
file operations (copy/cut/paste, new folder, rename, trash + Ctrl+Z undo via the
`trash` crate; context menu / Edit menu / shortcuts), a menu bar with hotkeys +
Preferences (Dark/Light theme, grid spacing, caption fields) dialogs, a custom app
icon, plus persisted settings and CLI flags.
Decodes png/gif/bmp/jpeg/webp/tga/tiff/pnm/qoi/**ico** (image crate), **PCX**,
**Aseprite** (`.aseprite`/`.ase`), **PSD**, GIMP **XCF**, **.draw** (PNG preview),
**SVG** (resvg), **XMind mind maps** (`.xmind` — unzip → parse `content.json` → tidy-tree
layout → SVG → the same resvg raster path), **ANSI/ASCII art** (`.ans`/`.asc`/`.nfo`/`.diz`/`.ice`/`.cia`,
embedded CP437 VGA font), and the binary scene formats **XBin** (`.xb`/`.xbin`),
**raw BIN** (`.bin`), **TundraDraw** (`.tnd`, 24-bit), **iCE Draw** (`.idf`),
**Artworx** (`.adf`), **PETSCII** (`.seq`/`.pet` — Commodore C64), **petmate** (`.petmate`
— nurpax/petmate JSON PETSCII) and **RIPscript**
(`.rip` — EGA vector BBS graphics), both via Mike Krüger's `icy_parser_core` — all with
SAUCE-driven hints, shown in the Details pane. Also **source code + text** (~90 exts: rs,
c/cpp/h, py, js/ts, css, html, php, lua, asm, gd, json, yaml, md, log, … — rasterized with
the CP437 font + a lean hand-rolled syntax highlighter, `decode/code.rs`), **PDF**
(`decode/pdf.rs`: the tile is the **real first page**, rendered in-process by **pdfium**
(`pdfium-render`, a bundled `pdfium.dll` on Windows — no external tool, thread-safe for the
thumbnailer), then falling back to poppler's `pdftoppm` (PDF on stdin, PNG on stdout), then a
labeled placeholder if neither is available; page-count/size/title/author metadata via pure-Rust
`lopdf`), and **audio**
(`decode/audio.rs`: a real waveform tile for mp3/wav/ogg/flac/**aiff** via `symphonia` — AIFF
needs the `aiff` feature on symphonia + `symphonia-aiff` on rodio, else `.aif` decoded to a flat
silent buffer; symphonia's AIFF reader also **over-reads** (SSND size not minus its 8-byte
offset/blockSize header → ~2 garbage frames from the next chunk = a pop after the audio), so
`decode_audio` trims to the `COMM` frame count via `parse_aiff_frames` — else a
music-note icon for trackers/voc/au/midi; duration/rate/channels/codec in Details, plus an
**in-app play/pause/seek preview** via `rodio`). These three are **toggleable plugins**
(Preferences → "Format plugins" checkboxes; a runtime atomic flag on the `Registry` — off
drops the type from the listing + skips decoding). **All three default OFF** (a fresh install:
`load_bool(PLUGIN_*_KEY, false)` in `new()` — they're the heaviest decode paths; an existing
user keeps whatever they persisted); enable them in Preferences. **Any** file also gets "Open in default
app" (xdg-open/open/explorer) in the right-click "Open in…" menu, the Details pane, and via
**Enter** in the viewer — so a source file drops into its associated editor.
**Animated GIFs** play (autoplay + seek in the viewer,
hover-to-play in the grid). Archives (`.zip`/`.lha`/`.arj`/…) and **16colo.rs** (the
online ANSI archive: a Places entry with a nav bar — Years / Latest / Groups / Artists
+ a server-side `?filter=` Search) are browsable as virtual folders. A **Year** lists
Packs (→ auto-downloaded pack art); **Artist / Group / Search** instead flatten to a
**sortable table of individual pieces** (thumb · filename · artist · type · year · group ·
pack + a per-row "download file / pack" menu), fetched from the JSON API with no pack
download — opening a piece grabs just its single `raw` file. Keys: 1–5/0 rate, Esc→grid, Backspace→up — the nav keys
are rebindable (Preferences → Hotkeys); in the viewer, Ctrl+wheel or hold-Z + 1-9/0
zooms (Snap locks to 100% steps).

## Project layout

Standard Cargo binary-crate layout (`Cargo.toml` has no `path` override):

```
src/
  main.rs            eframe entry / window setup
  app.rs             PixelView: the whole UI — panels, grid/single views, model,
                     settings persistence, sort/filter, ratings, CLI parsing
  image_types.rs     PixImage (RGBA + optional indexed/palette)
  cache.rs           persistent HTTP cache for 16colo (JSON/thumbnails/files/zips):
                     blob files under <data>/cache/ + a SQLite index (cache.db) for
                     TTL freshness, LRU eviction (2 GiB cap) + size stats; thread-safe
                     via a global Mutex<Connection>. `init`/`get_bytes`/`get_file`/
                     `stats`/`clear`. Used by sixteen.rs (get_json/download) + colo_thumb.
  thumb.rs           worker pool: thumbnails + image metadata (dims, color count)
  colo_thumb.rs      RemoteThumbs: HTTP worker pool fetching 16colo.rs `tn` PNGs
                     (mirrors ThumbBuilder; results uploaded to thumb_tex + thumb_rgba
                     by path, so 16colo tiles recolor). Holds an Arc<Registry>: a PDF
                     piece (no server render) fetches its RAW file + renders page 1
                     locally (pdftoppm) instead of the missing `tn` PNG.
                     Busy feedback: `net_busy()` (any *_rx in flight + colo_sauce_pending)
                     drives a status-bar `egui::Spinner`; grid/table tiles paint a
                     `paint_spinner` arc while a thumbnail loads; the empty grid/table
                     and the SAUCE panel ("fetching…") show one too.
  rating.rs          read/write star ratings via the user.baloo.rating xattr
  ratings.rs         cross-platform ratings sidecar (ratings.json) for virtual art
  anim.rs            decode animated GIF frames + per-frame delays
  soundfont.rs       .sf2 as a virtual folder: rustysynth parse → each sample extracted
                     to a WAV in a temp dir (mounted like an archive) + preset/instr counts
  sfz.rs             .sfz as a virtual folder: parse opcodes → symlink referenced samples
                     into a temp dir; region/sample/key-range info
  dls.rs             .dls as a virtual folder: walk the RIFF wave pool → rewrap each
                     embedded wave into a standalone WAV; instrument/sample counts
  xi.rs              .xi (FastTracker II instrument) as a virtual folder: xmrs parse →
                     each sample extracted to a WAV; name/sample count
                     (.xrns/.xrni are ZIPs → handled by archive.rs's force-zip path)
  libxmp.rs          FFI to bundled libxmp (vendor/libxmp, built by build.rs) — plays the
                     tracker formats xmrs doesn't (669/far/okt/med/amf/ult/mtm/stm)
  format_color.rs    per-format tile/waveform/badge accent colors — a process-global ext→RGB
                     map (Preferences-editable, persisted), read by the grid AND thumbnailer
  scale.rs           pixel-art UPSCALERS for the Recolor Resize→Upscale dropdown (a `Scaler`
                     enum + `apply(rgba,w,h)->(out,ow,oh)`): Scale2x/EPX, Scale3x, Eagle 2×/3×
                     (hand-rolled hard-edge), xBR 2×/3×/4× (ported from libxbr FILT2/FILT3;
                     4× = 2× chained), 2xSaI/Super2xSaI/SuperEagle (ported from snes9x
                     2xsai.cpp), and HQ2x/3x/4x (via the `hqx` crate, bridged u8-RGBA↔u32-ARGB).
                     Runs as a PRE-pipeline step in the 4 recolor consumers (`scale_source`).
build.rs             compiles vendor/libxmp (C) into a static lib via the cc crate
vendor/libxmp/       vendored libxmp 4.6.3 source (MIT) — src/ + include/ + libxmp-sources.cmake
  decode/
    opl3.rs          OPL3 FM-synth chip emulator (public-domain "Opal" port) — for .rad
    rad.rs           Reality Adlib Tracker replayer (public-domain RADPlayer port) — .rad
    mod.rs           Decoder trait + Registry (sniff-then-extension dispatch)
    builtin.rs       image-crate decoder: png/gif/bmp/jpeg/webp/tga/tiff/pnm/qoi/ico/draw
    pcx.rs           hand-written, palette-preserving PCX decoder
    aseprite.rs      .aseprite/.ase via the asefile crate (composited frame 0)
    psd.rs           .psd via the psd crate (flattened composite)
    xcf.rs           GIMP .xcf — composites layers (xcf crate; offsets not applied)
    svg.rs           .svg rasterized via resvg/usvg/tiny-skia
    xmind.rs         .xmind mind maps — unzip (zip crate) → parse content.json (serde_json)
                     → tidy-tree balanced-map layout → emit an SVG string → rasterize via
                     resvg/usvg (embedded DejaVu Sans loaded into usvg's fontdb so text
                     renders). **Reads the file's OWN theme** (`sheet.theme`, `parse_theme`):
                     canvas bg, per-branch `multi-line-colors` palette (→ filled branch topics)
                     OR a single-colour theme (→ outlined boxes in the theme's border colour),
                     central/main/sub fills+text+borders. Renders like the XMind program: central
                     topic, filled/outlined rounded-rect main branches, sub-topics as text on a
                     coloured underline, tapered filled-ribbon connectors. **Semantic features:**
                     markers (priority discs / task pies / flag / star / symbol — `draw_marker`),
                     a note indicator, labels, **embedded images** (resources/* → base64 data URI,
                     `resolve_image`), **boundaries** (translucent dashed group outline — subtree
                     rects unioned in `place`/`collect_boundaries`), **relationships** (dashed
                     arrows between topics by id, sheet- AND topic-level), and **detached/floating
                     topics** (`children.detached` merged with `attached`, so a "notes board" sheet
                     shows its content). Renders at RASTER_TARGET≈3200px longest side (supersamples
                     small maps so text isn't chunky). `decode()` renders sheet 0 (grid tile);
                     `xmind_sheet_titles` + `render_xmind_sheet(bytes, idx)` back the in-app
                     multi-sheet viewer + `--render --sheet N`. Falls back to the embedded
                     Thumbnails/thumbnail.png if content.json can't be parsed. **TODO (deferred):**
                     pseudo-vector re-render at zoom (re-rasterize the SVG / PDF page at higher res
                     as the user zooms in) — for XMind + the PDF viewer
    ansi.rs          .ans/.asc/.nfo/.diz/.ice/.cia — CP437 + ANSI SGR/cursor + iCE
                     + SAUCE-driven 8×8 (VGA50/EGA43) vs 8×16 cell selection;
                     optional 9-dot VGA cell (font_9px); pads to a ≥25-row screen;
                     TextStream renders byte prefixes for baud-rate ANSImation playback
    xbin.rs          .xb/.xbin — binary ANSI: palette/font + RLE; shared render_textmode;
                     default palette is ansi::VGA_PALETTE (raw VGA attr order, not SGR)
    bin.rs           .bin — raw char/attr pairs (SAUCE width); idf/adf reuse render_textmode
    code.rs          source code / text (~90 exts) → CP437 8x16 raster + a lean hand-rolled
                     highlighter (per-language comment/string rules, shared keyword union,
                     line-number gutter, tab expand, UTF-8→CP437, line+cell budget). `CODE_EXTS`
                     re-exported; registry routes code exts to `decode_ext(bytes, ext)`. ipynb
                     flattens to highlighted Python. Zero heavy deps (no syntect).
    pdf.rs           .pdf — the tile is the REAL first page via `render_page`. Tries in-process
                     **pdfium** first (`pdfium-render`; `pdfium_instance` lazily loads the lib next
                     to the exe / `PDFIUM_DYNAMIC_LIB_PATH` / system — Windows ships a bundled
                     `vendor/pdfium/win-x64/pdfium.dll`, copied beside the exe by build.rs), then
                     poppler `pdftoppm` (PDF→stdin, PNG→stdout, stdin fed from a thread to avoid
                     pipe deadlock), else a labeled placeholder. Metadata
                     (page count / MediaBox size / /Info title+author) via `lopdf`; `pdf_meta`/
                     `PdfMeta` feed the Details pane.
    audio.rs         audio → waveform tile (mp3/wav/ogg/flac via `symphonia`: decode → peak
                     envelope → resample → mirror) else a music-note icon (trackers/voc/au/midi).
                     `audio_info`/`AudioInfo` (duration/rate/channels/codec) feed Details; the
                     DECODE path is device-free so `cargo test` stays headless. `AUDIO_EXTS`
                     re-exported; registry routes audio exts to `decode_ext` (needs the ext hint).
                     In-app PLAYBACK is separate (`AudioPlayer` in app.rs, rodio) — see below.
    tundra.rs        .tnd — TundraDraw 24-bit truecolor command stream
    idf.rs           .idf — iCE Draw: bounds + RLE + end-of-file font/palette
    adf.rs           .adf — Artworx: version + 64-color palette + font + pairs
    petscii.rs       .seq/.pet — Commodore PETSCII: icy_parser_core parses → our render
    petmate.rs       .petmate — nurpax/petmate JSON PETSCII (screens of {code,color}
                     cells) → rendered with the same C64 font + VIC-II palette, stacked
    c64_font.rs      embedded C64 8x8 char ROM (MEGA65 open-roms, LGPL; for PETSCII)
    rip.rs           .rip — RIPscript: icy_parser_core parses → our 640×350 EGA rasterizer;
                     RipStream renders byte prefixes for "watch it draw" baud playback
    rip_chr.rs       RIP scalable text: the 10 BGI stroke fonts (rip_chr/*.CHR) as lines
    cp437_font.rs    embedded CP437 8x16 VGA font (generated from a system PSF)
    cp437_font_8x8.rs  embedded CP437 8x8 VGA50 font (IBM ROM, for 80×50 ANSI)
  palettes_builtin.rs  the bundled .GPL palette library, embedded via include_str!
assets/pixelview.png   generated app icon (4×4 thumbnail grid)
assets/palettes/       55 bundled .GPL palettes (embedded into the binary)
assets/pixelfx_builtin.ron  bundled PixelFX recolor presets (embedded; see PixelFX presets)
assets/DejaVuSans.ttf  embedded UI fallback font (fills egui's tofu gaps; see Font gotcha)
assets/SymbolsNerdFont-Regular.ttf  embedded icon font (the `icons::*` designed glyphs)
pixelview.desktop      desktop entry (StartupWMClass=pixelview) for the task icon
install-icon.sh        installs the .desktop + icon into ~/.local/share
```

Palettes are **embedded** (`src/palettes_builtin.rs` `include_str!`s every file in
`assets/palettes/`), so the palette library works on any platform with no external
directory. The optional `palette_dir` (persisted; defaults to a Linux path that
simply scans empty elsewhere) only *adds* user `.gpl` files on top of the bundled
set — see `all_palettes` / `builtin_palette_contents` in `app.rs`. To add a bundled
palette, drop a `.GPL` in `assets/palettes/` and add one `include_str!` line.

Note: `Cargo.lock` **is committed** (the monorepo `.gitignore`'s `**/Cargo.lock`
rule was removed for these binary crates). It pins `eframe`/`egui` to 0.34.3 — the
range `eframe = "0.34"` would otherwise let a fresh checkout drift to a newer patch
with a moved API (see the egui gotcha below — that's what bit the initial scaffold).

## Commands

```sh
cargo run --release      # build + launch (release; nearest-neighbor needs the GPU path)
cargo build --release
cargo check              # fast type-check during edits
cargo clippy             # lint
cargo fmt                # format
cargo test               # 238 tests (228 unit + 10 headless egui_kittest GUI tests; +12 ignored network/real-trash)
cargo test gui_tests     # just the egui_kittest UI tests; cargo test <name> for one
```

First-time eframe/winit system deps on Debian/KDE:
```sh
sudo apt-get install libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libssl-dev libasound2-dev
```

**Headless render-to-file (`--render`).** `pixelview --render <PATH>... [-o FILE | --outdir DIR]
[--font-9px] [--scale N] [--format FMT]` decodes any viewable format (ANS/XB/XBIN/RIP/BIN/… + raster)
to an image file and **exits without opening a window** (works over SSH / in a batch pack-conversion
script). `CliArgs::is_render()` short-circuits `main` into `app::run_render` *before* `eframe::run_native`.
It reuses the same `Registry::decode_path` → `PixImage` the GUI uses (text-mode decoders rasterize
inside `decode`, so `pixels` is already the finished art), then writes via `image::save_buffer[_with_format]`.
Inputs may be files (tried directly) or folders (scanned non-recursively, filtered by `is_render_candidate`
= scene/raster art, skipping audio/PDF/source-code). `--font-9px` calls `decode::set_font_9px` (the GUI
primes this from storage; headless must set it explicitly). `-o` is single-file-only; batches use `--outdir`
(default: beside each input, `<stem>.<fmt>`). `--scale` is an integer nearest-neighbor upscale (`upscale_nn`).
`--sheet N` renders sheet **N** (1-based) of a multi-sheet `.xmind` (via `render_xmind_sheet`); without it
the first sheet is rendered.

**Windows (MSVC) build.** Builds with the standard MSVC toolchain — no extra setup. Three
platform gotchas are already handled: (1) the `xattr` crate is Unix-only, so it's a
`[target.'cfg(unix)'.dependencies]` entry and `rating.rs` `#[cfg(unix)]`-gates it (Windows ratings
go through the `ratings.json` sidecar); (2) `build.rs` links libm (`m`) only off-Windows (MSVC has
no `m.lib` — math is in the CRT); (3) PDFs render via a **bundled `pdfium.dll`** (`vendor/pdfium/
win-x64`, copied beside the exe by `build.rs`) so poppler isn't needed. NB `cargo`/`cargo test`
must be run from **PowerShell**, not the Git-Bash shell — Git Bash's `/usr/bin/link.exe` (a GNU
hardlink util) shadows MSVC's real `link.exe` and fails linking with LNK1181.

## Architecture: the big picture

Three subsystems wired together in `PixelView::new` (`app.rs`):

**1. Decoder registry (`decode/mod.rs`)** — the entire extension story. `Registry`
holds `Vec<Box<dyn Decoder>>`. Dispatch in `decode_bytes` is **two-tier**: every
decoder's `sniff()` (magic bytes of the first ≤32 bytes) is tried first, then file
extension is the fallback. **Order matters**: `PcxDecoder` is registered before
`ImageCrateDecoder` in `with_builtins`, so PCX's magic byte (`0x0A`) wins before
the broad `image`-crate sniff. `known_extension()` is what filters a folder
listing down to viewable files.

**2. Threaded thumbnailer (`thumb.rs`)** — `ThumbBuilder` spawns N worker threads
(N = `available_parallelism`) sharing an `Arc<(Mutex<Vec<Job>>, Condvar)>` used as
a **LIFO stack**, so the most-recently-requested (i.e. just-scrolled-into-view)
thumbnail is decoded first. Results return over an `mpsc::channel`. `request()`
dedupes via a `requested: HashSet`, so the grid can cheaply call it every frame
for visible tiles. Workers do decode + nearest-neighbor scale (`make_thumb`)
entirely off the UI thread; **only CPU RGBA buffers cross back** — never GPU
handles.

**3. UI / eframe::App (`app.rs`)** — `ui()` (the eframe 0.34.3 entry; see gotcha)
`drain()`s finished thumbs and uploads them to GPU textures (`ctx.load_texture`,
`TextureOptions::NEAREST`) — **texture upload must happen on the UI thread**, which
is why workers stop at raw pixels. The chrome is a stack of `show_inside` panels:
`menubar` (top) → `rail` (left, a VSCode-style activity bar of icon toggles:
`ui_rail`) → `favorites` (top) → `crumbs`/path (top, under favorites) → optional
`searchbar` (top, vim `/` filename filter) → optional `advsearch` (top, the advanced
recursive search — see below) → `status` (bottom) → `sortbar` (bottom, mounted *after* status
so it sits above it) → optional left dock (`leftdock`: `details` on top, `explorer`
below) / `recolor` (right) → `CentralPanel` (grid or single view, by the `Mode` enum). The status bar reserves
its flush-right zoom readout first, then fills the rest with a truncating left
label, so they can't overlap. The grid is virtualized via `ScrollArea::show_rows`;
each tile is a `tile`-square thumbnail plus a configurable caption strip below
(`caption_lines`, `caption_fields` bitmask; independent `grid_gap`/`grid_gap_y`).
An optional per-tile **border** (`grid_tile_border`, Preferences → "Tile borders",
persisted, default on) frames each tile as a card (brighter on hover/selection),
painted **last** so it sits over the thumbnail edges. **`show_rows` gotcha:** it adds
`item_spacing.y` on top of the row height you pass to get the between-row pitch, so the
**vertical gap must live in `item_spacing.y`** (set it to `grid_gap_y` before `show_rows`)
and the row height passed is the *content* height (`cell_h`) — passing `cell_h + gap_y`
with default spacing reserved the gap but never rendered it (the vertical-spacing slider
looked dead).
`want_repaint` drives `ctx.request_repaint()` while thumbnails are pending.

File ops (`copy_selection`/`cut_selection`/`paste`/`new_folder`/`start_rename`/
`delete_selection`/`undo`, dispatched via `FileAction`/`do_file_action`) go to the
system trash (`trash` crate) and push reversible `UndoOp`s for Ctrl+Z; they set
`self.status` **after** `refresh()` (which re-scans via `open_folder`, clearing
stale status). Favorites are drag-reorderable (`favorites_buttons` + a unioned
drag sensor, *not* `dnd_drag_source` — its scope breaks `horizontal_wrapped`) and
right-click-removable in both the toolbar and the Places dock. Each favorite/pin can
be **color-tagged** from its right-click menu (a popup grid of the bundled **ANSI32**
swatches via `ansi32_palette`, + ✕ to clear); the chosen color fills the button (text
flipped to black/white by `contrast_text`). Stored in `fav_colors` (path → RGB,
persisted as `FAV_COLORS_KEY`); cleared when the favorite is removed. In the Places
dock the favorites split into **Local** vs **16colo.rs** sub-tabs (`places_tab`):
`favorites_buttons` takes a `filter: Fn(&Path) -> bool` (`!is_remote` / `is_remote`) and
filters with `continue` so the *global* favorite index — and thus reorder/remove —
stays correct. The **top toolbar** passes a filter that, when `fav_bar_colored_only`
(persisted, default on; View → "Favorites bar: colored only") is set, surfaces **only
color-tagged favorites** to declutter the bar — with a fallback to all when *none* are
colored (never an empty bar), and a faint `+N` marking how many uncolored ones are hidden
in the dock. Local also holds Home + smart
filters; 16colo.rs holds the 🌐 browse entry + the remote pins. The explorer's
folder tree uses `CollapsingState` (lazy: collapsed nodes do no I/O). The details
dock shows a fit thumbnail + palette swatches + `.GPL` export (`to_gpl`), fed by
the thumbnailer's `extract_palette` (authoritative palette for indexed art).

**4. The app model (`app.rs`)** — `open_folder` scans into `all_entries` (raw
`Entry { path, is_dir, size, mtime, ctime, rating }`); `rebuild_view()` filters (by
`min_rating`) and sorts (by `SortKey`, dirs-first optional) into `entries`, the list
the grid renders. **`selection` is keyed by `PathBuf`, not index**, so it survives
re-sorting/filtering. The caches `thumb_tex` / `img_meta` / `folder_previews` are
**persistent path-keyed maps, never cleared on navigation** — clearing `thumb_tex`
while the worker's `requested` set persisted once caused black tiles (and persisting
makes back-nav instant). Because an egui closure can't borrow `self` twice, menu and
combo handlers stash a deferred action (`MenuAction`, or locals like `clicked`/`nav`)
and apply it *after* the closure returns.

**Advanced recursive search (`ui_search`, Ctrl+F / Edit menu)** — a `SearchSpec`
(filename / extension list / W·H min-max / **size KB min-max** / **modified-date
range** / **min ★** / SAUCE text; all optional) drives `search_walk`, which BFS-walks
the current folder's subtree on a **background thread** (mirrors the 16colo `mpsc`
pattern: `search_rx` + an `AtomicBool` cancel, `Send`ing a `SearchMsg::Hit(Entry)` per
match then `Done(n)`). Filters run cheapest-first: name/type (path strings) → size/date
(one `metadata` read) → rating (xattr; sidecar-only ratings on archive/16colo art are
invisible to the thread) → SAUCE (`read_file_tail` + parse) → dimensions (`quick_dims`:
a header-only `image::image_dimensions`, full-decode fallback) — each only when its
field is set, and a cheap reject skips the costly ones. Dates use `date_ymd` (mtime →
`YYYY-MM-DD` via `civil_from_days`, no date crate; the format sorts as text so range
checks are string compares). `poll_search` drains
hits each frame into `search_results`, resolving each star via `read_rating`. When
`search_results.is_some()` **`rebuild_view` renders it instead of `all_entries`** — so
the grid, thumbnailer, sort/filter and open-in-viewer all work on results for free. Any
navigation (`show_folder`) cancels the search and drops the results. Colors filtering is
deferred (needs a full decode of every candidate). Result tiles get an extra caption
line — the match's folder relative to `search_root` (`result_folder_label`; `📁 ·` =
directly in the root), so you can see *where* a hit lives.

**Smart filters (saved searches)** — `save_filter` stores the current `SearchSpec`
under its `summary()` label (e.g. `*.ans · sauce:acid`) in `saved_filters`, persisted
as `Vec<Vec<String>>` (`[name, ...record]`; no serde-derive on `SearchSpec` — it
flattens via `record`/`from_record`). They render in the Places dock under "Smart
filters" with a 🔍 prefix; click → `recall_filter` (load spec + `start_search` from the
*current* folder), right-click → remove. Both deferred out of the `ui_explorer` closure
(`recall` / `remove_filter` locals applied after) since it can't borrow `self` twice.

**Grid context menu extras** — folder tiles get **📌 Pin to Places** (`pin_dir`
deferred → `favorites.push`). File tiles get a **🔍 Smart filter on…** submenu
(`SmartCriterion`) that seeds a fresh `SearchSpec` from that one file and runs it
(`smart_filter_from`): Type=its ext, File name=first ≥3-char word of the stem, File
size=±20% KB, Date=its mtime day, Rating=its stars+ (shown only when rated), SAUCE
group/artist=its SAUCE field (shown only for `is_textmode_ext`). Both are deferred
`pin_dir`/`smart_on` locals applied after the tile closure. **In a 16colo flat listing
(`colo_flat`) the rows are *pieces*, not dirs**, so `entry_context_menu` also takes a
`colo_pin: Option<(&str, bool)>` and offers **📌 Pin “<artist/group/search>” to Places**
(`TilePick::PinFolder` → `pin_current` → `pin_current_folder` pins `self.folder`). Since
the listing's virtual path encodes the search (`…/search/artist/x`), the pinned favorite
re-runs that exact artist/group/search when clicked — the way to bookmark an artist.

**External "Open in…" associations (`Opener`, `openers`, editor `ui_associations`)** — a
file tile's context menu has an **Open in…** submenu listing the user's programs registered
for that extension (+ "Other program…" → an rfd one-off). An `Opener` is `{name, exec, args,
env, icon, exts}` (`exts` comma/space-separated; `ext_list()` normalizes), persisted as a
flat `Vec<Vec<String>>` (`record`/`from_record`, no serde-derive — like `SearchSpec`).
`launch_external` spawns `Command::new(exec)` with `args` (a `{}` token → the **`resolve_local`d
real** file path, else appended — so virtual 16colo/archive art opens from its on-disk copy)
and `env` (`KEY=VALUE` lines). `open_external_for` routes both paths: a 16colo flat-listing
piece **not yet downloaded** kicks off `start_piece_open` and stashes `(exec,args,env)` in
`pending_external`, which `poll_colo_open` consumes to launch the program once the real file
lands (instead of opening the viewer); everything else launches immediately. `entry_context_menu` is a free fn so it can't borrow `self`; it
takes an owned `&[OpenerItem]` snapshot (`opener_items`, built inside the menu closure) and
filters by the entry's extension; the pick (`TilePick::OpenWith(idx)` / `OpenWithOther`) is a
deferred `open_with`/`open_other` local applied after the grid/table loop. Icons decode via
`ensure_opener_icons` (registry → 32px texture, cached by path in `opener_icons`; `None` on
failure so it doesn't re-decode). The editor (View → Associations…, `show_associations`) is a
two-pane window — a left list + right field editor on the `assoc_selected` opener, with
add/`opener_presets()`/remove deferred; it precomputes an owned `list` of (name, icon) so the
list pane doesn't borrow `openers` while the editor `get_mut`s the selected one.

**Table view (`ui_table`, `table_view` bool, persisted)** — an *alternate renderer*
for the browse mode, **not a third `Mode`**: `Mode` stays `Grid` and the central panel
picks `ui_table` vs `ui_grid` by `self.table_view`, so selection (`PathBuf`-keyed),
ratings, keyboard nav, and context menus (`entry_context_menu`, shared with the grid)
all keep working unchanged. Hand-rolled (no `egui_extras`) on the same virtualized
`ScrollArea::show_rows` the grid uses: a fixed-width header row above the body (click a
header → set `SortKey`, click again → reverse), then one row per `Entry` laid out cell
by cell with `item_spacing = 0` so header/body columns align. Two column sets: **file**
columns (name/type/size/dims/colors/rating/modified) for any folder, and **scene**
columns (filename/artist/type/year/group/pack + a ⬇ download menu) when `colo_flat`.
Sorting routes through the same `sorted_filtered_view`, which now also takes the
`colo_pieces` map for the scene `SortKey`s (`Artist/Group/Year/Pack` — in `SortKey::ALL`
for persistence but excluded from `SortKey::COMMON`, the sortbar combo). `Dimensions`
sorts like `Colors` (unknown-last in both directions). The toggle is also the rebindable
`Action::ToggleView` (default **`T`**, browse-mode only so it never clashes with `T` =
tile in the single view), shown in Preferences → Hotkeys + the Help window. Each cell
has `cell_pad` breathing room and clips its text to the cell, so columns never touch.
Rows always paint a **zebra stripe** (odd rows); the optional **`table_grid`** toggle
(Preferences → "Table dividing lines", persisted) additionally draws a subtle translucent
**bottom row separator + interior column dividers** per row (painted over the cells).
Columns key off a `ColKind` (not a position), so the **file** layout's optional columns
are user-toggled via a `TC_*` bitmask (`table_columns`, persisted; Preferences → "Table
columns" *and* the header right-click menu) and the **scene** layout's via a parallel
`CS_*` bitmask (`colo_columns`); Name + thumbnail are always shown. **Header UX:**
left-click a header sorts (re-click reverses); **right-click** → Sort ascending/descending
+ a "Show columns" checklist (toggles the layout's bitmask, no Preferences trip); a thin
border at each fixed column's right edge **drag-resizes** it (`col_widths`: ColKind →
points, persisted; flex columns absorb the slack); and **dragging a header body reorders**
the columns (a vertical drop indicator follows the pointer; the new order persists as
`table_order`/`colo_order` — a `Vec<u8>` of ColKind, applied by sorting the built `cols`,
unknown/new kinds appended). Thumbnail stays first, the scene Download menu last; click vs
drag vs border-drag disambiguate by sense (the cell is `click_and_drag`, the border its own
`drag` widget). Archive rows (.zip/…) render the folder glyph + a format badge like the grid. In the scene layout the **Pack / Year /
Group** cells are clickable links into the 16colo browser (`colo_link` deferred →
`open_folder` of the pack / year / `groups/<group>` path; the link click takes priority
over the row's open-the-art click).

**16colo.rs flat-piece listings (`ColoSource`, `colo_walk`, `start_colo_pieces`)** —
Artist / Group / Search no longer list pack *folders*; they stream individual **pieces**
into the table (the requested flat view), keyed by virtual path `<16colo.rs>/<year>/
<pack>/<FILE>`. `colo_walk` (a background thread mirroring `search_walk`: `colo_rx` +
`AtomicBool` cancel) emits `ColoMsg::Hit(Entry, ColoPiece)` then `Done(n)`; `poll_colo_pieces`
drains them, resolves each rating, appends to `all_entries` + the `colo_pieces` map, and
re-sorts. Pieces come from the JSON API with **no pack download**: an artist = one call
(`fetch_artist_pieces`), a group = per-pack (`fetch_group_pack_refs` → `fetch_pack_pieces`),
a search = matched artists + groups (capped). The nav-bar search is **facet-scoped**:
on the Artists tab it builds `SEARCH/artist/<q>` → `ColoSource::SearchArtists` (artist
names only), on Groups `SEARCH/group/<q>` → `SearchGroups`, otherwise `SEARCH/<q>` →
`Search` (both) — so "tainted" under Artists no longer drags in the *group* "tainted".
`do_artists`/`do_groups` closures in `colo_walk` are shared by all three. Thumbnails are
16colo's pre-rendered render PNGs fetched by the `RemoteThumbs` HTTP pool (`colo_thumb.rs`)
and uploaded to `thumb_tex` by virtual path (LINEAR — they're rendered previews, not
pixel-art); we fetch the **larger `/x1/`** render (≈768px, derived from the `tn` path via
`sixteen::x1_url`), not the tiny `/tn/`, so big grid tiles aren't a blurry upscale. **The
advanced search (Ctrl+F) filters a remote/flat listing in memory** (`colo_filter_in_memory`)
instead of walking disk: name matches the visible row text (filename + artist/group/pack),
SAUCE matches artist/group + cached SAUCE, plus ext/size/rating; dims/colours/date are
unknown for virtual pieces so those filters are ignored. Opening a piece
downloads just its single `raw` file (`start_piece_open`/`poll_colo_open` → `colo_files`
map; `load_full` decodes via `resolve_local`, keeping the virtual path as identity).
Per-row ⬇ menu saves the file or its pack `.zip` to disk (`download_piece` → rfd +
`sixteen::download_to`, reported via `colo_save_rx`). Entering a flat listing auto-switches
to table view; navigating away (`show_folder`) cancels the stream + clears `colo_pieces`.
**URL encoding:** the API returns *literal* paths, so a filename with a `#` (e.g.
`#44_FIRE.ANS`) would truncate every URL at the fragment — leaving the piece
un-downloadable and its thumbnail spinning forever. `sixteen::enc_path` percent-encodes a
site path **preserving `/`** (and `abs_url` runs every relative path through it; the
pack-view `raw_url` `enc()`s the built filename) so `#`→`%23` etc. survive
(`hash_in_filename_is_percent_encoded_in_urls`).

## Core invariants (read these before touching the data model)

- **`PixImage` (`image_types.rs`) always has `pixels` (RGBA, for display) and
  optionally `indexed` (the original `indices` + `palette`).** Palette-based
  formats must call `PixImage::from_indexed` (populates both); RGBA-only formats
  call `from_rgba` (leaves `indexed = None`). Preserving the palette is the whole
  point of the project — it's what makes future palette-swap / cycling / accurate
  re-export possible. `pcx.rs` is the reference for the palette-preserving path;
  `builtin.rs` (the `image` crate) deliberately leaves `indexed = None`.
- **Nearest-neighbor for *upscaling*; area-average for *downscaling*.** The viewer
  and source-res thumbnails upload `TextureOptions::NEAREST` so pixel art upscales
  crisply (and the viewer's pixel-perfect blit keeps it exact — see below). But a big
  image shrunk to a thumbnail must NOT be nearest-sampled: single-sampling a 50%
  dither (▒) aliases it into fake noise. So `make_thumb` **area-averages** (box
  filter) on downscale, and the upload picks `LINEAR` for any thumb it downscaled
  (`src_w/h > thumb w/h`), `NEAREST` otherwise — same rule in `grid_recolored_tex`.
  Net: crisp upscales, faithful-tone downscales. Don't nearest-sample a downscale.

## Colorizer / Recolor pane pipeline (`Adjust` + `apply_pipeline`)

The Recolor pane (`ui_recolor`) applies per-image **adjustments** then a **palette
rematch**, and the *order* of all of it is user-controlled.

- **`Adjust`** holds 12 value fields (brightness, contrast, gamma, shadows, highlights,
  posterize, hue, saturation, **vibrance**, pixelate, sharpen, **invert**) plus `order:
  [OpKind; 19]` — a permutation of `OpKind::ALL`. **8 of those 19 are marker ops** (no
  slider value), each marking *where* a step happens and configured in its own section:
  `Pixelate` (mosaic W×H), `Palette` (recolor/remap), `ColorBalance` (per-channel R/G/B
  offset), `Dither` (the dither pattern), and the CRT post-FX `Scanlines` / `Glow` /
  `Vignette` / `Phosphor`. New ops are **appended** to `OpKind::ALL` so persisted order
  indices stay valid. `OpKind::is_marker` is the slider-vs-marker test the row UI keys
  off. (Pixelate's *value* still lives in `Adjust.pixelate` (width); its height/lock are
  separate `PixelView` fields — see below.)
- **`apply_pipeline(rgba, w, h, &adjust, &PipeAux)`** is the one true pipeline: it
  walks `adjust.order` and runs each value op (point ops via 256-LUT, color ops via
  HSL, sharpen spatial); at each *marker* op it does that step inline — `Pixelate` via
  `pixelate_blocks(rgba, w, h, bw, bh)` (`bw = adjust.pixelate`, `bh = aux.pixelate_h`
  or square), `Palette` snaps via `thumb::remap_to_palette(aux.palette)`, `Dither` runs
  `thumb::dither_pass`, `ColorBalance` runs `color_balance(aux.balance)`, and the post-FX
  run `apply_scanlines`/`apply_glow`/`apply_vignette`/`apply_phosphor(&aux.fx)`. `PipeAux`
  carries the marker inputs (dither method/amount/custom matrix + **per-axis cell**
  `dither_scale_x/y`, `pixelate_h`, the balance offset, the `PostFx` value `fx`, the snap
  palette). All 5 recolor paths build it via `self.pipe_aux(palette, dscale_x, dscale_y)`:
  viewer preview (`make_preview`), full-res (`make_full_reduced`), grid tiles
  (`grid_recolored_tex`), swatch-flash (`make_flash_tex`, dither forced off), and
  `save_recolored`. `adjust_pixels` is a **test-only** wrapper (neutral aux).
- **`apply_pipeline_resized(rgba, w, h, tw, th, &adjust, &aux)`** wraps the pipeline for
  the **Resize/resample** feature: when `tw<w`/`th<h` it box-downscales to `tw×th`, runs
  the WHOLE pipeline there, then **nearest-upscales back** to `w×h` — so the output keeps
  native size (unchanged zoom/screen size) but shows the low-res degradation and dither
  runs at the reduced resolution. Factor-based (`resize_on`/`resize_fx`/`resize_fy`/
  `resize_lock`, all `PixelView` fields); `resize_target(w,h)` computes `(tw,th)`. Every
  recolor path calls this instead of `apply_pipeline` directly.
- **Pixel-art upscalers (`scale.rs`, `scale_algo: Scaler`, `SCALE_ALGO_KEY`).** The
  Resize section's **Upscale** dropdown runs an integer pixel-art scaler (Scale2x/3x,
  Eagle, xBR 2×/3×/4×, HQ2x/3x/4x, 2xSaI/Super2xSaI/SuperEagle) as a **pre-pipeline**
  step: `self.scale_source(w,h,rgba)` enlarges the source *before* the pipeline, so the
  whole stack + Save operate on the blown-up art. It's applied in all 4 recolor consumers
  (make_preview / make_full_reduced / grid_recolored_tex / save_recolored) right after
  the source decode; the dither `eff_dither_scale` native dims are multiplied by the scale
  factor so the small preview still predicts the full view. In `pipeline_active`/`_key` +
  captured in `FxPreset`. Adding a scaler = one `Scaler` variant + one fn (the UI/persist
  iterate `Scaler::ALL`). Still to add: MMPX + LQx (need their exact reference source).
- **Dither is decoupled from the snap.** Ordered/Bayer + the editable **Custom** matrix
  (`dither_custom`/`dither_custom_n`, seeded by `thumb::bayer_values`) are a *pure bias*
  applied at the `Dither` slot via `thumb::ordered_bias` — the later `Palette` op does
  the quantize, so dither can sit anywhere (e.g. before Posterize = dithered banding,
  no palette). Error-diffusion (Floyd–Steinberg/Atkinson) *can't* be moved off the
  snap, so it quantizes toward `aux.palette` at the Dither slot and no-ops if none.
  `thumb::dither_pass` dispatches both; `DITHER_NAMES`/`DITHER_CUSTOM` index the methods.
  The ordered methods take a **per-axis cell size** `scale_x`/`scale_y` (each Bayer cell
  spans `scale_x`×`scale_y` px) so the crosshatch reads on hi-res art; stored as
  `dither_scale_x`/`dither_scale_y` (+`dither_scale_lock`) `PixelView` fields, UI = Cell
  W/H sliders + Lock. **Auto** (`detect_dither_scale` → `thumb::detect_pixel_scale`)
  detects the art's integer upscale factor per axis (edge positions cluster onto a
  period-S grid) and matches the cell to it, else scales to the resolution. `eff_dither_scale`
  shrinks the authored scale by `buf/native` on a small preview so it predicts the full view.
- **Pixelate is a marker op** with per-axis Width×Height: width = `Adjust.pixelate`,
  height = `pixelate_h` (a `PixelView` field, threaded via `PipeAux`), `pixelate_lock`
  keeps it square. `apply_pipeline` reads both (height <2 ⇒ square fallback). A width-off/
  height-on vertical mosaic still renders because `pipeline_active` also checks `pixelate_h`.
- **CRT post-FX (`PostFx` value, threaded as `aux.fx`)** — four marker ops baked into the
  buffer (so they save + apply everywhere): `Scanlines` (amount/spacing/direction
  H `==`/V `||`/diag `\\` `//`/color), `Glow` (luminance-weighted bloom via a twice-run
  `box_blur_rgb`), `Vignette` (smoothstep edge darkening), `Phosphor` (RGB aperture-grille
  mask). `PostFx` persists as one record (`POSTFX_KEY`, `to_record`/`from_record`);
  `PostFx::active()`/`key()` gate the pipeline + cache. Config in the "Post FX" section;
  reorder each via its marker row in the Adjustments list.
- **Color balance** = `out = in + (color − 128)·2·strength` per channel (the picked
  R/G/B/hex color read as a ±offset around neutral grey), clamped. `balance_offset()`
  resolves it to `[i16; 3]` and feeds `pipe_aux`; `parse_hex` powers the hex paste field.
- **Vibrance** = saturation push weighted by `(1 - s)` so muted colors move most,
  vivid ones are protected, neutrals (s=0) stay neutral. **Invert** blends toward the
  negative (0 = original, 1 = full, in between = partial/solarize).
- **`pipeline_active()` / `pipeline_key()`** are the canonical "is anything on?" and
  cache-key helpers — they fold in dither (method/amount + custom matrix + per-axis cell),
  the balance offset, **resize**, **`postfx.key()`**, and **`pixelate_h`** on top of
  `adjust.key()`, so changing *any* stage invalidates the preview/full/grid caches. Use
  them, not bare `adjust.is_identity()`/`adjust.key()`.
- **Persistence:** values in `ADJUST_KEY` (`[f32; 12]`; legacy `[f32; 11]` via
  `from_array11`), order in `ADJUST_ORDER_KEY` (`[u8; 19]`; legacy `[u8; 15]`/`[u8; 12]`
  still load — `with_order` appends the missing ops so an old config gains the new ones at
  the end of its stack). `Adjust::key()` includes the order so reordering invalidates the
  caches. `with_order(&[u8])` tolerates corrupt/short/legacy orders. Dither/balance/scale/
  pixelate/resize/post-FX persist separately (`DITHER_CUSTOM*`, `DITHER_SCALE*`,
  `BALANCE_*`, `PIXELATE_*`, `RESIZE_*`, `POSTFX_KEY` keys).
- **UI rows** are reorderable two ways: a painted **drag-handle grip** (`drag_handle`,
  drag → `adjust_drag`, drop reorders) and **⬆/⬇** buttons. Layout per row:
  `grip [label] [==slider==] [value]  ⟲ ⬆ ⬇`, with the `⟲ ⬆ ⬇` cluster
  **right-anchored** (a `right_to_left` sub-layout) so arrows align on every row
  regardless of value-box width; marker rows (Palette/Dither/ColorBalance) show only a
  label (spanning `marker_w`) and keep an empty `⟲` slot to match. The slider+value
  body is the shared `value_slider` helper: the slider has `show_value(false)` and the
  value is a **right-anchored `DragValue`** in a fixed `VALUE_W` slot (so the value
  column is right-justified). Each row paints a **zebra stripe** (odd rows,
  `faint_bg_color`) + **hover highlight** behind its content via a pre-reserved
  `Shape::Noop` slot (`painter().add` then `.set` once the rect is known, so it draws
  *under* the widgets). The Color-balance section reuses `value_slider` for wide
  R/G/B/Strength rows (each with its own `⟲`); Reduce + the Dither dropdown sit under
  the palette quick-chooser (above the long "All palettes" list) so they stay visible.
- **Reduce** (`quantize_on`/`quantize_n`) works on **any** image, not just ≤`SWATCH_CAP`
  art. `reduce_source(path)` returns the colors to median-cut: the extracted palette when
  present (`palettes[path] == Some(Some(_))`), else — for a >`SWATCH_CAP` photo/gradient —
  the distinct opaque colors sampled from a thumbnail decode (`thumb::distinct_opaque_colors`,
  which has no `SWATCH_CAP`), cached in `reduce_src`. Editing/deleting a swatch keeps
  Reduce **checked** (`custom_palette` wins in `active_recolor`); toggling Reduce or
  changing the Colors count are the explicit resets that drop the hand-edit.
- **PixelFX presets (`FxPreset`, `pixelfx: Vec<FxPreset>`, `PIXELFX_KEY`)** — a snapshot
  of the WHOLE recolor stack (adjustments + order, post-FX, dither, balance, resize,
  reduce, active palette), saved/recalled from the **Places → PixelFX** sub-tab (`places_tab
  == 4`, inserted between Local and 16colo without renumbering — display order ≠ index).
  `capture_fx_preset`/`apply_fx_preset`; portable fields (`order` as `Vec<u8>`, `postfx`
  as a record) + `#[serde(default)]` so a preset survives new ops/params. Each preset has
  a background `color` + text `fg` tag (auto-contrasts when `fg` is None); `ansi32_swatch_grid`
  backs both pickers. **serde derive is used** here (added to `Cargo.toml`; serde+serde_derive
  were already in the tree via eframe/serde_json). **Bundled presets:** a curated retro set
  (CGA/Gameboy/Atari/EGA/…) ships in the binary — `assets/pixelfx_builtin.ron` (the same compact
  RON `eframe` persists `Vec<FxPreset>` as) is `include_str!`d + parsed by `builtin_fx_presets`
  (via the `ron` crate, already transitive through eframe). `new()` calls `merge_builtin_fx_presets`
  to overlay them onto the user's saved list — appending any bundled preset whose **name** isn't
  already present (user edits win by name; a fresh install gets the whole set; a deleted builtin
  re-appears next launch, like the bundled palettes). Every bundled preset references only
  `<built-in palettes>` (the embedded palette sentinel), so it resolves on any machine. To add one:
  save it in-app, copy its RON record from `<data>/app.ron`'s `pixelfx` value into the asset file.
  **Folders:** each preset has an optional `folder` (`FxPreset.folder`; the bundled set is
  `FX_FACTORY_FOLDER` = "Factory", set in `builtin_fx_presets` not the RON so the blob stays the
  exact eframe format). The tab renders **top-level presets first, then one collapsible
  `CollapsingHeader` per folder** (Factory `default_open(false)` = **starts collapsed**; a stable
  `id_salt` so the "(N)" count in the label can't reset the open state). Save into a folder via the
  "folder (optional)" field; re-file via a preset's right-click **Move to ▸** (Top level / an
  existing folder / ＋ the typed folder). `merge_builtin_fx_presets` **migrates** a folder-less copy
  of a builtin (the pre-folder release) into Factory, but leaves one the user re-filed elsewhere.
  The grouped render builds an owned `Vec<FxRow>` + a `render_row` closure so the nested
  `CollapsingHeader` closures don't borrow `self.pixelfx` (all edits go through the `fx_*` deferred
  locals, incl. `fx_move` / `fx_rename_edit`). **Factory presets are read-only** (`is_builtin_fx_name`,
  keyed on the name not the folder — a user preset in a "Factory"-named folder stays editable): the
  `read_only` arg to `render_row` gates the inline rename + swaps the whole context menu for an
  "apply only" note, so a bundled preset can't be renamed/moved/removed/recolored into a resurrecting
  duplicate. Fork one by applying it and Save-ing a copy (lands top-level / your folder).
- **Applies to 16colo.rs browsing too.** The remote-thumb upload (`colo_thumbs.drain`) now
  stores the rendered PNG's CPU pixels in `thumb_rgba` (previously only `thumb_tex`), so
  `grid_recolored_tex` can recolor 16colo tiles (LINEAR — they're rendered previews);
  `make_preview` falls back to that thumbnail when the raw art isn't downloaded, so a
  hovered piece recolors in the details pane.
- **Swatches + Export/Save** sit directly under the preview image (prioritized for
  small screens); the recolor preview + Details thumbnail scale to pane width.

## Viewer / large images

- **`TiledTexture`** uploads a still image as a grid of ≤ `MAX_TEX_DIM` (8192) tiles,
  so images over the GPU texture limit render at full resolution (`load_full`,
  `make_full_reduced`). GIF frames use `load_texture_capped` (downscale-to-fit).
- **Fit = `F`** is a sticky, persisted mode (`fit_mode`, `FIT_MODE_KEY`): toggling it
  on fits now and **auto-fits every newly loaded image** (`load_full`). `.` is unbound.
- **Per-kind viewer zoom, in device pixels (`N×`).** Text-mode/scene art
  (`is_textmode_ext`: ans/asc/nfo/diz/ice/cia/xb/xbin/bin/tnd/idf/adf) renders at a
  tiny **native 8×16 px per cell**, so a true 1:1 is unreadably small. Its zoom is
  measured in **device pixels per source pixel** — `textmode_zoom` (default **3×**, a
  Prefs combo), and the viewer reads out `N×` (not a %), because that's the unit that
  is actually crisp on a fractionally-scaled display (300% logical = a warped 3.9 dev-
  px; `3×` = a clean 3). `load_full` opens at `textmode_zoom / pixels_per_point`
  (logical), Z+N sets `N/ppp`, and Z+/Ctrl-wheel step the device ladder via
  `step_device_zoom`. **The ladder is integer in *both* directions** — `pp_device_scale`
  snaps the device scale to `N×` when upscaling (1,2,3,…) and **`1/N×` when zooming out
  below 1:1** (½,⅓,…,⅟₁₆), so a big or very tall scene can shrink to fit (the old
  `.max(1.0)` floor locked it at 1×; reads out `1/N×`). The viewer textures use
  `view_tex_opts` — **NEAREST magnification, LINEAR minification** — so a zoomed-out
  dither area-averages to grey instead of nearest-aliasing into noise. Raster art instead
  keeps a logical `%` zoom remembered across
  images (`raster_zoom`, persisted as the old `IMG_ZOOM_KEY`; `draw_image_view` writes
  it back each frame), whereas text-mode always reopens at its preference (manual zoom
  is transient). Both persist (`TEXTMODE_ZOOM_KEY`). `viewing_textmode` tracks the kind.
- **CRT aspect** (`crt_aspect`, `CRT_ASPECT_KEY`, **off by default**) is a viewer-only
  toggle shown in the status bar **only for text-mode art**: it stretches the blit
  ≈1.2× vertically (an 80×25 8×16 grid → 4:3) to match non-square VGA pixels. **X stays
  pixel-perfect** (integer device-px, for dither crispness); **Y is the CRT stretch** —
  `ny = round(nx·aspect_y)` *when that's still visibly taller than nx* (high zoom →
  uniform & crisp), else the **exact fractional** `nx·aspect_y` (low zoom). The exact
  fallback matters: a fit-to-screen tall ANSI sits at nx≈1–2, where `round(2·1.2)=2`
  **erased** the stretch — the main view looked unchanged on toggle while the
  linear-sampled previews (continuous 1.2×) did change. It's a *display* scale, not a zoom
  change, and never touches the texture. The
  **navigator minimap, Recolor preview and Details thumbnail all apply the same
  `aspect_y`** so every rendering of the open image agrees (the minimap used to stay
  native → looked squished next to the stretched main view + previews). The previews also
  take their **base aspect from the open image's `full_tex`** (via `preview_aspect`), not
  the downscaled thumbnail's own dims — otherwise a thumbnail decoded at a different width
  (e.g. cached at 8px before a 9px-cell toggle re-decoded the full view) renders squished.
  **But a 16colo flat-listing piece (`colo_pieces`) keeps its thumbnail's own dims** — its
  thumb is 16colo's pre-rendered PNG, a different renderer than our (often very tall) full
  decode, so forcing it to `full_tex`'s aspect squashed the preview into a thin sliver
  (looked "gone"). `make_preview` likewise decodes `resolve_local(path)` so the colorizer
  can recolor an opened 16colo piece (its bytes live at the cache file, not the virtual path).
- **9px VGA cell** (`font_9px`, `FONT_9PX_KEY`, **off by default**) is a separate
  status-bar toggle (next to CRT, text-mode art only) that renders the 8-pixel CP437
  glyph in a **9-dot-wide cell**, the way real VGA text mode did: the 9th column is
  background except for the line-draw range `0xC0..=0xDF`, where it repeats column 8 so
  box rules join (see `ansi::dot_on`). Off = exact 8px cells. Unlike CRT this is a
  *decode-time* change (the texture width grows ~12.5%, 80 cols → 720px), so it's a
  process-global flag (`decode::set_font_9px`, read by `AnsiDecoder`) primed from
  storage on startup; toggling it calls `redecode_full` to rebuild the viewer texture
  in place (keeping zoom/pan). `redecode_full` decodes `resolve_local(path)` (the **real**
  cache file) while keeping the virtual `path` as the stored identity — same split as
  `load_full`; without it, re-decoding a 16colo piece read the *virtual* path off disk,
  failed silently, and the toggle was a no-op. This is why ansilove/16colo (9-dot) render
  wider than a naive 8px blit. Thumbnails aren't re-rendered on toggle (sub-pixel at thumb
  scale).
- **Baud-rate playback** (ANSImation / "watch RIP draw"). The whole engine is "render
  the first N bytes into a fixed-size canvas": `ansi::TextStream` and `rip::RipStream`
  parse a byte *prefix* (canvas sized from the whole file so frames don't resize). The
  viewer's `Player` (parallel to the GIF `AnimState`) holds the stream + a byte cursor
  advanced at `Baud::cps()` × dt, caching the frame texture by cursor position. cps =
  baud/10 (8N1) **× a V.42bis compression factor** (`(baud/3600).clamp(1,4)`): real
  modems compressed ANSI ~4:1, so 14.4k+ "feels fast" like a real board while ≤2400
  stays an authentic crawl. Rates are the modem ladder (300/1200/2400/4800/9600/14.4k/
  28.8k/33.6k/56k/115.2k); RIP and ANSI keep **independent** remembered speeds
  (`baud_ansi`/`baud_rip`, picked by `Stream::is_rip()`). `Stream::for_file` makes a
  byte-prefix player for RIP + ANSI/CP437 streams; the **binary** scene formats
  (XBin/BIN/Tundra/IDF/ADF/PETSCII) aren't byte streams (RLE/headers/embedded
  font+palette), so `load_full` instead wraps their *decoded* image in a
  **`Stream::Cells(CellReveal)`** — a progressive **cell** reveal (first N cells in
  reading order over black, `textmode_cell` gives the 8×16 / 8×8 box) — so they "type
  out" at the baud rate too. The `Player` is unit-agnostic (it advances `pos` toward
  `len()` at `cps`; for `Cells`, `pos`/`len` are *cells* not bytes). A controls row (▶/⏸ · ⏮ Replay · byte
  seek) shows above the art; the **baud picker** is in the status bar. `Baud::None`
  (default) → instant, so the player sits at-rest and the view falls through to the
  static `full_tex` path (recolor/minimap keep working). Picking a baud restarts the open
  file. During playback the view auto-scrolls BBS-style to follow the typing **cursor**:
  `parse` returns the cursor's row extent (`max_y+1`, counting trailing blank lines the
  cursor moved onto — *not* just `grid.len()`), the canvas is sized to it, and
  `render_frame`/`Player.cursor_px` drive `play_autoscroll`. One extra frame renders on
  finish so the scroll lands exactly at the bottom (incl. a blank final line); the pan
  clamp keeps short (fits-the-screen) art put. Perf caveat: a frame re-parses+re-uploads
  the whole prefix, so very tall files at high baud are heavy (most ANSImations are 80×25).
  **Interrupt:** any user input while playing (`interrupt` in `ui_single` — a scroll,
  zoom gesture, or any key press) finishes the playback instantly (`pos=len`, `playing=
  false`) and clears `play_autoscroll` so the user can scroll/pan the full art; that
  input's own nav action is suppressed for the frame. The viewer also scrolls a long file
  with **Up/Down arrows** (Left/Right stay prev/next image), alongside the wheel, plus
  **Home/End** (top/bottom) and **PageUp/PageDown** — a page is **25 character rows** for
  text-mode art (an old 80×25 DOS screen: `25 · textmode_cell.h · scale.y`), else ~90% of
  the viewport for raster. All gated on `overflow_y` and re-clamped to the pan bounds.
- **Slideshow** (`auto_next` + `auto_next_secs` 1/3/5/10s, status bar, persisted). In the
  single view, once the file has *settled* (any baud transmission finished — `Player.playing`
  is the "busy" gate) and the delay elapses, `ui_single` steps to the next file. Dwell
  resets on load and while busy. While slideshow is on, RIP + raster (non-text-mode) art
  opens **fit-to-screen** (`load_full` sets `fit_requested`) so it's fully visible; text-mode
  keeps its readable zoom + fit-to-width. Great for flipping through a whole pack hands-free.
  **Auto-pause** (`auto_paused`, not persisted): while running, any deliberate user
  interaction in the viewer — scroll, zoom gesture, key press, or a drag-to-pan
  (`pointer.is_decidedly_dragging()`; passive mouse *movement* is excluded so a hands-off
  screensaver isn't paused) — sets `auto_paused`, which gates the advance. The status-bar
  "auto ▶" then renders **yellow** (a `RichText` color) with a "you took control — click to
  resume" hover; clicking it while paused **resumes** (the checkbox would set `auto_next`
  false, so the handler forces it back true and clears `auto_paused`) instead of toggling
  the slideshow off. Pause persists across manual navigation until you click to resume.
- **Metadata OSD** (`osd_enabled`/`osd_position`/`osd_secs`, Preferences →
  "Viewer info OSD", persisted). `load_full` resets `osd_t=0` + `osd_dismissed=false` (and
  primes SAUCE via `cached_sauce`) so a fading rounded panel appears on each opened image:
  fade-in (0.35s) → hold (`osd_secs`) → fade-out (0.7s), painted at the end of
  `draw_image_view` (covers static / player / GIF paths). `osd_content` returns a **headline
  title** + a list of `(gap_before, fields)` **rows** so sections get vertical breathing room:
  the title (larger + faux-bold via a 0.7px double-draw), then the **artist(s)** on their own
  line — a collab piece's `", "`-joined `piece.artist` is split so **each artist is its own
  link** (flows `a · b · c`) — then the **SAUCE comment / description** (the COMNT block on
  local scene files — `sauce::parse`,
  which `cached_sauce` reads a ~16 KB tail for since COMNT precedes the 128-byte record; the API
  `Comments` string on 16colo pieces), then an **attributes** row flowing `label value · …`
  (type / columns / lines / font / group / pack / year, or local type / size / dimensions /
  colors / created) — ending in a ★ rating. A one-field row reads as its own line; a
  multi-field row wraps only on overflow. **Placement** (`osd_position` 0..=7, a spatial 3×3
  Preferences picker with the center unused): each index decodes to a horizontal third
  (left/center/right) and vertical third (top/middle/bottom) resolved independently, so any
  corner or edge-center — top-left, top-right, bottom-left, bottom-right, etc. — is a single
  choice. **Interactive:** `paint_osd` returns `(rect, link rects, close rect)` —
  links (`osd_links`) each carry an `open_folder` target (a local directory or a 16colo
  artist/group/pack/year virtual path), and the top-right **`×`** (`osd_close`) **dismisses the
  OSD for this view** (`osd_dismissed`, reset on the next `load_full` so it returns on the next
  image). `draw_image_view` hit-tests last frame's rects (using `hover_pos().or(interact_pos())`
  so *passive* hovering — no button held — counts, which is what re-pins it reliably **even
  mid-fade-out**): hovering **pins** it open (`osd_t` reset to the hold, full opacity) **and
  pauses the slideshow** (`auto_paused`), underlines + pointing-hands the link under the cursor
  (the `×` takes click priority); a link click navigates there. Once pinned, the only ways out
  are the `×` or letting it fade un-hovered. The panel caps to the viewport width and clips
  overflow.
- **Random-pack screensaver** (`shuffle` + "🔀 Random pack" button, status bar). A worker
  (`start_random_pack` → `random_rx`, polled by `poll_random`) picks a random 16colo.rs year
  + pack (`pick_random`, wall-clock seeded — no `rand` dep), inserts its download URL into
  `remote_urls`, and `open_folder`s the virtual pack path (which downloads + mounts it).
  `pending_autoplay` then opens the pack's first art file once both `random_rx`/`remote_rx`
  idle. With **Shuffle** on, reaching a pack's end (a `step_image` no-op — it doesn't wrap)
  triggers the next random pack → endless. Shuffle auto-enables `auto_next`; pair with F11
  for a screensaver of real scene art. **R** jumps to a new random pack (skip a dud).
- **Look defaults** lean to the late-night-BBS aesthetic (`crt_aspect`, `black_bg`,
  `zoom_lock`/Snap, ANSI 4800 / RIP 9600 baud — all `unwrap_or(...)` in `PixelView::new`). The
  **CRT post-FX + slideshow default OFF** though: `crt_scanline_dark` 0, `crt_scanline_scale` false,
  `glow` false, `auto_next` false — a fresh install is clean + doesn't auto-advance; opt into the
  CRT look / slideshow in the status bar. `font_9px` is also default-off: its setter flips a
  process-global the decoder reads, which would leak into parallel decode tests. Persisted values
  always override the defaults.
- **Fit W** button (status bar, single view): re-applies the fit-to-viewport-width zoom
  (sets the one-shot `fit_width_on_open`) after you've zoomed away. Text-mode art already
  fits width on open.
- **Immersive mode** (`immersive`, F11). Hides every panel + the playback controls row,
  drops the window decorations, and goes OS-fullscreen (`ViewportCommand::Fullscreen` +
  `Decorations(false)`), showing only the art (navigator suppressed too). `update` computes
  `show_top/bottom/left/right` from the pointer's distance to each window edge
  (`content_rect`, 48px) and gates each panel group, so a bar reveals when the mouse reaches
  its edge and hides when it leaves. The mouse cursor auto-hides after ~1.5s of stillness
  (`idle_t`; `set_cursor_icon(None)` set last so it wins) and reappears on any movement. Not
  persisted (always starts windowed).
- **Phosphor glow** (`glow` toggle + `glow_amt` slider, status bar, persisted). `paint_crt`
  redraws the art a few times offset in a ring with **additive** blending — the egui trick
  is a premultiplied tint with **alpha 0**, which turns the "over" blend (`src + (1-a)·dst`)
  into `src + dst`. Dark areas add ~nothing, bright glyphs bloom into a halo. Radius tracks
  the zoom (`src_px_dpx`). Reuses the displayed texture (incl. the live player frame), so no
  rebuild. Composes under the scanlines.
- **Retro-monitor scanlines** (`crt_scanline_dark` slider, status bar, persisted, any
  single-tile image). `paint_crt` blits the (panned) art, then tiles a 1×3 opaque-black-row
  scanline texture down the **viewport** (the monitor, not the art — so a scrolling
  ANSImation isn't distorted); darkness is the **draw-time tint alpha** (slider → 0..255,
  no texture rebuild). The `scale` toggle (`crt_scanline_scale`) sets the line period to
  one-per-source-pixel-row (`scale.y·ppp`) so lines grow with zoom; off = fixed 3px. The
  `black bg` toggle (`black_bg`) fills the viewer background black instead of grey-28.
  (Barrel-curvature and vignette modes existed briefly but were dropped as not-useful.)
  Effects skipped for multi-tile (huge) images → plain blit.
- **Pixel-perfect blit (accuracy — read before touching `draw_image_view`).**
  Nearest-neighbor only stays undistorted when one source pixel maps to a **whole
  number of *device* pixels** — and integer `zoom` is not enough, because what counts
  is `zoom × pixels_per_point` (fractional desktop scaling, e.g. ppp≈1.3, otherwise
  duplicates some source pixels more than others → warped dither). So when
  `pixel_perfect` (`viewing_textmode || zoom_lock`) we round to an integer device-px
  scale per axis (`nx/ny`), set `scale = (nx,ny)/ppp` points-per-source-pixel, **and
  snap the image origin to the device grid** (a sub-pixel origin re-warps even at an
  integer scale). `self.zoom` is re-aligned to `nx/ppp` (idempotent), and the status
  bar shows that device scale as `N×` (= `round(zoom·ppp)`) rather than a fractional %.
  The font (`cp437_font.rs`) is the authentic IBM VGA ROM (identical to
  ansilove's BlockZone) — the shade glyphs 0xB0/B1/B2 ARE the canonical 25/50/75%
  dithers; if dither ever looks warped it's the *scale*, not the glyphs.
- **Margin wrap (`decode/ansi.rs`).** Writing the last column parks the cursor at
  `wrap`; the wrap fires at the **top of the parse loop, before the next byte** — like
  ansilove — so it happens whether that byte is a printable char *or an ESC sequence*
  (`ESC[s`, SGR). Checking it only on printable chars (an earlier "deferred wrap")
  mis-saved an `ESC[s` parked at the margin and **sheared cursor-addressed art**
  (ACID-RN.ANS, gj-os.ans) — see `save_at_right_margin_wraps_before_saving`. The top
  check excludes CR/LF, so an exactly-`wrap`-wide line + CRLF still occupies one row
  (no blank), keeping `full_width_line_plus_newline_has_no_blank_row`.

## Font glyph gotcha (solved — two embedded fallbacks + an icon set)

egui's *bundled* fonts (Ubuntu-Light + a small NotoEmoji subset) omit whole Unicode blocks,
so glyphs used to render as tofu (`□`). **`install_fallback_font` (`app.rs`, called first
thing in `PixelView::new`) appends TWO embedded fallbacks** to the Proportional + Monospace
families: **Symbols Nerd Font** (`assets/SymbolsNerdFont-Regular.ttf`) for designed UI icons,
then **DejaVu Sans** (`assets/DejaVuSans.ttf`) to back every remaining standard-Unicode symbol
(Arrows, Geometric Shapes, Misc Symbols, Box Drawing, …). Both are appended *last*, so they
only fill gaps — existing text + color-emoji (`📁`/`🎹`/`🔊`/`★`) come from the earlier fonts
unchanged. Between them, **nothing we draw tofus.**

- **For a real icon, use `icons::*`** (the `mod icons` block near `install_fallback_font`) —
  `PLAY`/`PAUSE`/`STOP`/`VOLUME`/`MUTE`/`LOOP`/`SHUFFLE`/`REFRESH`/`DOWNLOAD`/`SEARCH`/`BOLT`/
  `SORT_ASC`/`SORT_DESC`/`PIN`/`GLOBE`/`MUSIC`/`PIANO`. **Codepoint rule (critical):** use Nerd
  Font's **Material-Design range (U+F0xxx, plane 15)**, *not* the low FontAwesome range (U+F0xx) —
  the text font stores the fi/fl ligatures at U+F001/F002 and egui's bundled emoji-icon-font is
  FontAwesome-based, so a low codepoint gets **shadowed** (music note → "fi"). Plane-15 is
  untouchable by any other stacked font. Verify a new codepoint is in the font's cmap first
  (fonttools: `TTFont(path)['cmap']`; a venv one-liner — see git log).
- **For a plain symbol**, a normal Unicode char (`·`/`…`/`×`/`★`) still just works via DejaVu.
- Only ASCII (`*`) or a *painted* shape (see `drag_handle`'s dots) if something tofus even with
  both fallbacks (very new emoji / CJK / niche pictographs). When in doubt, test in the real app.

## Button-jiggle gotcha (read before a button whose label swaps by state)

egui sizes each widget to its content every frame, so a button whose label changes width
between states — a glyph swap (`▶`/`⏸`, `🔊`/`🔇`), a text swap (`↑ Asc`/`↓ Desc`,
`▶ Play`/`⏸ Pause`), or a live counter — **resizes on toggle and shoves the rest of its
horizontal row sideways** ("jiggle"). Fix: pin such a button to a fixed width with
`.min_size(fixed_btn_size(ui, &["state A", "state B", …]))` — the helper measures the widest
label's galley + button padding (font-robust, no magic px). For a *label* that shoves buttons
(e.g. the single-view zoom readout `N×`/`1/N×`/`%`), reserve a fixed width via
`ui.add_sized([w, h], Label::new(..))` with `w` measured off the widest reading. All the audio
transport / GIF / RIP-baud play-pause, the sortbar Asc/Desc, and the menu-bar mute are pinned
this way — match it for any new state-swapping control.

## Palette-order gotcha (ANSI SGR vs VGA attribute)

There are **two index orderings** for the same 16 colours, and mixing them swaps
red↔blue + cyan↔brown (and their bright variants). `ansi::PALETTE` is in **ANSI SGR**
order (SGR 31=red→index 1, 34=blue→4) — correct for `.ans`, whose parser maps SGR codes
to those indices. The **binary** text-mode formats (`.bin`/`.xbin`/iCE) store *raw VGA
attribute bytes*, where index 1=blue and 4=red, so they must use `ansi::VGA_PALETTE`
(the same colours in hardware order). `render_textmode` indexes whatever palette it's
handed by the raw `attr & 0x0f`, so the **caller** picks the right order: `bin.rs` and
`xbin.rs` (no embedded palette) pass `VGA_PALETTE`; XBIN/IDF/ADF *embedded* palettes are
already VGA-ordered (raw RGB by attribute index) and are indexed directly. Bug symptom:
a piece whose 16colo/ansilove thumbnail is red renders blue in the viewer
(`MULTI-13.BIN`); guarded by `bin::tests::vga_attribute_indices_are_not_ansi_order`.

## Settings & ratings

- **Persistence** uses eframe's `persistence` feature (storage at
  `~/.local/share/pixelview/`). Each setting is its own key (consts on `PixelView`:
  `ZOOM_KEY`, `THUMB_KEY`, `FAV_KEY`, `FOLDER_KEY`, sort/filter keys, `EXPLORER_KEY`,
  `DETAILS_KEY`, `GAP_KEY`/`GAP_Y_KEY` (h/v grid spacing), `CAPTION_KEY` (caption
  bitmask), `KEYMAP_KEY`).
  `persist_egui_memory()` returns `false` — we persist only our keys, in `save()`.
- **Last folder** (`FOLDER_KEY`) is reopened on launch (CLI `--folder` wins). `save()`
  stores the **display** path, not `self.folder` — inside an archive / downloaded 16colo
  pack the latter is a temp dir that's gone next run, whereas the display path
  (`pack.zip/…`, `<16colo.rs>/year/pack`) is stable. `new()` restores it when it's a real
  dir, a 16colo path (re-fetched), or an archive file (re-extracted) — `open_folder`
  routes each. (A subpath *inside* a local archive isn't restored — `is_archive` only
  matches the archive file itself.)
- **Two independent zoom axes:** Ctrl +/- = whole-GUI scale (egui `zoom_factor`);
  Ctrl+wheel = thumbnail tile size only. Ctrl+wheel arrives as `zoom_delta()`, NOT
  `smooth_scroll_delta` (see gotcha).
- **Ratings** (`rating.rs`) read/write `user.baloo.rating` (ASCII `0..10`, 2 per
  star) — the KDE Baloo / Gwenview scheme, so they interoperate with Gwenview and
  the user's `~/git/qb64pe-lab/greywood/sort-by-rating.sh`. Keys 1–5 set, 0 clears
  (removes the attr); single view rates the current image. In the grid/table the **tile
  under the cursor wins** when it isn't part of the selection (`apply_rating`) — so "point
  at a piece and press 3" rates *that* one even with an earlier-opened piece still selected
  (the 16colo flat-listing gotcha); hovering one of the selected tiles still rates the whole
  selection. The shared `entry_context_menu` also has a **★ Rating** submenu (Unrated/1–5
  with the 0–5 hotkeys shown via `Button::shortcut_text`, current marked selected →
  `TilePick::Rate` → `rate_entry`), a reliable rating path for 16colo pieces.
- **Cross-platform sidecar** (`ratings.rs`, `RatingStore`): xattrs only exist for a
  real on-disk file on Linux, but **virtual art has no such file** — archive contents
  are extracted to a *disposable* temp dir and 16colo.rs pieces are downloaded on
  demand. So ratings are also kept in `ratings.json` (in eframe's data dir), keyed by
  the **stable display path** (`/u/pack.zip/SUB/ART.ANS`, `<16colo.rs>/2023/p/ART.ANS`
  — what `to_display` yields). The rule, in `read_rating`/`set_rating`: a path whose
  display ≠ disk (i.e. inside a mount) is **sidecar-only**; an on-disk file uses xattr
  as source of truth (so external Gwenview edits win) **and** mirrors to the sidecar
  (a portable record + the fallback where xattrs don't exist). `make_entry` no longer
  reads the rating itself — `open_folder` overlays every file via `read_rating`, the
  single resolution path. This is what makes a piece **inside a zip or on 16colo.rs
  ratable at all**, and it survives re-extraction because the key is the archive path,
  not the temp file. `set_rating` flushes immediately; `save()` re-flushes on exit.

- **View history** (`viewdb.rs`, `ViewDb`): a **SQLite** store (`views.db` in eframe's
  data dir, via `rusqlite` `bundled`) of visited-state + view count + first/last-viewed,
  keyed by the same **stable display path** as ratings (`view_key` = `to_display` as a
  string). All rows mirror into an in-memory `HashMap` on open so the grid/table can ask
  `is_viewed` for every visible tile each frame without touching SQLite; writes go through
  immediately (user-paced). `ViewDb::open` returns `None` on any SQLite error → the app
  runs with tracking silently disabled (never crashes). **Recording:** `load_full` marks a
  file viewed when opened in the viewer; the top of `open_folder` marks a folder/pack
  viewed when entered (recorded *before* the remote/archive redirect so the key is the
  tile's own path). **Surfaces:** the table filename is a browser-style visited link
  (unvisited = `hyperlink_color` + underline, visited = `weak_text_color`, no underline);
  grid + pack tiles get a *painted* check badge (top-right — `paint_check_badge`, painted
  not glyphed because the bundled font lacks a reliable ✓); the Details pane shows
  `Views`/`Last viewed`; and the shared `entry_context_menu` has **Mark as (not) viewed**
  (`TilePick::ToggleViewed(bool)` → `set_viewed`). Timestamps are unix seconds
  (`unix_now`), formatted via `date_ymd_unix`. **rusqlite is pinned to 0.37** — 0.40 pulls
  `libsqlite3-sys` 0.38 whose build script uses the still-unstable `cfg_select!` and fails
  on stable rustc 1.92 (see Cargo.toml comment).

## Adding a format (the common task)

Copy `decode/pcx.rs`, implement the `Decoder` trait (`name`, `extensions`,
`sniff`, `decode`), and add one `Box::new(...)` line to `Registry::with_builtins`
in `decode/mod.rs`. Use `from_indexed` if the format is palette-based (IFF/ILBM,
LBM), `from_rgba` otherwise. Place the new sniff-able decoder ahead of
`ImageCrateDecoder` if its magic bytes could be ambiguous. Adding extensions to a
decoder's `extensions()` makes them viewable via `known_extension` **and** the two hard-coded
parallel lists in `app.rs` — `is_image_ext` (montages/counts/prev-next; `code.rs`/`audio.rs`
re-export `CODE_EXTS`/`AUDIO_EXTS` so it shares one list) and `is_textmode_ext` (crisp-integer
textmode zoom + CellReveal typeout + SAUCE panel — usually *not* wanted for non-scene files).

**Extension-routed decoders (`code`/`audio`).** A `Decoder::decode` only gets `bytes`, not
the path — but source code needs the extension to pick a language spec and audio needs it as a
format hint. So `decode_bytes` special-cases `CODE_EXTS`/`AUDIO_EXTS` *before* the generic
extension loop, calling `CodeDecoder::decode_ext(bytes, ext)` / `SoundDecoder::decode_ext`
inside `caught(||…)` (the same panic guard as `decode_caught`). Both always return *some* image
(highlighted text / plain / waveform / icon), never an error, so a weird file still shows a tile.

**Audio player + PDF viewer + trackers — all shipped (not feature-gated).**
- **PDF**: the tile is a real `pdftoppm`-rendered first page (placeholder fallback if poppler
  is absent). Opening a PDF enters an in-app **multi-page viewer** (`PdfView` in app.rs):
  Prev/Next + "Page N/M" + a 1-page vs 2-page spread toggle; Left/Right turn pages (not step
  images); each page renders on demand via `render_pdf_page` into `full_tex` (reusing zoom/pan/
  fit); two-page mode composes facing pages side by side.
- **XMind**: opening a `.xmind` enters an in-app **multi-sheet viewer** (`XMindView` in app.rs,
  modeled on `PdfView`): a sheet selector row (⬅ Prev / a `ComboBox` of sheet titles / Next ➡),
  shown only for a multi-sheet file; **Left/Right turn sheets** (single-sheet files fall through
  to normal prev/next-image). Each sheet renders on demand via `decode::render_xmind_sheet(bytes,
  idx)` into `full_tex` (so the existing zoom/pan/fit + Recolor pane + OSD all work on the map).
  `load_full`'s `is_xmind_path` branch sets it up + renders sheet 0; `render_xmind_to_full` /
  `xmind_step_sheet` mirror the PDF pair. `xmind` is in `is_image_ext` (prev/next + montages).
- **Audio** (`AudioPlayer` in app.rs, `rodio`): decodes the file to a sample buffer ONCE, then
  each play appends a fresh `SamplesBuffer` of the selected region to a new `Player` — so it
  restarts cleanly (the old "play once" bug), loops (`repeat_infinite`), and plays a drag-
  selected region. Opening an audio file puts the **full player right in the main viewer**
  (not just the side pane): `load_full` calls `ensure_audio_loaded` (decode + open device
  without necessarily starting — Autoplay starts it) so the transport + waveform + keyboard
  are visible **immediately on open**, no Play press needed. The whole player UI lives in one
  shared method, **`draw_audio_controls(ui, path, big, meta_dur)`**, called two ways: `ui_single`
  draws it `big` (220px waveform + 130px keyboard) filling the viewer when the open file
  `is_audio_ext`; `ui_details` draws it compact (`big=false`, 96px/66px) under the metadata grid.
  **Compact-view width gotcha:** any wide, non-wrapping row in the `big=false` player (a long
  single-line `ui.weak(...)` hint in an unbounded `ui.horizontal`, the pad-edit Loop/Pitch row, the
  editor-source path) forces the whole **left dock** wider than its `size_range` max — and egui then
  leaves an unpainted **clear-color (8,8,8) gap** between the dock and the central panel. So those
  rows are gated `if big` (the transport row uses `horizontal_wrapped` so it's safe); keep new
  compact-view rows narrow/wrapping or `big`-only.
  **Stable header (reserved control slot):** between the transport row and the waveform sit
  CONDITIONAL rows — the "Editing: <source>" row, the per-pad Loop/Pitch row (pad drill-in), and the
  **editor Loop/Type row** (Song/Sample focus) — which appear/disappear (drilling in/out of a pad,
  starting a browser-sample edit) and would **shove the waveform + keyboard + pads up/down**. So the big
  view reserves a **constant 2-row slot** there: record `ui.cursor().top()`, render whichever rows apply,
  then `add_space(target - used)` up to `2·(interact_size.y + item_spacing.y)`. The waveform's Y is then
  fixed regardless of edit state (measure-and-pad, robust to which 0/1/2 rows render). Confirmed by the
  user ("the UI no longer pushed down").
  **Embedded loop points (WAV `smpl` / tracker):** `decode_audio` parses a WAV's `smpl` chunk
  (`parse_wav_loop`, which `rodio` drops) — first loop's start/end frames + type — and
  `extract_tracker_samples` reads xmrs sample loops; both land in `DecodedAudio.loop_region` /
  `SampleBuf.loop_region` = `(start_sec, end_sec, pad_loop_type)`. **WAV loop type maps 0 fwd→0, 1
  alternating→2 ping-pong, 2 backward→1 reverse** (tracker: Forward→0, PingPong→2). On load the loop is
  honored: `from_decoded`/`put_buffer` seed the editor **selection** (`sel_start/sel_end`) + `loop_type`
  from it; `set_pad`/`audition_on_pad` seed a pad's `loop_start/end/type` + turn `loop_on` on. The editor
  **Loop + Type combo** (the reserved-slot row above) edits `AudioPlayer.loop_type`, and `play_source`
  honors it — reverse plays the region backwards, ping-pong appends `reverse_frames` so a `repeat_infinite`
  alternates (same shaping as `build_pad_region`). Pad WAVs written by `write_wav_16` carry no `smpl`
  chunk, so a pad's loop persists via its `Pad` record, not re-parsing. `load_full` resets `edit_focus`
  to `Song` so a freshly-opened file shows the editor Loop row, not a stale pad drill-in.
  It draws an **interactive waveform** (hi-res peaks + a moving loop-aware playhead + shaded
  region): drag = set loop region, click = seek; plus a loop toggle, an **Autoplay** checkbox
  (persisted: play on select + loop until Stop), a Stop button, and **Spacebar** play/pause. The
  method collects `want_*` locals while drawing and applies them at its own end (it's a
  `&mut self` method, not an egui closure, so no caller-side deferral). The device opens
  **lazily + fallibly on first Play** — a headless box reports "no audio output" and `cargo test`
  never touches a device. Leaving the file (`load_full` on a non-audio path) tears the player
  down so it can't keep looping.
- **Trackers** (MOD/XM/S3M/IT): `AudioPlayer::open` routes tracker extensions to **`xmrs`**
  (pure Rust — parses + synthesizes to interleaved-stereo i16 → f32) into the same sample
  buffer, so loop/region/playhead/waveform all work for modules too. voc/au (no in-app
  decoder) keep the icon tile + external open.
- **More trackers** (669/FAR/OKT/MED/AMF/ULT/MTM/STM): no pure-Rust player exists, so **libxmp**
  (MIT, vendored under `vendor/libxmp`) is **compiled from source** by `build.rs` (a `cc` build —
  no cmake/autotools/system dep; it's self-contained + only links libm). `build.rs` compiles
  exactly the files in libxmp's own `cmake/libxmp-sources.cmake` full list (a plain `src/**.c` glob
  pulls in two disabled/leftover ProWizard files that don't compile). `libxmp.rs` is a tiny FFI:
  `xmp_load_module_from_memory` → render to interleaved `f32`, run off the decode worker thread
  (own context per render). It renders **exactly the song length** from `xmp_get_frame_info`'s
  `total_time` with `xmp_play_buffer(loop=1)` — many modules loop forever, and 10 min of stereo
  f32 is 211 MB, so a blind cap would over-render. `is_tracker_ext` is now just MOD/XM/S3M/IT
  (which also get the sample explorer); `is_libxmp_ext` handles the rest.
- **MIDI** (`.mid`/`.midi`/`.kar`/`.rmi`): a MIDI file is only note events, so `render_midi`
  synthesizes it to PCM through a **General MIDI SoundFont** via `rustysynth`
  (`MidiFileSequencer::render`) — then it feeds the same player path as any audio. The SoundFont is
  a persisted preference (`midi_soundfont` / Preferences → "MIDI SoundFont"), else auto-detected
  from common system paths (**preferring a small ~6 MB TimGM6mb over a 100+ MB FluidR3** — the load
  is synchronous). The loaded font is cached in `midi_sf_cache` (an `Arc`, parsed once per session,
  not per MIDI); changing it clears that + the decode cache. `None` available → a "set one" note.
  `.rmi` is RIFF-wrapped MIDI; `rmid_inner` strips the wrapper before synthesis.
- **RAD** (`.rad`, Reality Adlib Tracker): **OPL2/OPL3 FM synthesis**, not PCM — so neither `xmrs`
  nor rodio can play it. `render_rad` drives two **public-domain C→Rust ports**: `decode/rad.rs`
  (the RADPlayer replayer — `new()` parses v1/v2, `update()` emits OPL register writes per tick,
  `hz()` the tick rate, end-of-song via the `RAD_DETECT_REPEATS` order map) feeds `(reg,val)` writes
  into `decode/opl3.rs` (the "Opal" OPL3 chip emulator — `write_reg` + `sample()` with an internal
  49716→44100 resampler), pulling `sr/hz` samples per tick. Both are faithful ports (tables verbatim,
  wrapping arithmetic, bounds-guarded so a malformed file returns `None`/never panics). Ported (not
  the GPL `opl-emu` crate) so pixelview stays MIT.

Both player surfaces also have an **onscreen piano keyboard** (`piano_keyboard(ui, octave, h,
highlights, pad_keys) -> (picked, hovered)` — `h` sizes it big vs compact) + Oct −/+ to audition
the sample as a one-shot instrument — a key plays the selected region pitch-shifted via rodio
`speed()` (2^(semitone/12)); `AudioPlayer::play_note` / `play_speed` keep the playhead correct at a
pitched tempo. Keys a **pad** is mapped to get a little **pad chip** (in the pad's tag colour) — the
sole key↔pad mapping indicator. (`piano_keyboard` still returns the `hovered` key, but the caller
ignores it: an earlier "hover a mapped key → outline its pad" highlight was removed — the bright
outline jumping to a distant pad as the pointer crossed the keyboard read as a selection, not a hint.)
The keyboard
**auto-ranges to keep every mapped pad visible**: `piano_keyboard` starts no higher than the
lowest mapped pad note's octave (`start = octave.min(floor(min pad_key / 12))`) and draws enough
octaves to reach the highest, so pads mapped **below the default C4 view** (the common case with a
hardware grid controller like a Launchpad, whose learned notes sit in the C3 range) still get a key
to chip — otherwise their chips fell off the left edge and never drew. Keys shrink a little for a
wide span; with no pads it's just `octave` filling the width. `octave` (Oct −/+) still pans, and
`picked`/`hovered` stay absolute (`base + semi`, i.e. `midi − 60`) so click-to-trigger is unaffected.

**Master audio controls** live flush-right in the **menu bar**, shown only while the audio
plugin is on (`self.plugin_audio`): a **🔊/🔇 mute** speaker toggle, a **global ⏹ stop**, and a
**volume slider** (reading left→right; added right-to-left in a `Layout::right_to_left` block).
Volume/mute are `audio_volume` (0..1) + `audio_muted`, both persisted (`AUDIO_VOLUME_KEY`/
`AUDIO_MUTED_KEY`) and pushed live to the player via `set_audio_volume`/`toggle_audio_mute`.
Because `play_source` rebuilds a fresh rodio `Player` on every play (to cleanly replace a
finished/looping source), the gain is re-applied there from `AudioPlayer::effective_volume()`
(= 0 when muted, else `volume`) — not just once; `apply_volume()` handles a live slider drag.
`AudioPlayer` mirrors the two values (seeded from `self.audio_volume`/`audio_muted` wherever a
player is created — `ensure_audio_loaded` + `toggle_audio`). The menu-bar handlers defer through
`audio_stop`/`audio_mute`/`audio_vol` locals applied after the `MenuBar` closure (it can't
borrow `self` twice). **Master volume scales pad voices too:** a pad voice stores a `base_gain`
(pad vol · solo/mute · velocity, **no** master) and its live gain is `base_gain ·
effective_volume()` — set at trigger AND re-applied to every sounding voice by `apply_volume()`,
so the fader is heard live even on a sustained looping pad (before, the pad gain omitted
`audio_volume` and only master *mute* reached the pads).

**Sample explorer (a swappable-buffer model).** A tracker module carries a bank of individual
samples; `AudioPlayer.tracker_samples: Vec<NamedSample>` holds them (`extract_tracker_samples`
walks the xmrs module's `InstrDefault.sample` lists → mono `f32` at a C-4-derived rate:
`8363 · 2^(relative_pitch/12)`, with precomputed peaks). Below the keyboard, `draw_audio_controls`
lists them: click one to **load** it (the transport/waveform/keyboard all follow it) or ⬇ **export**
it as WAV. The mechanism is a **swappable buffer**: `SampleBuf` bundles samples/channels/rate/
duration/peaks; `select_sample(Some(i))` stashes the song via `take_buffer` → `song_backup` and
`put_buffer`s the sample; `select_sample(None)` restores the stash — so **only one song copy is ever
held** (the "Full song" row reverts). `write_wav_16` is a tiny dependency-free 16-bit PCM writer.

**Hardware MIDI input (`midir`).** Pick a controller in the big player's "MIDI in:" combo
(`midi_input_port_names` enumerates with a throwaway `MidiInput`; `open_midi_port` connects — the
callback runs on **midir's own thread** and sends `(note, vel, on)` over an mpsc `Sender` passed as
the callback's data). `poll_midi(now)` (called each frame in `ui()`) drains the receiver and routes
each note-on through **`route_note_on(note, vel, now)`** (see the sample-pad grid below): a note
matching a loaded pad triggers that pad, otherwise it auditions the editor sample via
`AudioPlayer::play_note_vel(note − 60, vel)` (MIDI middle C = native pitch, velocity scales gain,
Note Off ignored — monophonic one-shot "preview"). `midi_conn` **must be kept alive** (drop =
close); `connect_midi(None)` closes it. The chosen device persists (`MIDI_PORT_KEY`), auto-reconnects
on launch when still present, and shows a **✓** in the big player's "MIDI in:" row when connected.
The callback also calls **`ctx.request_repaint()`** (via a stored `egui_ctx` clone passed to
`open_midi_port`) — otherwise a note while the UI is idle isn't drained by `poll_midi` until the next
repaint (a mouse move), which reads as "MIDI doesn't play until I move the mouse". midir's ALSA-seq
backend links the **same `libasound2`** rodio already needs — no new system dep.

**Sample-pad grid (a mini Battery) + kits.** The big audio player (`draw_audio_controls(.., big=true)`)
splits below the waveform into a **fully resizable** layout — draggable dividers (`drag_h_divider`)
set the **waveform height** (`AUDIO_WAVE_H_KEY`) and **keyboard height** (`AUDIO_KB_H_KEY`), and a
vertical divider sets the **sample-list pane width** (`AUDIO_LEFT_W_KEY`). The right column has a
**multi-octave keyboard** (`piano_keyboard` fits as many whole octaves as the pane width allows at
~34 px/white-key, C keys labeled by octave) above a **4×4 pad grid** (`draw_pad_grid`) that **fills
the remaining vertical space** (`cell_w`/`cell_h` divide the pane's width/leftover height, so the 16
pads fit the bottom exactly — the dividers reshape them). The big audio view is a
`ScrollArea(auto_shrink=[false;2])` so `available_height()` is real. Side docks set `min_size` so a
drag can't shrink them into a black sliver. A `Pad` (`pads: Vec<Pad>`, always
16) holds `buf: Option<Arc<SampleBuf>>`, an assigned MIDI `note`, `volume`/`muted`/`soloed`, and
per-pad **pitch / loop region / loop_on / loop_type (Forward/Reverse/Ping-pong)** + a **`note_lock`
key-lock** (see drag-move below), plus **`pan`** (−1..1), a **`choke_group`** (0 = none, 1–8), and an
**amplitude ADSR** (`amp_env_on` gate + `amp_attack`/`amp_decay`/`amp_sustain`/`amp_release`). Pan is
an Ableton-style `pan_knob` in the tile's top-right (drag vertically, double-click to center); choke +
the envelope controls live in the pad drill-in editor row. All are **baked into the voice at trigger
time** — `apply_amp_env` shapes the region gain **only when `amp_env_on`** (release fades a one-shot's
tail; a looping pad holds sustain) and `apply_pan` applies an equal-power law (expanding a mono region
to stereo → `trigger_pad_voice` takes an explicit `channels`); a **choke** stops every other live voice
sharing the group before the new one starts. All persist in `Pad::record`/`from_record` (appended
fields 15–21, defaults for old kits; `amp_env_on` at 21 is inferred true for a pre-field kit that
already carried a non-default ADSR) and **export to native SFZ** (`pan`, `group`/`off_by`/`off_mode=fast`,
and `ampeg_*` **only when the envelope is enabled** — off ⇒ the region plays raw with no `ampeg_*`).
**Visual ADSR editor:** the drill-in row's **`[x] (AMP ENVELOPE)`** — a checkbox to enable + a button
that toggles `env_edit`, an **on-waveform overlay** (`env_editing = big && env_edit && drilled into a
pad`) drawing the ADSR contour + draggable handles right on the waveform. **Node handles** (circles):
attack X · decay+sustain X·Y · release X. **Curvature handles** (diamonds at each segment midpoint):
drag vertically to **bow the segment** (`Pad::amp_{attack,decay,release}_curve` −1..1, baked via
`curve_shape` = `t^(2^3c)`; falling segments invert the drag sign so up always bows up). The overlay's
handles are `ui.interact` widgets allocated *after* the waveform `resp` (so a grab claims the pointer;
`Ui::interact` is `&self` so both handle closures can borrow `ui` at once) and the selection
drag-start is gated off while `env_editing`; edits defer through `want_env: (usize, [f32; 7])`
(a/d/s/rel + 3 curves — the player is borrowed immutably while drawing). Enabling a flat envelope
seeds a **duration-proportional** starter shape (`seed_pad_env_if_flat(i, dur)`: no attack, ~12%
decay, ~30% release) so the nodes spread across the width on any sample length instead of clustering
at the top-left; a **Preset** combo (`env_preset`/`ENV_PRESETS` — Reset / Pluck / Perc / Saw down /
Saw up / Gate / Pad, all duration-proportional) applies a named shape to the current target, and
**user presets** (`env_user_presets: Vec<(String,[f32;8])>`, persisted `ENV_USER_PRESETS_KEY`) save
the current envelope's 8 values under a name (the combo lists them with a ✕ delete; a `name` field +
Save button captures/overwrites; apply/delete are deferred out of the combo closure). `env_edit` is
transient (reset on `focus_back` / navigation). A **live pad-voice playhead** (`pad_voice_pos`) sweeps
across while the pad sounds. **BPM grid + snap** (`env_grid`/`env_snap`, persisted; share `self.bpm`
+ `musical_div` with the waveform's Musical grid via `env_grid_step`): draw beat-division lines in the
overlay and optionally snap each node's absolute time to the nearest beat.

**Multi-target envelopes (`EnvTarget` = Amp/Pitch/Cutoff/Res, `env_target`).** The SAME overlay shapes
any of four modulation targets, picked by the `Env:` button row in the drill-in editor (each button
tinted its target color, `EnvTarget::color`). A reusable **`Env`** struct `{on,a,d,s,rel,ca,cd,cr}`
serializes to ONE record field (`to_field`/`from_field`, `;`-joined) so the Pad record stays flat
(indices 28–31: `pitch_env`, `pitch_depth`, `cutoff_env`, `res_env`; amp keeps its legacy flat fields).
The overlay reads/writes the selected target generically via `Pad::env_values`/`set_env_values`/
`env_on`/`set_env_on` (an 8-value `[a,d,s,rel,ca,cd,cr,rel_end]`); `want_env` carries
`(pad, EnvTarget, [f32;8])`.

**MSEG (free-form multi-segment envelope) — an alternate mode per target.** Each target also has a
`Mseg { on, points: Vec<EnvPoint{t,level,curve}> }` (records 38–41; `amp_mseg`/`pitch_mseg`/…). When
`Mseg.on` it **replaces the ADSR** for that target in both baking and export. `eval_mseg` is piecewise
between points (each segment shaped by its destination point's `curve`, holding the last level after
the end = gate). `ModShape { Adsr(&Env) | Mseg(&[EnvPoint]) }` + `eval_shape` unify the two, so the
bakers (`apply_amp_env`, `apply_pitch_mod`, `apply_lowpass_env`) take a `ModShape`; `trigger_pad` picks
`Mseg` if on else `Adsr`. The **overlay** branches on `is_mseg`: the ADSR editor (fixed 4 nodes) vs. the
MSEG editor (arbitrary nodes — drag to move, double-click to add, right-click to delete, segment
midpoint diamonds to curve). MSEG edits defer through `want_mseg: (pad, EnvTarget, MsegEdit)` (Move
clamps a node's time between its neighbours so a drag can't reorder indices). Background (ghost)
envelopes now sample `Pad::eval_shape_at` generically so they render in either mode. **Export:**
`push_flex_eg_points` writes a flex EG from the arbitrary points (inter-point DELTA times) —
MSEG maps to flex EGs exactly. The waveform's own click/double-click/middle-click handlers are gated
off (`!env_editing`) so the MSEG editor owns those gestures. **Gate / release-end node:** each envelope has a `rel_end` (seconds; 0 =
sample end) — a 4th draggable node at the release end lets the envelope finish **before** the sample
(the tail is silenced; `Env::end(dur)` resolves it, `eval_env` returns 0 past it). Amp is baked
through the SAME `eval_env` now (`apply_amp_env(&Env)` — no more bespoke frame math), so amp, pitch,
cutoff and res all share one shape + gate. A gated OR curved envelope forces the **flex-EG** export
(`push_flex_eg` emits a 5-point EG incl. an explicit sustain-hold, no `egN_sustain`, so it plays
straight through and holds 0 = the gate); a plain linear un-gated envelope stays v1. **`eval_env(env,t,dur,one_shot)`**
(0..1, mirrors `apply_amp_env`'s shape) drives the non-amp bakers. **Filter**: a `Biquad` low-pass
(`filter_on`/`cutoff_hz`/`resonance`, records 25–27); `apply_lowpass` (static) or `apply_lowpass_env`
(cutoff swept 30 Hz…`cutoff_hz` log and/or resonance 0…1, coeffs recomputed only when they move).
**Pitch env**: variable-rate resample (`2^(semis/12)`; changes length, capped) — baked FIRST.
**LFOs (`Lfo` per target, `amp_lfo`/`pitch_lfo`/`cutoff_lfo`/`res_lfo`, records 34–37):** tremolo /
vibrato / filter-wobble. `eval_lfo` (−1..1, sine/tri/saw±/square/S&H, `fade`/`delay`/`phase`, and a
tempo-synced `freq(bpm)` from `LFO_SYNC`). Baked: `apply_amp_lfo` (a gain-dip pass), `apply_pitch_mod`
combines the pitch env + LFO into ONE resample, `apply_lowpass_env` combines cutoff/res env + LFO into
the biquad. Trigger order: **pitch(env+LFO) → amp env → amp LFO → filter(env+LFO or static) → pan**.
A faint LFO-shape preview draws centered on the waveform; LFO controls (wave/rate/sync/depth/fade) sit
in an `LFO` row under the envelope controls. Export: native v1 `amplfo`/`pitchlfo`/`fillfo` (SINE,
carrying the resolved Hz incl. tempo-sync + depth + fade) + a v2 flex `lfo04_resonance` (v1 has no res
LFO); **non-sine waves are baked in-app but export as a sine approximation** (SFZ v1 LFOs are sine). **Export
is hybrid per target**: linear amp→`ampeg_*`, pitch→`pitcheg_*`+`pitcheg_depth`(cents),
cutoff→`fil_type`+`cutoff`+`fileg_*`+`fileg_depth`; a **curved** one uses a v2 flex EG via `push_flex_eg`
(eg01 amp / eg02 pitch / eg03 cutoff / eg04 resonance — SFZ v1 has no res EG, so resonance is always
flex; ARIA/sforzando/Bitwig read flex EGs). **The SFZ text is built by the PURE free fn
`build_sfz_text(name, stem, entries, global_vel, bpm)`** (no I/O); `export_sfz` only gathers pads,
runs the dialog, writes the WAVs, then calls it — so the whole opcode-generation path is unit-testable
(`sfz_export_structural_invariants` checks the invariants the linter can't, e.g. **`loop_end` < sample
count** — SFZ `loop_end` is the last sample INDEX, so a full-sample loop must clamp to `samples−1` or
strict players like sforzando disable the region; `dump_sfz_for_lint` is an `#[ignore]` dumper for a
`sfzlint` pass). Every opcode we emit validates clean against `sfzlint` (v1/v2/aria). Pads **auto-map
chromatically from a base note** (`pad_base_note`,
default 48 = C3, a header dropdown); `pad_note(i)` = the individual override (MIDI-learn / a pinned
key-lock) or `base + i`. Per pad: ⟲ load (captures the current editor
selection → `load_pad`, WAV write-through to `<data>/pads/pad_NN.wav`), **e** drill-in editor
(`focus_pad`/`focus_back`: re-points the main waveform at the pad's sample so its loop/pitch/type are
set on the big editor, restored on Back — via `take_buffer`/`put_buffer` + an `EditorStash`, gated by
`EditFocus::{Song,Sample,Pad}`), 🎹 MIDI-learn (`pad_assign` → next note assigns), **🔒 key-lock**
(`icons::LOCK`/`LOCK_OPEN`, `nf-md-lock`; toggles `note_lock` — see drag-move), ⬇ WAV download,
× clear, **M/S** (`pad_is_audible` folds mute + kit-wide solo), a **V** velocity toggle (on = track
the played note's velocity, off = fixed 127), a short **vertical volume fader** on the right edge, and
a painted vertical **VU** (`pad_levels`). The fader is **vertical, not a bottom row** — a horizontal
slider row made the stacked controls taller than the short tile and overflowed into the pad below; a
vertical fader on the right edge costs no vertical row. It's drawn in a **non-advancing `new_child`**
(so it can't disturb the cell's horizontal layout — same gotcha as the pad-grid `scope_builder` bug).
Its **height FILLS the tile** (`fader_top = rect.top()+24`, `fader_bottom = rect.bottom()-14`) so it
stretches on big tiles and shrinks as the window is resized down; a degenerate guard skips it on an
ultra-short tile. **Critical:** a vertical `Slider`'s LENGTH is `spacing.slider_width` (default ~100px),
**not the container height** — so it must be pinned per-tile: `fchild.spacing_mut().slider_width =
fader_bottom - fader_top`. Without that it fell short on a big tile (empty space below) AND overflowed a
small tile's fader rect (the default 100px length spilling past the bottom, clipped right at the border
— the "touching the bottom line" bug). **`fchild.set_clip_rect(rect)`** (the tile) is then the hard
guarantee that the slider handle's circular overhang can never bleed into the pad below.
A pad is triggered by clicking it, or a matching note (onscreen/MIDI) via
`route_note_on`. **A pad click's X-position sets its velocity** (far left = soft 10 → far right =
127), passed to `trigger_pad` which still gates it — only heard with the pad's **V** on (fixed 127
when off; Global velocity wins). **Drop a sample onto a pad to load it**: the Samples-explorer file
rows + the tracker "Samples (N)" rows are drag sources (`PadDrop::File(PathBuf)` / `::Tracker(usize)`
set via `egui::DragAndDrop::set_payload`); a pad tile highlights while a payload hovers
(`dnd_hover_payload`) and loads it on release (`dnd_release_payload` → `load_pad_from_file` /
`load_pad_from_tracker`, both via the shared `set_pad`) — then **drills into that pad** (`focus_pad`)
so "Editing pad N" + its waveform confirm which pad got the sample and you land in trim/loop editing.
**Grid-width gotcha:** `draw_pad_grid`'s cells are sized from `right_w` = `avail − left_w − …`, and
`avail` must be `ui.available_width().min(ui.clip_rect().width())` — bare `available_width()` is
**inflated** by the wide non-wrapping MIDI-in row inside the vertical `auto_shrink=false` ScrollArea
(reports the widest child, not the viewport), which blew the cells up so pads 2/3/4 of each row fell
off the right edge — only column 1 (pads 1/5/9/13) stayed on-screen, so every drop landed on pad 1. **Drag one pad onto another to move/swap
them** (`PadDrop::Pad(usize)` — the tile is `Sense::click_and_drag`, so a plain click still triggers
it but a body-drag sets the payload → `move_pad(src, dst)` swaps the two slots). Each pad's firing
note then either **stays with the pad** (its 🔒 `note_lock` is on → its current absolute note is
pinned as an override so it keeps triggering on the **same key** in the new slot) or **follows the
slot** (lock off → the override is dropped so it takes the destination's `base + index` default) —
notes are resolved *before* the swap since `pad_note` reads the slot index. Because the persistent
kit reloads slot N from `<data>/pads/pad_N.wav`, `move_pad` calls **`persist_pad_wav`** on both
slots (rewrite/remove the WAV) so a swapped kit reloads correctly. **Alt+drag a pad = CLONE**
(`clone_pad`, a green drop-outline + Copy cursor vs. the blue move-outline; keyed off
`alt_down = ui.input(i.modifiers.alt)`): copies the whole pad (Arc buffer + every field) onto the
target WITHOUT removing the source — for quick variations of one sound. **The MIDI map is honoured**:
when the map is LOCKED — the kit-wide **Kit Map Lock** (`kit_map_lock`) *or* the destination pad's own
🔒 `note_lock` — the clone **keeps the destination's key** (its override, or its auto slot default;
only every *other* param is cloned), so the variation triggers on the pad's already-mapped MIDI key;
unlocked it adopts the source's key. (Both locks matter — a global-only lock check was the "clone
reassigns the key even with the midimap locked" bug.) **Per-tile borders are painted in a SECOND
pass** after ALL tile backgrounds (`tile_borders`), so a neighbouring tile can't clip a highlighted
tile's right/bottom edge (the drop-target rendered as a "[" — 3 sides — during a drag). **A pad CLICK also lights its assigned key** on the
onscreen keyboard — `trigger_pad` inserts `pad_note(i)` into `note_flash` (red, since it routes to a
pad), so a click, a keyboard press, and a MIDI note all light the key identically. **`AudioPlayer::trigger_pad_voice`** fires each hit as its own `rodio::Player` on the
**shared `_stream.mixer()`** (`pad_voices: Vec<PadVoice>`, reaped each frame) — the mixer sums them,
so pads are **polyphonic** (the base player is monophonic). The firing body is `fire_pad_voice`
(loop region + full modulation bake + voice), split out of `trigger_pad` (which keeps the
stop-on-repress / mono / choke guards) so a **live edit can re-fire a sounding loop**:
`poll_pad_retrigger` (each frame) hashes the drilled-in pad's `Pad::record()` and, when it changes,
re-fires the loop **on mouse-release** (`pad_edit_sig`/`pad_edit_dirty` + `pad_last_vel[i]` to match
level) — so tweaking loop range / pitch / filter / envelope is heard **without a manual re-trigger**,
and a continuous drag doesn't restart the loop every frame. Only the drilled-in pad's own looping
voice (`pad_has_loop_voice`); the editor player's loop already restarts on selection commit. Feedback (the user's ask): every played
note lights its keyboard key (`note_flash`) — **red** if it routes to a pad, an accent otherwise — and
the triggered **pad flashes green**. **Octave lock** (`octave_lock`) keeps the keyboard octave across
drill-ins **and, while on, auditions a browser-clicked sample at that octave** (`select_sample` takes
a `semitone` = `audio_octave * 12`; the "Full song" always plays native so it isn't sped up).
**Global velocity tracking** (`kit_global_vel` + `kit_global_vel_amt`, a kit-wide checkbox + 0–127
slider in the Pads header) overrides every pad's per-pad **V** with one fixed velocity for the whole
kit. **PANIC** (**Shift+Esc**, `audio_panic` → `AudioPlayer::panic`; a bright-red button in the menu
bar *and* both transport rows) stops all sound + pad voices + MIDI notes, incl. looping pads — the
truly-global escape hatch (the plain back-to-grid Escape excludes Shift). The whole kit is a
**persistent cross-file working set** (metadata in `PADS_KEY`, audio in
`<data>/pads/*.wav`, reloaded in `new()` via `decode_audio`), plus **named kits**: New/Save/Load + a name
field save the kit as a `.pvkit` (a zip — `manifest.txt` storing the kit name + **MIDI controller +
base/octave + global velocity + every pad's record** + `pad_NN.wav`s; `save_kit`/`load_kit` via the
`zip` crate). **New** (`new_kit`) clears all 16 pads + removes their persisted WAVs + resets
name/base/global-velocity to defaults (confirms first via native OK/Cancel when the kit has content,
since the working set can't be undone; keeps the MIDI connection). Saved
kits live in `<data>/kits/`. **Places dock sub-tabs are `Local · PixelFX · 16colo · Kits · Samples`**
(PixelFX is `places_tab == 4`, ordered between Local and 16colo by the button array, *not* by index,
so 16colo/Kits/Samples keep their indices; Kits/Samples audio-plugin-only): the **PixelFX** tab is the
recolor-preset library (see the Recolor section — save current stack, apply, rename, bg/fg colorize,
remove; deferred `fx_*` locals applied after the closure); the **Kits** tab lists saved `.pvkit`s
(click → `load_kit` into the current pads, no navigation) and **opens the standalone pad editor**
(below); **Samples** holds user-added sample folders (`sample_places`: name/dir/color,
`SAMPLE_PLACES_KEY`) — `＋ Add location…` (rfd), click to browse, right-click to rename (inline) /
colorize (ANSI32 swatches) / remove. Kits/Samples gated on `self.plugin_audio`.

**Samples file browser + keyboard hot-swap.** The Samples-tab file list renders **full-width rows
with the table view's look** (zebra `faint_bg_color` on odd rows, hover, and a `selection.bg_fill`
highlight on the selected row — the user asked to "reuse the table style, no headers"), tracked by
`sample_sel: Option<PathBuf>`. Clicking the editor's **source link** (`want_reveal_source`) now opens
the folder AND selects that file. **Renoise-style pane focus** (`sample_focus`): clicking a sample (or
revealing a source) focuses the pane — a blue **focus outline** (`ScrollArea` output's `inner_rect`)
shows it owns the keyboard; clicking a pad (`want_trigger`) or navigating (`show_folder`) unfocuses it.
While focused, the pane's keys drive **file-browser navigation** — all **`consume_key`-consumed** so they
never also fire the grid/viewer nav bound to the same keys (no duplicate-handler clashes). **The list is
subdirs-first-then-audio-files** (`sample_entries` — the same order the pane paints, so keyboard rows line
up), and folders show a `selection.bg_fill` highlight when the cursor lands on them, not just files.
**↑/↓** (`sample_browse_step`) move the selection through folders+files (wrapping); on a FILE it auditions —
on the hot-swapped pad if one's armed (`audition_on_pad`: in-memory buffer swap + `focus_pad` so "Editing
pad N" stays + play, **no WAV write yet**), else loaded into the editor — on a FOLDER it just moves the
cursor. **Enter / →** (`sample_browse_enter`) **descend into** a selected folder, else act on the file
(`hotswap_assign` → `persist_pad_wav` if a hot-swap is armed, else load into the editor); **→** on a file
instead replays. **Backspace / ←** (`sample_browse_up`) go **up a dir** (parent of `sample_browse`,
landing the cursor on the child we came from) — consumed *before* the global `Backspace`→ParentDir nav so
it moves the browser, not the main folder. **Esc** aborts a hot-swap (`hotswap_abort` → restore the pad's
pre-swap `Pad` from `hotswap_orig`; the disk WAV was never touched while auditioning) + unfocuses. The
hot-swap is armed when you reveal
a source / click a sample **while editing a pad** (`hotswap_pad`/`hotswap_orig` captured); with no pad
focus a click just loads into the editor as before. The key handler is gated on the pane being
**visible** (`sample_focus && show_explorer && places_tab == 3 && sample_browse.is_some()`) so a stale
flag can't hijack the keyboard.

**Pad-grid overlap (the real bug: `scope_builder` inside a `ui.horizontal` cell).** The 4×4 pad
tiles overlapped by exactly **8px** — each column's left edge sat 8px INSIDE the previous column's
right edge. Root cause (found by `eprintln!`ing every `allocate_exact_size` rect — don't eyeball 1px
borders): each cell drew its controls with **`ui.scope_builder(UiBuilder::max_rect(inner), …)`**, and
`scope_builder` — like `ui.put` — **advances the parent's cursor by the child's `min_rect`**. `inner`
is inset **14px** from the cell's right edge, so after each cell `scope_builder` yanked the horizontal
cursor back to `cell.right() − 14`, then `+ 6px` item-spacing put the NEXT `allocate_exact_size` at
`cell.right() − 8` → −8px pitch → overlap. **The fix:** draw the controls in a **non-advancing
`ui.new_child(UiBuilder::max_rect(inner)…)`** (borrow it as `let ui = &mut cui;`) so the cell's pitch
is set solely by `allocate_exact_size` (`cell_w + gap`, exact). Same class as the amplitude-slider
gotcha — a child's measured size corrupting the parent layout, cured by `new_child` not
`scope_builder`/`ui.put`. Verified with a `PAD_DEBUG` build: gaps went from −8.0 → **+6.0** between
every column at a 3000px window, and bright-red debug borders showed clean separation. **Lesson:** the
pad-grid `width` parameter + the pad-list clipped-`new_child` (deterministic pane width) were fixes for
a *misdiagnosed* cause (assumed `available_width` jitter / scrollbar bleed) — harmless robustness, but
the overlap was always this `scope_builder` cursor-advance, not the pane width.

**"Pads (N)" list — keyboard focus + nav.** The big audio view's left column shows the 16-pad list
(`audio_sample_list` when `big && pad_list_visible()` — the kit editor, OR the normal audio view
whenever a kit has loaded pads and there's no tracker "Samples (N)" list to show instead; that's why
editing a kit pad from a plain WAV now shows the pad list too, not an empty column). It **auto-focuses**
in the kit editor on
`enter_kit_editor` (`pad_list_focus`, a blue focus outline — mutually exclusive with the Samples pane's
`sample_focus`): **↑/↓** drill the editor into the prev/next **non-empty** pad (`pad_list_step` →
`focus_pad`, so "Editing pad N" + the waveform follow), **Enter** keeps that pad edited and **swaps to
the sample browser** (`pad_list_enter` → open the Samples tab on the pad's source folder, select it,
arm a hot-swap, hand focus to the browser — or, for a kit pad with no source file, just arm the
hot-swap so once you pick a Samples location ↑/↓ audition on the pad), **Esc** unfocuses. Same
`consume_key` gating as the Samples pane so the arrows never leak to grid/viewer nav; clicking a pad
row / a pad tile / navigating (`show_folder`) clears the focus. **While drilled into a pad the big
view's heading shows the PAD's sample** (`Pad N — <name>`), not the file you opened from the grid, so
it's clear which sound you're editing (the pad's buffer *is* in the editor — `focus_pad` swapped it in;
only the heading used to still read the opened file's name). Back-to-Song ⇒ the file name again.

**Standalone Sample-Pads editor (`kit_editor`).** Clicking the Kits tab or loading a kit shows the
pad grid + keyboard + a **silent waveform** with no audio file open (`enter_kit_editor` → `mode =
Single` + `ensure_kit_editor`, which installs a 1 s **silent `AudioPlayer`** at the synthetic
`kit_editor_path()` so the pad mixer has a device + a flat waveform). `ui_single` renders the "🎹
Sample Pads — <kit>" view (heading + ‹ Grid) via the shared `draw_audio_controls(big)`; `kit_editor`
clears on navigation (`open_folder`) or opening any file (`load_full`).

**Waveform editor (`draw_audio_controls`'s interactive waveform).** A pro sample-editor interaction,
resolved BEFORE drawing so the shade/edges track the live drag. `WaveDrag`/`Edge` model the drag:
drag empty = a **fresh** selection (`WaveDrag::Select { fresh: true }`), drag an edge = **resize** it
(`fresh: false`; the opposite edge anchors → crossover swaps L/R), drag inside = move; a green hover
line shows where a new selection begins, edges are bright-green handles (hot on hover/drag), the
cursor is `↔`/Grab; a **full-file selection counts as no selection** so a drag can always start a
sub-selection, and S/E handles are drawn **only for a real sub-selection** (a whole-sample pad shows
none — the old `|| editing_pad` forced phantom S/E at the very edges that read as "already selected").
The anchor is captured from **`pointer.press_origin()`** (the true button-down point), not the
post-threshold drag pos, so a selection starts exactly where you clicked; the moving edge follows the
pointer symmetrically (`(min, max)` of anchor and pointer), so L→R and R→L both work with no
inversion. The `fresh` flag also **suppresses the magnified edge inset** during a new-selection drag —
otherwise it popped up the instant the pointer sat on the new moving edge and hid the waveform you
were selecting on (the inset is for edge RESIZE / hover / wheel-nudge only). `wave_view` is a zoom window: **mousewheel
zooms** around the cursor (consuming `smooth_scroll_delta` so the audio ScrollArea doesn't scroll;
per-sample bars when zoomed in), **Shift+wheel pans**, double-click resets. Over an edge, **wheel
nudges** it by 1 / Shift 10 / Shift+Alt zero-crossing sample(s) (grow vs shrink by direction), and a
magnified **edge inset** (the `zoom_edit_pct` "Zoom Edit %" pref) overlays per-sample detail with an
`@ smp` readout. Move-drag snapping: **Alt+Shift** → nearest zero crossing (`next_zero_crossing`),
**Alt** → nearest transient. **Transients** (`detect_transients`: RMS energy-flux + refractory gap,
sensitivity slider; drawn as culled amber guideline markers) + **BPM** (`estimate_bpm` Detect) + a
**Musical** beat-division grid. `next_zero_crossing`/`detect_transients` are unit-tested (the detector
uses a nonlinear threshold `0.45→0.02·peak-flux` + a `7%` near-silence energy floor, so low
sensitivity is sparse; the caller treats a fully-left slider as **off** and doesn't detect at all).
**Snap** (`snap_transient`, a Transients-row checkbox, persisted) snaps selection edges to
`snap_boundaries(dur)` — a **unified** set of the detected transient onsets (only when Transients is on
AND sensitivity > 0) **plus** the Musical grid lines. So dropping sensitivity to zero hands the grid to
Musical: a fresh selection's anchor snaps at drag-start, its moving edge snaps while dragging, and
**moving a whole selection** snaps its start too (edge-adjust snaps only the moving edge). **Middle-click
a slice**: selects from the boundary at/before the click to the next one after and plays just that slice
(`want_play_region`) — so with Musical on it plays one division (a quarter-note's worth); no boundaries →
play-from-here (`want_play_at`/`play_from`). **Off-editor drag:** an active drag is driven from our own
`wave_drag` state + the **global** pointer (`pointer.latest_pos()`, NOT
`resp.dragged()`/`interact_pointer_pos()`, which egui ties to the widget and reads a stale/wrong X once
the cursor leaves the rect — that was what inverted the selection to span the far edge), so dragging
past the editor edge (or releasing over other UI entirely) stays clamped at the far left/right instead
of being dropped; it ends on the global button release wherever the pointer is. **Dual-waveform
ownership (`wave_drag_big`):** the SAME audio is drawn in BOTH the big central player *and* the compact
Details-pane player at once (both call `draw_audio_controls`, sharing the single `wave_drag`). Only the
instance that STARTED the drag (`big` recorded at `drag_started`) may process/commit it — otherwise the
non-owner maps the global pointer through *its own* far-away rect (→ `t` clamps to `dur`) and, on the
release frame (it draws first), commits that bogus `(anchor, dur)` selection + clears `wave_drag` before
the owner runs, so the selection "reverted to the far-right edge on release." **Undo/redo selections** (`sel_undo`/`sel_redo`
stacks, ↶/↷ in the Transients row): every commit pushes the pre-change selection; cleared by
`clear_sel_history()` when the editor's sample changes. A middle-click also drops a **neon-green ▶
play-from marker** (`play_from_mark`) at the slice/seek origin so you can see where playback began
after the playhead moves on. Selection edges are tagged **S** / **E**. When a
Samples-browser file is being edited, an **"Editing: <name> ↻ <dir>"** row shows under the transport —
the name links to that file's folder in the Samples explorer (`want_reveal_source` → `sample_browse` +
Samples tab), the ↻ reloads it fresh from disk (`want_reload_source` → `load_sample_into_editor`). A
`.pvkit` file (shown with a KIT badge) **loads on click** (`is_kit_ext` → `load_kit`, not the viewer).

**Window geometry** persists (`WINDOW_GEOM_KEY` = `[x,y,w,h]`, captured each frame, restored on the
first frame the monitor size is known, clamped on-screen) — except in `DEBUG_MODE`, where the
bottom-right dev dock wins.

**Window title** (`update_window_title` → `title_location`): `pixelview — <path> (<ui%>)`. In the
standalone kit editor (`kit_editor`) there's no open file, so `title_location` must special-case it —
it shows **`Sample Pads — <kit_name>`** (+ `· Pad N: <sample>` while drilled into a pad), NOT the
generic Single-view fallback (`full_tex` → the selected grid entry), which left a **stale file** (e.g.
the last-viewed `Atmo02.wav`) in the title bar — "the title is wrong for the kit".

**Sample banks as folders (`soundfont.rs` / `sfz.rs` / `dls.rs`).** A `.sf2`, `.sfz`, or `.dls`
is a **virtual folder** of its samples. `is_sample_bank(path)` folds all three into the
`Entry.is_archive` path (📁 glyph + a per-format badge, click-to-enter, prev/next skipping), and
each gets an `enter_*` that mirrors `enter_archive` — extract/mount to a per-file temp dir (cached
by path+size+mtime) via `ArchiveMount`, so `to_display`/`real_path` give the `<file>/NNN_name.wav`
breadcrumb and every sample opens/plays/rates/exports like a real file. A cached `*_info` (`sf_cache`
/ `sfz_cache` / `dls_cache`) feeds the Details pane:
- **SF2** (`soundfont::extract_to_cache`, `rustysynth`): writes each `SampleHeader`'s slice of
  `get_wave_data()` (one shared `&[i16]`) to a 16-bit WAV. Details: Presets / Instruments / Samples.
- **SFZ** (`sfz::mount_to_cache`): an `.sfz` is *text* referencing **external** samples (`sample=`
  resolved against `default_path` + the file's dir), so it **symlinks** each unique sample into the
  temp dir (copy fallback) — no data copy. Hand-rolled opcode parser handles spaces-in-paths, note
  names (`c4`/`f#3`/`60`), and `//` + `/* */` comments. Details: Regions / Samples / Key range.
- **DLS** (`dls::extract_to_cache`): each `LIST:wave` in the RIFF `wvpl` is an embedded WAV, so it
  rewraps each `fmt `+`data` (+ INFO/INAM name) into a standalone WAV bit-for-bit (no re-encode).
  Details: Instruments / Samples.
- **XI** (`xi::extract_to_cache`): a FastTracker II instrument — `xmrs` parses it
  (`XiInstrument::load` → `to_instrument()` → a core `Instrument`), then each sample's PCM is
  written to a WAV at its C-4-derived native rate (`8363·2^(relative_pitch/12)`). Details: Name /
  Samples. (Reachable because xmrs's `tracker::import::xm::xi_instrument` chain is all `pub`.)
- **XRNS / XRNI** (Renoise song / instrument): **ZIP containers**, so they go through the *archive*
  path, not `is_sample_bank` — `is_archive` includes them and `archive::extract_all` opens `.xrns`/
  `.xrni` **explicitly as ZIP** (`extract_zip` via unarc's `ZipArchive`, since unarc detects format
  by extension and would reject the suffix). You browse the extracted tree; the `SampleData/`
  FLAC/OGG/WAV samples play. Full Renoise song *playback* is out of scope (a DAW engine).

Extraction is synchronous on the UI thread (a huge multi-hundred-MB SF2 can hitch on first open;
it's cached after). **REX/RX2** aren't supported: the format is proprietary (RX2 audio is DWOP-
compressed) — decodable only by porting a reverse-engineered codec; deferred, not shipped.

**Decode cache + async load (CPU-intense work).** A tracker re-synthesizes seconds of audio via
`xmrs`, and a MIDI SoundFont can be 100+ MB to load — both far too slow for the UI thread.
`AudioPlayer::open` is split into the device-free `decode_audio` → `DecodedAudio` (the costly step)
and `AudioPlayer::from_decoded` (opens the rodio device). `ensure_audio_loaded`/`toggle_audio` call
**`start_audio_load`**: a `decode_from_cache` hit builds instantly (a memcpy — path+mtime-keyed LRU
`audio_decode_cache`, ~192 MB), a **miss spawns a worker thread** (reads bytes, loads the MIDI
SoundFont if needed, runs `decode_audio`, sends `DecodedAudio` + the `Arc<SoundFont>` back).
`poll_audio_load` (each frame) caches the result and builds the player; `paint_audio_loading_overlay`
dims the viewport + shows an **animated Spinner + "Loading …"** (delayed 0.2s so quick loads don't
flash) — so a slow load reads as *working*, not frozen. `load_full` cancels a pending load on
navigation. Other CPU work is already cached (thumbnails `thumb_tex`, `img_meta`, archive/soundfont
extraction dirs, recolor tiles, minimap, SAUCE); a *persistent on-disk* thumbnail cache (across
restarts) is the remaining unbuilt win. NB **sample-bank extraction** (entering a big `.sf2`) is
still synchronous — the same worker+spinner pattern could wrap it if it bites.

`rodio → cpal → alsa-sys` needs **`libasound2-dev` at BUILD time** on Linux (added to CI + the
first-time-deps list above), so `rodio`/`xmrs`/`midir` are normal (non-feature-gated) deps
(`rustysynth` is pure-Rust/std-only). The slideshow **auto-advance skips PDFs / audio / source-text**
(you'd lose your place / cut a track) — only images, text-mode art and RIP auto-advance.

**The icy ecosystem (Mike Krüger's scene-art formats), the lean way.** Mike's
`icy_engine` *renders* but hard-depends on `icy_net` → **tokio "full"** — too heavy for
a viewer. His **`icy_parser_core`** is the light alternative (no tokio/egui/image, ~2
extra crates): it only *parses*, emitting into a `CommandSink` trait **we implement**.
So the pattern (see `decode/petscii.rs`) is: depend on `icy_parser_core` (pinned by git
`rev` in `Cargo.toml`), implement `CommandSink` to fill our own buffer/canvas, and
render with *our* code (own fonts/palettes) — keeping the pixel-perfect zoom + thumbnail
quality and a lean binary. PETSCII drives `print`/`emit(TerminalCommand)` into a char
grid + C64 font. **RIP** (`decode/rip.rs`) handles `emit_rip(RipCommand)` — a vector
command stream — with **hand-rolled integer BGI primitives** (lines/rects/circles/
ovals/polys/beziers/arcs/flood-fill/8×8 text) onto a 640×350 EGA canvas, no AA, to keep
RIP's pixel-exact look. The primitives are **ported pixel-for-pixel from icy_engine's
BGI renderer** so outlines close and fills don't leak — getting these exactly right was
the whole game (verified by AE pixel-diff vs icy's reference PNGs; 13/16 scenes ≤ 8.6%,
most ≤ 4%): the **line** is the BGI *run-slice* line (whole H/V runs via `fill_x`/
`fill_y`), not plain Bresenham; **béziers** *truncate* their intermediate samples
(`as i32`, not round) — matching `rip_bezier` exactly is what closed the dragon's body
outline (20% → 0.15%); **ellipses/arcs** use the midpoint traversal (`ellipse_offsets`)
and a **circle truncates** the EGA `ASPECT` (only `arc()` rounds) — the 1px that seals
the dragon's eye (`eye_circle_isolation` guards this). **Flood-fill leak guard
(shape-based, not size-based):** even with matched rasterization a residual 1px gap can
let a fill escape, so `flood()` abandons a leak rather than flood the screen. But
abandoning *every* big fill (the old `> W*H/2` rule) wrongly blanked **legitimate
full-screen backgrounds** — the common case in real BBS art. The fix keys on *shape*: a
leak escapes into a finished scene and must weave around every drawn shape, exploding its
**`perimeter²/area`**, whereas a real background is one solid blob (disk≈12.6, square 16).
`flood()` abandons an over-`W*H/2` region only when `perimeter²/area > 40` — the 15 legit
backgrounds in the corpus sit at 16–20 (HOUND, weaving round the dog, at 65), leaks run
95–2185. This **solved the size-guard's "impossible" case**: jdraw's legit 177k-px fill
(p²/a≈20) now fills (56.9%→0.06% vs reference) while garfield's 170k-px leak (p²/a≈112)
is still abandoned (line art preserved, 2.55%). **Irreducible blind spot (measured, not
tunable):** the metric genuinely *overlaps* — HOUND's legit sky (p²/a≈**65**, a
false-positive that stays unfilled) is *more* complex than the real MSG/SH leaks (≈**50**),
and PMID1's empty-sky leak (≈68) is *simpler* than HOUND. So no threshold both fills HOUND
and blocks MSG/SH — raising the `>40` cut to pass HOUND (tried 66) floods MSG/SH. The
genuine fix is making the outline rasterisation exact so nothing leaks and the guard can go
away entirely — i.e. the wholesale icy BGI-renderer port, **not** a per-file hack or a
threshold nudge. Until then HOUND/PMID1 are accepted known-imperfect (project memory).
**Patterned fills
(two BGI quirks — get both or layered dithered art is wrong):** (1) `fill_poly` borders
the shape in the draw colour **only when that colour isn't 0** (`if self.color != 0`,
matching icy_engine + PabloDraw's `FillPoly`/`BGICanvas`) — drawing it unconditionally
painted a black contour seam around every band of a halftone-shaded portrait whose draw
colour is 0 (e.g. ACiD's `US-HUMA1.RIP`). (2) A BGI fill is **opaque**: `fill_span`/
`flood` paint a pattern's *clear* bits with `bkcolor` (black, the BGI default), **not**
leave them transparent — otherwise a 50% dither (`0x55/0xAA`) drawn over an earlier solid
band lets it bleed through the off-bits and the halftone reads too dense. Solid fills
(pattern `0xFF`) and on-black floods are unaffected, so the reference scenes don't move.
**Text:**
font 0 is the 8×8 bitmap; fonts 1–10 (the spec's scalable BGI fonts) are real stroke
fonts in `rip_chr.rs` — the `.CHR` glyphs are stroke lists we render with the same
`line()`. **Stroke text is always drawn thin + solid** (`draw_text` saves `thick`/
`line_pat`, forces `1`/`0xFFFF`, restores after — exactly icy_engine's `out_text_xy` /
PabloDraw's `OutTextXY`): a preceding `LineStyle thick:3` (common right before a title)
would otherwise bold every glyph stroke into a doubled "shadow" (P1-WNC2's "Wind Ninja
Chronicles" block; the fix also cut shadow 1.1%→0.07%, garfield 3.5%→2.6% vs reference).
**Vertical text** (`FontStyle direction:1`) stacks glyphs **top-to-bottom in a column**:
`rip_chr::draw` advances the pen along the text axis (x rightward for horizontal, *y
downward* for vertical) with the cross-axis fixed, and rotates each glyph (`nx = ox +
st.y`, `ny = oy + st.x`). Advancing `pen_x` for *both* directions was the bug that laid
vertical labels out as overlapping rotated glyphs in a row (MAIN/MSG's left-margin "The
Far Side BBS" + "USA"). NB the downward advance is PabloDraw's RIP convention — icy's BGI
`out_text_xy` goes bottom-to-top; we match Pablo (the reference these were diffed against).
*Known limit:* our vertical glyph transform matches Pablo's **direction** but not its
exact per-pixel placement (Pablo's `DrawCharacter` vertical formula isn't published, and
icy's differs in direction), so art that draws vertical stroke text as an outline then
**flood-fills it solid** via a hard-coded seed (MSG's big "USA" — `Fill{529,119}`) has the
seed miss the letters → "USA" renders as a hollow outline instead of solid. Cosmetic; the
real fix is the wholesale icy BGI-renderer port, not a positioning nudge.
**Buttons:** RIP_BUTTON_STYLE/RIP_BUTTON draw the beveled/recessed/chiseled
panels BBS *menus* are built from (`Btn` + `draw_button`/`button_label`, ported from
icy_engine's `add_button`) — so menu screens render (msg5 ≈ 1.5% vs reference); only the
panel visual is drawn (the mouse region is moot for a static viewer). **Image blits:**
RIP_GET_IMAGE/RIP_PUT_IMAGE (`get_image`/`put_image`) capture a screen rect to a clipboard
and paste it under a Copy/Xor/Or/And/Not mode — ported from icy's `image()`/`put_image`,
with the same *upper-exclusive* capture bounds so tiled stamps stay pixel-aligned. This is
how scenes synthesize textured fills (e.g. paleo XOR-stamps shifted copies of a grab to
make a dither, then tiles it: 72%→42% once implemented). **Bézier note:** sample with
icy's exact float ops (`tf.powf(2.0)`/`powf(3.0)`, *not* `tf*tf`) — the ~1-ULP difference
flips a truncated (`as i32`) sample at integer boundaries and was leaving a 1-px outline
gap. **Leak guard — size can't separate, but *shape* can:** a size threshold provably
fails (jdraw's legit 177,631-px fill is *larger* than garfield's 169,711-px leak, so any
size cut that passes jdraw passes garfield's leak). The guard instead keys on
`perimeter²/area` (see the leak-guard note above): jdraw's solid fill (≈20) passes, garfield's
weaving leak (≈112) is abandoned — so jdraw now fills (0.06% vs ref) *and* garfield keeps its
line art (2.55%). Remaining blind spot — a leak that floods an *empty* region (PMID1) is
shape-simple and slips through; a complex *legit* background (HOUND, weaving round the dog,
≈65) is over the cut and stays unfilled. Those few need the per-file outline-gap fix.
icy_parser_core also has ATASCII / Avatar / PCBoard / Renegade / Viewdata / IGS / SkyPix.

## egui version gotcha

Pinned to `eframe = "0.34"` / `image = "0.25"`. egui renames/moves symbols even
between *patch* releases. If `cargo build` complains about an egui symbol, it
almost certainly just moved — check the egui CHANGELOG for that version rather
than assuming a logic bug. Already hit and migrated for 0.34.3:

- `eframe::App::update(ctx, frame)` → **`App::ui(ui, frame)`** is now the required
  method (`update` is deprecated). The handler is given a root `egui::Ui`, not a
  `Context`; mount panels with `panel.show_inside(ui, ..)` instead of
  `panel.show(ctx, ..)`, and get the context via `ui.ctx().clone()`.
- `InputState::raw_scroll_delta` → `smooth_scroll_delta`. Also: a **Ctrl+wheel is
  pre-classified as a zoom gesture** — it lands in `zoom_delta()` and
  `smooth_scroll_delta` stays `0` (egui's `zoom_modifier` defaults to Ctrl/Cmd).
- `ComboBox::from_id_source` → `from_id_salt`; menu actions live in `egui::menu`.
- `Context::wants_keyboard_input()` → **`egui_wants_keyboard_input()`**. Used in the
  global key handler's `typing` guard so hotkeys (ratings, Backspace→ParentDir, R→random
  pack) are suppressed while a text field is focused — the explicit `path_edit`/`search`
  flags don't cover the 16colo search box / advanced-search fields, only this does.
- **`ui.put(rect, widget)` advances the layout cursor** — it isn't a pure "draw at this
  rect" overlay. `put` runs `scope_builder` → `advance_cursor_after_rect(child.min_rect())`,
  and for a widget that **doesn't stretch to fill** its `max_rect` (a *vertical* `Slider`,
  in `put`'s `centered_and_justified` layout) the child `min_rect` is *shorter* than `rect`,
  so the cursor is **set** to that shorter bottom — which can be **above** where you already
  parked it, making the *next* row overdraw earlier content. Bit the waveform amplitude
  slider (drawn in a right strip beside an `allocate_exact_size`d wave): the drag hint +
  Transients row painted over the wave's lower half. Fix: draw absolute-positioned widgets
  that must NOT disturb layout in a **non-advancing `ui.new_child(UiBuilder::new().max_rect(r)
  .layout(..))`**, not `ui.put`.

## Testing

`cargo test` runs 238 tests, all headless (228 unit + 10 GUI; plus 12 `#[ignore]`
network / real-trash tests that hit the live 16colo.rs API or the system trash):
- **Unit tests** (`#[cfg(test)] mod tests` per module): PCX decode + sniff,
  `Registry` dispatch (incl. a real PNG via the `image` crate), `make_thumb` /
  `count_colors`, `PixImage` palette expansion, `rating.rs` parse/encode + a
  guarded xattr round-trip, `sorted_filtered_view` (the sort/filter logic,
  extracted from `rebuild_view` so it's testable without a `PixelView`), `sauce`
  (record + COMNT parsing), `sixteen` (JSON → pieces + URL `#`-encoding), the RIP
  rasterizer (golden-scene guards), `viewdb` round-trips, the `blend_toward` tile-bg
  mix, and the sample-pad grid (`Pad::record`/`from_record` round-trip incl.
  loop/pitch/type, `pad_is_audible` solo/mute truth table, and a `wav_bytes_16` →
  `decode_audio` round-trip proving pad-WAV reload needs no separate reader).
- **GUI tests** (`gui_tests` in `app.rs`, via the `egui_kittest` dev-dep with its
  `eframe` feature): `Harness::builder().build_eframe(|cc| PixelView::new(cc,
  CliArgs::default()))` boots the real app with no window and drives menus through
  AccessKit. Custom-*painted* grid tiles have no a11y label, so kittest covers the
  chrome (menus/dialogs/Preferences), not the tiles.

For a **visual** check on KDE Wayland (KWin has no wlroots screencopy, so `grim`
fails): run under XWayland so `xdotool` can target the window, capture with KDE's
`spectacle`:
`env -u WAYLAND_DISPLAY DISPLAY=:1 ./target/release/pixelview --folder DIR &`,
then `WID=$(DISPLAY=:1 xdotool search --name pixelview)`, drive with
`xdotool`/`ydotool` (you're in the `input` group), `spectacle -b -n -f -o shot.png`.

Note: egui's bundled font lacks some glyphs (e.g. `→` U+2192 → tofu); the emoji
arrows `⬅`/`➡` and `…`/`×`/`›`/`★`/`📁` do render. Prefer those or ASCII in UI strings.
