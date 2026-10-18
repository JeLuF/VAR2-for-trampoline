//! Review mode: synchronised review of the running recording (REV-001), with a scrub
//! bar labelled in wall-clock time (REV-007, REV-007a) and the time of the shown frame
//! (VIEW-007). In review mode the grid shows only recorded video (VIEW-004).

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{Local, TimeZone, Timelike};
use eframe::egui::{self, Align2, Color32, FontId, Rect, Sense, Stroke};
use var_core::index::LiveIndex;
use var_playback::{CameraState, MAX_REVERSE_SPEED, ReviewPlayer, SPEEDS};

use crate::drawing::Drawing;
use crate::markers::Marker;
use crate::video::{Nv12Frame, VideoPaintCallback, ViewId};

/// Review tiles use their own textures, separate from the live view.
const REVIEW_VIEW_BASE: ViewId = 100;
pub const MARKER_COLOR: Color32 = Color32::from_rgb(0xf0, 0xc0, 0x20);

pub struct ReviewMode {
    player: ReviewPlayer,
    /// Capture time of the frame last uploaded per slot.
    uploaded: HashMap<usize, i64>,
    /// Laser pointer trails (DRW-001); gone when review ends.
    drawing: Drawing,
}

/// What the review bar asks for.
pub enum ReviewAction {
    None,
    BackToLive,
}

impl ReviewMode {
    /// Starts review of the running recording, paused `before_live_us` before the
    /// newest frame.
    pub fn new(
        sources: Vec<(usize, Arc<LiveIndex>)>,
        before_live_us: i64,
        ctx: &egui::Context,
    ) -> Self {
        let repaint = ctx.clone();
        let mut player = ReviewPlayer::new(sources, i64::MAX, move || repaint.request_repaint());
        if let Some((start, end)) = player.range() {
            player.seek((end - before_live_us).max(start));
        }
        ReviewMode {
            player,
            uploaded: HashMap::new(),
            drawing: Drawing::default(),
        }
    }

    pub fn player(&mut self) -> &mut ReviewPlayer {
        &mut self.player
    }

    /// How many seconds laser pointer trails stay (DRW-002).
    pub fn set_drawing_lifetime(&mut self, seconds: f32) {
        self.drawing.set_lifetime(seconds);
    }

    /// Advances playback and lets the drawing fade; call once per UI frame.
    pub fn tick(&mut self) {
        self.player.tick();
        self.drawing.prune();
    }

    /// Draws the recorded video of camera `slot` into `rect`, with the laser pointer
    /// trails drawn with the mouse (`response`: the tile's click and drag response).
    pub fn tile(
        &mut self,
        painter: &egui::Painter,
        rect: Rect,
        slot: usize,
        debug_overlay: bool,
        response: &egui::Response,
    ) {
        let video_rect = self.video(painter, rect, slot, debug_overlay);
        // DRW-001: drawing over the picture; a crosshair shows that it's possible.
        if response.hovered() {
            painter.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        self.drawing.input(slot, response, video_rect);
        self.drawing.paint(painter, slot, video_rect);
    }

    /// Draws the recorded video; returns where the picture is (the whole tile when
    /// there is none).
    fn video(
        &mut self,
        painter: &egui::Painter,
        rect: Rect,
        slot: usize,
        debug_overlay: bool,
    ) -> Rect {
        painter.rect_filled(rect, 0.0, Color32::BLACK);
        let Some(camera) = self.player.camera(slot) else {
            message(painter, rect, "Not recorded");
            return rect;
        };
        let state = camera.state();
        let frame = camera.frame();
        match (&state, &frame) {
            (CameraState::Failed(e), _) => {
                message(painter, rect, &format!("Review not possible: {e}"));
                return rect;
            }
            (CameraState::NoVideo, _) | (_, None) => {
                let text = if state == CameraState::NoVideo {
                    "No video at this time"
                } else {
                    "Loading…"
                };
                message(painter, rect, text);
                self.uploaded.remove(&slot);
                return rect;
            }
            _ => {}
        }
        let frame = frame.unwrap();
        let video_rect = fit(rect, frame.width(), frame.height());
        let upload = (self.uploaded.get(&slot) != Some(&frame.wall_us)).then(|| {
            self.uploaded.insert(slot, frame.wall_us);
            Arc::clone(&frame) as Arc<dyn Nv12Frame>
        });
        painter.add(VideoPaintCallback::shape(
            video_rect,
            REVIEW_VIEW_BASE + slot as ViewId,
            upload,
        ));
        if debug_overlay {
            let decoder = camera.decoder_name().unwrap_or_default();
            let lag_ms = (self.player.position() - frame.wall_us) / 1000;
            let text = format!("Review  {decoder}  frame −{lag_ms} ms");
            let galley = painter.layout_no_wrap(text, FontId::monospace(13.0), Color32::WHITE);
            let pos = video_rect.right_bottom() - galley.size() - egui::vec2(10.0, 10.0);
            painter.rect_filled(
                Rect::from_min_size(
                    pos - egui::vec2(6.0, 4.0),
                    galley.size() + egui::vec2(12.0, 8.0),
                ),
                4.0,
                Color32::from_black_alpha(170),
            );
            painter.galley(pos, galley, Color32::WHITE);
        }
        video_rect
    }

    /// The review controls: scrub bar with markers; frame steps, play/pause, speeds,
    /// time of the shown frame, back to live (REV-009). `reference`: camera whose
    /// frames the steps follow.
    pub fn bar(
        &mut self,
        ui: &mut egui::Ui,
        markers: &[Marker],
        reference: Option<usize>,
    ) -> ReviewAction {
        let mut action = ReviewAction::None;
        ui.add_space(4.0);
        if let Some((start, end)) = self.player.range() {
            let width = ui.available_width();
            if let Some(seek) = scrub_bar(ui, width, (start, end), self.player.position(), markers)
            {
                self.player.seek(seek);
            }
        }
        ui.horizontal(|ui| {
            let button = |ui: &mut egui::Ui, text: &str, width: f32, hint: &str| {
                ui.add_sized([width, 28.0], egui::Button::new(text))
                    .on_hover_text(hint)
                    .clicked()
            };
            // REV-006: jumps, left and right of the frame steps.
            let command = if cfg!(target_os = "macos") {
                "Cmd"
            } else {
                "Ctrl"
            };
            let jumps = [
                (60, "Alt".to_owned()),
                (10, command.to_owned()),
                (1, "Shift".to_owned()),
            ];
            for (seconds, modifier) in &jumps {
                if button(
                    ui,
                    &format!("−{seconds} s"),
                    42.0,
                    &format!("{modifier} + ←"),
                ) {
                    self.player.jump(-seconds);
                }
            }
            if button(ui, "−1 frame", 72.0, "←") {
                self.player.step(-1, reference);
            }
            // REV-005: reverse slow motion.
            let reverse = if self.player.is_reverse() {
                "Pause"
            } else {
                "Reverse"
            };
            if button(
                ui,
                reverse,
                64.0,
                "Shift + Space: play backwards (0.5× and slower)",
            ) {
                self.player.toggle_reverse();
            }
            let play = if self.player.is_playing() && !self.player.is_reverse() {
                "Pause"
            } else {
                "Play"
            };
            if button(ui, play, 64.0, "Space") {
                self.player.toggle();
            }
            if button(ui, "+1 frame", 72.0, "→") {
                self.player.step(1, reference);
            }
            for (seconds, modifier) in jumps.iter().rev() {
                if button(
                    ui,
                    &format!("+{seconds} s"),
                    42.0,
                    &format!("{modifier} + →"),
                ) {
                    self.player.jump(*seconds);
                }
            }
            ui.separator();

            // REV-003: speeds.
            for speed in SPEEDS.iter().rev() {
                let selected = self.player.speed() == *speed;
                let text = format!("{speed}×");
                let hint = if *speed > MAX_REVERSE_SPEED {
                    "+ / −  (forwards only)"
                } else {
                    "+ / −"
                };
                if ui
                    .add(egui::Button::selectable(selected, text))
                    .on_hover_text(hint)
                    .clicked()
                {
                    self.player.set_speed(*speed);
                }
            }
            ui.separator();

            // VIEW-007: wall-clock time of the shown frame.
            ui.label(
                egui::RichText::new(format_time(self.player.position(), true))
                    .monospace()
                    .size(18.0),
            );
            // In a narrow window, the distance to live gives way to "Back to live".
            if let Some((_, end)) = self.player.range()
                && ui.available_width() > 270.0
            {
                let behind = (end - self.player.position()) as f64 / 1e6;
                ui.label(
                    egui::RichText::new(format!("{behind:5.1} s behind live"))
                        .monospace()
                        .weak(),
                );
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if button(ui, "Back to live", 110.0, "L") {
                    action = ReviewAction::BackToLive;
                }
            });
        });
        ui.add_space(4.0);
        action
    }
}

fn message(painter: &egui::Painter, rect: Rect, text: &str) {
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(18.0),
        Color32::from_gray(200),
    );
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

/// Local time of day of a wall-clock time: `HH:MM:SS.mmm` or `HH:MM:SS`. As time of
/// day only, so labels continue 23:59 → 00:00 after midnight (REV-007a).
pub fn format_time(wall_us: i64, millis: bool) -> String {
    let Some(time) = Local.timestamp_micros(wall_us).single() else {
        return "--:--:--".to_owned();
    };
    if millis {
        time.format("%H:%M:%S%.3f").to_string()
    } else {
        time.format("%H:%M:%S").to_string()
    }
}

/// Seconds since local midnight.
fn local_seconds_of_day(wall_us: i64) -> i64 {
    Local
        .timestamp_micros(wall_us)
        .single()
        .map_or(0, |t| i64::from(t.num_seconds_from_midnight()))
}

/// Tick interval for a scrub bar showing `span_us` on `width` pixels: at least about
/// 90 pixels per label.
fn tick_interval_s(span_us: i64, width: f32) -> i64 {
    const STEPS: [i64; 15] = [
        1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 10800,
    ];
    let max_labels = (width / 90.0).max(1.0) as i64;
    let span_s = span_us / 1_000_000;
    STEPS
        .into_iter()
        .find(|step| span_s / step <= max_labels)
        .unwrap_or(21600)
}

/// The scrub bar over the whole recording (REV-007), with the markers (REV-008).
/// Returns a position to seek to when clicked or dragged.
fn scrub_bar(
    ui: &mut egui::Ui,
    width: f32,
    (start, end): (i64, i64),
    position: i64,
    markers: &[Marker],
) -> Option<i64> {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 44.0), Sense::click_and_drag());
    let painter = ui.painter_at(rect.expand(2.0));
    let span = (end - start).max(1);
    let x_of = |t: i64| rect.left() + (t - start) as f32 / span as f32 * rect.width();

    // Track and the part up to the position; markers above it.
    let track = Rect::from_min_max(
        egui::pos2(rect.left(), rect.top() + 14.0),
        egui::pos2(rect.right(), rect.top() + 22.0),
    );
    painter.rect_filled(track, 3.0, Color32::from_gray(60));
    let played = Rect::from_min_max(track.min, egui::pos2(x_of(position), track.max.y));
    painter.rect_filled(played, 3.0, Color32::from_rgb(0x3c, 0x78, 0xd8));

    // Labels at round local times.
    let step_s = tick_interval_s(span, rect.width());
    let step_us = step_s * 1_000_000;
    let offset_us =
        (local_seconds_of_day(start) % step_s) * 1_000_000 + start.rem_euclid(1_000_000);
    let mut tick = start - offset_us + step_us;
    let show_seconds = step_s < 60;
    while tick < end {
        let x = x_of(tick);
        painter.line_segment(
            [egui::pos2(x, track.max.y), egui::pos2(x, track.max.y + 4.0)],
            Stroke::new(1.0_f32, Color32::from_gray(140)),
        );
        let label = format_time(tick, false);
        let label = if show_seconds {
            label
        } else {
            label[..5].to_owned()
        };
        painter.text(
            egui::pos2(x, rect.bottom()),
            Align2::CENTER_BOTTOM,
            label,
            FontId::proportional(11.0),
            Color32::from_gray(170),
        );
        tick += step_us;
    }

    // Markers: a yellow line through the track and the number above it, if there is
    // room next to the previous number.
    let mut free_from = f32::MIN;
    for marker in markers
        .iter()
        .filter(|m| (start..=end).contains(&m.wall_us))
    {
        let x = x_of(marker.wall_us);
        painter.line_segment(
            [
                egui::pos2(x, rect.top() + 11.0),
                egui::pos2(x, track.max.y + 2.0),
            ],
            Stroke::new(2.0_f32, MARKER_COLOR),
        );
        let label = painter.layout_no_wrap(
            marker.number.to_string(),
            FontId::proportional(10.0),
            MARKER_COLOR,
        );
        let left = x - label.size().x / 2.0;
        if left >= free_from {
            free_from = left + label.size().x + 4.0;
            let top = rect.top() + 11.0 - label.size().y;
            painter.galley(egui::pos2(left, top), label, MARKER_COLOR);
        }
    }

    // Position.
    let x = x_of(position);
    painter.line_segment(
        [
            egui::pos2(x, track.min.y - 4.0),
            egui::pos2(x, track.max.y + 2.0),
        ],
        Stroke::new(3.0_f32, Color32::WHITE),
    );

    if (response.clicked() || response.dragged())
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let fraction = ((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        return Some(start + (fraction as f64 * span as f64) as i64);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_intervals() {
        // 30 s on 900 px: 10 labels → 5 s steps.
        assert_eq!(tick_interval_s(30_000_000, 900.0), 5);
        // 1 hour on 900 px → 10 min.
        assert_eq!(tick_interval_s(3_600_000_000, 900.0), 600);
        // 12 hours on 900 px → 2 h.
        assert_eq!(tick_interval_s(43_200_000_000, 900.0), 7200);
    }

    #[test]
    fn labels_continue_after_midnight() {
        // REV-007a: the time of day wraps from 23:59 to 00:00.
        let before = Local.with_ymd_and_hms(2026, 10, 17, 23, 59, 30).unwrap();
        let after = Local.with_ymd_and_hms(2026, 10, 18, 0, 0, 30).unwrap();
        assert_eq!(format_time(before.timestamp_micros(), false), "23:59:30");
        assert_eq!(format_time(after.timestamp_micros(), false), "00:00:30");
        assert_eq!(local_seconds_of_day(after.timestamp_micros()), 30);
    }
}
