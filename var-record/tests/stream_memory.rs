//! Memory use of a long SRT stream (endurance, NFR-001). Takes minutes:
//! `cargo test -p var-record --test stream_memory -- --ignored --nocapture`.

use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use var_capture::{Camera, PixelFormat, synthetic};
use var_record::select_encoder;
use var_record::stream::{self, StreamSettings, Streamer};

const DURATION: Duration = Duration::from_secs(180);

fn private_mb() -> u64 {
    let r = var_core::resources::sample().unwrap();
    r.private_bytes.unwrap_or(r.resident_bytes) / 1_000_000
}

#[test]
#[ignore = "takes minutes"]
fn stream_memory_stays_flat() {
    var_capture::init().unwrap();
    if !stream::available() {
        eprintln!("skipped: no SRT in this GStreamer");
        return;
    }
    let encoder = select_encoder(false).unwrap();
    println!("encoder: {}", encoder.factory);
    let device = synthetic::devices(1).remove(0);
    let mode = device
        .modes
        .iter()
        .find(|m| {
            m.format == PixelFormat::Mjpeg && m.width == 1920 && m.capture_framerate() == (60, 1)
        })
        .unwrap()
        .clone();
    let camera = Camera::start(&device, &mode, || {}).unwrap();

    let port = 19_500 + (std::process::id() % 400) as u16;
    let receiver = gst::parse::launch(&format!(
        "srtsrc uri=srt://127.0.0.1:{port}?mode=listener latency=120 ! fakesink sync=false"
    ))
    .unwrap();
    receiver.set_state(gst::State::Playing).unwrap();
    let streamer = Streamer::start(
        &encoder,
        StreamSettings {
            url: format!("srt://127.0.0.1:{port}"),
            latency_ms: 120,
            passphrase: String::new(),
            bitrate_kbps: 6000,
            fps: 60,
            width: 1920,
            height: 1080,
        },
    )
    .unwrap();
    let attached = camera.attach(&streamer.tap().unwrap()).unwrap();

    std::thread::sleep(Duration::from_secs(20));
    let start_mb = private_mb();
    let start = Instant::now();
    let mut last_mb = start_mb;
    while start.elapsed() < DURATION {
        std::thread::sleep(Duration::from_secs(15));
        last_mb = private_mb();
        println!(
            "{:>4.0} s: {last_mb} MB, {:?}, {:.1} Mbit/s, camera {:?}",
            start.elapsed().as_secs_f64(),
            streamer.state(),
            streamer.bitrate() / 1e6,
            camera.stats()
        );
    }
    camera.detach(&attached);
    std::thread::sleep(Duration::from_millis(200));
    camera.remove(attached);
    drop(streamer);
    receiver.set_state(gst::State::Null).unwrap();
    assert!(
        last_mb < start_mb + 50,
        "memory grew from {start_mb} MB to {last_mb} MB"
    );
}
