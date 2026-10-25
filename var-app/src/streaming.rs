//! Live stream of one camera over SRT, e.g. into OBS. The stream runs independently
//! of the recording; see `var_record::stream`.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{error, info};
use var_capture::AttachedBranch;
use var_record::EncoderInfo;
use var_record::stream::{self, StreamSettings, StreamState, Streamer};

use crate::live::LiveCamera;

/// A detached tap is removed from its pipeline after this long (the idle probe that
/// unlinks it runs between two frames).
const REMOVE_AFTER: Duration = Duration::from_millis(300);

/// Stream settings, saved in the configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamConfig {
    /// `srt://host:port` or `srt://:port?mode=listener`.
    pub url: String,
    pub latency_ms: u32,
    /// Encryption passphrase (10–79 characters); empty: no encryption.
    pub passphrase: String,
    pub bitrate_mbps: f32,
    /// Camera slot (0-based).
    pub camera: usize,
}

impl Default for StreamConfig {
    fn default() -> Self {
        StreamConfig {
            url: String::new(),
            latency_ms: 120,
            passphrase: String::new(),
            bitrate_mbps: 8.0,
            camera: 0,
        }
    }
}

/// What the menu bar and the Stream menu show.
pub enum StreamStatus {
    Off,
    Connecting,
    WaitingForReceiver,
    Streaming { bitrate: f64 },
    Retrying(String),
}

struct Tap {
    slot: usize,
    generation: u64,
    branch: AttachedBranch,
}

pub struct StreamControl {
    pub settings: StreamConfig,
    streamer: Option<Streamer>,
    tap: Option<Tap>,
    /// Detached taps waiting to be removed.
    detached: Vec<(Tap, Instant)>,
    /// Why the last start failed.
    pub start_error: Option<String>,
    /// SRT is in this GStreamer (missing e.g. in GStreamer for Windows on ARM).
    pub available: bool,
}

impl StreamControl {
    pub fn new(settings: StreamConfig) -> Self {
        StreamControl {
            settings,
            streamer: None,
            tap: None,
            detached: Vec::new(),
            start_error: None,
            available: stream::available(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.streamer.is_some()
    }

    /// Starts streaming the selected camera.
    pub fn start(&mut self, encoder: Option<&EncoderInfo>, cameras: &[LiveCamera]) {
        self.start_error = None;
        let result = (|| {
            let encoder = encoder.ok_or("No H.264 encoder available")?;
            let camera = cameras
                .get(self.settings.camera)
                .ok_or("The selected camera doesn't exist")?;
            let selection = camera
                .selection()
                .ok_or("No device selected for this camera")?;
            let (num, den) = selection.mode.capture_framerate();
            let settings = StreamSettings {
                url: self.settings.url.trim().to_owned(),
                latency_ms: self.settings.latency_ms,
                passphrase: self.settings.passphrase.clone(),
                bitrate_kbps: (self.settings.bitrate_mbps * 1000.0).round() as u32,
                fps: (num.max(1) / den.max(1)) as u32,
            };
            settings.validate()?;
            Streamer::start(encoder, settings).map_err(|e| e.to_string())
        })();
        match result {
            Ok(streamer) => {
                info!(
                    camera = self.settings.camera + 1,
                    url = %self.settings.url,
                    "streaming started"
                );
                self.streamer = Some(streamer);
            }
            Err(e) => {
                error!(%e, "cannot start streaming");
                self.start_error = Some(e);
            }
        }
    }

    pub fn stop(&mut self, cameras: &[LiveCamera]) {
        if self.streamer.take().is_some() {
            info!("streaming stopped");
        }
        self.detach(cameras);
    }

    /// Keeps the tap on the selected camera's current pipeline (it changes when the
    /// camera restarts after a failure) and removes detached taps. Call once per frame.
    pub fn update(&mut self, cameras: &[LiveCamera]) {
        // Detached taps: removed once unlinked, if their pipeline still exists.
        let now = Instant::now();
        let (due, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.detached)
            .into_iter()
            .partition(|(_, since)| now.duration_since(*since) >= REMOVE_AFTER);
        self.detached = waiting;
        for (tap, _) in due {
            if let Some((camera, _, generation)) = cameras.get(tap.slot).and_then(|c| c.camera())
                && generation == tap.generation
            {
                camera.remove(tap.branch);
            }
        }

        let Some(streamer) = &self.streamer else {
            return;
        };
        let current = cameras
            .get(self.settings.camera)
            .and_then(|c| c.camera())
            .map(|(camera, _, generation)| (camera, generation));
        let attached_to = self.tap.as_ref().map(|t| (t.slot, t.generation));
        let wanted = current.map(|(_, generation)| (self.settings.camera, generation));
        if attached_to == wanted {
            return;
        }
        // The new tap first: detaching the old one needs `self` mutably.
        let tap = current.map(|_| streamer.tap().map_err(|e| e.to_string()));
        self.detach(cameras);
        if let (Some((camera, generation)), Some(tap)) = (current, tap) {
            match tap.and_then(|bin| camera.attach(&bin).map_err(|e| e.to_string())) {
                Ok(branch) => {
                    info!(
                        camera = self.settings.camera + 1,
                        "stream attached to camera"
                    );
                    self.tap = Some(Tap {
                        slot: self.settings.camera,
                        generation,
                        branch,
                    });
                }
                Err(e) => error!(%e, "cannot attach the stream to the camera"),
            }
        }
    }

    /// Unlinks the tap from its camera; it is removed later in `update`.
    fn detach(&mut self, cameras: &[LiveCamera]) {
        let Some(tap) = self.tap.take() else { return };
        if let Some((camera, _, generation)) = cameras.get(tap.slot).and_then(|c| c.camera())
            && generation == tap.generation
        {
            camera.detach(&tap.branch);
            self.detached.push((tap, Instant::now()));
        }
        // Otherwise the camera's pipeline is gone, and the tap with it.
    }

    pub fn status(&self) -> StreamStatus {
        match &self.streamer {
            None => StreamStatus::Off,
            Some(streamer) => match streamer.state() {
                StreamState::Connecting => StreamStatus::Connecting,
                StreamState::WaitingForReceiver => StreamStatus::WaitingForReceiver,
                StreamState::Streaming => StreamStatus::Streaming {
                    bitrate: streamer.bitrate(),
                },
                StreamState::Error(e) => StreamStatus::Retrying(e),
            },
        }
    }
}
