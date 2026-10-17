//! Records synthetic cameras into segments and checks them with `var-check`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use std::time::Duration;
use var_core::index::LiveIndex;

use var_capture::{Camera, PixelFormat, synthetic};
use var_record::{Anchor, EncodingSettings, RecordingBranch, SegmentConfig, select_encoder};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("var-record-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Records all cameras at 720p `fps` YUY2 for `seconds` with one anchor, into
/// `event_dir/camera N`, while the live view keeps running.
fn record(force_software: bool, cameras: u32, fps: i32, seconds: u64, event_dir: &Path) -> bool {
    var_capture::init().unwrap();
    let Some(encoder) = select_encoder(force_software) else {
        eprintln!("skipped: no H.264 encoder available");
        return false;
    };

    let running: Vec<_> = synthetic::devices(cameras)
        .into_iter()
        .map(|device| {
            let mode = device
                .modes
                .iter()
                .find(|m| {
                    m.format == PixelFormat::Raw("YUY2".into())
                        && m.width == 1280
                        && m.capture_framerate() == (fps, 1)
                })
                .unwrap()
                .clone();
            let camera = Camera::start(&device, &mode, || {}).unwrap();
            (device, camera)
        })
        .collect();
    std::thread::sleep(Duration::from_secs(1));

    let anchor = Anchor::now();
    let settings = EncodingSettings::for_mode(1280, 720, fps, 1);
    let branches: Vec<_> = running
        .iter()
        .enumerate()
        .map(|(i, (_, camera))| {
            let live_index = Arc::new(LiveIndex::new((fps as u32, 1)));
            let branch = RecordingBranch::new(
                &encoder,
                settings,
                SegmentConfig {
                    dir: event_dir.join(format!("camera {}", i + 1)),
                    segment_length: Duration::from_secs(2),
                    anchor,
                    fps: (fps as u32, 1),
                    live_index: Some(Arc::clone(&live_index)),
                },
            )
            .unwrap();
            let attached = camera.attach(branch.bin()).unwrap();
            (branch, attached, live_index)
        })
        .collect();
    std::thread::sleep(Duration::from_secs(seconds));

    for ((_, camera), (_, attached, _)) in running.iter().zip(&branches) {
        camera.detach(attached);
    }
    for ((_, camera), (branch, attached, live_index)) in running.iter().zip(branches) {
        assert!(
            branch.wait_finished(Duration::from_secs(5)),
            "recording did not finish"
        );
        // The live index (for review while recording) equals the index files.
        live_index.read(|data| {
            assert!(!data.segments.is_empty());
            for segment in &data.segments {
                let (_, on_disk) = var_check::read_index(&segment.path).unwrap();
                assert_eq!(segment.records, on_disk, "{}", segment.path.display());
            }
        });
        camera.remove(attached);
        assert_eq!(camera.error(), None);
        assert_eq!(camera.branch_error(), None);
        assert_eq!(branch.error(), None);
        println!("{}: {:?}", branch.dir().display(), branch.stats());
        // Live view continues after recording.
        let frames_before = camera.stats().preview_frames;
        std::thread::sleep(Duration::from_millis(300));
        assert!(camera.stats().preview_frames > frames_before);
    }
    true
}

fn check_camera(dir: &Path, fps: i32, seconds: u64) {
    let camera = var_check::analyze_camera(dir).unwrap();
    let report = &camera.combined;
    println!(
        "{}: {} segments\n{report:#?}",
        dir.display(),
        camera.segments.len()
    );

    assert!(
        camera.segments.len() as u64 >= seconds / 2,
        "segments roll over every 2 s"
    );
    for segment in &camera.segments {
        assert_eq!(
            segment.index_problems.as_deref(),
            Some(&[][..]),
            "{}",
            segment.path.display()
        );
        assert_eq!(segment.report.audio_tracks, 0, "REC-013: no audio");
        assert_eq!(segment.report.video_tracks, 1);
    }
    for boundary in &camera.boundaries {
        assert!(
            boundary.is_continuous(),
            "boundary after {}: {:.2} frames",
            boundary.after,
            boundary.interval_frames
        );
    }
    assert!(
        report.max_gop <= (fps / 2) as u64,
        "keyframe every 0.5 s, got {}",
        report.max_gop
    );
    let expected_frames = (fps as u64 * seconds) as f64;
    assert!(
        (report.frames as f64 - expected_frames).abs() < expected_frames * 0.1,
        "{} frames, expected about {expected_frames}",
        report.frames
    );
}

#[test]
fn two_cameras_with_segments_index_and_sync() {
    let event = temp_dir("two-cameras");
    if !record(false, 2, 50, 11, &event) {
        return;
    }
    let cameras = var_check::camera_dirs(&event).unwrap();
    assert_eq!(cameras.len(), 2);
    for dir in &cameras {
        check_camera(dir, 50, 11);
    }

    // The synthetic cameras flash every 5 s on the shared clock (CAP-009).
    let offsets = var_check::sync(&cameras).unwrap();
    println!("{offsets:#?}");
    assert!(!offsets.is_empty(), "no flash found in both cameras");
    for offset in &offsets {
        assert!(offset.offset_frames.abs() <= 1.0, "{offset:?}");
    }
}

#[test]
fn software_fallback_with_segments() {
    let event = temp_dir("software");
    if record(true, 1, 50, 5, &event) {
        check_camera(&event.join("camera 1"), 50, 5);
    }
}
