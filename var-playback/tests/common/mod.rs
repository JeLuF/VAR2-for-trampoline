//! Shared setup: synthetic cameras recording into short segments with a live index.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use var_capture::{AttachedBranch, Camera, PixelFormat, synthetic};
use var_core::index::LiveIndex;
use var_record::{Anchor, EncodingSettings, RecordingBranch, SegmentConfig, select_encoder};

pub struct Recording {
    cameras: Vec<(Camera, RecordingBranch, AttachedBranch)>,
    /// `(slot, live index)` per camera, for the review player.
    pub sources: Vec<(usize, Arc<LiveIndex>)>,
}

/// Starts recording `count` synthetic cameras at 720p50 MJPEG into 2-second segments.
/// `None` if no encoder is available.
#[allow(dead_code)] // not used by every test
pub fn record(count: u32, dir: &Path) -> Option<Recording> {
    record_mode(count, dir, PixelFormat::Mjpeg, (1280, 720), 50)
}

/// Like [`record`], with another mode.
pub fn record_mode(
    count: u32,
    dir: &Path,
    format: PixelFormat,
    (width, height): (u32, u32),
    fps: i32,
) -> Option<Recording> {
    var_capture::init().unwrap();
    let Some(encoder) = select_encoder(false) else {
        eprintln!("skipped: no H.264 encoder");
        return None;
    };
    let _ = std::fs::remove_dir_all(dir);
    let anchor = Anchor::now();
    let mut recording = Recording {
        cameras: Vec::new(),
        sources: Vec::new(),
    };
    for (slot, device) in synthetic::devices(count).into_iter().enumerate() {
        let mode = device
            .modes
            .iter()
            .find(|m| {
                m.format == format
                    && m.width == width
                    && m.height == height
                    && m.capture_framerate() == (fps, 1)
            })
            .unwrap()
            .clone();
        let camera = Camera::start(&device, &mode, || {}).unwrap();
        let index = Arc::new(LiveIndex::new((fps as u32, 1)));
        let branch = RecordingBranch::new(
            &encoder,
            EncodingSettings::for_mode(width, height, fps, 1),
            SegmentConfig {
                dir: dir.join(&device.name),
                segment_length: Duration::from_secs(2),
                anchor,
                fps: (fps as u32, 1),
                live_index: Some(Arc::clone(&index)),
            },
        )
        .unwrap();
        let attached = camera.attach(branch.bin()).unwrap();
        recording.cameras.push((camera, branch, attached));
        recording.sources.push((slot, index));
    }
    Some(recording)
}

impl Recording {
    /// Stops recording and checks that it ended without errors.
    pub fn stop(self) {
        for (camera, _, attached) in &self.cameras {
            camera.detach(attached);
        }
        for (camera, branch, attached) in self.cameras {
            assert!(branch.wait_finished(Duration::from_secs(5)));
            camera.remove(attached);
            assert_eq!(branch.error(), None);
        }
    }
}

/// Waits until `check` holds, at most `timeout`.
pub fn wait_for(timeout: Duration, mut check: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    false
}
