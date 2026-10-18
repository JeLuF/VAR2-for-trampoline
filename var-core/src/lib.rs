//! Shared core of VAR for Trampoline.
//!
//! Holds everything that is used by several crates and has no dependency on capture,
//! recording or the UI: application paths, logging, the index format, frame drop
//! detection, colorimetry and the process's resource use.

pub mod color;
pub mod index;
pub mod logging;
pub mod paths;
pub mod resources;
pub mod runtime;
pub mod timing;

/// Human-readable application name, used for window titles and directories.
pub const APP_NAME: &str = "VAR for Trampoline";

/// Application version from the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
