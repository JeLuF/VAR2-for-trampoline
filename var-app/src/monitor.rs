//! Endurance monitoring (NFR-001, NFR-004): logs the process's resource use and the
//! frame counters of every camera periodically, so a 12-hour run can be evaluated
//! afterwards (`var-check --resources <log file>`).

use std::time::{Duration, Instant};

use tracing::{info, warn};
use var_core::resources::{self, Resources};

use crate::live::LiveCamera;
use crate::recording::Recorder;

/// Default interval between two log entries.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(60);

pub struct ResourceMonitor {
    interval: Duration,
    last: Option<(Instant, Resources)>,
    cores: f64,
}

impl ResourceMonitor {
    pub fn new(interval: Duration) -> Self {
        ResourceMonitor {
            interval,
            last: None,
            cores: std::thread::available_parallelism().map_or(1.0, |n| n.get() as f64),
        }
    }

    /// Logs a sample when the interval has passed. Call once per UI frame.
    pub fn update(&mut self, cameras: &[LiveCamera], recorder: &Recorder) {
        let now = Instant::now();
        if self
            .last
            .is_some_and(|(at, _)| now.duration_since(at) < self.interval)
        {
            return;
        }
        let Some(sample) = resources::sample() else {
            warn!("cannot sample the resource use");
            self.last = Some((now, Resources::default()));
            return;
        };
        // CPU use since the last sample, in percent of the whole machine.
        let cpu_percent = self.last.map(|(at, last)| {
            let cpu = sample.cpu_time.saturating_sub(last.cpu_time).as_secs_f64();
            cpu / now.duration_since(at).as_secs_f64() / self.cores * 100.0
        });
        self.last = Some((now, sample));

        let mb = |bytes: u64| bytes / 1_000_000;
        info!(
            rss_mb = mb(sample.resident_bytes),
            private_mb = sample.private_bytes.map(mb),
            cpu_percent = cpu_percent.map(|p| format!("{p:.1}")),
            handles = sample.handles,
            threads = sample.threads,
            recording = recorder.is_recording(),
            "resources"
        );
        for (slot, camera) in cameras.iter().enumerate() {
            let Some((camera, _, generation)) = camera.camera() else {
                continue;
            };
            let stats = camera.stats();
            info!(
                slot = slot + 1,
                generation,
                captured = stats.captured,
                dropped = stats.dropped,
                late = stats.late,
                fps = format!("{:.2}", stats.fps),
                "camera counters"
            );
        }
        if let Some(status) = recorder.status(cameras) {
            for camera in &status.cameras {
                info!(
                    slot = camera.slot + 1,
                    frames = camera.stats.frames,
                    mb = camera.total_bytes / 1_000_000,
                    interruptions = camera.interruptions,
                    "recording counters"
                );
            }
        }
    }
}
