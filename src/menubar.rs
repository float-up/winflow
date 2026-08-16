//! Menu bar (status bar) integration.
//!
//! Installs a small icon in the macOS menu bar with a two-item menu:
//! - 配置… — opens the settings panel (thumbnail capture interval + quick-tap
//!   delay; changes apply immediately and are persisted to
//!   ~/Library/Application Support/winflow/settings.conf).
//! - 退出 winflow — quits the app.
//!
//! All menu actions and the panel run on the main thread (AppKit guarantees
//! this for menu actions; the panel buttons dispatch on the main thread too).

use objc2::define_class;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{msg_send, sel, AnyThread, ClassType, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSBezelStyle, NSBezierPath, NSButton, NSColor, NSImage,
    NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSTextField, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::ffi;
use crate::util;

/// SF Symbol used for the status item.
const SYMBOL: &str = "rectangle.grid.2x2";

// The status item and menu target are not retained by the system (target is
// weak; the bar does not own the item), so keep them alive for the whole
// process. Raw pointers are deliberate process-lifetime leaks.
static KEEP_ITEM: std::sync::OnceLock<ffi::RawPtr> = std::sync::OnceLock::new();
static KEEP_TARGET: std::sync::OnceLock<ffi::RawPtr> = std::sync::OnceLock::new();
static KEEP_PANEL_TARGET: std::sync::OnceLock<ffi::RawPtr> = std::sync::OnceLock::new();

/// Install the menu bar icon + menu. Call once from the main thread.
pub fn install() {
    unsafe {
        let icon = system_symbol_icon(SYMBOL).unwrap_or_else(fallback_icon);
        let _: () = msg_send![&icon, setTemplate: true];

        let bar = NSStatusBar::systemStatusBar();
        let item: Retained<NSStatusItem> = msg_send![&bar, statusItemWithLength: -1.0];
        let _: () = msg_send![&item, setImage: &*icon];

        let target: Retained<MenuTarget> = msg_send![MenuTarget::class(), new];

        let menu: Retained<NSMenu> = msg_send![NSMenu::class(), new];

        let title = menu_item("winflow", None, "");
        let _: () = msg_send![&title, setEnabled: false];
        let _: () = msg_send![&menu, addItem: &*title];

        let sep1: Retained<NSMenuItem> = msg_send![NSMenuItem::class(), separatorItem];
        let _: () = msg_send![&menu, addItem: &*sep1];

        let m1 = menu_item("配置…", Some(sel!(openConfigPanel:)), "");
        let _: () = msg_send![&m1, setTarget: &*target];
        let _: () = msg_send![&menu, addItem: &*m1];

        let sep2: Retained<NSMenuItem> = msg_send![NSMenuItem::class(), separatorItem];
        let _: () = msg_send![&menu, addItem: &*sep2];

        let m2 = menu_item("退出 winflow", Some(sel!(quit:)), "");
        let _: () = msg_send![&m2, setTarget: &*target];
        let _: () = msg_send![&menu, addItem: &*m2];

        let _: () = msg_send![&item, setMenu: &*menu];

        // Self-check: confirm action selectors resolve.
        let ok1: bool = msg_send![&target, respondsToSelector: sel!(openConfigPanel:)];
        let ok2: bool = msg_send![&target, respondsToSelector: sel!(quit:)];

        // Keep alive for process lifetime.
        let _ = KEEP_ITEM.set(ffi::RawPtr(Retained::into_raw(item) as *const _));
        let _ = KEEP_TARGET.set(ffi::RawPtr(Retained::into_raw(target) as *const _));

        util::log(&format!("menu bar icon installed (actions ok: {} {})", ok1, ok2));
    }
}

fn menu_item(title: &str, action: Option<Sel>, key: &str) -> Retained<NSMenuItem> {
    unsafe {
        let ns_title = util::ns_string(title);
        let ns_key = util::ns_string(key);
        let mtm = objc2::MainThreadMarker::new().expect("menu built on main thread");
        NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &*ns_title, action, &*ns_key)
    }
}

/// Load an SF Symbol; returns None when the symbol name is invalid.
fn system_symbol_icon(name: &str) -> Option<Retained<NSImage>> {
    let ns = util::ns_string(name);
    NSImage::imageWithSystemSymbolName_accessibilityDescription(&ns, Some(&util::ns_string("winflow")))
}

/// Draw a tiny grid icon (fallback when SF Symbols are unavailable).
fn fallback_icon() -> Retained<NSImage> {
    unsafe {
        let img = NSImage::initWithSize(NSImage::alloc(), NSSize::new(18.0, 18.0));

        let _: () = msg_send![&img, lockFocus];
        let black = NSColor::blackColor();
        let _: () = msg_send![&black, setFill];
        for (x, y) in [(1.0, 10.0), (9.5, 10.0), (1.0, 1.5), (9.5, 1.5)] {
            let p: Retained<NSBezierPath> = msg_send![
                NSBezierPath::class(),
                bezierPathWithRoundedRect: NSRect::new(NSPoint::new(x, y), NSSize::new(7.5, 6.5)),
                xRadius: 2.0,
                yRadius: 2.0
            ];
            let _: () = msg_send![&p, fill];
        }
        let _: () = msg_send![&img, unlockFocus];
        let _: () = msg_send![&img, setTemplate: true];
        img
    }
}

// ---------------------------------------------------------------------------
// Settings panel
// ---------------------------------------------------------------------------

/// Shared panel state (raw pointers, main-thread only). The window is created
/// lazily on first open and reused; closing hides it (never deallocates).
struct PanelState {
    window: ffi::RawPtr,  // *const NSWindow, main-thread only
    field: ffi::RawPtr,   // *const NSTextField (capture interval), main-thread only
    field2: ffi::RawPtr,  // *const NSTextField (quick-tap delay), main-thread only
}

static PANEL: std::sync::OnceLock<std::sync::Mutex<Option<PanelState>>> = std::sync::OnceLock::new();

fn panel_state() -> std::sync::MutexGuard<'static, Option<PanelState>> {
    PANEL.get_or_init(|| std::sync::Mutex::new(None)).lock().unwrap()
}

fn open_config_panel() {
    unsafe {
        let mut st = panel_state();
        if let Some(p) = st.as_ref() {
            let win = &*(p.window.get() as *const NSWindow);
            let _: () = msg_send![&*win, makeKeyAndOrderFront: None::<&NSObject>];
            let app = NSApplication::sharedApplication(objc2::MainThreadMarker::new().expect("main"));
            let _: () = msg_send![&app, activateIgnoringOtherApps: true];
            return;
        }

        let mtm = objc2::MainThreadMarker::new().expect("main");

        // ---- window ----
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(300.0, 210.0));
        let win: Retained<NSWindow> = msg_send![
            NSWindow::alloc(mtm),
            initWithContentRect: rect,
            styleMask: NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            backing: NSBackingStoreType::Buffered,
            defer: false
        ];
        let _: () = msg_send![&win, setTitle: &*util::ns_string("winflow 设置")];
        let _: () = msg_send![&win, setReleasedWhenClosed: false];

        // ---- row 1: capture interval ----
        let label = NSTextField::labelWithString(&util::ns_string("缩略图更新间隔（秒）"), mtm);
        let _: () = msg_send![&label, setFrame: NSRect::new(NSPoint::new(20.0, 158.0), NSSize::new(260.0, 22.0))];
        let current = crate::overlay::current_capture_interval();
        let field = NSTextField::textFieldWithString(&util::ns_string(&format!("{}", current)), mtm);
        let _: () = msg_send![&field, setFrame: NSRect::new(NSPoint::new(20.0, 126.0), NSSize::new(260.0, 26.0))];
        let _: () = msg_send![&field, setBezeled: true];
        let _: () = msg_send![&field, setDrawsBackground: true];
        let _: () = msg_send![&field, setEditable: true];

        // ---- row 2: quick-tap delay ----
        let label2 = NSTextField::labelWithString(&util::ns_string("快捷键判定延迟（秒，0.05–2.0）"), mtm);
        let _: () = msg_send![&label2, setFrame: NSRect::new(NSPoint::new(20.0, 94.0), NSSize::new(260.0, 22.0))];
        let current2 = crate::overlay::current_quick_delay();
        let field2 = NSTextField::textFieldWithString(
            &util::ns_string(&format!("{:.2}", current2 as f64 / 1000.0)),
            mtm,
        );
        let _: () = msg_send![&field2, setFrame: NSRect::new(NSPoint::new(20.0, 62.0), NSSize::new(260.0, 26.0))];
        let _: () = msg_send![&field2, setBezeled: true];
        let _: () = msg_send![&field2, setDrawsBackground: true];
        let _: () = msg_send![&field2, setEditable: true];

        // ---- buttons ----
        let target: Retained<PanelTarget> = msg_send![PanelTarget::class(), new];

        let ok = NSButton::buttonWithTitle_target_action(
            &util::ns_string("确定"),
            Some(&target),
            Some(sel!(okClicked:)),
            mtm,
        );
        let _: () = msg_send![&ok, setBezelStyle: NSBezelStyle::Push];
        let _: () = msg_send![&ok, setFrame: NSRect::new(NSPoint::new(196.0, 12.0), NSSize::new(84.0, 30.0))];
        let _: () = msg_send![&ok, setKeyEquivalent: &*util::ns_string("\r")];

        let cancel = NSButton::buttonWithTitle_target_action(
            &util::ns_string("取消"),
            Some(&target),
            Some(sel!(cancelClicked:)),
            mtm,
        );
        let _: () = msg_send![&cancel, setBezelStyle: NSBezelStyle::Push];
        let _: () = msg_send![&cancel, setFrame: NSRect::new(NSPoint::new(104.0, 12.0), NSSize::new(84.0, 30.0))];
        let _: () = msg_send![&cancel, setKeyEquivalent: &*util::ns_string("\u{1b}")];

        // ---- assemble ----
        let content = win.contentView().expect("content view");
        let _: () = msg_send![&content, addSubview: &*label];
        let _: () = msg_send![&content, addSubview: &*field];
        let _: () = msg_send![&content, addSubview: &*label2];
        let _: () = msg_send![&content, addSubview: &*field2];
        let _: () = msg_send![&content, addSubview: &*ok];
        let _: () = msg_send![&content, addSubview: &*cancel];

        let _: () = msg_send![&win, center];
        let _: () = msg_send![&win, makeKeyAndOrderFront: None::<&NSObject>];
        let app = NSApplication::sharedApplication(mtm);
        let _: () = msg_send![&app, activateIgnoringOtherApps: true];

        let visible: bool = msg_send![&win, isVisible];
        let f: NSRect = msg_send![&win, frame];
        util::log(&format!(
            "settings panel shown (visible={}, frame={:.0}x{:.0} at {:.0},{:.0})",
            visible, f.size.width, f.size.height, f.origin.x, f.origin.y
        ));

        // Keep the panel target (weak by default) and the window alive.
        let _ = KEEP_PANEL_TARGET.set(ffi::RawPtr(Retained::into_raw(target) as *const _));
        *st = Some(PanelState {
            window: ffi::RawPtr(Retained::into_raw(win) as *const _),
            field: ffi::RawPtr(Retained::into_raw(field) as *const _),
            field2: ffi::RawPtr(Retained::into_raw(field2) as *const _),
        });
    }
}

/// Dev aid: open the settings panel directly (also used by `--panel`).
pub fn open_panel_for_test() {
    open_config_panel();
}

fn close_config_panel() {
    let st = panel_state();
    if let Some(p) = st.as_ref() {
        unsafe {
            let win = &*(p.window.get() as *const NSWindow);
            let _: () = msg_send![&*win, orderOut: None::<&NSObject>];
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "WinflowMenuTarget"]
    struct MenuTarget;

    unsafe impl NSObjectProtocol for MenuTarget {}

    impl MenuTarget {
        #[unsafe(method(openConfigPanel:))]
        fn open_config_panel(&self, _sender: Option<&AnyObject>) {
            open_config_panel();
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            unsafe {
                let mtm = objc2::MainThreadMarker::new().expect("menu actions run on main thread");
                let app = NSApplication::sharedApplication(mtm);
                let _: () = msg_send![&app, terminate: None::<&NSObject>];
            }
        }
    }
);

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "WinflowPanelTarget"]
    struct PanelTarget;

    unsafe impl NSObjectProtocol for PanelTarget {}

    impl PanelTarget {
        #[unsafe(method(okClicked:))]
        fn ok_clicked(&self, _sender: Option<&AnyObject>) {
            let (window, field, field2) = {
                let st = panel_state();
                let Some(p) = st.as_ref() else { return };
                (p.window.get(), p.field.get(), p.field2.get())
            };
            unsafe {
                // Capture interval (whole seconds).
                let field = &*(field as *const NSTextField);
                let v1: Retained<objc2_foundation::NSString> = msg_send![field, stringValue];
                match util::ns_string_to_rust(&v1).trim().parse::<u64>() {
                    Ok(secs) => crate::overlay::set_capture_interval(secs),
                    Err(_) => util::log("invalid interval value, keeping current"),
                }
                // Quick-tap delay (seconds, decimal allowed).
                let field2 = &*(field2 as *const NSTextField);
                let v2: Retained<objc2_foundation::NSString> = msg_send![field2, stringValue];
                match util::ns_string_to_rust(&v2).trim().parse::<f64>() {
                    Ok(secs) => crate::overlay::set_quick_delay((secs * 1000.0) as u64),
                    Err(_) => util::log("invalid delay value, keeping current"),
                }
                // Persist the APPLIED values (the setters clamp), so the
                // settings survive a restart.
                crate::config::save(
                    crate::overlay::current_capture_interval(),
                    crate::overlay::current_quick_delay(),
                );
                let win = &*(window as *const NSWindow);
                let _: () = msg_send![&*win, orderOut: None::<&NSObject>];
            }
        }

        #[unsafe(method(cancelClicked:))]
        fn cancel_clicked(&self, _sender: Option<&AnyObject>) {
            close_config_panel();
        }
    }
);
