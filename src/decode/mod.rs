//! Format decoding: a tiny registry of pluggable decoders.
//!
//! Adding a new format is the whole extension story: implement `Decoder`, then
//! push it into `Registry::with_builtins`. Decoders are tried by magic-byte
//! sniff first, then by file extension as a fallback.

mod adf;
mod ansi;
mod aseprite;
mod audio;
mod bin;
mod bsave;
mod builtin;
mod c64_font;
mod atascii_font; // Atari 8-bit ATASCII 8×8 ROM (rasterized from the bundled Atari Classic TTF)
mod apple2_font; // Apple II text 8×8 ROM (rasterized from Apple2.ttf)
mod apple2_80_font; // Apple II 80-column (PR#3) font (rasterized from PRNumber3.ttf)
mod apple2_mousetext; // Apple //e MouseText glyphs (from Kreative Korp PrintChar21)
// Re-export the C64 ROM font + VIC-II palette so the image→PETSCII converter (in `thumb`)
// can reach them without exposing the whole decoder modules.
pub(crate) use c64_font::C64_FONT;
pub(crate) use apple2_font::APPLE2_FONT;
pub(crate) use apple2_80_font::APPLE2_80_FONT;
pub(crate) use apple2_mousetext::APPLE2_MOUSETEXT;
pub(crate) use atascii_font::ATASCII_FONT;
pub(crate) use rexpaint::XP_TRANSPARENT;
#[cfg(test)]
pub(crate) use petscii::VIC2; // only the thumb.rs PETSCII tests reference it by this path
pub(crate) use petscii::{petscii_palette, PETSCII_PALETTES};
mod code;
mod eps;
pub mod font;
pub mod font3d; // 3D font extrusion (the "3D logo maker") → an extruded Mesh3D for mesh3d::render
pub mod fon;
pub mod ico;
pub mod amiga_font;
pub mod tdf;
pub(crate) mod cp437_font; // shared with the ANSI-shade renderer in `thumb`
pub(crate) mod cp437_font_8x8; // 8×8 VGA50 font, also shared with the ANSI-shade renderer
mod idf;
mod iff;
pub mod mesh3d; // 3D models (OBJ/STL/PLY/glTF/GLB/DAE) → CPU-rendered thumbnail + geometry
pub mod opl3; // OPL3 FM-synth chip emulator (Opal port) — drives RAD playback
mod pcx;
mod pdf;
mod petmate;
mod petscii;
mod psd;
pub mod rad; // Reality Adlib Tracker replayer (RADPlayer port) — .rad → OPL3 register writes
mod rip;
mod rexpaint; // REXPaint .xp (gzipped layered CP437 art)
pub(crate) mod rexfont; // REXPaint bitmap fonts pack (16×16 glyph-grid PNGs → GlyphFont)
pub(crate) mod uniart; // Unicode-range glyphs (DejaVu → GlyphFont) for the Unicode ramp style
mod rip_chr;
mod svg;
mod tundra;
pub mod video; // video containers (mp4/mkv/webm/…) → ffmpeg frame grab + ffprobe metadata
mod xbin;
mod xcf;
mod xmind;

use crate::image_types::PixImage;
use std::path::Path;

/// Toggle the 9-dot VGA cell width for ANSI/CP437 rendering (a process-wide
/// preference read at decode time). Re-decode affected images to apply it.
pub use ansi::set_font_9px;

/// Progressive (byte-prefix) renderers for baud-rate playback — "watch it type/draw".
pub use ansi::TextStream;
pub use rip::RipStream;

/// Every source-code / text extension the [`code::CodeDecoder`] handles — re-exported so
/// `app.rs`'s viewer predicates (`is_image_ext`) can share the one list, not duplicate it.
pub use code::{
    decode_text, decode_with, encode_text, highlight_lines, lang_scopes, set_syntax_theme, tok_rgb, Tok,
    Encoding, ALL_TOKS, CODE_EXTS,
};

/// PDF metadata (page count / size / title / author) for the Details pane, and a
/// single-page renderer for the in-app multi-page viewer.
pub use pdf::{pdf_meta, render_page as render_pdf_page, PdfMeta};

/// EPS/PostScript extensions handled by the ghostscript-backed decoder (for `is_image_ext`).
pub use eps::EPS_EXTS;

/// Audio metadata (duration / sample rate / channels / codec) + the extension list, for
/// the Details pane and `is_image_ext`.
pub use audio::{audio_info, AudioInfo, AUDIO_EXTS};

/// Video metadata (duration / fps / dimensions / codec) + the extension list + a single-frame
/// grabber, for the Details pane, `is_image_ext`, and the interactive player's PNG export.
pub use video::{grab_frame as grab_video_frame, probe as probe_video, VideoInfo, VIDEO_EXTS};

/// XMind mind-map sheet titles + a per-sheet renderer (with a resolution knob for the
/// pseudo-vector zoom re-render), for the in-app multi-sheet viewer.
pub use xmind::{render_xmind_sheet, render_xmind_sheet_at, xmind_sheet_titles};

/// Rasterize an SVG at a target longest-side — the pseudo-vector zoom re-render (crisp zoom).
pub use svg::render_svg_at;

#[derive(Debug)]
pub enum DecodeError {
    Unsupported,
    Malformed(String),
    Io(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Unsupported => write!(f, "unsupported format"),
            DecodeError::Malformed(m) => write!(f, "malformed image: {m}"),
            DecodeError::Io(m) => write!(f, "io error: {m}"),
        }
    }
}

pub trait Decoder: Send + Sync {
    /// Human-readable decoder name. Part of the trait's descriptive API; not yet
    /// surfaced in the UI, so allow it to be unused for now.
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
    fn extensions(&self) -> &'static [&'static str];
    /// Cheap check against the first bytes of the file.
    fn sniff(&self, header: &[u8]) -> bool;
    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError>;
}

pub struct Registry {
    decoders: Vec<Box<dyn Decoder>>,
    // Optional "plugin" decoders the user can turn off (Preferences). Runtime flags —
    // `Registry` is shared as an `Arc` across worker threads, so these are atomics rather
    // than `&mut`. When off, the plugin's extensions vanish from the listing + won't decode.
    pdf_on: std::sync::atomic::AtomicBool,
    audio_on: std::sync::atomic::AtomicBool,
    code_on: std::sync::atomic::AtomicBool,
    mesh_on: std::sync::atomic::AtomicBool,
    video_on: std::sync::atomic::AtomicBool,
}

impl Registry {
    pub fn with_builtins() -> Self {
        install_panic_filter(); // a malformed file must never crash a worker / the app
        Self {
            pdf_on: std::sync::atomic::AtomicBool::new(true),
            audio_on: std::sync::atomic::AtomicBool::new(true),
            code_on: std::sync::atomic::AtomicBool::new(true),
            mesh_on: std::sync::atomic::AtomicBool::new(true),
            video_on: std::sync::atomic::AtomicBool::new(true),
            decoders: vec![
                Box::new(pcx::PcxDecoder),            // hand-written, palette-preserving
                Box::new(bsave::BsaveDecoder),        // .bsv BSAVE screen dump (CGA / mode 13h)
                Box::new(iff::IlbmDecoder),           // .iff/.ilbm/.lbm Amiga ILBM + PC chunky PBM
                Box::new(aseprite::AsepriteDecoder),  // .aseprite/.ase (asefile crate)
                Box::new(psd::PsdDecoder),            // .psd flattened (psd crate)
                Box::new(xcf::XcfDecoder),            // .xcf composited (xcf crate)
                Box::new(svg::SvgDecoder),            // .svg rasterized (resvg)
                Box::new(font::FontDecoder),          // .ttf/.otf/.ttc sample render (ab_glyph)
                Box::new(tdf::TdfDecoder),            // .tdf TheDraw fonts (retrofont → CP437 render)
                Box::new(fon::FonDecoder),            // .fon/.fnt Windows bitmap fonts (hand-rolled FNT)
                Box::new(amiga_font::AmigaFontDecoder), // .font + <size>.<n>C Amiga (Color)Fonts
                Box::new(xmind::XMindDecoder),        // .xmind mind map → SVG → raster (resvg)
                Box::new(ansi::AnsiDecoder),          // .ans/.asc/.nfo/.diz (CP437 + ANSI)
                Box::new(xbin::XBinDecoder),          // .xb/.xbin (binary ANSI: palette/font/RLE)
                Box::new(tundra::TundraDecoder),      // .tnd (TundraDraw — 24-bit truecolor)
                Box::new(rexpaint::RexPaintDecoder),  // .xp (REXPaint — gzipped layered CP437)
                Box::new(idf::IdfDecoder),            // .idf (iCE Draw — RLE + embedded font/pal)
                Box::new(adf::AdfDecoder),            // .adf (Artworx — embedded font/palette)
                Box::new(petscii::PetsciiDecoder), // .seq/.pet (Commodore PETSCII; icy_parser_core)
                Box::new(petmate::PetmateDecoder), // .petmate (nurpax/petmate JSON PETSCII)
                Box::new(rip::RipDecoder),         // .rip (RIPscript vector; icy_parser_core)
                Box::new(bin::BinDecoder),         // .bin (raw char/attr pairs, SAUCE width)
                Box::new(pdf::PdfDecoder),         // .pdf/.ai (PDF-compatible) page tile + metadata
                Box::new(eps::EpsDecoder),         // .eps/.ps rasterized via ghostscript (gs)
                Box::new(audio::SoundDecoder), // audio waveform / icon tile + metadata (symphonia)
                Box::new(code::CodeDecoder),   // source code / text (CP437 + hand-rolled highlight)
                Box::new(mesh3d::MeshDecoder), // .obj/.stl/.ply/.gltf/.glb/.dae → CPU-shaded tile
                Box::new(mesh3d::MtlDecoder),  // .mtl → material colour swatches
                Box::new(mesh3d::BlendDecoder), // .blend/.blend1 → placeholder (open in Blender)
                Box::new(video::VideoDecoder), // mp4/mkv/webm/… → ffmpeg frame grab (path-routed)
                Box::new(builtin::ImageCrateDecoder), // png/gif/bmp/jpeg/webp/tga/tiff/pnm/qoi
            ],
        }
    }

    /// Enable/disable an optional plugin by name ("pdf" / "audio" / "code"). Takes `&self`
    /// (atomic) so it works through the shared `Arc<Registry>`. Unknown names are ignored.
    pub fn set_plugin(&self, name: &str, on: bool) {
        use std::sync::atomic::Ordering::Relaxed;
        match name {
            "pdf" => self.pdf_on.store(on, Relaxed),
            "audio" => self.audio_on.store(on, Relaxed),
            "code" => self.code_on.store(on, Relaxed),
            "3d" => self.mesh_on.store(on, Relaxed),
            "video" => self.video_on.store(on, Relaxed),
            _ => {}
        }
    }

    /// Whether the extension belongs to a plugin that's currently switched OFF.
    fn plugin_disabled(&self, ext: &str) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        // PDF plugin covers .pdf + .ai (PDF-compatible) + EPS/PS (ghostscript) — all "document"
        // formats needing an external renderer.
        (!self.pdf_on.load(Relaxed)
            && (ext == "pdf" || ext == "ai" || eps::EPS_EXTS.contains(&ext)))
            || (!self.audio_on.load(Relaxed) && audio::AUDIO_EXTS.contains(&ext))
            || (!self.code_on.load(Relaxed) && code::CODE_EXTS.contains(&ext))
            || (!self.mesh_on.load(Relaxed)
                && (mesh3d::MESH_EXTS.contains(&ext) || mesh3d::AUX_EXTS.contains(&ext)))
            || (!self.video_on.load(Relaxed) && video::VIDEO_EXTS.contains(&ext))
    }

    /// Does any decoder claim this extension? Used to filter a folder listing. A disabled
    /// plugin's extensions report `false` here, so those files drop out of the grid.
    pub fn known_extension(&self, ext: &str) -> bool {
        let ext = ext.to_ascii_lowercase();
        if self.plugin_disabled(&ext) {
            return false;
        }
        self.decoders
            .iter()
            .any(|d| d.extensions().iter().any(|e| *e == ext))
    }

    pub fn decode_path(&self, path: &Path) -> Result<PixImage, DecodeError> {
        let bytes = std::fs::read(path).map_err(|e| DecodeError::Io(e.to_string()))?;
        self.decode_bytes(&bytes, path)
    }

    pub fn decode_bytes(&self, bytes: &[u8], path: &Path) -> Result<PixImage, DecodeError> {
        let header = &bytes[..bytes.len().min(32)];

        // A switched-off plugin doesn't decode its types at all (even on a direct open),
        // so nothing sneaks past the listing filter via the sniff/PDF-magic path.
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let ext = ext.to_ascii_lowercase();
            if self.plugin_disabled(&ext) {
                return Err(DecodeError::Unsupported);
            }
            // 3D models are routed by extension *before* the sniff loop: OBJ/PLY are plain
            // text another decoder might otherwise sniff, and the loaders need the PATH
            // (for an OBJ's .mtl / a glTF's .bin), which `decode(bytes)` can't provide.
            if mesh3d::MESH_EXTS.contains(&ext.as_str()) {
                return caught(|| mesh3d::decode_thumb(path, 512));
            }
            // .blend / .mtl are path-routed too: `.blend`'s tile is a cached Blender render
            // if one exists (right-click → Render), else a placeholder; `.mtl` renders a
            // lit material ball per material with its `map_Kd` texture — both need the PATH.
            if ext == "blend" || ext == "blend1" {
                return caught(|| mesh3d::decode_blend(path));
            }
            if ext == "mtl" {
                return caught(|| mesh3d::decode_mtl_path(path));
            }
            // Video is path-routed too: ffmpeg opens the file itself (no byte slice), and the
            // tile is a real grabbed frame. `decode(bytes)` is never used for video.
            if video::VIDEO_EXTS.contains(&ext.as_str()) {
                return caught(|| video::decode_thumb(path, 512));
            }
            // An Amiga `.font` is only a DESCRIPTOR: it names a size file in a sibling directory
            // (`Aggress/36.8C`) and holds no glyphs itself, so it needs the PATH. The size files
            // decode from bytes alone and go through the ordinary sniff/extension path.
            if ext == "font" {
                return caught(|| amiga_font::decode_path(path, bytes));
            }
        }

        // 1) A decoder whose magic bytes match wins.
        for d in &self.decoders {
            if d.sniff(header) {
                if let Ok(img) = decode_caught(d.as_ref(), bytes) {
                    return Ok(img);
                }
            }
        }
        // 2) Fall back to file extension.
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let ext = ext.to_ascii_lowercase();
            // Source-code / text needs the *extension* to pick the language spec, which the
            // generic `decode(bytes)` call can't pass — route it here (still panic-guarded).
            if code::CODE_EXTS.contains(&ext.as_str()) {
                return caught(|| code::CodeDecoder::decode_ext(bytes, &ext));
            }
            // Audio likewise needs the extension for symphonia's format hint.
            if audio::AUDIO_EXTS.contains(&ext.as_str()) {
                return caught(|| audio::SoundDecoder::decode_ext(bytes, &ext));
            }
            // TGA has NO magic bytes, so the sniff pass above can never match it and the
            // image crate's `with_guessed_format` fails on it — every .tga spun forever.
            // Hand the format in explicitly; the extension is the only evidence there is.
            if ext == "tga" {
                return caught(|| {
                    builtin::ImageCrateDecoder::decode_with(bytes, image::ImageFormat::Tga)
                });
            }
            for d in &self.decoders {
                if d.extensions().iter().any(|e| *e == ext) {
                    return decode_caught(d.as_ref(), bytes);
                }
            }
        } else {
            // 3) No extension: scene/BBS art is often shipped extensionless. Render it
            //    as CP437 text via the ANSI decoder — the same path .nfo/.asc take.
            for d in &self.decoders {
                if d.extensions().contains(&"ans") {
                    return decode_caught(d.as_ref(), bytes);
                }
            }
        }
        // 4) A nonstandard extension the sniff + extension passes both missed (.tri from
        //    TRIBE, .ice, group-specific ones): if it *looks like* scene text art by content
        //    (SAUCE, ANSI escapes, or CP437 block glyphs), render it via the ANSI decoder like
        //    extensionless art. (The listing includes such files via `file_is_scene_art`.)
        if looks_like_scene_art(bytes) {
            for d in &self.decoders {
                if d.extensions().contains(&"ans") {
                    return decode_caught(d.as_ref(), bytes);
                }
            }
        }
        Err(DecodeError::Unsupported)
    }
}

thread_local! {
    /// Set while a decoder is running, so the panic hook can stay quiet for the
    /// panics we catch in [`decode_caught`] (vs. reporting a genuine app bug).
    static DECODING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Install (once) a panic hook that silences panics raised *inside* a decoder — we
/// catch those in [`decode_caught`] and turn them into a normal decode error — while
/// still reporting any real panic elsewhere. Without this, a single malformed file
/// (e.g. the `psd` crate slice-indexing out of range) would crash a worker thread,
/// or the whole app when it lands on the main thread.
pub fn install_panic_filter() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if DECODING.with(std::cell::Cell::get) {
                return; // caught + handled as a decode error
            }
            prev(info);
        }));
    });
}

/// Run a decode closure with the panic filter armed, so one bad file fails gracefully.
fn caught<F>(f: F) -> Result<PixImage, DecodeError>
where
    F: FnOnce() -> Result<PixImage, DecodeError>,
{
    DECODING.with(|f| f.set(true));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    DECODING.with(|f| f.set(false));
    r.unwrap_or_else(|_| {
        Err(DecodeError::Malformed(
            "decoder panicked on this file".into(),
        ))
    })
}

/// Call a decoder, catching any panic so one bad file fails gracefully.
fn decode_caught(d: &dyn Decoder, bytes: &[u8]) -> Result<PixImage, DecodeError> {
    caught(|| d.decode(bytes))
}

/// Does `bytes` look like an ANSI/CP437 **text stream** (the kind `TextStream` renders and can
/// animate at a baud rate)? Unlike [`looks_like_scene_art`] this does NOT accept on a SAUCE
/// record alone — the *binary* scene formats (XBin/BIN/Tundra/…) carry SAUCE too but are RLE/
/// header blobs, not byte streams, so we require actual text content: few control bytes, plus a
/// run of ANSI CSI escapes (colour codes / cursor moves) or a real fraction of CP437 block glyphs.
pub fn looks_like_ansi_text(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(64 * 1024)];
    if sample.is_empty() {
        return false;
    }
    // Reject binaries: text art is (almost) all printable/CP437/whitespace; a binary carries NUL
    // + other C0 control bytes. >2% "hard" controls ⇒ not text. (CR/LF/TAB/ESC/FF/EOF ok.)
    let hard = sample
        .iter()
        .filter(|&&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0c | 0x1a | 0x1b))
        .count();
    if hard * 100 > sample.len() * 2 {
        return false;
    }
    // (a) A run of ANSI CSI escapes (`ESC[`) — the hallmark of .ans art; real screens have many.
    if sample.windows(2).filter(|w| *w == b"\x1b[").count() >= 4 {
        return true;
    }
    // (b) CP437 shading/block glyphs (░▒▓ ▀▄█▌▐ = 0xB0-0xB2, 0xDB-0xDF) — the hallmark of CP437
    //     "ASCII" art, rare in ordinary text; a couple of % ⇒ art.
    let blocks = sample
        .iter()
        .filter(|&&b| matches!(b, 0xB0..=0xB2 | 0xDB..=0xDF))
        .count();
    blocks * 100 >= sample.len() * 2
}

/// Heuristic: does `bytes` look like **scene text art** (ANSI / ASCII / CP437) even without a
/// known extension? Scene groups shipped art under all sorts of extensions (`.tri`, `.ice`, …)
/// and plenty of it has **no SAUCE**, so extension + SAUCE alone miss a lot. Accept a trailing
/// SAUCE record OR ANSI/CP437 text content — conservative, so binaries/prose don't leak in.
pub fn looks_like_scene_art(bytes: &[u8]) -> bool {
    crate::sauce::present(bytes) || looks_like_ansi_text(bytes)
}

/// As [`looks_like_scene_art`] but for a file on disk: reads a bounded head chunk (enough for
/// the escape/block heuristics + a small file's SAUCE) plus the tail for a large file's SAUCE.
/// Cheap enough to sniff a folder of unknown-extension files while building the listing.
pub fn file_is_scene_art(path: &Path) -> bool {
    // A known source-code extension is source, full stop — never sniff it as scene art. DOS-era
    // Pascal/BASIC/C often carry CP437 box-drawing comment banners (═══) that the block-glyph
    // heuristic would otherwise read as ANSI art, sending a `.pas` to the image viewer.
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if code::CODE_EXTS.contains(&ext.to_ascii_lowercase().as_str()) {
            return false;
        }
    }
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = vec![0u8; 32 * 1024];
    let n = std::io::Read::read(&mut f, &mut head).unwrap_or(0);
    head.truncate(n);
    looks_like_scene_art(&head) || crate::sauce::file_has_record(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::path::Path;

    #[test]
    fn known_extension_is_case_insensitive() {
        let r = Registry::with_builtins();
        assert!(r.known_extension("pcx"));
        assert!(r.known_extension("PNG"));
        assert!(r.known_extension("png"));
        assert!(!r.known_extension("xyz"));
    }

    /// A `.tga` decodes — the one format in the image-crate list with NO magic bytes.
    ///
    /// It carries no signature at the start of the file, so `guess_format` cannot identify it:
    /// the sniff pass never matches, and `with_guessed_format` then fails, which meant every
    /// single `.tga` ever opened produced a decode error and a thumbnail that spun forever —
    /// while the extension was advertised as supported. Found by the QA harness, where the
    /// fixture tile never stopped spinning.
    ///
    /// The fixture is written here by hand rather than by an encoder, so the test pins the
    /// plain 24-bit uncompressed layout instead of whatever a tool happens to emit (ImageMagick,
    /// for one, writes a colour-mapped TGA unless the image has enough colours to prevent it).
    #[test]
    fn decodes_a_tga_which_has_no_magic_bytes() {
        let (w, h) = (4u16, 3u16);
        let mut tga = vec![
            0, // id length
            0, // no colour map
            2, // uncompressed true-colour
            0, 0, 0, 0, 0, // colour map spec
            0, 0, 0, 0, // x/y origin
        ];
        tga.extend_from_slice(&w.to_le_bytes());
        tga.extend_from_slice(&h.to_le_bytes());
        tga.push(24); // bits per pixel
        tga.push(0); // descriptor: bottom-left origin
        // BGR, bottom row first — so the LAST row written is the image's TOP row.
        for row in 0..h {
            for _ in 0..w {
                if row == 0 {
                    tga.extend_from_slice(&[0x00, 0x00, 0xFF]); // bottom row: red
                } else {
                    tga.extend_from_slice(&[0xFF, 0x00, 0x00]); // rest: blue
                }
            }
        }

        let reg = Registry::with_builtins();
        let img = reg
            .decode_bytes(&tga, Path::new("fixture.tga"))
            .expect("a plain 24-bit TGA must decode");
        assert_eq!((img.width, img.height), (4, 3));
        // Bottom-left origin means the first row of the file is the BOTTOM of the image, so
        // the decoder must flip it: the top-left pixel is blue and the bottom-left is red.
        assert_eq!(img.pixels[0], [0x00, 0x00, 0xFF, 0xFF], "top-left should be blue");
        let bottom_left = (img.height as usize - 1) * img.width as usize;
        assert_eq!(img.pixels[bottom_left], [0xFF, 0x00, 0x00, 0xFF], "bottom-left should be red");
    }

    #[test]
    fn dispatches_png_through_image_crate() {
        let mut buf = Cursor::new(Vec::new());
        let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 255]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        let bytes = buf.into_inner();
        let decoded = Registry::with_builtins()
            .decode_bytes(&bytes, Path::new("x.png"))
            .expect("decode png");
        assert_eq!((decoded.width, decoded.height), (1, 1));
        assert_eq!(decoded.pixels[0], [1, 2, 3, 255]);
    }

    #[test]
    fn panicking_decoder_is_caught() {
        // A third-party decoder that slice-indexes out of range (like psd 0.3.5 on some
        // files) must surface as a decode error, not unwind the worker / the app.
        struct Boom;
        impl Decoder for Boom {
            fn name(&self) -> &'static str {
                "boom"
            }
            fn extensions(&self) -> &'static [&'static str] {
                &["boom"]
            }
            fn sniff(&self, _: &[u8]) -> bool {
                false
            }
            fn decode(&self, _: &[u8]) -> Result<PixImage, DecodeError> {
                let v: Vec<u8> = vec![0; 4];
                let _ = v[10]; // out-of-range index → panic, like the psd crate
                unreachable!()
            }
        }
        install_panic_filter();
        assert!(
            decode_caught(&Boom, b"x").is_err(),
            "a decoder panic must become a decode error"
        );
    }

    #[test]
    fn scene_art_detected_by_content_decodes_as_ansi() {
        let reg = Registry::with_builtins();
        // (a) SAUCE-bearing .tri.
        let mut with_sauce = b"TRIBE".to_vec();
        let mut sauce = vec![0u8; 128];
        sauce[..7].copy_from_slice(b"SAUCE00");
        sauce[94] = 1;
        with_sauce.extend_from_slice(&sauce);
        assert!(reg.decode_bytes(&with_sauce, Path::new("A.TRI")).is_ok());
        // (b) SAUCELESS ANSI (color codes + cursor moves) — the common .tri case.
        let ansi = b"\x1b[2J\x1b[1;37mT\x1b[31mR\x1b[32mI\x1b[33mB\x1b[34mE\x1b[0m\r\n".repeat(3);
        assert!(super::looks_like_scene_art(&ansi));
        assert!(reg.decode_bytes(&ansi, Path::new("B.TRI")).is_ok());
        // (c) SAUCELESS CP437 block art (░▒▓█) — no escapes at all.
        let cp437 = b"\xB0\xB1\xB2\xDB\xDC\xDD\xDE\xDF ART \xDB\xB2\xB1\xB0\r\n".repeat(4);
        assert!(super::looks_like_scene_art(&cp437));
        assert!(reg.decode_bytes(&cp437, Path::new("C.TRI")).is_ok());
        // Not art: a binary and plain prose stay unsupported / undetected.
        assert!(!super::looks_like_scene_art(b"\x00\x01\x02\x00\x03random\x00binary\x00\x00"));
        assert!(!super::looks_like_scene_art(
            b"This is just a normal readme file with plain ASCII prose and nothing arty."
        ));
        assert!(reg.decode_bytes(b"\x00\x01\x02\x00 binary", Path::new("x.tri")).is_err());

        // A SAUCE-bearing *binary* (XBin-like) counts as scene art, but is NOT an ANSI text
        // stream — so `for_file` won't mis-route it to TextStream (it keeps its cell-reveal).
        let mut bin = vec![0u8; 200]; // lots of NUL/control bytes = binary
        bin.extend_from_slice(&sauce);
        assert!(super::looks_like_scene_art(&bin)); // via SAUCE
        assert!(!super::looks_like_ansi_text(&bin)); // but not a text stream
    }

    /// A Pascal source file with a DOS box-drawing comment banner (CP437 blocks) must NOT be
    /// mistaken for ANSI art: the block-glyph heuristic would say "scene art", but a known code
    /// extension overrides it, so `.pas` routes to the text viewer/editor. Regression for a
    /// Turbo Pascal `.PAS` that opened in the image viewer.
    #[test]
    fn pascal_with_a_cp437_banner_is_code_not_scene_art() {
        // A comment banner drawn with CP437 double-line box glyphs (═ = 0xCD, block 0xDB), then
        // real Pascal — the block glyphs alone would trip `looks_like_ansi_text`.
        let mut src = b"{ \xC9\xCD\xCD\xCD\xCD\xCD\xCD\xBB\r\n".to_vec();
        src.extend_from_slice(b"  \xBA \xDB\xDB HELLO \xDB\xDB \xBA }\r\n");
        src.extend_from_slice(b"program Hello;\r\nbegin\r\n  WriteLn('hi');\r\nend.\r\n");

        let dir = std::env::temp_dir().join(format!("pv_pas_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("HELLO.PAS");
        std::fs::write(&f, &src).unwrap();
        // The extension guard wins over the content heuristic.
        assert!(
            !super::file_is_scene_art(&f),
            "a .pas is source code, not scene art, even with a CP437 banner"
        );
        // And it's advertised as a known (code) extension, so the listing shows it.
        let reg = Registry::with_builtins();
        assert!(reg.known_extension("pas"));
        assert!(reg.known_extension("PAS")); // case-insensitive
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn new_formats_are_registered() {
        let r = Registry::with_builtins();
        for ext in [
            "aseprite", "ase", "psd", "pcx", "xcf", "draw", "ico", "svg", "xb", "xbin", "bin",
            "ice", "cia", "tnd", "idf", "adf", "xmind",
        ] {
            assert!(r.known_extension(ext), "{ext} should be a known extension");
        }
    }

    #[test]
    fn aseprite_and_psd_sniff_magic() {
        let mut ase_hdr = [0u8; 8];
        ase_hdr[4] = 0xE0; // Aseprite magic 0xA5E0 (LE) at offset 4
        ase_hdr[5] = 0xA5;
        assert!(super::aseprite::AsepriteDecoder.sniff(&ase_hdr));
        assert!(!super::aseprite::AsepriteDecoder.sniff(&[0u8; 8]));
        assert!(super::psd::PsdDecoder.sniff(b"8BPS\x00\x01"));
        assert!(!super::psd::PsdDecoder.sniff(b"NOPE"));
        assert!(super::xcf::XcfDecoder.sniff(b"gimp xcf v011\0"));
        assert!(!super::xcf::XcfDecoder.sniff(b"nope"));
        assert!(super::svg::SvgDecoder.sniff(b"<?xml version=\"1.0\"?><svg"));
        assert!(super::svg::SvgDecoder.sniff(b"<svg xmlns=\"http://...\">"));
        assert!(!super::svg::SvgDecoder.sniff(b"\x89PNG\r\n"));
    }

    #[test]
    fn decodes_real_samples_if_present() {
        // Best-effort against real files on this machine; skips cleanly elsewhere.
        let samples = [
            "/home/grymmjack/Dropbox/DRAW-MOCKUP/Ship.psd",
            "/home/grymmjack/Dropbox/jup-jerk.aseprite",
            "/home/grymmjack/Dropbox/GJSCI/GJSCI-TEMPLATE-TILES.ase",
            "/home/grymmjack/git/QB64-Museum/rokcoder/nonograms/resources/nonograms.xcf",
            "/home/grymmjack/Dropbox/demon-face-gpt.svg",
            "/home/grymmjack/Pictures/Launchpad.ico",
        ];
        let r = Registry::with_builtins();
        for s in samples {
            let p = Path::new(s);
            if p.exists() {
                let img = r
                    .decode_path(p)
                    .unwrap_or_else(|e| panic!("decode {s}: {e}"));
                assert!(img.width > 0 && img.height > 0, "{s} decoded to zero size");
            }
        }
    }
}
