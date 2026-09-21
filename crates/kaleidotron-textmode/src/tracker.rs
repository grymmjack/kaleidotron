//! Tracker-module rendering (MOD/XM/S3M/IT) → interleaved-stereo f32 PCM at
//! 44.1 kHz, for the vscode-kaleidotron audio player. Mirrors kaleidotron's
//! `render_tracker` (xmrs + xmrsplayer).

/// Extensions xmrs can parse + synthesize.
pub fn is_tracker_ext(ext: &str) -> bool {
    matches!(
        ext.trim_start_matches('.').to_ascii_lowercase().as_str(),
        "mod" | "xm" | "s3m" | "it"
    )
}

/// Render a module to interleaved-stereo f32 at 44.1 kHz (one pass; 10-min cap).
/// Returns `None` if the bytes aren't a parseable module.
pub fn render(bytes: &[u8]) -> Option<Vec<f32>> {
    use xmrsplayer::xmrsplayer::XmrsPlayer;
    let module = xmrs::core::module::Module::load(bytes).ok()?;
    let sr = 44_100u32;
    let mut player = XmrsPlayer::new(&module, sr, 0);
    player.set_max_loop_count(1);
    let cap = sr as usize * 2 * 600; // 10-min stereo safety cap
    let samples: Vec<f32> = (&mut player).take(cap).map(|s| s as f32 / 32768.0).collect();
    if samples.is_empty() {
        None
    } else {
        Some(samples)
    }
}

/// Render a RAD (Reality Adlib Tracker) module via OPL3 FM synthesis to
/// interleaved-stereo f32 at 44.1 kHz. Mirrors kaleidotron's `render_rad`.
pub fn render_rad(bytes: &[u8]) -> Option<Vec<f32>> {
    use crate::decode::{opl3::Opl3, rad::RadPlayer};
    let sr = 44_100u32;
    let mut player = RadPlayer::new(bytes)?;
    let mut chip = Opl3::new(sr);
    let hz = player.hz().clamp(1.0, 1000.0);
    let per_tick = (sr as f64 / hz).round().max(1.0) as usize;
    let max_frames = sr as usize * 600; // 10-min cap
    let mut out: Vec<f32> = Vec::new();
    loop {
        let playing = player.update(&mut |reg, val| chip.write_reg(reg, val));
        for _ in 0..per_tick {
            let (l, r) = chip.sample();
            out.push(l as f32 / 32768.0);
            out.push(r as f32 / 32768.0);
        }
        if !playing || out.len() >= max_frames * 2 {
            break;
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Strip a RIFF/RMID wrapper from a `.rmi`, returning the inner SMF bytes.
fn rmid_inner(bytes: &[u8]) -> &[u8] {
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"RMID" {
        let mut i = 12;
        while i + 8 <= bytes.len() {
            let sz =
                u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]]) as usize;
            let start = i + 8;
            let end = (start + sz).min(bytes.len());
            if &bytes[i..i + 4] == b"data" {
                return &bytes[start..end];
            }
            i = start + sz + (sz & 1);
        }
    }
    bytes
}

/// Render a standard MIDI file through a General MIDI SoundFont (`.sf2` bytes) to
/// interleaved-stereo f32 at 44.1 kHz. `None` if the MIDI or SoundFont is invalid.
pub fn render_midi(midi_bytes: &[u8], sf2_bytes: &[u8]) -> Option<Vec<f32>> {
    use rustysynth::{MidiFile, MidiFileSequencer, SoundFont, Synthesizer, SynthesizerSettings};
    use std::io::Cursor;
    use std::sync::Arc;
    let sr = 44_100i32;
    let sf = Arc::new(SoundFont::new(&mut Cursor::new(sf2_bytes)).ok()?);
    let settings = SynthesizerSettings::new(sr);
    let synth = Synthesizer::new(&sf, &settings).ok()?;
    let mut seq = MidiFileSequencer::new(synth);
    let midi = Arc::new(MidiFile::new(&mut Cursor::new(rmid_inner(midi_bytes))).ok()?);
    let secs = midi.get_length() + 1.0;
    seq.play(&midi, false);
    let frames = (((sr as f64) * secs).ceil() as usize).clamp(1, sr as usize * 3600);
    let mut left = vec![0f32; frames];
    let mut right = vec![0f32; frames];
    seq.render(&mut left, &mut right);
    let mut out = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        out.push(left[i]);
        out.push(right[i]);
    }
    Some(out)
}
