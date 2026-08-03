//! Interactive video playback (the still-frame + metadata side is `decode/video.rs`).
//!
//! Design mirrors the app's audio subsystem's three layers, adapted for a *stream*:
//!   - decode is not one blob but a **bounded frame pipe** — an ffmpeg process emits raw RGBA
//!     frames on stdout, a reader thread pushes them over a `sync_channel` (backpressure keeps
//!     memory flat regardless of clip length), and the UI pulls frames paced to a clock.
//!   - [`VideoLoading`] is the in-flight background open (probe + whole-track audio extract).
//!   - [`VideoPlayer`] is the live, device-bound player (frame stream + a small self-contained
//!     rodio soundtrack engine + the A/V sync clock).
//!
//! The soundtrack is **decoupled from the audio plugin** on purpose: video must have sound
//! whether or not the audio plugin is enabled, so it owns its own rodio device rather than
//! reusing `AudioPlayer`. Master volume/mute are passed in from the app's persisted settings.
//!
//! Everything shells out to `ffmpeg` (frames + audio) / `ffprobe` (metadata), degrading to a
//! silent, frameless player when the tools are missing — matching the pdf/blender ethos.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;

use crate::decode::VideoInfo;

/// Longest side (px) of decoded display frames. Streaming *raw* RGBA is bandwidth-heavy, so we
/// let ffmpeg downscale before the pipe — a 1600px frame is ~10 MB, ×`FRAME_BUF` ≈ 60 MB max.
/// PNG *export* re-grabs at native resolution, so this only bounds the on-screen preview.
const DISPLAY_CAP: u32 = 1600;
/// Frames buffered ahead of the playhead (the backpressure window).
const FRAME_BUF: usize = 8;

/// One decoded frame: its presentation timestamp (seconds from clip start) + RGBA bytes.
struct VideoFrame {
    pts: f32,
    data: Vec<u8>,
}

/// Compute the on-screen frame size: fit the native dimensions inside `DISPLAY_CAP` (never
/// upscaling), rounded to even numbers (some ffmpeg scalers dislike odd dims).
fn display_dims(info: &VideoInfo) -> (u32, u32) {
    let (w, h) = (info.width.max(2), info.height.max(2));
    let long = w.max(h);
    let (mut dw, mut dh) = if long > DISPLAY_CAP {
        let f = DISPLAY_CAP as f32 / long as f32;
        (
            ((w as f32 * f).round() as u32).max(2),
            ((h as f32 * f).round() as u32).max(2),
        )
    } else {
        (w, h)
    };
    dw -= dw % 2;
    dh -= dh % 2;
    (dw.max(2), dh.max(2))
}

/// A running ffmpeg raw-frame reader. Dropping it cancels the reader thread and kills ffmpeg
/// (closing the pipe), so seeking is just "drop the old stream, spawn a new one at `-ss t`".
struct FrameStream {
    child: Child,
    rx: Receiver<VideoFrame>,
    cancel: Arc<AtomicBool>,
    /// A frame pulled from the channel but not yet due — held back until the clock reaches it.
    pending: Option<VideoFrame>,
    /// True once the channel disconnected (ffmpeg finished / errored): no more frames coming.
    finished: bool,
}

impl FrameStream {
    /// Spawn `ffmpeg` decoding `path` from `start` seconds, scaled to `w×h`, RGBA on stdout.
    /// `-ss` before `-i` = fast keyframe seek; `-an` drops audio (played separately). `None`
    /// if ffmpeg can't be launched.
    fn spawn(path: &Path, w: u32, h: u32, fps: f32, start: f32) -> Option<Self> {
        let mut cmd = Command::new("ffmpeg");
        cmd.arg("-v").arg("quiet").arg("-nostdin");
        if start > 0.02 {
            cmd.arg("-ss").arg(format!("{start:.3}"));
        }
        cmd.arg("-i").arg(path);
        cmd.arg("-an") // no audio in this pipe
            .arg("-f")
            .arg("rawvideo")
            .arg("-pix_fmt")
            .arg("rgba")
            .arg("-s")
            .arg(format!("{w}x{h}"))
            .arg("-");
        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut stdout = child.stdout.take()?;
        let (tx, rx): (SyncSender<VideoFrame>, Receiver<VideoFrame>) = sync_channel(FRAME_BUF);
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_thread = cancel.clone();
        let fps = if fps > 0.1 { fps } else { 30.0 };
        let frame_bytes = (w as usize) * (h as usize) * 4;
        std::thread::spawn(move || {
            let mut idx: u64 = 0;
            let mut buf = vec![0u8; frame_bytes];
            loop {
                if cancel_thread.load(Ordering::Relaxed) {
                    break;
                }
                // A full frame's worth of bytes, or EOF/short read → stop.
                if stdout.read_exact(&mut buf).is_err() {
                    break;
                }
                let pts = start + idx as f32 / fps;
                idx += 1;
                // Blocks here once the buffer is full (backpressure). A dropped rx → Err → stop.
                if tx
                    .send(VideoFrame {
                        pts,
                        data: buf.clone(),
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Some(FrameStream {
            child,
            rx,
            cancel,
            pending: None,
            finished: false,
        })
    }
}

impl Drop for FrameStream {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.child.kill(); // closing the pipe unblocks the reader thread
        let _ = self.child.wait(); // reap the zombie
    }
}

/// A minimal self-contained rodio soundtrack engine (independent of the app's `AudioPlayer`).
/// Decoded PCM is held once; each (re)start slices from an offset onto a fresh player — the
/// same "fresh player per start" idiom the audio subsystem uses.
struct Sound {
    _stream: rodio::MixerDeviceSink,
    player: rodio::Player,
    samples: Vec<f32>, // interleaved stereo @ `rate`
    rate: u32,
    base: f32,   // seconds offset where the current appended buffer starts
    speed: f32,  // playback speed (also the clock multiplier)
    volume: f32, // 0..1 master volume
    muted: bool,
}

impl Sound {
    fn open(samples: Vec<f32>, rate: u32, volume: f32, muted: bool) -> Option<Self> {
        let stream = rodio::DeviceSinkBuilder::open_default_sink().ok()?;
        let player = rodio::Player::connect_new(stream.mixer());
        Some(Sound {
            _stream: stream,
            player,
            samples,
            rate,
            base: 0.0,
            speed: 1.0,
            volume,
            muted,
        })
    }

    fn effective_volume(&self) -> f32 {
        if self.muted {
            0.0
        } else {
            self.volume
        }
    }

    /// (Re)start playback from `secs` on a fresh player at the current speed; pause immediately
    /// if `play` is false (so a paused seek still cues the audio to the sought position).
    fn play_from(&mut self, secs: f32, play: bool) {
        use rodio::Source;
        let ch = 2usize;
        let n = self.samples.len();
        let s = (((secs.max(0.0) * self.rate as f32) as usize) * ch).min(n);
        let region: Vec<f32> = self.samples[s..].to_vec();
        let channels = std::num::NonZeroU16::new(2).unwrap();
        let rate = std::num::NonZeroU32::new(self.rate.max(1)).unwrap();
        let buf = rodio::buffer::SamplesBuffer::new(channels, rate, region).speed(self.speed);
        let player = rodio::Player::connect_new(self._stream.mixer());
        player.set_volume(self.effective_volume());
        player.append(buf);
        player.play();
        if !play {
            player.pause();
        }
        self.player = player;
        self.base = secs.max(0.0);
    }

    /// Current audio position in clip seconds (base offset + elapsed × speed). Frozen while paused.
    fn pos(&self) -> f32 {
        self.base + self.player.get_pos().as_secs_f32() * self.speed
    }

    fn pause(&self) {
        self.player.pause();
    }
    fn resume(&self) {
        self.player.play();
    }
    fn is_paused(&self) -> bool {
        self.player.is_paused()
    }
    fn apply_volume(&self) {
        self.player.set_volume(self.effective_volume());
    }
}

/// A background video open in flight (probe + whole audio-track extraction — the slow part).
/// `poll_video_load` builds the player when the worker finishes.
pub struct VideoLoading {
    pub path: PathBuf,
    pub t: f32, // seconds spent loading (delays the spinner so quick opens don't flash)
    rx: Receiver<Result<Loaded, String>>,
}

/// The worker's finished payload: metadata + the extracted stereo PCM (if the file has audio).
struct Loaded {
    info: VideoInfo,
    audio: Option<(Vec<f32>, u32)>, // (interleaved stereo samples, sample rate)
}

impl VideoLoading {
    /// Kick off the background open. Probes for metadata and extracts the full audio track to
    /// PCM (the "decode whole track upfront" choice — gives seek/volume for free).
    pub fn start(path: PathBuf) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_path = path.clone();
        std::thread::spawn(move || {
            let res = match crate::decode::probe_video(&worker_path) {
                Some(info) => {
                    let audio = if info.has_audio {
                        extract_audio(&worker_path)
                    } else {
                        None
                    };
                    Ok(Loaded { info, audio })
                }
                None => Err("ffprobe: not a readable video (is ffmpeg installed?)".to_string()),
            };
            let _ = tx.send(res);
        });
        VideoLoading { path, t: 0.0, rx }
    }

    /// Non-blocking check: `Some(Ok(player))` once the background open finishes (builds the live
    /// player), `Some(Err)` on failure, `None` while still loading. Master `volume`/`muted` and
    /// `autoplay` are applied to the new player.
    pub fn try_build(
        &mut self,
        volume: f32,
        muted: bool,
        autoplay: bool,
    ) -> Option<Result<VideoPlayer, String>> {
        use std::sync::mpsc::TryRecvError;
        match self.rx.try_recv() {
            Ok(Ok(loaded)) => Some(Ok(VideoPlayer::new(
                self.path.clone(),
                loaded.info,
                loaded.audio,
                volume,
                muted,
                autoplay,
            ))),
            Ok(Err(e)) => Some(Err(e)),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("video open failed".into())),
        }
    }
}

/// Extract the whole audio track as interleaved stereo f32 @ 44.1 kHz via ffmpeg. `None` if
/// ffmpeg is absent, the file has no audio, or output is empty.
fn extract_audio(path: &Path) -> Option<(Vec<f32>, u32)> {
    let out = Command::new("ffmpeg")
        .args(["-v", "quiet", "-nostdin"])
        .arg("-i")
        .arg(path)
        .args([
            "-vn", // no video
            "-ac", "2", "-ar", "44100", "-f", "f32le", "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.len() < 8 {
        return None;
    }
    let samples: Vec<f32> = out
        .stdout
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    if samples.is_empty() {
        return None;
    }
    Some((samples, 44100))
}

/// The live, device-bound video player.
pub struct VideoPlayer {
    pub path: PathBuf,
    pub info: VideoInfo,
    pub disp_w: u32,
    pub disp_h: u32,

    stream: Option<FrameStream>,
    sound: Option<Sound>,

    /// The currently displayed frame's RGBA bytes (`disp_w·disp_h·4`) + its timestamp.
    pub cur: Option<Vec<u8>>,
    pub cur_pts: f32,
    /// Set when `cur` changed since the last UI upload — the app re-uploads the texture then clears it.
    pub tex_dirty: bool,

    pub playing: bool,
    pub speed: f32,
    /// Wall-clock playhead used only when there's no audio track to sync to.
    wall: f32,
    pub ended: bool,
}

impl VideoPlayer {
    /// Build a live player: open the soundtrack device and start the frame stream at 0.
    /// `autoplay` starts playback immediately. Master `volume`/`muted` come from app settings.
    pub fn new(
        path: PathBuf,
        info: VideoInfo,
        audio: Option<(Vec<f32>, u32)>,
        volume: f32,
        muted: bool,
        autoplay: bool,
    ) -> Self {
        let (disp_w, disp_h) = display_dims(&info);
        let sound = audio.and_then(|(s, r)| Sound::open(s, r, volume, muted));
        let stream = FrameStream::spawn(&path, disp_w, disp_h, info.fps, 0.0);
        let mut vp = VideoPlayer {
            path,
            info,
            disp_w,
            disp_h,
            stream,
            sound,
            cur: None,
            cur_pts: 0.0,
            tex_dirty: false,
            playing: false,
            speed: 1.0,
            wall: 0.0,
            ended: false,
        };
        if let Some(s) = vp.sound.as_mut() {
            s.play_from(0.0, autoplay);
        }
        vp.playing = autoplay;
        vp
    }

    pub fn duration(&self) -> f32 {
        self.info.duration.max(0.0)
    }

    /// The master playback clock in clip seconds: the audio position when there's a soundtrack,
    /// else the wall-clock accumulator. This is what the frame selection below chases.
    fn clock(&self) -> f32 {
        match &self.sound {
            Some(s) => s.pos(),
            None => self.wall,
        }
    }

    /// Advance one UI frame: move the clock (when playing) and pull the newest *due* frame from
    /// the stream, holding the next one back. Returns nothing; sets `tex_dirty`/`ended`.
    ///
    /// The frame-selection policy — show the latest frame whose `pts <= clock`, hold the first
    /// future frame in `pending` — is a deliberate "present-on-time, drop-if-behind" scheme:
    /// on a slow machine it silently skips late frames to stay in audio sync rather than lagging.
    pub fn tick(&mut self, dt: f32) {
        if self.playing && self.sound.is_none() {
            self.wall += dt * self.speed; // no audio → drive the clock by wall time
        }
        let t = self.clock();

        // Pull frames up to the clock (even while paused, so a seek cues the correct frame).
        if let Some(st) = self.stream.as_mut() {
            loop {
                let frame = st.pending.take().or_else(|| match st.rx.try_recv() {
                    Ok(f) => Some(f),
                    Err(std::sync::mpsc::TryRecvError::Empty) => None,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        st.finished = true;
                        None
                    }
                });
                match frame {
                    Some(f) if f.pts <= t + 1e-3 => {
                        // Due (or overdue) — display it, keep chasing the clock.
                        self.cur_pts = f.pts;
                        self.cur = Some(f.data);
                        self.tex_dirty = true;
                    }
                    Some(f) => {
                        st.pending = Some(f); // a future frame — hold it back
                        break;
                    }
                    None => break, // nothing available right now
                }
            }
        }

        // End-of-clip: stream exhausted and the clock has passed the last frame.
        if let Some(st) = self.stream.as_ref() {
            if self.playing && st.finished && st.pending.is_none() && t > self.cur_pts + 0.25 {
                self.playing = false;
                self.ended = true;
                if let Some(s) = self.sound.as_ref() {
                    s.pause();
                }
            }
        }
    }

    /// Toggle play/pause. Restarts from the beginning if the clip had ended.
    pub fn toggle(&mut self) {
        if self.ended {
            self.seek(0.0);
            self.set_playing(true);
        } else {
            self.set_playing(!self.playing);
        }
    }

    pub fn set_playing(&mut self, play: bool) {
        self.playing = play;
        self.ended = false;
        if let Some(s) = self.sound.as_ref() {
            if play {
                s.resume();
            } else {
                s.pause();
            }
        }
    }

    /// Seek to `secs`: respawn the frame stream there and re-cue the soundtrack, preserving the
    /// current play/pause state. The displayed frame updates on the next `tick`(s).
    pub fn seek(&mut self, secs: f32) {
        let t = secs.clamp(0.0, self.duration().max(0.0));
        self.ended = false;
        self.wall = t;
        // Drop the old stream (kills ffmpeg) and start a fresh one at the seek point.
        self.stream = FrameStream::spawn(&self.path, self.disp_w, self.disp_h, self.info.fps, t);
        if let Some(s) = self.sound.as_mut() {
            s.play_from(t, self.playing);
        }
    }

    /// Scrub preview: like `seek`, but **always** audible (plays the soundtrack from `t` even when
    /// paused) so dragging the playhead scrubs the audio, DAW-style. The caller should throttle
    /// this (each call respawns ffmpeg) — see `video_scrub_t` in the app. Playback state is left
    /// untouched; the final `seek` on release restores the paused/playing frame pacing.
    pub fn scrub_to(&mut self, secs: f32) {
        let t = secs.clamp(0.0, self.duration().max(0.0));
        self.ended = false;
        self.wall = t;
        self.stream = FrameStream::spawn(&self.path, self.disp_w, self.disp_h, self.info.fps, t);
        if let Some(s) = self.sound.as_mut() {
            s.play_from(t, true); // audible while scrubbing, regardless of play/pause
        }
    }

    /// Frame index at the current position (for the "frame N" readout). 0 if fps is unknown.
    pub fn frame_index(&self) -> u64 {
        (self.cur_pts.max(0.0) * self.info.fps.max(0.0)).round() as u64
    }

    /// Total frame count estimate (duration × fps). 0 if unknown.
    pub fn frame_count(&self) -> u64 {
        (self.duration() * self.info.fps.max(0.0)).round() as u64
    }

    /// The playhead position in clip seconds (clamped to the duration for the scrubber).
    pub fn position(&self) -> f32 {
        self.clock().clamp(0.0, self.duration().max(0.0))
    }

    /// Set playback speed (0.25×–4×). Re-cues the soundtrack at the new rate; the frame clock
    /// then chases it. Frames themselves are unchanged — only the clock scales.
    pub fn set_speed(&mut self, speed: f32) {
        let sp = speed.clamp(0.25, 4.0);
        self.speed = sp;
        if let Some(s) = self.sound.as_mut() {
            s.speed = sp;
            let at = s.pos();
            s.play_from(at, self.playing);
        }
    }

    /// Push master volume/mute from the app settings to the live soundtrack (no restart).
    pub fn set_volume(&mut self, volume: f32, muted: bool) {
        if let Some(s) = self.sound.as_mut() {
            s.volume = volume;
            s.muted = muted;
            s.apply_volume();
        }
    }

    pub fn has_audio(&self) -> bool {
        self.sound.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(w: u32, h: u32) -> VideoInfo {
        VideoInfo {
            width: w,
            height: h,
            duration: 10.0,
            fps: 30.0,
            vcodec: "h264".into(),
            has_audio: false,
        }
    }

    #[test]
    fn display_dims_caps_and_evens() {
        // 4K → capped to 1600 longest side, even dims, aspect preserved.
        let (w, h) = display_dims(&info(3840, 2160));
        assert_eq!(w, 1600);
        assert_eq!(h, 900);
        assert_eq!(w % 2, 0);
        assert_eq!(h % 2, 0);
        // Small video is not upscaled.
        assert_eq!(display_dims(&info(320, 240)), (320, 240));
        // Odd dims round down to even.
        let (w, h) = display_dims(&info(641, 481));
        assert_eq!(w % 2, 0);
        assert_eq!(h % 2, 0);
    }
}
