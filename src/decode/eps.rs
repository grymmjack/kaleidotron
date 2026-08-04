//! EPS / PostScript preview via **Ghostscript** (`gs`) — shelled out, the same ethos as
//! poppler/ffmpeg/blender. EPS is PostScript, not PDF, so pdfium/poppler can't read it; `gs`
//! rasterizes it to a PNG (cropped to the bounding box). Absent `gs` ⇒ a labeled placeholder,
//! never a crash. Gated by the PDF plugin (a document format needing an external renderer).

use super::{DecodeError, Decoder};
use crate::image_types::PixImage;
use std::process::Command;

/// Extensions handled here. `.ps` is full PostScript (may be multi-page — we render page 1).
pub const EPS_EXTS: &[&str] = &["eps", "epsf", "epsi", "ps"];

/// Rasterize an EPS/PS `bytes` to RGBA via `gs` at `dpi`. Writes the input to a temp file (gs
/// wants a path), renders page 1 to a temp PNG (EPSCrop → the BoundingBox), reads it back.
pub fn render_eps(bytes: &[u8], dpi: u32) -> Option<PixImage> {
    let dir = std::env::temp_dir();
    let stamp = bytes.len(); // cheap unique-ish tag (no wall clock needed)
    let src = dir.join(format!("pv_eps_{stamp}.eps"));
    let out = dir.join(format!("pv_eps_{stamp}.png"));
    std::fs::write(&src, bytes).ok()?;
    let ok = Command::new("gs")
        .args([
            "-q",
            "-dSAFER",
            "-dBATCH",
            "-dNOPAUSE",
            "-dEPSCrop",
            "-sDEVICE=png16m",
            "-dGraphicsAlphaBits=4",
            "-dTextAlphaBits=4",
            "-dFirstPage=1",
            "-dLastPage=1",
            &format!("-r{}", dpi.clamp(24, 600)),
        ])
        .arg(format!("-sOutputFile={}", out.display()))
        .arg(&src)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let img = if ok {
        image::open(&out).ok().map(|i| {
            let rgba = i.to_rgba8();
            let (w, h) = rgba.dimensions();
            let px = rgba.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
            PixImage::from_rgba(w, h, px)
        })
    } else {
        None
    };
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
    img
}

/// Is `gs` on PATH? (Gates whether EPS renders vs shows a placeholder.) Used by the tests + a
/// future "install ghostscript" hint.
#[allow(dead_code)]
pub fn gs_available() -> bool {
    Command::new("gs")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub struct EpsDecoder;

impl Decoder for EpsDecoder {
    fn name(&self) -> &'static str {
        "eps"
    }
    fn extensions(&self) -> &'static [&'static str] {
        EPS_EXTS
    }
    fn sniff(&self, header: &[u8]) -> bool {
        // EPS: "%!PS-Adobe" (optionally after a DOS/EPSF binary header 0xC5D0D3C6).
        header.starts_with(b"%!PS")
            || header
                .get(0..4)
                .is_some_and(|b| b == [0xC5, 0xD0, 0xD3, 0xC6])
    }
    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        render_eps(bytes, 150).ok_or(DecodeError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: &[u8] = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 60\n\
        newpath 10 10 moveto 90 50 lineto 4 setlinewidth stroke\n\
        0 0 1 setrgbcolor 20 20 50 30 rectfill\nshowpage\n";

    #[test]
    fn sniffs_eps() {
        assert!(EpsDecoder.sniff(EPS));
        assert!(!EpsDecoder.sniff(b"\x89PNG\r\n"));
    }

    #[test]
    fn renders_when_gs_present() {
        if !gs_available() {
            return; // no ghostscript in CI — skip
        }
        let img = render_eps(EPS, 72).expect("gs should rasterize the EPS");
        assert!(img.width > 10 && img.height > 6);
        assert!(img.rgba_bytes().chunks(4).any(|p| p[2] > 200 && p[0] < 80)); // the blue rect
    }
}
