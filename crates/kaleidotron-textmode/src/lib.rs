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
