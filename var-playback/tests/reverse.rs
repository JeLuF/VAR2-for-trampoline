//! Step 12: reverse slow motion while recording (REV-005).

mod common;

use std::time::{Duration, Instant};

use var_playback::{CameraSource, MAX_REVERSE_SPEED, ReviewPlayer};

use common::wait_for;

#[test]
fn reverse_playback_shows_every_frame_backwards() {
    let dir = std::env::temp_dir()
        .join("var-playback-tests")
        .join("reverse");
    let Some(recording) = common::record(2, &dir) else {
        return;
    };
    std::thread::sleep(Duration::from_secs(7));
    let sources: Vec<CameraSource> = recording
        .sources
        .iter()
        .map(|(_, index)| CameraSource::new(std::sync::Arc::clone(index)))
        .collect();
    let mut player = ReviewPlayer::new(recording.sources.clone(), 0, || {});

    // Start 0.4 s after the start of camera 0's third segment: every run crosses
    // keyframes (every 0.5 s) and a segment boundary.
    let second = sources[0]
        .first()
        .map(|f| sources[0].next_segment(f).unwrap())
        .unwrap();
    let boundary = sources[0].next_segment(second).unwrap();
    let start = sources[0].record(boundary).unwrap().wall_us + 400_000;

    for (speed, seconds) in [(0.5, 3.0), (0.25, 4.0), (0.1, 6.0)] {
        player.pause();
        player.seek(start);
        let at_start: Vec<Option<i64>> = sources
            .iter()
            .map(|s| {
                s.frame_at(start)
                    .and_then(|f| s.record(f))
                    .map(|r| r.wall_us)
            })
            .collect();
        assert!(wait_for(Duration::from_secs(3), || {
            (0..sources.len())
                .map(|slot| player.camera(slot).unwrap().frame().map(|f| f.wall_us))
                .eq(at_start.iter().copied())
        }));

        player.set_speed(speed);
        let waited_before = player.waited();
        player.play_reverse();
        assert!(player.is_reverse());
        // Frames shown per camera, with the time they appeared.
        let mut shown: Vec<Vec<(Instant, i64)>> = vec![Vec::new(); sources.len()];
        let begin = Instant::now();
        while begin.elapsed() < Duration::from_secs_f64(seconds) {
            player.tick();
            for (slot, frames) in shown.iter_mut().enumerate() {
                let Some(wall) = player.camera(slot).unwrap().frame().map(|f| f.wall_us) else {
                    continue;
                };
                if frames.last().is_none_or(|(_, last)| *last != wall) {
                    frames.push((Instant::now(), wall));
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        player.pause();
        let waited = player.waited();
        println!(
            "reverse {speed}×: waited for cameras {} times, {} ms",
            waited.1 - waited_before.1,
            (waited.0 - waited_before.0).as_millis()
        );

        for (slot, frames) in shown.iter().enumerate() {
            let source = &sources[slot];
            // Every displayed frame is exactly the one before the previous one.
            for (i, pair) in frames.windows(2).enumerate() {
                let current = source.frame_at(pair[0].1).unwrap();
                let expected = source
                    .record(source.prev(current).unwrap())
                    .unwrap()
                    .wall_us;
                assert_eq!(
                    pair[1].1,
                    expected,
                    "{speed}×, camera {slot}: frame skipped or out of order after {i} frames, \
                     {} ms after the previous one; shown {:?}",
                    pair[1].0.duration_since(pair[0].0).as_millis(),
                    frames
                        .iter()
                        .map(|(_, w)| (w - start) / 1000)
                        .collect::<Vec<_>>()
                );
            }
            let crossed = frames.first().unwrap().1
                >= source.record(boundary_of(source, start)).unwrap().wall_us
                && frames.last().unwrap().1
                    < source.record(boundary_of(source, start)).unwrap().wall_us;
            assert!(
                crossed,
                "{speed}×, camera {slot}: segment boundary not crossed"
            );

            // Speed from the first change after starting to the last change.
            let (t0, w0) = frames[1];
            let (t1, w1) = *frames.last().unwrap();
            let measured = (w0 - w1) as f64 / 1e6 / t1.duration_since(t0).as_secs_f64();
            println!(
                "reverse {speed}×, camera {slot}: {} frames, measured {measured:.3}×",
                frames.len()
            );
            assert!(
                (measured / speed - 1.0).abs() <= 0.05,
                "reverse {speed}×: measured {measured}"
            );
        }
    }

    // Faster than MAX_REVERSE_SPEED is forward only.
    player.set_speed(2.0);
    player.play_reverse();
    assert_eq!(player.speed(), MAX_REVERSE_SPEED);
    player.change_speed(true);
    assert_eq!(player.speed(), MAX_REVERSE_SPEED, "no faster reverse");
    player.set_speed(1.0);
    assert!(!player.is_reverse(), "1× continues forwards");

    // Reverse stops at the start of the recording.
    let (range_start, _) = player.range().unwrap();
    player.seek(range_start + 100_000);
    player.set_speed(0.5);
    player.play_reverse();
    assert!(wait_for(Duration::from_secs(2), || {
        player.tick();
        !player.is_playing()
    }));
    assert_eq!(player.position(), range_start);

    drop(player);
    recording.stop();
}

/// First frame of the segment of the frame at `wall_us`.
fn boundary_of(source: &CameraSource, wall_us: i64) -> var_playback::FrameRef {
    let frame = source.frame_at(wall_us).unwrap();
    let mut first = frame;
    while let Some(prev) = source.prev(first) {
        if prev.segment != frame.segment {
            break;
        }
        first = prev;
    }
    first
}
