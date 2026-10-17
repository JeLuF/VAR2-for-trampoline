//! Review while recording: two synthetic cameras record into short segments while a
//! review player seeks and plays in the growing recording.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use var_capture::{Camera, PixelFormat, synthetic};
use var_core::index::LiveIndex;
use var_playback::{CameraSource, CameraState, ReviewPlayer};
use var_record::{Anchor, EncodingSettings, RecordingBranch, SegmentConfig, select_encoder};

fn now_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as i64
}

/// Waits until `check` holds, at most `timeout`.
fn wait_for(timeout: Duration, mut check: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn review_while_recording() {
    var_capture::init().unwrap();
    let Some(encoder) = select_encoder(false) else {
        eprintln!("skipped: no H.264 encoder");
        return;
    };
    let dir = std::env::temp_dir()
        .join("var-playback-tests")
        .join("review");
    let _ = std::fs::remove_dir_all(&dir);

    // Record two cameras at 720p50 into 2-second segments.
    let anchor = Anchor::now();
    let mut recordings = Vec::new();
    for device in synthetic::devices(2) {
        let mode = device
            .modes
            .iter()
            .find(|m| {
                m.format == PixelFormat::Mjpeg
                    && m.width == 1280
                    && m.capture_framerate() == (50, 1)
            })
            .unwrap()
            .clone();
        let camera = Camera::start(&device, &mode, || {}).unwrap();
        let index = Arc::new(LiveIndex::new((50, 1)));
        let branch = RecordingBranch::new(
            &encoder,
            EncodingSettings::for_mode(1280, 720, 50, 1),
            SegmentConfig {
                dir: dir.join(&device.name),
                segment_length: Duration::from_secs(2),
                anchor,
                fps: (50, 1),
                live_index: Some(Arc::clone(&index)),
            },
        )
        .unwrap();
        let attached = camera.attach(branch.bin()).unwrap();
        recordings.push((camera, branch, attached, index));
    }
    std::thread::sleep(Duration::from_secs(6));

    let sources: Vec<_> = recordings
        .iter()
        .enumerate()
        .map(|(slot, (_, _, _, index))| (slot, Arc::clone(index)))
        .collect();
    let mut player = ReviewPlayer::new(sources.clone(), 0, || {});

    // REV-002: the newest reviewable frame is at most 1 s old, sampled over 3 s.
    let mut max_delay_ms = 0;
    let sampling = Instant::now();
    while sampling.elapsed() < Duration::from_secs(3) {
        let (_, end) = player.range().unwrap();
        max_delay_ms = max_delay_ms.max((now_us() - end) / 1000);
        std::thread::sleep(Duration::from_millis(25));
    }
    let (start, end) = player.range().unwrap();
    println!(
        "recording range {:.1} s, review delay max {max_delay_ms} ms",
        (end - start) as f64 / 1e6
    );
    assert!(max_delay_ms <= 1000, "review delay {max_delay_ms} ms");

    // Seeks, including into the first segment and across segment boundaries: every
    // camera shows exactly the latest frame captured at or before the position.
    for offset_ms in [500, 2_990, 3_010, 1_200, 4_500] {
        let position = start + offset_ms * 1000;
        player.seek(position);
        for (slot, index) in &sources {
            let source = CameraSource::new(Arc::clone(index));
            let expected = source
                .frame_at(position)
                .and_then(|f| source.record(f))
                .unwrap()
                .wall_us;
            let camera = player.camera(*slot).unwrap();
            let shown = wait_for(Duration::from_secs(3), || {
                camera.frame().is_some_and(|f| f.wall_us == expected)
            });
            assert!(
                shown,
                "camera {slot}, seek +{offset_ms} ms: expected frame at {expected}, showing {:?}, state {:?}",
                camera.frame().map(|f| f.wall_us),
                camera.state()
            );
        }
        // Both cameras within one frame (20 ms) of each other (CAP-009).
        let walls: Vec<i64> = sources
            .iter()
            .map(|(slot, _)| player.camera(*slot).unwrap().frame().unwrap().wall_us)
            .collect();
        assert!(
            (walls[0] - walls[1]).abs() <= 20_000,
            "cameras apart: {walls:?}"
        );
    }
    println!("decoder: {:?}", player.camera(0).unwrap().decoder_name());

    // Play at 1× for 2 s: the shown frames follow the position. (Playback waits until
    // the cameras show the frames at the position.)
    player.seek(start + 1_000_000);
    assert!(wait_for(Duration::from_secs(3), || sources.iter().all(
        |(slot, _)| {
            player
                .camera(*slot)
                .unwrap()
                .frame()
                .is_some_and(|f| (start + 1_000_000 - f.wall_us).abs() < 40_000)
        }
    )));
    player.play();
    let play_start = Instant::now();
    while play_start.elapsed() < Duration::from_secs(2) {
        player.tick();
        std::thread::sleep(Duration::from_millis(16));
    }
    player.tick();
    let advanced = (player.position() - (start + 1_000_000)) as f64 / 1e6;
    assert!((advanced - 2.0).abs() < 0.1, "advanced {advanced} s");
    for (slot, _) in &sources {
        let camera = player.camera(*slot).unwrap();
        let lag_ms = camera
            .frame()
            .map(|f| (player.position() - f.wall_us) / 1000)
            .unwrap();
        println!(
            "camera {slot} while playing: frame {lag_ms} ms behind the position, {:?}",
            camera.state()
        );
        assert!(lag_ms < 200, "camera {slot} lags {lag_ms} ms");
    }

    // REC-010: the operator moves a completed segment of camera 0 away during review.
    // Review shows "no video" there for camera 0, camera 1 is unaffected.
    let moved = sources[0].1.read(|data| data.segments[1].path.clone());
    let moved_to = dir.join("moved.ts");
    std::fs::rename(&moved, &moved_to).unwrap();
    let inside = sources[0]
        .1
        .read(|data| data.segments[1].records[40].wall_us);
    player.pause();
    player.seek(inside);
    assert!(
        wait_for(Duration::from_secs(3), || {
            player.camera(0).unwrap().state() == CameraState::NoVideo
                && player.camera(1).unwrap().state() == CameraState::Showing
        }),
        "after moving a segment: camera 0 {:?}, camera 1 {:?}",
        player.camera(0).unwrap().state(),
        player.camera(1).unwrap().state()
    );
    // After the moved segment, camera 0 shows video again.
    let after = sources[0]
        .1
        .read(|data| data.segments[2].records[10].wall_us);
    player.seek(after);
    assert!(wait_for(Duration::from_secs(3), || {
        player
            .camera(0)
            .unwrap()
            .frame()
            .is_some_and(|f| f.wall_us <= after)
            && player.camera(0).unwrap().state() == CameraState::Showing
    }));

    // Seeking to the live edge follows the growing recording.
    player.seek(i64::MAX);
    std::thread::sleep(Duration::from_secs(1));
    player.seek(i64::MAX);
    let (_, end) = player.range().unwrap();
    assert!(player.position() == end);
    assert!(wait_for(Duration::from_secs(3), || {
        player.camera(0).unwrap().state() == CameraState::Showing
    }));
    drop(player);

    for (camera, branch, attached, _) in recordings {
        camera.detach(&attached);
        assert!(branch.wait_finished(Duration::from_secs(5)));
        camera.remove(attached);
        assert_eq!(branch.error(), None);
    }
}
