//! Hand-rolled FFI bindings for the minimal set of C APIs we need:
//! CoreGraphics (window list / imaging / event taps / CGS space SPI),
//! CoreFoundation (CFArray/CFDictionary/CFString/CFNumber helpers),
//! ApplicationServices (Accessibility AX), libdispatch (main-queue callbacks).
#![allow(non_upper_case_globals, dead_code)]

use std::collections::HashMap;
use std::ffi::{c_char, c_void, CString};
use std::sync::{Mutex, OnceLock};

// ---------- opaque types ----------

pub type CFIndex = isize;
pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFArrayRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFNumberRef = *const c_void;
pub type CFMachPortRef = *const c_void;
pub type CFRunLoopRef = *const c_void;
pub type CFRunLoopSourceRef = *const c_void;
pub type CFRunLoopTimerRef = *const c_void;
pub type CFTimeInterval = f64;
pub type CFTypeID = usize;
pub type CGWindowID = u32;
pub type CGDirectDisplayID = u32;
pub type CGImageRef = *const c_void;
pub type CGEventRef = *mut c_void;
pub type CGEventTapProxy = *const c_void;
pub type CGEventType = u32;
pub type CGEventFlags = u64;
pub type CGContextRef = *mut c_void;
pub type CGColorSpaceRef = *const c_void;
pub type CGSConnectionID = u32;
pub type AXUIElementRef = *const c_void;

/// C `CGEventTapCallBack` signature. Returning NULL from the callback
/// swallows the event (only allowed for non-listen-only taps).
pub type CGEventTapCallBack = unsafe extern "C" fn(
    proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    refcon: *mut c_void,
) -> CGEventRef;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

impl CGRect {
    pub fn null() -> CGRect {
        CGRect {
            origin: CGPoint { x: f64::INFINITY, y: f64::INFINITY },
            size: CGSize { width: 0.0, height: 0.0 },
        }
    }
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect { origin: CGPoint { x, y }, size: CGSize { width: w, height: h } }
    }
}

// ---------- constants ----------

pub const KCG_NULL_WINDOW_ID: CGWindowID = 0;
pub const KCG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY: u32 = 1 << 0;
pub const KCG_WINDOW_LIST_OPTION_INCLUDING_WINDOW: u32 = 1 << 3;
pub const KCG_WINDOW_LIST_OPTION_EXCLUDE_DESKTOP_ELEMENTS: u32 = 1 << 4;
pub const KCG_WINDOW_IMAGE_OPTION_BOUNDS_IGNORE_FRAMING: u32 = 1 << 0;

pub const KCG_HID_EVENT_TAP: u32 = 0;
pub const KCG_HEAD_INSERT_EVENT_TAP: u32 = 0;
pub const KCG_EVENT_TAP_OPTION_DEFAULT: u32 = 0;
pub const KCG_EVENT_KEY_DOWN: u32 = 10;
pub const KCG_EVENT_KEY_UP: u32 = 11;
pub const KCG_EVENT_FLAGS_CHANGED: u32 = 12;
pub const KCG_KEYBOARD_EVENT_KEYCODE: u32 = 9;

pub const KCG_EVENT_FLAG_SHIFT: u64 = 0x0002_0000;
pub const KCG_EVENT_FLAG_CONTROL: u64 = 0x0004_0000;
pub const KCG_EVENT_FLAG_ALTERNATE: u64 = 0x0008_0000;
pub const KCG_EVENT_FLAG_COMMAND: u64 = 0x0010_0000;

// Keycodes (HIToolbox Events.h)
pub const KVK_ANSI_A: u16 = 0;
pub const KVK_ANSI_S: u16 = 1;
pub const KVK_ANSI_D: u16 = 2;
pub const KVK_ANSI_F: u16 = 3;
pub const KVK_ANSI_H: u16 = 4;
pub const KVK_ANSI_G: u16 = 5;
pub const KVK_ANSI_Z: u16 = 6;
pub const KVK_ANSI_X: u16 = 7;
pub const KVK_ANSI_C: u16 = 8;
pub const KVK_ANSI_V: u16 = 9;
pub const KVK_ANSI_B: u16 = 11;
pub const KVK_ANSI_Q: u16 = 12;
pub const KVK_ANSI_W: u16 = 13;
pub const KVK_ANSI_E: u16 = 14;
pub const KVK_ANSI_R: u16 = 15;
pub const KVK_ANSI_Y: u16 = 16;
pub const KVK_ANSI_T: u16 = 17;
pub const KVK_ANSI_1: u16 = 18;
pub const KVK_ANSI_2: u16 = 19;
pub const KVK_ANSI_3: u16 = 20;
pub const KVK_ANSI_4: u16 = 21;
pub const KVK_ANSI_6: u16 = 22;
pub const KVK_ANSI_5: u16 = 23;
pub const KVK_ANSI_EQUAL: u16 = 24;
pub const KVK_ANSI_9: u16 = 25;
pub const KVK_ANSI_7: u16 = 26;
pub const KVK_ANSI_MINUS: u16 = 27;
pub const KVK_ANSI_8: u16 = 28;
pub const KVK_ANSI_0: u16 = 29;
pub const KVK_ANSI_RIGHTBRACKET: u16 = 30;
pub const KVK_ANSI_O: u16 = 31;
pub const KVK_ANSI_U: u16 = 32;
pub const KVK_ANSI_LEFTBRACKET: u16 = 33;
pub const KVK_ANSI_I: u16 = 34;
pub const KVK_ANSI_P: u16 = 35;
pub const KVK_RETURN: u16 = 36;
pub const KVK_ANSI_L: u16 = 37;
pub const KVK_ANSI_J: u16 = 38;
pub const KVK_ANSI_QUOTE: u16 = 39;
pub const KVK_ANSI_K: u16 = 40;
pub const KVK_ANSI_SEMICOLON: u16 = 41;
pub const KVK_ANSI_BACKSLASH: u16 = 42;
pub const KVK_ANSI_COMMA: u16 = 43;
pub const KVK_ANSI_SLASH: u16 = 44;
pub const KVK_ANSI_N: u16 = 45;
pub const KVK_ANSI_M: u16 = 46;
pub const KVK_ANSI_PERIOD: u16 = 47;
pub const KVK_TAB: u16 = 48;
pub const KVK_SPACE: u16 = 49;
pub const KVK_ANSI_GRAVE: u16 = 50;
pub const KVK_ESCAPE: u16 = 53;
pub const KVK_COMMAND_L: u16 = 55;
pub const KVK_SHIFT_L: u16 = 56;
pub const KVK_OPTION_L: u16 = 58;
pub const KVK_CONTROL_L: u16 = 59;
pub const KVK_SHIFT_R: u16 = 60;
pub const KVK_OPTION_R: u16 = 61;
pub const KVK_CONTROL_R: u16 = 62;
pub const KVK_LEFT_ARROW: u16 = 123;
pub const KVK_RIGHT_ARROW: u16 = 124;
pub const KVK_DOWN_ARROW: u16 = 125;
pub const KVK_UP_ARROW: u16 = 126;

// CoreFoundation
pub const KCF_NUMBER_SINT32_TYPE: u32 = 3;
pub const KCF_NUMBER_SINT64_TYPE: u32 = 4;
pub const KCF_NUMBER_DOUBLE_TYPE: u32 = 13;
pub const KCF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
pub const KCG_BITMAP_INFO_PREMULTIPLIED_LAST: u32 = 1;
/// `kCGInterpolationHigh` — high-quality resampling for thumbnail downscaling.
pub const KCG_INTERPOLATION_HIGH: i32 = 4;

// AXValue types
pub const KAX_VALUE_TYPE_CGPOINT: u32 = 1;
pub const KAX_VALUE_TYPE_CGSIZE: u32 = 2;

// AppKit-level window constants (raw values, mirroring the headers)
/// NSWindowLevel for pop-up menus: above normal windows and the menu bar.
pub const NSPOPUP_MENU_LEVEL: i64 = 101;
/// NSWindowCollectionBehavior: CanJoinAllSpaces(1<<0) | IgnoresCycle(1<<6) | FullScreenAuxiliary(1<<8)
pub const NS_COLLECTION_BEHAVIOR: usize = (1 << 0) | (1 << 6) | (1 << 8);

// ---------- CoreGraphics ----------

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub fn CGWindowListCopyWindowInfo(option: u32, relativeToWindow: CGWindowID) -> CFArrayRef;
    pub fn CGWindowListCreateImage(
        screenBounds: CGRect,
        listOption: u32,
        windowID: CGWindowID,
        imageOption: u32,
    ) -> CGImageRef;
    pub fn CGImageGetWidth(image: CGImageRef) -> usize;
    pub fn CGImageGetHeight(image: CGImageRef) -> usize;
    pub fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bitsPerComponent: usize,
        bytesPerRow: usize,
        space: CGColorSpaceRef,
        bitmapInfo: u32,
    ) -> CGContextRef;
    pub fn CGBitmapContextCreateImage(context: CGContextRef) -> CGImageRef;
    pub fn CGContextDrawImage(context: CGContextRef, rect: CGRect, image: CGImageRef);
    pub fn CGContextTranslateCTM(context: CGContextRef, tx: f64, ty: f64);
    pub fn CGContextScaleCTM(context: CGContextRef, sx: f64, sy: f64);
    pub fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
    pub fn CGColorSpaceRelease(space: CGColorSpaceRef);
    pub fn CGContextRelease(context: CGContextRef);
    pub fn CGContextSetInterpolationQuality(context: CGContextRef, quality: i32);

    pub fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        eventsOfInterest: u64,
        callback: CGEventTapCallBack,
        userInfo: *mut c_void,
    ) -> CFMachPortRef;
    pub fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    pub fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    pub fn CGEventGetFlags(event: CGEventRef) -> CGEventFlags;
    pub fn CGEventCreate(allocator: *const c_void) -> CGEventRef;
    pub fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
    pub fn CGPreflightScreenCaptureAccess() -> bool;
    pub fn CGPreflightListenEventAccess() -> bool;
    pub fn CGPreflightPostEventAccess() -> bool;

    // Displays: id of the display under the cursor and per-display geometry.
    pub fn CGMainDisplayID() -> CGDirectDisplayID;
    pub fn CGGetOnlineDisplayList(
        maxDisplays: u32,
        displays: *mut CGDirectDisplayID,
        displayCount: *mut u32,
    ) -> i32;
    pub fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;

    // Private-but-stable CGS SPI (space ids), used by most window switchers.
    pub fn CGSMainConnectionID() -> CGSConnectionID;
    pub fn CGSGetActiveSpace(cid: CGSConnectionID) -> i64;

    // CGWindowList dictionary keys (public CoreGraphics globals).
    pub static kCGWindowNumber: *const c_void;
    pub static kCGWindowOwnerPID: *const c_void;
    pub static kCGWindowOwnerName: *const c_void;
    pub static kCGWindowName: *const c_void;
    pub static kCGWindowBounds: *const c_void;
    pub static kCGWindowLayer: *const c_void;
    pub static kCGWindowAlpha: *const c_void;
    pub static kCGWindowIsOnscreen: *const c_void;
    pub static kCGWindowMemoryUsage: *const c_void;
}

// ---------- CoreFoundation ----------

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub static kCFBooleanTrue: *const c_void;
    pub static kCFBooleanFalse: *const c_void;
    pub fn CFArrayGetCount(array: CFArrayRef) -> CFIndex;
    pub fn CFArrayGetValueAtIndex(array: CFArrayRef, index: CFIndex) -> *const c_void;
    pub fn CFDictionaryGetValue(dictionary: CFDictionaryRef, key: *const c_void) -> *const c_void;
    pub fn CFStringCreateWithCString(
        allocator: *const c_void,
        cStr: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    pub fn CFStringGetCString(
        string: CFStringRef,
        buffer: *mut c_char,
        bufferSize: CFIndex,
        encoding: u32,
    ) -> bool;
    /// Returns a pointer to the string's UTF-8 bytes if it is already stored in
    /// that encoding (no copy), or NULL when a conversion is needed.
    pub fn CFStringGetCStringPtr(string: CFStringRef, encoding: u32) -> *const c_char;
    pub fn CFStringGetLength(string: CFStringRef) -> CFIndex;
    pub fn CFNumberGetValue(number: CFNumberRef, theType: u32, valuePtr: *mut c_void) -> bool;
    pub fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
    pub fn CFStringGetTypeID() -> CFTypeID;
    pub fn CFNumberGetTypeID() -> CFTypeID;
    pub fn CFRelease(cf: *const c_void);
    pub fn CFMachPortCreateRunLoopSource(
        allocator: *const c_void,
        port: CFMachPortRef,
        order: CFIndex,
    ) -> CFRunLoopSourceRef;
    pub fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub fn CFRunLoopRun();
    pub fn CFRunLoopTimerCreate(
        allocator: *const c_void,
        fireDate: CFTimeInterval,
        interval: CFTimeInterval,
        flags: u32,
        order: CFIndex,
        callout: Option<unsafe extern "C" fn(CFRunLoopTimerRef, *mut c_void)>,
        context: *const CFRunLoopTimerContext,
    ) -> CFRunLoopTimerRef;
    pub fn CFRunLoopTimerSetNextFireDate(timer: CFRunLoopTimerRef, fireDate: CFTimeInterval);
    pub fn CFRunLoopAddTimer(rl: CFRunLoopRef, timer: CFRunLoopTimerRef, mode: CFStringRef);
    pub fn CFAbsoluteTimeGetCurrent() -> CFTimeInterval;
    pub fn CFRunLoopWakeUp(rl: CFRunLoopRef);
    pub fn CFRunLoopGetMain() -> CFRunLoopRef;

    pub static kCFRunLoopCommonModes: *const c_void;
}

/// C `CFRunLoopTimerContext`.
#[repr(C)]
pub struct CFRunLoopTimerContext {
    pub version: CFIndex,
    pub info: *mut c_void,
    pub retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    pub release: Option<unsafe extern "C" fn(*const c_void)>,
    pub copy_description: Option<unsafe extern "C" fn(*const c_void) -> CFStringRef>,
}

// ---------- libdispatch is intentionally NOT used: the SDK's libdispatch.tbd
// only declares arm64e targets, so its symbols are not linkable for plain
// arm64. Cross-thread command delivery instead uses a CFRunLoopSource
// (see state.rs) which fires on the main run loop — same latency, zero
// extra dependencies.

// ---------- ApplicationServices (Accessibility) ----------

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    pub fn AXIsProcessTrusted() -> bool;
    pub fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    pub fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> i32;
    pub fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> i32;
    pub fn AXUIElementPerformAction(element: AXUIElementRef, action: CFStringRef) -> i32;
    pub fn AXValueGetValue(value: CFTypeRef, type_: u32, valuePtr: *mut c_void) -> bool;
    /// Private-but-stable SPI: map an AX window element to its CGWindowID
    /// (kAXErrorSuccess = 0 writes the id). More authoritative than matching by
    /// bounds, which can confuse same-size windows of one app across displays.
    pub fn _AXUIElementGetWindow(element: AXUIElementRef, out_window: *mut CGWindowID) -> i32;
}

// ---------- safe wrappers ----------

pub unsafe fn cg_window_list() -> CFArrayRef {
    CGWindowListCopyWindowInfo(
        KCG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY | KCG_WINDOW_LIST_OPTION_EXCLUDE_DESKTOP_ELEMENTS,
        KCG_NULL_WINDOW_ID,
    )
}

/// ALL windows managed by the window server (no on-screen filter): includes
/// minimized windows and windows on other Spaces. Used for tag-slot liveness
/// — a tagged window parked on another Space is still alive and keeps its
/// slot; a closed window disappears from this list and frees its slot.
pub unsafe fn cg_window_list_all() -> CFArrayRef {
    CGWindowListCopyWindowInfo(
        KCG_WINDOW_LIST_OPTION_EXCLUDE_DESKTOP_ELEMENTS,
        KCG_NULL_WINDOW_ID,
    )
}

pub unsafe fn active_space() -> Option<i64> {
    let s = CGSGetActiveSpace(CGSMainConnectionID());
    if s > 0 {
        Some(s)
    } else {
        None
    }
}

/// The display under the mouse cursor (falls back to the main display).
/// Iterates ONLINE displays and picks the one whose bounds contain the
/// cursor. (`CGGetDisplaysWithPoint` can report phantom/offline displays
/// that overlap the real ones on some setups, so it is not used.)
pub unsafe fn cursor_display() -> CGDirectDisplayID {
    let event = CGEventCreate(std::ptr::null());
    if !event.is_null() {
        let p = CGEventGetLocation(event);
        CFRelease(event as *const c_void);
        let mut ids = [0u32; 8];
        let mut n: u32 = 0;
        CGGetOnlineDisplayList(8, ids.as_mut_ptr(), &mut n);
        for i in 0..n as usize {
            let b = CGDisplayBounds(ids[i]);
            if p.x >= b.origin.x
                && p.x < b.origin.x + b.size.width
                && p.y >= b.origin.y
                && p.y < b.origin.y + b.size.height
            {
                return ids[i];
            }
        }
    }
    CGMainDisplayID()
}

/// Frame of a display in global CG coordinates (top-left origin, the same
/// space as `kCGWindowBounds`).
pub unsafe fn display_bounds(display: CGDirectDisplayID) -> Option<(f64, f64, f64, f64)> {
    let b = CGDisplayBounds(display);
    if b.size.width > 0.0 && b.size.height > 0.0 {
        Some((b.origin.x, b.origin.y, b.size.width, b.size.height))
    } else {
        None
    }
}

pub unsafe fn screen_capture_access() -> bool {
    CGPreflightScreenCaptureAccess()
}

pub unsafe fn listen_event_access() -> bool {
    CGPreflightListenEventAccess()
}

pub unsafe fn post_event_access() -> bool {
    CGPreflightPostEventAccess()
}

pub unsafe fn ax_is_trusted() -> bool {
    AXIsProcessTrusted()
}

pub unsafe fn cf_array_count(a: CFArrayRef) -> usize {
    if a.is_null() {
        0
    } else {
        CFArrayGetCount(a) as usize
    }
}

pub unsafe fn cf_array_get(a: CFArrayRef, i: usize) -> *const c_void {
    CFArrayGetValueAtIndex(a, i as CFIndex)
}

pub unsafe fn cf_dict_get(d: CFDictionaryRef, key: *const c_void) -> *const c_void {
    if d.is_null() {
        std::ptr::null()
    } else {
        CFDictionaryGetValue(d, key)
    }
}

pub unsafe fn cf_release(p: *const c_void) {
    if !p.is_null() {
        CFRelease(p);
    }
}

/// Create a CFString from a Rust string. Caller owns (+1).
pub fn cf_string(s: &str) -> CFStringRef {
    let c = CString::new(s).unwrap_or_default();
    unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), KCF_STRING_ENCODING_UTF8) }
}

/// A raw pointer that is explicitly `Send + Sync`. Only used for pointers
/// that are either immutable for the process lifetime or carefully guarded
/// by the surrounding synchronization (e.g. the event-tap context).
#[derive(Clone, Copy)]
pub struct RawPtr(pub *const c_void);
unsafe impl Send for RawPtr {}
unsafe impl Sync for RawPtr {}
impl RawPtr {
    pub fn get(&self) -> *const c_void {
        self.0
    }
}

/// A CGImageRef that is `Send + Sync`. The image is immutable and owned (+1);
/// cross-thread transfer is safe as long as exactly one thread releases it.
#[derive(Clone, Copy)]
pub struct CgImage(pub CGImageRef);
unsafe impl Send for CgImage {}
unsafe impl Sync for CgImage {}

/// Interned CFString for repeated use; deliberately leaked (lives for process).
///
/// The fixed set of literals used across the codebase is served by a lock-free
/// `OnceLock` per key (the previous single `Mutex<HashMap>` was taken on every
/// call — e.g. 4×/window in `cf_dict_bounds`). Unknown literals fall back to a
/// shared map so future call sites still work without panicking.
pub fn cstr_static(s: &'static str) -> CFStringRef {
    if let Some(entry) = known_static_str(s) {
        return entry.get_or_init(|| RawPtr(cf_string(s))).0;
    }
    static CACHE: OnceLock<Mutex<HashMap<&'static str, RawPtr>>> = OnceLock::new();
    let m = CACHE.get_or_init(Default::default);
    let mut m = m.lock().unwrap();
    m.entry(s).or_insert_with(|| RawPtr(cf_string(s))).0
}

/// Lock-free per-literal intern slots for the known `cstr_static` keys.
fn known_static_str(s: &'static str) -> Option<&'static OnceLock<RawPtr>> {
    static X: OnceLock<RawPtr> = OnceLock::new();
    static Y: OnceLock<RawPtr> = OnceLock::new();
    static WIDTH: OnceLock<RawPtr> = OnceLock::new();
    static HEIGHT: OnceLock<RawPtr> = OnceLock::new();
    static AX_POSITION: OnceLock<RawPtr> = OnceLock::new();
    static AX_SIZE: OnceLock<RawPtr> = OnceLock::new();
    static AX_PID: OnceLock<RawPtr> = OnceLock::new();
    static AX_WINDOWS: OnceLock<RawPtr> = OnceLock::new();
    static AX_TITLE: OnceLock<RawPtr> = OnceLock::new();
    static AX_RAISE: OnceLock<RawPtr> = OnceLock::new();
    static AX_FOCUSED_WINDOW: OnceLock<RawPtr> = OnceLock::new();
    match s {
        "X" => Some(&X),
        "Y" => Some(&Y),
        "Width" => Some(&WIDTH),
        "Height" => Some(&HEIGHT),
        "AXPosition" => Some(&AX_POSITION),
        "AXSize" => Some(&AX_SIZE),
        "AXPid" => Some(&AX_PID),
        "AXWindows" => Some(&AX_WINDOWS),
        "AXTitle" => Some(&AX_TITLE),
        "AXRaise" => Some(&AX_RAISE),
        "AXFocusedWindow" => Some(&AX_FOCUSED_WINDOW),
        _ => None,
    }
}

/// Convert a CFTypeRef that is a CFString into a Rust String.
pub unsafe fn cf_string_value(v: CFTypeRef) -> Option<String> {
    if v.is_null() || CFGetTypeID(v) != CFStringGetTypeID() {
        return None;
    }
    let s = v as CFStringRef;
    // Fast path: when the string is already stored as UTF-8, read it in place
    // (no allocation/copy). Window owners/titles are often plain ASCII/UTF-8,
    // and this runs once per window per field in the hot enumeration loop.
    let ptr = CFStringGetCStringPtr(s, KCF_STRING_ENCODING_UTF8);
    if !ptr.is_null() {
        return Some(std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned());
    }
    // Slow path: convert into a temporary buffer.
    let len = CFStringGetLength(s);
    let cap = (len * 4 + 8) as usize;
    let mut buf = vec![0u8; cap];
    let ok = CFStringGetCString(
        s,
        buf.as_mut_ptr() as *mut c_char,
        cap as CFIndex,
        KCF_STRING_ENCODING_UTF8,
    );
    if !ok {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..end]).into_owned())
}

pub unsafe fn cf_dict_str(d: CFDictionaryRef, key: *const c_void) -> Option<String> {
    cf_string_value(cf_dict_get(d, key))
}

unsafe fn cf_num(d: CFDictionaryRef, key: *const c_void) -> Option<*const c_void> {
    let v = cf_dict_get(d, key);
    if v.is_null() || CFGetTypeID(v) != CFNumberGetTypeID() {
        None
    } else {
        Some(v)
    }
}

pub unsafe fn cf_dict_num_i32(d: CFDictionaryRef, key: *const c_void) -> Option<i32> {
    let v = cf_num(d, key)?;
    let mut out: i32 = 0;
    if CFNumberGetValue(v as CFNumberRef, KCF_NUMBER_SINT32_TYPE, &mut out as *mut i32 as *mut c_void) {
        Some(out)
    } else {
        None
    }
}

pub unsafe fn cf_dict_num_i64(d: CFDictionaryRef, key: *const c_void) -> Option<i64> {
    let v = cf_num(d, key)?;
    let mut out: i64 = 0;
    if CFNumberGetValue(v as CFNumberRef, KCF_NUMBER_SINT64_TYPE, &mut out as *mut i64 as *mut c_void) {
        Some(out)
    } else {
        None
    }
}

pub unsafe fn cf_dict_num_double(d: CFDictionaryRef, key: *const c_void) -> Option<f64> {
    let v = cf_num(d, key)?;
    let mut out: f64 = 0.0;
    if CFNumberGetValue(v as CFNumberRef, KCF_NUMBER_DOUBLE_TYPE, &mut out as *mut f64 as *mut c_void) {
        Some(out)
    } else {
        None
    }
}

/// Read a dictionary boolean. Some keys (e.g. `kCGWindowIsOnscreen`) are
/// stored as CFBoolean, others as CFNumber.
pub unsafe fn cf_dict_bool(d: CFDictionaryRef, key: *const c_void) -> Option<bool> {
    let v = cf_dict_get(d, key);
    if v.is_null() {
        return None;
    }
    if v == kCFBooleanTrue {
        return Some(true);
    }
    if v == kCFBooleanFalse {
        return Some(false);
    }
    cf_dict_num_i32(d, key).map(|n| n != 0)
}

/// Parse the CGWindowBounds dict (keys "X","Y","Width","Height").
pub unsafe fn cf_dict_bounds(d: CFDictionaryRef) -> Option<(f64, f64, f64, f64)> {
    let b = cf_dict_get(d, kCGWindowBounds);
    if b.is_null() {
        return None;
    }
    let bd = b as CFDictionaryRef;
    let x = cf_dict_num_double(bd, cstr_static("X"))?;
    let y = cf_dict_num_double(bd, cstr_static("Y"))?;
    let w = cf_dict_num_double(bd, cstr_static("Width"))?;
    let h = cf_dict_num_double(bd, cstr_static("Height"))?;
    Some((x, y, w, h))
}

pub unsafe fn cg_image_size(img: CGImageRef) -> (usize, usize) {
    (CGImageGetWidth(img), CGImageGetHeight(img))
}

/// Capture a single window and downscale it to a thumbnail of height
/// `target_h_px` pixels (returns +1 CGImage, or NULL on failure).
pub unsafe fn capture_window_image(id: CGWindowID, target_h_px: usize) -> CGImageRef {
    let img = CGWindowListCreateImage(
        CGRect::null(),
        KCG_WINDOW_LIST_OPTION_INCLUDING_WINDOW,
        id,
        KCG_WINDOW_IMAGE_OPTION_BOUNDS_IGNORE_FRAMING,
    );
    if img.is_null() {
        return std::ptr::null();
    }
    let (sw, sh) = cg_image_size(img);
    if sw == 0 || sh == 0 {
        CFRelease(img);
        return std::ptr::null();
    }
    let tw = ((sw as f64 * target_h_px as f64 / sh as f64).round() as usize).max(1);
    let th = target_h_px.max(1);
    let space = CGColorSpaceCreateDeviceRGB();
    let ctx = CGBitmapContextCreate(
        std::ptr::null_mut(),
        tw,
        th,
        8,
        tw * 4,
        space,
        KCG_BITMAP_INFO_PREMULTIPLIED_LAST,
    );
    if ctx.is_null() {
        CGColorSpaceRelease(space);
        CFRelease(img);
        return std::ptr::null();
    }
    // NOTE: the CGWindowListCreateImage output is already in the correct
    // top-down orientation for CGBitmapContextCreateImage read-back. The old
    // translate+scale(1,-1) flip here mirrored the thumbnail vertically
    // (thumbnails appeared upside down in the overlay).
    // High-quality resampling keeps the downscaled thumbnail sharp.
    CGContextSetInterpolationQuality(ctx, KCG_INTERPOLATION_HIGH);
    CGContextDrawImage(ctx, CGRect::new(0.0, 0.0, tw as f64, th as f64), img);
    let out = CGBitmapContextCreateImage(ctx);
    CGContextRelease(ctx);
    CGColorSpaceRelease(space);
    CFRelease(img);
    out
}

/// Read an AX element's position/size via AXValue, in global display coords.
pub unsafe fn ax_window_bounds(el: CFTypeRef) -> Option<(f64, f64, f64, f64)> {
    let mut pos: CFTypeRef = std::ptr::null();
    let mut size: CFTypeRef = std::ptr::null();
    let ok_pos = AXUIElementCopyAttributeValue(
        el as AXUIElementRef,
        cstr_static("AXPosition"),
        &mut pos,
    ) == 0
        && !pos.is_null();
    let ok_size = AXUIElementCopyAttributeValue(
        el as AXUIElementRef,
        cstr_static("AXSize"),
        &mut size,
    ) == 0
        && !size.is_null();
    if !ok_pos || !ok_size {
        cf_release(pos);
        cf_release(size);
        return None;
    }
    let mut p = CGPoint { x: 0.0, y: 0.0 };
    let mut s = CGSize { width: 0.0, height: 0.0 };
    let gp = AXValueGetValue(pos, KAX_VALUE_TYPE_CGPOINT, &mut p as *mut CGPoint as *mut c_void);
    let gs = AXValueGetValue(size, KAX_VALUE_TYPE_CGSIZE, &mut s as *mut CGSize as *mut c_void);
    cf_release(pos);
    cf_release(size);
    if gp && gs {
        Some((p.x, p.y, s.width, s.height))
    } else {
        None
    }
}

/// Read a CFString attribute (e.g. "AXTitle") from an AX element.
pub unsafe fn ax_copy_string(el: AXUIElementRef, attribute: CFStringRef) -> Option<String> {
    let mut val: CFTypeRef = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(el, attribute, &mut val);
    if err != 0 || val.is_null() {
        return None;
    }
    let s = cf_string_value(val);
    cf_release(val);
    s
}

/// Read the "AXPid" attribute (the process id an AX element belongs to).
pub unsafe fn ax_copy_pid(el: AXUIElementRef) -> Option<i32> {
    let mut val: CFTypeRef = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(el, cstr_static("AXPid"), &mut val);
    if err != 0 || val.is_null() {
        return None;
    }
    let mut pid: i32 = 0;
    let ok = CFNumberGetValue(
        val as CFNumberRef,
        KCF_NUMBER_SINT32_TYPE,
        &mut pid as *mut i32 as *mut c_void,
    );
    cf_release(val);
    if ok {
        Some(pid)
    } else {
        None
    }
}




