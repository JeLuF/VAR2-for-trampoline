//! Setup: the menu bar (recording, view) with the recording status, and the context
//! menu of each camera tile (device and mode).

use eframe::egui::containers::menu::{MenuButton, MenuConfig};
use eframe::egui::{self, Color32, PopupCloseBehavior, RichText};
use var_capture::{DeviceInfo, Mode, PixelFormat};

use crate::disk::{DiskMonitor, Level, format_bytes, format_remaining};
use crate::live::{CameraStatus, LiveCamera, Selection};
use crate::recording::{EncoderState, Recorder, format_elapsed};
use crate::remote::RemoteControl;

const LIVE_COLOR: Color32 = Color32::from_rgb(0x3c, 0xa0, 0x5a);
pub const ERROR_COLOR: Color32 = Color32::from_rgb(0xe0, 0x50, 0x50);
pub const RECORDING_COLOR: Color32 = Color32::from_rgb(0xe0, 0x30, 0x30);
pub const WARNING_COLOR: Color32 = Color32::from_rgb(0xf0, 0xb0, 0x30);
/// Background of the "LOCKED" mode indicator.
const LOCKED_COLOR: Color32 = Color32::from_rgb(0x2a, 0x5d, 0xb0);
/// Debug menu: simulated free space choices, GB.
const SIMULATED_FREE_GB: [u64; 4] = [50, 20, 5, 1];

/// Something the menus ask the application to do.
pub enum SetupAction {
    None,
    /// "Stop recording" chosen; needs confirmation (EXP-002).
    StopRequested,
    /// Switch to review of the running recording.
    Review,
    /// Return from review to the live view (REV-009).
    BackToLive,
    /// Set a marker (MRK-001).
    SetMarker,
    /// Show the recording at this time (marker list, MRK-004).
    JumpTo(i64),
    /// "Lock settings" (ROLE-003).
    Lock,
    /// "Unlock settings…": asks for the PIN (ROLE-006).
    UnlockRequested,
    /// "Set PIN…" / "Change PIN…" (ROLE-007).
    SetPinRequested,
    RemovePin,
    /// "Quit" from the menu; goes through the same checks as closing the window.
    Quit,
    /// Show the keyboard shortcuts (INP-002).
    Help,
    /// Toggle the full screen window.
    WindowFullScreen,
}

/// What the menu bar shows and changes.
pub struct MenuBar<'a> {
    pub cameras: &'a [LiveCamera],
    pub recorder: &'a mut Recorder,
    pub disk: &'a mut DiskMonitor,
    pub remote: &'a mut RemoteControl,
    /// Locked mode: setup, start/stop and quitting are unavailable (ROLE-004).
    pub locked: bool,
    pub has_pin: bool,
    pub debug_overlay: &'a mut bool,
    /// The window covers the whole screen.
    pub window_full_screen: bool,
    /// Laser pointer trails fade out after this many seconds.
    pub laser_seconds: &'a mut f32,
    pub camera_count: &'a mut usize,
    pub synthetic_cameras: u32,
    pub reviewing: bool,
}

/// The menu bar: mode indicator and menus on the left, recording status and
/// Start/Stop on the right.
pub fn menu_bar(ui: &mut egui::Ui, m: MenuBar<'_>) -> SetupAction {
    let MenuBar {
        cameras,
        recorder,
        disk,
        remote,
        locked,
        has_pin,
        debug_overlay,
        window_full_screen,
        laser_seconds,
        camera_count,
        synthetic_cameras,
        reviewing,
    } = m;
    let mut action = SetupAction::None;
    egui::MenuBar::new().ui(ui, |ui| {
        lock_menu(ui, locked, has_pin, &mut action);
        // Locked: all setup settings are unavailable (ROLE-004).
        ui.add_enabled_ui(!locked, |ui| {
            // Stays open while editing the folder; buttons close it explicitly.
            MenuButton::new("Recording")
                .config(MenuConfig::new().close_behavior(PopupCloseBehavior::CloseOnClickOutside))
                .ui(ui, |ui| {
                    recording_menu(ui, cameras, recorder, disk, remote, &mut action)
                })
                .0
                .on_disabled_hover_text("Settings are locked");
        });
        let recording = recorder.is_recording();
        ui.menu_button("View", |ui| {
            ui.label("Cameras");
            // The recorded cameras can't change while recording (CAP-010).
            ui.add_enabled_ui(!recording && !locked, |ui| {
                ui.horizontal(|ui| {
                    for count in 1..=crate::app::MAX_CAMERAS {
                        ui.radio_value(camera_count, count, count.to_string());
                    }
                });
            });
            ui.separator();
            let mut full_screen = window_full_screen;
            if ui
                .checkbox(&mut full_screen, "Full screen window  (F11)")
                .on_hover_text("Hides the window's title bar and the Windows taskbar")
                .changed()
            {
                action = SetupAction::WindowFullScreen;
            }
            ui.checkbox(debug_overlay, "Debug overlay");
            ui.separator();
            // DRW-002: how long the laser pointer trails stay in review.
            ui.horizontal(|ui| {
                ui.label("Laser pointer fades after");
                ui.add(
                    egui::DragValue::new(laser_seconds)
                        .range(crate::drawing::LIFETIME_RANGE_S)
                        .speed(0.1)
                        .fixed_decimals(1)
                        .suffix(" s"),
                )
                .on_hover_text(
                    "Trails drawn with the mouse in review stay this long; they fade out \
                     during the last second",
                );
            });
        });
        markers_menu(ui, recorder, &mut action);
        ui.menu_button("Help", |ui| {
            if ui.button("Keyboard shortcuts  (F1)").clicked() {
                action = SetupAction::Help;
                ui.close();
            }
        });
        if synthetic_cameras > 0 || disk.simulated_free.is_some() {
            ui.add_enabled_ui(!locked, |ui| {
                ui.menu_button("Debug", |ui| debug_menu(ui, synthetic_cameras, disk));
            });
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Live / review switch (REV-009: one action back to live).
            if reviewing {
                let text = RichText::new("Back to live").strong();
                if ui.button(text).clicked() {
                    action = SetupAction::BackToLive;
                }
            } else {
                let response = ui.add_enabled(
                    recorder.is_recording(),
                    egui::Button::new(RichText::new("Review").strong()),
                );
                if response
                    .on_disabled_hover_text("Review is available while recording")
                    .clicked()
                {
                    action = SetupAction::Review;
                }
            }
            ui.separator();
            // Locked: no start or stop (ROLE-004, EXP-002).
            if !locked {
                start_stop_button(ui, cameras, recorder, &mut action);
                ui.separator();
            }
            recording_status(ui, cameras, recorder, disk);
        });
    });
    action
}

/// The mode indicator, always visible (ROLE-008), and the menu to lock and unlock.
fn lock_menu(ui: &mut egui::Ui, locked: bool, has_pin: bool, action: &mut SetupAction) {
    let title = if locked {
        RichText::new(" LOCKED ")
            .strong()
            .color(Color32::WHITE)
            .background_color(LOCKED_COLOR)
    } else {
        RichText::new("Unlocked")
    };
    ui.menu_button(title, |ui| {
        ui.set_min_width(200.0);
        if locked {
            ui.weak("Settings, start/stop and quitting are locked.");
            if ui.button("Unlock settings…").clicked() {
                *action = SetupAction::UnlockRequested;
                ui.close();
            }
            return;
        }
        let response = ui.add_enabled(has_pin, egui::Button::new("Lock settings"));
        if response
            .on_disabled_hover_text("Set a PIN first: it is needed to unlock")
            .clicked()
        {
            *action = SetupAction::Lock;
            ui.close();
        }
        ui.separator();
        let set = if has_pin {
            "Change PIN…"
        } else {
            "Set PIN…"
        };
        if ui.button(set).clicked() {
            *action = SetupAction::SetPinRequested;
            ui.close();
        }
        if has_pin && ui.button("Remove PIN").clicked() {
            *action = SetupAction::RemovePin;
            ui.close();
        }
    })
    .response
    .on_hover_text(if locked {
        "Locked mode: review and markers only"
    } else {
        "Unlocked mode: all functions and settings"
    });
}

fn debug_menu(ui: &mut egui::Ui, synthetic_cameras: u32, disk: &mut DiskMonitor) {
    if synthetic_cameras > 0 {
        ui.label("Synthetic cameras connected:");
        for number in 1..=synthetic_cameras {
            let mut connected = var_capture::synthetic::is_connected(number);
            if ui
                .checkbox(&mut connected, format!("Synthetic camera {number}"))
                .changed()
            {
                var_capture::synthetic::set_connected(number, connected);
            }
        }
        ui.separator();
    }
    ui.label("Simulated free disk space:");
    ui.horizontal(|ui| {
        ui.radio_value(&mut disk.simulated_free, None, "Off");
        for gb in SIMULATED_FREE_GB {
            ui.radio_value(
                &mut disk.simulated_free,
                Some(gb * 1_000_000_000),
                format!("{gb} GB"),
            );
        }
    });
}

/// The markers of the running recording (MRK-004): set a marker, or jump to one.
fn markers_menu(ui: &mut egui::Ui, recorder: &Recorder, action: &mut SetupAction) {
    let markers = recorder.markers();
    ui.add_enabled_ui(recorder.is_recording(), |ui| {
        ui.menu_button(format!("Markers ({})", markers.len()), |ui| {
            ui.set_min_width(220.0);
            if ui.button("Set marker  (M)").clicked() {
                *action = SetupAction::SetMarker;
                ui.close();
            }
            ui.separator();
            if markers.is_empty() {
                ui.weak("No markers yet");
            }
            egui::ScrollArea::vertical()
                .max_height(420.0)
                .show(ui, |ui| {
                    for marker in markers {
                        let time = marker.time.get(11..23).unwrap_or(&marker.time);
                        let text =
                            RichText::new(format!("{:>3}   {time}", marker.number)).monospace();
                        if ui.button(text).clicked() {
                            *action = SetupAction::JumpTo(marker.wall_us);
                            ui.close();
                        }
                    }
                });
        })
        .response
        .on_disabled_hover_text("Markers can be set while recording");
    });
}

fn recording_menu(
    ui: &mut egui::Ui,
    cameras: &[LiveCamera],
    recorder: &mut Recorder,
    disk: &mut DiskMonitor,
    remote: &mut RemoteControl,
    action: &mut SetupAction,
) {
    let recording = recorder.is_recording();
    ui.set_min_width(380.0);

    if recording {
        if ui.button("Stop recording…").clicked() {
            *action = SetupAction::StopRequested;
            ui.close();
        }
    } else {
        let blocked = recorder.blocked_reason(cameras);
        let response = ui.add_enabled(blocked.is_none(), egui::Button::new("Start recording"));
        if let Some(reason) = blocked {
            response.on_disabled_hover_text(reason);
        } else if response.clicked() {
            recorder.start(cameras);
            ui.close();
        }
    }
    ui.separator();

    egui::Grid::new("recording settings")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Folder");
            ui.add_enabled_ui(!recording, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut recorder.folder).desired_width(240.0));
                    if ui.button("Browse…").clicked() {
                        if let Some(folder) = rfd::FileDialog::new()
                            .set_directory(&recorder.folder)
                            .pick_folder()
                        {
                            recorder.folder = folder.to_string_lossy().into_owned();
                        }
                        ui.close();
                    }
                });
            });
            ui.end_row();

            ui.label("Segment length");
            ui.add_enabled_ui(!recording, |ui| {
                let mut minutes = (recorder.segment_length.as_secs() / 60).max(1) as u32;
                let changed = ui
                    .add(
                        egui::DragValue::new(&mut minutes)
                            .range(1..=60)
                            .suffix(" min"),
                    )
                    .on_hover_text(
                        "Length of the recording files. Completed files can be moved \
                         to another drive during the recording.",
                    )
                    .changed();
                if changed {
                    recorder.segment_length =
                        std::time::Duration::from_secs(u64::from(minutes) * 60);
                }
            });
            ui.end_row();

            // REC-007b: the encoder in use is shown to the operator.
            ui.label("Encoder");
            match recorder.encoder() {
                EncoderState::Detecting => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Detecting…");
                    });
                }
                EncoderState::Found(encoder) => {
                    let kind = if encoder.hardware {
                        "hardware"
                    } else {
                        "software"
                    };
                    ui.label(format!("{} ({kind})", encoder.description))
                        .on_hover_text(&encoder.factory);
                }
                EncoderState::Missing => {
                    ui.colored_label(ERROR_COLOR, "No H.264 encoder available");
                }
            }
            ui.end_row();

            // DSK-001, DSK-002.
            let estimate = disk.estimate().clone();
            ui.label("Free space");
            match (&estimate.error, estimate.free) {
                (Some(error), _) => {
                    ui.colored_label(ERROR_COLOR, "Cannot read")
                        .on_hover_text(error);
                }
                (None, Some(free)) => {
                    let simulated = if disk.simulated_free.is_some() {
                        " (simulated)"
                    } else {
                        ""
                    };
                    ui.label(format!("{}{simulated}", format_bytes(free)));
                }
                (None, None) => {
                    ui.weak("…");
                }
            }
            ui.end_row();

            ui.label("Recording time");
            match estimate.remaining {
                Some(remaining) => {
                    let basis = if estimate.measured {
                        "measured"
                    } else {
                        "configured bitrates"
                    };
                    ui.label(
                        RichText::new(format!("about {}", format_remaining(remaining)))
                            .color(level_color(ui, estimate.level)),
                    );
                    ui.weak(format!(
                        "{basis}, {:.1} MB/s",
                        estimate.bytes_per_second / 1e6
                    ));
                }
                None => {
                    ui.weak("Select a camera");
                }
            }
            ui.end_row();

            // DSK-003: configurable thresholds.
            ui.label("Warn below");
            ui.horizontal(|ui| {
                let [first, second] = &mut disk.warning_minutes;
                ui.add(
                    egui::DragValue::new(first)
                        .range(2..=24 * 60)
                        .suffix(" min"),
                );
                ui.label("and");
                ui.add(
                    egui::DragValue::new(second)
                        .range(1..=24 * 60 - 1)
                        .suffix(" min"),
                );
                // The second threshold is the more urgent one.
                if *second >= *first {
                    *second = *first - 1;
                }
            });
            ui.end_row();

            // INP-003: remote control for Bitfocus Companion, off by default.
            ui.label("Remote control");
            ui.horizontal(|ui| {
                ui.checkbox(&mut remote.settings.enabled, "Companion, TCP port")
                    .on_hover_text(
                        "Lets Bitfocus Companion (Generic TCP module) control review and \
                         markers over the network. Recording, settings and quitting can't \
                         be controlled remotely.",
                    );
                // Typed, not dragged: every port on the way would be opened.
                ui.add(
                    egui::DragValue::new(&mut remote.settings.port)
                        .range(1024..=65535)
                        .speed(0.0),
                );
            });
            ui.end_row();
        });
    match remote.status() {
        Some(Ok(text)) => {
            ui.label(RichText::new(text).small());
        }
        Some(Err(e)) => {
            ui.colored_label(ERROR_COLOR, RichText::new(e).small());
        }
        None => {}
    }

    if let Some(status) = recorder.status(cameras) {
        ui.separator();
        ui.label(
            RichText::new(status.event_dir.display().to_string())
                .small()
                .weak(),
        );
    }
    ui.separator();
    if ui.button("Quit").clicked() {
        *action = SetupAction::Quit;
        ui.close();
    }
}

/// Color of the remaining recording time by warning level.
fn level_color(ui: &egui::Ui, level: Option<Level>) -> Color32 {
    match level {
        Some(Level::Critical) => ERROR_COLOR,
        Some(Level::Low) => WARNING_COLOR,
        _ => ui.visuals().text_color(),
    }
}

/// Remaining disk time in the status (VIEW-005, DSK-002).
fn disk_status(ui: &mut egui::Ui, disk: &DiskMonitor) {
    let estimate = disk.estimate();
    if let Some(error) = &estimate.error {
        ui.colored_label(ERROR_COLOR, "Disk ?")
            .on_hover_text(format!("Cannot read the free space: {error}"));
        return;
    }
    let Some(free) = estimate.free else { return };
    let hover = format!(
        "{} free{}\n{:.1} MB/s ({})",
        format_bytes(free),
        if disk.simulated_free.is_some() {
            " (simulated)"
        } else {
            ""
        },
        estimate.bytes_per_second / 1e6,
        if estimate.measured {
            "measured"
        } else {
            "configured bitrates"
        }
    );
    match estimate.remaining {
        Some(remaining) => {
            let mut text = RichText::new(format!("Disk {}", format_remaining(remaining)))
                .color(level_color(ui, estimate.level));
            if estimate.level == Some(Level::Critical) {
                text = text.strong();
            }
            ui.label(text).on_hover_text(hover);
        }
        None => {
            ui.weak(format!("Disk {} free", format_bytes(free)))
                .on_hover_text(hover);
        }
    }
}

fn start_stop_button(
    ui: &mut egui::Ui,
    cameras: &[LiveCamera],
    recorder: &mut Recorder,
    action: &mut SetupAction,
) {
    if recorder.is_recording() {
        if ui.button("Stop recording").clicked() {
            *action = SetupAction::StopRequested;
        }
        return;
    }
    let blocked = recorder.blocked_reason(cameras);
    let text = RichText::new("Start recording").color(if blocked.is_none() {
        RECORDING_COLOR
    } else {
        Color32::GRAY
    });
    let response = ui.add_enabled(blocked.is_none(), egui::Button::new(text));
    if let Some(reason) = blocked {
        response.on_disabled_hover_text(reason);
    } else if response.clicked() {
        recorder.start(cameras);
    }
}

/// Status in the menu bar (VIEW-005): the health of each camera, the remaining disk
/// time, and while recording REC, elapsed time and the recording state of each camera.
/// A failed camera is shown in red (REC-006). Laid out right to left.
fn recording_status(
    ui: &mut egui::Ui,
    cameras: &[LiveCamera],
    recorder: &Recorder,
    disk: &DiskMonitor,
) {
    let status = recorder.status(cameras);
    // With more than two cameras, shorter texts; details on hover.
    let compact = cameras.len() > 2;

    for (slot, camera) in cameras.iter().enumerate().rev() {
        let recording = status
            .as_ref()
            .and_then(|s| s.cameras.iter().find(|c| c.slot == slot));
        let label = format!("Cam {}", slot + 1);
        match (camera.status(), recording) {
            (CameraStatus::NoSelection, None) => {
                ui.weak(format!("{label} –"));
            }
            (live, Some(recording)) if recording.interrupted.is_some() || live.is_alarm() => {
                let since = recording
                    .interrupted
                    .map_or_else(String::new, |d| format!(" {}", format_elapsed(d)));
                let not_recording = if compact { "NOT REC" } else { "NOT RECORDING" };
                ui.label(
                    RichText::new(format!("{label} {not_recording}{since}"))
                        .strong()
                        .color(ERROR_COLOR),
                )
                .on_hover_text(alarm_text(&live, recording.interruptions));
            }
            (_, Some(recording)) => {
                let text = if compact {
                    format!("{label} seg {}", recording.stats.segments)
                } else {
                    format!(
                        "{label}  {:.1} Mbit/s  seg {}",
                        recording.stats.bitrate / 1e6,
                        recording.stats.segments
                    )
                };
                match &recording.error {
                    Some(error) => {
                        ui.colored_label(ERROR_COLOR, format!("{label} ERROR"))
                            .on_hover_text(error);
                    }
                    None => {
                        ui.monospace(text).on_hover_text(format!(
                            "{:.1} Mbit/s, {:.0} MB recorded, {} interruption(s)",
                            recording.stats.bitrate / 1e6,
                            recording.total_bytes as f64 / 1e6,
                            recording.interruptions
                        ));
                    }
                }
            }
            (live, None) if live.is_alarm() => {
                ui.label(
                    RichText::new(format!("{label} FAILED"))
                        .strong()
                        .color(ERROR_COLOR),
                )
                .on_hover_text(alarm_text(&live, 0));
            }
            (CameraStatus::Running, None) => {
                ui.colored_label(LIVE_COLOR, format!("{label} live"));
            }
            (_, None) => {
                ui.weak(format!("{label} starting"));
            }
        }
        ui.separator();
    }
    disk_status(ui, disk);
    ui.separator();

    match status {
        Some(status) => {
            ui.label(
                RichText::new(format!("REC  {}", format_elapsed(status.elapsed)))
                    .strong()
                    .color(RECORDING_COLOR),
            );
            recording_dot(ui, 6.0);
        }
        None => {
            if let Some(error) = recorder.start_error() {
                ui.colored_label(ERROR_COLOR, "Recording could not start")
                    .on_hover_text(error);
            } else {
                ui.weak("Not recording");
            }
        }
    }
}

fn alarm_text(status: &CameraStatus<'_>, interruptions: u32) -> String {
    let reason = match status {
        CameraStatus::Failed(message) => format!("Camera failed: {message}. Restarting."),
        CameraStatus::Disconnected => "Camera disconnected. Waiting for it.".to_owned(),
        _ => "Camera restarting.".to_owned(),
    };
    if interruptions > 0 {
        format!("{reason}\nRecording interruptions: {interruptions}")
    } else {
        reason
    }
}

/// A filled red circle, drawn as a shape: the default font has no "●".
pub fn recording_dot(ui: &mut egui::Ui, radius: f32) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(radius * 2.0, radius * 2.0), egui::Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), radius, RECORDING_COLOR);
}

/// Context menu of a camera tile: device and mode of the camera slot (CAP-005,
/// CAP-006). Locked while recording (CAP-010).
pub fn camera_menu(
    ui: &mut egui::Ui,
    slot: usize,
    devices: &[DeviceInfo],
    cameras: &mut [LiveCamera],
    recording: bool,
    locked: bool,
) {
    ui.set_min_width(220.0);
    ui.label(RichText::new(format!("Camera {}", slot + 1)).strong());
    status_label(ui, cameras[slot].status());
    ui.separator();

    if locked {
        ui.weak("Settings are locked.");
        return;
    }
    if recording {
        ui.weak("Camera and mode can't be changed while recording.");
        return;
    }

    let current = cameras[slot].selection().cloned();
    let current_device = current
        .as_ref()
        .and_then(|s| devices.iter().find(|d| d.id == s.device_id));
    // A device can only be used by one camera slot.
    let used_by_other = |device: &DeviceInfo| {
        cameras.iter().enumerate().find_map(|(other, camera)| {
            (other != slot && camera.selection().is_some_and(|s| s.device_id == device.id))
                .then_some(other)
        })
    };
    let mut new_selection = None;

    ui.menu_button("Device", |ui| {
        if ui
            .add(egui::Button::selectable(current.is_none(), "None"))
            .clicked()
        {
            new_selection = Some(None);
        }
        if devices.is_empty() {
            ui.weak("No capture devices found");
        }
        for device in devices {
            let selected = current.as_ref().is_some_and(|s| s.device_id == device.id);
            let mut label = device_label(devices, device);
            let in_use = used_by_other(device);
            if let Some(other) = in_use {
                label = format!("{label} (camera {})", other + 1);
            }
            let supported = device.modes.iter().any(|m| m.format.is_supported());
            let response = ui.add_enabled(
                in_use.is_none() && supported,
                egui::Button::selectable(selected, label),
            );
            if !supported {
                response.on_disabled_hover_text("No supported format (MJPEG, YUY2, NV12)");
            } else if response
                .on_hover_text(format!("{} – ID: {}", device.api, device.id))
                .clicked()
                && !selected
            {
                new_selection = Some(default_selection(device));
            }
        }
    });

    ui.add_enabled_ui(current_device.is_some(), |ui| {
        ui.menu_button("Mode", |ui| {
            let (Some(selection), Some(device)) = (&current, current_device) else {
                return;
            };
            // Grouped by format: some devices offer hundreds of modes.
            for format in supported_formats(device) {
                ui.menu_button(format.to_string(), |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(420.0)
                        .show(ui, |ui| {
                            for mode in device.modes.iter().filter(|m| m.format == format) {
                                let selected = selection.mode == *mode;
                                let text = format!(
                                    "{} × {} @ {} fps",
                                    mode.width, mode.height, mode.framerate
                                );
                                if ui.add(egui::Button::selectable(selected, text)).clicked()
                                    && !selected
                                {
                                    new_selection = Some(Some(Selection {
                                        mode: mode.clone(),
                                        ..selection.clone()
                                    }));
                                }
                            }
                        });
                });
            }
        });
    });
    if let Some(selection) = &current {
        ui.weak(selection.mode.to_string());
    }

    if let Some(selection) = new_selection {
        cameras[slot].select(selection);
        ui.close();
    }
}

fn status_label(ui: &mut egui::Ui, status: CameraStatus<'_>) {
    match status {
        CameraStatus::NoSelection => ui.weak("No camera selected"),
        CameraStatus::Disconnected => ui.colored_label(ERROR_COLOR, "Disconnected"),
        CameraStatus::Starting => ui.label("Starting…"),
        CameraStatus::Running => ui.colored_label(LIVE_COLOR, "Live"),
        CameraStatus::Failed(message) => ui.colored_label(ERROR_COLOR, message),
    };
}

/// The supported pixel formats of a device, in the order of its mode list.
fn supported_formats(device: &DeviceInfo) -> Vec<PixelFormat> {
    let mut formats: Vec<PixelFormat> = Vec::new();
    for mode in device.modes.iter().filter(|m| m.format.is_supported()) {
        if !formats.contains(&mode.format) {
            formats.push(mode.format.clone());
        }
    }
    formats
}

/// Device name, numbered if several devices have the same name (e.g. two identical
/// capture cards).
fn device_label(devices: &[DeviceInfo], device: &DeviceInfo) -> String {
    let same_name: Vec<_> = devices.iter().filter(|d| d.name == device.name).collect();
    if same_name.len() > 1 {
        let index = same_name
            .iter()
            .position(|d| d.id == device.id)
            .unwrap_or(0)
            + 1;
        format!("{} ({index})", device.name)
    } else {
        device.name.clone()
    }
}

/// The device with the mode given as `FORMAT:WIDTHxHEIGHT@FPS`, e.g. `YUY2:1920x1080@50`.
pub fn selection_with_mode(device: &DeviceInfo, spec: &str) -> Option<Selection> {
    let (format, rest) = spec.split_once(':')?;
    let (size, fps) = rest.split_once('@')?;
    let (width, height) = size.split_once('x')?;
    let (width, height, fps): (u32, u32, f64) =
        (width.parse().ok()?, height.parse().ok()?, fps.parse().ok()?);
    let mode = device.modes.iter().find(|m| {
        m.format.is_supported()
            && m.format.to_string().eq_ignore_ascii_case(format)
            && m.width == width
            && m.height == height
            && (m.framerate.max_fps() - fps).abs() < 0.01
    })?;
    Some(Selection {
        device_id: device.id.clone(),
        device_name: device.name.clone(),
        mode: mode.clone(),
    })
}

/// The device with its default mode, or `None` if it has no supported mode.
pub fn default_selection(device: &DeviceInfo) -> Option<Selection> {
    default_mode(device).map(|mode| Selection {
        device_id: device.id.clone(),
        device_name: device.name.clone(),
        mode,
    })
}

/// The mode preselected for a device: 1080p60, then 1080p50, then the first
/// supported mode.
fn default_mode(device: &DeviceInfo) -> Option<Mode> {
    let supported = || device.modes.iter().filter(|m| m.format.is_supported());
    let full_hd_at = |fps: f64| {
        supported().find(|m| {
            m.width == 1920 && m.height == 1080 && (m.framerate.max_fps() - fps).abs() < 0.01
        })
    };
    full_hd_at(60.0)
        .or_else(|| full_hd_at(50.0))
        .or_else(|| supported().next())
        .cloned()
}
