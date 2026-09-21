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

