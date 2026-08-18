//! The overlay UI, owned exclusively by the main thread.
//!
//! The overlay is a borderless, transparent NSWindow above all normal windows.
//! Rendering happens by composing one NSImage per frame (bottom-left coords)
//! and setting it on an NSImageView. All keyboard handling lives in the global
//! event tap (see state.rs); this module handles mouse events via a local
//! event monitor and executes commands dispatched from background threads.
//! The local monitor only swallows mouse events while the overlay is visible;
//! otherwise it passes them through so other UI (e.g. the settings panel)
//! stays fully clickable.
//!
//! Selection is unified: keyboard nav, the scroll wheel and mouse hover all
//! move the same `Core::selection`, so the blue box follows the mouse just
//! like the keyboard (no animation — it jumps to the hovered thumbnail).

use crate::capture::ThumbCache;
use crate::config::Config;
use crate::layout;
use crate::state::{Core, Item, MainCmd, Mode, ShowData};
use crate::{ffi, util, windows};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{msg_send, AnyThread, ClassType, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSBezierPath, NSColor, NSCompositingOperation, NSEvent,
    NSEventMask, NSEventType, NSFont, NSGraphicsContext, NSImage, NSImageView, NSImageScaling,
    NSScreen, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSAttributedString, NSDictionary, NSObject, NSPoint, NSRect, NSString, NSSize};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Instant;

/// Main-thread-only UI state. The pointer fields are explicitly marked as
/// main-thread-owned; `with_app` is only called from the main run loop.
pub struct AppInner {
    shared: Arc<Mutex<Core>>,
    thumbs: Arc<RwLock<ThumbCache>>,
    cfg: Config,
    /// Retained NSWindow* (lazily created).
    panel: ffi::MainThreadPtr,
    /// Retained NSImageView* (content view of `panel`).
    image_view: ffi::MainThreadPtr,
    /// Retained local event monitor.
    _monitor: ffi::MainThreadPtr,
    /// window id -> (thumb gen, retained NSImage*).
    thumb_ns: HashMap<u32, (u64, ffi::MainThreadPtr)>,
    /// App pid that was frontmost right before the overlay opened.
    prev_app_pid: Option<i32>,
    /// Focused window id on the overlay's display (resolved by AX, with CG
    /// order as fallback), used to sync `active` for multi-window apps.
    front_window: Option<u32>,
    /// Last `windows::collect` result, reused by `quick_switch` for rapid A↔B
    /// toggling on the same display/mode.
    collect_cache: Option<CollectCache>,
    initialized: bool,
    warned_screen: bool,
    warned_ax: bool,
    scroll_acc: f64,
    last_redraw: Instant,
    /// When we last activated a window ourselves (quick-switch lag guard).
    last_activate: Option<Instant>,
    /// Monotonic token for delayed exact-window focus retries. A newer switch
    /// invalidates retries queued by an older one.
    activation_gen: u64,
    /// Coalesced thumbnail redraw pending (set by ThumbUpdated).
    thumb_dirty: bool,
    /// Last thumbnail-cache GC time.
    last_gc: Instant,
    /// Last desktop-change fingerprint of the overlay's display.
    last_fingerprint: Instant,
    /// Display the overlay is scoped to (CGDirectDisplayID of the display
    /// where the hotkey was pressed; 0 = not yet resolved).
    display_id: u32,
    /// Fingerprint of the display's visible windows (for desktop-change
    /// detection while the overlay is visible).
    last_window_ids: Vec<u32>,
    /// Transient hint drawn in the overlay (tag feedback / first-`p` cue).
    hint: Option<(String, Instant)>,
}

/// Cached result of a `windows::collect` call (see `COLLECT_CACHE_TTL`).
struct CollectCache {
    mode: Mode,
    display_id: u32,
    items: Vec<Item>,
    at: Instant,
}

static APP: OnceLock<Mutex<AppInner>> = OnceLock::new();

/// Lock-free mirrors of runtime-adjustable settings, readable from any
/// thread (e.g. the settings panel) WITHOUT taking the APP mutex (which is
/// not reentrant and may already be held by the command processor).
static SHARED: OnceLock<Arc<Mutex<Core>>> = OnceLock::new();
static CAPTURE_INTERVAL_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(crate::config::DEFAULT_CAPTURE_INTERVAL_SECS * 1000);
static QUICK_DELAY_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(crate::config::DEFAULT_QUICK_DELAY_MS);

/// After we activate a window ourselves, trust our stored pair (instead of the
/// frontmost query) for this long. `activateWithOptions` is asynchronous, so
/// `frontmost_pid` can lag briefly and would otherwise make a rapid second tap
/// a no-op.
const ACTIVATE_GRACE: std::time::Duration = std::time::Duration::from_millis(200);

/// How long a cached quick-switch window list stays reusable. Rapid A↔B
/// toggling re-collects (CGWindowList + sanitize) on every tap otherwise; within
/// this window the list is reused and only the cheap frontmost-app query runs.
const COLLECT_CACHE_TTL: std::time::Duration = std::time::Duration::from_millis(300);

/// How long tag feedback / hint text stays visible in the overlay.
const HINT_DURATION: std::time::Duration = std::time::Duration::from_secs(2);

/// How often to re-fingerprint the overlay display's visible windows to detect
/// a desktop change while the overlay is up. The old per-tick (150ms) check
/// re-enumerated + string-parsed the whole CG window list on every tick.
const FINGERPRINT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

pub fn init_app(cfg: Config, shared: Arc<Mutex<Core>>, thumbs: Arc<RwLock<ThumbCache>>) {
    {
        let ms = cfg.capture_interval.as_millis().max(200) as u64;
        let qd = cfg.quick_delay.as_millis().clamp(50, 2000) as u64;
        let mut core = shared.lock().unwrap();
        core.capture_interval_ms = ms;
        core.quick_delay_ms = qd;
    }
    CAPTURE_INTERVAL_MS.store(
        cfg.capture_interval.as_millis().max(200) as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
    QUICK_DELAY_MS.store(
        cfg.quick_delay.as_millis().clamp(50, 2000) as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
    let _ = SHARED.set(shared.clone());
    let _ = APP.set(Mutex::new(AppInner {
        shared,
        thumbs,
        cfg,
        panel: ffi::MainThreadPtr(std::ptr::null_mut()),
        image_view: ffi::MainThreadPtr(std::ptr::null_mut()),
        _monitor: ffi::MainThreadPtr(std::ptr::null_mut()),
        thumb_ns: HashMap::new(),
        prev_app_pid: None,
        front_window: None,
        collect_cache: None,
        initialized: false,
        warned_screen: false,
        warned_ax: false,
        scroll_acc: 0.0,
        last_redraw: Instant::now() - std::time::Duration::from_secs(1),
        last_activate: None,
        activation_gen: 0,
        thumb_dirty: false,
        last_gc: Instant::now(),
        last_fingerprint: Instant::now(),
        display_id: 0,
        last_window_ids: Vec::new(),
        hint: None,
    }));
}

/// Run `f` on the main thread with exclusive access to the overlay state.
fn with_app<R>(f: impl FnOnce(&mut AppInner) -> R) -> R {
    let app = APP.get().expect("winflow overlay not initialized");
    // A transient panic (e.g. bad selector during development) must not
    // permanently poison the lock and brick the app.
    let mut g = app.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}

pub fn handle_cmd(cmd: MainCmd) {
    with_app(|a| a.handle_cmd(cmd));
}

/// Quick-tap judgment delay in whole milliseconds (for the panel). Lock-free.
pub fn current_quick_delay() -> u64 {
    QUICK_DELAY_MS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Runtime-adjustable quick-tap judgment delay (ms), 50–2000. Applies
/// immediately (used by the event tap on the next hotkey press).
pub fn set_quick_delay(ms: u64) {
    let ms = ms.clamp(50, 2000);
    QUICK_DELAY_MS.store(ms, std::sync::atomic::Ordering::Relaxed);
    if let Some(shared) = SHARED.get() {
        if let Ok(mut core) = shared.lock() {
            core.quick_delay_ms = ms;
        }
    }
    util::log(&format!("quick-tap delay -> {}ms", ms));
}

/// Current thumbnail capture interval in whole seconds (for the panel).
/// Lock-free; safe to call from the main thread even while the command
/// processor holds the APP mutex.
pub fn current_capture_interval() -> u64 {
    CAPTURE_INTERVAL_MS.load(std::sync::atomic::Ordering::Relaxed) / 1000
}

/// Runtime-adjustable thumbnail capture interval (seconds). Called from the
/// settings panel on the main thread; applies immediately.
pub fn set_capture_interval(secs: u64) {
    let ms = secs.clamp(1, 3600) * 1000;
    CAPTURE_INTERVAL_MS.store(ms, std::sync::atomic::Ordering::Relaxed);
    if let Some(shared) = SHARED.get() {
        if let Ok(mut core) = shared.lock() {
            core.capture_interval_ms = ms;
        }
    }
    util::log(&format!("thumbnail capture interval -> {}s", ms / 1000));
}

/// MainThreadMarker for the main thread (all UI code runs there).
fn mtm() -> MainThreadMarker {
    unsafe { MainThreadMarker::new_unchecked() }
}

impl AppInner {
    fn handle_cmd(&mut self, cmd: MainCmd) {
        match cmd {
            MainCmd::Show(m, require_cmd_held) => self.show(m, require_cmd_held),
            MainCmd::ShowReady(d) => self.finish_show(d),
            MainCmd::Redraw => self.redraw(),
            MainCmd::Activate(i) => self.activate(i),
            MainCmd::Refocus(item, gen) => {
                if self.activation_gen == gen {
                    windows::refocus_item(&item);
                }
            }
            MainCmd::QuickSwitch(m) => self.quick_switch(m),
            MainCmd::Hide => self.hide(true),
            MainCmd::ThumbUpdated(id) => {
                if let Some((_, p)) = self.thumb_ns.remove(&id) {
                    unsafe { ffi::cf_release(p.get() as *const c_void) };
                }
                // Don't recompose here: a burst of thumbnails would trigger N
                // full redraws. Coalesce into the next tick.
                self.thumb_dirty = true;
            }
            MainCmd::ToggleTag(id) => self.toggle_tag(id),
            MainCmd::ActivateTag(n) => self.activate_tag(n),
            MainCmd::TagHint(s) => {
                self.set_hint(s);
                self.redraw();
            }
            MainCmd::OpenPanel => crate::menubar::open_panel_for_test(),
            MainCmd::PermDialog => crate::permissions::prompt_for_test(),
            MainCmd::Tick => self.tick(),
            MainCmd::Quit => {
                unsafe {
                    let app = NSApplication::sharedApplication(mtm());
                    let _: () = msg_send![&app, terminate: None::<&NSObject>];
                }
            }
        }
    }

    // ---------- show / hide ----------

    fn show(&mut self, mode: Mode, require_cmd_held: bool) {
        self.ensure_panel();
        self.warn_permissions();
        // A fresh overlay starts without a stale hint from a previous session.
        self.hint = None;
        // The overlay re-collects the window set below; drop any stale
        // quick-switch cache so a later quick tap re-enumerates fresh.
        self.collect_cache = None;
        // Scope the switcher to the display containing the currently focused
        // window. Mouse position is only a fallback when no focused window can
        // be resolved (e.g. Finder's bare desktop). This keeps keyboard intent
        // stable when the pointer drifts onto another monitor.
        let focus = windows::focus_snapshot();
        let front_pid = focus.map(|f| f.pid);
        let focused_window = focus.and_then(|f| f.window_id);
        self.display_id = focus
            .and_then(|f| f.display_id)
            .unwrap_or_else(|| unsafe { ffi::cursor_display() });
        let display_id = self.display_id;
        self.prev_app_pid = front_pid;
        {
            let mut core = self.shared.lock().unwrap();
            core.mode = mode;
            core.mru = match mode {
                Mode::Space => core.mru_space.clone(),
                Mode::App => core.mru_app.clone(),
            };
        }

        // The expensive part — CGWindowList enumeration + sanitize — runs on a
        // background thread so the main run loop isn't blocked while the
        // overlay is being prepared. The result comes back as `ShowReady`.
        // (Only CG/CoreFoundation is touched here; `front_pid` was computed on
        // the main thread because NSWorkspace is AppKit.)
        let cfg = self.cfg.clone();
        let prev_app_pid = self.prev_app_pid;
        let our_pid = std::process::id();
        std::thread::spawn(move || {
            let dbounds = unsafe { ffi::display_bounds(display_id) };
            let items = windows::collect(&cfg, mode, our_pid, dbounds.as_ref(), front_pid);
            let front_window = focused_window
                .filter(|id| items.iter().any(|i| i.id == *id))
                .or_else(|| {
                    prev_app_pid
                        .and_then(|p| items.iter().find(|i| i.pid == p).map(|i| i.id))
                });
            let last_window_ids = dbounds
                .map(windows::display_window_ids)
                .unwrap_or_default();
            crate::state::dispatch_main(MainCmd::ShowReady(ShowData {
                mode,
                display_id,
                items,
                front_window,
                last_window_ids,
                require_cmd_held,
            }));
        });
    }

    /// Finish showing the overlay once the background collect has returned.
    fn finish_show(&mut self, data: ShowData) {
        {
            let core = self.shared.lock().unwrap();
            // If this show required ⌘ to stay held and it's no longer held, a
            // release already dispatched QuickSwitch/Hide while the collect was
            // in flight — drop the result so the overlay doesn't pop up after
            // the user let go. Similarly drop a result whose mode no longer
            // matches (a newer show re-targeted the switcher meanwhile).
            if (data.require_cmd_held && !core.cmd_held) || core.mode != data.mode {
                return;
            }
        }
        self.display_id = data.display_id;
        self.front_window = data.front_window;
        self.last_window_ids = data.last_window_ids;
        {
            let mut core = self.shared.lock().unwrap();
            core.items = data.items;
            // Merge the current display's windows into the warm set instead of
            // replacing it (same logic as collect_items — see the note there
            // about not scoping `tracked` to one display).
            let mut ids: HashSet<u32> = core.tracked.iter().copied().collect();
            ids.extend(core.items.iter().map(|i| i.id));
            core.tracked = ids.into_iter().collect();
            core.refresh_all = true;
        }
        let sel_id = {
            let mut core = self.shared.lock().unwrap();
            // Sync with whatever is currently focused (external switches
            // count too): the frontmost window becomes the active one, the
            // previously active becomes `prev` (the first/selected item when
            // the overlay opens). `self.front_window` is the exact AX-focused
            // window captured before winflow takes focus, so multi-window apps
            // such as VSCode sync to the correct display/window.
            let f_id = self.front_window;
            if let Some(f) = f_id {
                if core.active != Some(f) {
                    // The window we are leaving becomes the toggle's previous
                    // half and the first/selected item next time the overlay
                    // opens.
                    core.prev = core.active;
                    core.active = Some(f);
                }
                touch_mru(&mut core.mru, f);
            }
            // Selection target: the window we switched FROM; fall back to the
            // second-most-recent MRU entry; finally to the active window.
            core.prev
                .filter(|id| core.items.iter().any(|i| i.id == *id))
                .or_else(|| {
                    core.mru
                        .get(1)
                        .filter(|id| core.items.iter().any(|i| i.id == **id))
                        .copied()
                })
                .or(core.active)
        };
        // Order: [prev] [active] [mru rest] [others by CG order].
        self.layout_items(sel_id);
        {
            let mut core = self.shared.lock().unwrap();
            core.selection = 0;
            core.hover = None;
            core.visible = true;
            // The overlay is up: end the quick-tap window (the overlay is
            // dismissed by releasing ⌘; see the event-tap flags handler).
            core.quick_pending = false;
            core.quick_show_dispatched = false;
        }
        self.redraw();
        unsafe {
            let win = &*(self.panel.get() as *const NSWindow);
            let _: () = msg_send![&*win, makeKeyAndOrderFront: None::<&NSObject>];
            let app = NSApplication::sharedApplication(mtm());
            let _: () = msg_send![&app, activateIgnoringOtherApps: true];
        }
    }

    /// Re-query windows and store them in `core.items` (raw CG order) plus
    /// bookkeeping (frontmost window, fingerprint, tracked, refresh flag).
    /// Ordering/layout is done separately by `layout_items`.
    fn collect_items(&mut self) {
        let mode = self.shared.lock().unwrap().mode;
        // Keep the display chosen when the overlay opened. The fallback path
        // is only for an uninitialized/dev-triggered recollect.
        let display = if self.display_id != 0 {
            self.display_id
        } else {
            windows::focus_snapshot()
                .and_then(|f| f.display_id)
                .unwrap_or_else(|| unsafe { ffi::cursor_display() })
        };
        self.display_id = display;
        let dbounds = unsafe { ffi::display_bounds(display) };
        let items = windows::collect(&self.cfg, mode, std::process::id(), dbounds.as_ref(), None);
        // The freshly collected list is in CG order (front-to-back); capture
        // the frontmost window of the front app BEFORE any MRU reorder.
        self.front_window = self
            .prev_app_pid
            .and_then(|p| items.iter().find(|i| i.pid == p).map(|i| i.id));
        // Remember the visible-window fingerprint for desktop-change detection.
        self.last_window_ids = dbounds
            .map(windows::display_window_ids)
            .unwrap_or_default();
        let mut core = self.shared.lock().unwrap();
        core.items = items;
        // Merge the current display's windows into the warm set instead of
        // replacing it. Replacing scoped `tracked` to one display, so the 1s
        // GC would evict other displays' thumbnails and the next show on a
        // different display had to re-capture everything (visible stutter).
        // The idle re-sync still rebuilds `tracked` from the global on-screen
        // set every ~10s, which prunes closed windows.
        let mut ids: HashSet<u32> = core.tracked.iter().copied().collect();
        ids.extend(core.items.iter().map(|i| i.id));
        core.tracked = ids.into_iter().collect();
        core.refresh_all = true;
    }

    /// Order `core.items` (selection/active/MRU) and rebuild layout rects.
    fn layout_items(&mut self, sel: Option<u32>) {
        let mut core = self.shared.lock().unwrap();
        let mru = core.mru.clone();
        let active = core.active;
        order_items(&mut core.items, &mru, sel, active);
        let (sw, sh) = self.screen_size();
        core.layout = Some(layout::compute(&core.items, sw, sh, &self.cfg));
        core.selection = core.selection.min(core.items.len().saturating_sub(1));
    }

    fn hide(&mut self, restore: bool) {
        let was_visible = {
            let mut core = self.shared.lock().unwrap();
            if !core.visible {
                return;
            }
            core.visible = false;
            core.hover = None;
            persist_mru(&mut core);
            true
        };
        if !was_visible {
            return;
        }
        self.scroll_acc = 0.0;
        if self.initialized {
            unsafe {
                let win = &*(self.panel.get() as *const NSWindow);
                let _: () = msg_send![&*win, orderOut: None::<&NSObject>];
            }
        }
        if restore {
            if let Some(pid) = self.prev_app_pid.take() {
                if let Some(app) = windows::running_app(pid) {
                    unsafe {
                        let _: bool = msg_send![&app, activateWithOptions: 2u64];
                    }
                }
            }
        } else {
            self.prev_app_pid = None;
        }
    }

    fn activate(&mut self, idx: usize) {
        let item = {
            let core = self.shared.lock().unwrap();
            core.items.get(idx).cloned()
        };
        let Some(item) = item else {
            self.hide(false);
            return;
        };
        self.activate_item(item);
    }

    /// Activate a window (by item) and update the two-window toggle state.
    fn activate_item(&mut self, item: Item) {
        {
            let mut core = self.shared.lock().unwrap();
            let active = core.active;
            // The window we are switching FROM becomes the toggle's previous
            // half. A no-op switch (target == current active) must not
            // overwrite `prev` with the active window itself, otherwise the
            // next quick tap would toggle to the same window.
            if active != Some(item.id) {
                core.prev = active;
                core.active = Some(item.id);
            }
            touch_mru(&mut core.mru, item.id);
            persist_mru(&mut core);
        }
        self.last_activate = Some(Instant::now());
        self.activation_gen = self.activation_gen.wrapping_add(1);
        let gen = self.activation_gen;
        windows::activate_item(&item);

        // VSCode/Electron may asynchronously restore its previously focused
        // window just after the app comes front, overwriting the exact AX target
        // (often with a window on another display). Reassert after activation
        // settles. The main-thread handler checks both `gen` and frontmost PID,
        // so retries cannot steal focus back after a newer/external switch.
        if item.n_same_pid > 1 && windows::ax_available() {
            let retry = item.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(50));
                crate::state::dispatch_main(MainCmd::Refocus(retry.clone(), gen));
                std::thread::sleep(std::time::Duration::from_millis(100));
                crate::state::dispatch_main(MainCmd::Refocus(retry, gen));
            });
        }
        self.hide(false);
    }

    /// Quick-tap: switch straight to the previously focused window, no overlay.
    ///
    /// Maintains a strict two-window toggle (`active` ↔ `prev`) that tracks the
    /// REAL currently focused window: external switches move the pair forward
    /// to `[newly focused, previous focused]`. A short grace period after our
    /// own activation guards against the asynchronous `activateWithOptions`
    /// (the frontmost query lags briefly and would otherwise be misread as an
    /// external switch).
    fn quick_switch(&mut self, mode: Mode) {
        let focus = windows::focus_snapshot();
        let front_pid = focus.map(|f| f.pid);
        let display = focus
            .and_then(|f| f.display_id)
            .unwrap_or_else(|| unsafe { ffi::cursor_display() });
        // Reuse a recent collect for the same display/mode so a rapid A↔B
        // toggle doesn't re-enumerate + sanitize the full CG window list on
        // every tap. Stale caches fall back to a fresh collect.
        let cache_hit = self.collect_cache.as_ref().is_some_and(|c| {
            c.mode == mode && c.display_id == display && c.at.elapsed() < COLLECT_CACHE_TTL
        });
        let items = if cache_hit {
            self.collect_cache.as_ref().unwrap().items.clone()
        } else {
            let dbounds = unsafe { ffi::display_bounds(display) };
            let items = windows::collect(
                &self.cfg,
                mode,
                std::process::id(),
                dbounds.as_ref(),
                front_pid,
            );
            self.collect_cache = Some(CollectCache {
                mode,
                display_id: display,
                items: items.clone(),
                at: Instant::now(),
            });
            items
        };
        if items.is_empty() {
            util::log("quick switch: no windows");
            return;
        }
        let present = |id: u32| items.iter().any(|i| i.id == id);

        let (active, prev) = {
            let core = self.shared.lock().unwrap();
            (core.active, core.prev)
        };
        let recent = self
            .last_activate
            .is_some_and(|t| t.elapsed() < ACTIVATE_GRACE);

        let front = focus
            .and_then(|f| f.window_id)
            .filter(|id| present(*id))
            .or_else(|| {
                front_pid.and_then(|p| items.iter().find(|i| i.pid == p).map(|i| i.id))
            })
            .or_else(|| items.first().map(|i| i.id));

        // Currently focused window: trust our own recent activation; otherwise
        // follow the real frontmost window so external switches are picked up.
        let current = if recent {
            active.filter(|a| present(*a)).or(front)
        } else {
            front.or(active)
        }
        .unwrap_or(items[0].id);

        // The window that was focused before `current`.
        let prev = if active != Some(current) { active } else { prev };

        // Record the pair and align mode/MRU before activating; `activate_item`
        // then flips it to (active = target, prev = current).
        {
            let mut core = self.shared.lock().unwrap();
            core.mode = mode;
            core.mru = match mode {
                Mode::Space => core.mru_space.clone(),
                Mode::App => core.mru_app.clone(),
            };
            core.active = Some(current);
            core.prev = prev;
            touch_mru(&mut core.mru, current);
        }

        let target_id = toggle_target(&items, current, prev);
        let Some(target_id) = target_id else {
            util::log("quick switch: no other window to switch to");
            return;
        };
        let Some(target) = items.iter().find(|i| i.id == target_id).cloned() else {
            return;
        };

        // Logging disabled for performance.
        // util::log(&format!(
        //     "quick switch -> [{}] {} (mode={:?} active={:?} front={:?} recent={} current={} prev={:?})",
        //     target.pid, target.owner, mode, active, front, recent, current, prev
        // ));
        self.activate_item(target);
    }

    // ---------- tags (quick entries) ----------

    /// Show a transient hint in the overlay (auto-expires after HINT_DURATION).
    fn set_hint(&mut self, text: String) {
        self.hint = Some((text, Instant::now() + HINT_DURATION));
    }

    /// Double-`p` on the selected window: add its quick-entry tag, or remove
    /// it when already tagged. Slots are never compacted; a new tag takes the
    /// lowest free slot (free = empty, or its window closed), and the limit
    /// (MAX_TAGS) shows a hint instead of adding.
    fn toggle_tag(&mut self, window_id: u32) {
        // Query liveness BEFORE taking the Core lock: the CGWindowList call is
        // comparatively slow, so keep the lock hold short.
        let alive = windows::window_ids_all();
        let mut core = self.shared.lock().unwrap();

        // Already tagged → remove (toggle).
        if let Some(slot) = core.tags.iter().position(|t| *t == Some(window_id)) {
            core.tags[slot] = None;
            let n = slot + 1;
            drop(core);
            self.set_hint(format!("已移除标签 {}", n));
            self.redraw();
            return;
        }

        match crate::state::free_tag_slot(&core.tags, |w| alive.contains(&w)) {
            Some(slot) => {
                core.tags[slot] = Some(window_id);
                let n = slot + 1;
                drop(core);
                self.set_hint(format!("已添加标签 {}（⌘⇧{} 快速切换）", n, n));
            }
            None => {
                drop(core);
                self.set_hint(format!("标签已达上限（最多 {} 个）", crate::state::MAX_TAGS));
            }
        }
        self.redraw();
    }

    /// `⌘⇧1/2/3`: switch straight to the tagged window (any display, current
    /// Spaces). Frees a slot when its window has closed; a window that merely
    /// lives on another Space is out of scope (CGWindowList is on-screen
    /// only) and gets a hint instead.
    fn activate_tag(&mut self, n: usize) {
        let id = {
            let core = self.shared.lock().unwrap();
            core.tags.get(n - 1).copied().flatten()
        };
        let Some(id) = id else {
            self.set_hint(format!("标签 {} 未设置（⌘⇧{}）", n, n));
            self.redraw();
            return;
        };
        // Global on-screen set across all displays (current Spaces).
        let items = windows::collect(&self.cfg, Mode::Space, std::process::id(), None, None);
        if let Some(item) = items.iter().find(|i| i.id == id).cloned() {
            self.activate_item(item);
            return;
        }
        // Not visible on any display: the window either closed or is parked
        // on another Space. Free the slot only when it is truly gone.
        let alive = windows::window_ids_all().contains(&id);
        if !alive {
            let mut core = self.shared.lock().unwrap();
            if core.tags.get(n - 1) == Some(&Some(id)) {
                core.tags[n - 1] = None;
            }
        }
        self.set_hint(if alive {
            format!("标签 {}：窗口不在当前桌面", n)
        } else {
            format!("标签 {}：窗口已关闭，已清除该标签", n)
        });
        self.redraw();
    }

    // ---------- rendering ----------

    fn redraw(&mut self) {
        let (items, layout, selection, visible) = {
            let core = self.shared.lock().unwrap();
            (
                core.items.clone(),
                core.layout.clone(),
                core.selection,
                core.visible,
            )
        };
        if !visible {
            return;
        }
        let Some(layout) = layout else { return };

        let (img, (w, h)) = self.compose(&items, &layout, selection);
        dump_image_once(&img, w, h);
        unsafe {
            let iv = &*(self.image_view.get() as *const NSImageView);
            let _: () = msg_send![&*iv, setImage: &*img];
            let win = &*(self.panel.get() as *const NSWindow);
            let cur: NSRect = msg_send![&*win, frame];
            let (fx, fy, fw, fh) = self.target_screen_frame();
            let x = fx + (fw - w) / 2.0;
            let y = fy + (fh - h) / 2.0;
            let want = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
            // Recenter whenever the size OR the on-screen position no longer
            // matches the target display. The size-only check let the panel
            // stay on the display of a previous show whenever two shows
            // produced the same layout size (equal-width displays + same row
            // count), so the overlay appeared on the startup desktop instead
            // of the focused window's display. (The panel is not user-draggable,
            // so recentering on every mismatch is always safe.)
            let size_changed =
                (cur.size.width - w).abs() > 0.5 || (cur.size.height - h).abs() > 0.5;
            let pos_changed = (cur.origin.x - x).abs() > 0.5 || (cur.origin.y - y).abs() > 0.5;
            if size_changed || pos_changed {
                let _: () = msg_send![&*win, setFrame: want, display: true];
            }
        }
    }

    /// Compose the whole grid into one NSImage (bottom-left coordinates).
    fn compose(
        &mut self,
        items: &[Item],
        layout: &layout::Layout,
        selection: usize,
    ) -> (Retained<NSImage>, (f64, f64)) {
        let w = layout.total_w;
        let h = layout.total_h;
        // Tag slots (window id -> tag number), copied out of the shared Core.
        let tags = self.shared.lock().unwrap().tags;

        // Build / refresh cached NSImage thumbnails (converted from CGImage).
        // Hold the thumbnail read lock once for the whole loop instead of
        // acquiring/releasing it per item (N items = 1 lock, not N).
        let mut ns_ptrs: Vec<Option<*const c_void>> = Vec::with_capacity(items.len());
        let thumbs = self.thumbs.read().unwrap();
        for it in items {
            let info = thumbs.get(&it.id).map(|th| (th.gen, th.image));
            match info {
                Some((gen, cg)) => {
                    let need = match self.thumb_ns.get(&it.id) {
                        Some((g, _)) => *g != gen,
                        None => true,
                    };
                    if need {
                        let ns = build_ns_image(cg.0);
                        if let Some(old) = self.thumb_ns.insert(it.id, (gen, ffi::MainThreadPtr(Retained::into_raw(ns) as *mut c_void))) {
                            unsafe { ffi::cf_release(old.1.get() as *const c_void) };
                        }
                    }
                    ns_ptrs.push(self.thumb_ns.get(&it.id).map(|(_, p)| p.get() as *const c_void));
                }
                None => ns_ptrs.push(None),
            }
        }

        let img: Retained<NSImage> =
            unsafe { msg_send![NSImage::alloc(), initWithSize: NSSize::new(w, h)] };
        unsafe {
            let _: () = msg_send![&*img, lockFocus];

            // High-quality interpolation when drawing thumbnails (avoids
            // softening when the layout shrinks a thumbnail below 1:1).
            let gc = NSGraphicsContext::currentContext();
            if let Some(g) = gc {
                let _: () = msg_send![&g, setImageInterpolation: 3u64]; // NSImageInterpolationHigh
            }

            // background
            let bg = NSColor::colorWithCalibratedWhite_alpha(0.13, 0.94);
            let _: () = msg_send![&bg, setFill];
            let full = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h));
            let path: Retained<NSBezierPath> = msg_send![
                NSBezierPath::class(),
                bezierPathWithRoundedRect: full,
                xRadius: 18.0,
                yRadius: 18.0
            ];
            let _: () = msg_send![&path, fill];

            if items.is_empty() {
                let t = util::ns_string("No windows here");
                let font = NSFont::boldSystemFontOfSize(14.0);
                let col = NSColor::colorWithCalibratedWhite_alpha(0.8, 1.0);
                let attrs = dict2(&*font_key(), &*font, &*color_key(), &*col);
                let at: Retained<NSAttributedString> = msg_send![
                    NSAttributedString::alloc(),
                    initWithString: &*t,
                    attributes: &*attrs
                ];
                let sz: NSSize = msg_send![&*at, size];
                let _: () = msg_send![
                    &*at,
                    drawAtPoint: NSPoint::new((w - sz.width) / 2.0, (h - sz.height) / 2.0)
                ];
            }

            for (i, item) in items.iter().enumerate() {
                let r = &layout.rects[i];
                let is_sel = i == selection;

                // One card per window: thumbnail on top, title below, treated
                // as a single unit (the selection ring wraps both).
                let card = NSRect::new(
                    NSPoint::new(r.thumb.x, r.label.y),
                    NSSize::new(r.thumb.w, r.thumb.h + r.label.h),
                );
                let card_path: Retained<NSBezierPath> = msg_send![
                    NSBezierPath::class(),
                    bezierPathWithRoundedRect: card,
                    xRadius: 12.0,
                    yRadius: 12.0
                ];

                // Card background (lighter than the overlay so each window pops).
                let card_bg = NSColor::colorWithCalibratedWhite_alpha(0.24, 0.95);
                let _: () = msg_send![&card_bg, setFill];
                let _: () = msg_send![&card_path, fill];

                // Thumbnail area: a dark "screen" well with the window image,
                // clipped to the card's rounded corners.
                let trect = NSRect::new(
                    NSPoint::new(r.thumb.x, r.thumb.y),
                    NSSize::new(r.thumb.w, r.thumb.h),
                );
                if let Some(g) = NSGraphicsContext::currentContext() {
                    let _: () = msg_send![&g, saveGraphicsState];
                }
                let _: () = msg_send![&card_path, addClip];

                let well = NSColor::colorWithCalibratedWhite_alpha(0.09, 0.9);
                let _: () = msg_send![&well, setFill];
                let _: () = msg_send![NSBezierPath::class(), fillRect: trect];

                // window thumbnail (aspect-fit)
                if let Some(p) = ns_ptrs[i] {
                    let ns = &*(p as *const NSImage);
                    let sz: NSSize = msg_send![ns, size];
                    if sz.width > 1.0 && sz.height > 1.0 {
                        let scale = (r.thumb.w / sz.width).min(r.thumb.h / sz.height);
                        let dw = sz.width * scale;
                        let dh = sz.height * scale;
                        let drect = NSRect::new(
                            NSPoint::new(
                                r.thumb.x + (r.thumb.w - dw) / 2.0,
                                r.thumb.y + (r.thumb.h - dh) / 2.0,
                            ),
                            NSSize::new(dw, dh),
                        );
                        let _: () = msg_send![
                            ns,
                            drawInRect: drect,
                            fromRect: NSRect::ZERO,
                            operation: NSCompositingOperation::SourceOver,
                            fraction: 1.0
                        ];
                    }
                }

                if let Some(g) = NSGraphicsContext::currentContext() {
                    let _: () = msg_send![&g, restoreGraphicsState];
                }

                // Subtle card outline.
                let border = NSColor::colorWithCalibratedWhite_alpha(0.55, 0.22);
                let _: () = msg_send![&border, setStroke];
                let _: () = msg_send![&card_path, setLineWidth: 1.0];
                let _: () = msg_send![&card_path, stroke];

                draw_label(item, r, is_sel);

                // Selection ring around the whole card (thumbnail + title).
                if is_sel {
                    let c = NSColor::colorWithSRGBRed_green_blue_alpha(0.30, 0.60, 1.0, 1.0);
                    let _: () = msg_send![&c, setStroke];
                    let sel_path: Retained<NSBezierPath> = msg_send![
                        NSBezierPath::class(),
                        bezierPathWithRoundedRect: card,
                        xRadius: 12.0,
                        yRadius: 12.0
                    ];
                    let _: () = msg_send![&sel_path, setLineWidth: 3.0];
                    let _: () = msg_send![&sel_path, stroke];
                }

                // Tag badge: a numbered amber pill at the thumbnail's top-right
                // corner. Purely a visual mark — tags never affect ordering,
                // layout or selection.
                if let Some(tag_n) = tags.iter().position(|t| *t == Some(item.id)).map(|i| i + 1) {
                    let bw = 20.0;
                    let bh = 18.0;
                    let bx = r.thumb.x + r.thumb.w - bw - 6.0;
                    let by = r.thumb.y + r.thumb.h - bh - 6.0;
                    let bpath: Retained<NSBezierPath> = msg_send![
                        NSBezierPath::class(),
                        bezierPathWithRoundedRect: NSRect::new(NSPoint::new(bx, by), NSSize::new(bw, bh)),
                        xRadius: 5.0,
                        yRadius: 5.0
                    ];
                    let bcol = NSColor::colorWithSRGBRed_green_blue_alpha(0.95, 0.62, 0.05, 0.95);
                    let _: () = msg_send![&bcol, setFill];
                    let _: () = msg_send![&bpath, fill];
                    let num = util::ns_string(&format!("{}", tag_n));
                    let font = NSFont::boldSystemFontOfSize(12.0);
                    let wcol = NSColor::whiteColor();
                    let attrs = dict2(&*font_key(), &*font, &*color_key(), &*wcol);
                    let at: Retained<NSAttributedString> = msg_send![
                        NSAttributedString::alloc(),
                        initWithString: &*num,
                        attributes: &*attrs
                    ];
                    let sz: NSSize = msg_send![&*at, size];
                    let _: () = msg_send![&*at, drawAtPoint: NSPoint::new(
                        bx + (bw - sz.width) / 2.0,
                        by + (bh - sz.height) / 2.0
                    )];
                }
            }

            // Transient hint (tag feedback / first-`p` cue) in the bottom
            // padding, auto-expired by tick.
            if let Some((text, until)) = &self.hint {
                if Instant::now() < *until {
                    let t = util::ns_string(text);
                    let font = NSFont::systemFontOfSize(12.0);
                    let col = NSColor::colorWithCalibratedWhite_alpha(0.95, 0.98);
                    let attrs = dict2(&*font_key(), &*font, &*color_key(), &*col);
                    let at: Retained<NSAttributedString> = msg_send![
                        NSAttributedString::alloc(),
                        initWithString: &*t,
                        attributes: &*attrs
                    ];
                    let sz: NSSize = msg_send![&*at, size];
                    let pw = sz.width + 22.0;
                    let ph = 18.0; // fits inside the 20pt bottom padding
                    let px = (w - pw) / 2.0;
                    let py = 2.0;
                    let pill: Retained<NSBezierPath> = msg_send![
                        NSBezierPath::class(),
                        bezierPathWithRoundedRect: NSRect::new(NSPoint::new(px, py), NSSize::new(pw, ph)),
                        xRadius: 9.0,
                        yRadius: 9.0
                    ];
                    let pcol = NSColor::colorWithCalibratedWhite_alpha(0.0, 0.45);
                    let _: () = msg_send![&pcol, setFill];
                    let _: () = msg_send![&pill, fill];
                    let _: () = msg_send![&*at, drawAtPoint: NSPoint::new(px + 11.0, py + (ph - sz.height) / 2.0)];
                }
            }

            let _: () = msg_send![&*img, unlockFocus];
        }
        (img, (w, h))
    }

    // ---------- mouse ----------

    fn handle_mouse(&mut self, event: &NSEvent) {
        let visible = self.shared.lock().unwrap().visible;
        if !visible {
            return;
        }
        match event.r#type() {
            NSEventType::LeftMouseDown | NSEventType::RightMouseDown => {
                let p = event.locationInWindow();
                let hit = {
                    let core = self.shared.lock().unwrap();
                    core.layout.as_ref().and_then(|l| l.hit(p.x, p.y))
                };
                match hit {
                    Some(i) => self.activate(i),
                    None => self.hide(true),
                }
            }
            NSEventType::MouseMoved => {
                let p = event.locationInWindow();
                let mut changed = false;
                {
                    let mut core = self.shared.lock().unwrap();
                    let hit = core.layout.as_ref().and_then(|l| l.hit(p.x, p.y));
                    if core.hover != hit {
                        core.hover = hit;
                        // Hovering a thumbnail moves the selection, exactly
                        // like keyboard navigation: the blue box follows the
                        // mouse (with the slide animation), and release/click
                        // acts on the hovered window.
                        if let Some(i) = hit {
                            core.selection = i;
                        }
                        changed = true;
                    }
                }
                if changed {
                    self.redraw();
                }
            }
            NSEventType::ScrollWheel => {
                self.scroll_acc += event.scrollingDeltaY();
                if self.scroll_acc.abs() >= 15.0 {
                    let forward = self.scroll_acc > 0.0;
                    {
                        let mut core = self.shared.lock().unwrap();
                        if forward {
                            core.nav_prev();
                        } else {
                            core.nav_next();
                        }
                    }
                    self.scroll_acc = 0.0;
                    self.redraw();
                }
            }
            _ => {}
        }
    }

    // ---------- periodic tick ----------

    fn tick(&mut self) {
        let visible = { self.shared.lock().unwrap().visible };
        // Expire transient hints so they don't linger forever while ⌘ is held
        // with no further interaction (redraw once to clear the drawn text).
        if let Some((_, until)) = &self.hint {
            if Instant::now() >= *until {
                self.hint = None;
                if visible {
                    self.redraw();
                }
            }
        }
        if visible {
            // NOTE: the overlay's visibility is tied to the ⌘ key being held
            // (the event tap hides it on release / explicit actions), so no
            // lost-focus dismissal here.

            // Desktop changed on the overlay's display: refresh the window
            // set. (Per-display space SPI is unavailable on modern macOS, so
            // we fingerprint the display's visible windows instead.) Throttled
            // to FINGERPRINT_INTERVAL — a desktop switch doesn't need per-tick
            // (150ms) re-enumeration of the full window list.
            if self.display_id != 0 && self.last_fingerprint.elapsed() >= FINGERPRINT_INTERVAL {
                self.last_fingerprint = Instant::now();
                if let Some(b) = unsafe { ffi::display_bounds(self.display_id) } {
                    let ids = windows::display_window_ids(b);
                    if ids != self.last_window_ids {
                        self.last_window_ids = ids;
                        self.collect_items();
                        self.layout_items(None);
                        self.thumb_dirty = false;
                        self.redraw();
                        return;
                    }
                }
            }
        }
        // Coalesced thumbnail redraw: many ThumbUpdated commands may arrive in
        // one burst (e.g. first show), so recompose once per tick instead of
        // once per thumbnail.
        if self.thumb_dirty {
            self.thumb_dirty = false;
            self.redraw();
        }
        // GC thumbnail caches that are no longer tracked, at most once a
        // second (building the tracked set every tick is wasteful).
        if self.last_gc.elapsed() >= std::time::Duration::from_secs(1) {
            self.last_gc = Instant::now();
            let tracked: HashSet<u32> = {
                let core = self.shared.lock().unwrap();
                core.tracked.iter().copied().collect()
            };
            {
                let mut t = self.thumbs.write().unwrap();
                t.retain(|id, th| {
                    let keep = tracked.contains(id);
                    if !keep {
                        unsafe { ffi::cf_release(th.image.0) };
                    }
                    keep
                });
            }
            self.thumb_ns.retain(|id, (_, p)| {
                let keep = tracked.contains(id);
                if !keep {
                    unsafe { ffi::cf_release(p.get() as *const c_void) };
                }
                keep
            });
        }
    }

    // ---------- setup ----------

    fn ensure_panel(&mut self) {
        if self.initialized {
            return;
        }
        unsafe {
            let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(200.0, 200.0));
            let win: Retained<NSWindow> = msg_send![
                NSWindow::alloc(mtm()),
                initWithContentRect: rect,
                styleMask: NSWindowStyleMask::Borderless,
                backing: NSBackingStoreType::Buffered,
                defer: false
            ];
            let clear = NSColor::clearColor();
            let _: () = msg_send![&win, setBackgroundColor: &*clear];
            let _: () = msg_send![&win, setOpaque: false];
            let _: () = msg_send![&win, setHasShadow: false];
            // Above normal windows + menus, below the screen saver.
            let _: () = msg_send![&win, setLevel: ffi::NSPOPUP_MENU_LEVEL];
            // Join all Spaces so the overlay is reachable everywhere.
            let _: () = msg_send![&win, setCollectionBehavior: ffi::NS_COLLECTION_BEHAVIOR];
            let _: () = msg_send![&win, setAcceptsMouseMovedEvents: true];
            let _: () = msg_send![&win, setReleasedWhenClosed: false];
            let _: () = msg_send![&win, setIgnoresMouseEvents: false];

            let iv: Retained<NSImageView> = msg_send![NSImageView::alloc(mtm()), initWithFrame: rect];
            let _: () = msg_send![&iv, setImageScaling: NSImageScaling::ScaleNone];
            let _: () = msg_send![&win, setContentView: &*iv];

            // Local monitor: mouse events only (keys are handled by the tap).
            // Only swallow events while the overlay is visible; otherwise pass
            // them through so other app UI (e.g. the settings panel) stays
            // fully clickable.
            let mask = NSEventMask::LeftMouseDown
                | NSEventMask::LeftMouseUp
                | NSEventMask::RightMouseDown
                | NSEventMask::RightMouseUp
                | NSEventMask::MouseMoved
                | NSEventMask::ScrollWheel;
            let shared = self.shared.clone();
            let blk: RcBlock<dyn Fn(std::ptr::NonNull<NSEvent>) -> *mut NSEvent> =
                RcBlock::new(move |event: std::ptr::NonNull<NSEvent>| -> *mut NSEvent {
                    let visible = shared.lock().unwrap().visible;
                    if !visible {
                        return event.as_ptr();
                    }
                    let e: &NSEvent = event.as_ref();
                    with_app(|a| a.handle_mouse(e));
                    std::ptr::null_mut() // swallow: the overlay handles the mouse
                });
            let mon: Option<Retained<AnyObject>> = msg_send![
                NSEvent::class(),
                addLocalMonitorForEventsMatchingMask: mask,
                handler: &*blk
            ];

            self.panel = ffi::MainThreadPtr(Retained::into_raw(win) as *mut c_void);
            self.image_view = ffi::MainThreadPtr(Retained::into_raw(iv) as *mut c_void);
            if let Some(m) = mon {
                self._monitor = ffi::MainThreadPtr(Retained::into_raw(m) as *mut c_void);
            }
            self.initialized = true;
        }
    }

    fn warn_permissions(&mut self) {
        // The startup dialog (permissions::prompt_if_missing) is the proactive
        // prompt; here we only re-log in case permissions were revoked later.
        if !self.warned_screen {
            self.warned_screen = true;
            if !unsafe { ffi::screen_capture_access() } {
                util::log("Screen Recording permission missing — thumbnails will be empty.");
            }
        }
        if !self.warned_ax {
            self.warned_ax = true;
            if !windows::ax_available() {
                util::log("Accessibility permission missing — ⌘Tab/⌘` interception disabled.");
            }
        }
    }

    /// Size of the display the overlay is scoped to (points).
    fn screen_size(&self) -> (f64, f64) {
        let (_, _, w, h) = self.target_screen_frame();
        (w, h)
    }

    /// AppKit frame of the target display (bottom-left origin). Falls back to
    /// the main screen when the display can't be resolved.
    fn target_screen_frame(&self) -> (f64, f64, f64, f64) {
        let scr = self.screen_for_display().or_else(|| NSScreen::mainScreen(mtm()));
        match scr {
            Some(s) => {
                let f: NSRect = unsafe { msg_send![&s, frame] };
                (f.origin.x, f.origin.y, f.size.width, f.size.height)
            }
            None => (0.0, 0.0, 1440.0, 900.0),
        }
    }

    /// Find the NSScreen whose display id matches `self.display_id`.
    fn screen_for_display(&self) -> Option<Retained<NSScreen>> {
        if self.display_id == 0 {
            return None;
        }
        unsafe {
            let screens = NSScreen::screens(mtm());
            for s in screens.iter() {
                let desc: Retained<NSDictionary<NSObject, NSObject>> =
                    msg_send![&s, deviceDescription];
                let key = util::ns_string("NSScreenNumber");
                let num: Option<Retained<NSObject>> = msg_send![&desc, objectForKey: &*key];
                if let Some(n) = num {
                    let v: u32 = msg_send![&n, unsignedIntValue];
                    if v == self.display_id {
                        return Some(Retained::clone(&s));
                    }
                }
            }
        }
        None
    }
}

// ---------- helpers ----------

fn persist_mru(core: &mut Core) {
    match core.mode {
        Mode::Space => core.mru_space = core.mru.clone(),
        Mode::App => core.mru_app = core.mru.clone(),
    }
}

/// Move `id` to the front of the MRU deque.
fn touch_mru(mru: &mut std::collections::VecDeque<u32>, id: u32) {
    if let Some(pos) = mru.iter().position(|x| *x == id) {
        mru.remove(pos);
    }
    mru.push_front(id);
}

/// Pick the window a quick tap should toggle to. Given the current window and
/// the stored previous window, return the previous window if it is still a
/// valid, different item; otherwise the first window in the list that is not
/// the current one (the user's default "previous").
fn toggle_target(items: &[Item], current: u32, prev: Option<u32>) -> Option<u32> {
    let present = |id: u32| items.iter().any(|i| i.id == id);
    prev.filter(|p| *p != current && present(*p))
        .or_else(|| items.iter().find(|i| i.id != current).map(|i| i.id))
}

/// Order items as: [sel] [active] [mru rest] [others by original (CG) order].
/// `sel` is the window that should be selected (0) when the overlay opens.
fn order_items(
    items: &mut [Item],
    mru: &std::collections::VecDeque<u32>,
    sel: Option<u32>,
    active: Option<u32>,
) {
    let mut rank: HashMap<u32, usize> = HashMap::new();
    let mut next = 0usize;
    if let Some(s) = sel {
        rank.insert(s, next);
        next += 1;
    }
    if let Some(a) = active {
        if Some(a) != sel {
            rank.insert(a, next);
            next += 1;
        }
    }
    for id in mru.iter() {
        if !rank.contains_key(id) {
            rank.insert(*id, next);
            next += 1;
        }
    }
    items.sort_by_key(|it| rank.get(&it.id).copied().unwrap_or(usize::MAX));
}

/// Dev aid: write the composed overlay to a PNG when WINFLOW_RENDER_OUT is set.
fn dump_image_once(img: &NSImage, w: f64, h: f64) {
    use std::sync::OnceLock;
    static DONE: OnceLock<bool> = OnceLock::new();
    let Some(path) = std::env::var("WINFLOW_RENDER_OUT").ok() else { return };
    if DONE.set(true).is_err() {
        return;
    }
    unsafe {
        // NSBitmapImageRep from the NSImage's TIFF representation.
        let tiff: Option<Retained<objc2_foundation::NSData>> =
            msg_send![img, TIFFRepresentation];
        let rep: Option<Retained<objc2_app_kit::NSBitmapImageRep>> =
            msg_send![objc2_app_kit::NSBitmapImageRep::class(), imageRepWithData: tiff.as_deref().unwrap()];
        let png: Option<Retained<objc2_foundation::NSData>> =
            msg_send![rep.as_deref().unwrap(), representationUsingType: 4u64, properties: None::<&NSDictionary<NSObject, NSObject>>];
        if let Some(p) = png {
            let ns = util::ns_string(&path);
            let ok: bool = msg_send![&p, writeToFile: &*ns, atomically: true];
            util::log(&format!("rendered overlay {}x{} -> {} ({})", w, h, path, ok));
        }
    }
}

/// Highest backing scale factor across all screens (>= 1.0). Thumbnails are
/// captured at this scale so they display 1:1 (crisp) instead of being
/// upscaled by the renderer.
pub fn screen_scale() -> f64 {
    static SCALE: OnceLock<f64> = OnceLock::new();
    *SCALE.get_or_init(|| {
        let mut best = 1.0f64;
        unsafe {
            let screens = NSScreen::screens(mtm());
            for s in screens.iter() {
                let f: f64 = msg_send![&s, backingScaleFactor];
                if f > best {
                    best = f;
                }
            }
        }
        best.max(1.0)
    })
}

fn build_ns_image(cg: ffi::CGImageRef) -> Retained<NSImage> {
    unsafe {
        let (pw, ph) = ffi::cg_image_size(cg);
        let scale = screen_scale();
        let sz = NSSize::new(pw as f64 / scale, ph as f64 / scale);
        // Borrowed reference: `initWithCGImage:size:` retains the CGImage
        // (verified empirically: retain count 2→3), and the Thumb cache keeps
        // its own +1. Never transfer ownership here — that caused a double
        // release (Thumb + snapshot rep both thought they owned the +1).
        let cg_ref = &*(cg as *const objc2_core_graphics::CGImage);
        objc2_app_kit::NSImage::initWithCGImage_size(NSImage::alloc(), cg_ref, sz)
    }
}

fn font_key() -> &'static NSString {
    static KEY: OnceLock<ffi::ProcessPtr> = OnceLock::new();
    let p = KEY.get_or_init(|| {
        ffi::ProcessPtr(Retained::into_raw(util::ns_string("NSFont")) as *const c_void)
    });
    unsafe { &*(p.get() as *const NSString) }
}

fn color_key() -> &'static NSString {
    static KEY: OnceLock<ffi::ProcessPtr> = OnceLock::new();
    let p = KEY.get_or_init(|| {
        ffi::ProcessPtr(Retained::into_raw(util::ns_string("NSColor")) as *const c_void)
    });
    unsafe { &*(p.get() as *const NSString) }
}

/// NSDictionary with two object/key pairs.
fn dict2(
    k1: &NSObject,
    v1: &NSObject,
    k2: &NSObject,
    v2: &NSObject,
) -> Retained<NSDictionary<NSObject, NSObject>> {
    unsafe {
        let keys = [
            k1 as *const NSObject as *const AnyObject,
            k2 as *const NSObject as *const AnyObject,
        ];
        let vals = [
            v1 as *const NSObject as *const AnyObject,
            v2 as *const NSObject as *const AnyObject,
        ];
        msg_send![
            NSDictionary::<NSObject, NSObject>::class(),
            dictionaryWithObjects: vals.as_ptr(),
            forKeys: keys.as_ptr(),
            count: 2usize
        ]
    }
}

fn draw_label(item: &Item, r: &layout::ItemRect, is_sel: bool) {
    unsafe {
        // Subtle divider between the thumbnail and the title.
        let div = NSColor::colorWithCalibratedWhite_alpha(0.9, 0.14);
        let _: () = msg_send![&div, setFill];
        let div_rect = NSRect::new(
            NSPoint::new(r.label.x + 8.0, r.label.y + r.label.h - 1.0),
            NSSize::new(r.label.w - 16.0, 1.0),
        );
        let _: () = msg_send![NSBezierPath::class(), fillRect: div_rect];

        let title = if item.title.is_empty() { &item.owner } else { &item.title };
        let font = NSFont::boldSystemFontOfSize(13.0);
        let col = NSColor::colorWithCalibratedWhite_alpha(if is_sel { 0.90 } else { 0.80 }, 1.0);
        // Right-prioritized ellipsis: keep the tail (most relevant) and drop
        // overflow from the left, so long titles never spill past the label.
        let max_w = (r.label.w - 16.0).max(0.0);
        let t = fit_label(title, max_w, &*font, &*col);
        let ns = util::ns_string(&t);
        let attrs = dict2(&*font_key(), &*font, &*color_key(), &*col);
        let at: Retained<NSAttributedString> = msg_send![
            NSAttributedString::alloc(),
            initWithString: &*ns,
            attributes: &*attrs
        ];
        let sz: NSSize = msg_send![&*at, size];
        let x = r.label.x + (r.label.w - sz.width) / 2.0;
        let y = r.label.y + (r.label.h - sz.height) / 2.0;
        let _: () = msg_send![&*at, drawAtPoint: NSPoint::new(x, y)];
    }
}

/// Fit `s` to at most `max_w` points, keeping the RIGHT-most characters and
/// ellipsizing the left ("…suffix"). Returns the original string when it fits.
fn fit_label(s: &str, max_w: f64, font: &NSFont, color: &NSColor) -> String {
    if text_width(s, font, color) <= max_w {
        return s.to_string();
    }
    let count = s.chars().count();
    // Binary search the largest tail that fits with the leading "…".
    let mut lo = 0usize;
    let mut hi = count;
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        if text_width(&format!("…{}", tail_chars(s, mid)), font, color) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        "…".to_string()
    } else {
        format!("…{}", tail_chars(s, lo))
    }
}

/// The last `n` chars of `s`.
fn tail_chars(s: &str, n: usize) -> String {
    s.chars().skip(s.chars().count().saturating_sub(n)).collect()
}

/// Rendered width of `s` with the given font/color (measured directly via
/// NSString `sizeWithAttributes:`, avoiding an NSAttributedString allocation
/// on every measurement — `fit_label` measures repeatedly during its binary
/// search).
fn text_width(s: &str, font: &NSFont, color: &NSColor) -> f64 {
    unsafe {
        let ns = util::ns_string(s);
        let attrs = dict2(&*font_key(), font, &*color_key(), color);
        let sz: NSSize = msg_send![&*ns, sizeWithAttributes: &*attrs];
        sz.width
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_set_get_roundtrip() {
        // Exercise the exact functions the settings-panel OK button calls.
        set_capture_interval(25);
        assert_eq!(current_capture_interval(), 25);
        set_capture_interval(1);
        assert_eq!(current_capture_interval(), 1);
        // out-of-range clamps
        set_capture_interval(0);
        assert_eq!(current_capture_interval(), 1);
        set_capture_interval(99999);
        assert_eq!(current_capture_interval(), 3600);
    }
}

#[cfg(test)]
mod order_tests {
    use super::*;
    use crate::state::Item;
    use std::collections::VecDeque;

    fn item(id: u32) -> Item {
        Item { id, pid: 1, owner: "o".into(), title: "t".into(), aspect: 1.0, x: 0.0, y: 0.0, w: 100.0, h: 100.0, n_same_pid: 1 }
    }

    fn ids(items: &[Item]) -> Vec<u32> {
        items.iter().map(|i| i.id).collect()
    }

    #[test]
    fn order_after_a_to_b() {
        // After switching A→B: prev=A(sel), active=B, mru=[B,A,C,D]
        // CG order was [C,A,B,D]; result must be [A,B,C,D] (A selected).
        let mut items = vec![item(3), item(1), item(2), item(4)];
        let mut mru = VecDeque::new();
        for id in [2, 1, 3, 4] {
            mru.push_back(id);
        }
        order_items(&mut items, &mru, Some(1), Some(2));
        assert_eq!(ids(&items), vec![1, 2, 3, 4]);
    }

    #[test]
    fn order_after_b_to_a_toggle() {
        // Toggle back B→A: prev=B(sel), active=A, mru=[A,B,C,D] → [B,A,C,D]
        let mut items = vec![item(1), item(2), item(3), item(4)];
        let mut mru = VecDeque::new();
        for id in [1, 2, 3, 4] {
            mru.push_back(id);
        }
        order_items(&mut items, &mru, Some(2), Some(1));
        assert_eq!(ids(&items), vec![2, 1, 3, 4]);
    }

    #[test]
    fn order_external_switch() {
        // Externally switched to D: prev=C(sel), active=D, mru=[D,C,A,B]
        let mut items = vec![item(1), item(2), item(3), item(4)];
        let mut mru = VecDeque::new();
        for id in [4, 3, 1, 2] {
            mru.push_back(id);
        }
        order_items(&mut items, &mru, Some(3), Some(4));
        assert_eq!(ids(&items), vec![3, 4, 1, 2]);
    }

    #[test]
    fn order_first_show() {
        // First ever show: sel=active=F → F first, rest by CG order.
        let mut items = vec![item(1), item(2), item(3)];
        order_items(&mut items, &VecDeque::new(), Some(1), Some(1));
        assert_eq!(ids(&items), vec![1, 2, 3]);
    }

    #[test]
    fn touch_mru_moves_to_front() {
        let mut mru: VecDeque<u32> = [1, 2, 3].into_iter().collect();
        touch_mru(&mut mru, 2);
        assert_eq!(mru.iter().copied().collect::<Vec<_>>(), vec![2, 1, 3]);
        touch_mru(&mut mru, 2); // already front: unchanged
        assert_eq!(mru.iter().copied().collect::<Vec<_>>(), vec![2, 1, 3]);
    }

    #[test]
    fn toggle_target_prefers_prev() {
        let items = vec![item(1), item(2), item(3)];
        assert_eq!(toggle_target(&items, 1, Some(2)), Some(2));
    }

    #[test]
    fn toggle_target_skips_prev_equal_current() {
        let items = vec![item(1), item(2), item(3)];
        // prev == current: fall back to the first other window.
        assert_eq!(toggle_target(&items, 1, Some(1)), Some(2));
    }

    #[test]
    fn toggle_target_defaults_to_first_other_window() {
        let items = vec![item(1), item(2), item(3)];
        assert_eq!(toggle_target(&items, 1, None), Some(2));
        // stale prev (not in items) is treated the same as no prev.
        assert_eq!(toggle_target(&items, 1, Some(99)), Some(2));
        // current is the first item: skip to the second.
        assert_eq!(toggle_target(&items, 2, None), Some(1));
    }

    #[test]
    fn toggle_target_none_for_single_window() {
        let items = vec![item(1)];
        assert_eq!(toggle_target(&items, 1, None), None);
    }
}
