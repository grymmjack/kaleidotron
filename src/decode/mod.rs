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
mod builtin;
mod c64_font;
mod code;
mod eps;
pub mod font;
pub mod fon;
pub mod tdf;
mod cp437_font;
mod cp437_font_8x8;
mod idf;
pub mod mesh3d; // 3D models (OBJ/STL/PLY/glTF/GLB/DAE) → CPU-rendered thumbnail + geometry
pub mod opl3; // OPL3 FM-synth chip emulator (Opal port) — drives RAD playback
mod pcx;
mod pdf;
mod petmate;
mod petscii;
mod psd;
pub mod rad; // Reality Adlib Tracker replayer (RADPlayer port) — .rad → OPL3 register writes
mod rip;
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
pub use code::CODE_EXTS;

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
                Box::new(aseprite::AsepriteDecoder),  // .aseprite/.ase (asefile crate)
                Box::new(psd::PsdDecoder),            // .psd flattened (psd crate)
                Box::new(xcf::XcfDecoder),            // .xcf composited (xcf crate)
                Box::new(svg::SvgDecoder),            // .svg rasterized (resvg)
                Box::new(font::FontDecoder),          // .ttf/.otf/.ttc sample render (ab_glyph)
                Box::new(tdf::TdfDecoder),            // .tdf TheDraw fonts (retrofont → CP437 render)
                Box::new(fon::FonDecoder),            // .fon/.fnt Windows bitmap fonts (hand-rolled FNT)
                Box::new(xmind::XMindDecoder),        // .xmind mind map → SVG → raster (resvg)
                Box::new(ansi::AnsiDecoder),          // .ans/.asc/.nfo/.diz (CP437 + ANSI)
                Box::new(xbin::XBinDecoder),          // .xb/.xbin (binary ANSI: palette/font/RLE)
                Box::new(tundra::TundraDecoder),      // .tnd (TundraDraw — 24-bit truecolor)
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
