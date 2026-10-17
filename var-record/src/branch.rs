//! The recording branch: a bin that is attached to a camera pipeline's tee.
//!
//! ```text
//! queue ─ videoconvert ─ H.264 encoder ─ h264parse ─ appsink → segment writer
//! ```
//!
//! The application writes the MPEG-TS segments and their index itself (AD-3): a disk
//! error is recorded and stops the writing, but never propagates into the capture
//! pipeline, so capture and live view continue. MPEG-TS needs no finalisation, so a
//! segment stays playable if the application is killed (REC-012). There is no audio
//! (REC-013).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use tracing::{error, info};

use crate::RecordError;
use crate::encoder::{EncoderInfo, EncodingSettings, make_encoder};
use crate::segment::{SegmentConfig, SegmentWriter};

/// The recording branch for one camera.
pub struct RecordingBranch {
    bin: gst::Bin,
    dir: PathBuf,
    shared: Arc<Shared>,
}

struct Shared {
    stats: Mutex<StatsCounter>,
    /// `None` after a write error or after the end of the stream.
    writer: Mutex<Option<SegmentWriter>>,
    error: Mutex<Option<String>>,
    finished: Mutex<bool>,
    finished_cond: Condvar,
}

impl Shared {
    fn write(&self, au: &[u8], clock_ns: u64, keyframe: bool) {
        let mut writer = self.writer.lock().unwrap();
        let Some(w) = writer.as_mut() else { return };
        match w.write_frame(au, clock_ns, keyframe) {
            Ok(written) => {
                let segments = w.segments_completed();
                let disk_bytes = w.bytes_written();
                drop(writer);
                let mut stats = self.stats.lock().unwrap();
                stats.stats.segments = segments + 1;
                stats.stats.disk_bytes = disk_bytes;
                if written {
                    stats.on_frame(au.len(), keyframe, Instant::now());
                }
            }
            Err(e) => {
                *writer = None;
                drop(writer);
                self.fail(format!("cannot write the recording: {e}"));
            }
        }
    }

    /// Flushes and closes the current segment at the end of the stream.
    fn finish(&self) {
        if let Some(mut w) = self.writer.lock().unwrap().take()
            && let Err(e) = w.finish()
        {
            self.fail(format!("cannot write the recording: {e}"));
        }
        *self.finished.lock().unwrap() = true;
        self.finished_cond.notify_all();
    }

    fn fail(&self, message: String) {
        error!(%message, "recording error");
        self.error.lock().unwrap().get_or_insert(message);
    }
}

/// Counters of a recording.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RecordingStats {
    /// Encoded frames written.
    pub frames: u64,
    pub keyframes: u64,
    /// Encoded video bytes (without MPEG-TS overhead).
    pub bytes: u64,
    /// Video bitrate over about the last second, in bit/s.
    pub bitrate: f64,
    /// Segments started so far.
    pub segments: u32,
    /// Bytes written to disk (MPEG-TS segments and indexes).
    pub disk_bytes: u64,
}

impl RecordingBranch {
    /// Builds the branch and creates the camera folder. Segments are created as frames
    /// arrive.
    pub fn new(
        encoder: &EncoderInfo,
        settings: EncodingSettings,
        segments: SegmentConfig,
    ) -> Result<Self, RecordError> {
        let err = |e: &dyn std::fmt::Display| RecordError::Pipeline(e.to_string());
        let make = |factory: &str| {
            gst::ElementFactory::make(factory).build().map_err(|_| {
                RecordError::Pipeline(format!("GStreamer element `{factory}` is not available"))
            })
        };

        // The queue absorbs encoder hiccups. It never drops: if the encoder can't keep
        // up, capture is throttled and the drops show up in the capture statistics.
        let queue = make("queue")?;
        queue.set_property("max-size-buffers", 0u32);
        queue.set_property("max-size-bytes", 0u32);
        queue.set_property("max-size-time", 2 * gst::ClockTime::SECOND.nseconds());

        // Converts NV12 for encoders that need another format (OpenH264: I420);
        // passthrough otherwise.
        let convert = make("videoconvert")?;
        convert.set_property("n-threads", 4u32);

        let encoder_element = make_encoder(encoder, settings)?;

        // SPS/PPS before every keyframe, so each GOP can be decoded on its own.
        let parse = make("h264parse")?;
        parse.set_property("config-interval", -1i32);

        let dir = segments.dir.clone();
        let writer = SegmentWriter::new(segments)
            .map_err(|e| RecordError::Pipeline(format!("cannot create {}: {e}", dir.display())))?;
        let shared = Arc::new(Shared {
            stats: Mutex::new(StatsCounter::default()),
            writer: Mutex::new(Some(writer)),
            error: Mutex::new(None),
            finished: Mutex::new(false),
            finished_cond: Condvar::new(),
        });

        let sink = gst_app::AppSink::builder()
            .caps(
                &gst::Caps::builder("video/x-h264")
                    .field("stream-format", "byte-stream")
                    .field("alignment", "au")
                    .build(),
            )
            .sync(false)
            // Added to a running pipeline: must not wait for preroll.
            .async_(false)
            .callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample({
                        let shared = Arc::clone(&shared);
                        move |sink| {
                            let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                            // Capture time on the shared monotonic clock (AD-5): base
                            // time + running time. Encoders may offset the timestamps
                            // (e.g. by 1000 hours) and adjust the segment, so the PTS
                            // must be converted with the segment.
                            let segment = sample
                                .segment()
                                .and_then(|s| s.downcast_ref::<gst::ClockTime>());
                            if let Some(buffer) = sample.buffer()
                                && let Some(running_time) =
                                    buffer.pts().and_then(|pts| segment?.to_running_time(pts))
                                && let Ok(map) = buffer.map_readable()
                            {
                                let base = sink.base_time().map_or(0, |t| t.nseconds());
                                let keyframe =
                                    !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT);
                                shared.write(&map, base + running_time.nseconds(), keyframe);
                            }
                            // Always OK: a write error must not stop capture.
                            Ok(gst::FlowSuccess::Ok)
                        }
                    })
                    .eos({
                        let shared = Arc::clone(&shared);
                        move |_| shared.finish()
                    })
                    .build(),
            )
            .build();
        let sink: gst::Element = sink.upcast();

        let bin = gst::Bin::with_name("recording");
        let chain = [&queue, &convert, &encoder_element, &parse, &sink];
        bin.add_many(chain).map_err(|e| err(&e))?;
        gst::Element::link_many(chain)
            .map_err(|e| RecordError::Pipeline(format!("cannot link recording branch: {e}")))?;
        let ghost =
            gst::GhostPad::with_target(&queue.static_pad("sink").unwrap()).map_err(|e| err(&e))?;
        ghost.set_active(true).map_err(|e| err(&e))?;
        bin.add_pad(&ghost).map_err(|e| err(&e))?;

        info!(
            dir = %dir.display(),
            encoder = %encoder.factory,
            bitrate_kbps = settings.bitrate_kbps,
            gop_frames = settings.gop_frames,
            "recording branch created"
        );

        Ok(RecordingBranch { bin, dir, shared })
    }

    /// The bin to attach to the camera pipeline. It has one sink pad, `sink`.
    pub fn bin(&self) -> &gst::Bin {
        &self.bin
    }

    /// The camera folder with the segments.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn stats(&self) -> RecordingStats {
        self.shared.stats.lock().unwrap().stats
    }

    /// Flushes and closes the current segment without waiting for the end of the
    /// stream, e.g. when the camera pipeline has stopped. Data still in the encoder is
    /// lost.
    pub fn close(&self) {
        self.shared.finish();
    }

    /// The first error writing the recording. After an error nothing more is written;
    /// capture continues.
    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().unwrap().clone()
    }

    /// Waits until the end of the stream has been written, after EOS was sent into
    /// the branch. Returns false on timeout.
    pub fn wait_finished(&self, timeout: Duration) -> bool {
        let finished = self.shared.finished.lock().unwrap();
        let (finished, _) = self
            .shared
            .finished_cond
            .wait_timeout_while(finished, timeout, |done| !*done)
            .unwrap();
        *finished
    }
}

#[derive(Default)]
struct StatsCounter {
    stats: RecordingStats,
    window_start: Option<Instant>,
    window_bytes: u64,
}

impl StatsCounter {
    fn on_frame(&mut self, size: usize, keyframe: bool, now: Instant) {
        self.stats.frames += 1;
        self.stats.bytes += size as u64;
        if keyframe {
            self.stats.keyframes += 1;
        }

        let start = *self.window_start.get_or_insert(now);
        let elapsed = now.duration_since(start);
        if elapsed >= Duration::from_secs(1) {
            self.stats.bitrate = self.window_bytes as f64 * 8.0 / elapsed.as_secs_f64();
            self.window_start = Some(now);
            self.window_bytes = 0;
        }
        self.window_bytes += size as u64;
    }
}
