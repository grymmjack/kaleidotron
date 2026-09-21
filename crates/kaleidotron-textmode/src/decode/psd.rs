use super::{DecodeError, Decoder};
use crate::image_types::PixImage;

/// Photoshop (`.psd`) via the `psd` crate — decodes the flattened composite
/// (`Psd::rgba`). Layers aren't preserved yet; this is the merged image.
pub struct PsdDecoder;

impl Decoder for PsdDecoder {
    fn name(&self) -> &'static str {
        "psd"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["psd"]
    }

    fn sniff(&self, header: &[u8]) -> bool {
        header.starts_with(b"8BPS")
    }

    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        let psd = psd::Psd::from_bytes(bytes).map_err(|e| DecodeError::Malformed(e.to_string()))?;
        let (w, h) = (psd.width(), psd.height());
        let pixels = psd
            .rgba()
            .chunks_exact(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect();
        Ok(PixImage::from_rgba(w, h, pixels))
    }
}
