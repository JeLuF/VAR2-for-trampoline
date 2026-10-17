//! Laser pointer drawing in review mode (DRW-001 – DRW-003): dragging with the mouse
//! over a camera draws a glowing trail that fades out after about 3 seconds.
//!
//! The trails are a display overlay only: they live in memory, are painted on top of
//! the video and are never written anywhere (DRW-003).

use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Pos2, Rect, Shape, Stroke};

/// A point of a trail disappears this long after it was drawn (DRW-002)...
const LIFETIME: Duration = Duration::from_secs(3);
/// ...fading out over this last part of its lifetime.
const FADE: Duration = Duration::from_millis(1000);
/// Points closer than this to the previous one (screen points) are skipped.
const MIN_DISTANCE: f32 = 1.5;
/// Fading is drawn in this many opacity steps, each as one connected line.
const ALPHA_STEPS: f32 = 16.0;
const CORE_COLOR: Color32 = Color32::from_rgb(0xff, 0x3a, 0x2a);
const CORE_WIDTH: f32 = 3.0;
const GLOW_WIDTH: f32 = 9.0;
const GLOW_ALPHA: f32 = 0.3;

struct Trail {
    slot: usize,
    /// Positions relative to the video picture (0–1), so a trail stays on the same
    /// spot of the picture when the tile changes size (full screen, window size).
    points: Vec<(Pos2, Instant)>,
}

#[derive(Default)]
pub struct Drawing {
    trails: Vec<Trail>,
    /// The last trail is still being drawn.
    drawing: bool,
}

impl Drawing {
    /// Follows a drag with the primary mouse button on the tile of `slot`, whose video
    /// picture is at `video`.
    pub fn input(&mut self, slot: usize, response: &egui::Response, video: Rect) {
        let now = Instant::now();
        let to_video = |pos: Pos2| {
            Pos2::new(
                (pos.x - video.left()) / video.width().max(1.0),
                (pos.y - video.top()) / video.height().max(1.0),
            )
        };
        if response.drag_started_by(egui::PointerButton::Primary) {
            // Start where the button was pressed, not where the drag was recognised.
            let origin = response
                .ctx
                .input(|i| i.pointer.press_origin())
                .or_else(|| response.interact_pointer_pos());
            self.trails.push(Trail {
                slot,
                points: origin.map(|p| (to_video(p), now)).into_iter().collect(),
            });
            self.drawing = true;
        }
        if self.drawing
            && response.dragged_by(egui::PointerButton::Primary)
            && let Some(pos) = response.interact_pointer_pos()
            && let Some(trail) = self.trails.last_mut()
            && trail.slot == slot
        {
            let point = to_video(pos);
            let far_enough = trail.points.last().is_none_or(|(last, _)| {
                let dx = (point.x - last.x) * video.width();
                let dy = (point.y - last.y) * video.height();
                dx.hypot(dy) >= MIN_DISTANCE
            });
            if far_enough {
                trail.points.push((point, now));
            }
        }
        if response.drag_stopped() {
            self.drawing = false;
        }
    }

    /// Paints the trails of `slot` over its video picture at `video`.
    pub fn paint(&self, painter: &egui::Painter, slot: usize, video: Rect) {
        let now = Instant::now();
        let to_screen = |p: Pos2| {
            Pos2::new(
                video.left() + p.x * video.width(),
                video.top() + p.y * video.height(),
            )
        };
        let mut any = false;
        for trail in self.trails.iter().filter(|t| t.slot == slot) {
            any = true;
            if let [(point, time)] = trail.points.as_slice() {
                // A single point: a dot.
                let alpha = opacity(now.duration_since(*time));
                painter.circle_filled(to_screen(*point), GLOW_WIDTH / 2.0, glow(alpha));
                painter.circle_filled(to_screen(*point), CORE_WIDTH / 2.0, core(alpha));
                continue;
            }
            // Runs of equal opacity as connected lines, so the joins are smooth. The
            // points get older towards the start of the trail.
            let mut run: Vec<Pos2> = Vec::new();
            let mut run_alpha = None;
            for (point, time) in &trail.points {
                let alpha = (opacity(now.duration_since(*time)) * ALPHA_STEPS).ceil() / ALPHA_STEPS;
                let pos = to_screen(*point);
                if run_alpha.is_some_and(|a| a != alpha) {
                    draw_run(painter, &run, run_alpha.unwrap());
                    // The runs share the boundary point, so there is no gap.
                    run = vec![*run.last().unwrap()];
                }
                run_alpha = Some(alpha);
                run.push(pos);
            }
            if let Some(alpha) = run_alpha {
                draw_run(painter, &run, alpha);
            }
        }
        if any {
            // Keep fading while nothing else repaints (paused review).
            painter.ctx().request_repaint();
        }
    }

    /// Removes faded points and trails. Call once per frame.
    pub fn prune(&mut self) {
        let now = Instant::now();
        for trail in &mut self.trails {
            trail
                .points
                .retain(|(_, time)| now.duration_since(*time) < LIFETIME);
        }
        let count = self.trails.len();
        let drawing = self.drawing;
        let mut index = 0;
        self.trails.retain(|trail| {
            let keep = !trail.points.is_empty() || (drawing && index + 1 == count);
            index += 1;
            keep
        });
    }

    #[cfg(test)]
    fn point_count(&self) -> usize {
        self.trails.iter().map(|t| t.points.len()).sum()
    }
}

fn draw_run(painter: &egui::Painter, points: &[Pos2], alpha: f32) {
    if alpha <= 0.0 || points.len() < 2 {
        return;
    }
    painter.add(Shape::line(
        points.to_vec(),
        Stroke::new(GLOW_WIDTH, glow(alpha)),
    ));
    painter.add(Shape::line(
        points.to_vec(),
        Stroke::new(CORE_WIDTH, core(alpha)),
    ));
}

/// Opacity of a point of this age: full, then fading out over [`FADE`].
fn opacity(age: Duration) -> f32 {
    let left = LIFETIME.saturating_sub(age);
    (left.as_secs_f32() / FADE.as_secs_f32()).clamp(0.0, 1.0)
}

fn core(alpha: f32) -> Color32 {
    CORE_COLOR.gamma_multiply(alpha)
}

fn glow(alpha: f32) -> Color32 {
    CORE_COLOR.gamma_multiply(alpha * GLOW_ALPHA)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opacity_fades_in_the_last_second() {
        assert_eq!(opacity(Duration::ZERO), 1.0);
        assert_eq!(opacity(Duration::from_millis(2000)), 1.0);
        assert!((opacity(Duration::from_millis(2500)) - 0.5).abs() < 1e-3);
        assert_eq!(opacity(Duration::from_millis(3000)), 0.0);
        assert_eq!(opacity(Duration::from_secs(10)), 0.0);
    }

    #[test]
    fn trails_disappear_by_themselves() {
        let mut drawing = Drawing::default();
        let old = Instant::now() - Duration::from_millis(3100);
        let recent = Instant::now();
        drawing.trails.push(Trail {
            slot: 0,
            points: vec![(Pos2::new(0.1, 0.1), old), (Pos2::new(0.2, 0.2), old)],
        });
        drawing.trails.push(Trail {
            slot: 1,
            points: vec![(Pos2::new(0.1, 0.1), old), (Pos2::new(0.5, 0.5), recent)],
        });
        drawing.prune();
        assert_eq!(drawing.trails.len(), 1, "the faded trail is gone");
        assert_eq!(drawing.point_count(), 1, "faded points of a trail are gone");
    }

    #[test]
    fn trail_being_drawn_is_kept_while_empty() {
        let mut drawing = Drawing {
            trails: vec![Trail {
                slot: 0,
                points: Vec::new(),
            }],
            drawing: true,
        };
        drawing.prune();
        assert_eq!(drawing.trails.len(), 1);
        drawing.drawing = false;
        drawing.prune();
        assert!(drawing.trails.is_empty());
    }
}
