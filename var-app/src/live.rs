//! A camera slot: runs the capture pipeline of the selected device, detects failures
//! and restarts the camera, and shows the live view with a debug overlay.

use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, Rect, Stroke};
use tracing::{error, info, warn};
use var_capture::{Camera, DeviceInfo, Mode};

use std::sync::Arc;

use crate::video::{Nv12Frame, VideoPaintCallback, ViewId};

/// A running camera that delivers no frame for this long has failed (REC-006).
const WATCHDOG: Duration = Duration::from_secs(1);
/// Time for a camera to deliver its first frame after the start.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
/// Wait before restarting a failed camera whose device is still connected.
const RETRY_DELAY: Duration = Duration::from_secs(3);

const ALARM_COLOR: Color32 = Color32::from_rgb(0xe0, 0x30, 0x30);

/// The camera chosen in setup.
#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    pub device_id: String,
    pub device_name: String,
    pub mode: Mode,
}

pub enum CameraStatus<'a> {
    NoSelection,
    /// The selected device is not connected.
    Disconnected,
    /// Starting, no frame yet.
    Starting,
    Running,
    /// Failed; restarts automatically when possible.
    Failed(&'a str),
}

impl CameraStatus<'_> {
    /// A selected camera that delivers no video (REC-006).
    pub fn is_alarm(&self) -> bool {
        matches!(self, CameraStatus::Disconnected | CameraStatus::Failed(_))
    }
}

/// A camera slot: runs the capture pipeline for the selected device and shows its
/// preview.
///
/// A camera fails on a pipeline error or when it delivers no frame for a second
/// (watchdog). Its pipeline is stopped in the background without affecting other
/// cameras, and it restarts automatically (REC-005): when the device reappears after
/// being disconnected, or after a short delay if it stayed connected. Starting and
/// stopping run on background threads, so a hanging device never blocks the UI.
pub struct LiveCamera {
    view: ViewId,
    selection: Option<Selection>,
    active: Option<Active>,
    starting: Option<Starting>,
    failure: Option<Failure>,
    /// Pipelines being stopped in the background; the device is reopened only after.
    stopping: Vec<JoinHandle<()>>,
    /// The selected device is in the device list.
    device_present: bool,
    display: DisplayStats,
    /// Number of pipelines started so far; identifies the running pipeline.
    starts: u64,
}

struct Active {
    selection: Selection,
    camera: Camera,
    generation: u64,
}

struct Starting {
    selection: Selection,
    result: mpsc::Receiver<Result<Camera, String>>,
}

struct Failure {
    selection: Selection,
    message: String,
    retry_at: Instant,
}

impl LiveCamera {
    pub fn new(view: ViewId) -> Self {
        LiveCamera {
            view,
            selection: None,
            active: None,
            starting: None,
            failure: None,
            stopping: Vec::new(),
            device_present: false,
            display: DisplayStats::default(),
            starts: 0,
        }
    }

    pub fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }

    /// The running camera with its generation, which changes whenever the pipeline
    /// is restarted. `None` if no pipeline is running.
    pub fn camera(&self) -> Option<(&Camera, &Selection, u64)> {
        self.active
            .as_ref()
            .map(|a| (&a.camera, &a.selection, a.generation))
    }

    pub fn select(&mut self, selection: Option<Selection>) {
        if self.selection != selection {
            match &selection {
                Some(s) => info!(device = %s.device_name, mode = %s.mode, "camera selected"),
                None => info!("camera deselected"),
            }
            self.selection = selection;
        }
    }

    pub fn status(&self) -> CameraStatus<'_> {
        let Some(selection) = &self.selection else {
            return CameraStatus::NoSelection;
        };
        if let Some(active) = &self.active {
            return if active.camera.latest_frame().is_some() {
                CameraStatus::Running
            } else {
                CameraStatus::Starting
            };
        }
        if !self.device_present {
            return CameraStatus::Disconnected;
        }
        match &self.failure {
            Some(f) if f.selection == *selection && self.starting.is_none() => {
                CameraStatus::Failed(&f.message)
            }
            _ => CameraStatus::Starting,
        }
    }

    /// Starts, stops or restarts the pipeline to match the selection and the devices,
    /// and checks the running camera (watchdog). Call once per UI frame.
    pub fn update(&mut self, devices: &[DeviceInfo], ctx: &egui::Context) {
        self.stopping.retain(|handle| !handle.is_finished());
        self.collect_started();
        self.device_present = self
            .selection
            .as_ref()
            .is_some_and(|s| devices.iter().any(|d| d.id == s.device_id));

        let Some(selection) = self.selection.clone() else {
            self.stop("camera deselected");
            self.failure = None;
            return;
        };

        if let Some(active) = &self.active {
            if active.selection != selection {
                self.stop("selection changed");
            } else if let Some(message) = active.camera.error() {
                self.fail(&selection, message);
            } else {
                let (silence, any_frame) = active.camera.time_without_frames();
                let limit = if any_frame {
                    WATCHDOG
                } else {
                    FIRST_FRAME_TIMEOUT
                };
                if silence >= limit {
                    let message = if any_frame {
                        format!("no video for {:.1} s", silence.as_secs_f64())
                    } else {
                        "no video after start".to_owned()
                    };
                    self.fail(&selection, message);
                } else {
                    return; // healthy
                }
            }
        }

        let Some(device) = devices.iter().find(|d| d.id == selection.device_id) else {
            // Restarts when the device reappears.
            return;
        };
        if self.starting.is_some() || !self.stopping.is_empty() {
            return;
        }
        if let Some(failure) = &self.failure
            && failure.selection == selection
            && Instant::now() < failure.retry_at
        {
            return;
        }
        if !device.modes.contains(&selection.mode) {
            self.fail(
                &selection,
                format!("the device doesn't offer {}", selection.mode),
            );
            return;
        }
        self.start(device, &selection, ctx);
    }

    /// Starts the pipeline on a background thread.
    fn start(&mut self, device: &DeviceInfo, selection: &Selection, ctx: &egui::Context) {
        info!(device = %selection.device_name, mode = %selection.mode, "starting camera");
        let (sender, receiver) = mpsc::channel();
        let device = device.clone();
        let mode = selection.mode.clone();
        let repaint = ctx.clone();
        std::thread::Builder::new()
            .name("var-camera-start".into())
            .spawn(move || {
                let notify = repaint.clone();
                let result = Camera::start(&device, &mode, move || notify.request_repaint())
                    .map_err(|e| e.to_string());
                // If nobody waits any more, the camera is dropped (stopped) here.
                let _ = sender.send(result);
                repaint.request_repaint();
            })
            .expect("failed to spawn camera start thread");
        self.starting = Some(Starting {
            selection: selection.clone(),
            result: receiver,
        });
    }

    /// Takes over a camera whose start has finished.
    fn collect_started(&mut self) {
        let Some(starting) = &self.starting else {
            return;
        };
        let result = match starting.result.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("camera start failed".to_owned()),
        };
        let selection = self.starting.take().unwrap().selection;
        match result {
            Ok(camera) if self.selection.as_ref() == Some(&selection) => {
                self.starts += 1;
                self.display = DisplayStats::default();
                if self.failure.take().is_some() {
                    info!(device = %selection.device_name, "camera restarted after failure");
                }
                self.active = Some(Active {
                    selection,
                    camera,
                    generation: self.starts,
                });
            }
            // Selection changed while starting.
            Ok(camera) => self.stopping.push(camera.stop_in_background()),
            Err(message) => self.fail(&selection, message),
        }
    }

    /// Releases the camera slot when it is removed: stops its pipeline in the
    /// background without waiting. A camera still starting is stopped by its start
    /// thread.
    pub fn release(mut self) {
        self.stop("camera slot removed");
        self.starting = None;
    }

    /// Stops the running pipeline in the background.
    fn stop(&mut self, reason: &str) {
        if let Some(active) = self.active.take() {
            info!(device = %active.selection.device_name, reason, "stopping camera");
            self.stopping.push(active.camera.stop_in_background());
        }
    }

    /// Records a failure, stops the pipeline and schedules a restart.
    fn fail(&mut self, selection: &Selection, message: String) {
        error!(device = %selection.device_name, mode = %selection.mode, %message, "camera failed");
        if let Some(active) = self.active.take() {
            self.stopping.push(active.camera.stop_in_background());
        }
        if self.stopping.len() > 3 {
            warn!(
                count = self.stopping.len(),
                "camera pipelines are slow to stop"
            );
        }
        self.failure = Some(Failure {
            selection: selection.clone(),
            message,
            retry_at: Instant::now() + RETRY_DELAY,
        });
    }

    /// Draws the live view into `rect` with a caption, or the failure alarm.
    /// `recording_interrupted`: the recording of this camera is interrupted. Returns
    /// the tile's response (clicks toggle full screen).
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        caption_text: &str,
        debug_overlay: bool,
        recording_interrupted: bool,
    ) -> egui::Response {
        let response = ui.allocate_rect(rect, egui::Sense::click());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::BLACK);
        self.paint(&painter, rect, debug_overlay, recording_interrupted);

        caption(&painter, rect, caption_text);
        response
    }

    fn paint(
        &mut self,
        painter: &egui::Painter,
        rect: Rect,
        debug_overlay: bool,
        recording_interrupted: bool,
    ) {
        // Camera failure alarm (REC-006): red frame and banner.
        let status = self.status();
        if status.is_alarm() {
            let detail = match status {
                CameraStatus::Failed(message) => format!("{message} – restarting"),
                _ => "Disconnected – waiting for the camera".to_owned(),
            };
            painter.rect_stroke(
                rect.shrink(2.0),
                0.0,
                Stroke::new(4.0_f32, ALARM_COLOR),
                egui::StrokeKind::Inside,
            );
            let mut y = rect.center().y - 30.0;
            for (text, size) in [
                ("CAMERA FAILURE".to_owned(), 30.0),
                (detail, 17.0),
                (
                    if recording_interrupted {
                        "Not recording".to_owned()
                    } else {
                        String::new()
                    },
                    17.0,
                ),
            ] {
                painter.text(
                    egui::pos2(rect.center().x, y),
                    Align2::CENTER_CENTER,
                    text,
                    FontId::proportional(size),
                    ALARM_COLOR,
                );
                y += size + 12.0;
            }
            return;
        }

        let message = match status {
            CameraStatus::NoSelection => Some("Right-click to select a camera".to_owned()),
            CameraStatus::Starting => Some("Starting camera…".to_owned()),
            _ => None,
        };
        if let Some(message) = message {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                message,
                FontId::proportional(18.0),
                Color32::from_gray(200),
            );
            return;
        }

        let Some(active) = &self.active else { return };
        let Some(frame) = active.camera.latest_frame() else {
            return;
        };

        let video_rect = fit(rect, frame.width(), frame.height());
        let upload = if self.display.last_seq != Some(frame.seq()) {
            self.display.on_new_frame(frame.seq());
            Some(frame.clone())
        } else {
            None
        };
        let upload = upload.map(|frame| frame as Arc<dyn Nv12Frame>);
        painter.add(VideoPaintCallback::shape(video_rect, self.view, upload));

        if debug_overlay {
            let stats = active.camera.stats();
            let lines = [
                (format!("{}", active.selection.mode), Color32::WHITE),
                (
                    format!("Capture  {:5.1} fps  {} frames", stats.fps, stats.captured),
                    Color32::WHITE,
                ),
                (
                    format!("Dropped  {}  late {}", stats.dropped, stats.late),
                    if stats.dropped > 0 {
                        Color32::from_rgb(0xff, 0x60, 0x60)
                    } else {
                        Color32::WHITE
                    },
                ),
                (
                    format!(
                        "Display  {:5.1} fps  {} skipped",
                        self.display.fps, self.display.skipped
                    ),
                    Color32::WHITE,
                ),
            ];
            // Bottom right, so the burned-in frame counter of the synthetic camera (top
            // left) stays visible.
            draw_overlay(
                painter,
                video_rect.right_bottom() - egui::vec2(8.0, 8.0),
                &lines,
            );
        }
    }
}

/// Caption of a camera tile (number and name), bottom left.
pub fn caption(painter: &egui::Painter, rect: Rect, text: &str) {
    let galley =
        painter.layout_no_wrap(text.to_owned(), FontId::proportional(14.0), Color32::WHITE);
    let pos = rect.left_bottom() + egui::vec2(8.0, -8.0 - galley.size().y);
    painter.rect_filled(
        Rect::from_min_size(
            pos - egui::vec2(4.0, 2.0),
            galley.size() + egui::vec2(8.0, 4.0),
        ),
        3.0,
        Color32::from_black_alpha(170),
    );
    painter.galley(pos, galley, Color32::WHITE);
}

/// Largest rectangle with the video's aspect ratio, centred in `outer`.
fn fit(outer: Rect, width: u32, height: u32) -> Rect {
    if width == 0 || height == 0 {
        return outer;
    }
    let aspect = width as f32 / height as f32;
    let size = if outer.width() / outer.height() > aspect {
        egui::vec2(outer.height() * aspect, outer.height())
    } else {
        egui::vec2(outer.width(), outer.width() / aspect)
    };
    Rect::from_center_size(outer.center(), size)
}

/// Draws the text lines in a box whose bottom right corner is `corner`.
fn draw_overlay(painter: &egui::Painter, corner: egui::Pos2, lines: &[(String, Color32)]) {
    let font = FontId::monospace(13.0);
    let galleys: Vec<_> = lines
        .iter()
        .map(|(text, color)| painter.layout_no_wrap(text.clone(), font.clone(), *color))
        .collect();
    let width = galleys.iter().map(|g| g.size().x).fold(0.0, f32::max);
    let line_height = galleys.first().map_or(0.0, |g| g.size().y);
    let padding = 6.0;
    let size =
        egui::vec2(width, line_height * galleys.len() as f32) + egui::vec2(padding, padding) * 2.0;
    let origin = corner - size;
    let background = Rect::from_min_size(origin, size);
    painter.rect_filled(background, 4.0, Color32::from_black_alpha(170));
    for (i, galley) in galleys.into_iter().enumerate() {
        let pos = origin + egui::vec2(padding, padding + i as f32 * line_height);
        painter.galley(pos, galley, Color32::WHITE);
    }
}

/// Frames shown by the UI. "Skipped" counts preview frames that were replaced by a
/// newer one before the UI drew them (display slower than capture).
#[derive(Default)]
struct DisplayStats {
    last_seq: Option<u64>,
    skipped: u64,
    fps: f64,
    window_start: Option<Instant>,
    window_frames: u32,
}

impl DisplayStats {
    fn on_new_frame(&mut self, seq: u64) {
        if let Some(last) = self.last_seq {
            self.skipped += seq.saturating_sub(last + 1);
        }
        self.last_seq = Some(seq);

        let now = Instant::now();
        let start = *self.window_start.get_or_insert(now);
        self.window_frames += 1;
        let elapsed = now.duration_since(start).as_secs_f64();
        if elapsed >= 1.0 {
            self.fps = f64::from(self.window_frames - 1) / elapsed;
            self.window_start = Some(now);
            self.window_frames = 1;
        }
    }
}
