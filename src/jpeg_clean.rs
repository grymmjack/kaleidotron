//! JPEG cleanup — recover crisp pixel art from a lossy JPEG (or any lossy raster).
//!
//! Pixel art breaks every assumption JPEG makes: hard edges become DCT ringing ("mosquito
//! noise"), flat fills pick up gradient contamination, and a plain background gets chroma
//! bleed smeared into the empty space. This module doesn't try to *undo* the DCT (that's
//! not recoverable) — it **re-quantises the image toward the two properties pixel art
//! actually has**: a flat, keyed-out background and a small palette.
//!
//! Two thresholds do almost all the work:
//! - *"close to the background ⇒ background"* erases the flat-area speckle, and
//! - *"snap to N colours"* both flattens the gradient noise into flat fills **and** hardens
//!   the smeared/ringing edge pixels back into crisp boundaries (each snaps to the nearest
//!   sprite colour or across the background threshold).
//!
//! A final connected-component pass drops isolated foreground specks the thresholds missed.
//!
//! Pure + headless (no egui, no I/O) so it's unit-testable; it reuses
//! [`crate::thumb::median_cut`] + [`crate::thumb::remap_to_palette`].

/// Options for [`cleanup`]. `key` is the background colour to remove; every pixel within
/// `tol` (squared-RGB) distance of it — or already transparent — is treated as background.
#[derive(Clone, Debug, PartialEq)]
pub struct CleanOpts {
    /// The background colour to key out (auto-detected or user-picked).
    pub key: [u8; 3],
    /// How close (RGB distance) a pixel must be to `key` to count as background.
    pub tol: u32,
    /// `true` → background becomes transparent; `false` → filled with `bg_color`.
    pub transparent: bool,
    /// The fill colour for the background when `!transparent`.
    pub bg_color: [u8; 3],
    /// Snap the surviving foreground to a median-cut palette of `colors` entries.
    pub snap: bool,
    /// Target palette size for the snap (ignored when `!snap`).
    pub colors: usize,
    /// Drop isolated foreground islands smaller than `min_island` pixels.
    pub despeckle: bool,
    /// Minimum foreground component size (pixels) kept when `despeckle`.
    pub min_island: usize,
    /// Absorb small **colour** islands (a blob of one colour embedded in a region of another)
    /// into their surrounding colour — cleans stray JPEG dots *inside* a sprite. Runs after the
    /// palette snap (on the quantised colours), so it needs `snap` to be effective.
    pub merge_islands: bool,
    /// Max size (pixels) of a colour island that is a candidate to be absorbed (the "island").
    pub merge_max: usize,
    /// How far out (pixels) to sample the surrounding region when deciding which colour to absorb
    /// the island into — a larger window finds the *real* dominant colour even when the island's
    /// immediate 1-px border is itself noisy.
    pub merge_radius: usize,
    /// Snap the image to a native pixel grid: each `grid_size`×`grid_size` cell takes the majority
    /// vote of its pixels (a colour or background) and becomes that single value. Recovers the true
    /// low-res pixel from a uniformly-upscaled JPEG (set `grid_size` to the JPEG's real upscale
    /// factor) and erases sub-cell edge fringe + noise in one pass.
    pub grid: bool,
    /// The native pixel size (JPEG pixels per art pixel) — the grid cell for `grid`.
    pub grid_size: usize,
}

/// Detect the background colour as the **mode of the border pixels**, coarse-quantised to 4
/// bits/channel so the JPEG noise around one true colour still votes together. Returns the
/// average of the winning bucket's pixels — the real colour, not the bucket centre. Falls
/// back to black on an empty/short buffer.
pub fn detect_background(rgba: &[u8], w: usize, h: usize) -> [u8; 3] {
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return [0, 0, 0];
    }
    use std::collections::HashMap;
    // bucket key (12-bit) -> (count, [sum r, sum g, sum b])
    let mut votes: HashMap<u16, (u64, [u64; 3])> = HashMap::new();
    let vote = |x: usize, y: usize, votes: &mut HashMap<u16, (u64, [u64; 3])>| {
        let i = (y * w + x) * 4;
        let (r, g, b) = (rgba[i], rgba[i + 1], rgba[i + 2]);
        let k = ((r as u16 >> 4) << 8) | ((g as u16 >> 4) << 4) | (b as u16 >> 4);
        let e = votes.entry(k).or_insert((0, [0; 3]));
        e.0 += 1;
        e.1[0] += r as u64;
        e.1[1] += g as u64;
        e.1[2] += b as u64;
    };
    for x in 0..w {
        vote(x, 0, &mut votes);
        vote(x, h - 1, &mut votes);
    }
    for y in 0..h {
        vote(0, y, &mut votes);
        vote(w - 1, y, &mut votes);
    }
    match votes.into_iter().max_by_key(|(_, (n, _))| *n) {
        Some((_, (n, s))) if n > 0 => [(s[0] / n) as u8, (s[1] / n) as u8, (s[2] / n) as u8],
        _ => [0, 0, 0],
    }
}

/// Clean a lossy raster into crisp pixel art. Returns a new RGBA buffer of the same size.
pub fn cleanup(rgba: &[u8], w: usize, h: usize, o: &CleanOpts) -> Vec<u8> {
    let n = w * h;
    if n == 0 || rgba.len() < n * 4 {
        return rgba.to_vec();
    }
    // 1. Background mask: within `tol` of the key colour (or already transparent).
    let tol2 = (o.tol as i64) * (o.tol as i64);
    let mut is_bg = vec![false; n];
    for (i, bg) in is_bg.iter_mut().enumerate() {
        let p = &rgba[i * 4..i * 4 + 4];
        if p[3] < 8 {
            *bg = true;
            continue;
        }
        let dr = p[0] as i64 - o.key[0] as i64;
        let dg = p[1] as i64 - o.key[1] as i64;
        let db = p[2] as i64 - o.key[2] as i64;
        if dr * dr + dg * dg + db * db <= tol2 {
            *bg = true;
        }
    }

    let mut out = rgba.to_vec();

    // 2. Snap the FOREGROUND to a median-cut palette (flattens gradient noise + hardens
    //    edges). Build the palette from foreground pixels *with* duplicates so median_cut
    //    weights by frequency and the rare ringing colours don't earn their own entry.
    //    `remap_to_palette` skips alpha==0, so zero the background alpha first to touch only
    //    the foreground.
    if o.snap && o.colors >= 2 {
        let fg: Vec<[u8; 4]> = (0..n)
            .filter(|&i| !is_bg[i])
            .map(|i| [out[i * 4], out[i * 4 + 1], out[i * 4 + 2], 255])
            .collect();
        if fg.len() >= 2 {
            let pal = crate::thumb::median_cut(&fg, o.colors);
            for (i, &bg) in is_bg.iter().enumerate() {
                if bg {
                    out[i * 4 + 3] = 0;
                }
            }
            crate::thumb::remap_to_palette(&mut out, &pal);
        }
    }

    // 3. Pixel-grid snap: quantise to a native S×S grid by majority vote (recovers the true
    //    low-res pixel from a uniformly-upscaled JPEG, erasing sub-cell edge fringe + noise).
    if o.grid && o.grid_size >= 2 {
        grid_snap(&mut out, &mut is_bg, w, h, o.grid_size);
    }

    // 4. Merge small COLOUR islands into their surrounding colour (stray dots inside a sprite),
    //    sampling a `merge_radius` window so the *real* dominant colour wins even over a noisy
    //    1-px border. Runs on the snapped colours.
    if o.merge_islands && o.merge_max >= 1 {
        merge_color_islands(&mut out, &is_bg, w, h, o.merge_max, o.merge_radius.max(1));
    }

    // 5. Despeckle: drop foreground connected-components smaller than `min_island`.
    if o.despeckle && o.min_island > 1 {
        despeckle(&mut is_bg, w, h, o.min_island);
    }

    // 6. Compose: background → transparent or the fill colour; foreground → opaque.
    for (i, &bg) in is_bg.iter().enumerate() {
        let px = &mut out[i * 4..i * 4 + 4];
        if bg {
            if o.transparent {
                px[3] = 0;
            } else {
                px[0] = o.bg_color[0];
                px[1] = o.bg_color[1];
                px[2] = o.bg_color[2];
                px[3] = 255;
            }
        } else {
            px[3] = 255;
        }
    }
    out
}

/// Flip every 4-connected foreground island (`!is_bg`) smaller than `min` pixels to
/// background. Iterative flood fill (no recursion — a big flat region could blow the stack).
fn despeckle(is_bg: &mut [bool], w: usize, h: usize, min: usize) {
    let n = w * h;
    let mut seen = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut comp: Vec<usize> = Vec::new();
    for start in 0..n {
        if is_bg[start] || seen[start] {
            continue;
        }
        comp.clear();
        stack.clear();
        stack.push(start);
        seen[start] = true;
        while let Some(i) = stack.pop() {
            comp.push(i);
            let x = i % w;
            let y = i / w;
            if x > 0 && !is_bg[i - 1] && !seen[i - 1] {
                seen[i - 1] = true;
                stack.push(i - 1);
            }
            if x + 1 < w && !is_bg[i + 1] && !seen[i + 1] {
                seen[i + 1] = true;
                stack.push(i + 1);
            }
            if y > 0 && !is_bg[i - w] && !seen[i - w] {
                seen[i - w] = true;
                stack.push(i - w);
            }
            if y + 1 < h && !is_bg[i + w] && !seen[i + w] {
                seen[i + w] = true;
                stack.push(i + w);
            }
        }
        if comp.len() < min {
            for &i in &comp {
                is_bg[i] = true;
            }
        }
    }
}

/// Snap the image to a native pixel grid of `cell`×`cell`: each cell takes the **majority vote**
/// of its pixels (a foreground colour, or "background") and the whole cell becomes that single
/// value. This is *downsample-by-mode* — it recovers the true low-res pixel from a uniformly
/// upscaled JPEG and erases sub-cell edge fringe + interior noise in one pass. `is_bg` is updated
/// so the later passes + compose agree. A cell is foreground only when foreground pixels are a
/// strict majority of the whole cell, which keeps edges tight (fringe cells drop to background).
fn grid_snap(out: &mut [u8], is_bg: &mut [bool], w: usize, h: usize, cell: usize) {
    use std::collections::HashMap;
    if cell < 2 {
        return;
    }
    let mut cy = 0;
    while cy < h {
        let y1 = (cy + cell).min(h);
        let mut cx = 0;
        while cx < w {
            let x1 = (cx + cell).min(w);
            let mut bg_votes = 0usize;
            let mut area = 0usize;
            let mut votes: HashMap<[u8; 3], usize> = HashMap::new();
            for y in cy..y1 {
                for x in cx..x1 {
                    area += 1;
                    let i = y * w + x;
                    if is_bg[i] {
                        bg_votes += 1;
                    } else {
                        *votes
                            .entry([out[i * 4], out[i * 4 + 1], out[i * 4 + 2]])
                            .or_insert(0) += 1;
                    }
                }
            }
            let best = votes.into_iter().max_by_key(|(_, v)| *v);
            let cell_fg = (area - bg_votes) * 2 > area; // fg only on a strict majority
            for y in cy..y1 {
                for x in cx..x1 {
                    let i = y * w + x;
                    match (cell_fg, best) {
                        (true, Some((c, _))) => {
                            is_bg[i] = false;
                            out[i * 4] = c[0];
                            out[i * 4 + 1] = c[1];
                            out[i * 4 + 2] = c[2];
                        }
                        _ => is_bg[i] = true,
                    }
                }
            }
            cx += cell;
        }
        cy += cell;
    }
}

/// A same-colour foreground component: its pixels + bounding box.
struct Island {
    pixels: Vec<usize>,
    minx: usize,
    miny: usize,
    maxx: usize,
    maxy: usize,
}

/// Absorb small colour islands into the dominant colour of a surrounding window.
///
/// Two governing sizes: `max_size` (the *island* — a blob up to this many pixels is a candidate)
/// and `radius` (the *influence* window — how far out to sample the surrounding colour). Sampling
/// a window rather than just the 1-px border is what makes it pick the *real* colour when the
/// border itself is JPEG-noisy. Targets are computed from the pre-merge buffer and applied at the
/// end, so every little island independently adopts its region's colour (they don't cascade).
fn merge_color_islands(out: &mut [u8], is_bg: &[bool], w: usize, h: usize, max_size: usize, radius: usize) {
    use std::collections::HashMap;
    let n = w * h;
    let col_at = |i: usize| [out[i * 4], out[i * 4 + 1], out[i * 4 + 2]];

    // 1. Label every foreground component by exact (snapped) colour.
    let mut comp_id = vec![-1i32; n];
    let mut islands: Vec<Island> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for start in 0..n {
        if is_bg[start] || comp_id[start] >= 0 {
            continue;
        }
        let color = col_at(start);
        let id = islands.len() as i32;
        let mut pixels = Vec::new();
        let (mut minx, mut miny, mut maxx, mut maxy) = (w, h, 0usize, 0usize);
        stack.clear();
        stack.push(start);
        comp_id[start] = id;
        while let Some(i) = stack.pop() {
            pixels.push(i);
            let (x, y) = (i % w, i / w);
            minx = minx.min(x);
            maxx = maxx.max(x);
            miny = miny.min(y);
            maxy = maxy.max(y);
            let mut nbrs = [usize::MAX; 4];
            let mut k = 0;
            if x > 0 { nbrs[k] = i - 1; k += 1; }
            if x + 1 < w { nbrs[k] = i + 1; k += 1; }
            if y > 0 { nbrs[k] = i - w; k += 1; }
            if y + 1 < h { nbrs[k] = i + w; k += 1; }
            for &nb in &nbrs[..k] {
                if !is_bg[nb] && comp_id[nb] < 0 && col_at(nb) == color {
                    comp_id[nb] = id;
                    stack.push(nb);
                }
            }
        }
        islands.push(Island { pixels, minx, miny, maxx, maxy });
    }

    // 2. For each small island, vote the dominant surrounding colour in a `radius` window.
    let mut targets: Vec<(usize, [u8; 3])> = Vec::new();
    for (ci, isl) in islands.iter().enumerate() {
        if isl.pixels.len() > max_size {
            continue;
        }
        let x0 = isl.minx.saturating_sub(radius);
        let y0 = isl.miny.saturating_sub(radius);
        let x1 = (isl.maxx + radius).min(w - 1);
        let y1 = (isl.maxy + radius).min(h - 1);
        let mut votes: HashMap<[u8; 3], usize> = HashMap::new();
        let mut total = 0usize;
        for y in y0..=y1 {
            for x in x0..=x1 {
                let j = y * w + x;
                total += 1; // bg + other colours both count toward the majority denominator
                if is_bg[j] || comp_id[j] == ci as i32 {
                    continue;
                }
                *votes.entry(col_at(j)).or_insert(0) += 1;
            }
        }
        if let Some((best, bv)) = votes.into_iter().max_by_key(|(_, v)| *v) {
            // Only absorb when the winning colour genuinely dominates the sampled window — an
            // island floating in mostly-background (an edge speck) is left for despeckle.
            if bv * 2 >= total.max(1) {
                targets.push((ci, best));
            }
        }
    }

    // 3. Apply all recolours.
    for (ci, color) in targets {
        for &i in &islands[ci].pixels {
            out[i * 4] = color[0];
            out[i * 4 + 1] = color[1];
            out[i * 4 + 2] = color[2];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 4×4 test image: black background with JPEG-ish dark speckle, and a 2×2 red block.
    fn sample() -> (Vec<u8>, usize, usize) {
        let (w, h) = (4, 4);
        let mut px = vec![0u8; w * h * 4];
        // fill: near-black speckle everywhere (simulates chroma noise around black)
        for i in 0..w * h {
            px[i * 4] = 6;
            px[i * 4 + 1] = 3;
            px[i * 4 + 2] = 9;
            px[i * 4 + 3] = 255;
        }
        // a 2×2 red block at (1,1)..(2,2), slightly noisy (JPEG'd flat fill)
        let reds = [[210, 12, 8], [200, 20, 4], [214, 6, 14], [205, 16, 10]];
        let mut k = 0;
        for y in 1..3 {
            for x in 1..3 {
                let i = (y * w + x) * 4;
                px[i] = reds[k][0];
                px[i + 1] = reds[k][1];
                px[i + 2] = reds[k][2];
                px[i + 3] = 255;
                k += 1;
            }
        }
        (px, w, h)
    }

    fn opts() -> CleanOpts {
        CleanOpts {
            key: [0, 0, 0],
            tol: 24,
            transparent: true,
            bg_color: [0, 0, 0],
            snap: true,
            colors: 4,
            despeckle: false,
            min_island: 2,
            merge_islands: false,
            merge_max: 8,
            merge_radius: 4,
            grid: false,
            grid_size: 4,
        }
    }

    #[test]
    fn detects_black_background_from_border() {
        let (px, w, h) = sample();
        let bg = detect_background(&px, w, h);
        // The border is all speckle-black; detected bg should be very dark.
        assert!(bg[0] < 16 && bg[1] < 16 && bg[2] < 16, "got {bg:?}");
    }

    #[test]
    fn keys_out_background_to_transparent_and_keeps_foreground() {
        let (px, w, h) = sample();
        let out = cleanup(&px, w, h, &opts());
        // Every border pixel (all background) is now fully transparent.
        for i in [0usize, 3, 12, 15] {
            assert_eq!(out[i * 4 + 3], 0, "pixel {i} should be transparent");
        }
        // The centre red block stays opaque and red-ish.
        for (x, y) in [(1, 1), (2, 2)] {
            let i = (y * w + x) * 4;
            assert_eq!(out[i + 3], 255);
            assert!(out[i] > 150 && out[i + 1] < 60, "kept red at {x},{y}");
        }
    }

    #[test]
    fn onto_bg_color_fills_instead_of_transparent() {
        let (px, w, h) = sample();
        let mut o = opts();
        o.transparent = false;
        o.bg_color = [0, 0, 255];
        let out = cleanup(&px, w, h, &o);
        let i = 0; // a corner (background)
        assert_eq!(&out[i * 4..i * 4 + 4], &[0, 0, 255, 255]);
    }

    #[test]
    fn snap_flattens_the_noisy_fill() {
        let (px, w, h) = sample();
        // 4 distinct JPEG'd reds in the fill; snapping to 2 must reduce the count.
        let mut o = opts();
        o.colors = 2;
        let out = cleanup(&px, w, h, &o);
        let mut colors = std::collections::HashSet::new();
        for (x, y) in [(1, 1), (2, 1), (1, 2), (2, 2)] {
            let i = (y * w + x) * 4;
            colors.insert([out[i], out[i + 1], out[i + 2]]);
        }
        assert!(
            colors.len() <= 2,
            "the 4 JPEG'd reds should snap to ≤2 colours, got {}",
            colors.len()
        );
    }

    #[test]
    fn despeckle_removes_a_lone_pixel() {
        // 3×3 black bg with a single stray non-bg pixel in the middle.
        let (w, h) = (3, 3);
        let mut px = vec![0u8; w * h * 4];
        for i in 0..w * h {
            px[i * 4 + 3] = 255; // opaque black
        }
        let c = (1 * w + 1) * 4;
        px[c] = 200;
        px[c + 1] = 200;
        px[c + 2] = 200;
        let mut o = opts();
        o.snap = false;
        o.despeckle = true;
        o.min_island = 2; // a lone 1-px island is below this → removed
        let out = cleanup(&px, w, h, &o);
        assert_eq!(out[c + 3], 0, "the isolated speck should be keyed to background");
    }

    #[test]
    fn merge_absorbs_a_stray_dot_into_the_surrounding_colour() {
        // 7×7 field of colour A (red) with a single stray blue pixel in the middle.
        let (w, h) = (7, 7);
        let a = [200u8, 30, 30];
        let b = [30u8, 30, 200];
        let mut px = vec![0u8; w * h * 4];
        for i in 0..w * h {
            px[i * 4] = a[0];
            px[i * 4 + 1] = a[1];
            px[i * 4 + 2] = a[2];
            px[i * 4 + 3] = 255;
        }
        let c = (3 * w + 3) * 4;
        px[c] = b[0];
        px[c + 1] = b[1];
        px[c + 2] = b[2];
        // No background here (opaque everywhere), snap off, despeckle off — isolate the merge.
        let o = CleanOpts {
            key: [255, 255, 255], // nothing matches → no background
            tol: 0,
            transparent: false,
            bg_color: [0, 0, 0],
            snap: false,
            colors: 4,
            despeckle: false,
            min_island: 2,
            merge_islands: true,
            merge_max: 4,
            merge_radius: 3,
            grid: false,
            grid_size: 4,
        };
        let out = cleanup(&px, w, h, &o);
        assert_eq!(&out[c..c + 3], &a, "the stray blue dot should be absorbed into red");
    }

    #[test]
    fn merge_leaves_a_large_region_alone() {
        // Two equal 4×8 halves (red | blue). Neither is a small "island" → nothing merges.
        let (w, h) = (8, 8);
        let a = [200u8, 30, 30];
        let b = [30u8, 30, 200];
        let mut px = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let c = if x < 4 { a } else { b };
                let i = (y * w + x) * 4;
                px[i] = c[0];
                px[i + 1] = c[1];
                px[i + 2] = c[2];
                px[i + 3] = 255;
            }
        }
        let o = CleanOpts {
            key: [255, 255, 255],
            tol: 0,
            transparent: false,
            bg_color: [0, 0, 0],
            snap: false,
            colors: 4,
            despeckle: false,
            min_island: 2,
            merge_islands: true,
            merge_max: 8, // each half is 32 px > max → untouched
            merge_radius: 3,
            grid: false,
            grid_size: 4,
        };
        let out = cleanup(&px, w, h, &o);
        let mid_blue = (0 * w + 6) * 4;
        assert_eq!(&out[mid_blue..mid_blue + 3], &b, "large regions are not merged");
    }

    #[test]
    fn grid_snap_recovers_the_native_pixel_and_drops_fringe() {
        // An 8×8 image = a 2×2 native grid at cell=4. Top-left native pixel is red with one
        // stray blue fringe pixel; the other three native pixels are solid green.
        let (w, h, cell) = (8usize, 8usize, 4usize);
        let red = [200u8, 30, 30];
        let green = [30u8, 200, 30];
        let blue = [30u8, 30, 200];
        let mut px = vec![0u8; w * h * 4];
        for i in 0..w * h {
            px[i * 4 + 3] = 255;
        }
        let put = |px: &mut [u8], x: usize, y: usize, c: [u8; 3]| {
            let i = (y * w + x) * 4;
            px[i] = c[0];
            px[i + 1] = c[1];
            px[i + 2] = c[2];
        };
        for y in 0..h {
            for x in 0..w {
                let c = if x < 4 && y < 4 { red } else { green };
                put(&mut px, x, y, c);
            }
        }
        put(&mut px, 0, 0, blue); // one stray fringe pixel in the red native cell
        let o = CleanOpts {
            key: [255, 255, 255], // nothing matches → no background keyed
            tol: 0,
            transparent: false,
            bg_color: [0, 0, 0],
            snap: false,
            colors: 4,
            despeckle: false,
            min_island: 2,
            merge_islands: false,
            merge_max: 8,
            merge_radius: 4,
            grid: true,
            grid_size: cell,
        };
        let out = cleanup(&px, w, h, &o);
        // The red native cell (majority red, 1 blue) becomes solid red — the fringe is gone.
        for (x, y) in [(0, 0), (3, 3), (1, 2)] {
            let i = (y * w + x) * 4;
            assert_eq!(&out[i..i + 3], &red, "red cell at {x},{y}");
        }
        // A green cell stays green.
        let g = (5 * w + 5) * 4;
        assert_eq!(&out[g..g + 3], &green);
    }

    #[test]
    fn empty_or_short_buffer_is_returned_unchanged() {
        assert!(cleanup(&[], 0, 0, &opts()).is_empty());
        let short = vec![1, 2, 3];
        assert_eq!(cleanup(&short, 4, 4, &opts()), short);
    }

    // Visual tuning dump: reads the real Desktop JPEGs and writes cleanup variants so a human
    // (and the assistant) can eyeball which defaults preserve the collage colours best.
    // Run:  cargo test --release dump_real_cleanup -- --ignored --nocapture
    #[test]
    #[ignore = "reads real Desktop JPEGs; run with --ignored to produce visual output"]
    fn dump_real_cleanup() {
        let inputs = [
            "/home/grymmjack/Desktop/ATARI-2600-PIXEL-ART.jpg",
            "/home/grymmjack/Desktop/MISC-OLDSCHOOL-PIXEL-ART.jpg",
        ];
        let dir = "/tmp/kt_jpeg_clean";
        std::fs::create_dir_all(dir).unwrap();
        for path in inputs {
            if !std::path::Path::new(path).exists() {
                eprintln!("skip (not found): {path}");
                continue;
            }
            let img = image::open(path).unwrap().to_rgba8();
            let (w, h) = (img.width() as usize, img.height() as usize);
            let rgba = img.into_raw();
            let key = detect_background(&rgba, w, h);
            let stem = std::path::Path::new(path)
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .to_string();
            eprintln!("{stem}: {w}x{h}, detected bg = {key:?}");
            let base = CleanOpts {
                key,
                tol: 24,
                transparent: false,
                bg_color: [0, 0, 0],
                snap: true,
                colors: 32,
                despeckle: true,
                min_island: 2,
                merge_islands: true,
                merge_max: 8,
                merge_radius: 4,
                grid: false,
                grid_size: 4,
            };
            // Variant A: no snap (preserves ALL colours; bg-key + despeckle only).
            let mut o = base.clone();
            o.snap = false;
            save(dir, &stem, "nosnap_black", &cleanup(&rgba, w, h, &o), w, h);
            // Variants B–D: snap to a few palette sizes, onto black (directly comparable).
            for n in [32usize, 64, 128] {
                let mut o = base.clone();
                o.colors = n;
                save(dir, &stem, &format!("snap{n}_black"), &cleanup(&rgba, w, h, &o), w, h);
            }
            // Transparent RGBA at the best-guess default (snap 64).
            let mut o = base.clone();
            o.colors = 64;
            o.transparent = true;
            save(dir, &stem, "snap64_transparent", &cleanup(&rgba, w, h, &o), w, h);
            // Pixel-grid snap at a few cell sizes (recover native pixels + kill fringe).
            for s in [3usize, 4, 5] {
                let mut o = base.clone();
                o.colors = 64;
                o.grid = true;
                o.grid_size = s;
                save(dir, &stem, &format!("grid{s}_black"), &cleanup(&rgba, w, h, &o), w, h);
            }
        }
    }

    #[cfg(test)]
    fn save(dir: &str, stem: &str, tag: &str, buf: &[u8], w: usize, h: usize) {
        image::save_buffer(
            format!("{dir}/{stem}__{tag}.png"),
            buf,
            w as u32,
            h as u32,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
}
