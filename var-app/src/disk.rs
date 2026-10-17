//! Disk space monitoring (DSK-001 – DSK-003): free space of the recording drive and
//! the estimated remaining recording time, with warnings at two thresholds.
//!
//! Before recording, the estimate uses the configured bitrates of the selected
//! cameras; during recording, the data actually written over the last minute.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::{info, warn};

/// Default warning thresholds in minutes of remaining recording time (DSK-003).
pub const DEFAULT_WARNING_MINUTES: [u32; 2] = [120, 30];
/// How often the free space is read.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// The measured data rate is averaged over this window...
const RATE_WINDOW: Duration = Duration::from_secs(60);
/// ...and used once at least this much has been recorded.
const MIN_RATE_SPAN: Duration = Duration::from_secs(10);
/// How often the estimate is logged while recording.
const LOG_INTERVAL: Duration = Duration::from_secs(60);

/// How critical the remaining recording time is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok,
    /// Below the first threshold (default 2 h).
    Low,
    /// Below the second threshold (default 30 min).
    Critical,
}

/// Free space and estimated remaining recording time.
#[derive(Debug, Clone, Default)]
pub struct DiskEstimate {
    /// Free bytes on the recording drive; `None` until read, or on error.
    pub free: Option<u64>,
    pub error: Option<String>,
    /// Expected disk usage of the recording, bytes per second; 0 without cameras.
    pub bytes_per_second: f64,
    /// The rate was measured (recording) rather than taken from the configuration.
    pub measured: bool,
    pub remaining: Option<Duration>,
    pub level: Option<Level>,
}

pub struct DiskMonitor {
    polled: Arc<Mutex<Polled>>,
    /// Debug: free space as if the drive had this many bytes free at recording start
    /// (`--simulate-free-gb`, Debug menu).
    pub simulated_free: Option<u64>,
    /// Bytes recorded at times, for the measured rate.
    samples: VecDeque<(Instant, u64)>,
    /// Warning thresholds in minutes, the first larger than the second.
    pub warning_minutes: [u32; 2],
    /// The level last logged.
    logged_level: Level,
    /// The banner is hidden up to this level (the operator dismissed it).
    dismissed: Level,
    last_log: Option<Instant>,
    estimate: DiskEstimate,
}

struct Polled {
    folder: PathBuf,
    free: Option<Result<u64, String>>,
}

impl DiskMonitor {
    /// Starts reading the free space in the background: a slow or hanging drive (e.g.
    /// a network share) never blocks the UI.
    pub fn new(folder: &str) -> Self {
        let polled = Arc::new(Mutex::new(Polled {
            folder: PathBuf::from(folder),
            free: None,
        }));
        std::thread::Builder::new()
            .name("var-disk".into())
            .spawn({
                let polled = Arc::clone(&polled);
                move || {
                    loop {
                        let folder = polled.lock().unwrap().folder.clone();
                        let free =
                            var_record::disk::available_space(&folder).map_err(|e| e.to_string());
                        let mut polled = polled.lock().unwrap();
                        // The folder may have changed while reading.
                        if polled.folder == folder {
                            polled.free = Some(free);
                        }
                        drop(polled);
                        std::thread::sleep(POLL_INTERVAL);
                    }
                }
            })
            .expect("failed to spawn disk monitor thread");
        DiskMonitor {
            polled,
            simulated_free: None,
            samples: VecDeque::new(),
            warning_minutes: DEFAULT_WARNING_MINUTES,
            logged_level: Level::Ok,
            dismissed: Level::Ok,
            last_log: None,
            estimate: DiskEstimate::default(),
        }
    }

    /// Updates the estimate. `configured_rate`: bytes per second of the selected
    /// cameras with their configured bitrates; `recorded`: bytes written by the
    /// running recording, `None` when not recording.
    pub fn update(&mut self, folder: &str, configured_rate: f64, recorded: Option<u64>) {
        let free = {
            let mut polled = self.polled.lock().unwrap();
            let folder = Path::new(folder.trim());
            if polled.folder != folder {
                polled.folder = folder.to_owned();
                polled.free = None;
            }
            polled.free.clone()
        };

        let now = Instant::now();
        match recorded {
            Some(bytes) => {
                if self
                    .samples
                    .back()
                    .is_none_or(|(t, _)| now.duration_since(*t) >= Duration::from_secs(1))
                {
                    self.samples.push_back((now, bytes));
                }
                while self.samples.len() > 2 && now.duration_since(self.samples[0].0) > RATE_WINDOW
                {
                    self.samples.pop_front();
                }
            }
            None => self.samples.clear(),
        }
        let measured_rate = match (self.samples.front(), self.samples.back()) {
            (Some(&(t0, b0)), Some(&(t1, b1))) if t1.duration_since(t0) >= MIN_RATE_SPAN => {
                Some(b1.saturating_sub(b0) as f64 / t1.duration_since(t0).as_secs_f64())
            }
            _ => None,
        };

        let (free, error) = match (self.simulated_free, free) {
            (Some(simulated), _) => (Some(simulated.saturating_sub(recorded.unwrap_or(0))), None),
            (None, Some(Ok(free))) => (Some(free), None),
            (None, Some(Err(e))) => (None, Some(e)),
            (None, None) => (None, None),
        };
        let bytes_per_second = measured_rate.unwrap_or(configured_rate);
        let remaining = free
            .filter(|_| bytes_per_second > 0.0)
            .map(|free| Duration::from_secs_f64(free as f64 / bytes_per_second));
        let level = remaining.map(|remaining| self.level(remaining));

        self.estimate = DiskEstimate {
            free,
            error,
            bytes_per_second,
            measured: measured_rate.is_some(),
            remaining,
            level,
        };
        self.log(recorded.is_some());
    }

    pub fn estimate(&self) -> &DiskEstimate {
        &self.estimate
    }

    fn level(&self, remaining: Duration) -> Level {
        let minutes = remaining.as_secs_f64() / 60.0;
        let [first, second] = self.warning_minutes;
        if minutes < f64::from(second) {
            Level::Critical
        } else if minutes < f64::from(first) {
            Level::Low
        } else {
            Level::Ok
        }
    }

    /// Logs warnings when the level changes (NFR-005), and the estimate once a minute
    /// while recording.
    fn log(&mut self, recording: bool) {
        let estimate = &self.estimate;
        let level = estimate.level.unwrap_or(Level::Ok);
        let free_gb = estimate.free.map(|f| format!("{:.2}", f as f64 / 1e9));
        let remaining = estimate.remaining.map(format_remaining);
        if level != self.logged_level {
            if level > self.logged_level {
                warn!(?level, ?free_gb, ?remaining, "disk space warning");
            } else if level > Level::Ok {
                info!(
                    ?level,
                    ?free_gb,
                    ?remaining,
                    "disk space warning level lowered"
                );
            } else {
                info!(?level, ?free_gb, ?remaining, "disk space sufficient again");
            }
            self.logged_level = level;
        }
        if level < self.dismissed {
            self.dismissed = level;
        }
        if recording {
            if self.last_log.is_none_or(|t| t.elapsed() >= LOG_INTERVAL) {
                info!(
                    ?free_gb,
                    rate_mb_s = format!("{:.3}", estimate.bytes_per_second / 1e6),
                    measured = estimate.measured,
                    ?remaining,
                    "disk estimate"
                );
                self.last_log = Some(Instant::now());
            }
        } else {
            self.last_log = None;
        }
    }

    /// The warning to show as a banner, unless dismissed (DSK-003).
    pub fn warning(&self) -> Option<Level> {
        self.estimate
            .level
            .filter(|level| *level > Level::Ok && *level > self.dismissed)
    }

    /// Hides the banner until the next threshold is crossed.
    pub fn dismiss(&mut self) {
        if let Some(level) = self.estimate.level {
            self.dismissed = level;
        }
    }
}

/// `3 h 05 min`, `45 min`, `< 1 min`.
pub fn format_remaining(remaining: Duration) -> String {
    let minutes = remaining.as_secs() / 60;
    match minutes {
        0 => "< 1 min".to_owned(),
        1..60 => format!("{minutes} min"),
        _ => format!("{} h {:02} min", minutes / 60, minutes % 60),
    }
}

/// `123.4 GB`, `850 MB`.
pub fn format_bytes(bytes: u64) -> String {
    let gb = bytes as f64 / 1e9;
    if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{:.0} MB", bytes as f64 / 1e6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(format_remaining(Duration::from_secs(30)), "< 1 min");
        assert_eq!(
            format_remaining(Duration::from_secs(45 * 60 + 10)),
            "45 min"
        );
        assert_eq!(
            format_remaining(Duration::from_secs(3 * 3600 + 5 * 60)),
            "3 h 05 min"
        );
        assert_eq!(format_bytes(123_456_000_000), "123.5 GB");
        assert_eq!(format_bytes(850_000_000), "850 MB");
    }

    #[test]
    fn simulated_space_gives_levels_and_warnings() {
        let mut monitor = DiskMonitor::new(&std::env::temp_dir().to_string_lossy());
        // 2 cameras at about 15 Mbit/s: 3.75 MB/s, 13.5 GB per hour.
        let rate = 3.75e6;
        let cases = [
            (50e9, Level::Ok),      // 3 h 42 min
            (20e9, Level::Low),     // 1 h 29 min
            (5e9, Level::Critical), // 22 min
        ];
        for (free, level) in cases {
            monitor.simulated_free = Some(free as u64);
            monitor.update(&std::env::temp_dir().to_string_lossy(), rate, None);
            let estimate = monitor.estimate();
            assert_eq!(estimate.level, Some(level), "free {free}");
            assert!(!estimate.measured);
            let expected = free / rate;
            assert!((estimate.remaining.unwrap().as_secs_f64() - expected).abs() < 1.0);
        }
        // Critical warning shown; dismissed until the level gets worse again.
        assert_eq!(monitor.warning(), Some(Level::Critical));
        monitor.dismiss();
        assert_eq!(monitor.warning(), None);

        // More space again (segments moved away): no new warning for the lower level...
        monitor.simulated_free = Some(20e9 as u64);
        monitor.update(&std::env::temp_dir().to_string_lossy(), rate, None);
        assert_eq!(monitor.warning(), None);
        // ...but crossing the second threshold again shows it.
        monitor.simulated_free = Some(5e9 as u64);
        monitor.update(&std::env::temp_dir().to_string_lossy(), rate, None);
        assert_eq!(monitor.warning(), Some(Level::Critical));

        // No cameras: no estimate.
        monitor.update(&std::env::temp_dir().to_string_lossy(), 0.0, None);
        assert_eq!(monitor.estimate().remaining, None);
        assert_eq!(monitor.warning(), None);
    }

    #[test]
    fn measured_rate_replaces_configured() {
        let mut monitor = DiskMonitor::new(&std::env::temp_dir().to_string_lossy());
        monitor.simulated_free = Some(10_000_000_000);
        let start = Instant::now() - Duration::from_secs(30);
        // 2 MB/s written over 30 s.
        for s in 0..30u64 {
            monitor
                .samples
                .push_back((start + Duration::from_secs(s), s * 2_000_000));
        }
        monitor.update(
            &std::env::temp_dir().to_string_lossy(),
            5e6,
            Some(60_000_000),
        );
        let estimate = monitor.estimate();
        assert!(estimate.measured);
        assert!(
            (estimate.bytes_per_second - 2e6).abs() < 0.1e6,
            "{}",
            estimate.bytes_per_second
        );
        // Simulated free space shrinks by what was recorded.
        assert_eq!(estimate.free, Some(10_000_000_000 - 60_000_000));
    }
}
