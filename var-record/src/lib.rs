//! Recording for VAR for Trampoline: H.264 encoding, the recording branch attached to a
//! camera's capture pipeline, and the segment writer with its own MPEG-TS muxer and
//! per-segment index, and the free disk space query.

mod branch;
pub mod disk;
mod encoder;
mod segment;
pub mod ts;

pub use branch::{RecordingBranch, RecordingStats};
pub use encoder::{EncoderInfo, EncodingSettings, select_encoder};
pub use segment::{Anchor, SegmentConfig, SegmentWriter, segment_files};

/// Errors of the recording layer.
#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("{0}")]
    Pipeline(String),
}
