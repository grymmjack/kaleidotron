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

    // 3. Despeckle: drop foreground connected-components smaller than `min_island`.
    if o.despeckle && o.min_island > 1 {
        despeckle(&mut is_bg, w, h, o.min_island);
    }

    // 4. Compose: background → transparent or the fill colour; foreground → opaque.
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
