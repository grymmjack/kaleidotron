//! A small worker pool that fetches 16colo.rs's pre-rendered thumbnail PNGs off the
//! UI thread. It mirrors [`crate::thumb::ThumbBuilder`] — a LIFO stack (so the most
//! recently scrolled-into-view piece downloads first), per-path dedup, and results
//! over an `mpsc` channel — but the job is an HTTPS GET + PNG decode instead of a
//! local-file decode. Results are keyed by the piece's *virtual* display path, so the
//! grid/table upload them into `thumb_tex` exactly like a locally-decoded thumbnail.

use crate::decode::Registry;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

pub struct RemoteThumbResult {
    pub path: PathBuf, // the piece's virtual display path (the cache key)
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>, // width * height * 4
}

struct Job {
    path: PathBuf,
    url: String,
    target: u32,
    // 16colo has no pre-rendered PNG for a PDF piece (its `tn`/`x1` render 404s), so
    // `url` is the *raw* PDF and we render page 1 ourselves via the registry (pdftoppm).
    via_registry: bool,
}

pub struct RemoteThumbs {
    queue: Arc<(Mutex<Vec<Job>>, Condvar)>,
    results: Receiver<RemoteThumbResult>,
    requested: HashSet<PathBuf>,
}

impl RemoteThumbs {
    pub fn new(workers: usize, registry: Arc<Registry>) -> Self {
        let queue: Arc<(Mutex<Vec<Job>>, Condvar)> =
            Arc::new((Mutex::new(Vec::new()), Condvar::new()));
        let (tx, rx): (Sender<RemoteThumbResult>, Receiver<RemoteThumbResult>) = channel();

        for _ in 0..workers.max(1) {
            let queue = Arc::clone(&queue);
            let tx = tx.clone();
            let registry = Arc::clone(&registry);
            std::thread::spawn(move || loop {
                let job = {
                    let (lock, cvar) = &*queue;
                    let mut q = lock.lock().unwrap();
                    while q.is_empty() {
                        q = cvar.wait(q).unwrap();
                    }
                    // LIFO: the most-recently-requested (visible) thumbnail first.
                    q.pop().unwrap()
                };
                if let Some(res) = fetch(&job, &registry) {
                    let _ = tx.send(res);
                }
            });
        }

        Self {
            queue,
            results: rx,
            requested: HashSet::new(),
        }
    }

    /// Enqueue once per path. Cheap to call every frame for visible rows. `via_registry` picks
    /// the decode path: `false` = fetch a pre-rendered raster (PNG/JPEG) at `url` and decode it
    /// with the `image` crate; `true` = `url` is a raw asset the `image` crate can't read (a PDF,
    /// an SVG icon) → decode it through the registry (SvgDecoder / pdfium-poppler) instead.
    pub fn request(&mut self, path: &Path, url: &str, target: u32, via_registry: bool) {
        if self.requested.insert(path.to_path_buf()) {
            let (lock, cvar) = &*self.queue;
            lock.lock().unwrap().push(Job {
                path: path.to_path_buf(),
                url: url.to_string(),
                target,
                via_registry,
            });
            cvar.notify_one();
        }
    }

    pub fn drain(&self) -> Vec<RemoteThumbResult> {
        self.results.try_iter().collect()
    }

    /// Forget that `path` was requested, so a later `request` re-decodes it (mirrors
    /// `ThumbBuilder::forget`). Used by Shift+F5's cache clear to re-fetch/re-render a
    /// 16colo piece's thumbnail from the persistent disk cache.
    pub fn forget(&mut self, path: &Path) {
        self.requested.remove(path);
    }
}

/// Download + decode one thumbnail, area-downscaling if it's bigger than `target`. A
/// PDF piece (`via_registry`) downloads its raw file and renders page 1 through the registry
/// (poppler `pdftoppm`, with a labeled placeholder fallback), since 16colo has no PDF
/// render; everything else fetches 16colo's pre-rendered PNG. Both go through the
/// persistent disk cache — re-browsing a pack/artist doesn't re-fetch.
fn fetch(job: &Job, registry: &Registry) -> Option<RemoteThumbResult> {
    let buf = crate::cache::get_bytes(&job.url, None).ok()?;
    if job.via_registry {
        let img = registry.decode_bytes(&buf, &job.path).ok()?;
        let (w, h, rgba) = crate::thumb::make_thumb(&img, job.target);
        return Some(RemoteThumbResult {
            path: job.path.clone(),
            width: w,
            height: h,
            rgba,
        });
    }
    let img = image::load_from_memory(&buf).ok()?.to_rgba8();
    let (sw, sh) = (img.width() as usize, img.height() as usize);
    if sw == 0 || sh == 0 {
        return None;
    }
    let rgba = img.into_raw();
    let target = job.target.max(1) as usize;
    // The `tn` previews are ~180px wide, usually already ≤ target; only downscale a
    // larger render. Box-average (not nearest) so a 50% dither isn't aliased to noise.
    let (w, h, rgba) = if sw.max(sh) > target {
        let scale = target as f32 / sw.max(sh) as f32;
        let dw = ((sw as f32 * scale).round() as usize).max(1);
        let dh = ((sh as f32 * scale).round() as usize).max(1);
        (dw, dh, crate::thumb::box_downscale(&rgba, sw, sh, dw, dh))
    } else {
        (sw, sh, rgba)
    };
    Some(RemoteThumbResult {
        path: job.path.clone(),
        width: w,
        height: h,
        rgba,
    })
}
