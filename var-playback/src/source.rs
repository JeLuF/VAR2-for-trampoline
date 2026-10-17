//! Reads the frames of one camera's running recording via its live index (AD-3).

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;

use var_core::index::{IndexRecord, LiveIndex};

use crate::ts::extract_access_unit;

/// Position of a frame: segment and frame number within the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FrameRef {
    pub segment: usize,
    pub frame: usize,
}

/// One encoded frame read from disk.
pub struct Packet {
    pub frame: FrameRef,
    pub record: IndexRecord,
    /// H.264 access unit (Annex B).
    pub data: Vec<u8>,
}

/// Frames of one camera, found by wall-clock time and read from the segment files.
/// Segments that can't be read (moved to another drive, REC-010) are skipped.
pub struct CameraSource {
    index: Arc<LiveIndex>,
    /// The open segment file.
    file: Option<(usize, File)>,
    /// Segments that could not be read.
    missing: HashSet<usize>,
    chunk: Vec<u8>,
}

impl CameraSource {
    pub fn new(index: Arc<LiveIndex>) -> Self {
        CameraSource {
            index,
            file: None,
            missing: HashSet::new(),
            chunk: Vec::new(),
        }
    }

    /// Nominal frame duration in µs.
    pub fn frame_us(&self) -> i64 {
        self.index.read(|data| {
            let (num, den) = data.fps;
            if num == 0 {
                20_000
            } else {
                1_000_000 * i64::from(den) / i64::from(num)
            }
        })
    }

    /// Wall-clock times of the first and the last available frame.
    pub fn range(&self) -> Option<(i64, i64)> {
        self.index.read(|data| {
            let first = data.segments.iter().find_map(|s| s.records.first())?;
            let last = data.segments.iter().rev().find_map(|s| s.records.last())?;
            Some((first.wall_us, last.wall_us))
        })
    }

    pub fn record(&self, frame: FrameRef) -> Option<IndexRecord> {
        self.index.read(|data| {
            data.segments
                .get(frame.segment)?
                .records
                .get(frame.frame)
                .copied()
        })
    }

    /// The latest frame captured at or before `wall_us` (AD-6).
    pub fn frame_at(&self, wall_us: i64) -> Option<FrameRef> {
        self.index.read(|data| {
            for (segment, entry) in data.segments.iter().enumerate().rev() {
                let records = &entry.records;
                if records.first().is_none_or(|r| r.wall_us > wall_us) {
                    continue;
                }
                let frame = records.partition_point(|r| r.wall_us <= wall_us) - 1;
                return Some(FrameRef { segment, frame });
            }
            None
        })
    }

    /// The first frame of the recording.
    pub fn first(&self) -> Option<FrameRef> {
        self.index.read(|data| {
            data.segments
                .iter()
                .position(|s| !s.records.is_empty())
                .map(|segment| FrameRef { segment, frame: 0 })
        })
    }

    /// The keyframe from which decoding must start to show `frame`.
    pub fn keyframe_for(&self, frame: FrameRef) -> FrameRef {
        self.index.read(|data| {
            let records = &data.segments[frame.segment].records;
            let key = records[..=frame.frame]
                .iter()
                .rposition(|r| r.keyframe)
                .unwrap_or(0);
            FrameRef {
                segment: frame.segment,
                frame: key,
            }
        })
    }

    /// The frame after `frame`, possibly in the next segment; `None` if it is not on
    /// disk yet.
    pub fn next(&self, frame: FrameRef) -> Option<FrameRef> {
        self.index.read(|data| {
            if frame.frame + 1 < data.segments.get(frame.segment)?.records.len() {
                return Some(FrameRef {
                    segment: frame.segment,
                    frame: frame.frame + 1,
                });
            }
            (frame.segment + 1..data.segments.len())
                .find(|&s| !data.segments[s].records.is_empty())
                .map(|segment| FrameRef { segment, frame: 0 })
        })
    }

    /// The frame before `frame`, possibly in an earlier segment.
    pub fn prev(&self, frame: FrameRef) -> Option<FrameRef> {
        if frame.frame > 0 {
            return Some(FrameRef {
                segment: frame.segment,
                frame: frame.frame - 1,
            });
        }
        self.index.read(|data| {
            (0..frame.segment)
                .rev()
                .find(|&s| !data.segments[s].records.is_empty())
                .map(|segment| FrameRef {
                    segment,
                    frame: data.segments[segment].records.len() - 1,
                })
        })
    }

    /// The first frame of the segment after `frame`'s segment.
    pub fn next_segment(&self, frame: FrameRef) -> Option<FrameRef> {
        self.index.read(|data| {
            (frame.segment + 1..data.segments.len())
                .find(|&s| !data.segments[s].records.is_empty())
                .map(|segment| FrameRef { segment, frame: 0 })
        })
    }

    /// Closes the open segment file; the next read opens it again. Called on jumps,
    /// so a segment moved away in the meantime is noticed (REC-010) and no file stays
    /// open longer than needed.
    pub fn close_file(&mut self) {
        self.file = None;
    }

    pub fn is_missing(&self, segment: usize) -> bool {
        self.missing.contains(&segment)
    }

    /// Reads a frame from its segment file.
    pub fn read(&mut self, frame: FrameRef) -> io::Result<Packet> {
        if self.missing.contains(&frame.segment) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "segment not available",
            ));
        }
        let (path, record) = self
            .index
            .read(|data| {
                let segment = data.segments.get(frame.segment)?;
                Some((segment.path.clone(), *segment.records.get(frame.frame)?))
            })
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "frame not in the index"))?;

        if self.file.as_ref().is_none_or(|(s, _)| *s != frame.segment) {
            // Opened with sharing for reading, writing and deleting (Rust's default on
            // Windows), so the operator can still move the file (REC-010).
            match File::open(&path) {
                Ok(file) => self.file = Some((frame.segment, file)),
                Err(e) => {
                    self.missing.insert(frame.segment);
                    self.file = None;
                    return Err(e);
                }
            }
        }
        let (_, file) = self.file.as_mut().unwrap();
        self.chunk.resize(record.size as usize, 0);
        let result = file
            .seek(SeekFrom::Start(record.offset))
            .and_then(|_| file.read_exact(&mut self.chunk));
        if let Err(e) = result {
            // E.g. the file was moved while open on another system.
            self.missing.insert(frame.segment);
            self.file = None;
            return Err(e);
        }

        let mut data = Vec::with_capacity(self.chunk.len());
        if !extract_access_unit(&self.chunk, &mut data) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid MPEG-TS data",
            ));
        }
        Ok(Packet {
            frame,
            record,
            data,
        })
    }
}
