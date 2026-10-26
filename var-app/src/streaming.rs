//! Stream of one camera over SRT, e.g. into OBS. It follows the screen: the live
//! picture, or in review the review playback (paused, slow motion, stepping as on
//! screen); the camera shown in full screen, or in the split view the camera chosen
//! in the Stream menu. The stream runs independently of the recording; see
//! `var_record::stream`.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{error, info};
use var_capture::AttachedBranch;
use var_playback::ReviewFrame;
use var_record::EncoderInfo;
use var_record::stream::{self, StreamSettings, StreamState, Streamer};

use crate::live::LiveCamera;
use crate::review::ReviewMode;

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
    /// Camera slot (0-based) streamed in the split view.
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

impl StreamConfig {
    /// The Input to enter in OBS's Media Source for this SRT URL: OBS listens when
    /// this computer connects to it, and connects to `local` (this computer's
    /// address) when this computer listens. Includes the passphrase if set; with
    /// `reveal` false it is shown as `***`. `None` without a valid port in the URL.
    pub fn obs_input_url(&self, local: Option<IpAddr>, reveal: bool) -> Option<String> {
        let rest = self.url.trim().strip_prefix("srt://")?;
        let (authority, query) = rest.split_once('?').unwrap_or((rest, ""));
        let port: u16 = authority.rsplit_once(':')?.1.parse().ok()?;
        let listener = query.split('&').any(|option| option == "mode=listener");
        let mut url = if listener {
            let host = match local {
                Some(IpAddr::V6(ip)) => format!("[{ip}]"),
                Some(ip) => ip.to_string(),
                None => "<this computer>".to_owned(),
            };
            format!("srt://{host}:{port}")
        } else {
            format!("srt://0.0.0.0:{port}?mode=listener")
        };
        if !self.passphrase.is_empty() {
            url.push(if listener { '?' } else { '&' });
            url.push_str("passphrase=");
            if reveal {
                url.push_str(&percent_encode(&self.passphrase));
            } else {
                url.push_str("***");
            }
        }
        Some(url)
    }
}

/// Encodes all but the URL's unreserved characters, so a passphrase with `&`, `#`
/// or spaces survives as one query value.
fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
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
    /// The camera slot streamed now.
    pub shown: usize,
    /// The review frame streamed now; `None`: the live picture.
    review_frame: Option<Arc<ReviewFrame>>,
}

impl StreamControl {
    pub fn new(settings: StreamConfig) -> Self {
        let settings_camera = settings.camera;
        StreamControl {
            settings,
            streamer: None,
            tap: None,
            detached: Vec::new(),
            start_error: None,
            available: stream::available(),
            shown: settings_camera,
            review_frame: None,
        }
    }

    /// The stream shows review playback (not the live picture).
    pub fn is_review(&self) -> bool {
        self.review_frame.is_some()
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
            // Size and frame rate of this camera; other cameras' pictures are scaled.
            let (num, den) = selection.mode.capture_framerate();
            let settings = StreamSettings {
                url: self.settings.url.trim().to_owned(),
                latency_ms: self.settings.latency_ms,
                passphrase: self.settings.passphrase.clone(),
                bitrate_kbps: (self.settings.bitrate_mbps * 1000.0).round() as u32,
                fps: (num.max(1) / den.max(1)) as u32,
                width: selection.mode.width,
                height: selection.mode.height,
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
        self.review_frame = None;
        self.detach(cameras);
    }

    /// Follows the screen: streams camera `full_screen` (or in the split view the
    /// chosen camera), live or, while `review` is given, its review frame. Keeps the
    /// tap on that camera's current pipeline (it changes when the camera restarts after
    /// a failure) and removes detached taps. Call once per frame.
    pub fn update(
        &mut self,
        cameras: &[LiveCamera],
        full_screen: Option<usize>,
        review: Option<&ReviewMode>,
    ) {
        let slot = full_screen
            .filter(|&slot| slot < cameras.len())
            .unwrap_or(self.settings.camera);
        if slot != self.shown && self.streamer.is_some() {
            info!(camera = slot + 1, "stream follows the screen");
        }
        self.shown = slot;
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
        match review {
            // The review frame on screen. Until the camera has one (loading) or where it
            // has none (no video at the position), the stream keeps the last picture.
            Some(review) => {
                if let Some(frame) = review.frame(slot)
                    && !self
                        .review_frame
                        .as_ref()
                        .is_some_and(|shown| Arc::ptr_eq(shown, &frame))
                {
                    let (buffer, caps) = frame.to_buffer();
                    streamer.show_frame(buffer, caps);
                    self.review_frame = Some(frame);
                }
            }
            None => {
                if self.review_frame.take().is_some() {
                    streamer.show_live();
                }
            }
        }

        // The live tap stays on the shown camera also in review, so the stream returns
        // to live at once.
        let current = cameras
            .get(slot)
            .and_then(|c| c.camera())
            .map(|(camera, _, generation)| (camera, generation));
        let attached_to = self.tap.as_ref().map(|t| (t.slot, t.generation));
        let wanted = current.map(|(_, generation)| (slot, generation));
        if attached_to == wanted {
            return;
        }
        // The new tap first: detaching the old one needs `self` mutably.
        let tap = current.map(|_| streamer.tap().map_err(|e| e.to_string()));
        self.detach(cameras);
        if let (Some((camera, generation)), Some(tap)) = (current, tap) {
            match tap.and_then(|bin| camera.attach(&bin).map_err(|e| e.to_string())) {
                Ok(branch) => {
                    info!(camera = slot + 1, "stream attached to camera");
                    self.tap = Some(Tap {
                        slot,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn config(url: &str, passphrase: &str) -> StreamConfig {
        StreamConfig {
            url: url.to_owned(),
            passphrase: passphrase.to_owned(),
            ..StreamConfig::default()
        }
    }

    const LOCAL: Option<IpAddr> = Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 30)));

    #[test]
    fn obs_listens_when_sending_to_it() {
        let url = config("srt://192.168.1.20:9000", "");
        assert_eq!(
            url.obs_input_url(LOCAL, true).as_deref(),
            Some("srt://0.0.0.0:9000?mode=listener")
        );
        let url = config(
            " srt://obs-pc.local:9001?latency=80 ",
            "secret passphrase&1",
        );
        assert_eq!(
            url.obs_input_url(LOCAL, true).as_deref(),
            Some("srt://0.0.0.0:9001?mode=listener&passphrase=secret%20passphrase%261")
        );
        assert_eq!(
            url.obs_input_url(LOCAL, false).as_deref(),
            Some("srt://0.0.0.0:9001?mode=listener&passphrase=***")
        );
    }

    #[test]
    fn obs_connects_when_listening() {
        let url = config("srt://:9000?mode=listener", "0123456789");
        assert_eq!(
            url.obs_input_url(LOCAL, true).as_deref(),
            Some("srt://192.168.1.30:9000?passphrase=0123456789")
        );
        assert_eq!(
            url.obs_input_url(None, true).as_deref(),
            Some("srt://<this computer>:9000?passphrase=0123456789")
        );
    }

    #[test]
    fn no_obs_url_without_port() {
        for url in [
            "",
            "srt://",
            "srt://obs-pc",
            "udp://obs-pc:9000",
            "srt://obs-pc:x",
        ] {
            assert_eq!(config(url, "").obs_input_url(LOCAL, true), None, "{url}");
        }
    }
}
