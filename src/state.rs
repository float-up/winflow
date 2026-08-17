//! Shared state (Core) plus the global event-tap / tick / dispatch plumbing.
//!
//! Threading model:
//! - `Core` is behind a Mutex, touched by the event-tap thread, capture threads
//!   and the main thread. It contains no AppKit objects, so it is Send.
//! - All UI work (panel, rendering, activation) happens on the main thread.
//!   Background threads only mutate `Core` and dispatch `MainCmd` to the main
//!   queue via libdispatch (`dispatch_async_f`).

use crate::ffi;
use crate::layout::{self, Dir};
use crate::util;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Every active window on the current Space (one entry per window).
    Space,
    /// Subset of Space: all windows of the frontmost app.
    App,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub id: u32,
    pub pid: i32,
    pub owner: String,
    pub title: String,
    /// width/height, clamped to a sane range for layout.
    pub aspect: f64,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Number of windows of the same app in the current (display-scoped) list.
    /// `1` lets `activate_item` skip the AX raise/focus round trips entirely.
    pub n_same_pid: usize,
}

/// Fixed hotkeys — they deliberately override the system shortcuts:
/// `⌘Tab` (space switcher) and `⌘\`` (current-app switcher). The HID-level
/// event tap swallows the keydowns so the system switcher never appears.
pub const HOTKEY_SPACE: Hotkey =
    Hotkey { mods: ffi::KCG_EVENT_FLAG_COMMAND, keycode: ffi::KVK_TAB };
pub const HOTKEY_APP: Hotkey =
    Hotkey { mods: ffi::KCG_EVENT_FLAG_COMMAND, keycode: ffi::KVK_ANSI_GRAVE };

/// Maximum number of quick-entry tags (`⌘⇧1` / `⌘⇧2` / `⌘⇧3`).
pub const MAX_TAGS: usize = 3;
/// Max gap between the two `p` presses that toggles a tag (double-tap).
pub const DOUBLE_P_MS: u64 = 400;

/// Lowest free tag slot (0-based, = tag number − 1): a slot is free when it
/// is empty or when its window is no longer alive (per `alive`). Returns
/// `None` when all slots are taken by live windows. Slots are NEVER
/// compacted — when a tagged window closes, tags 2/3 don't shift down to
/// fill the gap; a new tag simply takes the lowest free slot (so after tag 1
/// closes, the next tag added becomes 1 again).
pub fn free_tag_slot<F: Fn(u32) -> bool>(tags: &[Option<u32>; MAX_TAGS], alive: F) -> Option<usize> {
    tags.iter()
        .position(|t| match t {
            None => true,
            Some(id) => !alive(*id),
        })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hotkey {
    /// Required modifier mask (KCG_EVENT_FLAG_* bits).
    pub mods: u64,
    /// Keycode (HIToolbox virtual keycode).
    pub keycode: u16,
}

pub struct Core {
    pub visible: bool,
    pub mode: Mode,
    pub selection: usize,
    pub hover: Option<usize>,
    pub items: Vec<Item>,
    pub layout: Option<layout::Layout>,
    /// MRU for the active mode; front = most recently activated window.
    pub mru: VecDeque<u32>,
    pub mru_space: VecDeque<u32>,
    pub mru_app: VecDeque<u32>,
    /// Globally active window id (synced from the frontmost window on show,
    /// updated on every activation).
    pub active: Option<u32>,
    /// The window we are switching FROM (the "previous" half of the quick
    /// switch toggle; also used as the first/selected item when the overlay
    /// opens).
    pub prev: Option<u32>,
    /// Quick-tap state: hotkey pressed, overlay not shown yet.
    pub quick_pending: bool,
    /// Show already dispatched (overlay is loading); used to keep repeated
    /// hotkey presses from re-arming the quick window while it loads.
    pub quick_show_dispatched: bool,
    pub quick_deadline: std::time::Instant,
    pub quick_mode: Mode,
    /// Quick-tap judgment delay (ms), runtime-adjustable.
    pub quick_delay_ms: u64,
    /// Whether the hotkey modifier (⌘) is currently held. Written by the
    /// event-tap thread on every flags change; the quick-tap timer uses it so
    /// the overlay is only shown while ⌘ is still down.
    pub cmd_held: bool,
    /// Quick-entry tags (⌘⇧1/2/3). Index 0..2 = tag numbers 1..3; each slot
    /// holds the tagged window id or None when free. Never compacted: when a
    /// tagged window closes its slot stays empty (other tags don't shift),
    /// and a new tag takes the lowest free slot (see `free_tag_slot`).
    pub tags: [Option<u32>; MAX_TAGS],
    /// Window ids the capture loop should keep thumbnails for.
    pub tracked: Vec<u32>,
    /// Ask the capture loop to refresh missing/provisional thumbnails now
    /// (startup warm-up / overlay opened / desktop change). Stale thumbnails
    /// are refreshed only by the periodic `capture_interval` pass, so a wake
    /// never re-captures an already-cached image.
    pub refresh_all: bool,
    pub wrap: bool,
    /// Thumbnail capture interval in milliseconds (runtime-adjustable).
    pub capture_interval_ms: u64,
}

impl Core {
    pub fn new(wrap: bool) -> Self {
        Core {
            visible: false,
            mode: Mode::Space,
            selection: 0,
            hover: None,
            items: Vec::new(),
            layout: None,
            mru: VecDeque::new(),
            mru_space: VecDeque::new(),
            mru_app: VecDeque::new(),
            active: None,
            prev: None,
            quick_pending: false,
            quick_show_dispatched: false,
            quick_deadline: std::time::Instant::now(),
            quick_mode: Mode::Space,
            quick_delay_ms: 80,
            tracked: Vec::new(),
            refresh_all: false,
            wrap,
            capture_interval_ms: 45_000,
            cmd_held: false,
            tags: [None, None, None],
        }
    }

    /// Tag number (1..=MAX_TAGS) bound to `window_id`, if any.
    pub fn tag_of(&self, window_id: u32) -> Option<usize> {
        self.tags
            .iter()
            .position(|t| *t == Some(window_id))
            .map(|i| i + 1)
    }

    pub fn nav_next(&mut self) {
        if self.items.is_empty() {
            self.selection = 0;
            return;
        }
        self.selection = (self.selection + 1) % self.items.len();
    }

    pub fn nav_prev(&mut self) {
        if self.items.is_empty() {
            self.selection = 0;
            return;
        }
        self.selection = (self.selection + self.items.len() - 1) % self.items.len();
    }

    pub fn nav_dir(&mut self, dir: Dir) {
        if self.items.is_empty() {
            return;
        }
        if let Some(l) = &self.layout {
            self.selection = l.nav(self.selection, dir, self.wrap);
        } else {
            match dir {
                Dir::Next => self.nav_next(),
                Dir::Prev => self.nav_prev(),
                _ => {}
            }
        }
    }
}

/// Commands dispatched to the main thread.
pub enum MainCmd {
    Show(Mode),
    Redraw,
    Activate(usize),
    /// Quick-tap: switch straight to the previous window without the overlay.
    QuickSwitch(Mode),
    Hide,
    ThumbUpdated(u32),
    /// Double-`p` on the selected window: add/remove its tag (quick entry).
    ToggleTag(u32),
    /// `⌘⇧1/2/3`: switch straight to the tagged window (arg = tag number).
    ActivateTag(usize),
    /// Transient hint text shown in the overlay (tag feedback / cue).
    TagHint(String),
    OpenPanel,
    PermDialog,
    Tick,
    Quit,
}

// ---------- main-thread command delivery ----------
//
// Background threads push `MainCmd` into a mutex queue. A CFRunLoopTimer
// installed on the main run loop drains it; `dispatch_main` re-arms that timer
// to fire immediately (and wakes the run loop) so commands are processed with
// ~0 latency instead of waiting for the next periodic fire. The 100ms interval
// is only a backstop, so there are no extra idle wakeups.
// (A CFRunLoopSource was tried first, but on macOS 26 a version-0 source
// created via FFI spuriously fired at `CFRunLoopAddSource`; the timer is
// deterministic and costs ~nothing when the queue is empty.)

static CMD_QUEUE: std::sync::OnceLock<Mutex<Vec<MainCmd>>> = std::sync::OnceLock::new();
/// The main-thread drain timer, kept so `dispatch_main` can re-arm it to fire
/// immediately from background threads (see `dispatch_main`).
static CMD_TIMER: std::sync::OnceLock<ffi::RawPtr> = std::sync::OnceLock::new();

/// Must be called on the main thread before background threads start.
pub fn init_cmd_timer() {
    let ctx = ffi::CFRunLoopTimerContext {
        version: 0,
        info: std::ptr::null_mut(),
        retain: None,
        release: None,
        copy_description: None,
    };
    let timer = unsafe {
        ffi::CFRunLoopTimerCreate(
            std::ptr::null(),
            0.02, // fire soon after startup
            0.1,  // backstop interval: `dispatch_main` re-arms the timer to fire
                  // immediately on every push, so this only catches a missed
                  // wake; keeping it at 100ms means no extra idle wakeups.
            0,
            0,
            Some(process_cmds),
            &ctx,
        )
    };
    let _ = CMD_TIMER.set(ffi::RawPtr(timer));
    unsafe {
        ffi::CFRunLoopAddTimer(ffi::CFRunLoopGetMain(), timer, ffi::kCFRunLoopCommonModes);
    }
}

pub fn dispatch_main(cmd: MainCmd) {
    {
        let q = CMD_QUEUE.get_or_init(|| Mutex::new(Vec::new()));
        let mut q = q.lock().unwrap_or_else(|e| e.into_inner());
        q.push(cmd);
    }
    // Re-arm the drain timer to fire NOW and wake the main run loop, so
    // hotkey/activate/redraw commands are processed with ~0 latency instead of
    // waiting for the next scheduled 100ms fire. Both CF functions are
    // thread-safe; the timer only wakes when a command is actually queued.
    if let Some(t) = CMD_TIMER.get() {
        unsafe {
            ffi::CFRunLoopTimerSetNextFireDate(
                t.get() as ffi::CFRunLoopTimerRef,
                ffi::CFAbsoluteTimeGetCurrent(),
            );
            ffi::CFRunLoopWakeUp(ffi::CFRunLoopGetMain());
        }
    }
}

unsafe extern "C" fn process_cmds(_timer: ffi::CFRunLoopTimerRef, _info: *mut std::ffi::c_void) {
    // FFI callbacks must not unwind: catch panics (e.g. msg_send selector
    // validation) so a bug never aborts the whole process.
    let r = std::panic::catch_unwind(|| {
        let cmds = {
            let q = CMD_QUEUE.get_or_init(|| Mutex::new(Vec::new()));
            let mut q = q.lock().unwrap();
            std::mem::take(&mut *q)
        };
        for cmd in cmds {
            crate::overlay::handle_cmd(cmd);
        }
    });
    if let Err(e) = r {
        let msg = if let Some(s) = e.downcast_ref::<&str>() { (*s).to_string() } else if let Some(s) = e.downcast_ref::<String>() { s.clone() } else { "unknown panic".to_string() };
        crate::util::log(&format!("main-thread command panicked: {}", msg));
    }
}

// ---------- event tap ----------

struct TapCtx {
    shared: Arc<Mutex<Core>>,
    /// Currently held hotkey modifiers (alt|cmd|ctrl|shift).
    mods_down: u64,
    /// Timestamp of the last `p` keydown while the overlay was visible; a
    /// second `p` within `DOUBLE_P_MS` toggles a tag on the selected window.
    last_p_press: Option<std::time::Instant>,
}

pub fn start_tap(shared: Arc<Mutex<Core>>) {
    let ctx = Box::into_raw(Box::new(TapCtx { shared, mods_down: 0, last_p_press: None }));
    let mask = (1u64 << ffi::KCG_EVENT_KEY_DOWN)
        | (1u64 << ffi::KCG_EVENT_KEY_UP)
        | (1u64 << ffi::KCG_EVENT_FLAGS_CHANGED);
    let port = unsafe {
        ffi::CGEventTapCreate(
            ffi::KCG_HID_EVENT_TAP,
            ffi::KCG_HEAD_INSERT_EVENT_TAP,
            ffi::KCG_EVENT_TAP_OPTION_DEFAULT,
            mask,
            tap_callback,
            ctx as *mut std::ffi::c_void,
        )
    };
    if port.is_null() {
        util::log("event tap could not be created (accessibility permission?)");
        return;
    }
    let source = unsafe { ffi::CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0) };
    let common = ffi::RawPtr(unsafe { ffi::kCFRunLoopCommonModes });
    let port = ffi::RawPtr(port as *const std::ffi::c_void);
    let source = ffi::RawPtr(source as *const std::ffi::c_void);
    std::thread::spawn(move || unsafe {
        ffi::CFRunLoopAddSource(
            ffi::CFRunLoopGetCurrent(),
            source.get() as ffi::CFRunLoopSourceRef,
            common.get(),
        );
        ffi::CGEventTapEnable(port.get() as ffi::CFMachPortRef, true);
        ffi::CFRunLoopRun();
    });
}

unsafe extern "C" fn tap_callback(
    _proxy: ffi::CGEventTapProxy,
    event_type: ffi::CGEventType,
    event: ffi::CGEventRef,
    refcon: *mut std::ffi::c_void,
) -> ffi::CGEventRef {
    let ctx = &mut *(refcon as *mut TapCtx);
    let keycode = ffi::CGEventGetIntegerValueField(event, ffi::KCG_KEYBOARD_EVENT_KEYCODE) as u16;
    let flags = ffi::CGEventGetFlags(event);
    // FFI callbacks must not unwind: a panic (e.g. a poisoned Core mutex or a
    // handler bug) would otherwise abort the whole process.
    let swallow = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ctx.handle(event_type, keycode, flags)
    })) {
        Ok(swallow) => swallow,
        Err(e) => {
            let msg = if let Some(s) = e.downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = e.downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown panic".to_string()
            };
            util::log(&format!("event tap panicked: {}", msg));
            false // never swallow an event we failed to process
        }
    };
    if swallow {
        std::ptr::null_mut()
    } else {
        event
    }
}

const MOD_MASK: u64 = ffi::KCG_EVENT_FLAG_ALTERNATE
    | ffi::KCG_EVENT_FLAG_COMMAND
    | ffi::KCG_EVENT_FLAG_CONTROL
    | ffi::KCG_EVENT_FLAG_SHIFT;

impl TapCtx {
    fn handle(&mut self, event_type: u32, keycode: u16, flags: u64) -> bool {
        match event_type {
            ffi::KCG_EVENT_FLAGS_CHANGED => {
                let now = flags & MOD_MASK;
                // Both hotkeys use ⌘ as their only modifier, so a plain ⌘
                // held/released check is enough to drive show/hide/quick-switch.
                let cmd = ffi::KCG_EVENT_FLAG_COMMAND;
                let prev_had = (self.mods_down & cmd) == cmd;
                let now_had = (now & cmd) == cmd;
                self.mods_down = now;
                let mut core = self.shared.lock().unwrap_or_else(|e| e.into_inner());
                // The overlay's visibility tracks the hotkey modifier (⌘): it
                // is shown only while ⌘ is held, and released on ⌘ release.
                core.cmd_held = now & ffi::KCG_EVENT_FLAG_COMMAND != 0;
                if prev_had && !now_had {
                    if core.quick_pending {
                        // Released with the overlay not yet visible: always
                        // toggle straight to the previous window. This covers
                        // both a true quick tap AND a slightly slow tap (or a
                        // release right as the overlay is still loading) —
                        // without it, taps near the threshold occasionally did
                        // nothing because the overlay had not shown yet.
                        core.quick_pending = false;
                        dispatch_main(MainCmd::QuickSwitch(core.quick_mode));
                    } else if core.visible {
                        // Releasing ⌘ switches to the window the selection box
                        // is on (no explicit Enter/click needed) and closes the
                        // overlay — the classic hold-to-preview, release-to-switch.
                        let sel = core.selection;
                        dispatch_main(MainCmd::Activate(sel));
                    } else {
                        // Overlay not visible: cancel any Show still in the
                        // command queue (e.g. ⌘⇧Tab released within ~100ms).
                        dispatch_main(MainCmd::Hide);
                    }
                }
                false // never swallow modifier up/down
            }
            ffi::KCG_EVENT_KEY_DOWN => self.on_key_down(keycode, flags),
            _ => false,
        }
    }

    fn on_key_down(&mut self, keycode: u16, flags: u64) -> bool {
        let mods = flags & MOD_MASK;
        let h1 = HOTKEY_SPACE;
        let h2 = HOTKEY_APP;
        let mut core = self.shared.lock().unwrap_or_else(|e| e.into_inner());

        // ⌘Shift+Tab: open the space switcher backwards like the system.
        if !core.visible
            && keycode == ffi::KVK_TAB
            && mods == (ffi::KCG_EVENT_FLAG_COMMAND | ffi::KCG_EVENT_FLAG_SHIFT)
        {
            dispatch_main(MainCmd::Show(Mode::Space));
            return true;
        }

        // Mode-1 hotkey (⌘Tab): opens space switcher; repeats cycle.
        if mods == h1.mods && keycode == h1.keycode {
            if core.visible && core.mode == Mode::Space {
                core.nav_next();
                dispatch_main(MainCmd::Redraw);
            } else {
                arm_quick(&mut core, Mode::Space);
            }
            return true;
        }
        // Mode-2 hotkey (⌘`): opens app-window switcher.
        if mods == h2.mods && keycode == h2.keycode {
            if core.visible && core.mode == Mode::App {
                core.nav_next();
                dispatch_main(MainCmd::Redraw);
            } else {
                arm_quick(&mut core, Mode::App);
            }
            return true;
        }

        // Tag quick-switch hotkeys ⌘⇧1/2/3: jump straight to a tagged window
        // from anywhere (overlay open or not). CGEvent flags don't distinguish
        // left/right Shift, so either shift key works.
        if mods == (ffi::KCG_EVENT_FLAG_COMMAND | ffi::KCG_EVENT_FLAG_SHIFT)
            && matches!(keycode, ffi::KVK_ANSI_1 | ffi::KVK_ANSI_2 | ffi::KVK_ANSI_3)
        {
            let n = match keycode {
                ffi::KVK_ANSI_1 => 1,
                ffi::KVK_ANSI_2 => 2,
                _ => 3,
            };
            // A tag hotkey is an explicit switch request: cancel any pending
            // quick-tap judgment (armed by a prior ⌘Tab/⌘` press) so neither
            // the overlay nor a follow-up QuickSwitch fires on ⌘ release.
            core.quick_pending = false;
            core.quick_show_dispatched = false;
            dispatch_main(MainCmd::ActivateTag(n));
            return true;
        }

        if core.visible {
            let mut changed = false;
            match keycode {
                ffi::KVK_TAB => {
                    if mods & ffi::KCG_EVENT_FLAG_SHIFT != 0 {
                        core.nav_prev();
                    } else {
                        core.nav_next();
                    }
                    changed = true;
                }
                ffi::KVK_ESCAPE => {
                    dispatch_main(MainCmd::Hide);
                    return true;
                }
                ffi::KVK_RETURN => {
                    let sel = core.selection;
                    dispatch_main(MainCmd::Activate(sel));
                    return true;
                }
                ffi::KVK_ANSI_H | ffi::KVK_LEFT_ARROW => {
                    core.nav_dir(Dir::Left);
                    changed = true;
                }
                ffi::KVK_ANSI_L | ffi::KVK_RIGHT_ARROW => {
                    core.nav_dir(Dir::Right);
                    changed = true;
                }
                ffi::KVK_ANSI_K | ffi::KVK_UP_ARROW => {
                    core.nav_dir(Dir::Up);
                    changed = true;
                }
                ffi::KVK_ANSI_J | ffi::KVK_DOWN_ARROW => {
                    core.nav_dir(Dir::Down);
                    changed = true;
                }
                // Double-`p` toggles a quick-entry tag on the selected window.
                // First press only arms + shows a hint; the second press
                // (within DOUBLE_P_MS) dispatches the toggle. Both presses are
                // swallowed like every other key while the overlay is up.
                ffi::KVK_ANSI_P => {
                    let now = std::time::Instant::now();
                    let is_double = self
                        .last_p_press
                        .is_some_and(|t| now.duration_since(t) <= std::time::Duration::from_millis(DOUBLE_P_MS));
                    if is_double {
                        self.last_p_press = None;
                        if let Some(it) = core.items.get(core.selection) {
                            dispatch_main(MainCmd::ToggleTag(it.id));
                        }
                    } else {
                        self.last_p_press = Some(now);
                        dispatch_main(MainCmd::TagHint(
                            format!("再按一次 P 添加/移除标签（⌘⇧1-{} 快速切换）", MAX_TAGS),
                        ));
                    }
                }
                _ => {}
            }
            if changed {
                dispatch_main(MainCmd::Redraw);
            }
            // Swallow every key while the overlay is up so it never reaches
            // the application underneath.
            return true;
        }

        false
    }
}

/// Arm the quick-tap judgment window: the overlay is NOT shown immediately;
/// a timer thread shows it after `quick_delay_ms` if the hotkey modifier is
/// still held, and a fast release instead triggers `QuickSwitch`.
fn arm_quick(core: &mut Core, mode: Mode) {
    if core.quick_pending && core.quick_mode == mode {
        return; // repeated ⌘Tab keydowns must not extend the deadline
    }
    if core.quick_show_dispatched {
        return; // overlay is already loading; do not re-arm
    }
    core.quick_pending = true;
    core.quick_mode = mode;
    core.quick_deadline = std::time::Instant::now() + std::time::Duration::from_millis(core.quick_delay_ms);
}

/// Background poller (one thread) that shows the switcher once the quick-tap
/// delay elapses while the hotkey modifier (⌘) is still held.
pub fn start_quick_timer(shared: Arc<Mutex<Core>>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut core = shared.lock().unwrap_or_else(|e| e.into_inner());
        if core.quick_pending
            && !core.quick_show_dispatched
            && core.cmd_held
            && std::time::Instant::now() >= core.quick_deadline
        {
            core.quick_show_dispatched = true;
            let mode = core.quick_mode;
            // `quick_pending` stays set until Show() actually processes, so
            // repeated ⌘Tab keydowns while the overlay loads do not re-arm.
            dispatch_main(MainCmd::Show(mode));
        }
    });
}

// ---------- periodic tick ----------

pub fn start_tick() {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(150));
        dispatch_main(MainCmd::Tick);
    });
}

#[cfg(test)]
mod tag_tests {
    use super::*;

    #[test]
    fn tags_fill_in_order() {
        // First three adds take slots 1, 2, 3; the 4th is refused.
        // (`alive` returns true when the tagged window is still alive.)
        let mut tags = [None, None, None];
        assert_eq!(free_tag_slot(&tags, |_| true), Some(0));
        tags[0] = Some(1);
        assert_eq!(free_tag_slot(&tags, |_| true), Some(1));
        tags[1] = Some(2);
        assert_eq!(free_tag_slot(&tags, |_| true), Some(2));
        tags[2] = Some(3);
        assert_eq!(free_tag_slot(&tags, |_| true), None);
    }

    #[test]
    fn closed_window_frees_lowest_slot_without_compaction() {
        // Tag 1's window (id 1) closed: slot 0 becomes free even though
        // slots 1 and 2 still hold live windows — tags 2/3 do NOT shift.
        let tags = [Some(1), Some(2), Some(3)];
        assert_eq!(free_tag_slot(&tags, |id| id != 1), Some(0));
        // Tag 2's window closed instead: lowest free slot is 1.
        let tags = [Some(1), Some(2), Some(3)];
        assert_eq!(free_tag_slot(&tags, |id| id != 2), Some(1));
    }

    #[test]
    fn all_slots_live_means_full() {
        let tags = [Some(1), Some(2), Some(3)];
        assert_eq!(free_tag_slot(&tags, |_| true), None);
    }

    #[test]
    fn tag_of_returns_one_based_number() {
        let mut core = Core::new(true);
        assert_eq!(core.tag_of(7), None);
        core.tags[0] = Some(7);
        core.tags[2] = Some(9);
        assert_eq!(core.tag_of(7), Some(1));
        assert_eq!(core.tag_of(9), Some(3));
        assert_eq!(core.tag_of(42), None);
    }
}
