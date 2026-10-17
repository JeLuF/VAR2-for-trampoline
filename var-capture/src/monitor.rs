//! Watches the connected capture devices and keeps an up-to-date list.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use tracing::{debug, info, warn};

use crate::device::DeviceInfo;
use crate::synthetic;

/// How long the watcher thread blocks on the monitor bus before checking for shutdown.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Interval for re-reading the full device list, in addition to hot-plug messages.
/// Covers device providers that don't report hot-plug events, so the list is never
/// more than about a second out of date (verification: update within 2 s).
const RESYNC_INTERVAL: Duration = Duration::from_secs(1);

/// The capture API used on each OS (AD-1). Other providers list the same cameras a
/// second time (e.g. Kernel Streaming on Windows, PipeWire on Linux) and are hidden.
#[cfg(target_os = "windows")]
const CAPTURE_APIS: &[&str] = &["mediafoundation"];
#[cfg(target_os = "linux")]
const CAPTURE_APIS: &[&str] = &["v4l2"];
#[cfg(not(any(target_os = "windows", target_os = "linux")))]
const CAPTURE_APIS: &[&str] = &[]; // macOS: AVFoundation is the only video provider.

#[derive(Debug, Clone, Default)]
pub struct WatcherOptions {
    /// Number of synthetic test cameras to add to the device list (debug flag).
    pub synthetic_cameras: u32,
    /// List devices from all GStreamer providers, not only the OS's capture API.
    pub all_providers: bool,
}

/// Keeps the list of capture devices current on a background thread.
///
/// The list is replaced as a whole on every change, so [`DeviceWatcher::devices`]
/// is cheap and never blocks on device enumeration.
pub struct DeviceWatcher {
    devices: Arc<Mutex<Arc<Vec<DeviceInfo>>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl DeviceWatcher {
    /// Enumerates the devices and starts watching for changes. `on_change` is called
    /// from the watcher thread after the list has changed.
    pub fn start(options: WatcherOptions, on_change: impl Fn() + Send + 'static) -> Self {
        let monitor = gst::DeviceMonitor::new();
        monitor.add_filter(Some("Video/Source"), None);
        // Providers can hide each other (PipeWire hides V4L2); the API filter below
        // decides instead, so the result is the same on every machine.
        monitor.set_show_all_devices(true);

        let started = match monitor.start() {
            Ok(()) => true,
            Err(err) => {
                warn!(%err, "device monitor did not start; falling back to polling");
                false
            }
        };

        let enumerator = Enumerator { monitor, options };
        let initial = enumerator.enumerate();
        log_changes(&[], &initial);
        info!(count = initial.len(), "capture devices enumerated");

        let devices = Arc::new(Mutex::new(Arc::new(initial)));
        let stop = Arc::new(AtomicBool::new(false));

        let thread = thread::Builder::new()
            .name("var-device-watcher".into())
            .spawn({
                let devices = Arc::clone(&devices);
                let stop = Arc::clone(&stop);
                move || enumerator.run(started, &devices, &stop, on_change)
            })
            .expect("failed to spawn device watcher thread");

        DeviceWatcher {
            devices,
            stop,
            thread: Some(thread),
        }
    }

    /// The current device list.
    pub fn devices(&self) -> Arc<Vec<DeviceInfo>> {
        Arc::clone(&self.devices.lock().unwrap())
    }
}

impl Drop for DeviceWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Enumerator {
    monitor: gst::DeviceMonitor,
    options: WatcherOptions,
}

impl Enumerator {
    fn run(
        self,
        started: bool,
        devices: &Mutex<Arc<Vec<DeviceInfo>>>,
        stop: &AtomicBool,
        on_change: impl Fn(),
    ) {
        let bus = self.monitor.bus();
        let mut last_resync = Instant::now();

        while !stop.load(Ordering::Relaxed) {
            let mut dirty = false;
            if let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(
                POLL_INTERVAL.as_millis() as u64,
            )) {
                use gst::MessageView;
                match message.view() {
                    MessageView::DeviceAdded(_)
                    | MessageView::DeviceRemoved(_)
                    | MessageView::DeviceChanged(_) => dirty = true,
                    _ => {}
                }
            }
            if last_resync.elapsed() >= RESYNC_INTERVAL {
                last_resync = Instant::now();
                dirty = true;
            }
            if !dirty {
                continue;
            }

            let new = self.enumerate();
            let mut current = devices.lock().unwrap();
            if **current != new {
                log_changes(&current, &new);
                *current = Arc::new(new);
                drop(current);
                on_change();
            }
        }

        if started {
            self.monitor.stop();
        }
        debug!("device watcher stopped");
    }

    fn enumerate(&self) -> Vec<DeviceInfo> {
        let mut list: Vec<DeviceInfo> = self
            .monitor
            .devices()
            .iter()
            .map(DeviceInfo::from_gst)
            .filter(|device| {
                self.options.all_providers
                    || CAPTURE_APIS.is_empty()
                    || CAPTURE_APIS.contains(&device.api.as_str())
            })
            .collect();
        list.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        make_ids_unique(&mut list);
        list.extend(synthetic::devices(self.options.synthetic_cameras));
        list
    }
}

/// Several capture nodes of one USB device can share the stable ID (e.g. the USB bus
/// path on Linux). The second and later get `#2`, `#3`, … in list order.
fn make_ids_unique(list: &mut [DeviceInfo]) {
    let mut seen: HashMap<String, u32> = HashMap::new();
    for device in list {
        let count = seen.entry(device.id.clone()).or_insert(0);
        *count += 1;
        if *count > 1 {
            device.id = format!("{}#{count}", device.id);
        }
    }
}

fn log_changes(old: &[DeviceInfo], new: &[DeviceInfo]) {
    let old_ids: HashSet<&str> = old.iter().map(|d| d.id.as_str()).collect();
    let new_ids: HashSet<&str> = new.iter().map(|d| d.id.as_str()).collect();
    for device in new.iter().filter(|d| !old_ids.contains(d.id.as_str())) {
        info!(
            name = %device.name,
            id = %device.id,
            api = %device.api,
            modes = device.modes.len(),
            "capture device connected"
        );
    }
    for device in old.iter().filter(|d| !new_ids.contains(d.id.as_str())) {
        info!(name = %device.name, id = %device.id, "capture device disconnected");
    }
}
