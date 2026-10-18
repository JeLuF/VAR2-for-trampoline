//! Configuration, saved when it changes and restored on start: cameras and modes,
//! recording folder, segment length, view options.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use var_capture::Mode;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Number of camera slots shown (1–4).
    pub camera_count: Option<usize>,
    /// Per camera slot: the selected device and mode.
    pub cameras: Vec<Option<CameraConfig>>,
    pub recording_folder: Option<String>,
    pub segment_minutes: Option<u32>,
    pub debug_overlay: bool,
    /// The window covers the whole screen (no title bar, no taskbar).
    pub window_full_screen: bool,
    /// Laser pointer trails fade out after this many seconds (DRW-002).
    pub laser_seconds: Option<f32>,
    /// Disk space warnings below these remaining recording times, minutes (DSK-003).
    pub disk_warning_minutes: Option<[u32; 2]>,
    /// Argon2 hash of the PIN for unlocking (ROLE-006). Never the PIN itself.
    pub pin_hash: Option<String>,
    /// Remote control for Bitfocus Companion (INP-003); off by default.
    pub remote_control: Option<RemoteControlConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteControlConfig {
    pub enabled: bool,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraConfig {
    /// Stable device ID (CAP-007).
    pub device_id: String,
    pub device_name: String,
    pub mode: Mode,
}

/// Loads the configuration; a missing or unreadable file gives the defaults.
pub fn load(path: &Path) -> Config {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            info!(path = %path.display(), "no configuration yet, using defaults");
            return Config::default();
        }
        Err(e) => {
            warn!(path = %path.display(), %e, "cannot read the configuration, using defaults");
            return Config::default();
        }
    };
    match serde_json::from_str(&text) {
        Ok(config) => {
            info!(path = %path.display(), "configuration loaded");
            config
        }
        Err(e) => {
            warn!(path = %path.display(), %e, "invalid configuration, using defaults");
            Config::default()
        }
    }
}

/// Saves the configuration: written to a temporary file and renamed, so a crash never
/// leaves a half-written file.
pub fn save(path: &Path, config: &Config) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, serde_json::to_string_pretty(config)?)?;
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use var_capture::{Framerate, PixelFormat};

    #[test]
    fn round_trip() {
        let path =
            std::env::temp_dir().join(format!("var-config-test-{}.json", std::process::id()));
        let config = Config {
            camera_count: Some(2),
            cameras: vec![
                Some(CameraConfig {
                    device_id: r"\\?\usb#vid_33f1&pid_1097#{e5323777}\global".into(),
                    device_name: "UHD Camera".into(),
                    mode: Mode {
                        format: PixelFormat::Mjpeg,
                        width: 1920,
                        height: 1080,
                        framerate: Framerate::Fixed { num: 50, den: 1 },
                    },
                }),
                None,
            ],
            recording_folder: Some(r"D:\VAR".into()),
            segment_minutes: Some(10),
            debug_overlay: true,
            window_full_screen: true,
            laser_seconds: Some(5.0),
            disk_warning_minutes: Some([90, 20]),
            pin_hash: Some("$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaA".into()),
            remote_control: Some(RemoteControlConfig {
                enabled: true,
                port: 7450,
            }),
        };
        save(&path, &config).unwrap();
        assert_eq!(load(&path), config);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn missing_or_broken_file_gives_defaults() {
        let path = std::env::temp_dir().join("var-config-test-missing.json");
        assert_eq!(load(&path), Config::default());
        let broken =
            std::env::temp_dir().join(format!("var-config-broken-{}.json", std::process::id()));
        std::fs::write(&broken, "{ not json").unwrap();
        assert_eq!(load(&broken), Config::default());
        std::fs::remove_file(&broken).unwrap();
    }
}
