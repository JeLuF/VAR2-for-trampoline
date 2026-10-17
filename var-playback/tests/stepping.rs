//! Step 7: frame stepping, playback speeds and jumps, while recording.

mod common;

use std::time::{Duration, Instant};

use var_playback::{CameraSource, ReviewPlayer, SPEEDS};

use common::wait_for;

/// The frame each camera shows, as its position in the camera's index.
fn shown_frames(player: &ReviewPlayer, slots: &[usize]) -> Vec<Option<i64>> {
    slots
        .iter()
        .map(|&slot| player.camera(slot).unwrap().frame().map(|f| f.wall_us))
        .collect()
}

#[test]
fn stepping_speeds_and_jumps() {
    let dir = std::env::temp_dir()
        .join("var-playback-tests")
        .join("stepping");
    let Some(recording) = common::record(2, &dir) else {
        return;
    };
    std::thread::sleep(Duration::from_secs(7));
    let slots: Vec<usize> = recording.sources.iter().map(|(s, _)| *s).collect();
    let sources: Vec<CameraSource> = recording
        .sources
        .iter()
        .map(|(_, index)| CameraSource::new(std::sync::Arc::clone(index)))
        .collect();
    let mut player = ReviewPlayer::new(recording.sources.clone(), 0, || {});

    // REV-004: start 6 frames before the start of camera 0's third segment, so the
    // steps cross a segment boundary and keyframes.
    let second = sources[0]
        .first()
        .map(|f| sources[0].next_segment(f).unwrap())
        .unwrap();
    let boundary = sources[0].next_segment(second).unwrap();
    let mut start = boundary;
    for _ in 0..6 {
        start = sources[0].prev(start).unwrap();
    }
    let position = sources[0].record(start).unwrap().wall_us;
    player.seek(position);
    // While decoding from the keyframe, earlier frames are shown briefly; wait for the
    // exact frame at the position.
    let at_position: Vec<Option<i64>> = sources
        .iter()
        .map(|s| {
            s.frame_at(position)
                .and_then(|f| s.record(f))
                .map(|r| r.wall_us)
        })
        .collect();
    assert!(wait_for(Duration::from_secs(3), || {
        shown_frames(&player, &slots) == at_position
    }));

    let mut back_latency_max = Duration::ZERO;
    let mut back_latencies = Vec::new();
    // 12 steps forward, then 100 back: beyond the frame cache (64 frames), so stepping
    // back needs earlier GOPs decoded again. Steps at about 30 per second, the key
    // repeat rate when a referee holds the arrow key.
    let pace = Duration::from_millis(33);
    for (direction, steps) in [(1, 12), (-1, 100)] {
        for step in 0..steps {
            let before = shown_frames(&player, &slots);
            // Expected for each camera: exactly the neighbouring frame.
            let expected: Vec<i64> = before
                .iter()
                .zip(&sources)
                .map(|(wall, source)| {
                    let current = source.frame_at(wall.unwrap()).unwrap();
                    let neighbour = if direction > 0 {
                        source.next(current).unwrap()
                    } else {
                        source.prev(current).unwrap()
                    };
                    source.record(neighbour).unwrap().wall_us
                })
                .collect();
            let requested = Instant::now();
            player.step(direction, Some(0));
            let done = wait_for(Duration::from_secs(3), || {
                shown_frames(&player, &slots)
                    .iter()
                    .zip(&expected)
                    .all(|(shown, expected)| *shown == Some(*expected))
            });
            assert!(
                done,
                "step {step} in direction {direction}: expected {expected:?}, showing {:?}",
                shown_frames(&player, &slots)
            );
            let latency = requested.elapsed();
            if direction < 0 {
                back_latency_max = back_latency_max.max(latency);
                back_latencies.push(latency.as_millis());
            }
            std::thread::sleep(pace.saturating_sub(latency));
        }
    }
    back_latencies.sort_unstable();
    println!(
        "backward step: max {} ms, median {} ms until shown",
        back_latency_max.as_millis(),
        back_latencies[back_latencies.len() / 2]
    );
    assert!(
        back_latency_max <= Duration::from_millis(100),
        "backward step too slow"
    );

    // REV-003: playback speeds, measured from the frames shown on camera 0.
    let (range_start, _) = player.range().unwrap();
    for speed in SPEEDS {
        player.pause();
        player.seek(range_start + 500_000);
        assert!(wait_for(Duration::from_secs(3), || player
            .camera(0)
            .unwrap()
            .frame()
            .is_some()));
        player.set_speed(speed);
        player.play();
        // At least 1 s of recording time, and 2 s of real time.
        let duration = Duration::from_secs_f64((1.0 / speed).max(2.0));
        let mut changes: Vec<(Instant, i64)> = Vec::new();
        let begin = Instant::now();
        while begin.elapsed() < duration {
            player.tick();
            let wall = player.camera(0).unwrap().frame().map(|f| f.wall_us);
            if let Some(wall) = wall
                && changes.last().is_none_or(|(_, last)| *last != wall)
            {
                changes.push((Instant::now(), wall));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        // From the first change after starting to the last change.
        let (t0, w0) = changes[1];
        let (t1, w1) = *changes.last().unwrap();
        let measured = (w1 - w0) as f64 / 1e6 / t1.duration_since(t0).as_secs_f64();
        println!("speed {speed}×: measured {measured:.3}×");
        assert!(
            (measured / speed - 1.0).abs() <= 0.05,
            "speed {speed}: {measured}"
        );
    }
    player.pause();
    player.set_speed(1.0);

    // REV-006: jumps.
    let middle = range_start + 3_000_000;
    player.seek(middle);
    player.jump(1);
    assert_eq!(player.position(), middle + 1_000_000);
    player.jump(-1);
    assert_eq!(player.position(), middle);
    player.jump(-10);
    assert_eq!(player.position(), range_start, "clamped to the start");
    player.jump(10);
    assert_eq!(player.position(), range_start + 10_000_000);
    player.jump(60);
    assert_eq!(
        player.position(),
        player.range().unwrap().1,
        "clamped to the live edge"
    );

    drop(player);
    recording.stop();
}
