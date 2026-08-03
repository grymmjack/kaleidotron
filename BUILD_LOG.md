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
