//! Segment writer: MPEG-TS segments with an index per segment (AD-3).
//!
//! A new segment starts on the first keyframe after the segment length. Completed
//! segments are closed and never touched again, so they can be moved to another drive
//! during the recording (REC-010).

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gstreamer as gst;
use gstreamer::prelude::*;
use tracing::{info, warn};
use var_core::index::{self, IndexHeader, IndexRecord, LiveIndex};

use crate::ts::{TIMESTAMP_MASK, TsMuxer};

/// Write buffer of a segment file; flushed at every keyframe anyway.
const WRITE_BUFFER: usize = 1 << 20;

/// The first frame of a recording gets this MPEG-TS timestamp (1 s), so the PCR,
/// which runs ahead of the timestamps, never becomes negative.
const PTS_OFFSET: u64 = 90_000;

/// Links the pipelines' monotonic clock to the wall clock, taken once when the
/// recording starts and shared by all cameras (AD-5). The wall-clock time of a frame
/// is derived from its capture time on the monotonic clock, so changes of the system
/// time during the event (NTP) don't make the recording jump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    /// Monotonic clock time (GStreamer system clock), ns.
    clock_ns: u64,
    /// Wall-clock time at `clock_ns`, µs since the Unix epoch.
    wall_us: i64,
}

impl Anchor {
    /// Takes the anchor now.
    pub fn now() -> Self {
        let clock_ns = gst::SystemClock::obtain().time().nseconds();
        let wall_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_micros() as i64);
        Anchor { clock_ns, wall_us }
    }

    /// An anchor with given values (tests).
    pub fn new(clock_ns: u64, wall_us: i64) -> Self {
        Anchor { clock_ns, wall_us }
    }

    /// The current time on the recording's timeline (wall clock derived from the
    /// monotonic clock, like the frames' capture times).
    pub fn now_wall_us(&self) -> i64 {
        self.wall_us(gst::SystemClock::obtain().time().nseconds())
    }

    /// Wall-clock time of a monotonic clock time, µs since the Unix epoch.
    pub fn wall_us(&self, clock_ns: u64) -> i64 {
        self.wall_us + (clock_ns as i64 - self.clock_ns as i64) / 1000
    }

    /// MPEG-TS timestamp (90 kHz) of a monotonic clock time. Equal for all cameras of a
    /// recording, and continuous across segments.
    pub fn pts90(&self, clock_ns: u64) -> u64 {
        let since = clock_ns as i128 - self.clock_ns as i128;
        let ticks = (since * 9 / 100_000 + PTS_OFFSET as i128).max(0) as u64;
        ticks & TIMESTAMP_MASK
    }
}

/// Where and how a camera's segments are written.
#[derive(Debug, Clone)]
pub struct SegmentConfig {
    /// Folder of the camera within the event folder.
    pub dir: PathBuf,
    /// A new segment starts on the first keyframe after this length.
    pub segment_length: Duration,
    pub anchor: Anchor,
    pub fps: (u32, u32),
    /// In-memory index for review while recording; gets the records of all frames
    /// once they are flushed to disk.
    pub live_index: Option<Arc<LiveIndex>>,
}

struct Segment {
    number: u32,
    ts_path: PathBuf,
    ts: BufWriter<File>,
    index: BufWriter<File>,
    /// Bytes written to the segment so far.
    offset: u64,
    start_clock_ns: u64,
    frames: u64,
    /// Records written since the last flush; published to the live index after it.
    unpublished: Vec<IndexRecord>,
}

impl Segment {
    /// Flushes data, then index file, then publishes the flushed records, so neither
    /// the index file nor the live index ever points past the data on disk.
    fn flush(&mut self, live_index: Option<&LiveIndex>) -> io::Result<()> {
        self.ts.flush()?;
        self.index.flush()?;
        if let Some(live_index) = live_index {
            live_index.publish(&self.unpublished);
        }
        self.unpublished.clear();
        Ok(())
    }
}

/// Writes one camera's frames into segments.
pub struct SegmentWriter {
    config: SegmentConfig,
    muxer: TsMuxer,
    current: Option<Segment>,
    next_number: u32,
    packet_buffer: Vec<u8>,
    /// Segments completed so far.
    completed: u32,
    /// Bytes written to disk: segment files and indexes.
    bytes_written: u64,
}

impl SegmentWriter {
    /// Creates the camera folder. The first segment is created with the first keyframe.
    /// If the folder already has segments (the camera was restarted after a failure),
    /// numbering continues after the highest existing number.
    pub fn new(config: SegmentConfig) -> io::Result<Self> {
        std::fs::create_dir_all(&config.dir)?;
        let next_number = segment_files(&config.dir)?
            .iter()
            .filter_map(|p| {
                p.file_name()?
                    .to_str()?
                    .split('_')
                    .next()?
                    .parse::<u32>()
                    .ok()
            })
            .max()
            .map_or(1, |n| n + 1);
        Ok(SegmentWriter {
            config,
            muxer: TsMuxer::new(),
            current: None,
            next_number,
            packet_buffer: Vec::with_capacity(1 << 20),
            completed: 0,
            bytes_written: 0,
        })
    }

    pub fn segments_completed(&self) -> u32 {
        self.completed
    }

    /// Bytes written to disk so far (segments and indexes), for the disk space
    /// estimate (DSK-002).
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Writes one H.264 access unit captured at `clock_ns` (monotonic clock).
    /// Frames before the first keyframe are skipped. Returns whether it was written.
    pub fn write_frame(&mut self, au: &[u8], clock_ns: u64, keyframe: bool) -> io::Result<bool> {
        let roll_over = keyframe
            && self.current.as_ref().is_some_and(|s| {
                clock_ns.saturating_sub(s.start_clock_ns)
                    >= self.config.segment_length.as_nanos() as u64
            });
        if roll_over {
            self.finish_segment()?;
        }
        if self.current.is_none() {
            if !keyframe {
                return Ok(false);
            }
            self.current = Some(self.open_segment(clock_ns)?);
        }

        // Flush before every keyframe (every 0.5 s): limits data lost in a crash, and
        // makes the frames available for review while recording within about 0.5 s
        // (REV-002).
        if keyframe {
            let segment = self.current.as_mut().unwrap();
            if segment.frames > 0 {
                segment.flush(self.config.live_index.as_deref())?;
            }
        }

        let pts90 = self.config.anchor.pts90(clock_ns);
        self.packet_buffer.clear();
        self.muxer
            .write_frame(&mut self.packet_buffer, au, pts90, pts90, keyframe);

        let segment = self.current.as_mut().unwrap();
        segment.ts.write_all(&self.packet_buffer)?;
        let record = IndexRecord {
            pts90,
            wall_us: self.config.anchor.wall_us(clock_ns),
            offset: segment.offset,
            size: self.packet_buffer.len() as u32,
            keyframe,
        };
        segment.index.write_all(&record.to_bytes())?;
        segment.unpublished.push(record);
        segment.offset += self.packet_buffer.len() as u64;
        segment.frames += 1;
        self.bytes_written += (self.packet_buffer.len() + index::RECORD_SIZE) as u64;
        Ok(true)
    }

    /// Flushes and closes the current segment.
    pub fn finish(&mut self) -> io::Result<()> {
        self.finish_segment()
    }

    fn open_segment(&mut self, clock_ns: u64) -> io::Result<Segment> {
        let number = self.next_number;
        self.next_number += 1;
        let start = chrono_local(self.config.anchor.wall_us(clock_ns));
        let stem = format!("{number:04}_{start}");
        let ts_path = self.config.dir.join(format!("{stem}.ts"));
        let index_path = self.config.dir.join(format!("{stem}.{}", index::EXTENSION));

        let ts = BufWriter::with_capacity(WRITE_BUFFER, File::create(&ts_path)?);
        let mut index_file = BufWriter::with_capacity(64 * 1024, File::create(&index_path)?);
        index::write_header(
            &mut index_file,
            &IndexHeader {
                fps_num: self.config.fps.0,
                fps_den: self.config.fps.1,
            },
        )?;
        info!(path = %ts_path.display(), "segment started");
        if let Some(live_index) = &self.config.live_index {
            live_index.start_segment(ts_path.clone());
        }
        Ok(Segment {
            number,
            ts_path,
            ts,
            index: index_file,
            offset: 0,
            start_clock_ns: clock_ns,
            frames: 0,
            unpublished: Vec::new(),
        })
    }

    fn finish_segment(&mut self) -> io::Result<()> {
        let Some(mut segment) = self.current.take() else {
            return Ok(());
        };
        let result = segment.flush(self.config.live_index.as_deref());
        match &result {
            Ok(()) => {
                self.completed += 1;
                info!(
                    path = %segment.ts_path.display(),
                    number = segment.number,
                    frames = segment.frames,
                    bytes = segment.offset,
                    "segment completed"
                );
            }
            Err(e) => warn!(path = %segment.ts_path.display(), %e, "cannot complete segment"),
        }
        result
    }
}

/// Local time `HH-MM-SS` of a wall-clock time, for file names.
fn chrono_local(wall_us: i64) -> String {
    use chrono::{Local, TimeZone};
    Local.timestamp_micros(wall_us).single().map_or_else(
        || "00-00-00".to_owned(),
        |t| t.format("%H-%M-%S").to_string(),
    )
}

/// Segment files of a camera folder, in recording order.
pub fn segment_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "ts"))
        .collect();
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_maps_clock_to_wall_and_pts() {
        let anchor = Anchor::new(5_000_000_000, 1_760_000_000_000_000);
        assert_eq!(anchor.wall_us(5_000_000_000), 1_760_000_000_000_000);
        assert_eq!(anchor.wall_us(6_500_000_000), 1_760_000_001_500_000);
        assert_eq!(anchor.pts90(5_000_000_000), 90_000);
        assert_eq!(anchor.pts90(6_000_000_000), 180_000);
        // Frames captured shortly before the anchor still get valid timestamps.
        assert_eq!(anchor.pts90(4_500_000_000), 45_000);
    }

    #[test]
    fn segments_roll_over_on_keyframes() {
        let dir = std::env::temp_dir().join(format!("var-segment-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let anchor = Anchor::new(0, 1_760_000_000_000_000);
        let mut writer = SegmentWriter::new(SegmentConfig {
            dir: dir.clone(),
            segment_length: Duration::from_secs(1),
            anchor,
            fps: (10, 1),
            live_index: None,
        })
        .unwrap();

        // 10 fps for 2.5 s, keyframe every 5 frames; a delta frame first (skipped).
        assert!(!writer.write_frame(&[0, 0, 0, 1, 0x41], 0, false).unwrap());
        for i in 1..=25u64 {
            let keyframe = i % 5 == 1;
            assert!(
                writer
                    .write_frame(&[0, 0, 0, 1, 0x65, i as u8], i * 100_000_000, keyframe)
                    .unwrap()
            );
        }
        writer.finish().unwrap();
        assert_eq!(writer.segments_completed(), 3);

        let files = segment_files(&dir).unwrap();
        assert_eq!(files.len(), 3);
        let mut total = 0;
        for (n, ts) in files.iter().enumerate() {
            let data = std::fs::read(ts).unwrap();
            let (header, records) =
                index::read(&mut File::open(ts.with_extension("idx")).unwrap()).unwrap();
            assert_eq!((header.fps_num, header.fps_den), (10, 1));
            assert!(records[0].keyframe, "segment {n} starts with a keyframe");
            for r in &records {
                assert_eq!(data[r.offset as usize], 0x47);
            }
            let last = records.last().unwrap();
            assert_eq!(last.offset + u64::from(last.size), data.len() as u64);
            total += records.len();
        }
        assert_eq!(total, 25);

        // A new writer for the same folder (camera restarted) continues the numbering.
        let mut writer = SegmentWriter::new(SegmentConfig {
            dir: dir.clone(),
            segment_length: Duration::from_secs(1),
            anchor,
            fps: (10, 1),
            live_index: None,
        })
        .unwrap();
        writer
            .write_frame(&[0, 0, 0, 1, 0x65], 5_000_000_000, true)
            .unwrap();
        writer.finish().unwrap();
        let files = segment_files(&dir).unwrap();
        assert_eq!(files.len(), 4);
        assert!(
            files[3]
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("0004_")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
