//! `kaleidotron-textmode` — the text-mode / scene-art decoders from kaleidotron,
//! extracted as a dependency-light, egui-free library so they can be reused from
//! a WebAssembly build (the `vscode-kaleidotron` native viewer).
//!
//! Everything here decodes a byte buffer to a finished [`PixImage`] (RGBA, with
//! the original indexed palette preserved when the source was palette-based) —
//! the exact same authentic rendering kaleidotron ships (IBM VGA ROM font, the
//! ANSI-SGR-vs-VGA palette order, iCE colours, 24-bit ANSI, XBIN charsets, the
//! C64 font + VIC-II palette for PETSCII).

pub mod decode;
pub mod image_types;
pub mod sauce;
pub mod tracker;

pub use decode::{decode, known_extensions, set_font_9px, DecodeError, Decoder};
pub use image_types::{Indexed, PixImage, Rgba};

/// Render just the sample: custom `text`, or the format's default (font name)
/// when `text` is empty. `None` if nothing is drawable.
fn font_sample(bytes: &[u8], ext: &str, text: &str) -> Option<PixImage> {
    use decode::{fon, font, tdf};
    if text.trim().is_empty() {
        // Default "font name" sample = the ordinary Decoder path (what a thumbnail shows).
        return decode(bytes, ext).ok();
    }
    match ext {
        "tdf" => tdf::render_tdf(bytes, 0, text, &tdf::TdfOpts::default()),
        "ttf" | "otf" | "ttc" | "otc" => {
            font::render_text(bytes, text, &font::TextOpts::default())
        }
        _ => fon::render_text(bytes, 0, text, [235, 235, 235]),
    }
}

/// Render the full glyph grid. `None` if the font can't be parsed.
fn font_grid(bytes: &[u8], ext: &str) -> Option<PixImage> {
    use decode::{fon, font, tdf};
    match ext {
        "tdf" => {
            let chars: Vec<char> = (33u8..=126).map(|b| b as char).collect();
            tdf::render_glyph_grid(bytes, 0, &chars, 16, 48, &tdf::TdfOpts::default())
                .map(|(img, _)| img)
        }
        "ttf" | "otf" | "ttc" | "otc" => {
            let chars = font::glyph_chars(bytes);
            font::render_glyph_grid(bytes, &chars, 16, 48, [235, 235, 235]).map(|(img, _)| img)
        }
        _ => fon::render_glyph_grid(bytes, 0, 0, 256, 16, 22, [235, 235, 235])
            .map(|(img, _, _)| img),
    }
}

/// Stack two images vertically (each centred), with `gap` transparent px between.
fn stack_vertical(top: &PixImage, bottom: &PixImage, gap: u32) -> PixImage {
    let w = top.width.max(bottom.width);
    let h = top.height + gap + bottom.height;
    let mut px = vec![[0u8, 0, 0, 0]; (w * h) as usize];
    let blit = |px: &mut [Rgba], img: &PixImage, y0: u32| {
        let x0 = (w - img.width) / 2;
        for y in 0..img.height {
            for x in 0..img.width {
                let s = img.pixels[(y * img.width + x) as usize];
                if s[3] == 0 {
                    continue;
                }
                px[((y0 + y) * w + x0 + x) as usize] = s;
            }
        }
    };
    blit(&mut px, top, 0);
    blit(&mut px, bottom, top.height + gap);
    PixImage::from_rgba(w, h, px)
}

/// Render a font for the viewer. `mode`: 0 = sample only, 1 = grid only,
/// 2 = both (sample on top, grid below). `text` is the sample string (empty =
/// the font's name). Dispatches across TTF/OTF, bitmap fonts and TheDraw `.tdf`.
pub fn render_font(bytes: &[u8], ext: &str, mode: u32, text: &str) -> Option<PixImage> {
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    match mode {
        1 => font_grid(bytes, &ext),
        2 => match (font_sample(bytes, &ext, text), font_grid(bytes, &ext)) {
            (Some(s), Some(g)) => Some(stack_vertical(&s, &g, 20)),
            (s, None) => s,
            (None, g) => g,
        },
        _ => font_sample(bytes, &ext, text),
    }
}
