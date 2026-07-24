# AGENTS.md

Guidance for AI agents / LLMs driving **pixelview**. For human docs see
[`README.md`](README.md); for deep implementation internals see [`CLAUDE.md`](CLAUDE.md).

---

## Tool: `pixelview --render` — headless text-art → image converter

`pixelview` converts BBS/scene text art and images to image files with **no window**
(works over SSH and in batch scripts). Command shape:

```
pixelview --render <INPUT>... [-o FILE | --outdir DIR] [--font-9px] [--scale N] [--format FMT]
```

### Rules

- `INPUT` can be one or more **files AND/OR folders**. Inputs must come **right after
  `--render`**, before any other flag.
- A **folder** is scanned (non-recursive) and every art file inside is converted
  (audio, PDF and source-code files are skipped).
- `-o FILE` → **single input file only** (one in, one out).
- `--outdir DIR` → **batch**; each file is written as `<name>.<fmt>` in `DIR` (created if
  needed).
- With neither `-o` nor `--outdir`, output is written **beside each input** as `<name>.png`.
- Output format is inferred from the output extension; `--format png|bmp|tga|…` forces it.
- `--font-9px` = authentic **9-dot VGA cell** (line-draw chars join; matches ansilove /
  16colo widths). Default is the exact 8-dot cell. **Prefer `--font-9px` when the goal is
  faithful reproduction of how the art looks on 16colo.rs / ansilove.**
- `--scale N` = integer **nearest-neighbor upscale** (crisp, no blur).
- **Exit code:** `0` = all ok, `1` = some failed / nothing found, `2` = bad usage
  (e.g. `-o` with a batch, or an unknown `--format`).

### Per-format examples (all text-mode types)

```sh
pixelview --render art.ans -o art.png     # ANSI/ASCII (.ans .asc .nfo .diz .ice .cia)
pixelview --render art.xb  -o art.png     # XBin (.xb / .xbin)
pixelview --render art.bin -o art.png     # raw BIN (SAUCE width)
pixelview --render art.tnd -o art.png     # TundraDraw (24-bit truecolor)
pixelview --render art.idf -o art.png     # iCE Draw
pixelview --render art.adf -o art.png     # Artworx
pixelview --render art.seq -o art.png     # Commodore PETSCII (.seq / .pet)
pixelview --render art.rip -o art.png     # RIPscript (EGA vector)
```

Ordinary images (`png`, `gif`, `bmp`, `jpg`, `pcx`, `svg`, …) render too — an explicitly
named input file is always tried regardless of type.

### Common recipes

```sh
pixelview --render art.ans --font-9px -o art.png        # ansilove-accurate width
pixelview --render art.ans --scale 2 -o art@2x.png      # 2× upscale
pixelview --render pack/ --outdir out/ --font-9px       # convert a whole pack folder
pixelview --render a.ans b.xb c.rip --outdir out/       # several files at once
pixelview --render art.ans --format tga -o art          # force TGA output
```

---

## Repo conventions for code changes

- Rust + eframe/egui, single binary crate (`pixelview`). Pinned `eframe = "0.34"` /
  `image = "0.25"`; `Cargo.lock` is committed.
- Before finishing a change: `cargo fmt` **only your own edits** (the tree has some
  pre-existing rustfmt drift — do not sweep it into an unrelated diff), then
  `cargo check`, `cargo clippy`, and `cargo test` (233 tests, all headless).
- Adding a format = one new file in `src/decode/` implementing the `Decoder` trait + one
  `Box::new(...)` line in `Registry::with_builtins`. See [`CLAUDE.md`](CLAUDE.md) for the
  full architecture (decoder registry, threaded thumbnailer, the recolor pipeline, the
  pixel-perfect blit math, the RIP BGI rasterizer, and the egui version gotchas).
