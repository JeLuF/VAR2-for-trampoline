//! Headless test of the capture pipeline with the synthetic camera.

use std::time::{Duration, Instant};

use var_capture::{Camera, PixelFormat, synthetic};

fn run_mode(format: PixelFormat, width: u32, fps: i32) {
    var_capture::init().unwrap();
    let device = synthetic::devices(1).remove(0);
    let mode = device
        .modes
        .iter()
        .find(|m| m.format == format && m.width == width && m.capture_framerate() == (fps, 1))
        .expect("synthetic camera offers the mode")
        .clone();

    let camera = Camera::start(&device, &mode, || {}).unwrap();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        assert_eq!(camera.error(), None);
        std::thread::sleep(Duration::from_millis(100));
    }

    let stats = camera.stats();
    println!("{mode}: {stats:?}");
    let frame = camera.latest_frame().expect("preview frames arrive");
    assert_eq!((frame.width(), frame.height()), (mode.width, mode.height));
    assert!(frame.stride(0) >= frame.width());
    assert!(
        frame.plane(0).len() >= (frame.stride(0) * (frame.height() - 1) + frame.width()) as usize
    );
    assert!(
        frame.plane(1).len()
            >= (frame.stride(1) * (frame.height() / 2 - 1) + frame.width()) as usize
    );
    // Timing-dependent counts are only sanity-checked, so slow CI machines don't fail.
    // The real-time check is the debug overlay and the manual test checklist.
    assert!(stats.captured >= u64::from(fps as u32), "{stats:?}");
    assert!(stats.fps > 0.0);
    if stats.fps < f64::from(fps) * 0.95 {
        println!("warning: synthetic camera slower than real time on this machine");
    }
}

/// Long run without UI; reports dropped frames. Not run by default:
/// `VAR_SOAK_SECS=600 cargo test --release -p var-capture --test camera soak -- --ignored --nocapture`
#[test]
#[ignore]
fn soak() {
    var_capture::init().unwrap();
    let secs: u64 = std::env::var("VAR_SOAK_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let device = synthetic::devices(1).remove(0);
    for format in [PixelFormat::Mjpeg, PixelFormat::Raw("YUY2".into())] {
        let mode = device
            .modes
            .iter()
            .find(|m| m.format == format && m.width == 1920 && m.capture_framerate() == (60, 1))
            .unwrap()
            .clone();
        let camera = Camera::start(&device, &mode, || {}).unwrap();
        std::thread::sleep(Duration::from_secs(secs));
        assert_eq!(camera.error(), None);
        println!("{mode}, {secs} s: {:?}", camera.stats());
    }
}

#[test]
fn mjpeg_1080p50_delivers_nv12_preview() {
    run_mode(PixelFormat::Mjpeg, 1920, 50);
}

#[test]
fn yuy2_1080p60_delivers_nv12_preview() {
    run_mode(PixelFormat::Raw("YUY2".into()), 1920, 60);
}
