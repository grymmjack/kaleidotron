use super::{DecodeError, Decoder};
use crate::image_types::PixImage;

/// Hand-written PCX decoder. PCX is a great first "exotic" format: simple RLE,
/// genuinely palette-based, and everywhere in DOS-era pixel art. This is the
/// template to copy for IFF/ILBM, LBM, or anything the `image` crate lacks.
///
/// Handles the two common variants:
///   * 8bpp / 1 plane  -> indexed, 256-color VGA palette at end of file
///   * 8bpp / 3 planes -> truecolor RGB, stored plane-by-plane per scanline
pub struct PcxDecoder;

impl Decoder for PcxDecoder {
    fn name(&self) -> &'static str {
        "pcx"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["pcx"]
    }

    fn sniff(&self, header: &[u8]) -> bool {
        header.first() == Some(&0x0A)
    }

    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        if bytes.len() < 128 || bytes[0] != 0x0A {
            return Err(DecodeError::Malformed("not a PCX file".into()));
        }
        let rd_u16 = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]);

        let encoding = bytes[2];
        let bits_per_pixel = bytes[3];
        let xmin = rd_u16(4) as i32;
        let ymin = rd_u16(6) as i32;
        let xmax = rd_u16(8) as i32;
        let ymax = rd_u16(10) as i32;
        let n_planes = bytes[65] as usize;
        let bytes_per_line = rd_u16(66) as usize;

        if encoding != 1 {
            return Err(DecodeError::Malformed(
                "only RLE-encoded PCX supported".into(),
            ));
        }
        let width = (xmax - xmin + 1).max(0) as usize;
        let height = (ymax - ymin + 1).max(0) as usize;
        if width == 0 || height == 0 || bytes_per_line == 0 {
            return Err(DecodeError::Malformed("zero-sized image".into()));
        }
        let total_per_line = n_planes * bytes_per_line;

        // RLE-decode exactly height * total_per_line bytes from the body.
        let body = &bytes[128..];
        let mut scan = vec![0u8; height * total_per_line];
        let mut si = 0usize; // source index into body
        let mut di = 0usize; // dest index into scan
        while di < scan.len() {
            let b = *body
                .get(si)
                .ok_or_else(|| DecodeError::Malformed("truncated RLE stream".into()))?;
            si += 1;
            if b & 0xC0 == 0xC0 {
                let count = (b & 0x3F) as usize;
                let val = *body
                    .get(si)
                    .ok_or_else(|| DecodeError::Malformed("truncated RLE run".into()))?;
                si += 1;
                for _ in 0..count {
                    if di >= scan.len() {
                        break;
                    }
                    scan[di] = val;
                    di += 1;
                }
            } else {
                scan[di] = b;
                di += 1;
            }
        }

        // 8bpp, single plane => indexed with a 256-color VGA palette at EOF.
        if bits_per_pixel == 8 && n_planes == 1 {
            let mut palette = vec![[0u8, 0, 0, 255]; 256];
            if bytes.len() >= 769 && bytes[bytes.len() - 769] == 0x0C {
                let pal = &bytes[bytes.len() - 768..];
                for i in 0..256 {
                    palette[i] = [pal[i * 3], pal[i * 3 + 1], pal[i * 3 + 2], 255];
                }
            }
            let mut indices = vec![0u8; width * height];
            for y in 0..height {
                let row = &scan[y * total_per_line..y * total_per_line + bytes_per_line];
                for x in 0..width {
                    indices[y * width + x] = row[x];
                }
            }
            return Ok(PixImage::from_indexed(
                width as u32,
                height as u32,
                indices,
                palette,
            ));
        }

        // 8bpp, three planes => truecolor RGB, one plane after another per line.
        if bits_per_pixel == 8 && n_planes == 3 {
            let mut pixels = vec![[0u8, 0, 0, 255]; width * height];
            for y in 0..height {
                let line = &scan[y * total_per_line..(y + 1) * total_per_line];
                for x in 0..width {
                    let r = line[x];
                    let g = line[bytes_per_line + x];
                    let b = line[2 * bytes_per_line + x];
                    pixels[y * width + x] = [r, g, b, 255];
                }
            }
            return Ok(PixImage::from_rgba(width as u32, height as u32, pixels));
        }

        Err(DecodeError::Malformed(format!(
            "unsupported PCX variant: {bits_per_pixel}bpp x {n_planes} planes"
        )))
    }
}

