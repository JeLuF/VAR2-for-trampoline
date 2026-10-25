//! SRT stream of a camera into a receiver (as OBS would be), on the loopback address.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use var_capture::{Camera, PixelFormat, synthetic};
use var_record::select_encoder;
use var_record::stream::{self, StreamSettings, StreamState, Streamer};

/// A receiver like OBS's Media Source: listens, demuxes MPEG-TS, decodes H.264.
/// Counts decoded frames.
fn receiver(port: u16) -> (gst::Pipeline, Arc<AtomicU64>) {
    let pipeline = gst::parse::launch(&format!(
        "srtsrc uri=srt://127.0.0.1:{port}?mode=listener latency=120 \
         ! tsdemux ! h264parse ! decodebin ! appsink name=sink sync=false"
    ))
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    let frames = Arc::new(AtomicU64::new(0));
    let sink = pipeline
        .by_name("sink")
        .unwrap()
        .downcast::<gst_app::AppSink>()
        .unwrap();
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample({
                let frames = Arc::clone(&frames);
                move |sink| {
                    let _ = sink.pull_sample();
                    frames.fetch_add(1, Ordering::Relaxed);
                    Ok(gst::FlowSuccess::Ok)
                }
            })
            .build(),
    );
    (pipeline, frames)
}

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
fn camera_streams_to_receiver_and_reconnects() {
    var_capture::init().unwrap();
    if !stream::available() {
        eprintln!("skipped: no SRT in this GStreamer");
        return;
    }
    let Some(encoder) = select_encoder(false) else {
        eprintln!("skipped: no H.264 encoder");
        return;
    };
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

    let port = 19_000 + (std::process::id() % 1000) as u16;
    let (receiver, frames) = receiver(port);
    // VAR first, as caller: the receiver isn't there yet, so it retries.
    let streamer = Streamer::start(
        &encoder,
        StreamSettings {
            url: format!("srt://127.0.0.1:{port}"),
            latency_ms: 120,
            passphrase: String::new(),
            bitrate_kbps: 4000,
            fps: 50,
        },
    )
    .unwrap();
    let tap = streamer.tap().unwrap();
    let attached = camera.attach(&tap).unwrap();
    assert!(
        wait_for(Duration::from_secs(15), || matches!(
            streamer.state(),
            StreamState::Error(_)
        )),
        "no receiver yet: error, retrying; state {:?}",
        streamer.state()
    );
    assert_ne!(streamer.state(), StreamState::Streaming);

    // The receiver appears: the stream connects by itself and video arrives.
    receiver.set_state(gst::State::Playing).unwrap();
    let connected = Instant::now();
    assert!(
        wait_for(Duration::from_secs(20), || frames.load(Ordering::Relaxed)
            >= 100),
        "frames received: {}, state {:?}",
        frames.load(Ordering::Relaxed),
        streamer.state()
    );
    println!(
        "stream: 100 frames received {:.1} s after the receiver started, {:.1} Mbit/s",
        connected.elapsed().as_secs_f64(),
        streamer.bitrate() / 1e6
    );
    assert_eq!(streamer.state(), StreamState::Streaming);
    assert!(streamer.bitrate() > 0.0, "bitrate not measured");

    // The camera is not held up by the stream (a few drops may come from load).
    let stats = camera.stats();
    println!(
        "camera: {} captured, {} dropped",
        stats.captured, stats.dropped
    );
    assert!(
        stats.dropped * 100 <= stats.captured,
        "camera dropped frames: {stats:?}"
    );

    camera.detach(&attached);
    std::thread::sleep(Duration::from_millis(200));
    camera.remove(attached);
    drop(streamer);
    receiver.set_state(gst::State::Null).unwrap();
}
