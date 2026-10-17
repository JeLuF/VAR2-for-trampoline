//! Review at full load: two cameras at 1080p60 recording while being reviewed at 1×
//! and in reverse at 0.5×. Measures the decoder's throughput, how far the shown
//! frames lag behind the review position, how much playback had to wait for the
//! cameras, and how many frames reverse playback skips. Not run by default:
//!
//! ```sh
//! cargo test --release -p var-playback --test load -- --ignored --nocapture
//! ```
//!
//! `VAR_LOAD_REVIEW_ONLY=1` stops the recording before reviewing (no encoder load);
//! `VAR_LOG=var_playback=debug` logs decoding restarts, prefetching and waits.

mod common;

use std::time::{Duration, Instant};

use var_capture::PixelFormat;
use var_playback::{CameraSource, ReviewPlayer};

use common::wait_for;

#[test]
#[ignore]
fn review_at_1080p60() {
    let dir = std::env::temp_dir().join("var-playback-tests").join("load");
    // VAR_LOG=var_playback=debug shows decoding restarts and prefetching.
    let _log = std::env::var_os("VAR_LOG").map(|_| {
        var_core::logging::init(&std::env::temp_dir().join("var-playback-tests")).unwrap()
    });
    // YUY2, as from HDMI capture cards: the synthetic MJPEG camera would add a JPEG
    // encoder per camera that real cameras don't have.
    let Some(recording) =
        common::record_mode(2, &dir, PixelFormat::Raw("YUY2".into()), (1920, 1080), 60)
    else {
        return;
    };
    std::thread::sleep(Duration::from_secs(16));
    let slots: Vec<usize> = recording.sources.iter().map(|(s, _)| *s).collect();
    let mut sources: Vec<CameraSource> = recording
        .sources
        .iter()
        .map(|(_, index)| CameraSource::new(std::sync::Arc::clone(index)))
        .collect();
    let review_sources = recording.sources.clone();
    // VAR_LOAD_REVIEW_ONLY=1: stop recording first, to compare review without the
    // encoders' load.
    let recording = if std::env::var_os("VAR_LOAD_REVIEW_ONLY").is_some() {
        recording.stop();
        println!("recording stopped: review only");
        None
    } else {
        Some(recording)
    };
    decoder_throughput(&mut sources[0]);
    let mut player = ReviewPlayer::new(review_sources, 0, || {});
    let (start, _) = player.range().unwrap();

    // 1× for 10 s, sampling the lag of each camera's frame every 10 ms (a UI frame).
    player.seek(start + 1_000_000);
    assert!(wait_for(Duration::from_secs(3), || slots
        .iter()
        .all(|&s| player.camera(s).unwrap().frame().is_some())));
    player.play();
    let mut lags: Vec<Vec<i64>> = vec![Vec::new(); slots.len()];
    let begin = Instant::now();
    while begin.elapsed() < Duration::from_secs(10) {
        player.tick();
        for (i, &slot) in slots.iter().enumerate() {
            if let Some(frame) = player.camera(slot).unwrap().frame() {
                lags[i].push((player.position() - frame.wall_us) / 1000);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    player.pause();
    let advanced = (player.position() - start - 1_000_000) as f64 / 1e6;
    println!(
        "1×: {advanced:.2} s of recording in 10 s, waited for cameras {:?}",
        player.waited()
    );
    for (i, lags) in lags.iter_mut().enumerate() {
        lags.sort_unstable();
        let p95 = lags[lags.len() * 95 / 100];
        let max = *lags.last().unwrap();
        println!(
            "1× camera {i}: lag median {} ms, p95 {p95} ms, max {max} ms",
            lags[lags.len() / 2]
        );
    }

    // Reverse 0.5× for 6 s (3 s of recording): frames skipped per camera.
    let from = player.position();
    player.seek(from);
    assert!(wait_for(Duration::from_secs(3), || slots
        .iter()
        .zip(&sources)
        .all(|(&s, source)| {
            let expected = source
                .frame_at(from)
                .and_then(|f| source.record(f))
                .map(|r| r.wall_us);
            player.camera(s).unwrap().frame().map(|f| f.wall_us) == expected
        })));
    player.set_speed(0.5);
    player.play_reverse();
    let mut shown: Vec<Vec<i64>> = vec![Vec::new(); slots.len()];
    let begin = Instant::now();
    while begin.elapsed() < Duration::from_secs(6) {
        player.tick();
        for (i, &slot) in slots.iter().enumerate() {
            if let Some(wall) = player.camera(slot).unwrap().frame().map(|f| f.wall_us)
                && shown[i].last() != Some(&wall)
            {
                shown[i].push(wall);
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    player.pause();
    println!(
        "reverse: {:.2} s of recording in 6 s, waited for cameras {:?} in total",
        (from - player.position()) as f64 / 1e6,
        player.waited()
    );
    for (i, walls) in shown.iter().enumerate() {
        let source = &sources[i];
        let first = source.frame_at(walls[0]).unwrap();
        let last = source.frame_at(*walls.last().unwrap()).unwrap();
        let mut frames = 0;
        let mut frame = first;
        while frame != last {
            frame = source.prev(frame).unwrap();
            frames += 1;
        }
        println!(
            "reverse 0.5× camera {i}: {} of {} frames shown",
            walls.len() - 1,
            frames
        );
    }

    drop(player);
    if let Some(recording) = recording {
        recording.stop();
    }
}

/// Decodes 300 frames of a camera with one decoder (at most 16 in the decoder, like
/// the review player) and prints the frames per second. Review at 1× needs the
/// camera's frame rate per camera.
fn decoder_throughput(source: &mut CameraSource) {
    use var_playback::Decoder;
    let mut packets = Vec::new();
    let mut frame = source.first().unwrap();
    while packets.len() < 300 {
        let packet = source.read(frame).unwrap();
        packets.push((
            packet.data,
            packet.record.pts90 * 100_000 / 9,
            packet.record.keyframe,
        ));
        frame = source.next(frame).unwrap();
    }
    let decoder = Decoder::new().unwrap();
    let start = Instant::now();
    let (mut fed, mut out) = (0, 0);
    // The last frames stay in the decoder until more input arrives.
    while out < 280 && start.elapsed() < Duration::from_secs(20) {
        while fed < packets.len() && fed - out < 16 {
            let (data, pts, key) = packets[fed].clone();
            decoder.push(data, pts, key, fed == 0);
            fed += 1;
        }
        if decoder
            .pull(gstreamer::ClockTime::from_mseconds(1))
            .is_some()
        {
            out += 1;
        }
    }
    println!(
        "decoder {}: {:.0} frames per second",
        decoder.decoder_name().unwrap_or_default(),
        out as f64 / start.elapsed().as_secs_f64()
    );

    // The same, sleeping until the decoder signals a frame (as the review player).
    let signal = std::sync::Arc::new((std::sync::Mutex::new(0u64), std::sync::Condvar::new()));
    let decoder = Decoder::with_notify({
        let signal = std::sync::Arc::clone(&signal);
        move || {
            *signal.0.lock().unwrap() += 1;
            signal.1.notify_all();
        }
    })
    .unwrap();
    let start = Instant::now();
    let (mut fed, mut out, mut sleeps) = (0, 0, 0);
    while out < 280 && start.elapsed() < Duration::from_secs(20) {
        let seen = *signal.0.lock().unwrap();
        while fed < packets.len() && fed - out < 16 {
            let (data, pts, key) = packets[fed].clone();
            decoder.push(data, pts, key, fed == 0);
            fed += 1;
        }
        let mut pulled = false;
        while decoder.pull(gstreamer::ClockTime::ZERO).is_some() {
            out += 1;
            pulled = true;
        }
        if !pulled {
            sleeps += 1;
            let guard = signal.0.lock().unwrap();
            let _ = signal
                .1
                .wait_timeout_while(guard, Duration::from_millis(10), |n| *n == seen);
        }
    }
    println!(
        "decoder with wake-up: {:.0} frames per second, {sleeps} sleeps",
        out as f64 / start.elapsed().as_secs_f64()
    );
}
