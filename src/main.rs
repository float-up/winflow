//! winflow — a lightweight macOS window switcher that enhances Cmd+Tab with
//! live window thumbnails, per-Space grouping and LRU ordering.
//!
//! Run it, grant Accessibility + Screen Recording permissions, then press
//! `⌘Tab` (all windows on the current Space) or `⌘\`` (current app's windows).

use winflow::capture;
use winflow::config;
use winflow::menubar;
use winflow::overlay;
use winflow::state;
use winflow::util;

use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

fn main() {
    util::init_log_file();
    // Load persisted settings (capture interval / quick-tap delay) over the
    // defaults, so panel changes survive restarts.
    let cfg = config::load();
    util::log("winflow started — 当前桌面窗口: ⌘Tab，当前程序窗口: ⌘` (菜单栏可退出)");

    let shared = Arc::new(Mutex::new(state::Core::new(cfg.wrap)));
    let thumbs: Arc<RwLock<capture::ThumbCache>> = Arc::new(RwLock::new(HashMap::new()));

    unsafe {
        let app = NSApplication::sharedApplication(mtm());
        // Menu-bar agent: no dock icon.
        let _: bool = objc2::msg_send![&app, setActivationPolicy: NSApplicationActivationPolicy::Accessory];
    }

    overlay::init_app(cfg.clone(), shared.clone(), thumbs.clone());
    state::init_cmd_timer();
    menubar::install();
    winflow::permissions::prompt_if_missing();

    // Debug aids (dev only):
    // - `winflow --show`  pops the space switcher once after 2s.
    // - `winflow --panel` opens the settings panel once after 2s.
    if std::env::args().any(|a| a == "--show") {
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(2));
            state::dispatch_main(state::MainCmd::Show(state::Mode::Space, false));
        });
    }
    if std::env::args().any(|a| a == "--force-perm-dialog") {
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(1));
            state::dispatch_main(state::MainCmd::PermDialog);
        });
    }
    if std::env::args().any(|a| a == "--panel") {
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(2));
            state::dispatch_main(state::MainCmd::OpenPanel);
        });
    }

    // Warm the thumbnail cache with the currently visible windows right away:
    // the capture scheduler only captures `core.tracked`, which is otherwise
    // empty until the first overlay show. Do this BEFORE starting capture so
    // the scheduler's first (~50ms) pass always sees a populated list — there
    // is no race with the initial `CGWindowList` query.
    {
        let items = winflow::windows::collect(&cfg, state::Mode::Space, std::process::id(), None, None);
        let mut core = shared.lock().unwrap();
        core.tracked = items.iter().map(|i| i.id).collect();
        core.refresh_all = true;
    }
    capture::start(
        cfg.clone(),
        shared.clone(),
        thumbs.clone(),
        (cfg.thumb_height * overlay::screen_scale() * cfg.thumb_px_scale).round() as usize,
    );
    state::start_tap(shared.clone());
    state::start_quick_timer(shared.clone());
    state::start_tick();

    unsafe {
        let app = NSApplication::sharedApplication(mtm());
        let _: () = objc2::msg_send![&app, run];
    }
}

fn mtm() -> objc2::MainThreadMarker {
    unsafe { objc2::MainThreadMarker::new_unchecked() }
}
