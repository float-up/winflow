//! Small shared helpers.

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::ClassType;
use objc2_foundation::NSString;
// Logging disabled for performance.
// use std::io::Write;
// use std::sync::{Mutex, OnceLock};

// Optional log file sink (used when running as a bundled .app without a
// terminal). Disabled for performance.
// static LOG_FILE: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

/// Open `~/Library/Logs/winflow.log` for append. Call once at startup.
pub fn init_log_file() {
    // Logging disabled for performance.
    // let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    // let dir = std::path::PathBuf::from(&home).join("Library/Logs");
    // let _ = std::fs::create_dir_all(&dir);
    // let path = dir.join("winflow.log");
    // let f = std::fs::OpenOptions::new()
    //     .create(true)
    //     .append(true)
    //     .open(&path)
    //     .ok();
    // let _ = LOG_FILE.set(Mutex::new(f));
    // if let Some(m) = LOG_FILE.get() {
    //     if m.lock().unwrap().is_some() {
    //         eprintln!("[winflow] 日志文件: {}", path.display());
    //     }
    // }
}

pub fn log(_msg: &str) {
    // Logging disabled for performance.
    // eprintln!("[winflow] {}", msg);
    // if let Some(m) = LOG_FILE.get() {
    //     if let Ok(mut f) = m.lock() {
    //         if let Some(f) = f.as_mut() {
    //             let _ = writeln!(f, "{}", msg);
    //         }
    //     }
    // }
}

pub fn truncate(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        s.to_string()
    } else {
        let head: String = s.chars().take(max_chars).collect();
        format!("{}…", head)
    }
}

/// Build an NSString from a Rust &str.
pub fn ns_string(s: &str) -> Retained<NSString> {
    let c = std::ffi::CString::new(s).unwrap_or_default();
    unsafe { msg_send![NSString::class(), stringWithUTF8String: c.as_ptr()] }
}

/// Convert an NSString to a Rust String (via the UTF-8 C string).
pub fn ns_string_to_rust(s: &NSString) -> String {
    unsafe {
        let ptr: *const std::ffi::c_char = msg_send![s, UTF8String];
        if ptr.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }
}
