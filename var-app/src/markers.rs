//! Markers (MRK-001 – MRK-007): moments set by referees to find them again.
//!
//! Stored as `markers.json` in the recording folder and saved every time a marker is
//! added (written to a temporary file and renamed, so the file is always complete).
//! Markers can't be edited or deleted (MRK-005).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// File name of the marker file in the recording folder.
pub const FILE_NAME: &str = "markers.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    /// Number in the order the markers were set, from 1.
    pub number: u32,
    /// Marked moment on the recording's timeline, µs since the Unix epoch.
    pub wall_us: i64,
    /// The same as local time, for people reading the file.
    pub time: String,
    /// "live" or "review": where the marker was set.
    pub set_in: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct MarkerFile {
    markers: Vec<Marker>,
}

/// The markers of one recording, sorted by time.
pub struct MarkerList {
    path: PathBuf,
    markers: Vec<Marker>,
}

impl MarkerList {
    /// The markers of the recording in `event_dir`; loads an existing marker file.
    pub fn new(event_dir: &Path) -> Self {
        let path = event_dir.join(FILE_NAME);
        let markers = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<MarkerFile>(&text).ok())
            .map(|file| file.markers)
            .unwrap_or_default();
        MarkerList { path, markers }
    }

    pub fn markers(&self) -> &[Marker] {
        &self.markers
    }

    /// Adds a marker and saves the file at once (MRK-006). The marker is kept in
    /// memory even if saving fails.
    pub fn add(&mut self, wall_us: i64, set_in: &str) -> (Marker, std::io::Result<()>) {
        let number = self.markers.iter().map(|m| m.number).max().unwrap_or(0) + 1;
        let marker = Marker {
            number,
            wall_us,
            time: local_time(wall_us),
            set_in: set_in.to_owned(),
        };
        let index = self.markers.partition_point(|m| m.wall_us <= wall_us);
        self.markers.insert(index, marker.clone());
        let saved = self.save();
        match &saved {
            Ok(()) => info!(number, time = %marker.time, set_in, "marker set"),
            Err(e) => warn!(number, %e, path = %self.path.display(), "cannot save markers"),
        }
        (marker, saved)
    }

    /// The first marker after `wall_us` (MRK-003).
    pub fn next_after(&self, wall_us: i64) -> Option<&Marker> {
        self.markers.iter().find(|m| m.wall_us > wall_us)
    }

    /// The last marker before `wall_us` (MRK-003).
    pub fn previous_before(&self, wall_us: i64) -> Option<&Marker> {
        self.markers.iter().rev().find(|m| m.wall_us < wall_us)
    }

    fn save(&self) -> std::io::Result<()> {
        let file = MarkerFile {
            markers: self.markers.clone(),
        };
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_string_pretty(&file)?)?;
        std::fs::rename(&temp, &self.path)
    }
}

/// Local date and time with milliseconds and offset, e.g. `2026-10-17T21:30:12.345+02:00`.
fn local_time(wall_us: i64) -> String {
    use chrono::{Local, TimeZone};
    Local
        .timestamp_micros(wall_us)
        .single()
        .map_or_else(String::new, |t| {
            t.format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_are_sorted_saved_and_reloaded() {
        let dir = std::env::temp_dir().join(format!("var-markers-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut list = MarkerList::new(&dir);
        list.add(3_000_000, "live").1.unwrap();
        list.add(1_000_000, "review").1.unwrap();
        list.add(2_000_000, "live").1.unwrap();

        let walls: Vec<i64> = list.markers().iter().map(|m| m.wall_us).collect();
        assert_eq!(walls, [1_000_000, 2_000_000, 3_000_000]);
        let numbers: Vec<u32> = list.markers().iter().map(|m| m.number).collect();
        assert_eq!(numbers, [2, 3, 1], "numbered in the order they were set");

        assert_eq!(list.next_after(1_000_000).unwrap().wall_us, 2_000_000);
        assert_eq!(list.previous_before(2_000_000).unwrap().wall_us, 1_000_000);
        assert!(list.next_after(3_000_000).is_none());

        // MRK-006: after a restart, all markers are there.
        let reloaded = MarkerList::new(&dir);
        assert_eq!(reloaded.markers(), list.markers());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
