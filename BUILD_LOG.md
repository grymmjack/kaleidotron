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
