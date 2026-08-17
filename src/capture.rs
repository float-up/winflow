//! Background thumbnail capture: a scheduler thread walks the tracked window
//! set on a timer, and a small pool of workers grab + downscale window images
//! into a shared cache. Results are pushed to the main thread as `ThumbUpdated`.

use crate::ffi;
use crate::state::{Core, MainCmd};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

pub struct Thumb {
    /// +1 CGImage (caller releases via cf_release).
    pub image: ffi::CgImage,
    pub at: Instant,
    /// Monotonic generation, used by the main thread to invalidate NSImage caches.
    pub gen: u64,
    /// False for a first (possibly partial) capture; true once a confirm shot
    /// has re-captured it after the window had time to settle.
    pub confirmed: bool,
}

pub type ThumbCache = HashMap<u32, Thumb>;

static GEN: AtomicU64 = AtomicU64::new(0);

/// How long a freshly captured window is given to settle before a one-time
/// confirm re-capture replaces the (possibly partial) first frame.
const SETTLE: Duration = Duration::from_millis(50);

struct Job {
    id: u32,
}

pub fn start(
    shared: Arc<Mutex<Core>>,
    thumbs: Arc<RwLock<ThumbCache>>,
    thumb_px_h: usize,
) {
    let (tx, rx) = std::sync::mpsc::channel::<Job>();
    let rx = Arc::new(Mutex::new(rx));
    for _ in 0..2 {
        let rx = rx.clone();
        let thumbs = thumbs.clone();
        std::thread::spawn(move || worker(rx, thumbs, thumb_px_h));
    }
    std::thread::spawn(move || {
        let mut last_stale_pass = Instant::now();
        let mut last_resync = Instant::now();
        loop {
            // Poll quickly so `refresh_all` (startup warm-up / overlay just
            // opened) is acted on almost immediately instead of waiting for
            // the next full capture-interval pass.
            std::thread::sleep(Duration::from_millis(50));
            let mut core = shared.lock().unwrap();
            // The interval is runtime-adjustable via the settings panel, so
            // read it from Core every cycle.
            let interval = Duration::from_millis(core.capture_interval_ms.max(200));

            // While the overlay is closed, keep `tracked` in sync with the
            // on-screen windows so thumbnails stay warm for the next show
            // (background capture runs continuously from launch). This is a
            // cheap id-only re-sync (no sanitize); exactness does not matter
            // because show() re-collects and re-warms anyway. Re-warm at most
            // every ~10s to keep the window-list query cheap.
            let resync = !core.visible && last_resync.elapsed() >= Duration::from_secs(10);

            // `refresh_all` (startup warm-up / overlay opened / desktop change)
            // triggers an immediate pass, but that pass captures only MISSING
            // or provisional thumbnails — never one that is merely stale.
            // Stale thumbnails are refreshed exclusively by the periodic
            // `stale_due` pass below, so waking the overlay never re-captures
            // an already-cached image (and fresh thumbnails are never touched
            // by either pass).
            let refresh_all = std::mem::take(&mut core.refresh_all);
            let stale_due = last_stale_pass.elapsed() >= interval;

            // A first frame can be partially drawn while a window is still
            // settling (launch/restore animation). Any unconfirmed thumbnail
            // that has had `SETTLE` to settle needs a confirm re-capture.
            // Hold the thumbnail read lock once for both the provisional scan
            // and the job-list build below (was: one lock per tracked id).
            let thumbs_r = thumbs.read().unwrap();
            let provisional_due = core.tracked.iter().any(|id| {
                thumbs_r
                    .get(id)
                    .is_some_and(|th| !th.confirmed && th.at.elapsed() >= SETTLE)
            });
            let capture = refresh_all || stale_due || provisional_due;

            if resync {
                last_resync = Instant::now();
                core.tracked = crate::windows::onscreen_window_ids(
                    &crate::config::Config::default(),
                    std::process::id(),
                );
            }

            let jobs = if capture {
                // Only a periodic stale pass advances the interval clock. A
                // wake/desktop-change pass must not, otherwise a busy user
                // (frequent ⌘Tab) could keep pushing the periodic refresh out
                // indefinitely and stale thumbnails would never refresh.
                if stale_due {
                    last_stale_pass = Instant::now();
                }
                let mut jobs: Vec<Job> = Vec::new();
                for id in core.tracked.iter() {
                    match thumbs_r.get(id) {
                        None => jobs.push(Job { id: *id }),
                        Some(th) => {
                            let provisional = !th.confirmed && th.at.elapsed() >= SETTLE;
                            let stale = stale_due && th.at.elapsed() > interval;
                            if provisional || stale {
                                jobs.push(Job { id: *id });
                            }
                        }
                    }
                }
                jobs
            } else {
                Vec::new()
            };
            drop(core);
            for j in jobs {
                let _ = tx.send(j);
            }
        }
    });
}

fn worker(rx: Arc<Mutex<std::sync::mpsc::Receiver<Job>>>, thumbs: Arc<RwLock<ThumbCache>>, thumb_px_h: usize) {
    loop {
        let job = rx.lock().unwrap().recv();
        let Ok(job) = job else { return };
        let img = unsafe { ffi::capture_window_image(job.id, thumb_px_h) };
        if img.is_null() {
            continue;
        }
        let gen = GEN.fetch_add(1, Ordering::Relaxed) + 1;
        let existed = {
            let t = thumbs.read().unwrap();
            t.contains_key(&job.id)
        };
        let old = {
            let mut t = thumbs.write().unwrap();
            // First capture of a window is provisional (may be partial); any
            // re-capture (confirm or stale refresh) is confirmed.
            t.insert(job.id, Thumb { image: ffi::CgImage(img), at: Instant::now(), gen, confirmed: existed })
        };
        if let Some(o) = old {
            unsafe { ffi::cf_release(o.image.0) };
        }
        crate::state::dispatch_main(MainCmd::ThumbUpdated(job.id));
    }
}
