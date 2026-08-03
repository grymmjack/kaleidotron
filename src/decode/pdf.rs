//! PDF support: the tile is the REAL first page, rendered by poppler's `pdftoppm`
//! (`render_first_page`: PDF on stdin → PNG on stdout, no bundled lib) — falling back to a
//! labeled placeholder "page" tile when poppler isn't installed. Metadata (page count,
//! first-page size, /Info title + author) comes from pure-Rust `lopdf` and drives the
//! Details pane. Enter / "Open in default app" launches the associated PDF editor.

use super::cp437_font::CP437_8X16;
use super::{DecodeError, Decoder};
use crate::image_types::PixImage;

/// Parsed PDF metadata for the Details pane (and to label the placeholder tile).
#[derive(Clone, Default)]
pub struct PdfMeta {
    pub pages: usize,
    pub width_pt: f32,
    pub height_pt: f32,
    pub title: String,
    pub author: String,
}

/// Best-effort metadata parse — never panics; returns `None` only if lopdf can't load it.
pub fn pdf_meta(bytes: &[u8]) -> Option<PdfMeta> {
    let doc = lopdf::Document::load_mem(bytes).ok()?;
    let pages = doc.get_pages();
    let count = pages.len();
    // First page MediaBox → page size in points (default US Letter if absent/odd).
    let (mut w, mut h) = (612.0_f32, 792.0_f32);
    if let Some((_, &pid)) = pages.iter().next() {
        if let Some(mb) = media_box(&doc, pid) {
            w = (mb[2] - mb[0]).abs();
            h = (mb[3] - mb[1]).abs();
            if w < 1.0 || h < 1.0 {
                w = 612.0;
                h = 792.0;
            }
        }
    }
    let (title, author) = info_strings(&doc);
    Some(PdfMeta {
        pages: count,
        width_pt: w,
        height_pt: h,
        title,
        author,
    })
}

/// The page's MediaBox `[x0 y0 x1 y1]`, following `/Parent` inheritance.
fn media_box(doc: &lopdf::Document, mut id: lopdf::ObjectId) -> Option<[f32; 4]> {
    for _ in 0..32 {
        let dict = doc.get_object(id).ok()?.as_dict().ok()?;
        if let Ok(arr) = dict.get(b"MediaBox").and_then(|o| o.as_array()) {
            let mut v = [0.0f32; 4];
            for (i, o) in arr.iter().take(4).enumerate() {
                v[i] = o
                    .as_float()
                    .or_else(|_| o.as_i64().map(|n| n as f32))
                    .ok()?;
            }
            return Some(v);
        }
        // Walk up to the parent page-tree node.
        id = dict.get(b"Parent").and_then(|o| o.as_reference()).ok()?;
    }
    None
}

/// /Info Title + Author, decoded best-effort (UTF-16BE BOM or Latin-1/PDFDoc).
fn info_strings(doc: &lopdf::Document) -> (String, String) {
    let get = |key: &[u8]| -> String {
        doc.trailer
            .get(b"Info")
            .and_then(|o| o.as_reference())
            .and_then(|id| doc.get_object(id))
            .and_then(|o| o.as_dict())
            .and_then(|d| d.get(key))
            .and_then(|o| o.as_str())
            .map(decode_pdf_string)
            .unwrap_or_default()
    };
    (get(b"Title"), get(b"Author"))
}

fn decode_pdf_string(raw: &[u8]) -> String {
    if raw.len() >= 2 && raw[0] == 0xFE && raw[1] == 0xFF {
        // UTF-16BE.
        let u16s: Vec<u16> = raw[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&u16s)
    } else {
        // PDFDocEncoding overlaps Latin-1 for the printable range; good enough here.
        raw.iter().map(|&b| b as char).collect()
    }
}

// Placeholder colors.
const CANVAS: [u8; 4] = [24, 24, 30, 255];
const PAGE: [u8; 4] = [242, 242, 236, 255];
const BORDER: [u8; 4] = [168, 168, 168, 255];
const RED: [u8; 4] = [198, 45, 45, 255];
const DARK: [u8; 4] = [64, 64, 72, 255];
const MID: [u8; 4] = [118, 118, 128, 255];

/// Draw a recognizable PDF "page" tile: an off-white page at the true aspect ratio, a red
/// PDF badge, and the page count + point size. This is the thumbnail *and* the viewer image.
fn render_placeholder(meta: &PdfMeta) -> PixImage {
    // Size the raster so the longer side is ~360px, preserving the page aspect.
    let (pw, ph) = (meta.width_pt.max(1.0), meta.height_pt.max(1.0));
    let long = 360.0;
    let (w, h) = if pw >= ph {
        (long, (long * ph / pw).round())
    } else {
        ((long * pw / ph).round(), long)
    };
    let (w, h) = (w.max(120.0) as usize, h.max(120.0) as usize);
    let mut px = vec![CANVAS; w * h];

    // Page rectangle inset from the canvas, with a border.
    let inset = 10usize;
    let (px0, py0, px1, py1) = (inset, inset, w - inset, h - inset);
    fill_rect(&mut px, w, px0, py0, px1, py1, PAGE);
    stroke_rect(&mut px, w, px0, py0, px1, py1, BORDER);

    // Red PDF badge near the top, "PDF" scaled up.
    let badge_h = (h / 7).clamp(28, 64);
    fill_rect(
        &mut px,
        w,
        px0 + 6,
        py0 + 8,
        px1 - 6,
        py0 + 8 + badge_h,
        RED,
    );
    let scale = (badge_h / 16).max(1);
    let word = "PDF";
    let tw = word.len() * 8 * scale;
    let tx = px0 + 6 + ((px1 - px0 - 12).saturating_sub(tw)) / 2;
    let ty = py0 + 8 + (badge_h.saturating_sub(16 * scale)) / 2;
    blit_text(&mut px, w, tx, ty, word, [255, 255, 255, 255], scale);

    // "N pages" centered below the badge.
    let pages_line = if meta.pages == 1 {
        "1 page".to_string()
    } else {
        format!("{} pages", meta.pages)
    };
    let s2 = (scale.saturating_sub(1)).max(1);
    center_text(
        &mut px,
        w,
        px0,
        px1,
        py0 + 8 + badge_h + 20,
        &pages_line,
        DARK,
        s2,
    );

    // Point dimensions near the bottom.
    let dims = format!(
        "{} x {} pt",
        meta.width_pt.round() as i32,
        meta.height_pt.round() as i32
    );
    center_text(&mut px, w, px0, px1, py1.saturating_sub(24), &dims, MID, 1);

    PixImage::from_rgba(w as u32, h as u32, px)
}

fn fill_rect(px: &mut [[u8; 4]], w: usize, x0: usize, y0: usize, x1: usize, y1: usize, c: [u8; 4]) {
    for y in y0..y1 {
        for x in x0..x1 {
            if x < w {
                let i = y * w + x;
                if i < px.len() {
                    px[i] = c;
                }
            }
        }
    }
}

fn stroke_rect(
    px: &mut [[u8; 4]],
    w: usize,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    c: [u8; 4],
) {
    for x in x0..x1 {
        set(px, w, x, y0, c);
        set(px, w, x, y1 - 1, c);
    }
    for y in y0..y1 {
        set(px, w, x0, y, c);
        set(px, w, x1 - 1, y, c);
    }
}

fn set(px: &mut [[u8; 4]], w: usize, x: usize, y: usize, c: [u8; 4]) {
    if x < w {
        let i = y * w + x;
        if i < px.len() {
            px[i] = c;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn center_text(
    px: &mut [[u8; 4]],
    w: usize,
    x0: usize,
    x1: usize,
    y: usize,
    s: &str,
    c: [u8; 4],
    scale: usize,
) {
    let tw = s.chars().count() * 8 * scale;
    let x = x0 + ((x1 - x0).saturating_sub(tw)) / 2;
    blit_text(px, w, x, y, s, c, scale);
}

fn blit_text(
    px: &mut [[u8; 4]],
    w: usize,
    x0: usize,
    y0: usize,
    s: &str,
    c: [u8; 4],
    scale: usize,
) {
    for (i, ch) in s.chars().enumerate() {
        let byte = if (0x20..0x7f).contains(&(ch as u32)) {
            ch as u8
        } else {
            b'?'
        };
        let glyph = &CP437_8X16[byte as usize];
        let gx = x0 + i * 8 * scale;
        for (ry, &bits) in glyph.iter().enumerate() {
            for rx in 0..8 {
                if (bits >> (7 - rx)) & 1 == 1 {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            set(px, w, gx + rx * scale + sx, y0 + ry * scale + sy, c);
                        }
                    }
                }
            }
        }
    }
}

pub struct PdfDecoder;

impl Decoder for PdfDecoder {
    fn name(&self) -> &'static str {
        "pdf"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["pdf"]
    }

    fn sniff(&self, header: &[u8]) -> bool {
        header.starts_with(b"%PDF-")
    }

    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        // Prefer a REAL first-page render via poppler's `pdftoppm` (renders the actual page,
        // no bundled library). Fall back to the labeled placeholder tile when poppler isn't
        // installed or the render fails, so PDFs always show *something*.
        if let Some(page) = render_first_page(bytes) {
            return Ok(page);
        }
        let meta =
            pdf_meta(bytes).ok_or_else(|| DecodeError::Malformed("not a readable PDF".into()))?;
        Ok(render_placeholder(&meta))
    }
}

fn render_first_page(bytes: &[u8]) -> Option<PixImage> {
    render_page(bytes, 1, 1100)
}

/// Render one PDF page (1-based) to a raster. `scale_to` sets the longest side in px. Tries,
/// in order: in-process **pdfium** (bundled `pdfium.dll` on Windows — no external tool, no
/// console flash, works even in a folder full of PDFs), then poppler's **`pdftoppm`** (the
/// portable Unix path), else `None` so the caller shows a labeled placeholder tile. Backs both
/// the grid tile and the in-app multi-page viewer.
pub fn render_page(bytes: &[u8], page: usize, scale_to: u32) -> Option<PixImage> {
    if let Some(img) = render_via_pdfium(bytes, page, scale_to) {
        return Some(img);
    }
    render_via_pdftoppm(bytes, page, scale_to)
}

/// The lazily-loaded, process-wide pdfium instance (`None` if the library can't be found).
/// The `thread_safe` feature makes `Pdfium` `Send + Sync`, so a single instance is shared by
/// the multithreaded thumbnailer. Searched: next to the exe (where `build.rs` copies the
/// bundled DLL), the exe's parent (covers `target/<profile>/deps/` test binaries), the
/// `PDFIUM_DYNAMIC_LIB_PATH` override, then any system-installed pdfium.
fn pdfium_instance() -> Option<&'static pdfium_render::prelude::Pdfium> {
    use pdfium_render::prelude::*;
    use std::sync::OnceLock;
    static PDFIUM: OnceLock<Option<Pdfium>> = OnceLock::new();
    PDFIUM
        .get_or_init(|| {
            for candidate in pdfium_lib_candidates() {
                if let Ok(bindings) = Pdfium::bind_to_library(&candidate) {
                    return Some(Pdfium::new(bindings));
                }
            }
            Pdfium::bind_to_system_library().ok().map(Pdfium::new)
        })
        .as_ref()
}

fn pdfium_lib_candidates() -> Vec<std::path::PathBuf> {
    use pdfium_render::prelude::*;
    let mut v = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(Pdfium::pdfium_platform_library_name_at_path(&dir));
            if let Some(up) = dir.parent() {
                v.push(Pdfium::pdfium_platform_library_name_at_path(&up));
            }
        }
    }
    if let Ok(p) = std::env::var("PDFIUM_DYNAMIC_LIB_PATH") {
        let pb = std::path::PathBuf::from(&p);
        if pb.is_dir() {
            v.push(Pdfium::pdfium_platform_library_name_at_path(&pb));
        } else {
            v.push(pb);
        }
    }
    v
}

fn render_via_pdfium(bytes: &[u8], page: usize, scale_to: u32) -> Option<PixImage> {
    use pdfium_render::prelude::*;
    let pdfium = pdfium_instance()?;
    let doc = pdfium.load_pdf_from_byte_slice(bytes, None).ok()?;
    let index = (page.max(1) - 1) as i32;
    let pdf_page = doc.pages().get(index).ok()?;
    let wpt = pdf_page.width().value.max(1.0);
    let hpt = pdf_page.height().value.max(1.0);
    let factor = (scale_to.max(64) as f32 / wpt.max(hpt)).clamp(0.02, 20.0);
    let config = PdfRenderConfig::new().scale_page_by_factor(factor);
    let bitmap = pdf_page.render_with_config(&config).ok()?;
    let img = bitmap.as_image().ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let px: Vec<[u8; 4]> = img
        .into_raw()
        .chunks_exact(4)
        .map(|c| [c[0], c[1], c[2], c[3]])
        .collect();
    if px.len() != (w * h) as usize {
        return None;
    }
    Some(PixImage::from_rgba(w, h, px))
}

/// Render via poppler's `pdftoppm`: PDF on stdin, PNG on stdout — no temp file, no bundled lib.
/// `None` if pdftoppm is absent or errors.
fn render_via_pdftoppm(bytes: &[u8], page: usize, scale_to: u32) -> Option<PixImage> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let pg = page.max(1).to_string();
    let sc = scale_to.max(64).to_string();
    let mut child = Command::new("pdftoppm")
        .args([
            "-png",
            "-f",
            &pg,
            "-l",
            &pg,
            "-singlefile",
            "-scale-to",
            &sc,
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Feed stdin from a thread so a large PDF can't deadlock against stdout backpressure.
    let mut stdin = child.stdin.take()?;
    let owned = bytes.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&owned);
    });
    let out = child.wait_with_output().ok()?;
    let _ = writer.join();
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    let img = image::load_from_memory(&out.stdout).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    let px: Vec<[u8; 4]> = img
        .into_raw()
        .chunks_exact(4)
        .map(|c| [c[0], c[1], c[2], c[3]])
        .collect();
    if px.len() != (w * h) as usize {
        return None;
    }
    Some(PixImage::from_rgba(w, h, px))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Document, Object};

    /// A valid single-page Letter PDF built via lopdf (correct xref, no offset math).
    fn mini_pdf() -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        buf
    }

    #[test]
    fn sniffs_pdf_magic() {
        assert!(PdfDecoder.sniff(b"%PDF-1.7\n..."));
        assert!(!PdfDecoder.sniff(b"\x89PNG\r\n"));
    }

    #[test]
    fn parses_page_count_and_size() {
        let meta = pdf_meta(&mini_pdf()).expect("loads");
        assert_eq!(meta.pages, 1);
        assert!((meta.width_pt - 612.0).abs() < 1.0);
        assert!((meta.height_pt - 792.0).abs() < 1.0);
    }

    #[test]
    fn renders_a_placeholder_tile() {
        let img = PdfDecoder.decode(&mini_pdf()).unwrap();
        assert!(img.width > 100 && img.height > 100);
        // Portrait Letter → taller than wide.
        assert!(img.height > img.width);
    }

    #[test]
    fn utf16_title_decodes() {
        let raw = [0xFE, 0xFF, 0x00, b'H', 0x00, b'i'];
        assert_eq!(decode_pdf_string(&raw), "Hi");
    }

    /// End-to-end pdfium render of a real page. Only asserts when pdfium can be loaded
    /// (bundled `pdfium.dll` on Windows, or a system lib) — elsewhere the render path falls
    /// back to pdftoppm/placeholder, so there's nothing to check and the test no-ops.
    #[test]
    fn pdfium_renders_real_page_when_available() {
        if pdfium_instance().is_none() {
            eprintln!("pdfium not available on this platform — skipping render assertion");
            return;
        }
        let img = render_via_pdfium(&mini_pdf(), 1, 400).expect("pdfium should render page 1");
        // US Letter portrait → taller than wide; longest side ≈ the 400px request.
        assert!(
            img.height > img.width,
            "portrait page renders taller than wide"
        );
        assert!(
            img.width > 50 && img.height > 50 && img.height <= 420,
            "sane raster dimensions ({}x{})",
            img.width,
            img.height
        );
    }
}
