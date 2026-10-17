//! Evaluation of the application's resource log (endurance runs, NFR-001, NFR-004):
//! the `resources`, `camera counters` and `recording counters` entries that the
//! application writes every minute.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;
use chrono::NaiveDateTime;

/// One `resources` entry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Seconds since the first entry.
    pub at_s: f64,
    pub rss_mb: f64,
    pub private_mb: Option<f64>,
    pub cpu_percent: Option<f64>,
    pub handles: Option<f64>,
    pub recording: bool,
}

/// Counters of one camera slot over the run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CameraTotals {
    /// Frames dropped by the source, summed over all pipeline generations (restarts).
    pub dropped: u64,
    pub captured: u64,
    /// Pipeline generations seen; more than one means the camera was restarted.
    pub generations: usize,
    /// Recording interruptions reported last.
    pub interruptions: u64,
}

/// Memory in a reference window after the warm-up and in the last window, MB; window
/// bounds in seconds since recording start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MemoryWindows {
    pub reference: (f64, f64),
    pub last: (f64, f64),
    pub reference_peak: f64,
    pub reference_mean: f64,
    pub last_peak: f64,
    pub last_mean: f64,
}

impl MemoryWindows {
    /// Change of the peak, percent: a leak raises the peak.
    pub fn peak_change_percent(&self) -> f64 {
        (self.last_peak / self.reference_peak - 1.0) * 100.0
    }

    pub fn mean_change_percent(&self) -> f64 {
        (self.last_mean / self.reference_mean - 1.0) * 100.0
    }
}

#[derive(Debug, Default)]
pub struct ResourceReport {
    pub samples: Vec<Sample>,
    pub cameras: BTreeMap<u32, CameraTotals>,
    pub camera_failures: usize,
    pub errors: usize,
    pub warnings: usize,
}

impl ResourceReport {
    /// Duration covered by the samples, seconds.
    pub fn duration_s(&self) -> f64 {
        self.samples.last().map_or(0.0, |s| s.at_s)
    }

    /// Memory (private if available, else resident) of the samples while recording.
    fn memory(&self) -> Vec<(f64, f64)> {
        self.samples
            .iter()
            .filter(|s| s.recording)
            .map(|s| (s.at_s, s.private_mb.unwrap_or(s.rss_mb)))
            .collect()
    }

    /// NFR-001: memory after the warm-up compared with the end of the recording. The
    /// memory use goes up and down by several hundred MB with review (frame caches),
    /// so single values can't be compared: peak and mean over two windows are. The
    /// reference window starts after the first hour (after 10 minutes for runs shorter
    /// than 1.5 hours); both windows are up to 30 minutes long and don't overlap.
    /// `None` for runs too short to compare.
    pub fn memory_windows(&self) -> Option<MemoryWindows> {
        let memory = self.memory();
        let start = memory.first()?.0;
        let points: Vec<(f64, f64)> = memory.iter().map(|(t, m)| (t - start, *m)).collect();
        let duration = points.last()?.0;
        let warm_up = if duration >= 5400.0 { 3600.0 } else { 600.0 };
        let window = ((duration - warm_up) / 2.0).min(1800.0);
        if window < 240.0 {
            return None;
        }
        let stats = |from: f64, to: f64| {
            let values: Vec<f64> = points
                .iter()
                .filter(|(t, _)| (from..=to).contains(t))
                .map(|(_, m)| *m)
                .collect();
            let peak = values.iter().copied().fold(0.0, f64::max);
            let mean = values.iter().sum::<f64>() / values.len().max(1) as f64;
            (peak, mean)
        };
        let reference = (warm_up, warm_up + window);
        let last = (duration - window, duration);
        let (reference_peak, reference_mean) = stats(reference.0, reference.1);
        let (last_peak, last_mean) = stats(last.0, last.1);
        Some(MemoryWindows {
            reference,
            last,
            reference_peak,
            reference_mean,
            last_peak,
            last_mean,
        })
    }

    /// Mean memory after the warm-up, MB.
    pub fn memory_mean_mb(&self) -> Option<f64> {
        let memory = self.memory();
        let start = memory.first()?.0;
        let values: Vec<f64> = memory
            .iter()
            .filter(|(t, _)| *t >= start + 600.0)
            .map(|(_, m)| *m)
            .collect();
        (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
    }

    pub fn handles_range(&self) -> Option<(f64, f64, f64)> {
        let handles: Vec<f64> = self
            .samples
            .iter()
            .filter(|s| s.recording)
            .filter_map(|s| s.handles)
            .collect();
        let first = *handles.first()?;
        let last = *handles.last()?;
        Some((first, last, handles.iter().copied().fold(0.0, f64::max)))
    }
}

/// Reads the resource entries of one or more log files (in order: a run over midnight
/// writes two daily files).
pub fn resource_log(paths: &[&Path]) -> anyhow::Result<ResourceReport> {
    let mut report = ResourceReport::default();
    let mut first: Option<NaiveDateTime> = None;
    // Per slot and generation: the last counters.
    let mut counters: BTreeMap<(u32, u64), (u64, u64)> = BTreeMap::new();
    for path in paths {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        for line in text.lines() {
            let Some(time) = line
                .get(..23)
                .and_then(|t| NaiveDateTime::parse_from_str(t, "%Y-%m-%d %H:%M:%S%.3f").ok())
            else {
                continue;
            };
            let level = line.get(24..29).unwrap_or("").trim();
            match level {
                "ERROR" => report.errors += 1,
                "WARN" => report.warnings += 1,
                _ => {}
            }
            if line.contains("camera failed") {
                report.camera_failures += 1;
            }
            let fields = fields(line);
            let number = |name: &str| fields.get(name).and_then(|v| v.parse::<f64>().ok());
            if line.contains(" resources ") || line.contains(": resources") {
                let start = *first.get_or_insert(time);
                let Some(rss_mb) = number("rss_mb") else {
                    continue;
                };
                report.samples.push(Sample {
                    at_s: (time - start).as_seconds_f64(),
                    rss_mb,
                    private_mb: number("private_mb"),
                    cpu_percent: number("cpu_percent"),
                    handles: number("handles"),
                    recording: fields.get("recording").is_some_and(|v| v == "true"),
                });
            } else if line.contains("camera counters") {
                if let (Some(slot), Some(generation), Some(captured), Some(dropped)) = (
                    number("slot"),
                    number("generation"),
                    number("captured"),
                    number("dropped"),
                ) {
                    counters.insert(
                        (slot as u32, generation as u64),
                        (captured as u64, dropped as u64),
                    );
                }
            } else if line.contains("recording counters")
                && let (Some(slot), Some(interruptions)) = (number("slot"), number("interruptions"))
            {
                report.cameras.entry(slot as u32).or_default().interruptions = interruptions as u64;
            }
        }
    }
    for ((slot, _), (captured, dropped)) in counters {
        let totals = report.cameras.entry(slot).or_default();
        totals.captured += captured;
        totals.dropped += dropped;
        totals.generations += 1;
    }
    Ok(report)
}

/// `key=value` fields of a log line; quotes around values removed.
fn fields(line: &str) -> BTreeMap<String, String> {
    line.split_whitespace()
        .filter_map(|word| {
            let (key, value) = word.split_once('=')?;
            Some((key.to_owned(), value.trim_matches('"').to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_a_log() {
        let dir = std::env::temp_dir().join(format!("var-check-resources-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("var.log");
        let mut log = String::new();
        // 2 hours, one entry per minute, memory 1000 MB rising to 1020 MB.
        for minute in 0..=120u32 {
            let time = format!(
                "2026-10-17 {:02}:{:02}:00.000",
                10 + minute / 60,
                minute % 60
            );
            log += &format!(
                "{time}  INFO var::monitor: resources rss_mb=900 private_mb={} cpu_percent=\"40.5\" handles={} recording=true\n",
                1000 + minute / 6,
                1700 + minute % 3
            );
            let generation = if minute < 60 { 1 } else { 2 };
            log += &format!(
                "{time}  INFO var::monitor: camera counters slot=1 generation={generation} captured={} dropped={} late=0 fps=\"50.00\"\n",
                minute * 3000,
                u32::from((30..60).contains(&minute)) * 2
            );
            log += &format!(
                "{time}  INFO var::monitor: recording counters slot=1 frames=1 mb=1 interruptions={}\n",
                u32::from(minute >= 60)
            );
        }
        log += "2026-10-17 11:00:00.000 ERROR var::live: camera failed device=Synthetic camera 1\n";
        std::fs::write(&path, log).unwrap();

        let report = resource_log(&[&path]).unwrap();
        assert_eq!(report.samples.len(), 121);
        assert_eq!(report.duration_s(), 7200.0);
        // 2 h: reference window 1:00–1:30, last window 1:30–2:00.
        let memory = report.memory_windows().unwrap();
        assert_eq!(memory.reference, (3600.0, 5400.0));
        assert_eq!(memory.last, (5400.0, 7200.0));
        assert_eq!((memory.reference_peak, memory.last_peak), (1015.0, 1020.0));
        assert!((memory.peak_change_percent() - 0.49).abs() < 0.01);
        assert_eq!(report.handles_range(), Some((1700.0, 1700.0, 1702.0)));
        let camera = &report.cameras[&1];
        assert_eq!(camera.generations, 2);
        assert_eq!(
            camera.dropped, 2,
            "generation 1 ended with 2 dropped, generation 2 none"
        );
        assert_eq!(camera.interruptions, 1);
        assert_eq!(report.camera_failures, 1);
        assert_eq!(report.errors, 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
