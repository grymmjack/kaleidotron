use super::{DecodeError, Decoder};
use crate::image_types::PixImage;
use std::io::Cursor;

/// GIMP (`.xcf`) via the `xcf` crate. The crate exposes layers but doesn't flatten,
/// so we composite them back-to-front with a simple alpha-over. It's a best-effort
/// view: layer offsets, visibility, and blend modes aren't applied (the crate is
/// immature), which is correct for the common full-canvas-layer case.
pub struct XcfDecoder;

impl Decoder for XcfDecoder {
    fn name(&self) -> &'static str {
        "xcf"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["xcf"]
    }

    fn sniff(&self, header: &[u8]) -> bool {
        header.starts_with(b"gimp xcf ")
    }

    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        let xcf = xcf::Xcf::load(Cursor::new(bytes))
            .map_err(|e| DecodeError::Malformed(format!("{e:?}")))?;
        let (w, h) = (xcf.width(), xcf.height());
        if w == 0 || h == 0 {
            return Err(DecodeError::Malformed("zero-sized XCF".into()));
        }
        let mut canvas = vec![[0u8, 0, 0, 0]; (w * h) as usize];
        // layers is top→bottom, so reverse to paint bottom→top.
        for layer in xcf.layers.iter().rev() {
            let lw = layer.width.min(w);
            let lh = layer.height.min(h);
            for y in 0..lh {
                for x in 0..lw {
                    if let Some(px) = layer.pixel(x, y) {
                        let s = px.0;
                        if s[3] == 0 {
                            continue;
                        }
                        let d = &mut canvas[(y * w + x) as usize];
                        let sa = f32::from(s[3]) / 255.0;
                        for c in 0..3 {
                            d[c] =
                                (f32::from(s[c]) * sa + f32::from(d[c]) * (1.0 - sa)).round() as u8;
                        }
                        d[3] = (f32::from(s[3]) + f32::from(d[3]) * (1.0 - sa))
                            .round()
                            .min(255.0) as u8;
                    }
                }
            }
        }
        Ok(PixImage::from_rgba(w, h, canvas))
    }
}
