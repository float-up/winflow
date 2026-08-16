//! Startup permission validation.
//!
//! winflow needs two TCC permissions to work:
//! - Accessibility: for the HID event tap to intercept ⌘Tab / ⌘` and for
//!   AX-based window raise/focus.
//! - Screen Recording: for window thumbnails and window titles.
//!
//! On every launch we check both; if any is missing we proactively show an
//! NSAlert dialog with a button that opens the matching System Settings pane.

use objc2::rc::Retained;
use objc2::msg_send;
use objc2_app_kit::{NSAlert, NSAlertStyle, NSApplication, NSModalResponse};

use crate::ffi;
use crate::util;
use crate::windows;

pub struct PermStatus {
    pub accessibility: bool,
    pub screen_recording: bool,
}

pub fn check() -> PermStatus {
    PermStatus {
        accessibility: unsafe { ffi::ax_is_trusted() },
        screen_recording: unsafe { ffi::screen_capture_access() },
    }
}

/// Check permissions at startup; when something is missing, show a dialog.
/// Must be called on the main thread after the app is set up.
pub fn prompt_if_missing() {
    let s = check();
    if s.accessibility && s.screen_recording {
        util::log("permissions OK (accessibility + screen recording)");
        return;
    }
    prompt(&s);
}

/// Dev aid: force-show the permission dialog as if both permissions were
/// missing (used by `--force-perm-dialog` to exercise the alert UI).
pub fn prompt_for_test() {
    prompt(&PermStatus { accessibility: false, screen_recording: false });
}

fn prompt(s: &PermStatus) {

    let mut lines: Vec<String> = Vec::new();
    if !s.accessibility {
        lines.push("• 辅助功能 — 拦截 ⌘Tab / ⌘` 并置前窗口".to_string());
    }
    if !s.screen_recording {
        lines.push("• 屏幕录制 — 显示窗口缩略图与标题".to_string());
    }
    let info = format!(
        "检测到缺少以下权限：\n{}\n\n请点击下方按钮前往系统设置授权，然后重新打开 winflow。",
        lines.join("\n")
    );

    unsafe {
        let mtm = objc2::MainThreadMarker::new().expect("permission check runs on main thread");
        let app = NSApplication::sharedApplication(mtm);
        let _: () = msg_send![&app, activateIgnoringOtherApps: true];

        let alert = NSAlert::new(mtm);
        let _: () = msg_send![&alert, setMessageText: &*util::ns_string("winflow 需要权限")];
        let _: () = msg_send![&alert, setInformativeText: &*util::ns_string(&info)];
        let _: () = msg_send![&alert, setAlertStyle: NSAlertStyle::Warning];

        // Buttons (order = first/second/third button return codes).
        let (has_ax_missing, has_screen_missing) = (!s.accessibility, !s.screen_recording);
        let _: Retained<objc2_app_kit::NSButton> = if has_ax_missing && has_screen_missing {
            msg_send![&alert, addButtonWithTitle: &*util::ns_string("打开辅助功能设置")]
        } else {
            msg_send![&alert, addButtonWithTitle: &*util::ns_string("打开系统设置")]
        };
        let _: Retained<objc2_app_kit::NSButton> = if has_screen_missing {
            msg_send![&alert, addButtonWithTitle: &*util::ns_string("打开屏幕录制设置")]
        } else {
            msg_send![&alert, addButtonWithTitle: &*util::ns_string("稍后再说")]
        };
        if has_ax_missing && has_screen_missing {
            let _: Retained<objc2_app_kit::NSButton> =
                msg_send![&alert, addButtonWithTitle: &*util::ns_string("稍后再说")];
        }

        let response: NSModalResponse = msg_send![&alert, runModal];
        match response {
            r if r == 1000 => {
                if has_ax_missing {
                    windows::open_settings_pane(
                        "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
                    );
                } else if has_screen_missing {
                    windows::open_settings_pane(
                        "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
                    );
                }
            }
            r if r == 1001 && has_ax_missing && has_screen_missing => {
                windows::open_settings_pane(
                    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
                );
            }
            _ => {
                util::log("permission prompt dismissed — winflow 将持续提醒直至授权");
            }
        }
    }
}
