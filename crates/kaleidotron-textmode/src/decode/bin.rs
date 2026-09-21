//! Raw BIN — the simplest scene text-mode format: a headerless stream of
//! (character, attribute) pairs. There's no width in the data, so it comes from
//! the SAUCE record (TInfo1) or the 160-column scene default; iCE colors and the
//! line count likewise come from SAUCE. Rendered with the default VGA font/palette.

use super::{DecodeError, Decoder};
use crate::image_types::PixImage;

pub struct BinDecoder;

const DEFAULT_WIDTH: usize = 160; // the scene default for header-less BIN
const MAX_CELLS: usize = 250_000;

impl Decoder for BinDecoder {
    fn name(&self) -> &'static str {
        "bin"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["bin"]
    }

    fn sniff(&self, _header: &[u8]) -> bool {
        false // headerless — dispatched by the .bin extension only
    }

    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        let sauce = crate::sauce::parse(bytes);
        let ice = sauce.as_ref().map(|s| s.ice).unwrap_or(true);
        let width = sauce
            .as_ref()
            .and_then(|s| s.char_width())
            .unwrap_or(DEFAULT_WIDTH)
            .clamp(1, 1000);
        let data = crate::sauce::strip(bytes);
        let pairs: Vec<(u8, u8)> = data.chunks_exact(2).map(|c| (c[0], c[1])).collect();
        if pairs.is_empty() {
            return Err(DecodeError::Malformed("empty BIN".into()));
        }
        // Height is inferred from the data length, NOT SAUCE's TInfo2: for an
        // uncompressed (char, attr) stream the byte count is authoritative, and ansilove
        // (what 16colo.rs renders with) computes rows = bytes/2/width, ignoring TInfo2 for
        // BIN. A stale/garbage TInfo2 would otherwise pad the canvas with blank rows or
        // clip the art — the same wrong-dimension trap as the width default. This also
        // matches every other binary decoder here (IDF/ADF/Tundra all infer from data).
        let height = pairs.len().div_ceil(width).max(1);
        if width * height > MAX_CELLS {
            return Err(DecodeError::Malformed("BIN too large".into()));
        }
        Ok(super::xbin::render_textmode(
            width,
            height,
            &pairs,
            // Raw VGA attribute bytes → the VGA-ordered palette (index 1=blue, 4=red),
            // NOT the SGR-ordered ansi::PALETTE (which would swap red↔blue).
            &super::ansi::VGA_PALETTE,
            None,
            16,
            ice,
            false,
        ))
    }
}

