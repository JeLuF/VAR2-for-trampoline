//! Review of the running recording for VAR for Trampoline: reading frames from the
//! growing segments via the live index, decoding, and the synchronised player with a
//! single review position (AD-6).

mod decoder;
mod player;
mod source;
mod ts;

pub use decoder::{DecodedFrame, Decoder};
pub use player::{CameraPlayer, CameraState, MAX_REVERSE_SPEED, ReviewFrame, ReviewPlayer, SPEEDS};
pub use source::{CameraSource, FrameRef, Packet};

/// Errors of the playback layer.
#[derive(Debug, thiserror::Error)]
pub enum PlaybackError {
    #[error("decoder: {0}")]
    Decoder(String),
}
