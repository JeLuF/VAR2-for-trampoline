//! Per-user application directories.

use std::path::PathBuf;

use directories::{ProjectDirs, UserDirs};

fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("", "", crate::APP_NAME)
}

/// Directory for log files: `logs` in the platform's local data directory for the
/// application (e.g. `%LOCALAPPDATA%\VAR for Trampoline\data\logs` on Windows).
/// Falls back to `./logs` if no home directory can be determined.
pub fn log_dir() -> PathBuf {
    project_dirs()
        .map(|dirs| dirs.data_local_dir().join("logs"))
        .unwrap_or_else(|| PathBuf::from("logs"))
}

/// The configuration file: `config.json` in the platform's configuration directory for
/// the application (e.g. `%APPDATA%\VAR for Trampoline\config\config.json` on Windows).
pub fn config_file() -> PathBuf {
    project_dirs()
        .map(|dirs| dirs.config_dir().join("config.json"))
        .unwrap_or_else(|| PathBuf::from("config.json"))
}

/// Default folder for recordings: `VAR` in the user's videos folder, else in the home
/// folder, else `./VAR`.
pub fn default_recording_dir() -> PathBuf {
    UserDirs::new()
        .and_then(|dirs| {
            dirs.video_dir()
                .map(|d| d.to_owned())
                .or_else(|| Some(dirs.home_dir().to_owned()))
        })
        .unwrap_or_default()
        .join("VAR")
}
