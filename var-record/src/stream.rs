//! Live stream of one camera over SRT, e.g. into OBS (Media Source).
//!
//! ```text
//! camera pipeline:  tee ─ tap: queue (leaky) ─ appsink
//!                                                │ frames handed over
//! stream pipeline:  appsrc (leaky) ─ videoconvert ─ H.264 encoder ─ h264parse
//!                   ─ mpegtsmux ─ srtsink
//! ```
//!
//! The stream runs in a pipeline of its own: a missing receiver, network errors or a
//! slow encoder never reach the camera pipeline, so recording and live view are not
//! affected. The tap only hands frames over (no copy) and drops them when the stream
//! falls behind. A thread of its own connects (which can block for seconds), watches
//! the stream and reconnects after errors.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use tracing::{info, warn};

use crate::RecordError;
use crate::encoder::{EncoderInfo, EncodingSettings, make_encoder};

/// After an error the stream is restarted after this long.
const RETRY_DELAY: Duration = Duration::from_secs(3);
/// Caller: without a connection for this long, the stream is restarted (the receiver
/// may have started meanwhile, or come back).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const SINK_NAME: &str = "srt";
/// Frames waiting in the tap and in the stream's input; more are dropped.
const MAX_QUEUED_FRAMES: u32 = 3;

/// What to stream where.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamSettings {
    /// `srt://host:port` (caller: connect to the receiver) or `srt://:port?mode=listener`
    /// (listener: the receiver connects). Further SRT options may follow in the query.
    pub url: String,
    /// SRT latency (receive buffer) in ms; the larger of sender's and receiver's applies.
    pub latency_ms: u32,
    /// Encryption passphrase, 10–79 characters; empty: no encryption.
    pub passphrase: String,
    pub bitrate_kbps: u32,
    /// Frame rate of the camera, for the keyframe interval (1 s).
    pub fps: u32,
}

impl StreamSettings {
    /// Checks the settings; the message is for the operator.
    pub fn validate(&self) -> Result<(), String> {
        let url = self.url.trim();
        let Some(rest) = url.strip_prefix("srt://") else {
            return Err("The URL must start with srt://".into());
        };
        let address = rest.split(['?', '/']).next().unwrap_or("");
        let port = address.rsplit(':').next().unwrap_or("");
        if !address.contains(':') || port.parse::<u16>().map_or(true, |p| p == 0) {
            return Err("The URL needs a port, e.g. srt://192.168.1.20:9000".into());
        }
        let host = &address[..address.len() - port.len() - 1];
        if host.is_empty() && !self.is_listener() {
            return Err("A host is needed, or ?mode=listener to wait for the receiver".into());
        }
        let length = self.passphrase.chars().count();
        if length != 0 && !(10..=79).contains(&length) {
            return Err("The passphrase needs 10 to 79 characters (or none)".into());
        }
        Ok(())
    }

    /// Listener mode: the receiver (OBS) connects to this computer.
    pub fn is_listener(&self) -> bool {
        self.url.to_ascii_lowercase().contains("mode=listener")
    }
}

/// State of the stream.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamState {
    /// Connecting to the receiver (caller) or starting.
    Connecting,
    /// Listener: waiting for the receiver to connect.
    WaitingForReceiver,
    Streaming,
    /// The last attempt failed; retrying.
    Error(String),
}

/// Whether this GStreamer has what streaming needs (the SRT plugin is missing in some
/// builds, e.g. GStreamer for Windows on ARM).
pub fn available() -> bool {
    ["srtsink", "mpegtsmux", "h264parse"]
        .iter()
        .all(|factory| gst::ElementFactory::find(factory).is_some())
}

struct Shared {
    /// The running stream's input; `None` while (re)connecting.
    source: Mutex<Option<gst_app::AppSrc>>,
    state: Mutex<StreamState>,
    /// Bytes handed to the SRT sink.
    bytes: AtomicU64,
    /// Listener: receivers connected.
    receivers: AtomicI32,
    stop: AtomicBool,
}

/// A running SRT stream. Stops when dropped.
pub struct Streamer {
    shared: Arc<Shared>,
    /// Bytes and time of the last bitrate measurement, and the bitrate (bit/s).
    rate: Mutex<(Instant, u64, f64)>,
}

impl Streamer {
    /// Starts streaming in the background. Frames come from [`tap`](Self::tap) bins
    /// attached to a camera.
    pub fn start(encoder: &EncoderInfo, settings: StreamSettings) -> Result<Self, RecordError> {
        settings.validate().map_err(RecordError::Pipeline)?;
        if !available() {
            return Err(RecordError::Pipeline(
                "SRT is not available in this GStreamer".into(),
            ));
        }
        let shared = Arc::new(Shared {
            source: Mutex::new(None),
            state: Mutex::new(StreamState::Connecting),
            bytes: AtomicU64::new(0),
            receivers: AtomicI32::new(0),
            stop: AtomicBool::new(false),
        });
        let encoder = encoder.clone();
        std::thread::Builder::new()
            .name("var-stream".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || run(&encoder, &settings, &shared)
            })
            .map_err(|e| RecordError::Pipeline(e.to_string()))?;
        Ok(Streamer {
            shared,
            rate: Mutex::new((Instant::now(), 0, 0.0)),
        })
    }

    /// A bin to attach to a camera's tee (`Camera::attach`): hands the camera's frames
    /// to the stream. It never blocks the camera: frames are dropped when the stream
    /// falls behind or is reconnecting.
    pub fn tap(&self) -> Result<gst::Bin, RecordError> {
        let err = |e: &dyn std::fmt::Display| RecordError::Pipeline(e.to_string());
        let queue = gst::ElementFactory::make("queue")
            .property("max-size-buffers", MAX_QUEUED_FRAMES)
            .property("max-size-bytes", 0u32)
            .property("max-size-time", 0u64)
            .build()
            .map_err(|e| err(&e))?;
        queue.set_property_from_str("leaky", "downstream");
        let shared = Arc::clone(&self.shared);
        let sink = gst_app::AppSink::builder()
            .sync(false)
            .async_(false)
            .max_buffers(1)
            .drop(true)
            .callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |sink| {
                        let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        let source = shared.source.lock().unwrap().clone();
                        if let (Some(source), Some(buffer), Some(caps)) =
                            (source, sample.buffer(), sample.caps())
                        {
                            if source.caps().as_ref() != Some(&caps.to_owned()) {
                                source.set_caps(Some(&caps.to_owned()));
                            }
                            // New timestamps from the stream's own clock; the data
                            // itself is shared, not copied.
                            let mut buffer = buffer.copy();
                            {
                                let buffer = buffer.get_mut().unwrap();
                                buffer.set_pts(gst::ClockTime::NONE);
                                buffer.set_dts(gst::ClockTime::NONE);
                            }
                            let _ = source.push_buffer(buffer);
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            )
            .build();
        let sink: gst::Element = sink.upcast();
        let bin = gst::Bin::with_name("stream");
        bin.add_many([&queue, &sink]).map_err(|e| err(&e))?;
        queue.link(&sink).map_err(|e| err(&e))?;
        let ghost =
            gst::GhostPad::with_target(&queue.static_pad("sink").unwrap()).map_err(|e| err(&e))?;
        ghost.set_active(true).map_err(|e| err(&e))?;
        bin.add_pad(&ghost).map_err(|e| err(&e))?;
        Ok(bin)
    }

    pub fn state(&self) -> StreamState {
        self.shared.state.lock().unwrap().clone()
    }

    /// Bitrate sent over about the last second, bit/s.
    pub fn bitrate(&self) -> f64 {
        let bytes = self.shared.bytes.load(Ordering::Relaxed);
        let mut rate = self.rate.lock().unwrap();
        let elapsed = rate.0.elapsed();
        if elapsed >= Duration::from_secs(1) {
            rate.2 = bytes.saturating_sub(rate.1) as f64 * 8.0 / elapsed.as_secs_f64();
            *rate = (Instant::now(), bytes, rate.2);
        }
        rate.2
    }
}

impl Drop for Streamer {
    fn drop(&mut self) {
        // The thread ends by itself; not waiting for it, as connecting can block.
        self.shared.stop.store(true, Ordering::Relaxed);
        *self.shared.source.lock().unwrap() = None;
    }
}

fn set_state(shared: &Shared, state: StreamState) {
    let mut current = shared.state.lock().unwrap();
    if *current != state {
        info!(?state, "stream");
        *current = state;
    }
}

/// The stream thread: build, connect, watch; after an error wait and start again.
fn run(encoder: &EncoderInfo, settings: &StreamSettings, shared: &Arc<Shared>) {
    info!(url = %settings.url, bitrate_kbps = settings.bitrate_kbps, latency_ms = settings.latency_ms, "stream starting");
    while !shared.stop.load(Ordering::Relaxed) {
        set_state(shared, StreamState::Connecting);
        let error = match build(encoder, settings, shared) {
            Ok((pipeline, source)) => {
                let error = play(&pipeline, &source, settings, shared);
                *shared.source.lock().unwrap() = None;
                let _ = pipeline.set_state(gst::State::Null);
                error
            }
            Err(e) => Some(e.to_string()),
        };
        let Some(error) = error else { break };
        if shared.stop.load(Ordering::Relaxed) {
            break;
        }
        warn!(%error, "stream error, retrying");
        set_state(shared, StreamState::Error(error));
        let until = Instant::now() + RETRY_DELAY;
        while Instant::now() < until && !shared.stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    info!("stream stopped");
}

/// Starts the pipeline and watches it until stopped (`None`) or an error (`Some`).
fn play(
    pipeline: &gst::Pipeline,
    source: &gst_app::AppSrc,
    settings: &StreamSettings,
    shared: &Shared,
) -> Option<String> {
    let bus = pipeline.bus()?;
    // Caller mode connects here; that can take seconds if the receiver is missing.
    if pipeline.set_state(gst::State::Playing).is_err() {
        return Some(bus_error(&bus).unwrap_or_else(|| "cannot connect".into()));
    }
    *shared.source.lock().unwrap() = Some(source.clone());
    let sink = pipeline.by_name(SINK_NAME)?;
    // Caller: since when there is no connection (the sink doesn't reconnect reliably
    // by itself, so the stream is restarted).
    let mut unconnected_since = Some(Instant::now());
    while !shared.stop.load(Ordering::Relaxed) {
        if let Some(error) = bus_error_timeout(&bus, Duration::from_millis(200)) {
            return Some(error);
        }
        if settings.is_listener() {
            let state = if shared.receivers.load(Ordering::Relaxed) > 0 {
                StreamState::Streaming
            } else {
                StreamState::WaitingForReceiver
            };
            set_state(shared, state);
        } else if connected(&sink) {
            unconnected_since = None;
            set_state(shared, StreamState::Streaming);
        } else {
            let since = *unconnected_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= CONNECT_TIMEOUT {
                return Some(format!("no connection to {}", settings.url.trim()));
            }
        }
    }
    None
}

/// Whether the SRT sink has a connection: its statistics then contain the socket's
/// counters; unconnected only `bytes-sent-total`.
fn connected(sink: &gst::Element) -> bool {
    sink.property::<gst::Structure>("stats")
        .has_field("packets-sent")
}

fn bus_error(bus: &gst::Bus) -> Option<String> {
    bus_error_timeout(bus, Duration::ZERO)
}

fn bus_error_timeout(bus: &gst::Bus, timeout: Duration) -> Option<String> {
    let message = bus.timed_pop_filtered(
        gst::ClockTime::from_nseconds(timeout.as_nanos() as u64),
        &[gst::MessageType::Error, gst::MessageType::Eos],
    )?;
    Some(match message.view() {
        gst::MessageView::Error(e) => match e.debug() {
            Some(debug) => format!("{} ({debug})", e.error()),
            None => e.error().to_string(),
        },
        _ => "the stream ended".into(),
    })
}

/// The stream pipeline: appsrc ─ videoconvert ─ encoder ─ h264parse ─ mpegtsmux ─ srtsink.
fn build(
    encoder: &EncoderInfo,
    settings: &StreamSettings,
    shared: &Arc<Shared>,
) -> Result<(gst::Pipeline, gst_app::AppSrc), RecordError> {
    let err = |e: &dyn std::fmt::Display| RecordError::Pipeline(e.to_string());
    let make = |factory: &str| {
        gst::ElementFactory::make(factory).build().map_err(|_| {
            RecordError::Pipeline(format!("GStreamer element `{factory}` is not available"))
        })
    };

    let source = gst_app::AppSrc::builder()
        .format(gst::Format::Time)
        .is_live(true)
        .do_timestamp(true)
        .block(false)
        .build();
    source.set_property("max-buffers", u64::from(MAX_QUEUED_FRAMES));
    // Limited by frames only (the default 200 kB is less than one HD frame).
    source.set_property("max-bytes", 0u64);
    if source.find_property("leaky-type").is_some() {
        source.set_property_from_str("leaky-type", "downstream");
    }
    let convert = make("videoconvert")?;
    // One keyframe per second: OBS shows the picture quickly after (re)connecting.
    let encoder_element = make_encoder(
        encoder,
        EncodingSettings {
            bitrate_kbps: settings.bitrate_kbps,
            gop_frames: settings.fps.max(1),
        },
    )?;
    for low_latency in ["low-latency", "zerolatency"] {
        if encoder_element
            .find_property(low_latency)
            .is_some_and(|p| p.value_type() == bool::static_type())
        {
            encoder_element.set_property(low_latency, true);
        }
    }
    let parse = make("h264parse")?;
    parse.set_property("config-interval", -1i32);
    let mux = make("mpegtsmux")?;
    // 7 × 188 bytes: one TS packet group per SRT packet.
    if mux.find_property("alignment").is_some() {
        mux.set_property("alignment", 7i32);
    }
    let sink = gst::ElementFactory::make("srtsink")
        .name(SINK_NAME)
        .build()
        .map_err(|_| {
            RecordError::Pipeline("GStreamer element `srtsink` is not available".into())
        })?;
    sink.set_property("uri", settings.url.trim());
    sink.set_property("latency", settings.latency_ms as i32);
    if !settings.passphrase.is_empty() {
        sink.set_property("passphrase", &settings.passphrase);
    }
    // Never block the stream (and with it the camera's frames) on the network.
    if sink.find_property("wait-for-connection").is_some() {
        sink.set_property("wait-for-connection", false);
    }
    sink.set_property("sync", false);
    sink.set_property("async", false);
    if settings.is_listener() {
        let added = Arc::clone(shared);
        sink.connect("caller-added", false, move |_| {
            added.receivers.fetch_add(1, Ordering::Relaxed);
            info!("stream receiver connected");
            None
        });
        let removed = Arc::clone(shared);
        sink.connect("caller-removed", false, move |_| {
            removed.receivers.fetch_sub(1, Ordering::Relaxed);
            info!("stream receiver disconnected");
            None
        });
    }
    // Count what is sent, for the bitrate shown (the muxer pushes buffer lists).
    if let Some(pad) = sink.static_pad("sink") {
        let counter = Arc::clone(shared);
        pad.add_probe(
            gst::PadProbeType::BUFFER | gst::PadProbeType::BUFFER_LIST,
            move |_, probe| {
                let size = match &probe.data {
                    Some(gst::PadProbeData::Buffer(buffer)) => buffer.size(),
                    Some(gst::PadProbeData::BufferList(list)) => list.calculate_size(),
                    _ => 0,
                };
                counter.bytes.fetch_add(size as u64, Ordering::Relaxed);
                gst::PadProbeReturn::Ok
            },
        );
    }

    let pipeline = gst::Pipeline::with_name("stream");
    let chain = [
        source.upcast_ref::<gst::Element>(),
        &convert,
        &encoder_element,
        &parse,
        &mux,
        &sink,
    ];
    pipeline.add_many(chain).map_err(|e| err(&e))?;
    gst::Element::link_many(chain).map_err(|e| err(&e))?;
    Ok((pipeline, source))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(url: &str, passphrase: &str) -> StreamSettings {
        StreamSettings {
            url: url.into(),
            latency_ms: 120,
            passphrase: passphrase.into(),
            bitrate_kbps: 8000,
            fps: 50,
        }
    }

    #[test]
    fn validates_settings() {
        assert!(settings("srt://192.168.1.20:9000", "").validate().is_ok());
        assert!(
            settings("srt://obs.local:9000?latency=200", "")
                .validate()
                .is_ok()
        );
        assert!(settings("srt://:9000?mode=listener", "").validate().is_ok());
        assert!(
            settings("srt://0.0.0.0:9000?mode=listener", "0123456789")
                .validate()
                .is_ok()
        );
        for bad in [
            "rtmp://host/live",
            "srt://192.168.1.20",
            "srt://192.168.1.20:0",
            "srt://192.168.1.20:x",
            "srt://:9000",
        ] {
            assert!(settings(bad, "").validate().is_err(), "{bad} accepted");
        }
        assert!(settings("srt://h:9000", "short").validate().is_err());
        assert!(settings(":9000?mode=listener", "").validate().is_err());
    }

    #[test]
    fn listener_mode_from_url() {
        assert!(settings("srt://:9000?mode=listener", "").is_listener());
        assert!(settings("srt://:9000?latency=50&MODE=LISTENER", "").is_listener());
        assert!(!settings("srt://host:9000", "").is_listener());
    }
}
