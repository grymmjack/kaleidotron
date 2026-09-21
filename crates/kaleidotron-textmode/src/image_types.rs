//! Core in-memory image representation.
//!
//! Pixel-art design choice: when a format is palette-based we keep the original
//! `indices` + `palette` in `indexed`, instead of throwing them away after
//! producing RGBA. That's what makes palette-swap, palette-cycling, and accurate
//! re-export possible later. `pixels` is always populated for display, so the
//! rest of the app never has to care how a file was encoded.

pub type Rgba = [u8; 4];

#[derive(Clone)]
pub struct Indexed {
    // Preserved for future palette-swap / cycling / accurate re-export (the point of
    // the project); not read yet, so silence the dead-code lint until it is.
    #[allow(dead_code)]
    pub indices: Vec<u8>, // width * height
    pub palette: Vec<Rgba>, // up to 256 entries
}

#[derive(Clone)]
pub struct PixImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<Rgba>, // width * height, always present (for display)
    pub indexed: Option<Indexed>, // Some(..) when the source was palette-based
}

impl PixImage {
    pub fn from_rgba(width: u32, height: u32, pixels: Vec<Rgba>) -> Self {
        debug_assert_eq!(pixels.len(), (width * height) as usize);
        Self {
            width,
            height,
            pixels,
            indexed: None,
        }
    }

    pub fn from_indexed(width: u32, height: u32, indices: Vec<u8>, palette: Vec<Rgba>) -> Self {
        let pixels = indices
            .iter()
            .map(|&i| palette.get(i as usize).copied().unwrap_or([0, 0, 0, 255]))
            .collect();
        Self {
            width,
            height,
            pixels,
            indexed: Some(Indexed { indices, palette }),
        }
    }

    /// Flat RGBA8 buffer, ready for `egui::ColorImage::from_rgba_unmultiplied`.
    pub fn rgba_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len() * 4);
        for p in &self.pixels {
            out.extend_from_slice(p);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_indexed_expands_palette_and_keeps_original() {
        let palette = vec![[10, 20, 30, 255], [40, 50, 60, 255]];
        let indices = vec![0u8, 1, 1, 0];
        let img = PixImage::from_indexed(2, 2, indices.clone(), palette.clone());
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(
            img.pixels,
            vec![
                [10, 20, 30, 255],
                [40, 50, 60, 255],
                [40, 50, 60, 255],
                [10, 20, 30, 255]
            ]
        );
        let idx = img.indexed.expect("palette preserved");
        assert_eq!(idx.indices, indices);
        assert_eq!(idx.palette, palette);
    }

    #[test]
    fn out_of_range_index_falls_back_to_black() {
        let img = PixImage::from_indexed(1, 1, vec![5], vec![[1, 2, 3, 255]]);
        assert_eq!(img.pixels, vec![[0, 0, 0, 255]]);
    }

    #[test]
    fn from_rgba_has_no_palette_and_flat_bytes() {
        let img = PixImage::from_rgba(1, 1, vec![[9, 8, 7, 255]]);
        assert!(img.indexed.is_none());
        assert_eq!(img.rgba_bytes(), vec![9, 8, 7, 255]);
    }
}
