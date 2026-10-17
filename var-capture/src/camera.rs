//! Capture pipeline for one camera.
//!
//! ```text
//! source ─ mode caps ─ queue ─ [jpegdec] ─ videoconvert ─ NV12 ─ tee ─ queue (leaky) ─ preview appsink
//!                    └ probe: capture statistics                     └ attached branches (recording)
//! ```
//!
//! MJPEG is decoded, YUY2 is converted to NV12; NV12 passes through. The capture
//! queue decouples the source thread from decoding, so short processing delays don't
//! cost frames. The preview branch never blocks capture: it keeps only the newest frame.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::VideoFrameExt;
use tracing::{debug, error, info, warn};
use var_core::color::{YuvColorimetry, YuvMatrix};
use var_core::timing::DropDetector;

use crate::CaptureError;
use crate::device::{DeviceInfo, DeviceKind, Mode, PixelFormat};
use crate::synthetic;

/// Threads for pixel format conversion per camera.
const CONVERT_THREADS: u32 = 4;

/// Name of the tee that branches (preview, recording) are attached to.
const TEE_NAME: &str = "tee";

/// A running capture pipeline for one device and mode. Stops when dropped.
pub struct Camera {
    pipeline: gst::Pipeline,
    shared: Arc<Shared>,
    label: String,
    started: Instant,
}

struct Shared {
    latest: Mutex<Option<Arc<PreviewFrame>>>,
    stats: Mutex<StatsCounter>,
    /// When the source delivered its last frame (watchdog).
    last_frame: Mutex<Option<Instant>>,
    error: Mutex<Option<String>>,
    /// Names of the attached branch bins.
    branches: Mutex<Vec<String>>,
    /// First error reported by an attached branch since the last attach.
    branch_error: Mutex<Option<String>>,
    notify: Box<dyn Fn() + Send + Sync>,
}

impl Camera {
    /// Builds and starts the pipeline. `notify` is called from GStreamer threads
    /// whenever a new preview frame or an error is available.
    pub fn start(
        device: &DeviceInfo,
        mode: &Mode,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, CaptureError> {
        let label = format!("{} – {mode}", device.name);
        let (num, den) = mode.capture_framerate();
        let shared = Arc::new(Shared {
            latest: Mutex::new(None),
            stats: Mutex::new(StatsCounter::new(num, den)),
            last_frame: Mutex::new(None),
            error: Mutex::new(None),
            branches: Mutex::new(Vec::new()),
            branch_error: Mutex::new(None),
            notify: Box::new(notify),
        });

        let pipeline = build_pipeline(device, mode, &shared)?;
        // All cameras share the monotonic system clock, so their capture times are
        // comparable (AD-5). A source must not bring its own clock.
        pipeline.use_clock(Some(&gst::SystemClock::obtain()));
        let camera = Camera {
            pipeline,
            shared,
            label,
            started: Instant::now(),
        };

        if let Err(err) = camera.pipeline.set_state(gst::State::Playing) {
            let detail = camera.error().unwrap_or_else(|| err.to_string());
            return Err(CaptureError::Pipeline(format!(
                "cannot start capture: {detail}"
            )));
        }
        info!(camera = %camera.label, "capture started");
        Ok(camera)
    }

    /// The newest preview frame, if any has arrived yet.
    pub fn latest_frame(&self) -> Option<Arc<PreviewFrame>> {
        self.shared.latest.lock().unwrap().clone()
    }

    pub fn stats(&self) -> CameraStats {
        self.shared.stats.lock().unwrap().stats
    }

    /// The first error reported by the pipeline. A camera with an error delivers no
    /// more frames.
    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().unwrap().clone()
    }

    /// Watchdog (REC-006): time since the source delivered its last frame, or since
    /// the start if no frame has arrived yet; `true` if a frame has arrived.
    pub fn time_without_frames(&self) -> (Duration, bool) {
        match *self.shared.last_frame.lock().unwrap() {
            Some(last) => (last.elapsed(), true),
            None => (self.started.elapsed(), false),
        }
    }

    /// Stops the pipeline on a background thread, so a device that hangs on stopping
    /// (e.g. just unplugged) doesn't block the caller. Join the handle before opening
    /// the device again.
    pub fn stop_in_background(self) -> std::thread::JoinHandle<()> {
        std::thread::Builder::new()
            .name("var-camera-stop".into())
            .spawn(move || drop(self))
            .expect("failed to spawn camera stop thread")
    }

    /// Attaches a branch (e.g. recording) to the NV12 tee while the pipeline runs.
    /// The bin must have a sink pad named `sink`. The live view is not interrupted.
    pub fn attach(&self, bin: &gst::Bin) -> Result<AttachedBranch, CaptureError> {
        let err = |e: &dyn std::fmt::Display| CaptureError::Pipeline(e.to_string());
        let tee = self
            .pipeline
            .by_name(TEE_NAME)
            .ok_or_else(|| CaptureError::Pipeline("pipeline has no tee".into()))?;
        let sink = bin
            .static_pad("sink")
            .ok_or_else(|| CaptureError::Pipeline("branch has no sink pad".into()))?;

        *self.shared.branch_error.lock().unwrap() = None;
        self.shared
            .branches
            .lock()
            .unwrap()
            .push(bin.name().to_string());
        self.pipeline.add(bin).map_err(|e| err(&e))?;
        if let Err(e) = bin.sync_state_with_parent() {
            let _ = self.pipeline.remove(bin);
            return Err(err(&e));
        }
        let tee_pad = tee
            .request_pad_simple("src_%u")
            .ok_or_else(|| CaptureError::Pipeline("cannot get a tee pad".into()))?;
        if let Err(e) = tee_pad.link(&sink) {
            tee.release_request_pad(&tee_pad);
            let _ = bin.set_state(gst::State::Null);
            let _ = self.pipeline.remove(bin);
            return Err(CaptureError::Pipeline(format!("cannot link branch: {e}")));
        }
        info!(camera = %self.label, branch = %bin.name(), "branch attached");
        Ok(AttachedBranch {
            bin: bin.clone(),
            tee_pad,
        })
    }

    /// Stops the data flow into an attached branch and sends end-of-stream into it, so
    /// it can flush. The caller waits until the branch has finished, then calls
    /// [`Camera::remove`].
    pub fn detach(&self, branch: &AttachedBranch) {
        let Some(sink) = branch.bin.static_pad("sink") else {
            return;
        };
        // The idle probe runs between two buffers, so no frame is cut in half.
        branch
            .tee_pad
            .add_probe(gst::PadProbeType::IDLE, move |pad, _| {
                let _ = pad.unlink(&sink);
                sink.send_event(gst::event::Eos::new());
                gst::PadProbeReturn::Remove
            });
    }

    /// Removes a detached branch from the pipeline.
    pub fn remove(&self, branch: AttachedBranch) {
        let _ = branch.bin.set_state(gst::State::Null);
        let _ = self.pipeline.remove(&branch.bin);
        if let Some(tee) = self.pipeline.by_name(TEE_NAME) {
            tee.release_request_pad(&branch.tee_pad);
        }
        let name = branch.bin.name();
        self.shared.branches.lock().unwrap().retain(|b| *b != name);
        info!(camera = %self.label, branch = %name, "branch removed");
    }

    /// The first error of an attached branch since it was attached. Capture and the
    /// live view continue; the branch receives no more data.
    pub fn branch_error(&self) -> Option<String> {
        self.shared.branch_error.lock().unwrap().clone()
    }
}

/// A branch attached to a camera's tee; see [`Camera::attach`].
pub struct AttachedBranch {
    bin: gst::Bin,
    tee_pad: gst::Pad,
}

impl Drop for Camera {
    fn drop(&mut self) {
        let start = Instant::now();
        let _ = self.pipeline.set_state(gst::State::Null);
        let stats = self.stats();
        info!(
            camera = %self.label,
            captured = stats.captured,
            dropped = stats.dropped,
            stop_ms = start.elapsed().as_millis() as u64,
            "capture stopped"
        );
    }
}

fn make(factory: &str) -> Result<gst::Element, CaptureError> {
    gst::ElementFactory::make(factory).build().map_err(|_| {
        CaptureError::Pipeline(format!("GStreamer element `{factory}` is not available"))
    })
}

fn build_pipeline(
    device: &DeviceInfo,
    mode: &Mode,
    shared: &Arc<Shared>,
) -> Result<gst::Pipeline, CaptureError> {
    if !mode.format.is_supported() {
        return Err(CaptureError::Pipeline(format!(
            "format {} is not supported for capture",
            mode.format
        )));
    }

    let source: gst::Element = match (&device.kind, &device.handle) {
        (DeviceKind::Synthetic, _) => {
            let bin =
                gst::parse::bin_from_description(&synthetic::launch_description(mode, None), true)
                    .map_err(|err| CaptureError::Pipeline(format!("synthetic camera: {err}")))?;
            synthetic::add_effects(&bin, mode, synthetic::number(&device.id));
            bin.upcast()
        }
        (DeviceKind::System, Some(handle)) => handle
            .create_element(Some("source"))
            .map_err(|err| CaptureError::Pipeline(format!("cannot open device: {err}")))?,
        (DeviceKind::System, None) => {
            return Err(CaptureError::Pipeline(
                "device has no GStreamer handle".into(),
            ));
        }
    };

    let capture_caps = gst::ElementFactory::make("capsfilter")
        .name("capture-caps")
        .property("caps", mode.to_caps())
        .build()
        .map_err(|err| CaptureError::Pipeline(err.to_string()))?;

    let capture_queue = make("queue")?;
    capture_queue.set_property("max-size-buffers", 0u32);
    capture_queue.set_property("max-size-bytes", 0u32);
    capture_queue.set_property("max-size-time", gst::ClockTime::SECOND.nseconds());

    let mut chain = vec![source, capture_caps.clone(), capture_queue];
    if mode.format == PixelFormat::Mjpeg {
        chain.push(make("jpegdec")?);
    }
    // Single-threaded conversion of 1080p doesn't reach 60 fps on all machines (e.g.
    // Windows on ARM, where GStreamer has no SIMD conversion code). Chroma filtering
    // is skipped: 4:2:2 → 4:2:0 by dropping lines is good enough here and much faster.
    let convert = make("videoconvert")?;
    convert.set_property("n-threads", CONVERT_THREADS);
    convert.set_property_from_str("dither", "none");
    convert.set_property_from_str("chroma-mode", "none");
    chain.push(convert);
    chain.push(
        gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                gst::Caps::builder("video/x-raw")
                    .field("format", "NV12")
                    .build(),
            )
            .build()
            .map_err(|err| CaptureError::Pipeline(err.to_string()))?,
    );
    chain.push(
        gst::ElementFactory::make("tee")
            .name(TEE_NAME)
            .build()
            .map_err(|_| {
                CaptureError::Pipeline("GStreamer element `tee` is not available".into())
            })?,
    );

    let preview_queue = make("queue")?;
    preview_queue.set_property("max-size-buffers", 2u32);
    preview_queue.set_property("max-size-bytes", 0u32);
    preview_queue.set_property("max-size-time", 0u64);
    preview_queue.set_property_from_str("leaky", "downstream");
    chain.push(preview_queue);

    let appsink = gst_app::AppSink::builder()
        .name("preview")
        .max_buffers(1)
        .drop(true)
        .sync(false)
        .callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample({
                    let shared = Arc::clone(shared);
                    move |sink| on_preview_sample(sink, &shared)
                })
                .build(),
        )
        .build();
    chain.push(appsink.upcast());

    let pipeline = gst::Pipeline::new();
    pipeline
        .add_many(&chain)
        .map_err(|err| CaptureError::Pipeline(err.to_string()))?;
    gst::Element::link_many(&chain)
        .map_err(|err| CaptureError::Pipeline(format!("cannot link capture pipeline: {err}")))?;

    // Capture statistics are taken directly after the source, so frames dropped by
    // the preview branch don't count as capture drops.
    let src_pad = capture_caps
        .static_pad("src")
        .expect("capsfilter has a src pad");
    src_pad.add_probe(gst::PadProbeType::BUFFER, {
        let shared = Arc::clone(shared);
        move |_, info| {
            if let Some(buffer) = info.buffer() {
                let now = Instant::now();
                *shared.last_frame.lock().unwrap() = Some(now);
                shared.stats.lock().unwrap().on_capture(buffer.pts(), now);
            }
            gst::PadProbeReturn::Ok
        }
    });

    let bus = pipeline.bus().expect("pipeline has a bus");
    bus.set_sync_handler({
        let shared = Arc::clone(shared);
        move |_, message| {
            on_bus_message(message, &shared);
            gst::BusSyncReply::Drop
        }
    });

    Ok(pipeline)
}

/// The attached branch bin that `object` belongs to, if any.
fn branch_of(object: Option<&gst::Object>, shared: &Shared) -> Option<String> {
    let branches = shared.branches.lock().unwrap();
    let mut current = object.cloned();
    while let Some(o) = current {
        let name = o.name().to_string();
        if branches.contains(&name) {
            return Some(name);
        }
        current = o.parent();
    }
    None
}

fn on_bus_message(message: &gst::Message, shared: &Shared) {
    match message.view() {
        gst::MessageView::Error(err) => {
            let text = err.error().to_string();
            let source = message.src().map(|s| s.path_string());
            // A failing branch (e.g. recording to a full disk) doesn't stop capture.
            if let Some(branch) = branch_of(message.src(), shared) {
                error!(%branch, error = %text, debug = ?err.debug(), ?source, "branch error");
                let mut slot = shared.branch_error.lock().unwrap();
                if slot.is_none() {
                    *slot = Some(text);
                    drop(slot);
                    (shared.notify)();
                }
                return;
            }
            error!(error = %text, debug = ?err.debug(), ?source, "capture error");
            let mut slot = shared.error.lock().unwrap();
            if slot.is_none() {
                *slot = Some(text);
                drop(slot);
                (shared.notify)();
            }
        }
        gst::MessageView::Warning(w) => {
            warn!(warning = %w.error(), debug = ?w.debug(), "capture warning");
        }
        gst::MessageView::Eos(_) => {
            let mut slot = shared.error.lock().unwrap();
            if slot.is_none() {
                *slot = Some("the device stopped delivering video".into());
                drop(slot);
                (shared.notify)();
            }
        }
        _ => {}
    }
}

fn on_preview_sample(
    sink: &gst_app::AppSink,
    shared: &Shared,
) -> Result<gst::FlowSuccess, gst::FlowError> {
    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
    let caps = sample.caps().ok_or(gst::FlowError::NotNegotiated)?;
    let info = gst_video::VideoInfo::from_caps(caps).map_err(|_| gst::FlowError::NotNegotiated)?;
    let buffer = sample.buffer_owned().ok_or(gst::FlowError::Error)?;
    let colorimetry = yuv_colorimetry(&info);
    let frame = gst_video::VideoFrame::from_buffer_readable(buffer, &info)
        .map_err(|_| gst::FlowError::Error)?;

    let seq = {
        let mut stats = shared.stats.lock().unwrap();
        stats.stats.preview_frames += 1;
        stats.stats.preview_frames
    };
    *shared.latest.lock().unwrap() = Some(Arc::new(PreviewFrame {
        frame,
        seq,
        colorimetry,
    }));
    (shared.notify)();
    Ok(gst::FlowSuccess::Ok)
}

/// A decoded NV12 preview frame. Plane 0 is Y, plane 1 is interleaved CbCr at half
/// resolution in both directions.
pub struct PreviewFrame {
    frame: gst_video::VideoFrame<gst_video::video_frame::Readable>,
    seq: u64,
    colorimetry: YuvColorimetry,
}

impl PreviewFrame {
    /// Sequence number, increasing by one for each preview frame of a camera.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn width(&self) -> u32 {
        self.frame.width()
    }

    pub fn height(&self) -> u32 {
        self.frame.height()
    }

    /// Data of plane `index` (0: Y, 1: CbCr), rows `stride(index)` bytes apart.
    pub fn plane(&self, index: u32) -> &[u8] {
        self.frame.plane_data(index).unwrap_or(&[])
    }

    pub fn stride(&self, index: u32) -> u32 {
        self.frame.plane_stride()[index as usize] as u32
    }

    pub fn colorimetry(&self) -> YuvColorimetry {
        self.colorimetry
    }
}

/// The colorimetry of NV12 frames described by `info`.
pub fn yuv_colorimetry(info: &gst_video::VideoInfo) -> YuvColorimetry {
    let colorimetry = info.colorimetry();
    let matrix = match colorimetry.matrix() {
        gst_video::VideoColorMatrix::Bt709 => YuvMatrix::Bt709,
        gst_video::VideoColorMatrix::Bt2020 => YuvMatrix::Bt2020,
        gst_video::VideoColorMatrix::Bt601
        | gst_video::VideoColorMatrix::Fcc
        | gst_video::VideoColorMatrix::Smpte240m => YuvMatrix::Bt601,
        _ if info.height() >= 720 => YuvMatrix::Bt709,
        _ => YuvMatrix::Bt601,
    };
    YuvColorimetry {
        matrix,
        full_range: colorimetry.range() == gst_video::VideoColorRange::Range0_255,
    }
}

/// Counters of one camera.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CameraStats {
    /// Frames delivered by the device.
    pub captured: u64,
    /// Frames missing from the device's output, detected from gaps in the timestamps
    /// (counted a few frames after the gap).
    pub dropped: u64,
    /// Frames that arrived late without a frame being lost (timing jitter).
    pub late: u64,
    /// Capture frame rate from the arrival times of the frames, measured over about one
    /// second. Unlike the timestamps, this also shows a source that falls behind.
    pub fps: f64,
    /// Frames delivered to the preview (fewer than captured if the preview falls behind).
    pub preview_frames: u64,
}

struct StatsCounter {
    stats: CameraStats,
    drops: DropDetector,
    window_start: Option<Instant>,
    window_frames: u64,
}

impl StatsCounter {
    const WINDOW: Duration = Duration::from_secs(1);

    fn new(fps_num: i32, fps_den: i32) -> Self {
        let frame_ns = if fps_num > 0 && fps_den > 0 {
            1e9 * f64::from(fps_den) / f64::from(fps_num)
        } else {
            0.0
        };
        StatsCounter {
            stats: CameraStats::default(),
            drops: DropDetector::new(frame_ns),
            window_start: None,
            window_frames: 0,
        }
    }

    fn on_capture(&mut self, pts: Option<gst::ClockTime>, arrival: Instant) {
        self.stats.captured += 1;

        match self.window_start {
            None => {
                self.window_start = Some(arrival);
                self.window_frames = 0;
            }
            Some(start) => {
                self.window_frames += 1;
                let elapsed = arrival.saturating_duration_since(start);
                if elapsed >= Self::WINDOW {
                    self.stats.fps = self.window_frames as f64 / elapsed.as_secs_f64();
                    self.window_start = Some(arrival);
                    self.window_frames = 0;
                }
            }
        }

        let Some(pts) = pts.map(gst::ClockTime::nseconds) else {
            return;
        };
        if let Some(gap) = self.drops.push(pts) {
            if gap.missing > 0 {
                self.stats.dropped += gap.missing;
                debug!(missing = gap.missing, "capture frames dropped");
            } else {
                self.stats.late += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stopped camera must release its pipeline (and with it all buffers), or every
    /// restart after a camera failure leaks memory.
    #[test]
    fn stopped_camera_releases_its_pipeline() {
        crate::init().unwrap();
        let device = crate::synthetic::devices(1).remove(0);
        let mode = device.modes[0].clone();
        for stop_in_background in [false, true] {
            let camera = Camera::start(&device, &mode, || {}).unwrap();
            std::thread::sleep(Duration::from_millis(1500));
            assert!(camera.latest_frame().is_some());
            let pipeline = camera.pipeline.downgrade();
            if stop_in_background {
                camera.stop_in_background().join().unwrap();
            } else {
                drop(camera);
            }
            assert!(
                pipeline.upgrade().is_none(),
                "pipeline still alive after stop"
            );
        }
    }

    /// Frames with timestamps `times_ms`, arriving exactly at those times.
    fn feed(counter: &mut StatsCounter, times_ms: impl IntoIterator<Item = u64>) {
        let base = Instant::now();
        for ms in times_ms {
            counter.on_capture(
                Some(gst::ClockTime::from_mseconds(ms)),
                base + Duration::from_millis(ms),
            );
        }
    }

    #[test]
    fn fps_follows_arrival_not_timestamps() {
        // Timestamps say 60 fps, but frames arrive only every 50 ms.
        let mut counter = StatsCounter::new(60, 1);
        let base = Instant::now();
        for i in 0..=40u64 {
            counter.on_capture(
                Some(gst::ClockTime::from_nseconds(i * 16_666_667)),
                base + Duration::from_millis(i * 50),
            );
        }
        assert_eq!(counter.stats.dropped, 0);
        assert!(
            (counter.stats.fps - 20.0).abs() < 0.01,
            "{}",
            counter.stats.fps
        );
    }

    #[test]
    fn steady_50fps_has_no_drops() {
        let mut counter = StatsCounter::new(50, 1);
        feed(&mut counter, (0..=100).map(|i| i * 20));
        assert_eq!(counter.stats.captured, 101);
        assert_eq!(counter.stats.dropped, 0);
        assert!(
            (counter.stats.fps - 50.0).abs() < 0.01,
            "{}",
            counter.stats.fps
        );
    }

    #[test]
    fn gaps_count_missing_frames() {
        let mut counter = StatsCounter::new(50, 1);
        // 20 ms frames; 40→100 ms misses 2 frames, 100→160 misses 2 more. Gaps are
        // judged three frames later.
        feed(&mut counter, [0, 20, 40, 100, 160, 180, 200, 220, 240]);
        assert_eq!(counter.stats.dropped, 4);
        assert_eq!(counter.stats.late, 0);
    }

    #[test]
    fn jitter_below_threshold_is_not_a_drop() {
        let mut counter = StatsCounter::new(60, 1);
        feed(&mut counter, [0, 16, 40, 50, 66, 83, 100, 116]);
        assert_eq!(counter.stats.dropped, 0);
    }

    #[test]
    fn late_frame_is_counted_as_late() {
        let mut counter = StatsCounter::new(30, 1);
        feed(&mut counter, [0, 33, 67, 130, 133, 167, 200, 233, 267]);
        assert_eq!((counter.stats.dropped, counter.stats.late), (0, 1));
    }
}
