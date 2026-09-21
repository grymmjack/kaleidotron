//! Text-mode / scene-art decoders, extracted from the kaleidotron binary so they
//! can be shared with a WebAssembly build (the `vscode-kaleidotron` viewer) while
//! staying free of egui / the app's `thumb` module.
//!
//! The module layout deliberately mirrors kaleidotron's own `src/decode/` so the
//! decoder source files are byte-for-byte the same (only their `#[cfg(test)]`
//! blocks — which reach into the app's `thumb` module — are dropped here).

pub mod ansi;
pub mod adf;
pub mod aseprite;
pub mod bin;
pub mod builtin;
pub mod fon;
pub mod font;
pub mod idf;
pub mod iff;
pub mod pcx;
pub mod petmate;
pub mod petscii;
pub mod psd;
pub mod rip;
pub mod tdf;
pub mod tundra;
pub mod xbin;
pub mod xcf;

pub(crate) mod c64_font;
pub(crate) mod cp437_font;
pub(crate) mod cp437_font_8x8;
pub(crate) mod rip_chr;

use crate::image_types::PixImage;

/// Toggle the 9-dot VGA cell width for ANSI/CP437 rendering (a process-wide
/// preference read at decode time). Re-decode affected images to apply it.
pub use ansi::set_font_9px;

#[derive(Debug)]
pub enum DecodeError {
    Unsupported,
    Malformed(String),
    Io(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Unsupported => write!(f, "unsupported format"),
            DecodeError::Malformed(m) => write!(f, "malformed image: {m}"),
            DecodeError::Io(m) => write!(f, "io error: {m}"),
        }
    }
}

impl std::error::Error for DecodeError {}

pub trait Decoder: Send + Sync {
    /// Human-readable decoder name.
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
    fn extensions(&self) -> &'static [&'static str];
    /// Cheap check against the first bytes of the file.
    fn sniff(&self, header: &[u8]) -> bool;
    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError>;
}

/// The registered text-mode decoders, in dispatch order. The binary formats
/// (XBIN/Tundra/IDF/ADF) sniff their magic bytes; ANSI/BIN/PETSCII have no magic
/// and are reached by extension (with ANSI as the catch-all for text files).
fn decoders() -> Vec<Box<dyn Decoder>> {
    vec![
        Box::new(rip::RipDecoder),
        Box::new(xbin::XBinDecoder),
        Box::new(tundra::TundraDecoder),
        Box::new(idf::IdfDecoder),
        Box::new(adf::AdfDecoder),
        Box::new(petscii::PetsciiDecoder),
        Box::new(petmate::PetmateDecoder),
        Box::new(bin::BinDecoder),
        // Raster: specific magic-byte decoders first, then the broad image crate.
        // PCX before ImageCrate (its 0x0A magic is ambiguous — same as the app).
        Box::new(aseprite::AsepriteDecoder),
        Box::new(psd::PsdDecoder),
        Box::new(xcf::XcfDecoder),
        Box::new(iff::IlbmDecoder),
        Box::new(pcx::PcxDecoder),
        // Fonts → a rendered preview / glyph grid.
        Box::new(tdf::TdfDecoder),
        Box::new(font::FontDecoder),
        Box::new(fon::FonDecoder),
        Box::new(builtin::ImageCrateDecoder),
        Box::new(ansi::AnsiDecoder), // catch-all for text
    ]
}

/// Decode `bytes` to a finished RGBA image. `ext` is the lowercased file
/// extension without the dot (e.g. `"ans"`, `"xb"`); it is used only when no
/// decoder recognises the magic bytes. Unknown text extensions (`nfo`, `txt`,
/// `msg`, `diz`, …) fall through to the ANSI/CP437 decoder.
pub fn decode(bytes: &[u8], ext: &str) -> Result<PixImage, DecodeError> {
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    let regs = decoders();
    let header = &bytes[..bytes.len().min(64)];
    // 1) magic-byte sniff (catches a mislabeled file).
    for d in &regs {
        if d.sniff(header) {
            return d.decode(bytes);
        }
    }
    // 2) by extension.
    for d in &regs {
        if d.extensions().iter().any(|e| *e == ext) {
            return d.decode(bytes);
        }
    }
    // 3) catch-all: treat anything else as ANSI/CP437 text.
    ansi::AnsiDecoder.decode(bytes)
}

/// The set of extensions the text-mode decoders claim by magic/extension. Text
/// extensions handled by the ANSI catch-all (`nfo`, `txt`, `msg`, …) are added
/// by the caller; this is the authoritative per-decoder list.
pub fn known_extensions() -> Vec<&'static str> {
    decoders()
        .iter()
        .flat_map(|d| d.extensions().iter().copied())
        .collect()
}
