//! Analysis of VAR recordings.
//!
//! - Single MPEG-TS files: frame count vs. duration, dropped frames, keyframe interval,
//!   bitrate, tracks. Works on the encoded stream (no decoding).
//! - Camera folders (segments with index): every segment, the index checked against
//!   the file content, and continuity across segment boundaries from the index
//!   timestamps.
//! - Event folders: all camera folders, and with [`sync`] the offset between cameras
//!   from flashes visible to all cameras (decodes the video).
//!
//! Used by the `var-check` tool and by integration tests.

pub mod resources;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, bail};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use var_core::index::{self, IndexHeader, IndexRecord};
use var_core::timing::{DropDetector, Gap};

/// One encoded frame of a file.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    /// Timestamp from the demuxer, ns (starts near 0 in every file).
    pub pts_ns: u64,
    pub keyframe: bool,
    pub size: usize,
}

/// The encoded content of one MPEG-TS file.
#[derive(Debug, Clone, Default)]
pub struct FileContent {
    pub file_size: u64,
    /// Caps of all elementary streams.
    pub tracks: Vec<String>,
    /// Frame rate declared in the stream, if any.
    pub declared_fps: Option<f64>,
    /// Frames in decoding order.
    pub frames: Vec<Frame>,
}

/// Result of analysing a file or a series of segments.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub file_size: u64,
    pub tracks: Vec<String>,
    pub video_tracks: usize,
    pub audio_tracks: usize,
    pub declared_fps: Option<f64>,
    pub frames: u64,
    pub keyframes: u64,
    /// Time from the first frame to the end of the last frame, in seconds.
    pub duration: f64,
    /// Frame rate from frame count and duration.
    pub measured_fps: f64,
    /// Gaps with missing frames: (position in seconds, missing frames).
    pub gaps: Vec<(f64, u64)>,
    pub dropped_frames: u64,
    /// Late frames without a lost frame (timing jitter of the camera).
    pub late_frames: u64,
    /// Longest and average keyframe interval, in frames.
    pub max_gop: u64,
    pub average_gop: f64,
    /// Average bitrate of the video stream and of the whole file, in bit/s.
    pub video_bitrate: f64,
    pub file_bitrate: f64,
}

/// Analyses a single MPEG-TS file from its content.
pub fn analyze(path: &Path) -> anyhow::Result<Report> {
    let content = read_file(path)?;
    let frames = content.frames.clone();
    Ok(evaluate(
        content.file_size,
        content.tracks,
        content.declared_fps,
        frames,
    ))
}

/// Reads the encoded frames of an MPEG-TS file.
pub fn read_file(path: &Path) -> anyhow::Result<FileContent> {
    gst::init()?;
    let file_size = std::fs::metadata(path)
        .with_context(|| format!("cannot read {}", path.display()))?
        .len();

    let pipeline = gst::Pipeline::new();
    let source = gst::ElementFactory::make("filesrc")
        .property("location", path.to_string_lossy().as_ref())
        .build()?;
    let demux = gst::ElementFactory::make("tsdemux").build()?;
    pipeline.add_many([&source, &demux])?;
    source.link(&demux)?;

    let tracks = Arc::new(Mutex::new(Vec::<String>::new()));
    let video_sink: Arc<Mutex<Option<gst_app::AppSink>>> = Arc::new(Mutex::new(None));

    demux.connect_pad_added({
        let pipeline = pipeline.downgrade();
        let tracks = Arc::clone(&tracks);
        let video_sink = Arc::clone(&video_sink);
        move |_, pad| {
            let Some(pipeline) = pipeline.upgrade() else {
                return;
            };
            let caps = pad
                .current_caps()
                .map(|c| c.to_string())
                .unwrap_or_default();
            tracks.lock().unwrap().push(caps.clone());

            let mut video_sink = video_sink.lock().unwrap();
            let elements: Vec<gst::Element> =
                if caps.starts_with("video/x-h264") && video_sink.is_none() {
                    let parse = gst::ElementFactory::make("h264parse").build().unwrap();
                    let sink = gst_app::AppSink::builder()
                        .caps(
                            &gst::Caps::builder("video/x-h264")
                                .field("stream-format", "byte-stream")
                                .field("alignment", "au")
                                .build(),
                        )
                        .sync(false)
                        .max_buffers(64)
                        .build();
                    *video_sink = Some(sink.clone());
                    vec![parse, sink.upcast()]
                } else {
                    vec![
                        gst::ElementFactory::make("fakesink")
                            .property("sync", false)
                            .build()
                            .unwrap(),
                    ]
                };
            pipeline.add_many(&elements).unwrap();
            gst::Element::link_many(&elements).unwrap();
            for element in &elements {
                element.sync_state_with_parent().unwrap();
            }
            pad.link(&elements[0].static_pad("sink").unwrap()).unwrap();
        }
    });

    pipeline.set_state(gst::State::Playing)?;
    let bus = pipeline.bus().unwrap();

    let mut frames = Vec::new();
    let mut declared_fps = None;
    loop {
        let sink = video_sink.lock().unwrap().clone();
        if let Some(sink) = &sink {
            while let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(10)) {
                if declared_fps.is_none() {
                    declared_fps = sample
                        .caps()
                        .and_then(|c| c.structure(0)?.get::<gst::Fraction>("framerate").ok())
                        .filter(|f| f.numer() > 0 && f.denom() > 0)
                        .map(|f| f64::from(f.numer()) / f64::from(f.denom()));
                }
                let Some(buffer) = sample.buffer() else {
                    continue;
                };
                let Some(pts) = buffer.pts().or(buffer.dts()) else {
                    continue;
                };
                frames.push(Frame {
                    pts_ns: pts.nseconds(),
                    keyframe: !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT),
                    size: buffer.size(),
                });
            }
        }
        match bus.timed_pop_filtered(
            gst::ClockTime::from_mseconds(if sink.is_some() { 0 } else { 10 }),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        ) {
            Some(message) => match message.view() {
                gst::MessageView::Eos(_) => break,
                gst::MessageView::Error(err) => {
                    let _ = pipeline.set_state(gst::State::Null);
                    bail!("cannot read {}: {}", path.display(), err.error());
                }
                _ => {}
            },
            None => continue,
        }
    }
    let _ = pipeline.set_state(gst::State::Null);

    let tracks = tracks.lock().unwrap().clone();
    Ok(FileContent {
        file_size,
        tracks,
        declared_fps,
        frames,
    })
}

fn evaluate(
    file_size: u64,
    tracks: Vec<String>,
    declared_fps: Option<f64>,
    mut frames: Vec<Frame>,
) -> Report {
    let mut report = Report {
        file_size,
        video_tracks: tracks.iter().filter(|t| t.starts_with("video/")).count(),
        audio_tracks: tracks.iter().filter(|t| t.starts_with("audio/")).count(),
        tracks,
        declared_fps,
        frames: frames.len() as u64,
        ..Default::default()
    };
    if frames.len() < 2 {
        return report;
    }
    frames.sort_by_key(|f| f.pts_ns);

    // Nominal frame duration: declared rate, else the median timestamp difference.
    let frame_ns = match declared_fps {
        Some(fps) => 1e9 / fps,
        None => {
            let mut deltas: Vec<u64> = frames
                .windows(2)
                .map(|w| w[1].pts_ns - w[0].pts_ns)
                .collect();
            deltas.sort_unstable();
            deltas[deltas.len() / 2] as f64
        }
    };

    // Same detection as the capture statistics: late frames after which the stream
    // catches up are jitter, not drops.
    let first = frames[0].pts_ns;
    let mut detector = DropDetector::new(frame_ns);
    let mut gaps: Vec<Gap> = frames
        .iter()
        .filter_map(|f| detector.push(f.pts_ns))
        .collect();
    gaps.extend(detector.finish());
    for gap in gaps {
        if gap.missing > 0 {
            report
                .gaps
                .push(((gap.after_ns - first) as f64 / 1e9, gap.missing));
            report.dropped_frames += gap.missing;
        } else {
            report.late_frames += 1;
        }
    }

    let last = frames[frames.len() - 1].pts_ns;
    report.duration = ((last - first) as f64 + frame_ns) / 1e9;
    report.measured_fps = report.frames as f64 / report.duration;

    let keyframe_positions: Vec<usize> = frames
        .iter()
        .enumerate()
        .filter(|(_, f)| f.keyframe)
        .map(|(i, _)| i)
        .collect();
    report.keyframes = keyframe_positions.len() as u64;
    let gops: Vec<u64> = keyframe_positions
        .windows(2)
        .map(|w| (w[1] - w[0]) as u64)
        .collect();
    report.max_gop = gops.iter().copied().max().unwrap_or(0);
    report.average_gop = if gops.is_empty() {
        0.0
    } else {
        gops.iter().sum::<u64>() as f64 / gops.len() as f64
    };

    let video_bytes: u64 = frames.iter().map(|f| f.size as u64).sum();
    report.video_bitrate = video_bytes as f64 * 8.0 / report.duration;
    report.file_bitrate = file_size as f64 * 8.0 / report.duration;
    report
}

/// Index file of a segment.
pub fn index_path(segment: &Path) -> PathBuf {
    segment.with_extension(index::EXTENSION)
}

/// Reads a segment's index.
pub fn read_index(segment: &Path) -> anyhow::Result<(IndexHeader, Vec<IndexRecord>)> {
    let path = index_path(segment);
    let mut file = File::open(&path).with_context(|| format!("cannot open {}", path.display()))?;
    Ok(index::read(&mut file)?)
}

/// Compares a segment's index with its content. Returns the problems found.
pub fn check_index(segment: &Path, content: &FileContent, records: &[IndexRecord]) -> Vec<String> {
    let mut problems = Vec::new();
    // Fewer records than frames: unindexed frames at the end after a crash (normal).
    if records.len() > content.frames.len() {
        problems.push(format!(
            "index has {} records, file has only {} frames",
            records.len(),
            content.frames.len()
        ));
    }
    let data = match std::fs::read(segment) {
        Ok(data) => data,
        Err(e) => return vec![format!("cannot read segment: {e}")],
    };
    let first_pts = records.first().map_or(0, |r| r.pts90);
    let first_frame = content.frames.first().map_or(0, |f| f.pts_ns);
    let mut expected_offset = 0;
    for (i, (record, frame)) in records.iter().zip(&content.frames).enumerate() {
        if record.offset != expected_offset {
            problems.push(format!(
                "record {i}: offset {} instead of {expected_offset}",
                record.offset
            ));
        }
        expected_offset = record.offset + u64::from(record.size);
        if data.get(record.offset as usize) != Some(&0x47) {
            problems.push(format!(
                "record {i}: offset {} is not a TS packet",
                record.offset
            ));
        }
        if record.keyframe != frame.keyframe {
            problems.push(format!("record {i}: keyframe flag differs from the stream"));
        }
        let index_ns = (record.pts90 - first_pts) as i64 * 100_000 / 9;
        let file_ns = (frame.pts_ns - first_frame) as i64;
        if (index_ns - file_ns).abs() > 100_000 {
            problems.push(format!(
                "record {i}: timestamp differs from the stream by {} µs",
                (index_ns - file_ns) / 1000
            ));
        }
        if problems.len() > 10 {
            problems.push("…".into());
            break;
        }
    }
    // Frames at the end that are not indexed are normal after a crash (the index is
    // written after the data); an index pointing past the data is not.
    if expected_offset > data.len() as u64 && problems.len() <= 10 {
        problems.push(format!(
            "index covers {expected_offset} bytes, segment has only {}",
            data.len()
        ));
    }
    problems
}

/// One segment of a camera folder.
#[derive(Debug, Clone)]
pub struct SegmentReport {
    pub path: PathBuf,
    pub report: Report,
    /// Problems of the index; `None` if there is no index.
    pub index_problems: Option<Vec<String>>,
}

/// The transition from one segment to the next.
#[derive(Debug, Clone)]
pub struct Boundary {
    /// File name of the segment before the boundary.
    pub after: String,
    /// Time from the last frame of one segment to the first of the next, in frames
    /// (1.0 = continuous).
    pub interval_frames: f64,
    /// Segments missing between the two (moved or deleted, REC-010).
    pub segments_missing: u32,
}

impl Boundary {
    /// Continuous, or not checkable because segments in between are missing.
    pub fn is_continuous(&self) -> bool {
        self.segments_missing > 0 || (self.interval_frames > 0.5 && self.interval_frames < 1.5)
    }
}

/// Analysis of a camera folder.
#[derive(Debug, Clone)]
pub struct CameraReport {
    pub dir: PathBuf,
    pub segments: Vec<SegmentReport>,
    /// The whole recording, from the index timestamps of all segments.
    pub combined: Report,
    pub boundaries: Vec<Boundary>,
    /// Gaps in the segment numbering (moved or deleted segments).
    pub missing_segments: Vec<u32>,
}

/// Segment files of a camera folder in recording order.
pub fn segment_files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("cannot read {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "ts"))
        .collect();
    files.sort();
    Ok(files)
}

fn segment_number(path: &Path) -> Option<u32> {
    path.file_name()?.to_str()?.split('_').next()?.parse().ok()
}

/// Analyses all segments of a camera folder.
pub fn analyze_camera(dir: &Path) -> anyhow::Result<CameraReport> {
    let files = segment_files(dir)?;
    if files.is_empty() {
        bail!("no segments in {}", dir.display());
    }

    let mut segments = Vec::new();
    let mut all_records: Vec<IndexRecord> = Vec::new();
    let mut boundaries = Vec::new();
    let mut tracks = Vec::new();
    let mut file_size = 0;
    let mut fps = None;
    let mut missing_segments = Vec::new();
    // Segments are numbered from 1, so missing first segments are found too.
    let mut previous_number = Some(0);
    // Time removed from the combined timeline where segments are missing, so the
    // missing segments don't count as dropped frames.
    let mut removed_pts90 = 0;

    for path in &files {
        let content = read_file(path)?;
        let index = read_index(path).ok();
        let index_problems = index
            .as_ref()
            .map(|(_, records)| check_index(path, &content, records));

        let number = segment_number(path);
        let segments_missing = match (number, previous_number) {
            (Some(number), Some(previous)) if number > previous + 1 => {
                missing_segments.extend(previous + 1..number);
                number - previous - 1
            }
            _ => 0,
        };
        previous_number = number;

        if let Some((header, mut records)) = index {
            if header.fps_den > 0 {
                fps = Some(f64::from(header.fps_num) / f64::from(header.fps_den));
            }
            if let (Some(last), Some(first), Some(fps)) = (all_records.last(), records.first(), fps)
            {
                let first_pts = first.pts90 - removed_pts90;
                let interval_s = (first_pts as f64 - last.pts90 as f64) / 90_000.0;
                boundaries.push(Boundary {
                    after: segments
                        .last()
                        .map(|s: &SegmentReport| {
                            s.path.file_name().unwrap().to_string_lossy().into_owned()
                        })
                        .unwrap_or_default(),
                    interval_frames: interval_s * fps,
                    segments_missing,
                });
                if segments_missing > 0 {
                    // Join the timeline as if the next segment followed directly.
                    removed_pts90 += first_pts - last.pts90 - (90_000.0 / fps).round() as u64;
                }
            }
            for record in &mut records {
                record.pts90 -= removed_pts90;
            }
            all_records.extend(records);
        }

        file_size += content.file_size;
        if tracks.is_empty() {
            tracks = content.tracks.clone();
        }
        let report = evaluate(
            content.file_size,
            content.tracks.clone(),
            content.declared_fps,
            content.frames.clone(),
        );
        segments.push(SegmentReport {
            path: path.clone(),
            report,
            index_problems,
        });
    }

    let frames: Vec<Frame> = all_records
        .iter()
        .map(|r| Frame {
            pts_ns: r.pts90 * 100_000 / 9,
            keyframe: r.keyframe,
            size: r.size as usize,
        })
        .collect();
    let combined = evaluate(file_size, tracks, fps, frames);

    Ok(CameraReport {
        dir: dir.to_owned(),
        segments,
        combined,
        boundaries,
        missing_segments,
    })
}

/// Camera folders of an event folder (subfolders containing segments).
pub fn camera_dirs(event_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(event_dir)
        .with_context(|| format!("cannot read {}", event_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir() && segment_files(p).is_ok_and(|f| !f.is_empty()))
        .collect();
    dirs.sort();
    Ok(dirs)
}

/// A flash: a frame much brighter than the frame before.
#[derive(Debug, Clone, Copy)]
pub struct Flash {
    /// Wall-clock capture time from the index, µs since the Unix epoch.
    pub wall_us: i64,
    /// Increase of the mean luma (0–255).
    pub jump: f64,
}

/// Minimum increase of the mean luma that counts as a flash.
const FLASH_THRESHOLD: f64 = 40.0;

/// Finds flashes in a camera folder by decoding all segments. Returns the frame rate
/// and the flashes.
pub fn find_flashes(dir: &Path) -> anyhow::Result<(f64, Vec<Flash>)> {
    let mut flashes = Vec::new();
    let mut fps = 0.0;
    let mut previous_luma: Option<f64> = None;
    for path in segment_files(dir)? {
        let (header, records) = read_index(&path)?;
        fps = f64::from(header.fps_num) / f64::from(header.fps_den.max(1));
        let lumas = decode_luma(&path)?;
        if lumas.len() != records.len() {
            eprintln!(
                "warning: {}: {} decoded frames, {} index records",
                path.display(),
                lumas.len(),
                records.len()
            );
        }
        for (luma, record) in lumas.iter().zip(&records) {
            if let Some(previous) = previous_luma
                && luma - previous > FLASH_THRESHOLD
            {
                flashes.push(Flash {
                    wall_us: record.wall_us,
                    jump: luma - previous,
                });
            }
            previous_luma = Some(*luma);
        }
    }
    Ok((fps, flashes))
}

/// Mean luma of every frame of a segment, in output order.
pub fn decode_luma(path: &Path) -> anyhow::Result<Vec<f64>> {
    gst::init()?;
    let pipeline = gst::parse::launch(
        "filesrc name=src ! decodebin ! videoscale method=nearest-neighbour \
         ! videoconvert ! video/x-raw,format=GRAY8,width=64,height=36 \
         ! appsink name=sink sync=false max-buffers=64",
    )?
    .downcast::<gst::Pipeline>()
    .unwrap();
    pipeline
        .by_name("src")
        .unwrap()
        .set_property("location", path.to_string_lossy().as_ref());
    let sink = pipeline
        .by_name("sink")
        .unwrap()
        .downcast::<gst_app::AppSink>()
        .unwrap();

    pipeline.set_state(gst::State::Playing)?;
    let mut lumas = Vec::new();
    while let Ok(sample) = sink.pull_sample() {
        if let Some(buffer) = sample.buffer()
            && let Ok(map) = buffer.map_readable()
        {
            let sum: u64 = map.iter().map(|&b| u64::from(b)).sum();
            lumas.push(sum as f64 / map.len().max(1) as f64);
        }
    }
    let error = pipeline
        .bus()
        .unwrap()
        .pop_filtered(&[gst::MessageType::Error]);
    let _ = pipeline.set_state(gst::State::Null);
    if let Some(message) = error
        && let gst::MessageView::Error(err) = message.view()
    {
        bail!("cannot decode {}: {}", path.display(), err.error());
    }
    Ok(lumas)
}

/// Offset of one camera's flash against the reference camera's.
#[derive(Debug, Clone)]
pub struct FlashOffset {
    pub camera: PathBuf,
    /// Wall-clock time of the flash in the reference camera, µs since the Unix epoch.
    pub reference_wall_us: i64,
    /// Offset in µs (positive: later than the reference) and in frames.
    pub offset_us: i64,
    pub offset_frames: f64,
}

/// Synchronisation analysis (CAP-009): every flash of the first camera matched with
/// the nearest flash of each other camera within half a second.
pub fn sync(camera_dirs: &[PathBuf]) -> anyhow::Result<Vec<FlashOffset>> {
    let Some((reference_dir, others)) = camera_dirs.split_first() else {
        return Ok(Vec::new());
    };
    let (fps, reference) = find_flashes(reference_dir)?;
    let frame_us = 1e6 / fps.max(1.0);
    let mut offsets = Vec::new();
    for dir in others {
        let (_, flashes) = find_flashes(dir)?;
        for flash in &reference {
            let nearest = flashes
                .iter()
                .min_by_key(|f| (f.wall_us - flash.wall_us).abs())
                .filter(|f| (f.wall_us - flash.wall_us).abs() < 500_000);
            if let Some(other) = nearest {
                let offset_us = other.wall_us - flash.wall_us;
                offsets.push(FlashOffset {
                    camera: dir.clone(),
                    reference_wall_us: flash.wall_us,
                    offset_us,
                    offset_frames: offset_us as f64 / frame_us,
                });
            }
        }
    }
    Ok(offsets)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames_at(times_ms: &[u64], gop: usize) -> Vec<Frame> {
        times_ms
            .iter()
            .enumerate()
            .map(|(i, ms)| Frame {
                pts_ns: ms * 1_000_000,
                keyframe: i % gop == 0,
                size: 1000,
            })
            .collect()
    }

    #[test]
    fn detects_gaps_gop_and_bitrate() {
        // 50 fps, frames 3 and 4 missing.
        let times: Vec<u64> = (0..50u64)
            .filter(|i| *i != 3 && *i != 4)
            .map(|i| i * 20)
            .collect();
        let report = evaluate(
            100_000,
            vec!["video/x-h264".into()],
            Some(50.0),
            frames_at(&times, 25),
        );
        assert_eq!(report.frames, 48);
        assert_eq!(report.dropped_frames, 2);
        assert_eq!(report.gaps.len(), 1);
        assert!((report.duration - 1.0).abs() < 1e-9);
        assert_eq!(report.max_gop, 25);
        assert_eq!(report.video_bitrate, 48.0 * 1000.0 * 8.0);
        assert_eq!(report.audio_tracks, 0);
    }

    #[test]
    fn counts_audio_tracks() {
        let report = evaluate(
            0,
            vec!["video/x-h264".into(), "audio/mpeg, mpegversion=4".into()],
            None,
            Vec::new(),
        );
        assert_eq!((report.video_tracks, report.audio_tracks), (1, 1));
    }

    #[test]
    fn segment_numbers() {
        assert_eq!(segment_number(Path::new("x/0012_21-30-00.ts")), Some(12));
        assert_eq!(segment_number(Path::new("x/recording.ts")), None);
    }
}
