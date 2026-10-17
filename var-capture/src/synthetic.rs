//! Synthetic test camera.
//!
//! A `videotestsrc` with a burned-in frame counter and wall-clock time that appears in
//! the device list like a real camera when the application is started with
//! `--synthetic-cameras`. It makes most tests possible without capture hardware.
//! Every 5 seconds it shows a white frame (see [`FLASH_INTERVAL`]).

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;

use crate::device::{DeviceInfo, DeviceKind, Framerate, Mode, PixelFormat};

/// API name reported for synthetic cameras.
pub const API: &str = "synthetic";

const ID_PREFIX: &str = "synthetic:";

/// Numbers (1-based) of synthetic cameras that are unplugged (debug).
static UNPLUGGED: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());

/// Simulates unplugging or replugging synthetic camera `number` (1-based): an unplugged
/// camera disappears from the device list and stops delivering frames, like a real
/// camera that is disconnected. For testing camera failures (REC-004 – REC-006).
pub fn set_connected(number: u32, connected: bool) {
    let mut unplugged = UNPLUGGED.lock().unwrap();
    let changed = if connected {
        unplugged.remove(&number)
    } else {
        unplugged.insert(number)
    };
    if changed {
        tracing::info!(number, connected, "synthetic camera plugged");
    }
}

pub fn is_connected(number: u32) -> bool {
    !UNPLUGGED.lock().unwrap().contains(&number)
}

/// Number of a synthetic camera from its device ID.
pub fn number(device_id: &str) -> Option<u32> {
    device_id.strip_prefix(ID_PREFIX)?.parse().ok()
}

/// Descriptions of `count` synthetic cameras, without unplugged ones.
pub fn devices(count: u32) -> Vec<DeviceInfo> {
    (1..=count)
        .filter(|&n| is_connected(n))
        .map(|n| DeviceInfo {
            id: format!("{ID_PREFIX}{n}"),
            name: format!("Synthetic camera {n}"),
            api: API.to_owned(),
            kind: DeviceKind::Synthetic,
            modes: modes(),
            handle: None,
        })
        .collect()
}

/// The modes a synthetic camera offers: the formats and frame rates of CAP-002/CAP-004.
pub fn modes() -> Vec<Mode> {
    let mut modes = Vec::new();
    for format in [PixelFormat::Mjpeg, PixelFormat::Raw("YUY2".to_owned())] {
        for (width, height) in [(1920, 1080), (1280, 720)] {
            for fps in [60, 50] {
                modes.push(Mode {
                    format: format.clone(),
                    width,
                    height,
                    framerate: Framerate::Fixed { num: fps, den: 1 },
                });
            }
        }
    }
    modes
}

/// `gst-launch` description of a synthetic source delivering `mode`, without a sink.
/// With `num_buffers`, the source ends with EOS after that many frames (for tests).
/// Frame counter, clock and flash are added by [`add_effects`].
pub fn launch_description(mode: &Mode, num_buffers: Option<u32>) -> String {
    let (num, den) = mode.capture_framerate();
    let size = format!(
        "width={},height={},framerate={num}/{den}",
        mode.width, mode.height
    );
    let num_buffers = num_buffers
        .map(|n| format!(" num-buffers={n}"))
        .unwrap_or_default();
    // `videotestsrc` is too slow to draw 1080p60 in real time, so the pattern is drawn
    // once and repeated by `imagefreeze`; only counter and clock are drawn per frame.
    // `imagefreeze` takes each timestamp from the clock when its thread gets to it, so
    // its thread must do nothing else: the queue moves drawing and JPEG encoding or
    // conversion to another thread. The description ends with an explicit
    // capsfilter, so it also works as a bin with a ghost src pad (a trailing bare caps
    // string would be read as an element).
    //
    // Counter and clock are drawn by the application, not with `timeoverlay` and
    // `clockoverlay`: those (Pango) leak about 40 MB per pipeline instance on some
    // platforms, which adds up over camera restarts.
    let queue = "queue max-size-buffers=3 max-size-bytes=0 max-size-time=0";
    let output = match &mode.format {
        PixelFormat::Mjpeg => {
            format!("jpegenc quality=75 idct-method=ifast ! capsfilter caps=\"image/jpeg,{size}\"")
        }
        PixelFormat::Raw(format) | PixelFormat::Other(format) => format!(
            "videoconvert n-threads=4 ! capsfilter caps=\"video/x-raw,format={format},{size}\""
        ),
    };

    format!(
        "videotestsrc num-buffers=1 pattern=smpte \
         ! video/x-raw,format=I420,width={},height={} \
         ! imagefreeze is-live=true{num_buffers} \
         ! video/x-raw,{size} \
         ! {queue} name={EFFECTS_ELEMENT} ! {output}",
        mode.width, mode.height
    )
}

/// The synthetic camera shows a white frame every `FLASH_INTERVAL` on the shared
/// clock: the first frame after each multiple of the interval. All synthetic cameras
/// flash at the same moment, like a camera flash seen by real cameras, which makes
/// the synchronisation of recordings testable (CAP-009) with `var-check --sync`.
pub const FLASH_INTERVAL: Duration = Duration::from_secs(5);

/// Element in the launch description after which the effects are applied (I420 frames).
const EFFECTS_ELEMENT: &str = "effects";

/// Adds the per-frame effects to a synthetic source bin built from
/// [`launch_description`]:
/// - frame counter (top left, frames since the source started) and local time (top
///   right), for checking frame stepping and synchronisation by eye;
/// - the flash every [`FLASH_INTERVAL`];
/// - no frames while synthetic camera `number` is unplugged (see [`set_connected`]).
pub(crate) fn add_effects(bin: &gst::Bin, mode: &Mode, number: Option<u32>) {
    let Some(pad) = bin
        .by_name(EFFECTS_ELEMENT)
        .and_then(|e| e.static_pad("src"))
    else {
        return;
    };
    let (width, height) = (mode.width as usize, mode.height as usize);
    let interval = FLASH_INTERVAL.as_nanos() as u64;
    let last_period = AtomicU64::new(u64::MAX);
    let frames = AtomicU64::new(0);

    pad.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
        if number.is_some_and(|n| !is_connected(n)) {
            return gst::PadProbeReturn::Drop;
        }
        let base = pad
            .parent_element()
            .and_then(|e| e.base_time())
            .map_or(0, |t| t.nseconds());
        let Some(gst::PadProbeData::Buffer(buffer)) = &mut info.data else {
            return gst::PadProbeReturn::Ok;
        };
        let frame = frames.fetch_add(1, Ordering::Relaxed);
        let period = buffer.pts().map(|pts| (base + pts.nseconds()) / interval);
        let previous = period.map(|p| last_period.swap(p, Ordering::Relaxed));
        let flash =
            matches!((previous, period), (Some(prev), Some(p)) if prev != u64::MAX && prev != p);

        let Ok(mut map) = buffer.make_mut().map_writable() else {
            return gst::PadProbeReturn::Ok;
        };
        let mut image = I420 {
            data: &mut map,
            width,
            height,
        };
        if flash {
            // White: Y 235, Cb = Cr = 128.
            image.data.fill(128);
            image.data[..width * height].fill(235);
        }
        let scale = (height / 120).max(2);
        let margin = scale * 4;
        image.draw_text(margin, margin, scale, &frame.to_string());
        let clock = chrono::Local::now().format("%H:%M:%S").to_string();
        let clock_width = text_width(&clock, scale);
        image.draw_text(
            width.saturating_sub(clock_width + margin),
            margin,
            scale,
            &clock,
        );
        gst::PadProbeReturn::Ok
    });
}

/// 5×7 pixel font for digits and ':' (rows top to bottom, 5 bits each, MSB left).
fn glyph(c: char) -> [u8; 7] {
    match c {
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        ':' => [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00],
        _ => [0; 7],
    }
}

/// Width in pixels of `text` drawn at `scale` (6 columns per character incl. spacing,
/// plus a border of one column on each side).
fn text_width(text: &str, scale: usize) -> usize {
    (text.chars().count() * 6 + 1) * scale
}

/// An I420 frame with tightly packed planes (true for the synthetic modes).
struct I420<'a> {
    data: &'a mut [u8],
    width: usize,
    height: usize,
}

impl I420<'_> {
    /// Draws white text on a black box, top left corner at (x, y).
    fn draw_text(&mut self, x: usize, y: usize, scale: usize, text: &str) {
        let box_w = text_width(text, scale);
        let box_h = 9 * scale;
        let (w, h) = (self.width, self.height);
        let luma_size = w * h;
        let chroma_w = w / 2;
        let chroma_size = chroma_w * (h / 2);
        if x + box_w > w || y + box_h > h || self.data.len() < luma_size + 2 * chroma_size {
            return;
        }

        // Black box with neutral chroma.
        for row in y..y + box_h {
            self.data[row * w + x..row * w + x + box_w].fill(16);
        }
        for row in y / 2..(y + box_h) / 2 {
            for plane in [luma_size, luma_size + chroma_size] {
                let start = plane + row * chroma_w + x / 2;
                self.data[start..start + box_w / 2].fill(128);
            }
        }
        // Glyphs.
        for (i, c) in text.chars().enumerate() {
            let left = x + (i * 6 + 1) * scale;
            for (gy, bits) in glyph(c).iter().enumerate() {
                for gx in 0..5 {
                    if bits & (0x10 >> gx) == 0 {
                        continue;
                    }
                    for dy in 0..scale {
                        let row = y + (gy + 1) * scale + dy;
                        let start = row * w + left + gx * scale;
                        self.data[start..start + scale].fill(235);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_have_unique_ids_and_required_modes() {
        let devices = devices(2);
        assert_eq!(devices.len(), 2);
        assert_ne!(devices[0].id, devices[1].id);
        for device in &devices {
            assert_eq!(device.kind, DeviceKind::Synthetic);
            assert!(device.modes.iter().any(|m| m.is_full_hd_50_or_60()));
            assert!(device.modes.iter().all(|m| m.format.is_supported()));
        }
    }

    /// Runs every synthetic mode for a few frames. Needs the GStreamer base plugins
    /// (`videotestsrc`, `pango`, `videoconvert`) and good plugins (`jpeg`).
    #[test]
    fn source_runs_in_every_mode() {
        gst::init().unwrap();
        for mode in modes() {
            let description = format!("{} ! fakesink", launch_description(&mode, Some(3)));
            let pipeline = gst::parse::launch(&description)
                .unwrap_or_else(|err| panic!("cannot build `{description}`: {err}"));
            pipeline.set_state(gst::State::Playing).unwrap();

            let bus = pipeline.bus().unwrap();
            let message = bus
                .timed_pop_filtered(
                    gst::ClockTime::from_seconds(10),
                    &[gst::MessageType::Eos, gst::MessageType::Error],
                )
                .expect("synthetic source did not finish within 10 s");
            pipeline.set_state(gst::State::Null).unwrap();

            if let gst::MessageView::Error(err) = message.view() {
                panic!("{mode:?}: {} ({:?})", err.error(), err.debug());
            }
        }
    }
}
