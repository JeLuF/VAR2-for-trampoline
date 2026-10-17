//! Detection of dropped frames from frame timestamps.

use std::collections::VecDeque;

/// Frames after a long interval that are examined to tell a dropped frame from a late
/// one.
const LOOKAHEAD: usize = 3;

/// An interval longer than this many frame durations is a gap.
const GAP_THRESHOLD: f64 = 1.5;

/// A gap in the frame timestamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    /// Timestamp of the last frame before the gap, in nanoseconds.
    pub after_ns: u64,
    /// Frames missing: 0 for jitter, i.e. a late frame after which the following frames
    /// catch up with the timeline.
    pub missing: u64,
}

/// Finds dropped frames in a stream of frame timestamps (in nanoseconds, increasing).
///
/// Cameras timestamp frames on arrival, so a frame that is delivered late produces a
/// long interval followed by a short one, without any frame being lost. A long
/// interval only counts as dropped frames if the following frames don't catch up:
/// the frames lost are the frame slots that stay missing over the next few intervals.
/// This is local, so a camera clock that is slightly faster or slower than nominal
/// doesn't accumulate false drops.
#[derive(Debug, Clone)]
pub struct DropDetector {
    frame_ns: f64,
    window: VecDeque<u64>,
}

impl DropDetector {
    /// `frame_ns`: nominal frame duration in nanoseconds.
    pub fn new(frame_ns: f64) -> Self {
        DropDetector {
            frame_ns,
            window: VecDeque::with_capacity(LOOKAHEAD + 2),
        }
    }

    /// Adds a frame timestamp. Returns the gap after the frame `LOOKAHEAD + 1` frames
    /// back, once it can be judged.
    pub fn push(&mut self, pts_ns: u64) -> Option<Gap> {
        if self.frame_ns <= 0.0 {
            return None;
        }
        if let Some(&last) = self.window.back()
            && pts_ns <= last
        {
            return None; // duplicate or out of order: ignore
        }
        self.window.push_back(pts_ns);
        if self.window.len() < LOOKAHEAD + 2 {
            return None;
        }
        let gap = self.judge_first();
        self.window.pop_front();
        gap
    }

    /// Judges the remaining intervals at the end of the stream, with whatever lookahead
    /// is left.
    pub fn finish(&mut self) -> Vec<Gap> {
        let mut gaps = Vec::new();
        while self.window.len() >= 2 {
            gaps.extend(self.judge_first());
            self.window.pop_front();
        }
        self.window.clear();
        gaps
    }

    /// Judges the interval between the first two timestamps of the window.
    fn judge_first(&self) -> Option<Gap> {
        let first = self.window[0];
        let interval = (self.window[1] - first) as f64;
        if interval <= GAP_THRESHOLD * self.frame_ns {
            return None;
        }
        let intervals = self.window.len() - 1;
        let span = (self.window[intervals] - first) as f64;
        let slots = (span / self.frame_ns).round() as i64;
        let missing_in_span = (slots - intervals as i64).max(0) as u64;
        let missing_in_gap = ((interval / self.frame_ns).round() as u64).saturating_sub(1);
        Some(Gap {
            after_ns: first,
            missing: missing_in_span.min(missing_in_gap),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    /// Feeds intervals (ms) at 30 fps and returns the gaps.
    fn gaps(intervals_ms: &[f64]) -> Vec<Gap> {
        let mut detector = DropDetector::new(1e9 / 30.0);
        let mut t = 0.0;
        let mut gaps: Vec<Gap> = detector.push(0).into_iter().collect();
        for interval in intervals_ms {
            t += interval;
            gaps.extend(detector.push((t * MS as f64) as u64));
        }
        gaps.extend(detector.finish());
        gaps
    }

    #[test]
    fn steady_stream_has_no_gaps() {
        assert!(gaps(&[33.3; 100]).is_empty());
    }

    #[test]
    fn late_frame_is_jitter() {
        // Measured on a USB camera: 62.8 ms, then 3.3 ms.
        let g = gaps(&[33.9, 33.3, 62.8, 3.3, 33.8, 33.3, 33.3]);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].missing, 0);
    }

    #[test]
    fn real_drops_are_counted() {
        let g = gaps(&[33.3, 66.7, 33.3, 33.3, 33.3, 33.3, 100.0, 33.3, 33.3, 33.3]);
        let missing: Vec<u64> = g.iter().map(|g| g.missing).collect();
        assert_eq!(missing, [1, 2]);
    }

    #[test]
    fn drop_at_the_end_is_judged_by_finish() {
        let g = gaps(&[33.3, 33.3, 33.3, 66.7]);
        assert_eq!(g.iter().map(|g| g.missing).sum::<u64>(), 1);
    }

    #[test]
    fn drop_with_partial_catch_up() {
        // Two frames missing, the next frame is 10 ms early: still two missing.
        let g = gaps(&[33.3, 100.0, 23.3, 33.3, 33.3, 33.3]);
        assert_eq!(g.iter().map(|g| g.missing).sum::<u64>(), 2);
    }

    #[test]
    fn fast_camera_clock_does_not_accumulate() {
        // 30.02 fps camera judged against nominal 30 fps for 10 minutes.
        let interval = 1000.0 / 30.02;
        assert!(gaps(&vec![interval; 18_000]).is_empty());
    }
}
