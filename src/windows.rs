//! Window discovery, filtering, per-app representative selection,
//! Accessibility-based raise/focus and app activation.

use crate::config::Config;
use crate::ffi;
use crate::state::{Item, Mode};
use crate::util;
use objc2::rc::Retained;
use objc2::{msg_send, ClassType};
use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct Win {
    pub id: u32,
    pub pid: i32,
    pub owner: String,
    pub title: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub layer: i32,
    pub alpha: f64,
    pub onscreen: bool,
}

/// Owner names that are never real windows worth switching to.
const NOISE_OWNERS: &[&str] = &[
    "Window Server",
    "Dock",
    "Control Center",
    "Notification Center",
    "Spotlight",
    "Siri",
    "TextInputMenuAgent",
    "SystemUIServer",
    "loginwindow",
];

/// Owner names whose windows are only watermarks / overlays.
fn is_noise_owner(owner: &str) -> bool {
    NOISE_OWNERS.iter().any(|n| owner.eq_ignore_ascii_case(n))
}

/// Fetch and parse the on-screen window list (front-to-back order).
fn all_windows() -> Vec<Win> {
    let arr = unsafe { ffi::cg_window_list() };
    if arr.is_null() {
        return Vec::new();
    }
    let n = unsafe { ffi::cf_array_count(arr) };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let d = unsafe { ffi::cf_array_get(arr, i) };
        if d.is_null() {
            continue;
        }
        let d = d as ffi::CFDictionaryRef;
        let bounds = unsafe { ffi::cf_dict_bounds(d) }.unwrap_or((0.0, 0.0, 0.0, 0.0));
        let onscreen = unsafe { ffi::cf_dict_bool(d, ffi::kCGWindowIsOnscreen) }.unwrap_or(false);
        out.push(Win {
            id: unsafe { ffi::cf_dict_num_i32(d, ffi::kCGWindowNumber) }.unwrap_or(0) as u32,
            pid: unsafe { ffi::cf_dict_num_i32(d, ffi::kCGWindowOwnerPID) }.unwrap_or(-1),
            owner: unsafe { ffi::cf_dict_str(d, ffi::kCGWindowOwnerName) }.unwrap_or_default(),
            title: unsafe { ffi::cf_dict_str(d, ffi::kCGWindowName) }.unwrap_or_default(),
            x: bounds.0,
            y: bounds.1,
            w: bounds.2,
            h: bounds.3,
            layer: unsafe { ffi::cf_dict_num_i32(d, ffi::kCGWindowLayer) }.unwrap_or(0),
            alpha: unsafe { ffi::cf_dict_num_double(d, ffi::kCGWindowAlpha) }.unwrap_or(1.0),
            onscreen,
        });
    }
    unsafe { ffi::cf_release(arr) };
    out
}

/// Cheap O(n) sanity filter shared by `sanitize` and the capture scheduler's
/// idle re-sync (`onscreen_window_ids`): a real window is layer-0, on-screen,
/// fully opaque, big enough to matter, and neither a known system/UI owner nor
/// a configured filter term. The expensive overlay heuristics (duplicate-bounds
/// dedup, contained-watermark removal, per-app main-window guarantee) live in
/// `sanitize` only.
fn basic_ok(w: &Win, cfg: &Config) -> bool {
    w.layer == 0
        && w.onscreen
        && w.alpha >= 0.95
        && w.w >= cfg.min_window_w
        && w.h >= cfg.min_window_h
        && !is_noise_owner(&w.owner)
        && !cfg
            .filter_terms
            .iter()
            .any(|t| w.owner.contains(t) || w.title.contains(t))
}

/// Filter out anomalous windows (watermark layers, duplicates, tiny helpers).
/// All surviving windows are kept — the switcher shows every active window on
/// the current Space, one entry per window (not one per app).
///
/// Guarantee: an app that has at least one window passing the basic checks
/// always keeps ONE main window in the list, even if every one of its windows
/// would otherwise be dropped as a suspicious overlay (e.g. Feishu keeps its
/// titled fullscreen main window instead of vanishing entirely).
fn sanitize(wins: &mut Vec<Win>, cfg: &Config) {
    wins.retain(|w| basic_ok(w, cfg));

    // Remember each app's main window candidate (from the basic-filtered
    // list): the frontmost titled window; fall back to the frontmost window.
    let mut mains: HashMap<i32, Win> = HashMap::new();
    for w in wins.iter() {
        let cur = mains.entry(w.pid).or_insert_with(|| w.clone());
        if cur.title.is_empty() && !w.title.is_empty() {
            *cur = w.clone();
        }
    }

    // Dedupe exact duplicate bounds within the same app. Two windows with
    // identical bounds are only duplicates when at least one is an UNTITLED
    // overlay layer (e.g. Feishu's fullscreen main window + its untitled
    // child). Two genuinely titled windows with the same bounds (e.g. two
    // maximized VSCode windows on one display) are both real — keep both.
    let mut seen: HashMap<(i32, i64, i64, i64, i64), usize> = HashMap::new();
    let mut keep = vec![true; wins.len()];
    for i in 0..wins.len() {
        let w = &wins[i];
        let k = (w.pid, w.x as i64, w.y as i64, w.w as i64, w.h as i64);
        match seen.get(&k) {
            None => {
                seen.insert(k, i);
            }
            Some(&j) => {
                // Both titled: two real windows, no dedup.
                if !wins[j].title.is_empty() && !w.title.is_empty() {
                    continue;
                }
                // At least one untitled overlay layer: keep the titled one,
                // otherwise the frontmost.
                if wins[j].title.is_empty() && !w.title.is_empty() {
                    keep[j] = false;
                    seen.insert(k, i);
                } else {
                    keep[i] = false;
                }
            }
        }
    }
    let mut keep_iter = keep.into_iter();
    wins.retain(|_| keep_iter.next().unwrap());

    // Drop same-app windows fully contained in another window when they look
    // like overlays (empty title, transparent, or much smaller). This is the
    // Feishu "watermark layer" case: a child overlay window with no title.
    // Only apply the empty-title rule when titles are actually available
    // (without Screen Recording permission every title comes back empty).
    let titles_available = wins.iter().any(|w| !w.title.is_empty());
    let n = wins.len();
    let mut remove = vec![false; n];

    // Group indices by pid: only same-app windows can contain one another, so
    // the containment scan is per-app instead of an O(n²) cross-app sweep. With
    // many apps this turns sum(group_size²) comparisons into just the few
    // relevant ones.
    let mut by_pid: HashMap<i32, Vec<usize>> = HashMap::new();
    for (i, w) in wins.iter().enumerate() {
        by_pid.entry(w.pid).or_default().push(i);
    }

    for (i, w) in wins.iter().enumerate() {
        let empty_title = titles_available && w.title.is_empty();
        let mut suspicious = empty_title || w.alpha < 0.98;
        if !suspicious {
            if let Some(group) = by_pid.get(&w.pid) {
                for &j in group {
                    if i == j {
                        continue;
                    }
                    let o = &wins[j];
                    let contained = o.x <= w.x + 2.0
                        && o.y <= w.y + 2.0
                        && o.x + o.w >= w.x + w.w - 2.0
                        && o.y + o.h >= w.y + w.h - 2.0;
                    if contained && (w.w * w.h) < 0.3 * (o.w * o.h) {
                        suspicious = true;
                        break;
                    }
                }
            }
        }
        remove[i] = suspicious;
    }
    let mut remove_iter = remove.into_iter();
    wins.retain(|_| !remove_iter.next().unwrap());

    // Per-app guarantee: never wipe out an entire app. If every window of an
    // app was dropped by the overlay heuristics above, re-add its main window.
    for (pid, main) in mains {
        if !wins.iter().any(|w| w.pid == pid) {
            wins.push(main);
        }
    }
}

/// Cheap on-screen window-id enumeration for background warm-up: applies only
/// `basic_ok` (O(n)) and skips `sanitize`'s O(n²) overlay heuristics. The
/// capture scheduler's idle re-sync uses this to keep `tracked` warm between
/// shows; exactness is not required because `collect()` runs again on every
/// show (and its refresh captures anything missing).
pub fn onscreen_window_ids(cfg: &Config, our_pid: u32) -> Vec<u32> {
    let mut ids: Vec<u32> = all_windows()
        .iter()
        .filter(|w| w.pid != our_pid as i32 && basic_ok(w, cfg))
        .map(|w| w.id)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// True when the window's center lies inside `b` (x, y, w, h in global CG
/// coordinates, top-left origin — the same space as `kCGWindowBounds`).
fn center_in_bounds(w: &Win, b: (f64, f64, f64, f64)) -> bool {
    let cx = w.x + w.w / 2.0;
    let cy = w.y + w.h / 2.0;
    cx >= b.0 && cx <= b.0 + b.2 && cy >= b.1 && cy <= b.1 + b.3
}

/// Sorted ids of the layer-0 on-screen windows on `display` (noise owners
/// excluded). Used to detect a desktop switch on the overlay's display while
/// it is visible — per-display space SPI (`CGSGetDisplayActiveSpace`) is
/// unavailable on modern macOS, so we fingerprint the visible window set
/// instead.
pub fn display_window_ids(display: (f64, f64, f64, f64)) -> Vec<u32> {
    let mut ids: Vec<u32> = all_windows()
        .iter()
        .filter(|w| w.layer == 0 && !is_noise_owner(&w.owner) && center_in_bounds(w, display))
        .map(|w| w.id)
        .collect();
    ids.sort_unstable();
    ids
}

/// All window ids currently managed by the window server, INCLUDING minimized
/// windows and windows on other Spaces (no on-screen filter). Used to decide
/// whether a tag slot is free after its window closed: a window that merely
/// lives on another Space is still alive and keeps its slot; a closed window
/// disappears from the window server and frees the slot for reuse.
pub fn window_ids_all() -> HashSet<u32> {
    let arr = unsafe { ffi::cg_window_list_all() };
    if arr.is_null() {
        return HashSet::new();
    }
    let n = unsafe { ffi::cf_array_count(arr) };
    let mut ids = HashSet::with_capacity(n);
    for i in 0..n {
        let d = unsafe { ffi::cf_array_get(arr, i) };
        if d.is_null() {
            continue;
        }
        if let Some(id) = unsafe {
            ffi::cf_dict_num_i32(d as ffi::CFDictionaryRef, ffi::kCGWindowNumber)
        } {
            ids.insert(id as u32);
        }
    }
    unsafe { ffi::cf_release(arr) };
    ids
}

/// Collect the item list for a mode.
/// Returns (items, mode-2 pid).
///
/// `display` is the frame (global CG coords) of the display the switcher is
/// scoped to. Each display's current desktop is its own independent world:
/// `CGWindowList` is on-screen-only, so once windows are limited to one
/// display's bounds they are exactly that display's current desktop.
///
/// `front_pid` is the frontmost app pid for `Mode::App`. It must be computed
/// by the caller on the main thread when `collect` runs off-thread
/// (NSWorkspace is AppKit and must not be touched from a background thread);
/// `None` lets `collect` query it itself (only valid on the main thread).
pub fn collect(
    cfg: &Config,
    mode: Mode,
    our_pid: u32,
    display: Option<&(f64, f64, f64, f64)>,
    front_pid: Option<i32>,
) -> Vec<Item> {
    let mut wins = all_windows();
    wins.retain(|w| w.pid != our_pid as i32);
    sanitize(&mut wins, cfg);
    scope_and_annotate(wins, mode, display, front_pid, our_pid)
}

/// Post-sanitize stage of `collect`: compute per-app sibling counts, scope to
/// the target display, then map to items.
///
/// Counts MUST be computed over the GLOBAL on-screen list (before scoping):
/// `activate_item` skips the AX raise/focus round trips only when the count is
/// 1. Using the display-scoped count would mislabel a multi-display app (e.g.
/// one VSCode window per display) as "single-window", and `ActivateAllWindows`
/// would then wrongly raise the windows on the OTHER displays too.
fn scope_and_annotate(
    wins: Vec<Win>,
    mode: Mode,
    display: Option<&(f64, f64, f64, f64)>,
    front_pid: Option<i32>,
    our_pid: u32,
) -> Vec<Item> {
    let mut counts: HashMap<i32, usize> = HashMap::new();
    for w in &wins {
        *counts.entry(w.pid).or_insert(0) += 1;
    }

    // Scope to the target display. A failed match is intentionally empty:
    // falling back to the global list would violate display isolation.
    let mut wins = wins;
    if let Some(b) = display {
        let matched = wins.iter().filter(|w| center_in_bounds(w, *b)).count();
        if matched == 0 {
            return Vec::new();
        }
        wins.retain(|w| center_in_bounds(w, *b));
    }

    match mode {
        Mode::Space => {
            // Every active window on the current Space, one entry per window.
            // (A per-app representative list was tried before, but the product
            // wants every window — e.g. multiple VSCode/Chrome windows.)
            wins.iter().map(|w| item_from(w, counts[&w.pid])).collect()
        }
        Mode::App => {
            // Subset of Mode::Space: only the frontmost app's windows.
            let front = match front_pid {
                Some(p) => p,
                None => frontmost_pid().unwrap_or(-1),
            };
            if front <= 0 || front == our_pid as i32 {
                return Vec::new();
            }
            wins.iter().filter(|w| w.pid == front).map(|w| item_from(w, counts[&w.pid])).collect()
        }
    }
}

fn item_from(w: &Win, n_same_pid: usize) -> Item {
    let aspect = if w.h > 1.0 { w.w / w.h } else { 1.0 };
    Item {
        id: w.id,
        pid: w.pid,
        owner: w.owner.clone(),
        title: w.title.clone(),
        aspect: aspect.clamp(0.4, 3.0),
        x: w.x,
        y: w.y,
        w: w.w,
        h: w.h,
        n_same_pid,
    }
}

// ---------- Accessibility helpers ----------

/// True if the process has Accessibility trust.
pub fn ax_available() -> bool {
    unsafe { ffi::AXIsProcessTrusted() }
}

/// True when the AX window bounds match the item's CG bounds closely enough.
fn bounds_match(item: &Item, x: f64, y: f64, w: f64, h: f64) -> bool {
    (x - item.x).abs() <= 6.0
        && (y - item.y).abs() <= 6.0
        && (w - item.w).abs() <= 8.0
        && (h - item.h).abs() <= 8.0
}

/// Raise + focus a window via AX. Returns whether AX succeeded.
fn ax_raise_window(item: &Item) -> bool {
    if !ax_available() {
        return false;
    }
    let app = unsafe { ffi::AXUIElementCreateApplication(item.pid) };
    if app.is_null() {
        return false;
    }
    let mut val: ffi::CFTypeRef = std::ptr::null();
    let err =
        unsafe { ffi::AXUIElementCopyAttributeValue(app, ffi::cstr_static("AXWindows"), &mut val) };
    if err != 0 || val.is_null() {
        unsafe { ffi::cf_release(app) };
        return false;
    }
    let arr = val as ffi::CFArrayRef;
    let n = unsafe { ffi::cf_array_count(arr) };
    let mut target: ffi::AXUIElementRef = std::ptr::null();

    // Pass 0: match by CGWindowID. `_AXUIElementGetWindow` maps an AX window
    // element to its CGWindowNumber, which is exactly `item.id` — authoritative
    // and immune to AX/CG coordinate mismatches (which otherwise confuse two
    // same-size windows of one app on different displays).
    for i in 0..n {
        let el = unsafe { ffi::cf_array_get(arr, i) };
        let ax = el as ffi::AXUIElementRef;
        let mut cg_id: ffi::CGWindowID = 0;
        if unsafe { ffi::_AXUIElementGetWindow(ax, &mut cg_id) } == 0 && cg_id == item.id {
            target = ax;
            break;
        }
    }

    // Pass 1: prefer the AX window whose pid, bounds AND title match. Two
    // maximized windows of the same app (e.g. VSCode) have identical bounds,
    // so matching by bounds alone raises the frontmost one instead of the
    // selected one. The pid check guards against apps that proxy windows
    // from helper/child processes into their AXWindows list.
    if target.is_null() && !item.title.is_empty() {
        for i in 0..n {
            let el = unsafe { ffi::cf_array_get(arr, i) };
            let ax = el as ffi::AXUIElementRef;
            if let Some(pid) = unsafe { ffi::ax_copy_pid(ax) } {
                if pid != item.pid {
                    continue;
                }
            }
            let Some((x, y, w, h)) = (unsafe { ffi::ax_window_bounds(el) }) else {
                continue;
            };
            if !bounds_match(item, x, y, w, h) {
                continue;
            }
            let title = unsafe { ffi::ax_copy_string(ax, ffi::cstr_static("AXTitle")) };
            if let Some(t) = title {
                if t.trim() == item.title.trim() {
                    target = ax;
                    break;
                }
            }
        }
    }

    // Pass 2: fall back to bounds-only matching (previous behavior), still
    // preferring the window whose own pid matches the item.
    if target.is_null() {
        let mut any_bounds: ffi::AXUIElementRef = std::ptr::null();
        for i in 0..n {
            let el = unsafe { ffi::cf_array_get(arr, i) };
            let ax = el as ffi::AXUIElementRef;
            // If the element exposes AXPid, require it to match; otherwise
            // (attribute unavailable) accept it so existing behavior is kept.
            let pid_ok = match unsafe { ffi::ax_copy_pid(ax) } {
                Some(pid) => pid == item.pid,
                None => true,
            };
            let Some((x, y, w, h)) = (unsafe { ffi::ax_window_bounds(el) }) else {
                continue;
            };
            if !bounds_match(item, x, y, w, h) {
                continue;
            }
            // Last-resort: keep the first bounds-matching window in case no
            // pid-matching window exists.
            if any_bounds.is_null() {
                any_bounds = ax;
            }
            if pid_ok {
                target = ax;
                break;
            }
        }
        if target.is_null() {
            target = any_bounds;
        }
    }

    // Raising OR focusing the specific window is what matters for bringing it
    // forward; either success keeps us on `ActivateIgnoringOtherApps` so we
    // don't surface the wrong window via `ActivateAllWindows`.
    let raise_ok = if !target.is_null() {
        unsafe { ffi::AXUIElementPerformAction(target, ffi::cstr_static("AXRaise")) == 0 }
    } else {
        false
    };
    let focus_ok = if !target.is_null() {
        unsafe {
            ffi::AXUIElementSetAttributeValue(app, ffi::cstr_static("AXFocusedWindow"), target)
                == 0
        }
    } else {
        false
    };
    let ok = raise_ok || focus_ok;
    unsafe {
        ffi::cf_release(val);
        ffi::cf_release(app);
    }
    ok
}

/// Activate the app of `item`, raising the window first (AX) when possible.
pub fn activate_item(item: &Item) {
    // A globally single-window app has no ambiguity about which window to
    // raise, so skip the AX round trips and activate the app directly. Apps
    // with more than one on-screen window (across all displays — e.g. one
    // VSCode window per display, or two same-bounds VSCode windows) still go
    // through AX so we raise the specific window instead of `ActivateAllWindows`
    // (which would wrongly raise the other displays' windows too).
    let multi = item.n_same_pid > 1;
    let ax_ok = if multi { ax_raise_window(item) } else { false };
    if multi && !ax_ok {
        util::log(&format!(
            "AX raise failed for [{}] {} (pid {}) — falling back to app activation",
            item.id, item.owner, item.pid
        ));
    }
    if let Some(app) = running_app(item.pid) {
        let opts = if ax_ok {
            2u64 // ActivateIgnoringOtherApps
        } else {
            3u64 // | ActivateAllWindows
        };
        unsafe {
            let _: bool = msg_send![&app, activateWithOptions: opts];
        }
    }
}

pub fn running_app(pid: i32) -> Option<Retained<NSRunningApplication>> {
    unsafe {
        let app: Option<Retained<NSRunningApplication>> = msg_send![
            NSRunningApplication::class(),
            runningApplicationWithProcessIdentifier: pid
        ];
        app
    }
}

pub fn frontmost_pid() -> Option<i32> {
    unsafe {
        let ws = NSWorkspace::sharedWorkspace();
        let front: Option<Retained<NSRunningApplication>> = msg_send![&ws, frontmostApplication];
        front.map(|a| msg_send![&a, processIdentifier])
    }
}

/// Open a System Settings pane URL once (best-effort).
pub fn open_settings_pane(url: &str) {
    unsafe {
        let ns = util::ns_string(url);
        let url: Option<Retained<objc2_foundation::NSURL>> =
            msg_send![objc2_foundation::NSURL::class(), URLWithString: &*ns];
        if let Some(u) = url {
            let ws = NSWorkspace::sharedWorkspace();
            let _: bool = msg_send![&ws, openURL: &*u];
        }
    }
}

#[cfg(test)]
mod sanitize_tests {
    use super::*;
    use crate::config::Config;

    fn win(pid: i32, id: u32, title: &str, x: f64, y: f64, w: f64, h: f64) -> Win {
        Win {
            id,
            pid,
            owner: "App".into(),
            title: title.into(),
            x,
            y,
            w,
            h,
            layer: 0,
            alpha: 1.0,
            onscreen: true,
        }
    }

    #[test]
    fn feishu_keeps_titled_main_window() {
        // Feishu: two fullscreen windows, identical bounds. The UNTITLED child
        // layer is frontmost (appears first); the titled main window is behind.
        // Before the fix the frontmost untitled one survived the bounds-dedup,
        // then the empty-title rule erased it -> Feishu vanished entirely.
        let mut wins = vec![
            win(1375, 112, "", 0.0, 30.0, 1920.0, 1050.0), // frontmost, untitled
            win(1375, 99, "飞书", 0.0, 30.0, 1920.0, 1050.0),
        ];
        sanitize(&mut wins, &Config::default());
        assert_eq!(wins.len(), 1, "Feishu must keep exactly one window");
        assert_eq!(wins[0].id, 99, "the titled main window must survive");
        assert!(!wins[0].title.is_empty());
    }

    #[test]
    fn app_with_all_untitled_windows_keeps_one_main() {
        // An app whose windows all have empty titles must still keep one
        // (frontmost) window instead of disappearing.
        let mut wins = vec![
            win(7, 1, "", 0.0, 0.0, 800.0, 600.0),
            win(7, 2, "", 100.0, 100.0, 400.0, 300.0),
        ];
        sanitize(&mut wins, &Config::default());
        assert!(!wins.is_empty(), "app with only untitled windows must survive");
        assert!(wins.iter().all(|w| w.pid == 7));
        assert_eq!(wins.len(), 1, "the frontmost main window is kept");
        assert_eq!(wins[0].id, 1);
    }

    #[test]
    fn real_watermark_layer_still_dropped() {
        // A genuine Feishu watermark: untitled child fully contained in a much
        // bigger same-app window. The bigger (titled) window survives; the
        // watermark is dropped and NOT resurrected by the per-app guarantee.
        let mut wins = vec![
            win(1375, 100, "飞书", 0.0, 30.0, 1920.0, 1050.0),
            win(1375, 101, "", 200.0, 200.0, 400.0, 300.0), // watermark
        ];
        sanitize(&mut wins, &Config::default());
        assert_eq!(wins.len(), 1);
        assert_eq!(wins[0].id, 100);
        assert!(!wins[0].title.is_empty());
    }

    #[test]
    fn exact_duplicate_bounds_dedupe_prefers_title() {
        // Same bounds, both untitled: keep the frontmost only.
        let mut wins = vec![win(3, 11, "", 0.0, 0.0, 500.0, 400.0), win(3, 12, "", 0.0, 0.0, 500.0, 400.0)];
        sanitize(&mut wins, &Config::default());
        assert_eq!(wins.len(), 1);
        assert_eq!(wins[0].id, 11);
    }

    #[test]
    fn two_titled_windows_same_bounds_both_kept() {
        // Two maximized VSCode windows on one display: identical bounds but
        // both genuinely titled windows — neither is an overlay layer, so
        // both must stay (only one used to survive the bounds dedup).
        let mut wins = vec![
            win(24334, 9603, "dev.log — workspace [SSH: a]", 0.0, 30.0, 1920.0, 1050.0),
            win(24334, 9602, ".gitignore — workspace [SSH: b]", 0.0, 30.0, 1920.0, 1050.0),
        ];
        sanitize(&mut wins, &Config::default());
        assert_eq!(wins.len(), 2, "both titled same-bounds windows must survive");
        assert_eq!(wins[0].id, 9603);
        assert_eq!(wins[1].id, 9602);
    }

    #[test]
    fn distinct_windows_all_kept() {
        // Multiple distinct windows per app (the ⌘Tab all-windows feature):
        // nothing is wrongly deduped when bounds differ.
        let mut wins = vec![
            win(5, 20, "a", 0.0, 0.0, 800.0, 600.0),
            win(5, 21, "b", 900.0, 0.0, 800.0, 600.0),
            win(5, 22, "c", 0.0, 700.0, 800.0, 600.0),
        ];
        sanitize(&mut wins, &Config::default());
        assert_eq!(wins.len(), 3);
    }

    #[test]
    fn multi_display_app_keeps_global_sibling_count() {
        // Regression: an app with one window per display must keep
        // n_same_pid == 2 even after scoping to a single display. A scoped
        // count of 1 would make `activate_item` skip AX and use
        // `ActivateAllWindows`, wrongly raising the OTHER display's window too.
        let wins = vec![
            win(24334, 9601, "a.ts", 0.0, 30.0, 1920.0, 1050.0),     // display 1
            win(24334, 9602, "b.ts", 2560.0, 30.0, 1920.0, 1050.0),  // display 2
        ];
        let display2 = (2560.0, 0.0, 1920.0, 1080.0);
        let items = scope_and_annotate(wins, Mode::Space, Some(&display2), None, 9999);
        assert_eq!(items.len(), 1, "only the display-2 window is in scope");
        assert_eq!(items[0].id, 9602);
        assert_eq!(items[0].n_same_pid, 2, "sibling count must be global, not scoped");
    }

    #[test]
    fn single_window_app_sibling_count_one() {
        let wins = vec![win(7, 1, "only", 0.0, 0.0, 800.0, 600.0)];
        let items = scope_and_annotate(wins, Mode::Space, None, None, 9999);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].n_same_pid, 1);
    }

    #[test]
    fn display_filter_failure_returns_empty() {
        let wins = vec![win(7, 1, "only", 0.0, 0.0, 800.0, 600.0)];
        let display = (2000.0, 0.0, 1000.0, 800.0);
        let items = scope_and_annotate(wins, Mode::Space, Some(&display), None, 9999);
        assert!(items.is_empty());
    }
}
