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

/// Render a font file as either a **sample** (custom `text`, or the format's
/// default when `text` is empty) or a full **glyph grid** (`grid = true`).
/// Dispatches by extension across TTF/OTF (ab_glyph), bitmap fonts (fon/fnt/psf/
/// .fNN) and TheDraw `.tdf`. Powers the vscode-kaleidotron font viewer's
/// "Display as / Custom text / character grid" controls.
pub fn render_font(bytes: &[u8], ext: &str, grid: bool, text: &str) -> Option<PixImage> {
    use decode::{fon, font, tdf};
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    let is_ttf = matches!(ext.as_str(), "ttf" | "otf" | "ttc" | "otc");
    let is_tdf = ext == "tdf";

    // Default "font name" sample = the ordinary Decoder path (what a thumbnail shows).
    if !grid && text.trim().is_empty() {
        return decode(bytes, &ext).ok();
    }

    if is_tdf {
        let opts = tdf::TdfOpts::default();
        if grid {
            let chars: Vec<char> = (33u8..=126).map(|b| b as char).collect();
            tdf::render_glyph_grid(bytes, 0, &chars, 16, 48, &opts).map(|(img, _)| img)
        } else {
            tdf::render_tdf(bytes, 0, text, &opts)
        }
    } else if is_ttf {
        if grid {
            let chars = font::glyph_chars(bytes);
            font::render_glyph_grid(bytes, &chars, 16, 48, [235, 235, 235]).map(|(img, _)| img)
        } else {
            font::render_text(bytes, text, &font::TextOpts::default())
        }
    } else {
        // Raw bitmap fonts (.fon/.fnt/.psf/.fNN).
        let ink = [235, 235, 235];
        if grid {
            fon::render_glyph_grid(bytes, 0, 0, 256, 16, 22, ink).map(|(img, _, _)| img)
        } else {
            fon::render_text(bytes, 0, text, ink)
        }
    }
}
