//! Headless test of device enumeration. Runs without capture hardware: real devices
//! are listed if present, the synthetic cameras always are.

use std::time::{Duration, Instant};

use var_capture::{DeviceKind, DeviceWatcher, WatcherOptions};

#[test]
fn lists_synthetic_cameras_and_shuts_down() {
    var_capture::init().unwrap();

    let watcher = DeviceWatcher::start(
        WatcherOptions {
            synthetic_cameras: 2,
            all_providers: false,
        },
        || {},
    );

    let devices = watcher.devices();
    let synthetic: Vec<_> = devices
        .iter()
        .filter(|d| d.kind == DeviceKind::Synthetic)
        .collect();
    assert_eq!(synthetic.len(), 2, "devices: {devices:#?}");
    for device in devices.iter() {
        println!(
            "{} [{}] {} modes, id {}",
            device.name,
            device.api,
            device.modes.len(),
            device.id
        );
    }

    // The watcher thread must stop promptly when dropped.
    let start = Instant::now();
    drop(watcher);
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn without_flag_no_synthetic_camera() {
    var_capture::init().unwrap();
    let watcher = DeviceWatcher::start(WatcherOptions::default(), || {});
    assert!(
        watcher
            .devices()
            .iter()
            .all(|d| d.kind == DeviceKind::System)
    );
}
