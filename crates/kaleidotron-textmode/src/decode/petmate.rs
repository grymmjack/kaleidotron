//! Decode nurpax/petmate `.petmate` workspaces — the JSON PETSCII format from the
//! petmate editor (<https://nurpax.github.io/petmate/>). A workspace is
//! `{ version, screens: [indices], framebufs: [Framebuf] }`; each framebuffer is a
//! `width × height` grid of `{ code, color }` cells (C64 screen code + VIC-II colour
//! index) plus a `backgroundColor` and an `upper`/`lower` `charset`. We render with the
//! same embedded C64 ROM + VIC-II palette as PETSCII (.seq/.pet) — inheriting its
//! pixel-perfect zoom and area-averaged thumbnails — stacking multiple screens
//! vertically so the whole workspace is visible.

use super::c64_font::C64_FONT;
use super::petscii::{CELL, VIC2};
use super::{DecodeError, Decoder};
use crate::image_types::PixImage;

pub struct PetmateDecoder;

impl Decoder for PetmateDecoder {
    fn name(&self) -> &'static str {
        "petmate"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["petmate"]
    }

    fn sniff(&self, header: &[u8]) -> bool {
        // A JSON workspace, e.g. `{"version":2,"screens":[…],"framebufs":[…]}`.
        header.first() == Some(&b'{') && contains(header, b"version")
    }

    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        let v: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|e| DecodeError::Malformed(format!("petmate JSON: {e}")))?;
        let framebufs = v["framebufs"]
            .as_array()
            .ok_or_else(|| DecodeError::Malformed("petmate: no framebufs".into()))?;
        // `screens` indexes into `framebufs`; if absent/empty, render them all in order.
        let order: Vec<usize> = match v["screens"].as_array() {
            Some(s) if !s.is_empty() => s
                .iter()
                .filter_map(|i| i.as_u64().map(|n| n as usize))
                .collect(),
            _ => (0..framebufs.len()).collect(),
        };

        // Render each screen, then stack them vertically (left-aligned on a canvas as
        // wide as the widest screen; black fills any narrower screen's margin).
        let panels: Vec<(usize, usize, Vec<u8>)> = order
            .iter()
            .filter_map(|&i| framebufs.get(i))
            .filter_map(render_framebuf)
            .collect();
        if panels.is_empty() {
            return Err(DecodeError::Malformed(
                "petmate: no renderable screens".into(),
            ));
        }
        let w = panels.iter().map(|p| p.0).max().unwrap_or(0);
        let h: usize = panels.iter().map(|p| p.1).sum();
        let mut indices = vec![0u8; w * h];
        let mut y0 = 0;
        for (pw, ph, px) in &panels {
            for row in 0..*ph {
                let d = (y0 + row) * w;
                let s = row * pw;
                indices[d..d + pw].copy_from_slice(&px[s..s + pw]);
            }
            y0 += ph;
        }
        Ok(PixImage::from_indexed(
            w as u32,
            h as u32,
            indices,
            VIC2.to_vec(),
        ))
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Render one petmate framebuffer to `(width_px, height_px, palette-index buffer)`.
/// `code` is a C64 screen code (bit 7 = reverse); `charset` picks the font page.
fn render_framebuf(fb: &serde_json::Value) -> Option<(usize, usize, Vec<u8>)> {
    let width = fb["width"].as_u64()? as usize;
    let height = fb["height"].as_u64()? as usize;
    if !(1..=1000).contains(&width) || !(1..=1000).contains(&height) {
        return None;
    }
    let bg = (fb["backgroundColor"].as_u64().unwrap_or(0) & 0x0f) as u8;
    let page: u16 = if fb["charset"].as_str() == Some("lower") {
        1
    } else {
        0
    };
    let rows = fb["framebuf"].as_array()?;
    let (w, h) = (width * CELL, height * CELL);
    let mut indices = vec![bg; w * h]; // fill the background colour, then stamp glyphs
    for (cy, row) in rows.iter().take(height).enumerate() {
        let Some(cells) = row.as_array() else {
            continue;
        };
        for (cx, cell) in cells.iter().take(width).enumerate() {
            let code = (cell["code"].as_u64().unwrap_or(32) & 0xff) as u16;
            let fg = (cell["color"].as_u64().unwrap_or(14) & 0x0f) as u8;
            let glyph = &C64_FONT[(page * 256 + code) as usize % C64_FONT.len()];
            for (ry, &bits) in glyph.iter().enumerate() {
                for rx in 0..CELL {
                    if (bits >> (7 - rx)) & 1 == 1 {
                        indices[(cy * CELL + ry) * w + (cx * CELL + rx)] = fg;
                    }
                }
            }
        }
    }
    Some((w, h, indices))
}

