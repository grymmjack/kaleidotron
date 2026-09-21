//! A lean, no-bindgen WebAssembly ABI over `kaleidotron-textmode`.
//!
//! The webview drives it with four steps, reading wasm linear memory directly:
//!   1. `p = input_ptr(len)`  — reserve an input buffer, get its pointer.
//!   2. copy the file bytes into `memory[p .. p+len]`.
//!   3. `ok = decode_input(ext_code, font9)` — decode; 1 = success, 0 = failure.
//!   4. read `out_w()` × `out_h()` RGBA8 pixels from `out_ptr() .. + out_len()`.
//!
//! No wasm-bindgen, no allocator shims beyond `Vec` — the whole thing is a
//! handful of `extern "C"` functions over two thread-local buffers.

use std::cell::RefCell;

thread_local! {
    /// The caller-filled input file bytes.
    static INPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    /// The finished RGBA8 output + its dimensions.
    static OUTPUT: RefCell<(u32, u32, Vec<u8>)> = const { RefCell::new((0, 0, Vec::new())) };
    /// Rendered PCM (interleaved-stereo f32 at 44.1 kHz) — tracker/RAD/MIDI.
    static AUDIO: RefCell<Vec<f32>> = const { RefCell::new(Vec::new()) };
    /// The caller-filled SoundFont (.sf2) bytes, for MIDI synthesis.
    static SOUNDFONT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    /// The caller-filled custom sample text (UTF-8), for the font viewer.
    static TEXT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Map a small integer code to the extension string the decoder dispatches on.
/// The webview picks the code from the file's extension. Anything unrecognised
/// (`nfo`/`txt`/`msg`/…) maps to ANSI/CP437, which is the text catch-all anyway.
fn ext_str(code: u32) -> &'static str {
    match code {
        0 => "ans",
        1 => "xb",
        2 => "xbin",
        3 => "bin",
        4 => "tnd",
        5 => "idf",
        6 => "adf",
        7 => "seq",
        8 => "pet",
        9 => "rip",
        // Raster formats (most are caught by magic-byte sniff regardless of code;
        // the extension only matters for TGA, which has no magic).
        10 => "pcx",
        11 => "psd",
        12 => "xcf",
        13 => "ase",
        14 => "iff",
        15 => "tga",
        16 => "tiff",
        17 => "qoi",
        18 => "pnm",
        19 => "ff",
        20 => "petmate",
        21 => "ttf", // TTF/OTF vector font preview
        22 => "f16", // raw bitmap font (.fon/.fnt/.psf/.fNN) — height from file size
        23 => "tdf", // TheDraw font
        25 => "bsave", // QB64/BASIC BSAVE image
        _ => "ans",
    }
}

/// Reserve (or grow) the input buffer to `len` bytes and return a pointer to it.
/// The caller writes the file bytes there, then calls [`decode_input`].
///
/// # Safety
/// The returned pointer is valid until the next call to `input_ptr`.
#[no_mangle]
pub extern "C" fn input_ptr(len: usize) -> *mut u8 {
    INPUT.with(|b| {
        let mut b = b.borrow_mut();
        b.clear();
        b.resize(len, 0);
        b.as_mut_ptr()
    })
}

/// Decode the bytes currently in the input buffer. `ext_code` selects the format
/// (see [`ext_str`]); `font9` (0/1) toggles the 9-dot VGA cell for ANSI/CP437.
/// Returns 1 on success, 0 on failure.
#[no_mangle]
pub extern "C" fn decode_input(ext_code: u32, font9: u32) -> u32 {
    kaleidotron_textmode::set_font_9px(font9 != 0);
    let result = INPUT.with(|b| {
        let bytes = b.borrow();
        kaleidotron_textmode::decode(&bytes, ext_str(ext_code))
    });
    match result {
        Ok(img) => {
            OUTPUT.with(|o| {
                *o.borrow_mut() = (img.width, img.height, img.rgba_bytes());
            });
            1
        }
        Err(_) => {
            OUTPUT.with(|o| *o.borrow_mut() = (0, 0, Vec::new()));
            0
        }
    }
}

/// Width of the last successful decode, in pixels.
#[no_mangle]
pub extern "C" fn out_w() -> u32 {
    OUTPUT.with(|o| o.borrow().0)
}

/// Height of the last successful decode, in pixels.
#[no_mangle]
pub extern "C" fn out_h() -> u32 {
    OUTPUT.with(|o| o.borrow().1)
}

/// Pointer to the last decode's RGBA8 buffer (`out_len()` bytes).
#[no_mangle]
pub extern "C" fn out_ptr() -> *const u8 {
    OUTPUT.with(|o| o.borrow().2.as_ptr())
}

/// Byte length of the last decode's RGBA8 buffer (= w·h·4).
#[no_mangle]
pub extern "C" fn out_len() -> u32 {
    OUTPUT.with(|o| o.borrow().2.len() as u32)
}

// ---- font viewer: sample (custom text) or glyph grid ----

/// Reserve (or grow) the text buffer to `len` bytes and return its pointer. The
/// caller writes the UTF-8 sample text there before calling [`decode_font`].
#[no_mangle]
pub extern "C" fn text_ptr(len: usize) -> *mut u8 {
    TEXT.with(|b| {
        let mut b = b.borrow_mut();
        b.clear();
        b.resize(len, 0);
        b.as_mut_ptr()
    })
}

/// Render the font in the input buffer to the RGBA output (read via `out_*`).
/// `mode`: 0 = sample only, 1 = grid only, 2 = both (sample + grid). The sample
/// uses the text buffer (empty → the format's default "font name"). Returns 1 ok.
#[no_mangle]
pub extern "C" fn decode_font(ext_code: u32, mode: u32) -> u32 {
    let text = TEXT.with(|t| String::from_utf8_lossy(&t.borrow()).into_owned());
    let img = INPUT.with(|b| {
        kaleidotron_textmode::render_font(&b.borrow(), ext_str(ext_code), mode, &text)
    });
    match img {
        Some(im) => {
            OUTPUT.with(|o| *o.borrow_mut() = (im.width, im.height, im.rgba_bytes()));
            1
        }
        None => {
            OUTPUT.with(|o| *o.borrow_mut() = (0, 0, Vec::new()));
            0
        }
    }
}

// ---- audio: render a tracker module (MOD/XM/S3M/IT) to PCM ----

/// Render the tracker module currently in the input buffer to interleaved-stereo
/// f32 PCM at 44.1 kHz. Returns 1 on success, 0 on failure.
#[no_mangle]
pub extern "C" fn decode_tracker() -> u32 {
    let pcm = INPUT.with(|b| kaleidotron_textmode::tracker::render(&b.borrow()));
    match pcm {
        Some(p) if !p.is_empty() => {
            AUDIO.with(|a| *a.borrow_mut() = p);
            1
        }
        _ => {
            AUDIO.with(|a| a.borrow_mut().clear());
            0
        }
    }
}

/// Render the RAD (Reality Adlib Tracker) module in the input buffer via OPL3 FM
/// synthesis. Returns 1 on success, 0 on failure.
#[no_mangle]
pub extern "C" fn decode_rad() -> u32 {
    let pcm = INPUT.with(|b| kaleidotron_textmode::tracker::render_rad(&b.borrow()));
    match pcm {
        Some(p) if !p.is_empty() => {
            AUDIO.with(|a| *a.borrow_mut() = p);
            1
        }
        _ => {
            AUDIO.with(|a| a.borrow_mut().clear());
            0
        }
    }
}

/// Reserve (or grow) the SoundFont buffer to `len` bytes and return its pointer.
/// The caller writes the `.sf2` bytes there before calling [`decode_midi`].
#[no_mangle]
pub extern "C" fn soundfont_ptr(len: usize) -> *mut u8 {
    SOUNDFONT.with(|b| {
        let mut b = b.borrow_mut();
        b.clear();
        b.resize(len, 0);
        b.as_mut_ptr()
    })
}

/// Render the MIDI file in the input buffer through the SoundFont in the
/// soundfont buffer. Returns 1 on success, 0 on failure (bad MIDI/SoundFont).
#[no_mangle]
pub extern "C" fn decode_midi() -> u32 {
    let pcm = INPUT.with(|midi| {
        SOUNDFONT.with(|sf| {
            kaleidotron_textmode::tracker::render_midi(&midi.borrow(), &sf.borrow())
        })
    });
    match pcm {
        Some(p) if !p.is_empty() => {
            AUDIO.with(|a| *a.borrow_mut() = p);
            1
        }
        _ => {
            AUDIO.with(|a| a.borrow_mut().clear());
            0
        }
    }
}

/// Pointer to the rendered PCM (f32 samples, interleaved stereo).
#[no_mangle]
pub extern "C" fn audio_ptr() -> *const u8 {
    AUDIO.with(|a| a.borrow().as_ptr() as *const u8)
}

/// Byte length of the rendered PCM (= sample count · 4).
#[no_mangle]
pub extern "C" fn audio_len() -> u32 {
    AUDIO.with(|a| (a.borrow().len() * 4) as u32)
}

/// Channel count of the rendered PCM (always 2).
#[no_mangle]
pub extern "C" fn audio_channels() -> u32 {
    2
}

/// Sample rate of the rendered PCM (always 44100).
#[no_mangle]
pub extern "C" fn audio_rate() -> u32 {
    44_100
}
