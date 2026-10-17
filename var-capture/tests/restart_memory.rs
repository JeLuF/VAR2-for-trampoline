//! Memory over repeated camera restarts (step 4: "20 times without memory growth").
//! Not run by default (takes about a minute per variant):
//! `cargo test --release -p var-capture --test restart_memory -- --ignored --nocapture`

use std::time::Duration;

use var_capture::{
    Camera, DeviceInfo, DeviceWatcher, Mode, PixelFormat, WatcherOptions, synthetic,
};

#[cfg(windows)]
fn private_mb() -> f64 {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut counters: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    // SAFETY: valid pointer to a correctly sized, initialised structure.
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
            counters.cb,
        )
    };
    counters.PrivateUsage as f64 / 1e6
}

#[cfg(not(windows))]
fn private_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
        })
        .map_or(0.0, |kb| kb / 1000.0)
}

fn cycles(format: PixelFormat, unplug: bool) -> Vec<f64> {
    var_capture::init().unwrap();
    let device = synthetic::devices(1).remove(0);
    let mode = device
        .modes
        .iter()
        .find(|m| m.format == format && m.width == 1920 && m.capture_framerate() == (50, 1))
        .unwrap()
        .clone();
    restart(&device, &mode, unplug)
}

fn restart(device: &DeviceInfo, mode: &Mode, unplug: bool) -> Vec<f64> {
    let mut memory = Vec::new();
    for _ in 0..15 {
        synthetic::set_connected(1, true);
        let camera = Camera::start(device, mode, || {}).unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        if unplug {
            synthetic::set_connected(1, false);
            std::thread::sleep(Duration::from_millis(1200));
        }
        camera.stop_in_background().join().unwrap();
        memory.push(private_mb());
    }
    synthetic::set_connected(1, true);
    memory
}

fn report(name: &str, memory: &[f64]) {
    let text: Vec<String> = memory.iter().map(|m| format!("{m:.0}")).collect();
    println!("{name}: {} MB", text.join(" "));
}

#[test]
#[ignore]
fn restart_memory() {
    report("YUY2 stop", &cycles(PixelFormat::Raw("YUY2".into()), false));
    report(
        "YUY2 unplug+stop",
        &cycles(PixelFormat::Raw("YUY2".into()), true),
    );
    report("MJPEG stop", &cycles(PixelFormat::Mjpeg, false));

    // Real cameras, if connected: each in its first 1080p MJPEG and YUY2/NV12 mode.
    let watcher = DeviceWatcher::start(WatcherOptions::default(), || {});
    for device in watcher.devices().iter() {
        for format in ["MJPEG", "YUY2", "NV12"] {
            let Some(mode) = device
                .modes
                .iter()
                .find(|m| m.format.to_string() == format && m.height == 1080)
            else {
                continue;
            };
            report(
                &format!("{} {mode}", device.name),
                &restart(device, mode, false),
            );
        }
    }
}
