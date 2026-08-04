use super::{DecodeError, Decoder};
use crate::image_types::PixImage;
use resvg::{tiny_skia, usvg};

/// SVG via resvg/usvg/tiny-skia — rasterizes at the SVG's intrinsic size (capped
/// so a huge viewBox can't allocate gigabytes). Text uses usvg's default fonts.
pub struct SvgDecoder;

const MAX_DIM: f32 = 2048.0;

/// Rasterize `tree` at `scale` → a `PixImage` (un-premultiplied RGBA).
fn rasterize(tree: &usvg::Tree, scale: f32) -> Result<PixImage, DecodeError> {
    let size = tree.size();
    let w = (size.width() * scale).round().max(1.0) as u32;
    let h = (size.height() * scale).round().max(1.0) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(w, h)
        .ok_or_else(|| DecodeError::Malformed("SVG too large to rasterize".into()))?;
    resvg::render(
        tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let pixels = pixmap
        .pixels()
        .iter()
        .map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    Ok(PixImage::from_rgba(w, h, pixels))
}

/// Rasterize an SVG so its LONGEST side is ~`target_longest` px — the pseudo-vector zoom
/// re-render (zooming in re-rasterizes crisply instead of upscaling a fixed raster). Clamped so a
/// tiny/huge viewBox can't allocate wildly.
pub fn render_svg_at(bytes: &[u8], target_longest: f32) -> Result<PixImage, DecodeError> {
    let tree = usvg::Tree::from_data(bytes, &usvg::Options::default())
        .map_err(|e| DecodeError::Malformed(e.to_string()))?;
    let size = tree.size();
    let longest = size.width().max(size.height()).max(1.0);
    let scale = (target_longest / longest).clamp(0.01, 32.0);
    rasterize(&tree, scale)
}

impl Decoder for SvgDecoder {
    fn name(&self) -> &'static str {
        "svg"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["svg"]
    }

    fn sniff(&self, header: &[u8]) -> bool {
        let head = String::from_utf8_lossy(&header[..header.len().min(64)]);
        let head = head.trim_start();
        head.starts_with("<?xml") || head.starts_with("<svg") || head.contains("<svg")
    }

    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        // Intrinsic size, capped (never upscale on first decode — the zoom re-render does that).
        let tree = usvg::Tree::from_data(bytes, &usvg::Options::default())
            .map_err(|e| DecodeError::Malformed(e.to_string()))?;
        let size = tree.size();
        let scale = (MAX_DIM / size.width().max(size.height()).max(1.0)).clamp(0.01, 1.0);
        rasterize(&tree, scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="20"><rect width="10" height="20" fill="red"/></svg>"#;

    #[test]
    fn render_at_scales_to_target_longest() {
        // longest side 20 → target 200 ⇒ ~10× ⇒ 100×200.
        let img = render_svg_at(SVG, 200.0).unwrap();
        assert_eq!((img.width, img.height), (100, 200));
        // decode() stays at intrinsic (≤ MAX_DIM).
        let d = SvgDecoder.decode(SVG).unwrap();
        assert_eq!((d.width, d.height), (10, 20));
    }
}
