//! Recording control: encoder detection, start and stop of all cameras, interruption
//! and resumption of a camera's recording when it fails, status.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use tracing::{error, info, warn};
use var_capture::{AttachedBranch, Camera};
use var_core::index::LiveIndex;
use var_record::{
    Anchor, EncoderInfo, EncodingSettings, RecordingBranch, RecordingStats, SegmentConfig,
};

use crate::live::{CameraStatus, LiveCamera};
use crate::markers::{Marker, MarkerList};

/// How long stopping waits for the encoders to flush.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// Default segment length.
pub const DEFAULT_SEGMENT_MINUTES: u32 = 10;

#[derive(Debug, Clone)]
pub enum EncoderState {
    Detecting,
    Found(EncoderInfo),
    Missing,
}

pub struct Recorder {
    encoder: Arc<Mutex<EncoderState>>,
    /// Folder in which each recording (event) gets its own subfolder.
    pub folder: String,
    pub segment_length: Duration,
    session: Option<Session>,
    /// Error of the last start attempt.
    start_error: Option<String>,
}

/// A running recording of all cameras.
struct Session {
    event_dir: PathBuf,
    started: Instant,
    /// The time reference of all cameras (AD-5).
    anchor: Anchor,
    cameras: Vec<CameraRecording>,
    markers: MarkerList,
}

/// The recording of one camera slot. It survives camera failures: when the camera is
/// live again, a new recording branch continues in the same folder (REC-005).
struct CameraRecording {
    slot: usize,
    encoder: EncoderInfo,
    settings: EncodingSettings,
    segments: SegmentConfig,
    /// The branch on the camera pipeline of `generation`; `None` while interrupted.
    current: Option<Attached>,
    /// Since when the recording is interrupted (camera failed).
    interrupted_since: Option<Instant>,
    interruptions: u32,
    /// Counters of the branches before the current one.
    earlier_frames: u64,
    earlier_bytes: u64,
    earlier_disk_bytes: u64,
    error: Option<String>,
}

struct Attached {
    branch: RecordingBranch,
    attached: AttachedBranch,
    generation: u64,
}

/// What the UI shows about a running recording.
pub struct SessionStatus<'a> {
    pub elapsed: Duration,
    pub event_dir: &'a Path,
    pub cameras: Vec<CameraRecordingStatus>,
}

pub struct CameraRecordingStatus {
    pub slot: usize,
    /// Counters of the current branch (bitrate, segment).
    pub stats: RecordingStats,
    /// Bytes recorded for this camera in total.
    pub total_bytes: u64,
    /// Bytes written to disk for this camera in total (segments and indexes).
    pub total_disk_bytes: u64,
    /// The recording of this camera is interrupted for this long (camera failed).
    pub interrupted: Option<Duration>,
    pub interruptions: u32,
    pub error: Option<String>,
}

impl Recorder {
    /// Starts encoder detection in the background.
    pub fn new(ctx: &egui::Context, force_software: bool) -> Self {
        let encoder = Arc::new(Mutex::new(EncoderState::Detecting));
        std::thread::Builder::new()
            .name("var-encoder-probe".into())
            .spawn({
                let encoder = Arc::clone(&encoder);
                let ctx = ctx.clone();
                move || {
                    let state = match var_record::select_encoder(force_software) {
                        Some(info) => EncoderState::Found(info),
                        None => EncoderState::Missing,
                    };
                    *encoder.lock().unwrap() = state;
                    ctx.request_repaint();
                }
            })
            .expect("failed to spawn encoder probe thread");

        Recorder {
            encoder,
            folder: var_core::paths::default_recording_dir()
                .to_string_lossy()
                .into_owned(),
            segment_length: Duration::from_secs(u64::from(DEFAULT_SEGMENT_MINUTES) * 60),
            session: None,
            start_error: None,
        }
    }

    pub fn encoder(&self) -> EncoderState {
        self.encoder.lock().unwrap().clone()
    }

    pub fn is_recording(&self) -> bool {
        self.session.is_some()
    }

    pub fn start_error(&self) -> Option<&str> {
        self.start_error.as_deref()
    }

    /// The live indexes of the running recording, per camera slot, for review while
    /// recording. Empty when not recording.
    pub fn review_sources(&self) -> Vec<(usize, Arc<LiveIndex>)> {
        self.session
            .as_ref()
            .map(|s| {
                s.cameras
                    .iter()
                    .filter_map(|c| Some((c.slot, Arc::clone(c.segments.live_index.as_ref()?))))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The current time on the recording's timeline, while recording.
    pub fn now_wall_us(&self) -> Option<i64> {
        self.session.as_ref().map(|s| s.anchor.now_wall_us())
    }

    /// The markers of the running recording, sorted by time.
    pub fn markers(&self) -> &[Marker] {
        self.session.as_ref().map_or(&[], |s| s.markers.markers())
    }

    /// Sets a marker at `wall_us` (MRK-001). `None` when not recording.
    pub fn add_marker(
        &mut self,
        wall_us: i64,
        set_in: &str,
    ) -> Option<(Marker, std::io::Result<()>)> {
        Some(self.session.as_mut()?.markers.add(wall_us, set_in))
    }

    /// The marker after / before `wall_us` (MRK-003).
    pub fn adjacent_marker(&self, wall_us: i64, next: bool) -> Option<&Marker> {
        let markers = &self.session.as_ref()?.markers;
        if next {
            markers.next_after(wall_us)
        } else {
            markers.previous_before(wall_us)
        }
    }

    /// Whether the recording of camera `slot` is interrupted (camera failed).
    pub fn is_interrupted(&self, slot: usize) -> bool {
        self.session.as_ref().is_some_and(|s| {
            s.cameras
                .iter()
                .any(|c| c.slot == slot && c.interrupted_since.is_some())
        })
    }

    /// Bytes written to disk by the running recording, all cameras. `None` when not
    /// recording.
    pub fn disk_bytes(&self, cameras: &[LiveCamera]) -> Option<u64> {
        Some(
            self.status(cameras)?
                .cameras
                .iter()
                .map(|c| c.total_disk_bytes)
                .sum(),
        )
    }

    /// Why recording can't be started now, if it can't. All selected cameras must be
    /// live, and at least one must be selected.
    pub fn blocked_reason(&self, cameras: &[LiveCamera]) -> Option<String> {
        match self.encoder() {
            EncoderState::Detecting => return Some("Detecting the encoder…".into()),
            EncoderState::Missing => return Some("No H.264 encoder available".into()),
            EncoderState::Found(_) => {}
        }
        if cameras.iter().all(|c| c.selection().is_none()) {
            return Some("Select a camera".into());
        }
        for (slot, camera) in cameras.iter().enumerate() {
            if camera.selection().is_some() && !matches!(camera.status(), CameraStatus::Running) {
                return Some(format!("Camera {} is not live", slot + 1));
            }
        }
        if self.folder.trim().is_empty() {
            return Some("Select a recording folder".into());
        }
        None
    }

    /// Starts recording all live cameras into a new event folder named after the
    /// start time, with one subfolder per camera (REC-001).
    pub fn start(&mut self, cameras: &[LiveCamera]) {
        self.start_error = None;
        if let Err(message) = self.try_start(cameras) {
            error!(%message, "cannot start recording");
            self.start_error = Some(message);
        }
    }

    fn try_start(&mut self, cameras: &[LiveCamera]) -> Result<(), String> {
        let EncoderState::Found(encoder) = self.encoder() else {
            return Err("no encoder".into());
        };
        let event_dir = PathBuf::from(self.folder.trim())
            .join(chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string());
        std::fs::create_dir_all(&event_dir)
            .map_err(|e| format!("cannot create {}: {e}", event_dir.display()))?;

        // One anchor for all cameras (AD-5).
        let anchor = Anchor::now();
        let mut session = Session {
            event_dir: event_dir.clone(),
            started: Instant::now(),
            anchor,
            cameras: Vec::new(),
            markers: MarkerList::new(&event_dir),
        };

        for (slot, live) in cameras.iter().enumerate() {
            let Some((camera, selection, generation)) = live.camera() else {
                continue;
            };
            let mode = &selection.mode;
            let (num, den) = mode.capture_framerate();
            let mut recording = CameraRecording {
                slot,
                encoder: encoder.clone(),
                settings: EncodingSettings::for_mode(mode.width, mode.height, num, den),
                segments: SegmentConfig {
                    dir: event_dir.join(camera_folder_name(slot, &selection.device_name)),
                    segment_length: self.segment_length,
                    anchor,
                    fps: (num.max(0) as u32, den.max(1) as u32),
                    live_index: Some(Arc::new(LiveIndex::new((
                        num.max(0) as u32,
                        den.max(1) as u32,
                    )))),
                },
                current: None,
                interrupted_since: None,
                interruptions: 0,
                earlier_frames: 0,
                earlier_bytes: 0,
                earlier_disk_bytes: 0,
                error: None,
            };
            if let Err(message) = recording.attach(camera, generation) {
                // Don't leave the other cameras recording.
                stop_session(session, cameras);
                return Err(format!("camera {}: {message}", slot + 1));
            }
            info!(
                slot = slot + 1,
                camera = %selection.device_name,
                mode = %mode,
                dir = %recording.segments.dir.display(),
                encoder = %encoder.factory,
                hardware = encoder.hardware,
                bitrate_kbps = recording.settings.bitrate_kbps,
                gop_frames = recording.settings.gop_frames,
                "camera recording started"
            );
            session.cameras.push(recording);
        }

        info!(
            event_dir = %event_dir.display(),
            cameras = session.cameras.len(),
            segment_s = self.segment_length.as_secs(),
            "recording started"
        );
        self.session = Some(session);
        Ok(())
    }

    /// Stops the recording of all cameras: flushes the encoders and closes the files.
    pub fn stop(&mut self, cameras: &[LiveCamera]) {
        if let Some(session) = self.session.take() {
            stop_session(session, cameras);
        }
    }

    /// Follows camera failures and restarts: a camera whose pipeline is gone has its
    /// recording interrupted; when the camera is live again, its recording resumes in
    /// a new segment (REC-004, REC-005). The other cameras are not affected.
    pub fn update(&mut self, cameras: &[LiveCamera]) {
        let Some(session) = &mut self.session else {
            return;
        };
        for recording in &mut session.cameras {
            let live = &cameras[recording.slot];
            let current_generation = live.camera().map(|(_, _, generation)| generation);

            if let Some(current) = &recording.current
                && current_generation != Some(current.generation)
            {
                let current = recording.current.take().unwrap();
                // The pipeline is gone: close the segment with what has arrived.
                current.branch.close();
                let stats = current.branch.stats();
                recording.earlier_frames += stats.frames;
                recording.earlier_bytes += stats.bytes;
                recording.earlier_disk_bytes += stats.disk_bytes;
                recording.interrupted_since = Some(Instant::now());
                recording.interruptions += 1;
                error!(
                    slot = recording.slot + 1,
                    frames = stats.frames,
                    "camera recording interrupted: camera failed"
                );
            }

            if recording.current.is_none()
                && matches!(live.status(), CameraStatus::Running)
                && let Some((camera, _, generation)) = live.camera()
            {
                match recording.attach(camera, generation) {
                    Ok(()) => {
                        let gap = recording
                            .interrupted_since
                            .take()
                            .map_or(0.0, |since| since.elapsed().as_secs_f64());
                        info!(
                            slot = recording.slot + 1,
                            gap_s = format!("{gap:.1}"),
                            "camera recording resumed"
                        );
                    }
                    Err(message) => {
                        if recording.error.as_deref() != Some(&message) {
                            error!(slot = recording.slot + 1, %message, "cannot resume recording");
                        }
                        recording.error = Some(message);
                    }
                }
            }
        }
    }

    pub fn status(&self, cameras: &[LiveCamera]) -> Option<SessionStatus<'_>> {
        let session = self.session.as_ref()?;
        let status = session
            .cameras
            .iter()
            .map(|recording| {
                let (stats, error) = match &recording.current {
                    Some(current) => {
                        let branch_error = cameras[recording.slot]
                            .camera()
                            .filter(|(_, _, generation)| *generation == current.generation)
                            .and_then(|(camera, _, _)| camera.branch_error());
                        (
                            current.branch.stats(),
                            current.branch.error().or(branch_error),
                        )
                    }
                    None => (RecordingStats::default(), recording.error.clone()),
                };
                CameraRecordingStatus {
                    slot: recording.slot,
                    stats,
                    total_bytes: recording.earlier_bytes + stats.bytes,
                    total_disk_bytes: recording.earlier_disk_bytes + stats.disk_bytes,
                    interrupted: recording.interrupted_since.map(|since| since.elapsed()),
                    interruptions: recording.interruptions,
                    error,
                }
            })
            .collect();
        Some(SessionStatus {
            elapsed: session.started.elapsed(),
            event_dir: &session.event_dir,
            cameras: status,
        })
    }
}

impl CameraRecording {
    /// Creates a recording branch and attaches it to the camera's pipeline.
    fn attach(&mut self, camera: &Camera, generation: u64) -> Result<(), String> {
        let branch = RecordingBranch::new(&self.encoder, self.settings, self.segments.clone())
            .map_err(|e| e.to_string())?;
        let attached = camera.attach(branch.bin()).map_err(|e| e.to_string())?;
        self.current = Some(Attached {
            branch,
            attached,
            generation,
        });
        self.error = None;
        Ok(())
    }
}

fn stop_session(mut session: Session, cameras: &[LiveCamera]) {
    let camera_of = |slot: usize, generation: u64| {
        cameras[slot]
            .camera()
            .filter(|(_, _, g)| *g == generation)
            .map(|(camera, _, _)| camera)
    };

    // End all cameras first, so their recordings end at the same moment...
    for recording in &session.cameras {
        if let Some(current) = &recording.current
            && let Some(camera) = camera_of(recording.slot, current.generation)
        {
            camera.detach(&current.attached);
        }
    }
    // ...then wait for each to flush.
    for recording in &mut session.cameras {
        if let Some(current) = recording.current.take() {
            match camera_of(recording.slot, current.generation) {
                Some(camera) => {
                    if !current.branch.wait_finished(STOP_TIMEOUT) {
                        warn!(slot = recording.slot + 1, "encoder did not finish in time");
                    }
                    camera.remove(current.attached);
                }
                None => warn!(
                    slot = recording.slot + 1,
                    "camera pipeline was gone at stop"
                ),
            }
            current.branch.close();
            let stats = current.branch.stats();
            recording.earlier_frames += stats.frames;
            recording.earlier_bytes += stats.bytes;
            recording.earlier_disk_bytes += stats.disk_bytes;
            if let Some(error) = current.branch.error() {
                recording.error = Some(error);
            }
        }
        info!(
            slot = recording.slot + 1,
            dir = %recording.segments.dir.display(),
            frames = recording.earlier_frames,
            bytes = recording.earlier_bytes,
            interruptions = recording.interruptions,
            error = ?recording.error,
            "camera recording stopped"
        );
    }
    info!(
        event_dir = %session.event_dir.display(),
        duration_s = session.started.elapsed().as_secs(),
        "recording stopped"
    );
}

/// Expected disk usage of recording the selected cameras with their configured
/// bitrates, in bytes per second, for the estimate before recording (DSK-001).
pub fn configured_bytes_per_second(cameras: &[LiveCamera]) -> f64 {
    cameras
        .iter()
        .filter_map(|c| c.selection())
        .map(|selection| {
            let mode = &selection.mode;
            let (num, den) = mode.capture_framerate();
            let settings = EncodingSettings::for_mode(mode.width, mode.height, num, den);
            let fps = f64::from(num.max(1)) / f64::from(den.max(1));
            settings.bitrate_kbps as f64 * 1000.0 / 8.0 * TS_OVERHEAD
                + fps * var_core::index::RECORD_SIZE as f64
        })
        .sum()
}

/// MPEG-TS overhead over the encoded video: 4-byte packet headers, PES headers, PCR
/// and the PAT/PMT before keyframes (measured: about 3 %).
const TS_OVERHEAD: f64 = 1.03;

/// Folder of a camera within the event folder: `Camera 1 - <device name>`.
fn camera_folder_name(slot: usize, device_name: &str) -> String {
    format!("Camera {} - {}", slot + 1, file_name(device_name))
}

/// A file name from a device name: characters that are invalid in file names on any
/// OS are replaced.
fn file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_().".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').to_owned();
    if cleaned.is_empty() {
        "camera".to_owned()
    } else {
        cleaned
    }
}

/// `hh:mm:ss`.
pub fn format_elapsed(elapsed: Duration) -> String {
    let s = elapsed.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_safe() {
        assert_eq!(file_name("Integrated Camera"), "Integrated Camera");
        assert_eq!(
            file_name("USB3.0 HD: Capture <2>"),
            "USB3.0 HD_ Capture _2_"
        );
        assert_eq!(file_name("cam/..\\x"), "cam_.._x");
        assert_eq!(file_name("..."), "camera");
        assert_eq!(camera_folder_name(1, "UHD Camera"), "Camera 2 - UHD Camera");
    }

    #[test]
    fn elapsed_format() {
        assert_eq!(format_elapsed(Duration::from_secs(3723)), "01:02:03");
        assert_eq!(format_elapsed(Duration::from_secs(59)), "00:00:59");
    }
}
