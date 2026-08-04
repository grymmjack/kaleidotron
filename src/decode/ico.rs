//! Multi-image `.ico` / `.cur` support — a single icon file holds several images (different sizes /
//! colour depths). The `image` crate decodes an ICO but only hands back the *best* one, so for the
//! viewer we parse the ICONDIR ourselves and decode each embedded image on demand by wrapping it in
//! a synthetic 1-entry ICO and handing THAT back to the `image` crate (reusing its per-entry PNG +
//! BMP/DIB handling). The grid tile still uses the `image` crate's default single-image path.

use crate::image_types::PixImage;

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Is this a `.ico`/`.cur` ICONDIR (reserved=0, type 1=icon / 2=cursor, count ≥ 1)?
fn is_icon_dir(b: &[u8]) -> bool {
    b.len() >= 6 && u16le(b, 0) == 0 && matches!(u16le(b, 2), 1 | 2) && u16le(b, 4) >= 1
}

/// One embedded image's dimensions + bit depth (for the viewer's picker labels).
#[derive(Clone, Copy)]
pub struct IcoEntry {
    pub w: u32,
    pub h: u32,
    pub bpp: u16,
}

/// The images embedded in an `.ico`/`.cur` (empty if not an icon file / malformed).
pub fn entries(b: &[u8]) -> Vec<IcoEntry> {
    if !is_icon_dir(b) {
        return Vec::new();
    }
    let count = u16le(b, 4) as usize;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let e = 6 + i * 16;
        if e + 16 > b.len() {
            break;
        }
        // A 0 in the width/height byte means 256 (the format's way to encode the max).
        let w = if b[e] == 0 { 256 } else { b[e] as u32 };
        let h = if b[e + 1] == 0 { 256 } else { b[e + 1] as u32 };
        out.push(IcoEntry { w, h, bpp: u16le(b, e + 6) });
    }
    out
}

/// Decode embedded image `idx` → RGBA. Builds a synthetic 1-entry ICO around that image's bytes and
/// decodes it with the `image` crate (so PNG-compressed + BMP/DIB entries both work).
pub fn render_entry(b: &[u8], idx: usize) -> Option<PixImage> {
    if !is_icon_dir(b) {
        return None;
    }
    let n = u16le(b, 4) as usize;
    if idx >= n {
        return None;
    }
    let e = 6 + idx * 16;
    if e + 16 > b.len() {
        return None;
    }
    let bytes_in_res = u32le(b, e + 8) as usize;
    let img_off = u32le(b, e + 12) as usize;
    if bytes_in_res == 0 || img_off.saturating_add(bytes_in_res) > b.len() {
        return None;
    }
    let img_data = &b[img_off..img_off + bytes_in_res];

    // ICONDIR (reserved=0, type=1 icon, count=1) + one ICONDIRENTRY (this entry's first 12 bytes,
    // then a fresh offset of 22) + the image bytes.
    let mut out = Vec::with_capacity(22 + img_data.len());
    out.extend_from_slice(&[0, 0, 1, 0, 1, 0]); // force type=1 so `image` treats it as an icon
    out.extend_from_slice(&b[e..e + 12]); // width..bytesInRes (unchanged)
    out.extend_from_slice(&22u32.to_le_bytes()); // new image offset
    out.extend_from_slice(img_data);

    let img = image::load_from_memory_with_format(&out, image::ImageFormat::Ico).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let px = rgba.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
    Some(PixImage::from_rgba(w, h, px))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-build a 2-image ICO (a 1×1 and a 2×2, both BMP/DIB) and check we enumerate + decode both.
    fn dib(w: u8, h: u8) -> Vec<u8> {
        // BITMAPINFOHEADER (40) with height*2 (ICO stores XOR+AND), 32bpp, then w*h BGRA + AND mask.
        let mut v = Vec::new();
        v.extend_from_slice(&40u32.to_le_bytes()); // header size
        v.extend_from_slice(&(w as i32).to_le_bytes()); // width
        v.extend_from_slice(&((h as i32) * 2).to_le_bytes()); // height (xor+and)
        v.extend_from_slice(&1u16.to_le_bytes()); // planes
        v.extend_from_slice(&32u16.to_le_bytes()); // bpp
        v.extend_from_slice(&[0u8; 24]); // compression..colors (all 0)
        for _ in 0..(w as usize * h as usize) {
            v.extend_from_slice(&[0x40, 0x80, 0xC0, 0xFF]); // BGRA
        }
        let and_row = ((w as usize + 31) / 32) * 4; // padded to 32-bit
        v.extend(std::iter::repeat_n(0u8, and_row * h as usize));
        v
    }

    #[test]
    fn enumerates_and_decodes_each_entry() {
        let imgs = [dib(1, 1), dib(2, 2)];
        let mut ico = Vec::new();
        ico.extend_from_slice(&[0, 0, 1, 0, 2, 0]); // ICONDIR: type 1, count 2
        let mut off = 6 + 16 * 2;
        for (i, img) in imgs.iter().enumerate() {
            let (w, h) = if i == 0 { (1u8, 1u8) } else { (2u8, 2u8) };
            ico.extend_from_slice(&[w, h, 0, 0]); // w,h,colors,reserved
            ico.extend_from_slice(&1u16.to_le_bytes()); // planes
            ico.extend_from_slice(&32u16.to_le_bytes()); // bpp
            ico.extend_from_slice(&(img.len() as u32).to_le_bytes());
            ico.extend_from_slice(&(off as u32).to_le_bytes());
            off += img.len();
        }
        for img in &imgs {
            ico.extend_from_slice(img);
        }
        let es = entries(&ico);
        assert_eq!(es.len(), 2);
        assert_eq!((es[0].w, es[0].h), (1, 1));
        assert_eq!((es[1].w, es[1].h), (2, 2));
        let d0 = render_entry(&ico, 0).expect("entry 0 decodes");
        assert_eq!((d0.width, d0.height), (1, 1));
        let d1 = render_entry(&ico, 1).expect("entry 1 decodes");
        assert_eq!((d1.width, d1.height), (2, 2));
    }
}

#[cfg(test)]
mod real {
    use super::*;
    #[test]
    #[ignore]
    fn dump_vlc_ico() {
        let Ok(bytes) = std::fs::read("/usr/share/vlc/vlc.ico") else { return };
        let es = entries(&bytes);
        eprintln!("vlc.ico: {} embedded images", es.len());
        for (i, e) in es.iter().enumerate() {
            let ok = render_entry(&bytes, i).is_some();
            eprintln!("  [{i}] {}x{} {}bpp → decode {}", e.w, e.h, e.bpp, if ok {"ok"} else {"FAIL"});
        }
        // decode the biggest → PNG
        if let Some((i,_)) = es.iter().enumerate().max_by_key(|(_,e)| e.w*e.h) {
            if let Some(img) = render_entry(&bytes, i) {
                image::save_buffer("/tmp/ico_biggest.png", &img.rgba_bytes(), img.width, img.height, image::ColorType::Rgba8).unwrap();
                eprintln!("wrote /tmp/ico_biggest.png {}x{}", img.width, img.height);
            }
        }
    }
}
