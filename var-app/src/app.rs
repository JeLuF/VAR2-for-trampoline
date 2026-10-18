//! Application shell: menu bar, camera grid, footer, confirmation dialogs, and the
//! configuration.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use tracing::{error, info, warn};
use var_capture::{DeviceInfo, DeviceWatcher, WatcherOptions, synthetic};

use crate::config::{self, CameraConfig, Config, RemoteControlConfig};
use crate::disk::{self, DiskMonitor, Level};
use crate::drawing;
use crate::inhibit::SleepInhibitor;
use crate::live;
use crate::live::{LiveCamera, Selection};
use crate::lock::{self, Lock, UnlockError};
use crate::monitor::ResourceMonitor;
use crate::recording::{self, Recorder};
use crate::remote::RemoteControl;
use crate::review::{MARKER_COLOR, ReviewAction, ReviewMode};
use crate::setup::{self, ERROR_COLOR, MenuBar, RECORDING_COLOR, SetupAction, WARNING_COLOR};
use crate::shortcuts::{self, Action};
use crate::video::VideoRenderer;
use var_playback::SPEEDS;

/// Maximum number of camera slots; at least 2 are required (CAP-001).
pub const MAX_CAMERAS: usize = 4;
/// Number of camera slots without a saved configuration.
const DEFAULT_CAMERAS: usize = 2;
/// "Review" starts this far before the newest recorded frame.
const REVIEW_START_BEFORE_LIVE_US: i64 = 5_000_000;
/// How long a notice (e.g. "Marker set") is shown.
const NOTICE_DURATION: Duration = Duration::from_millis(2500);

/// A question that needs the operator's confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialog {
    /// "Stop recording" pressed (EXP-002).
    ConfirmStop,
    /// Window closed while recording (SYS-001a).
    ConfirmQuit,
    /// "Unlock settings": asks for the PIN (ROLE-006).
    Unlock,
    /// "Set PIN" / "Change PIN" (ROLE-007).
    SetPin,
}

/// Text fields of the PIN dialogs.
#[derive(Default)]
struct PinInput {
    pin: String,
    repeat: String,
    error: Option<String>,
    /// The PIN field still has to get the keyboard focus (dialog just opened).
    focus_pin: bool,
}

pub struct VarApp {
    devices: DeviceWatcher,
    cameras: Vec<LiveCamera>,
    /// Camera shown full screen instead of the grid (VIEW-002).
    full_screen: Option<usize>,
    recorder: Recorder,
    disk: DiskMonitor,
    lock: Lock,
    /// Keeps the system awake while recording (SYS-002).
    sleep_inhibitor: Option<SleepInhibitor>,
    resource_monitor: ResourceMonitor,
    /// Remote control for Bitfocus Companion (INP-003).
    remote: RemoteControl,
    /// The keyboard shortcut overlay is shown (F1, INP-002).
    help_open: bool,
    /// The window covers the whole screen, without title bar and taskbar.
    window_full_screen: bool,
    /// The saved window mode was applied to the window (first frame).
    window_mode_applied: bool,
    /// Full screen state last reported by the window.
    observed_full_screen: Option<bool>,
    /// Laser pointer trails fade out after this many seconds (DRW-002).
    laser_seconds: f32,
    dialog: Option<Dialog>,
    pin_input: PinInput,
    /// Quitting was confirmed; the next close request is let through.
    quit_confirmed: bool,
    /// Devices to select per slot as soon as they are listed (`--camera`, `--mode`).
    pending_cameras: Vec<Option<(String, Option<String>)>>,
    auto_record: AutoRecord,
    debug_overlay: bool,
    /// Review of the running recording; `None` in live mode.
    review: Option<ReviewMode>,
    /// Short message on top of the video, and when it was set.
    notice: Option<(String, Instant)>,
    /// Random review actions for testing (`--review-exercise`): next action time, PRNG state.
    review_exercise: Option<(Instant, u64)>,
    /// Number of synthetic cameras (debug menu).
    synthetic_cameras: u32,
    /// Toggles synthetic camera 2 every this often (`--unplug-cycle`), from this time.
    unplug_cycle: Option<(Duration, Instant)>,
    config_path: PathBuf,
    /// The configuration as last saved; saved again when the state differs.
    saved_config: Config,
    ui_timing: UiTiming,
    gstreamer_version: String,
    renderer: String,
    log_dir: PathBuf,
}

/// Startup options of the application.
pub struct Options {
    pub watcher: WatcherOptions,
    /// Devices for the camera slots, in order (`--camera`).
    pub cameras: Vec<String>,
    /// Modes for `cameras`, e.g. `YUY2:1920x1080@50` (`--mode`).
    pub modes: Vec<String>,
    pub force_software_encoder: bool,
    /// Record once for this long, starting as soon as possible (`--record-seconds`).
    pub record_for: Option<Duration>,
    pub recording_dir: Option<PathBuf>,
    pub segment_length: Option<Duration>,
    pub debug_overlay: bool,
    /// Unplug and replug synthetic camera 2 at this interval (`--unplug-cycle`).
    pub unplug_cycle: Option<Duration>,
    /// Random review actions every 2 s while recording (`--review-exercise`).
    pub review_exercise: bool,
    /// Behave as if the recording drive had this many bytes free (`--simulate-free-gb`).
    pub simulated_free: Option<u64>,
    /// Interval of the resource log (`--resource-log-seconds`).
    pub resource_log_interval: Duration,
    /// Enable the remote control on this port (`--remote-control-port`).
    pub remote_control_port: Option<u16>,
    pub config_path: PathBuf,
    pub log_dir: PathBuf,
}

/// UI frame rate and the CPU time of `update` per frame (debug overlay).
#[derive(Default)]
struct UiTiming {
    window_start: Option<Instant>,
    frames: u32,
    update_time: Duration,
    fps: f64,
    update_ms: f64,
}

impl UiTiming {
    fn on_frame(&mut self, update_time: Duration) {
        let now = Instant::now();
        let start = *self.window_start.get_or_insert(now);
        self.frames += 1;
        self.update_time += update_time;
        let elapsed = now.duration_since(start);
        if elapsed >= Duration::from_secs(1) {
            self.fps = f64::from(self.frames) / elapsed.as_secs_f64();
            self.update_ms = self.update_time.as_secs_f64() * 1000.0 / f64::from(self.frames);
            *self = UiTiming {
                fps: self.fps,
                update_ms: self.update_ms,
                window_start: Some(now),
                ..Default::default()
            };
        }
    }
}

/// State of the automatic test recording (`--record-seconds`).
enum AutoRecord {
    Off,
    Waiting(Duration),
    Recording(Duration, Instant),
    Done,
}

impl VarApp {
    pub fn new(cc: &eframe::CreationContext<'_>, options: Options) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("the application runs with the wgpu renderer");
        let adapter = render_state.adapter.get_info();
        let renderer = format!("{} ({:?})", adapter.name, adapter.backend);
        info!(%renderer, target_format = ?render_state.target_format, "window opened");
        VideoRenderer::register(render_state);

        let synthetic_cameras = options.watcher.synthetic_cameras;
        let ctx = cc.egui_ctx.clone();
        let devices = DeviceWatcher::start(options.watcher, move || ctx.request_repaint());

        // Restore the configuration; command line options take precedence.
        let config = config::load(&options.config_path);
        let mut recorder = Recorder::new(&cc.egui_ctx, options.force_software_encoder);
        if let Some(folder) = &config.recording_folder {
            recorder.folder = folder.clone();
        }
        if let Some(minutes) = config.segment_minutes {
            recorder.segment_length = Duration::from_secs(u64::from(minutes.clamp(1, 60)) * 60);
        }
        if let Some(dir) = options.recording_dir {
            recorder.folder = dir.to_string_lossy().into_owned();
        }
        if let Some(length) = options.segment_length {
            recorder.segment_length = length;
        }

        let count = config
            .camera_count
            .unwrap_or(DEFAULT_CAMERAS)
            .max(options.cameras.len())
            .clamp(1, MAX_CAMERAS);
        let mut cameras: Vec<LiveCamera> = (0..count)
            .map(|slot| LiveCamera::new(slot as u64))
            .collect();
        let pending_cameras = (0..count)
            .map(|slot| {
                options
                    .cameras
                    .get(slot)
                    .map(|id| (id.clone(), options.modes.get(slot).cloned()))
            })
            .collect();
        if options.cameras.is_empty() {
            // The cameras of the last session; a device that is not connected yet is
            // shown as disconnected and starts when it appears (CAP-007).
            for (camera, saved) in cameras.iter_mut().zip(&config.cameras) {
                if let Some(saved) = saved {
                    camera.select(Some(Selection {
                        device_id: saved.device_id.clone(),
                        device_name: saved.device_name.clone(),
                        mode: saved.mode.clone(),
                    }));
                }
            }
        }

        let mut disk = DiskMonitor::new(&recorder.folder);
        disk.simulated_free = options.simulated_free;
        if let Some([first, second]) = config.disk_warning_minutes {
            let first = first.clamp(2, 24 * 60);
            disk.warning_minutes = [first, second.clamp(1, first - 1)];
        }
        // Always unlocked at start (ROLE-001).
        let lock = Lock::new(config.pin_hash.clone());
        // Remote control: off unless enabled in the configuration or on the command line.
        let mut remote_settings = config.remote_control.unwrap_or(RemoteControlConfig {
            enabled: false,
            port: var_control::DEFAULT_PORT,
        });
        if let Some(port) = options.remote_control_port {
            remote_settings = RemoteControlConfig {
                enabled: true,
                port,
            };
        }
        let remote = RemoteControl::new(&cc.egui_ctx, remote_settings);

        VarApp {
            devices,
            cameras,
            full_screen: None,
            recorder,
            disk,
            lock,
            sleep_inhibitor: None,
            resource_monitor: ResourceMonitor::new(options.resource_log_interval),
            remote,
            help_open: false,
            window_full_screen: config.window_full_screen,
            window_mode_applied: false,
            observed_full_screen: None,
            laser_seconds: config
                .laser_seconds
                .unwrap_or(drawing::DEFAULT_LIFETIME_S)
                .clamp(
                    *drawing::LIFETIME_RANGE_S.start(),
                    *drawing::LIFETIME_RANGE_S.end(),
                ),
            dialog: None,
            pin_input: PinInput::default(),
            quit_confirmed: false,
            pending_cameras,
            auto_record: options
                .record_for
                .map_or(AutoRecord::Off, AutoRecord::Waiting),
            debug_overlay: options.debug_overlay || config.debug_overlay,
            review: None,
            notice: None,
            review_exercise: options
                .review_exercise
                .then(|| (Instant::now(), 0x9E37_79B9_7F4A_7C15)),
            synthetic_cameras,
            unplug_cycle: options
                .unplug_cycle
                .map(|interval| (interval, Instant::now())),
            config_path: options.config_path,
            saved_config: config,
            ui_timing: UiTiming::default(),
            gstreamer_version: var_capture::gstreamer_version(),
            renderer,
            log_dir: options.log_dir,
        }
    }

    /// Selects the devices given on the command line once they are listed.
    fn select_pending_cameras(&mut self, devices: &[DeviceInfo]) {
        for (slot, pending) in self.pending_cameras.iter_mut().enumerate() {
            let Some((id, mode)) = pending else { continue };
            let Some(device) = devices.iter().find(|d| &d.id == id) else {
                continue;
            };
            let selection = match mode {
                Some(spec) => {
                    let selection = setup::selection_with_mode(device, spec);
                    if selection.is_none() {
                        error!(mode = %spec, device = %device.name, "mode not offered by the device");
                    }
                    selection
                }
                None => setup::default_selection(device),
            };
            self.cameras[slot].select(selection);
            *pending = None;
        }
    }

    /// The current configuration, to be saved when it changes.
    fn current_config(&self) -> Config {
        Config {
            camera_count: Some(self.cameras.len()),
            cameras: self
                .cameras
                .iter()
                .map(|camera| {
                    camera.selection().map(|s| CameraConfig {
                        device_id: s.device_id.clone(),
                        device_name: s.device_name.clone(),
                        mode: s.mode.clone(),
                    })
                })
                .collect(),
            recording_folder: Some(self.recorder.folder.clone()),
            segment_minutes: Some((self.recorder.segment_length.as_secs() / 60).max(1) as u32),
            debug_overlay: self.debug_overlay,
            window_full_screen: self.window_full_screen,
            laser_seconds: Some(self.laser_seconds),
            disk_warning_minutes: Some(self.disk.warning_minutes),
            pin_hash: self.lock.pin_hash().map(str::to_owned),
            remote_control: Some(self.remote.settings),
        }
    }

    fn save_config_if_changed(&mut self) {
        let config = self.current_config();
        if config != self.saved_config {
            match config::save(&self.config_path, &config) {
                Ok(()) => info!(path = %self.config_path.display(), "configuration saved"),
                Err(e) => {
                    warn!(path = %self.config_path.display(), %e, "cannot save the configuration")
                }
            }
            // Also on error: don't retry every frame.
            self.saved_config = config;
        }
    }

    /// Changes the number of camera slots (1–4). Removed slots stop their cameras in
    /// the background. Not while recording: the recorded cameras would change (CAP-010).
    fn set_camera_count(&mut self, count: usize) {
        let count = count.clamp(1, MAX_CAMERAS);
        if count == self.cameras.len() || self.recorder.is_recording() {
            return;
        }
        info!(count, "number of cameras changed");
        while self.cameras.len() < count {
            self.cameras
                .push(LiveCamera::new(self.cameras.len() as u64));
        }
        for camera in self.cameras.drain(count..) {
            camera.release();
        }
        self.pending_cameras.resize(count, None);
        if self.full_screen.is_some_and(|slot| slot >= count) {
            self.full_screen = None;
        }
    }

    /// Switches to review of the running recording (REV-001), paused `before_live_us`
    /// before the newest recorded frame. Returns whether review is active.
    fn enter_review(&mut self, ctx: &egui::Context, before_live_us: i64) -> bool {
        if self.review.is_some() {
            return true;
        }
        let sources = self.recorder.review_sources();
        if sources.is_empty() {
            return false;
        }
        info!(cameras = sources.len(), "review mode");
        self.review = Some(ReviewMode::new(sources, before_live_us, ctx));
        true
    }

    /// Performs a user action from a shortcut, a menu or a button. Review functions
    /// used in live mode switch to review first, at the live edge, so e.g. "←" right
    /// after something happened shows it frame by frame.
    fn perform(&mut self, action: Action, ctx: &egui::Context) {
        let reference = self.full_screen;
        match action {
            Action::FullScreen(slot) if slot < self.cameras.len() => {
                self.full_screen = (self.full_screen != Some(slot)).then_some(slot);
            }
            Action::FullScreen(_) => {}
            Action::ExitFullScreen if self.help_open => self.help_open = false,
            Action::ExitFullScreen => self.full_screen = None,
            Action::Help => self.help_open = !self.help_open,
            Action::WindowFullScreen => self.set_window_full_screen(!self.window_full_screen, ctx),
            Action::Pause => {
                if let Some(review) = &mut self.review {
                    review.player().pause();
                }
            }
            Action::Speed(thousandths) => {
                let speed = f64::from(thousandths) / 1000.0;
                if let Some(review) = &mut self.review
                    && SPEEDS.contains(&speed)
                {
                    review.player().set_speed(speed);
                }
            }
            Action::Review => {
                self.enter_review(ctx, REVIEW_START_BEFORE_LIVE_US);
            }
            Action::Live => self.back_to_live(),
            Action::SetMarker => self.set_marker(),
            Action::PreviousMarker | Action::NextMarker => {
                let next = action == Action::NextMarker;
                let position = match &mut self.review {
                    Some(review) => review.player().position(),
                    None => match self.recorder.now_wall_us() {
                        Some(now) => now,
                        None => return,
                    },
                };
                // 1 ms tolerance, so a marker at the position itself is skipped.
                let from = position + if next { 1_000 } else { -1_000 };
                let Some(wall) = self.recorder.adjacent_marker(from, next).map(|m| m.wall_us)
                else {
                    return;
                };
                self.jump_to(wall, ctx);
            }
            Action::PlayPause
            | Action::PlayReverse
            | Action::Step(_)
            | Action::Jump(_)
            | Action::Faster
            | Action::Slower => {
                let before_live = if action == Action::PlayPause {
                    REVIEW_START_BEFORE_LIVE_US
                } else {
                    0
                };
                if !self.enter_review(ctx, before_live) {
                    return;
                }
                let player = self.review.as_mut().unwrap().player();
                match action {
                    Action::PlayPause => player.toggle(),
                    Action::PlayReverse => player.toggle_reverse(),
                    Action::Step(frames) => player.step(frames, reference),
                    Action::Jump(seconds) => player.jump(seconds),
                    Action::Faster => player.change_speed(true),
                    Action::Slower => player.change_speed(false),
                    _ => {}
                }
            }
        }
    }

    /// Full screen window: no title bar, the taskbar hidden. The menu bar of the
    /// application stays, with the recording status (VIEW-005).
    fn set_window_full_screen(&mut self, on: bool, ctx: &egui::Context) {
        info!(on, "full screen window");
        self.window_full_screen = on;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
    }

    /// Shows the recording at `wall_us` (marker list, next/previous marker).
    fn jump_to(&mut self, wall_us: i64, ctx: &egui::Context) {
        if self.enter_review(ctx, 0) {
            let player = self.review.as_mut().unwrap().player();
            player.pause();
            player.seek(wall_us);
        }
    }

    /// Sets a marker: in live mode at the current time, in review mode at the shown
    /// moment (MRK-001).
    fn set_marker(&mut self) {
        let (wall, set_in) = match &mut self.review {
            Some(review) => (Some(review.player().position()), "review"),
            None => (self.recorder.now_wall_us(), "live"),
        };
        let Some(wall) = wall else {
            self.notice("Markers can be set while recording");
            return;
        };
        if let Some((marker, saved)) = self.recorder.add_marker(wall, set_in) {
            let text = match saved {
                Ok(()) => format!("Marker {} set at {}", marker.number, &marker.time[11..23]),
                Err(e) => format!("Marker {} set, but not saved: {e}", marker.number),
            };
            self.notice(&text);
        }
    }

    /// Shows a short message on top of the video.
    fn notice(&mut self, text: &str) {
        self.notice = Some((text.to_owned(), Instant::now()));
    }

    fn show_notice(&mut self, ctx: &egui::Context) {
        let Some((text, since)) = &self.notice else {
            return;
        };
        if since.elapsed() > NOTICE_DURATION {
            self.notice = None;
            return;
        }
        egui::Area::new(egui::Id::new("notice"))
            .anchor(egui::Align2::CENTER_TOP, [0.0, 48.0])
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(egui::RichText::new(text).size(16.0).color(MARKER_COLOR));
                });
            });
        ctx.request_repaint_after(NOTICE_DURATION);
    }

    /// Returns to the live view with one action (REV-009).
    fn back_to_live(&mut self) {
        if self.review.take().is_some() {
            info!("live mode");
        }
    }

    /// Debug: random review use every 2 s while recording (`--review-exercise`), to
    /// test that review has no influence on the recording (REV-010, REC-003).
    fn review_exercise(&mut self, ctx: &egui::Context) {
        let Some((next, mut state)) = self.review_exercise else {
            return;
        };
        if !self.recorder.is_recording() || Instant::now() < next {
            return;
        }
        // xorshift64
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        self.review_exercise = Some((Instant::now() + Duration::from_secs(2), state));

        let choice = state % 6;
        match (&mut self.review, choice) {
            (None, _) => {
                self.enter_review(ctx, REVIEW_START_BEFORE_LIVE_US);
            }
            (Some(_), 0) => self.back_to_live(),
            (Some(review), 1 | 2) => {
                if let Some((start, end)) = review.player().range() {
                    let span = (end - start).max(1) as u64;
                    review.player().seek(start + ((state >> 8) % span) as i64);
                }
            }
            (Some(review), 3) => review.player().toggle(),
            (Some(_), _) => {
                let slot = (state >> 16) as usize % self.cameras.len();
                self.full_screen = (self.full_screen != Some(slot)).then_some(slot);
            }
        }
    }

    /// Debug: unplug and replug synthetic camera 2 periodically (`--unplug-cycle`).
    fn unplug_cycle(&mut self) {
        if let Some((interval, start)) = self.unplug_cycle {
            let phase = start.elapsed().as_secs_f64() / interval.as_secs_f64();
            synthetic::set_connected(2, (phase as u64).is_multiple_of(2));
        }
    }

    fn auto_record(&mut self) {
        match self.auto_record {
            AutoRecord::Waiting(duration)
                if self.pending_cameras.iter().all(Option::is_none)
                    && self.recorder.blocked_reason(&self.cameras).is_none() =>
            {
                info!(seconds = duration.as_secs(), "automatic test recording");
                self.recorder.start(&self.cameras);
                self.auto_record = if self.recorder.is_recording() {
                    AutoRecord::Recording(duration, Instant::now())
                } else {
                    AutoRecord::Done
                };
            }
            AutoRecord::Recording(duration, started) if started.elapsed() >= duration => {
                self.recorder.stop(&self.cameras);
                self.auto_record = AutoRecord::Done;
            }
            _ => {}
        }
    }

    /// The live grid, or one camera full screen (VIEW-001, VIEW-002, VIEW-003).
    /// Right-clicking a camera opens its menu (device and mode).
    fn camera_grid(&mut self, ui: &mut egui::Ui, devices: &[DeviceInfo]) {
        let area = ui.available_rect_before_wrap();
        let tiles: Vec<(usize, egui::Rect)> = match self.full_screen {
            Some(slot) => vec![(slot, area)],
            None => grid_tiles(area, self.cameras.len()),
        };
        for (slot, rect) in tiles {
            let caption = match self.cameras[slot].selection() {
                Some(s) => format!("{}  {}", slot + 1, s.device_name),
                None => format!("{}", slot + 1),
            };
            let response = match &mut self.review {
                // Review mode: recorded video only, no live view (VIEW-004).
                Some(review) => {
                    let rect = rect.shrink(1.0);
                    // Click: full screen; drag: laser pointer (DRW-001).
                    let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                    let painter = ui.painter_at(rect);
                    review.tile(&painter, rect, slot, self.debug_overlay, &response);
                    live::caption(&painter, rect, &format!("{caption}  (review)"));
                    response
                }
                None => {
                    let interrupted = self.recorder.is_interrupted(slot);
                    self.cameras[slot].show(
                        ui,
                        rect.shrink(1.0),
                        &caption,
                        self.debug_overlay,
                        interrupted,
                    )
                }
            };
            if response.clicked() {
                self.full_screen = (self.full_screen != Some(slot)).then_some(slot);
            }
            let recording = self.recorder.is_recording();
            let locked = self.lock.is_locked();
            response.context_menu(|ui| {
                setup::camera_menu(ui, slot, devices, &mut self.cameras, recording, locked);
            });
        }
    }

    /// Window close, Alt+F4, Cmd+Q or "Quit": refused in locked mode (SYS-001); while
    /// recording only after confirmation (SYS-001a). Returns whether to quit now.
    fn may_quit(&mut self) -> bool {
        if self.quit_confirmed {
            return true;
        }
        if self.lock.is_locked() {
            info!("quit refused: settings are locked");
            self.notice("Settings are locked: unlock to quit");
            return false;
        }
        if self.recorder.is_recording() {
            info!("quit requested while recording");
            self.dialog = Some(Dialog::ConfirmQuit);
            return false;
        }
        true
    }

    /// Handles a menu action that changes the mode or the PIN.
    fn lock_action(&mut self, action: &SetupAction) {
        match action {
            SetupAction::Lock => {
                if self.lock.lock() {
                    self.notice("Settings locked");
                }
            }
            SetupAction::UnlockRequested => self.open_pin_dialog(Dialog::Unlock),
            SetupAction::SetPinRequested if !self.lock.is_locked() => {
                self.open_pin_dialog(Dialog::SetPin);
            }
            SetupAction::RemovePin => {
                self.lock.remove_pin();
                self.notice("PIN removed: locking is not possible without a PIN");
            }
            _ => {}
        }
    }

    fn open_pin_dialog(&mut self, dialog: Dialog) {
        self.pin_input = PinInput {
            focus_pin: true,
            ..PinInput::default()
        };
        self.dialog = Some(dialog);
    }

    /// The PIN dialogs: unlock (ROLE-006, the only PIN prompt) and set PIN.
    fn show_pin_dialog(&mut self, ctx: &egui::Context, dialog: Dialog) {
        let unlock = dialog == Dialog::Unlock;
        let input = &mut self.pin_input;
        let response = egui::Modal::new(egui::Id::new("pin")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.heading(if unlock { "Unlock settings" } else { "Set PIN" });
            ui.add_space(8.0);
            ui.label(if unlock {
                "Enter the PIN to unlock the settings."
            } else {
                "The PIN is needed to unlock the settings after locking them."
            });
            ui.add_space(8.0);
            let pin = ui.add(
                egui::TextEdit::singleline(&mut input.pin)
                    .password(true)
                    .hint_text("PIN"),
            );
            if std::mem::take(&mut input.focus_pin) {
                pin.request_focus();
            }
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let mut submit = pin.lost_focus() && enter && unlock;
            if !unlock {
                let repeat = ui.add(
                    egui::TextEdit::singleline(&mut input.repeat)
                        .password(true)
                        .hint_text("Repeat the PIN"),
                );
                if pin.lost_focus() && enter {
                    repeat.request_focus();
                }
                submit |= repeat.lost_focus() && enter;
                ui.weak(format!("At least {} characters.", lock::MIN_PIN_LENGTH));
            }
            if let Some(error) = &input.error {
                ui.colored_label(ERROR_COLOR, error);
            }
            ui.add_space(12.0);
            let mut answer = None;
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    answer = Some(false);
                }
                if ui
                    .button(if unlock { "Unlock" } else { "Set PIN" })
                    .clicked()
                    || submit
                {
                    answer = Some(true);
                }
            });
            answer
        });
        let answer = response
            .inner
            .or_else(|| response.should_close().then_some(false));

        match answer {
            Some(true) if unlock => match self.lock.unlock(&self.pin_input.pin) {
                Ok(()) => {
                    self.dialog = None;
                    self.pin_input = PinInput::default();
                    self.notice("Settings unlocked");
                }
                Err(e) => {
                    self.pin_input.pin.clear();
                    self.pin_input.focus_pin = true;
                    self.pin_input.error = Some(match e {
                        UnlockError::WrongPin => "Wrong PIN.".to_owned(),
                        UnlockError::Wait(wait) => format!(
                            "Too many wrong PINs. Try again in {} s.",
                            wait.as_secs().max(1)
                        ),
                    });
                }
            },
            Some(true) => {
                let input = &mut self.pin_input;
                let result = if input.pin != input.repeat {
                    Err("The PINs don't match.".to_owned())
                } else {
                    self.lock.set_pin(&input.pin)
                };
                match result {
                    Ok(()) => {
                        self.dialog = None;
                        self.pin_input = PinInput::default();
                        self.notice("PIN set: settings can now be locked");
                    }
                    Err(e) => {
                        input.error = Some(e);
                        input.pin.clear();
                        input.repeat.clear();
                        input.focus_pin = true;
                    }
                }
            }
            Some(false) => {
                self.dialog = None;
                self.pin_input = PinInput::default();
            }
            None => {}
        }
    }

    /// Warning banner below the menu bar when the remaining recording time is below a
    /// threshold (DSK-003). Shown in all modes; can be dismissed until the next
    /// threshold.
    fn disk_warning(&mut self, ctx: &egui::Context) {
        let Some(level) = self.disk.warning() else {
            return;
        };
        let estimate = self.disk.estimate();
        let remaining = estimate
            .remaining
            .map(disk::format_remaining)
            .unwrap_or_default();
        let free = estimate.free.map(disk::format_bytes).unwrap_or_default();
        let (fill, text) = match level {
            Level::Critical => (
                ERROR_COLOR,
                format!(
                    "Disk almost full: only about {remaining} of recording left ({free} free)."
                ),
            ),
            _ => (
                WARNING_COLOR,
                format!("Disk space low: about {remaining} of recording left ({free} free)."),
            ),
        };
        let mut dismiss = false;
        egui::TopBottomPanel::top("disk warning")
            .frame(
                egui::Frame::NONE
                    .fill(fill)
                    .inner_margin(egui::Margin::symmetric(8, 4)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(text)
                            .strong()
                            .size(15.0)
                            .color(egui::Color32::BLACK),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        dismiss = ui.button("Dismiss").clicked();
                    });
                });
            });
        if dismiss {
            info!(?level, "disk space warning dismissed");
            self.disk.dismiss();
        }
    }

    /// Prevents sleep and the screen saver while recording (SYS-002).
    fn update_sleep_inhibitor(&mut self) {
        match (self.recorder.is_recording(), self.sleep_inhibitor.is_some()) {
            (true, false) => self.sleep_inhibitor = Some(SleepInhibitor::new()),
            (false, true) => self.sleep_inhibitor = None,
            _ => {}
        }
    }

    fn show_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.dialog else { return };
        let (title, text, confirm) = match dialog {
            Dialog::ConfirmStop => (
                "Stop recording?",
                "The recording of all cameras ends. Starting again creates a new recording.",
                "Stop recording",
            ),
            Dialog::ConfirmQuit => (
                "Quit while recording?",
                "Quitting stops the recording.",
                "Stop recording and quit",
            ),
            Dialog::Unlock | Dialog::SetPin => return self.show_pin_dialog(ctx, dialog),
        };

        let response = egui::Modal::new(egui::Id::new("confirmation")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading(title);
            ui.add_space(8.0);
            ui.label(text);
            ui.add_space(12.0);
            let mut answer = None;
            ui.horizontal(|ui| {
                // Cancel first: the safe choice gets the focus.
                if ui.button("Cancel").clicked() {
                    answer = Some(false);
                }
                if ui
                    .button(egui::RichText::new(confirm).color(RECORDING_COLOR))
                    .clicked()
                {
                    answer = Some(true);
                }
            });
            answer
        });
        let answer = response
            .inner
            .or_else(|| response.should_close().then_some(false));

        match answer {
            // Stop only in unlocked mode (EXP-002).
            Some(true) if self.lock.is_locked() => self.dialog = None,
            Some(true) => {
                self.dialog = None;
                self.recorder.stop(&self.cameras);
                if dialog == Dialog::ConfirmQuit {
                    info!("quit confirmed while recording");
                    self.quit_confirmed = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Some(false) => {
                info!(?dialog, "cancelled");
                self.dialog = None;
            }
            None => {}
        }
    }
}

/// Tiles for `count` cameras: a grid with as many columns as needed for a roughly
/// square arrangement (2 cameras: side by side).
fn grid_tiles(area: egui::Rect, count: usize) -> Vec<(usize, egui::Rect)> {
    let columns = (count as f32).sqrt().ceil().max(1.0) as usize;
    let rows = count.div_ceil(columns).max(1);
    let size = egui::vec2(area.width() / columns as f32, area.height() / rows as f32);
    (0..count)
        .map(|i| {
            let min =
                area.min + egui::vec2((i % columns) as f32 * size.x, (i / columns) as f32 * size.y);
            (i, egui::Rect::from_min_size(min, size))
        })
        .collect()
}

impl eframe::App for VarApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let update_start = Instant::now();
        // macOS: Cmd+Q, which the (disabled) default menu would handle, as a close
        // request.
        if cfg!(target_os = "macos")
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Q))
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        // Window close, Alt+F4 (SYS-001, SYS-001a).
        if ctx.input(|i| i.viewport().close_requested()) && !self.may_quit() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }

        // The saved window mode on the first frame; afterwards follow the window, in
        // case the OS ended full screen (e.g. a display change).
        if !self.window_mode_applied {
            self.window_mode_applied = true;
            if self.window_full_screen {
                self.set_window_full_screen(true, ctx);
            }
        }
        if let Some(actual) = ctx.input(|i| i.viewport().fullscreen)
            && self.observed_full_screen != Some(actual)
        {
            // Only a change of the window's state counts: right after a toggle, the
            // window may still report the old state for a few frames.
            if self.observed_full_screen.is_some() {
                self.window_full_screen = actual;
            }
            self.observed_full_screen = Some(actual);
        }
        self.unplug_cycle();
        let devices = self.devices.devices();
        self.select_pending_cameras(&devices);
        for camera in &mut self.cameras {
            camera.update(&devices, ctx);
        }
        self.recorder.update(&self.cameras);
        self.auto_record();
        self.update_sleep_inhibitor();
        self.resource_monitor.update(&self.cameras, &self.recorder);
        self.disk.update(
            &self.recorder.folder,
            recording::configured_bytes_per_second(&self.cameras),
            self.recorder.disk_bytes(&self.cameras),
        );
        self.review_exercise(ctx);
        // Keyboard shortcuts (INP-001), unless a text field or a dialog has the keys.
        if !ctx.wants_keyboard_input() && self.dialog.is_none() {
            for action in ctx.input_mut(shortcuts::read) {
                self.perform(action, ctx);
            }
        }
        // Remote control (INP-003): the same actions; ignored while a dialog is open,
        // like the keys.
        for action in self.remote.update() {
            if self.dialog.is_none() {
                self.perform(action, ctx);
            } else {
                info!(?action, "remote control command ignored: dialog open");
            }
        }
        // Review is of the running recording; it ends with the recording.
        if self.review.is_some() && !self.recorder.is_recording() {
            info!("recording stopped: back to live");
            self.review = None;
        }
        if let Some(review) = &mut self.review {
            review.set_drawing_lifetime(self.laser_seconds);
            review.tick();
        }

        egui::TopBottomPanel::top("menu bar").show(ctx, |ui| {
            let mut camera_count = self.cameras.len();
            let action = setup::menu_bar(
                ui,
                MenuBar {
                    cameras: &self.cameras,
                    recorder: &mut self.recorder,
                    disk: &mut self.disk,
                    remote: &mut self.remote,
                    locked: self.lock.is_locked(),
                    has_pin: self.lock.has_pin(),
                    debug_overlay: &mut self.debug_overlay,
                    window_full_screen: self.window_full_screen,
                    laser_seconds: &mut self.laser_seconds,
                    camera_count: &mut camera_count,
                    synthetic_cameras: self.synthetic_cameras,
                    reviewing: self.review.is_some(),
                },
            );
            match action {
                SetupAction::StopRequested if !self.lock.is_locked() => {
                    self.dialog = Some(Dialog::ConfirmStop);
                }
                SetupAction::StopRequested => {}
                SetupAction::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                SetupAction::Help => self.help_open = true,
                SetupAction::WindowFullScreen => {
                    self.set_window_full_screen(!self.window_full_screen, ctx);
                }
                SetupAction::Lock
                | SetupAction::UnlockRequested
                | SetupAction::SetPinRequested
                | SetupAction::RemovePin => self.lock_action(&action),
                SetupAction::Review => {
                    self.enter_review(ctx, REVIEW_START_BEFORE_LIVE_US);
                }
                SetupAction::BackToLive => self.back_to_live(),
                SetupAction::SetMarker => self.set_marker(),
                SetupAction::JumpTo(wall) => self.jump_to(wall, ctx),
                SetupAction::None => {}
            }
            if !self.lock.is_locked() {
                self.set_camera_count(camera_count);
            }
        });
        self.disk_warning(ctx);

        egui::TopBottomPanel::bottom("footer").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.weak(format!("Version {}", var_core::VERSION));
                ui.separator();
                ui.weak(&self.gstreamer_version);
                ui.separator();
                ui.weak(format!("Renderer: {}", self.renderer));
                ui.separator();
                ui.weak(format!("Log: {}", self.log_dir.display()));
                if self.debug_overlay {
                    ui.separator();
                    ui.weak(format!(
                        "UI {:.0} fps, {:.1} ms/frame",
                        self.ui_timing.fps, self.ui_timing.update_ms
                    ));
                }
            });
        });

        // Review controls above the footer.
        let mut back_to_live = false;
        if let Some(review) = &mut self.review {
            egui::TopBottomPanel::bottom("review").show(ctx, |ui| {
                let markers = self.recorder.markers();
                back_to_live = matches!(
                    review.bar(ui, markers, self.full_screen),
                    ReviewAction::BackToLive
                );
            });
        }
        if back_to_live {
            self.back_to_live();
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::from_gray(20)))
            .show(ctx, |ui| self.camera_grid(ui, &devices));

        self.show_dialog(ctx);
        self.show_notice(ctx);
        // INP-002: all keyboard shortcuts, over the video.
        egui::Window::new("Keyboard shortcuts")
            .open(&mut self.help_open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                shortcuts::help_table(ui);
                ui.add_space(6.0);
                ui.weak("F1 or Esc closes this.");
            });

        // Run the watchdog and keep the status current even when no camera delivers
        // frames (which would otherwise trigger the repaints).
        ctx.request_repaint_after(Duration::from_millis(250));
        self.save_config_if_changed();
        self.ui_timing.on_frame(update_start.elapsed());
    }

    fn on_exit(&mut self) {
        // Any other way the application ends: still close the recording cleanly.
        if self.recorder.is_recording() {
            info!("application exiting while recording");
            self.recorder.stop(&self.cameras);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_cameras_side_by_side() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 900.0));
        let tiles = grid_tiles(area, 2);
        assert_eq!(
            tiles[0].1,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(800.0, 900.0))
        );
        assert_eq!(
            tiles[1].1,
            egui::Rect::from_min_max(egui::pos2(800.0, 0.0), egui::pos2(1600.0, 900.0))
        );
        assert_eq!(grid_tiles(area, 4).len(), 4);
    }

    #[test]
    fn three_and_four_cameras_in_two_by_two() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 900.0));
        let quarter = egui::vec2(800.0, 450.0);
        for count in [3, 4] {
            let tiles = grid_tiles(area, count);
            assert_eq!(tiles.len(), count);
            let corners = [(0.0, 0.0), (800.0, 0.0), (0.0, 450.0), (800.0, 450.0)];
            for ((_, rect), (x, y)) in tiles.iter().zip(corners) {
                assert_eq!(*rect, egui::Rect::from_min_size(egui::pos2(x, y), quarter));
            }
        }
        assert_eq!(grid_tiles(area, 1), vec![(0, area)]);
    }
}
