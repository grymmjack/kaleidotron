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
