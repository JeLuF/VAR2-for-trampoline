//! Capture devices for VAR for Trampoline.
//!
//! Device enumeration with hot-plug updates ([`DeviceWatcher`]), the synthetic test
//! camera ([`synthetic`]) and the capture pipeline per camera ([`Camera`]).
//!
//! This crate has no UI dependency; the application is notified of changes through
//! callbacks.

mod camera;
mod device;
mod monitor;
mod power;
pub use power::raise_priority;
pub mod synthetic;

pub use camera::{AttachedBranch, Camera, CameraStats, PreviewFrame, yuv_colorimetry};
pub use device::{DeviceInfo, DeviceKind, Framerate, Mode, PixelFormat};
pub use monitor::{DeviceWatcher, WatcherOptions};
pub use var_core::color::{YuvColorimetry, YuvMatrix};

use gstreamer as gst;

/// Errors of the capture layer.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("failed to initialise GStreamer: {0}")]
    Init(#[from] gst::glib::Error),
    #[error("{0}")]
    Pipeline(String),
}

/// Initialises GStreamer and opts the process out of OS power throttling. Must be
/// called before anything else in this crate.
pub fn init() -> Result<(), CaptureError> {
    power::disable_power_throttling();
    gst::init()?;
    tracing::info!(version = %gst::version_string(), "GStreamer initialised");
    Ok(())
}

/// Version string of the GStreamer runtime in use, e.g. `GStreamer 1.28.6`.
pub fn gstreamer_version() -> String {
    gst::version_string().to_string()
}
