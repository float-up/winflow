//! Runtime configuration.
//!
//! Defaults come from `Config::default()`; the two panel-adjustable settings
//! (thumbnail capture interval, quick-tap delay) are persisted to a small
//! text file at `~/Library/Application Support/winflow/settings.conf`
//! (`key=value` lines, no serde) and merged over the defaults at startup.
//! Other fields are not exposed in the panel and keep their defaults.

use std::path::PathBuf;
use std::time::Duration;

pub const DEFAULT_CAPTURE_INTERVAL_SECS: u64 = 45;
pub const DEFAULT_QUICK_DELAY_MS: u64 = 80;

#[derive(Clone, Debug)]
pub struct Config {
    /// Background thumbnail capture interval.
    pub capture_interval: Duration,
    /// Quick-tap judgment delay: releasing the hotkey modifier within this
    /// window switches straight to the previous window; holding longer shows
    /// the thumbnail switcher for manual selection.
    pub quick_delay: Duration,
    /// Thumbnail height in points.
    pub thumb_height: f64,
    /// Crispness multiplier over the display's backing scale factor
    /// (1.0 = 1:1 pixels at the display scale; >1 oversamples for extra
    /// sharpness at the cost of memory).
    pub thumb_px_scale: f64,
    /// Overlay max width as a fraction of screen width.
    pub max_width_frac: f64,
    pub min_window_w: f64,
    pub min_window_h: f64,
    /// Extra terms (owner or title substring) to filter out.
    pub filter_terms: Vec<String>,
    /// Wrap-around navigation at grid edges.
    pub wrap: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            capture_interval: Duration::from_secs(DEFAULT_CAPTURE_INTERVAL_SECS),
            quick_delay: Duration::from_millis(DEFAULT_QUICK_DELAY_MS),
            thumb_height: 200.0,
            thumb_px_scale: 1.0,
            max_width_frac: 0.8,
            min_window_w: 120.0,
            min_window_h: 80.0,
            filter_terms: Vec::new(),
            wrap: true,
        }
    }
}

/// Path of the persisted settings file. Overridable via `WINFLOW_CONFIG_FILE`
/// (used by tests and for custom setups).
fn settings_path() -> PathBuf {
    if let Ok(p) = std::env::var("WINFLOW_CONFIG_FILE") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(&home)
        .join("Library/Application Support/winflow/settings.conf")
}

/// Persisted settings file key used for the capture interval (seconds).
const KEY_INTERVAL: &str = "capture_interval";
/// Persisted settings file key used for the quick-tap delay (milliseconds).
const KEY_QUICK_DELAY: &str = "quick_delay";

/// Load persisted settings over the defaults. Missing/corrupt entries keep
/// the defaults; out-of-range values are clamped to the panel's ranges.
pub fn load() -> Config {
    let mut cfg = Config::default();
    let Ok(text) = std::fs::read_to_string(settings_path()) else {
        return cfg;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        match (k.trim(), v.trim()) {
            (KEY_INTERVAL, s) => {
                if let Ok(secs) = s.parse::<u64>() {
                    cfg.capture_interval = Duration::from_secs(secs.clamp(1, 3600));
                }
            }
            (KEY_QUICK_DELAY, s) => {
                if let Ok(ms) = s.parse::<u64>() {
                    cfg.quick_delay = Duration::from_millis(ms.clamp(50, 2000));
                }
            }
            _ => {}
        }
    }
    cfg
}

/// Persist the two panel-adjustable settings (best-effort; failures are
/// ignored — settings just won't survive a restart).
pub fn save(capture_interval_secs: u64, quick_delay_ms: u64) {
    let path = settings_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let body = format!(
        "# winflow settings (edited via the menu-bar panel)\n\
         # capture_interval: thumbnail capture interval in seconds (1-3600)\n\
         # quick_delay: hotkey judgment delay in milliseconds (50-2000)\n\
         {KEY_INTERVAL}={}\n\
         {KEY_QUICK_DELAY}={}\n",
        capture_interval_secs.clamp(1, 3600),
        quick_delay_ms.clamp(50, 2000),
    );
    let _ = std::fs::write(path, body);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip_and_clamping() {
        let dir = std::env::temp_dir().join(format!("winflow_cfg_test_{}", std::process::id()));
        let path = dir.join("settings.conf");
        std::env::set_var("WINFLOW_CONFIG_FILE", &path);

        // 1) Missing file -> defaults.
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = load();
        assert_eq!(cfg.capture_interval, Duration::from_secs(DEFAULT_CAPTURE_INTERVAL_SECS));
        assert_eq!(cfg.quick_delay, Duration::from_millis(DEFAULT_QUICK_DELAY_MS));

        // 2) Save + load roundtrip.
        std::fs::create_dir_all(&dir).unwrap();
        save(25, 300);
        let cfg = load();
        assert_eq!(cfg.capture_interval, Duration::from_secs(25));
        assert_eq!(cfg.quick_delay, Duration::from_millis(300));
        assert_eq!(cfg.thumb_height, 200.0, "unexposed fields keep defaults");

        // 3) Out-of-range and garbage lines are clamped/ignored.
        std::fs::write(&path, "capture_interval=99999\nquick_delay=1\nbogus=xyz\n").unwrap();
        let cfg = load();
        assert_eq!(cfg.capture_interval, Duration::from_secs(3600));
        assert_eq!(cfg.quick_delay, Duration::from_millis(50));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
