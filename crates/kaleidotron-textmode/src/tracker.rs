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
