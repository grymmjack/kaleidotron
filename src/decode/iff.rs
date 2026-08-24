//! IFF ILBM/PBM (`.iff`/`.ilbm`/`.lbm`) — the Amiga's interleaved bitmap format **and** PC Deluxe
//! Paint's chunky `PBM ` variant (what a DOS `.LBM` almost always is).
//!
//! An IFF file is chunks inside a `FORM`: `BMHD` (dimensions, plane count, compression), `CMAP`
//! (the palette), `CAMG` (Amiga display flags — HAM / EHB), and `BODY` (the pixels). ILBM stores
//! rows **plane-interleaved**: for each scanline, plane 0's bits for the whole row, then plane 1's,
//! and so on — the exact opposite of a chunky bitmap, and the thing every ILBM decoder has to undo.
//!
//! It is genuinely palette-based, so this joins PCX as a **palette-preserving** decoder: the result
//! is a [`PixImage::from_indexed`] and the swatches / `.GPL` export / recolor pipeline work on it.
//! The two exceptions are the Amiga's colour tricks, which are computed per pixel and so cannot keep
//! an index — **HAM** (hold-and-modify: a pixel adjusts one channel of the previous colour) and
//! **EHB** (extra-half-brite: 64 colours where 32–63 are half-brightness copies of 0–31). HAM
//! produces true colour with no palette; EHB is expanded to a real 64-entry palette and stays
//! indexed.
//!
//! Handles: compression 0 (none) and 1 (ByteRun1 / PackBits), 1–8 bitplanes, optional 1-bit mask
//! plane (skipped), CMAP, CAMG. Verified against the Stone Oakvalley ColorFont previews (1030 files:
//! FORM/ILBM, ByteRun1, 4 planes, 736×512) and synthetic HAM/EHB fixtures.

use super::{DecodeError, Decoder};
use crate::image_types::PixImage;

fn u16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(o)?, *b.get(o + 1)?]))
}
fn u32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes([*b.get(o)?, *b.get(o + 1)?, *b.get(o + 2)?, *b.get(o + 3)?]))
}

/// CAMG display flags we care about.
const CAMG_HAM: u32 = 0x0800;
const CAMG_EHB: u32 = 0x0080;

struct Bmhd {
    w: u16,
    h: u16,
    planes: u8,
    masking: u8, // 0 none, 1 has-mask, 2 transparent-colour, 3 lasso
    compression: u8,
}

/// Decode an ILBM to an indexed (or, for HAM, true-colour) `PixImage`.
pub fn decode(bytes: &[u8]) -> Result<PixImage, DecodeError> {
    let bad = |m: &str| DecodeError::Malformed(m.to_string());
    // ILBM = Amiga planar; PBM (note the trailing space) = PC Deluxe Paint's CHUNKY variant, which
    // is what a DOS `.LBM` almost always is. Both share the same chunk layout (BMHD/CMAP/CAMG/BODY);
    // only the BODY encoding differs (interleaved bitplanes vs one byte per pixel).
    if bytes.len() < 12 || &bytes[0..4] != b"FORM" {
        return Err(bad("not a FORM"));
    }
    let is_pbm = &bytes[8..12] == b"PBM ";
    if &bytes[8..12] != b"ILBM" && !is_pbm {
        return Err(bad("not a FORM ILBM/PBM"));
    }

    let mut bmhd: Option<Bmhd> = None;
    let mut cmap: Vec<[u8; 4]> = Vec::new();
    let mut camg: u32 = 0;
    let mut body: Option<&[u8]> = None;

    // Walk the chunks. Each is a 4-byte id, a big-endian u32 length, that many bytes, then padding
    // to an even boundary — the pad byte trips a naive walker that forgets it.
    let mut o = 12usize;
    while o + 8 <= bytes.len() {
        let id = &bytes[o..o + 4];
        let len = u32(bytes, o + 4).ok_or_else(|| bad("truncated chunk header"))? as usize;
        let start = o + 8;
        let end = start.checked_add(len).ok_or_else(|| bad("chunk length overflow"))?;
        if end > bytes.len() {
            return Err(bad("chunk runs past end of file"));
        }
        let data = &bytes[start..end];
        match id {
            b"BMHD" => {
                if data.len() < 20 {
                    return Err(bad("short BMHD"));
                }
                bmhd = Some(Bmhd {
                    w: u16(data, 0).unwrap(),
                    h: u16(data, 2).unwrap(),
                    planes: data[8],
                    masking: data[9],
                    compression: data[10],
                });
            }
            b"CMAP" => {
                cmap = data.chunks_exact(3).map(|c| [c[0], c[1], c[2], 255]).collect();
            }
            b"CAMG" => {
                camg = u32(data, 0).unwrap_or(0);
            }
            b"BODY" => body = Some(data),
            _ => {}
        }
        // + the pad byte for an odd length.
        o = end + (len & 1);
    }

    let bmhd = bmhd.ok_or_else(|| bad("no BMHD"))?;
    let body = body.ok_or_else(|| bad("no BODY"))?;
    let (w, h) = (bmhd.w as usize, bmhd.h as usize);
    if w == 0 || h == 0 || bmhd.planes == 0 || bmhd.planes > 8 {
        return Err(bad("implausible ILBM dimensions"));
    }

    let mut indices = vec![0u16; w * h];
    if is_pbm {
        // PBM: chunky — one byte per pixel is the palette index directly. Rows are padded to an
        // even byte width (the IFF alignment rule), and ByteRun1 compresses that chunky stream.
        let row_stride = w + (w & 1);
        let unpacked: Vec<u8> = match bmhd.compression {
            0 => body.to_vec(),
            1 => byterun1(body, row_stride * h),
            c => return Err(bad(&format!("unsupported compression {c}"))),
        };
        for y in 0..h {
            for x in 0..w {
                indices[y * w + x] = unpacked.get(y * row_stride + x).copied().unwrap_or(0) as u16;
            }
        }
    } else {
        // ILBM: unpack the BODY into an index-per-pixel buffer. Rows are byte-padded: a plane row is
        // ceil(w/8) bytes, and there is one such row per plane per scanline (plus a mask row if
        // masking == 1), all interleaved.
        let row_bytes = w.div_ceil(8);
        let mask_rows = usize::from(bmhd.masking == 1);
        let stride = (bmhd.planes as usize + mask_rows) * row_bytes;

        let unpacked: Vec<u8> = match bmhd.compression {
            0 => body.to_vec(),
            1 => byterun1(body, stride * h),
            c => return Err(bad(&format!("unsupported compression {c}"))),
        };

        for y in 0..h {
            let row = &unpacked[(y * stride).min(unpacked.len())..];
            for p in 0..bmhd.planes as usize {
                let plane = &row[(p * row_bytes)..];
                for x in 0..w {
                    let byte = plane.get(x >> 3).copied().unwrap_or(0);
                    if byte & (0x80 >> (x & 7)) != 0 {
                        indices[y * w + x] |= 1 << p;
                    }
                }
            }
        }
    }

    // ── Amiga colour modes ──────────────────────────────────────────────────
    if camg & CAMG_HAM != 0 {
        return Ok(decode_ham(&indices, w, h, bmhd.planes, &cmap));
    }

    let mut palette = cmap;
    if camg & CAMG_EHB != 0 {
        // Extra-half-brite: the file has 32 real colours; 32..63 are those at half brightness.
        palette.truncate(32);
        let base = palette.clone();
        for c in &base {
            palette.push([c[0] >> 1, c[1] >> 1, c[2] >> 1, 255]);
        }
    }
    // A missing or short palette (a mask-only bitmap, a corrupt CMAP) gets a grey ramp rather than
    // a refusal — the shape is still worth seeing.
    let need = 1usize << bmhd.planes;
    if palette.len() < need {
        for i in palette.len()..need {
            let v = (i * 255 / need.saturating_sub(1).max(1)) as u8;
            palette.push([v, v, v, 255]);
        }
    }

    let idx8: Vec<u8> = indices.iter().map(|&i| i as u8).collect();
    Ok(PixImage::from_indexed(w as u32, h as u32, idx8, palette))
}

/// ByteRun1 (PackBits): a signed control byte n. 0..=127 → copy the next n+1 bytes literally;
/// -1..=-127 → repeat the next byte 1-n times; -128 is a no-op. Bounded by the expected output size
/// so a malformed stream can't allocate without limit.
fn byterun1(src: &[u8], expected: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(expected);
    let mut i = 0;
    while i < src.len() && out.len() < expected {
        let n = src[i] as i8;
        i += 1;
        if n >= 0 {
            let count = n as usize + 1;
            for _ in 0..count {
                if i >= src.len() {
                    break;
                }
                out.push(src[i]);
                i += 1;
            }
        } else if n != -128 {
            let count = (1 - n as i32) as usize;
            if i >= src.len() {
                break;
            }
            let b = src[i];
            i += 1;
            out.extend(std::iter::repeat_n(b, count));
        }
    }
    out.resize(expected, 0); // pad a short stream so row indexing never panics
    out
}

/// HAM decode: the low `planes-2` bits are an index or a modify value; the top two bits select the
/// mode. 00 = take the CMAP colour at the index; 01/10/11 = hold the previous pixel's colour and
/// replace its blue / red / green channel (respectively) with the value scaled to 8 bits. Produces
/// true colour, so the result is RGBA with no palette.
fn decode_ham(indices: &[u16], w: usize, h: usize, planes: u8, cmap: &[[u8; 4]]) -> PixImage {
    let val_bits = planes.saturating_sub(2);
    let val_mask = (1u16 << val_bits) - 1;
    // Scale an n-bit channel value to 0..=255 by replicating the high bits (so max → 255).
    let scale = |v: u16| -> u8 {
        if val_bits == 0 {
            0
        } else {
            ((v as u32 * 255) / val_mask as u32) as u8
        }
    };
    let grey = [0u8, 0, 0, 255];
    let mut px = vec![0u8; w * h * 4];
    for y in 0..h {
        let mut prev = [0u8, 0, 0]; // each row starts from black, per the HAM spec
        for x in 0..w {
            let code = indices[y * w + x];
            let mode = code >> val_bits;
            let val = code & val_mask;
            let rgb = match mode {
                0 => {
                    let c = cmap.get(val as usize).copied().unwrap_or(grey);
                    [c[0], c[1], c[2]]
                }
                1 => [prev[0], prev[1], scale(val)],
                2 => [scale(val), prev[1], prev[2]],
                _ => [prev[0], scale(val), prev[2]],
            };
            prev = rgb;
            let o = (y * w + x) * 4;
            px[o..o + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    let pixels = px.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
    PixImage::from_rgba(w as u32, h as u32, pixels)
}

/// Registry decoder for ILBM.
pub struct IlbmDecoder;

impl Decoder for IlbmDecoder {
    fn name(&self) -> &'static str {
        "ilbm"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["iff", "ilbm", "lbm"]
    }
    fn sniff(&self, header: &[u8]) -> bool {
        header.len() >= 12
            && &header[0..4] == b"FORM"
            && matches!(&header[8..12], b"ILBM" | b"PBM ")
    }
    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        decode(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assemble one IFF chunk: id, big-endian length, data, pad byte for odd length.
    fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend_from_slice(&(data.len() as u32).to_be_bytes());
        v.extend_from_slice(data);
        if data.len() & 1 == 1 {
            v.push(0);
        }
        v
    }

    fn form(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut inner = b"ILBM".to_vec();
        for c in chunks {
            inner.extend_from_slice(c);
        }
        let mut v = b"FORM".to_vec();
        v.extend_from_slice(&(inner.len() as u32).to_be_bytes());
        v.extend_from_slice(&inner);
        v
    }

    fn bmhd(w: u16, h: u16, planes: u8, comp: u8, masking: u8) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&w.to_be_bytes());
        d.extend_from_slice(&h.to_be_bytes());
        d.extend_from_slice(&0u16.to_be_bytes()); // x
        d.extend_from_slice(&0u16.to_be_bytes()); // y
        d.push(planes);
        d.push(masking);
        d.push(comp);
        d.push(0); // pad
        d.extend_from_slice(&0u16.to_be_bytes()); // transparent colour
        d.push(1); // xAspect
        d.push(1); // yAspect
        d.extend_from_slice(&w.to_be_bytes()); // pageWidth
        d.extend_from_slice(&h.to_be_bytes()); // pageHeight
        chunk(b"BMHD", &d) // a full chunk, like cmap/body — not the bare payload
    }

    /// A 2-plane, 8x1 uncompressed image with a real palette decodes to the expected indices and
    /// keeps its palette — the interleaved plane assembly is the core of the format.
    #[test]
    fn decodes_interleaved_planes_and_keeps_the_palette() {
        // Plane 0 = 1010_0000, plane 1 = 0110_0000. Per-pixel index (bit1 bit0):
        //   x0: p1=0 p0=1 -> 1   x1: p1=1 p0=0 -> 2   x2: p1=1 p0=1 -> 3   x3: 0
        let body = chunk(b"BODY", &[0b1010_0000, 0b0110_0000]);
        let cmap = chunk(b"CMAP", &[0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255]); // 4 colours
        let iff = form(&[bmhd(8, 1, 2, 0, 0), cmap, body]);

        let img = decode(&iff).expect("decodes");
        assert_eq!((img.width, img.height), (8, 1));
        let idx = img.indexed.as_ref().expect("palette-preserving");
        assert_eq!(&idx.indices[..4], &[1, 2, 3, 0]);
        assert_eq!(idx.palette[1], [255, 0, 0, 255], "CMAP colour 1 is red");
    }

    /// A PC Deluxe Paint `.LBM` is `FORM…PBM ` (chunky: one byte per pixel), not planar ILBM.
    /// Odd width exercises the even-byte row padding. Decodes to the byte indices + keeps the CMAP.
    #[test]
    fn decodes_pbm_chunky_dos_deluxe_paint() {
        let mut cmap_data = Vec::new();
        for i in 0..8u8 {
            cmap_data.extend_from_slice(&[i * 10, 0, 0]);
        }
        // w=3 (odd → row padded to 4 bytes); the 4th byte per row is padding.
        let body = chunk(b"BODY", &[1, 2, 3, 0]);
        let mut inner = b"PBM ".to_vec();
        inner.extend_from_slice(&bmhd(3, 1, 8, 0, 0));
        inner.extend_from_slice(&chunk(b"CMAP", &cmap_data));
        inner.extend_from_slice(&body);
        let mut iff = b"FORM".to_vec();
        iff.extend_from_slice(&(inner.len() as u32).to_be_bytes());
        iff.extend_from_slice(&inner);

        assert!(IlbmDecoder.sniff(&iff), "PBM must sniff");
        let img = decode(&iff).expect("PBM decodes");
        assert_eq!((img.width, img.height), (3, 1));
        let idx = img.indexed.as_ref().expect("palette-preserving");
        assert_eq!(idx.indices, vec![1, 2, 3], "chunky byte-per-pixel indices");
        assert_eq!(idx.palette[3], [30, 0, 0, 255], "CMAP colour 3");
    }

    /// ByteRun1: a literal run and a replicate run reconstruct the same bytes the uncompressed
    /// path would give.
    #[test]
    fn byterun1_unpacks_literals_and_replicates() {
        // control 1 (=copy 2) 0xAA 0x55, control -3 (=repeat 4) 0xFF
        let packed = [1u8, 0xAA, 0x55, (-3i8) as u8, 0xFF];
        assert_eq!(byterun1(&packed, 6), vec![0xAA, 0x55, 0xFF, 0xFF, 0xFF, 0xFF]);

        // The same, wrapped as a compressed 8x1 mono image.
        let iff = form(&[bmhd(8, 1, 1, 1, 0), chunk(b"BODY", &[0u8, 0b1111_0000])]);
        let img = decode(&iff).expect("decodes compressed");
        assert_eq!(img.indexed.as_ref().unwrap().indices, vec![1, 1, 1, 1, 0, 0, 0, 0]);
    }

    /// EHB expands a 32-colour CMAP to 64, the upper half at half brightness, and stays indexed.
    #[test]
    fn ehb_expands_the_palette_to_half_brights() {
        let mut cmap_data = Vec::new();
        for i in 0..32u8 {
            cmap_data.extend_from_slice(&[i.wrapping_mul(8), 0, 0]);
        }
        let iff = form(&[
            bmhd(8, 1, 6, 0, 0),
            chunk(b"CMAP", &cmap_data),
            chunk(b"CAMG", &CAMG_EHB.to_be_bytes()),
            chunk(b"BODY", &[0u8; 6]), // 6 planes, all zero
        ]);
        let img = decode(&iff).expect("decodes EHB");
        let pal = &img.indexed.as_ref().expect("EHB stays indexed").palette;
        assert_eq!(pal.len(), 64, "32 real + 32 half-bright");
        assert_eq!(pal[33], [pal[1][0] >> 1, 0, 0, 255], "colour 33 is colour 1 halved");
    }

    /// HAM produces true colour (no palette), holding the previous pixel and modifying one channel.
    #[test]
    fn ham_modifies_channels_of_the_running_colour() {
        // 6-plane HAM: 4 value bits, 2 mode bits. Two pixels:
        //   pixel 0: mode 00, index 1 -> CMAP[1] = (255,0,0)
        //   pixel 1: mode 11 (green), value 15 -> hold red, set green to 255 -> (255,255,0)
        let cmap = {
            let mut d = vec![0, 0, 0]; // colour 0 black
            d.extend_from_slice(&[255, 0, 0]); // colour 1 red
            d
        };
        // Build the two 6-bit codes and lay them into 6 interleaved plane bytes (8px row, first
        // two pixels used).
        let codes = [0b00_0001u16, 0b11_1111u16];
        let mut planebytes = [0u8; 6];
        for (x, &code) in codes.iter().enumerate() {
            for p in 0..6 {
                if code & (1 << p) != 0 {
                    planebytes[p] |= 0x80 >> x;
                }
            }
        }
        let iff = form(&[
            bmhd(8, 1, 6, 0, 0),
            chunk(b"CMAP", &cmap),
            chunk(b"CAMG", &CAMG_HAM.to_be_bytes()),
            chunk(b"BODY", &planebytes),
        ]);
        let img = decode(&iff).expect("decodes HAM");
        assert!(img.indexed.is_none(), "HAM is true colour, not indexed");
        assert_eq!(img.pixels[0], [255, 0, 0, 255], "pixel 0 is CMAP red");
        assert_eq!(img.pixels[1], [255, 255, 0, 255], "pixel 1 holds red, sets green");
    }

    #[test]
    fn rejects_non_ilbm() {
        assert!(decode(b"FORM\0\0\0\x04ANBM").is_err());
        assert!(decode(b"not an iff").is_err());
    }

    /// Every real ColorFont preview decodes. Ignored (needs the archive), but it is the check that
    /// matters — the previews are the actual target, and a synthetic fixture only proves the
    /// decoder agrees with itself.
    #[test]
    #[ignore]
    fn decodes_the_real_previews() {
        let Ok(root) = std::env::var("AMIGA_IFF_DIR") else {
            println!("set AMIGA_IFF_DIR=<a dir of .iff previews> to run this");
            return;
        };
        let (mut ok, mut failed) = (0, 0);
        fn walk(dir: &std::path::Path, ok: &mut i32, failed: &mut i32) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, ok, failed);
                } else if p.extension().and_then(|x| x.to_str()) == Some("iff") {
                    match decode(&std::fs::read(&p).unwrap_or_default()) {
                        Ok(img) => {
                            *ok += 1;
                            assert!(img.width > 0 && img.height > 0, "{p:?} decoded empty");
                        }
                        Err(err) => {
                            *failed += 1;
                            println!("FAILED {p:?}: {err:?}");
                        }
                    }
                }
            }
        }
        walk(std::path::Path::new(&root), &mut ok, &mut failed);
        println!("decoded {ok} previews, {failed} failed");
        assert!(ok > 100 && failed == 0, "ok={ok} failed={failed}");
    }
}
