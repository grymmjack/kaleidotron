# Autonomous build log — pixelview backlog

Started by grymmjack with full approval to build the backlog unattended, PR-per-feature,
merge each so the next builds on clean main. Order: quick-ish → medium → large.

## Plan
- [ ] **Quick-ish** (branch `quick-wins`)
  - [ ] Audio: drag a selected waveform region → drop onto a sample pad (trim at the selection)
  - [ ] Video: "Extract frame → Open in…" (image editors); honors recolor (recolored if on, else source)
- [ ] **Medium**
  - [ ] YouTube channel browser (backend already in main): right-click Go to channel / Pin; channel view w/ Videos|Playlists
  - [ ] Custom video lists / Watch Later (Places tab, add-order grid, rename/delete/open)
- [ ] **Large**
  - [ ] AI generation plugin (pixelmon/soundmon/ansimon) — AI Places tab, tool/style/prompt editors, batches, folder right-click "generate", pad-load. Mirrors the DRAW system.

## Progress log
- (setup) branched `quick-wins` off main @ PR#6 merge. Dice UV "bug" = not a bug (qb64-dungeon needs inside-out); left alone.

### Quick-ish (branch `quick-wins`)
- ✅ **Audio drag-to-pad** — `⠿ → pad` handle in the big transport row; drag onto a pad loads the
  current waveform selection (trimmed) via `load_pad` (PadDrop::Selection). Drills into the pad.
- ✅ **Video "Open frame in…"** — new menu in the video controls: extract the current frame to a
  temp PNG (recolored if the Recolor pane is active, else source — the user's rule) and open it in
  a registered image editor (DRAW/Aseprite/GIMP, filtered to png handlers) or the OS default app.
  Shared `grab_video_frame_now` with the PNG export.

### Medium: YouTube channel browser (branch `yt-channels`, backend was already in main)
- ✅ `YtSource` enum + `YtMsg::PlaylistHit`; `yt_walk` refactored to list search / channel videos /
  playlist videos / a channel's playlists. `start_yt_list` generalizes `start_yt_search`.
- ✅ `open_yt` routes `<youtube>/channel/<id>` (videos), `.../channel/<id>/playlists`,
  `<youtube>/playlist/<Title [plid]>` (folder-like tile → its videos), + pinned-video leaves.
- ✅ Details pane: **▸ Browse channel** (opens the channel in pixelview) next to Web + the channel
  name; `go_to_channel`. Pin a channel via the ★ toolbar once there.
- ✅ Breadcrumb shows the channel NAME (yt_channel_names, learned from results) + friendly
  Channel/Playlists/Playlist segment labels.
- ✅ **Videos | Playlists** switcher on the breadcrumb row while in a channel.
- ✅ Playlist tiles fetch their cover thumbnail (poll_yt + grid request).

### Medium: video lists / Watch Later (branch `video-lists`)
- ✅ `VideoList`/`ListItem` (serde, persisted `VIDEO_LISTS_KEY`); a `<lists>/<name>` virtual root.
- ✅ Right-click a video (YouTube or local) → **＋ Add to list** → ⏰ Watch Later / ＋ New list… /
  existing lists. Captures title + thumb_url so it renders/opens after a restart (add_to_video_list).
- ✅ Places → YouTube tab **Lists** section: click a list to open it (videos in add-order,
  yt_videos re-seeded), ＋ New, right-click Rename (inline) / Delete.
- ✅ `open_folder` routes `<lists>/…` → `open_video_list`; `is_video_entry` gates the menu; picks
  deferred through both grid + table (`apply_add_to_list`).

### Large: AI generation plugin (branch `ai-plugin`)
Mirrors grymmjack's DRAW AI system. **New `src/ai.rs`**: `AiTool`/`AiStyle`/`AiPrompt` (serde),
`{macro}` expansion (`{prompt}` `{seed}` `{outdir}` `{sw}` `{pal}` … + UPPERCASE→upper, `:slug`,
whitespace-trim — unit-tested), a quote-aware `tokenize`, and `run()` (spawn the tool, poll so
Ctrl+Alt+K can kill it, import the NEW files that land in `{outdir}`, skipping `_`/hidden).
- ✅ Preferences → Format plugins → **AI generation** toggle (default off); starter tools seeded
  (echo test + pixelmon + soundmon + ansimon, pointed at sibling `~/git/*` repos — editable).
- ✅ Places **AI tab**: ✨ Generate… launcher + Tools / Styles / Prompts editors (add/remove/inline-edit).
- ✅ **Generate dialog**: tool / style / prompt-preset / prompt / size / count (batch, seeds
  seed..seed+count) / seed; runs async, imports each result into the target folder (or a pad),
  live N/M progress + Cancel. Style prefix/suffix/args/seed-lock applied.
- ✅ Folder right-click empty space → **🤖 Generate images here…**. Ctrl+Alt+K aborts.
- ✅ Verified end-to-end with the DRAW echo tool (macro-expanded args → PNG imported).
- **Deferred** (follow-ups): a per-pad "🤖 Generate sound…" button (backend + dialog already
  support `ai_gen_pad`), the `[?]` live macro-reference, steering images ({limg}/{dimg}), and
  saved generation batches beyond `count`.

## ✅ Session complete — all merged to main
| PR | Feature |
|----|---------|
| #7 | Quick wins: audio drag-selection-to-pad + video "Open frame in…" an editor (recolor-aware) |
| #8 | YouTube channel browser (Browse channel, Videos\|Playlists, channel breadcrumb) |
| #9 | Video lists / Watch Later (add-to-list, Places section, add-order grid) |
| #10 | AI generation plugin (pixelmon/soundmon/ansimon) — tools/styles/prompts, generate dialog, batches, Ctrl+Alt+K |

Build clean, 274 unit/GUI tests pass (the 1 failing `clicking_a_favorite_navigates_to_it` is a
pre-existing flaky GUI-harness test, present on `main` before this session — a spinner-repaint
timing quirk, not from any of this work).

**Deferred / follow-ups** noted inline: per-pad "🤖 Generate sound…" button (AI backend already
supports it), the AI `[?]` live macro reference, steering images, and saved generation batches
beyond `count`. Dice `.obj` mirroring is **not a bug** (qb64-dungeon needs the inside-out winding)
— left as-is.

### Font viewer (branch `font-viewer`)
Preview `.ttf` / `.otf` / `.ttc` / `.otc` (sniffed + by extension). New `decode/font.rs` using
`ab_glyph` (glyph raster, the crate egui uses) + `ttf-parser` (names/metadata) — both already
transitive, no heavy new deps.
- ✅ **Grid thumbnail**: a rendered sample ("AaBbCcDdEe / 0123456789 / Grymm!") in the font.
- ✅ **Interactive viewer**: family/style/glyph-count/monospace/upem header; a **type-to-sample**
  box with a live rendered preview + size slider; a **paged glyph grid** of the font's real glyphs
  — hover shows `U+XXXX`, click copies the char; **📋 Copy** family name / sample text.
- ✅ In `is_image_ext` (prev/next, montages); sample text persisted.
- Unit-tested (parse + render + grid). Verified the thumbnail render on DejaVu Sans.
- **Deferred**: bitmap/scene font formats (.fon, TheDraw .tdf, PSF/BDF/PCF, Type-1 .pfb) — each
  needs its own parser; TDF (scene) is the highest-value next.

### SVG resample-on-zoom (branch `svg-zoom`)
SVG now gets the same **pseudo-vector zoom** as XMind/PDF: zooming in **re-rasterizes from the
SVG source** at a higher resolution instead of upscaling a fixed raster → crisp at any zoom.
- `decode/svg.rs`: `render_svg_at(bytes, target_longest)` (+ shared `rasterize`); `decode` stays
  at intrinsic size (capped) and the re-render bumps it up on zoom.
- Wired into `draw_image_view`'s re-render trigger + `rerender_at` + excluded from pixel-perfect
  (smooth sampling), all keyed off `is_svg_path(open)`. Unit-tested.

### Vector documents: .ai + EPS/PS (branch `vector-docs`)
- ✅ **Adobe Illustrator `.ai`** — modern `.ai` IS a PDF (`%PDF-`), so it renders through the exact
  PDF path (pdfium/poppler) + opens in the multi-page PDF viewer. Added `.ai` to the PDF decoder's
  extensions, `is_pdf_path`, `is_image_ext`. Verified: the PabloDraw logo renders.
- ✅ **EPS / PostScript (.eps/.epsf/.epsi/.ps)** — new `decode/eps.rs` shells out to **ghostscript
  (`gs`)** (EPSCrop → PNG), the poppler/ffmpeg ethos; absent `gs` → placeholder. Sniffs `%!PS`.
- Both gated under the **PDF plugin** (document formats needing an external renderer; default off).
  Unit-tested; verified end-to-end via `--render`.

### TheDraw fonts .tdf (branch `tdf-fonts`)
The user's "and other fonts … .tdf" ask + "Add TDF next". `.tdf` = classic DOS ANSI-art figlet
fonts; one file holds several named fonts (outline / block / colour sub-types). Rather than
hand-parse the fiddly binary (my prototype kept misaligning the header), I depend on Mike Krüger's
**`retrofont`** crate (same icy ecosystem as `icy_parser_core`, MIT/Apache) which fully resolves
every glyph into a uniform `Vec<GlyphPart>` cell stream; `decode/tdf.rs` rasterises those with
pixelview's own CP437 8×16 font + VGA palette (crisp pixel-perfect zoom, matches the other
text-mode decoders).
- `decode/tdf.rs`: `TdfDecoder` (sniff `0x13`+"TheDraw FONTS file"), `font_list`/`render_tdf`;
  `render_string` walks GlyphPart (NewLine/Skip/HardBlank/FillMarker/OutlinePlaceholder→
  `transform_outline`/Char/AnsiChar) into a `(ch,fg,bg)` grid → CP437 blit. Unicode→CP437 via a
  reversed `CP437_TO_UNICODE`. Grid tile spells the font's own name.
- Viewer (`draw_tdf_ui`, mirrors the TTF viewer): file-name header + **font picker** (a .tdf holds
  N fonts) + a **type-to-sample** box (shares the persisted `font_sample`) + a big NEAREST render,
  plus **📋 art** = export the sample as a PNG beside the file. `is_tdf_ext`, added to `is_image_ext`.
- Verified against the user's real corpus (~1200 .tdf): ARCHANA (dripping magenta colour font),
  4Max Colour, Thin Cyan (outline), multi-font files (ASSYLUM/THINX = 4 fonts each) all render
  correctly. Unit-tested (sniff + Unicode round-trip) + an `#[ignore]` corpus dump. 283 tests pass.

### Free image search — Openverse (branch `image-search`)
The user's ask for a "free image search … unlimited art/photos". **Openverse** (`api.openverse.org`,
the CC-search WordPress runs — ~800M CC/public-domain images) is keyless + JSON, so it slots into
the exact virtual-source pattern as 16colo/YouTube/Steam.
- `src/imgsearch.rs` (pure, unit-tested): `<images>` ROOT / `is_remote` / `rel_parts`, `ImgResult`
  (title/creator/license/provider/img+thumb+page urls/dims/ext), `parse_results`, `search(q,n)` via
  the shared HTTP cache (1-day TTL). Ext inferred from `filetype`→url→jpg; licence label "CC BY-…".
- `app.rs` wiring (a leaner `yt_*` sibling — opening a result is a cache-first `cache::get_file`,
  no yt-dlp): `img_results`/`img_files`/`img_rx`/`img_open_rx`/`img_search_cache` fields, `ImgMsg`,
  `img_walk` worker, `open_images` route, `start_img_search`/`poll_img`, `start_img_open`/
  `poll_img_open` (status credits creator + licence for CC attribution), `cancel_img` on nav.
  Folded into `resolve_local`, `activate`, `open_folder`, the poll battery, `any_remote`, and the
  grid/table thumbnail dispatch.
- **Places → Images tab** (idx 8): a search box + "★ Save" pinned searches (re-run on click).
  Results stream as normal grid tiles (Openverse thumbnails), so **recolor / palette / Save** all
  work on them. Click → download-in-place → view locally.
- Verified live against the real API (pixel-art query returns CC-licensed jpgs); unit-tested; 285 tests pass.
- **Also answered**: SWF/Flash — Ruffle (Rust) exists but full embedding is a major lift (own AVM +
  wgpu renderer fighting egui's); recommended a shell-out-to-`ruffle` placeholder as the pragmatic
  path, deferred. Pexels/Unsplash/Pixabay noted as key-required alternatives to Openverse.

### Font viewing improvements + Windows .fon support (branch `font-improvements`)
User feedback on the font browser (+ new asks). Batch:
- **Configurable tile preview text** — the font grid tile had "Grymm!" baked in. Now a process-
  global (`decode::font::set_thumb_sample`, primed from a persisted `font_preview_text`), editable
  in **Preferences → "Font preview sample"** (multiline; default name-free "AaBbCcDdEe / 0123456789").
  Editing it drops cached font tiles so they re-render. Shared by the .ttf AND .fon tiles.
- **Windows bitmap fonts (.fon/.fnt)** — `decode/fon.rs`, hand-rolled (NO crate exists: the crates.io
  `*fnt*` are BMFont/Infinity-Engine, unrelated). Walks MZ→NE→resource table→each `RT_FONT` FNT,
  parses the header + glyph table, decodes the **column-major** 1bpp bitmaps (`data[off + (col/8)*h +
  row]`). Handles v1/v2/v3, proportional + monospace, multi-size files. Confirmed against the FNT spec
  (Q65123 / sgtatham's mkwinfont) + rendered across the user's real corpus (MS Sans Serif ×6 sizes,
  Fixedsys, Terminal). Viewer `draw_fon_ui`: size(face) picker + type-to-sample + paged glyph grid.
- **Glyph-grid cell-size slider** — shared `font_grid_cell` (persisted), added to BOTH the TTF and
  FON viewers.
- **Unicode block / code-page picker** — the TTF glyph grid filters to a chosen block (ASCII, Latin-1,
  Cyrillic, Box Drawing, Braille, CJK, Private Use, …) via `UNICODE_BLOCKS`.
- **Copy sample as image (Character-Map style)** — egui's clipboard is text-only, so added `arboard`
  (already transitive via egui-winit) + `copy_image_to_clipboard`; a **📋 image** button in all three
  font viewers copies the rendered sample as a bitmap to paste into any program. (X11: lives on the
  clipboard while pixelview runs.)
- 286 tests pass.
- **DRAW font-format survey** (user asked for "every format DRAW supports"): DRAW has ttf/otf ✅,
  .FON ✅ (now), plus **.psf** (Linux console) + **.F16/.F08** (raw 8×16/8×8 bitmap dumps) — both
  small + fit this same viewer pattern; next up.

### More bitmap fonts + recolor + TDF export (branch `more-bitmap-fonts`)
Continuing the font work from user feedback:
- **PSF + raw .fNN fonts** — extended `decode/fon.rs`: **PSF1/PSF2** (Linux console, magic-sniffed)
  and the **raw fixed-width `.fNN` dumps** (`.f08`…`.f20`, 8×NN row-major; height derived from file
  size) used by Fontraption / **Moebius** / TheDraw. All feed the same `FonFace` model → same tile +
  viewer. Verified across the user's moebius corpus (F08/F16/F19 = PETSCII/Topaz/CP851) + DRAW's PSFs.
- **Recolor works for fonts (incl. TDF)** — new `recolor_sample(path, img)` runs the full recolor
  pipeline (palette remap + adjustments + dither + post-FX) on the rendered sample, preserving
  transparency. Wired into the .ttf / .tdf / .fon viewers (live preview + the 📋 image copy + PNG
  export), keyed off `pipeline_key()`. So the Recolor pane now tints/palettizes font art.
- **TDF viewer extras** (user asks): a **letter-spacing slider** (negative overlaps letters — i32
  gap, i32 layout so glyphs can overlap without erasing), a **preview zoom slider**, and three
  exports — **💾 PNG** (recolored), **💾 ANS** (renders to ANSI art: CP437 bytes + SGR colour runs,
  VGA→SGR colour fix; round-trips through our own ANSI decoder), **💾 TDF** (the selected font as a
  standalone `.tdf` via retrofont's serializer). All persisted (`tdf_spacing`/`tdf_zoom`).
- 286 tests pass; export round-trips unit-verified (`ans_and_tdf_export_roundtrip`).

### Fixes + "Open in…" for any file (branch `font-and-search-fixes`)
Three from user testing:
- **Recolor didn't auto-apply to fonts/TDF** (had to navigate away+back). Cause: the sample cache
  key used `pipeline_key()`, which covers adjustments/dither/post-FX but NOT the *palette* selection
  (that lives in `active_recolor`). Added a cheap `&self` `recolor_ident()` (custom / selected .gpl /
  reduce) to all three font-viewer sample keys → picking a palette now invalidates + re-renders live.
- **Free image search returned nothing.** The backend was fine (verified 10 live "skull" results
  through ureq); `poll_img` broke on `Err(Empty)` WITHOUT setting `want_repaint`, so once the initial
  input frames settled the repaint loop died and the async results never drained (they'd only appear
  on the next mouse move). Fixed: `want_repaint = true` while pending (matches the other pollers) +
  `img_rx`/`img_open_rx` folded into `net_busy` (busy spinner) + kick at search start.
- **"Open in…" association button in the Details pane** — a `📂 Open in…` menu listing the configured
  programs for the file's extension (+ "Other program…"), so a `.ans` opens straight into PabloDraw /
  Moebius / etc. without the right-click. Set up in View → Associations…
- 286 tests pass.
