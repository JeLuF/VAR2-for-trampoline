//! Segment index: one record per frame of an MPEG-TS segment (AD-3).
//!
//! The index file sits next to its segment (`0001_21-30-00.ts` → `0001_21-30-00.idx`)
//! and is appended to as frames are written, so it can be read while it grows.
//!
//! Layout (little endian): a 32-byte header, then 32-byte records.
//!
//! | Header field | Type |
//! |---|---|
//! | magic `VARIDX01` | 8 bytes |
//! | version (1) | u32 |
//! | record size (32) | u32 |
//! | frame rate numerator, denominator | u32, u32 |
//! | reserved | 8 bytes |
//!
//! | Record field | Type | |
//! |---|---|---|
//! | `pts90` | u64 | MPEG-TS presentation timestamp (90 kHz) written in the segment |
//! | `wall_us` | i64 | capture time, wall clock, µs since the Unix epoch (UTC) |
//! | `offset` | u64 | byte offset of the frame's first TS packet in the segment |
//! | `size` | u32 | bytes of TS packets of the frame (incl. PAT/PMT before keyframes) |
//! | `flags` | u32 | bit 0: keyframe |

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::RwLock;

pub const MAGIC: &[u8; 8] = b"VARIDX01";
pub const VERSION: u32 = 1;
pub const HEADER_SIZE: usize = 32;
pub const RECORD_SIZE: usize = 32;
const FLAG_KEYFRAME: u32 = 1;

/// File extension of index files.
pub const EXTENSION: &str = "idx";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexHeader {
    pub fps_num: u32,
    pub fps_den: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexRecord {
    pub pts90: u64,
    pub wall_us: i64,
    pub offset: u64,
    pub size: u32,
    pub keyframe: bool,
}

impl IndexHeader {
    pub fn to_bytes(&self) -> [u8; HEADER_SIZE] {
        let mut b = [0u8; HEADER_SIZE];
        b[0..8].copy_from_slice(MAGIC);
        b[8..12].copy_from_slice(&VERSION.to_le_bytes());
        b[12..16].copy_from_slice(&(RECORD_SIZE as u32).to_le_bytes());
        b[16..20].copy_from_slice(&self.fps_num.to_le_bytes());
        b[20..24].copy_from_slice(&self.fps_den.to_le_bytes());
        b
    }

    pub fn from_bytes(b: &[u8; HEADER_SIZE]) -> io::Result<Self> {
        let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        if &b[0..8] != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a VAR index file",
            ));
        }
        if u32_at(8) != VERSION || u32_at(12) as usize != RECORD_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported index version {}", u32_at(8)),
            ));
        }
        Ok(IndexHeader {
            fps_num: u32_at(16),
            fps_den: u32_at(20),
        })
    }
}

impl IndexRecord {
    pub fn to_bytes(&self) -> [u8; RECORD_SIZE] {
        let mut b = [0u8; RECORD_SIZE];
        b[0..8].copy_from_slice(&self.pts90.to_le_bytes());
        b[8..16].copy_from_slice(&self.wall_us.to_le_bytes());
        b[16..24].copy_from_slice(&self.offset.to_le_bytes());
        b[24..28].copy_from_slice(&self.size.to_le_bytes());
        let flags = if self.keyframe { FLAG_KEYFRAME } else { 0 };
        b[28..32].copy_from_slice(&flags.to_le_bytes());
        b
    }

    pub fn from_bytes(b: &[u8; RECORD_SIZE]) -> Self {
        let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        IndexRecord {
            pts90: u64_at(0),
            wall_us: u64_at(8) as i64,
            offset: u64_at(16),
            size: u32::from_le_bytes(b[24..28].try_into().unwrap()),
            keyframe: u32::from_le_bytes(b[28..32].try_into().unwrap()) & FLAG_KEYFRAME != 0,
        }
    }
}

/// The index of one camera's running recording, kept in memory for review while
/// recording (step 6). The segment writer adds records only once the data they point
/// to has been flushed to disk, so everything listed here can be read from the
/// segment files. It spans camera restarts: a restarted camera continues in the same
/// index with a new segment.
#[derive(Debug, Default)]
pub struct LiveIndex {
    data: RwLock<LiveIndexData>,
}

#[derive(Debug, Default)]
pub struct LiveIndexData {
    /// Frame rate of the camera (numerator, denominator).
    pub fps: (u32, u32),
    pub segments: Vec<IndexedSegment>,
}

#[derive(Debug, Clone)]
pub struct IndexedSegment {
    pub path: PathBuf,
    /// Records of the frames on disk, in recording order.
    pub records: Vec<IndexRecord>,
}

impl LiveIndex {
    pub fn new(fps: (u32, u32)) -> Self {
        LiveIndex {
            data: RwLock::new(LiveIndexData {
                fps,
                segments: Vec::new(),
            }),
        }
    }

    /// A new segment file has been started.
    pub fn start_segment(&self, path: PathBuf) {
        self.data.write().unwrap().segments.push(IndexedSegment {
            path,
            records: Vec::new(),
        });
    }

    /// Frames of the current segment are on disk.
    pub fn publish(&self, records: &[IndexRecord]) {
        if let Some(segment) = self.data.write().unwrap().segments.last_mut() {
            segment.records.extend_from_slice(records);
        }
    }

    /// Reads the index without copying it.
    pub fn read<R>(&self, f: impl FnOnce(&LiveIndexData) -> R) -> R {
        f(&self.data.read().unwrap())
    }
}

/// Writes the header.
pub fn write_header(out: &mut impl Write, header: &IndexHeader) -> io::Result<()> {
    out.write_all(&header.to_bytes())
}

/// Reads a complete index. A partial last record (file still growing, or cut off by a
/// crash) is ignored.
pub fn read(input: &mut impl Read) -> io::Result<(IndexHeader, Vec<IndexRecord>)> {
    let mut header = [0u8; HEADER_SIZE];
    input.read_exact(&mut header)?;
    let header = IndexHeader::from_bytes(&header)?;
    let mut data = Vec::new();
    input.read_to_end(&mut data)?;
    let records = data
        .as_chunks::<RECORD_SIZE>()
        .0
        .iter()
        .map(IndexRecord::from_bytes)
        .collect();
    Ok((header, records))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_partial_record() {
        let header = IndexHeader {
            fps_num: 60,
            fps_den: 1,
        };
        let records = [
            IndexRecord {
                pts90: 90_000,
                wall_us: 1_760_000_000_000_000,
                offset: 0,
                size: 9024,
                keyframe: true,
            },
            IndexRecord {
                pts90: 91_500,
                wall_us: 1_760_000_000_016_667,
                offset: 9024,
                size: 1880,
                keyframe: false,
            },
        ];
        let mut bytes = Vec::new();
        write_header(&mut bytes, &header).unwrap();
        for r in &records {
            bytes.extend_from_slice(&r.to_bytes());
        }
        bytes.extend_from_slice(&[1, 2, 3]); // partial record

        let (h, r) = read(&mut bytes.as_slice()).unwrap();
        assert_eq!(h, header);
        assert_eq!(r, records);
    }

    #[test]
    fn rejects_other_files() {
        let bytes = [0u8; 64];
        assert!(read(&mut bytes.as_slice()).is_err());
    }
}
