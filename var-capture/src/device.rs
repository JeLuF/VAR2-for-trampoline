//! Capture device descriptions, built from GStreamer devices and their caps.

use std::cmp::Ordering;
use std::fmt;

use gstreamer as gst;
use gstreamer::prelude::*;
use tracing::debug;

/// A capture device.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceInfo {
    /// Identifier of the device, unique among the connected devices and stable across
    /// reconnects and reboots as far as the OS allows (CAP-007).
    pub id: String,
    /// Name reported by the OS.
    pub name: String,
    /// Capture API that provides the device (`mediafoundation`, `avf`, `v4l2`, …).
    pub api: String,
    pub kind: DeviceKind,
    /// Supported modes, sorted by format, then largest resolution and frame rate first.
    pub modes: Vec<Mode>,
    /// The GStreamer device, used to create the capture source. `None` for synthetic cameras.
    pub(crate) handle: Option<gst::Device>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    /// A device reported by the OS.
    System,
    /// The synthetic test camera (debug flag only).
    Synthetic,
}

/// One combination of pixel format, resolution and frame rate a device can deliver.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Mode {
    pub format: PixelFormat,
    pub width: u32,
    pub height: u32,
    pub framerate: Framerate,
}

impl Mode {
    /// True for the minimum modes required by CAP-002: 1080p at 50 or 60 fps.
    pub fn is_full_hd_50_or_60(&self) -> bool {
        self.width == 1920 && self.height == 1080 && self.framerate.covers_any(&[50.0, 60.0])
    }

    /// Frame rate the device is asked for: the fixed rate, or the maximum of a range.
    pub fn capture_framerate(&self) -> (i32, i32) {
        match self.framerate {
            Framerate::Fixed { num, den } => (num, den),
            Framerate::Range { max, .. } => max,
        }
    }

    /// Caps that select this mode on the capture source.
    pub(crate) fn to_caps(&self) -> gst::Caps {
        let builder = match &self.format {
            PixelFormat::Mjpeg => gst::Caps::builder("image/jpeg"),
            PixelFormat::Raw(format) => gst::Caps::builder("video/x-raw").field("format", format),
            PixelFormat::Other(media_type) => gst::Caps::builder(media_type.as_str()),
        };
        let (num, den) = self.capture_framerate();
        builder
            .field("width", self.width as i32)
            .field("height", self.height as i32)
            .field("framerate", gst::Fraction::new(num, den))
            .build()
    }
}

impl fmt::Display for Mode {
    /// `MJPEG 1920×1080 @ 60 fps`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {}×{} @ {} fps",
            self.format, self.width, self.height, self.framerate
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PixelFormat {
    /// Motion JPEG (`image/jpeg`).
    Mjpeg,
    /// Uncompressed video (`video/x-raw`) with the GStreamer format name, e.g. `YUY2`, `NV12`.
    Raw(String),
    /// Any other media type, e.g. `video/x-h264`. Listed but not supported for capture.
    Other(String),
}

impl PixelFormat {
    /// Whether the application can capture this format: MJPEG and YUY2 (CAP-004), and
    /// NV12, the pipeline's internal format, which Media Foundation delivers for many
    /// cameras.
    pub fn is_supported(&self) -> bool {
        match self {
            PixelFormat::Mjpeg => true,
            PixelFormat::Raw(format) => format == "YUY2" || format == "NV12",
            PixelFormat::Other(_) => false,
        }
    }

    fn sort_rank(&self) -> u8 {
        match self {
            PixelFormat::Mjpeg => 0,
            PixelFormat::Raw(format) if format == "YUY2" => 1,
            PixelFormat::Raw(format) if format == "NV12" => 2,
            PixelFormat::Raw(_) => 3,
            PixelFormat::Other(_) => 4,
        }
    }

    fn name(&self) -> &str {
        match self {
            PixelFormat::Mjpeg => "MJPEG",
            PixelFormat::Raw(name) | PixelFormat::Other(name) => name,
        }
    }
}

impl fmt::Display for PixelFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Frame rate as reported in the device caps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Framerate {
    /// A fixed rate `num/den` frames per second.
    Fixed { num: i32, den: i32 },
    /// Any rate between `min` and `max`, each given as `(num, den)`.
    Range { min: (i32, i32), max: (i32, i32) },
}

impl Framerate {
    /// Highest frame rate in frames per second.
    pub fn max_fps(&self) -> f64 {
        match *self {
            Framerate::Fixed { num, den } => fraction_to_f64(num, den),
            Framerate::Range {
                max: (num, den), ..
            } => fraction_to_f64(num, den),
        }
    }

    fn covers_any(&self, rates: &[f64]) -> bool {
        const TOLERANCE: f64 = 0.01;
        rates.iter().any(|&fps| match *self {
            Framerate::Fixed { num, den } => (fraction_to_f64(num, den) - fps).abs() < TOLERANCE,
            Framerate::Range { min, max } => {
                fraction_to_f64(min.0, min.1) - TOLERANCE <= fps
                    && fps <= fraction_to_f64(max.0, max.1) + TOLERANCE
            }
        })
    }
}

impl fmt::Display for Framerate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Framerate::Fixed { num, den } => f.write_str(&format_fps(num, den)),
            Framerate::Range { min, max } => {
                write!(
                    f,
                    "{}–{}",
                    format_fps(min.0, min.1),
                    format_fps(max.0, max.1)
                )
            }
        }
    }
}

fn fraction_to_f64(num: i32, den: i32) -> f64 {
    if den == 0 {
        0.0
    } else {
        f64::from(num) / f64::from(den)
    }
}

/// `60/1` → `60`, `30000/1001` → `29.97`.
fn format_fps(num: i32, den: i32) -> String {
    if den != 0 && num % den == 0 {
        (num / den).to_string()
    } else {
        let text = format!("{:.2}", fraction_to_f64(num, den));
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

impl DeviceInfo {
    /// Builds the description of a GStreamer device.
    pub(crate) fn from_gst(device: &gst::Device) -> Self {
        let name = device.display_name().to_string();
        let properties = device.properties();
        let property = |key: &str| properties.as_ref().and_then(|p| string_property(p, key));

        let api = property("device.api").unwrap_or_default();
        let id = stable_id_keys(&api)
            .iter()
            .copied()
            .find_map(property)
            .unwrap_or_else(|| name.clone());

        debug!(
            %name,
            class = %device.device_class(),
            properties = ?properties.as_ref().map(|p| p.to_string()),
            "device properties"
        );

        let modes = device
            .caps()
            .map(|caps| modes_from_caps(&caps))
            .unwrap_or_default();

        DeviceInfo {
            id,
            name,
            api,
            kind: DeviceKind::System,
            modes,
            handle: Some(device.clone()),
        }
    }
}

/// Device properties that identify a device stably across reconnects and reboots
/// (CAP-007), in order of preference, per capture API.
///
/// - Media Foundation: the device path (symbolic link). It contains the USB instance
///   ID, i.e. the serial number or, for devices without one, the USB port.
/// - V4L2: the USB bus path (port topology); `/dev/videoN` depends on the order in
///   which devices are found.
/// - AVFoundation: the unique ID, which is based on the USB location.
fn stable_id_keys(api: &str) -> &'static [&'static str] {
    match api {
        "v4l2" => &["device.bus_path", "device.serial", "device.path"],
        "avf" => &["avf.unique_id", "device.path"],
        _ => &[
            "device.path",
            "avf.unique_id",
            "object.serial",
            "device.serial",
        ],
    }
}

/// Reads a device property as a string, converting numeric values.
fn string_property(properties: &gst::StructureRef, key: &str) -> Option<String> {
    let value = properties.value(key).ok()?;
    value
        .get::<String>()
        .ok()
        .or_else(|| value.transform::<String>().ok()?.get::<String>().ok())
        .filter(|s| !s.is_empty())
}

/// Expands device caps into a flat, sorted list of modes.
///
/// Caps structures may contain lists of formats, sizes and frame rates; every
/// combination becomes a mode. Structures with size ranges are skipped, as capture
/// cards report discrete sizes.
pub(crate) fn modes_from_caps(caps: &gst::CapsRef) -> Vec<Mode> {
    let mut modes = Vec::new();
    for structure in caps.iter() {
        let formats: Vec<PixelFormat> = match structure.name().as_str() {
            "image/jpeg" => vec![PixelFormat::Mjpeg],
            "video/x-raw" => string_values(structure, "format")
                .into_iter()
                .map(PixelFormat::Raw)
                .collect(),
            other => vec![PixelFormat::Other(other.to_owned())],
        };
        let widths = int_values(structure, "width");
        let heights = int_values(structure, "height");
        let framerates = framerate_values(structure);

        if widths.is_empty() || heights.is_empty() {
            debug!(caps = %structure, "skipping caps without fixed size");
            continue;
        }

        for format in &formats {
            for &width in &widths {
                for &height in &heights {
                    for &framerate in &framerates {
                        modes.push(Mode {
                            format: format.clone(),
                            width,
                            height,
                            framerate,
                        });
                    }
                }
            }
        }
    }

    modes.sort_by(compare_modes);
    modes.dedup();
    modes
}

fn compare_modes(a: &Mode, b: &Mode) -> Ordering {
    a.format
        .sort_rank()
        .cmp(&b.format.sort_rank())
        .then_with(|| a.format.name().cmp(b.format.name()))
        .then_with(|| (b.width * b.height).cmp(&(a.width * a.height)))
        .then_with(|| b.width.cmp(&a.width))
        .then_with(|| b.framerate.max_fps().total_cmp(&a.framerate.max_fps()))
}

/// The values of a field that holds a single value or a list of values.
fn field_values(structure: &gst::StructureRef, field: &str) -> Vec<gst::glib::SendValue> {
    let Ok(value) = structure.value(field) else {
        return Vec::new();
    };
    match value.get::<gst::List>() {
        Ok(list) => list.iter().cloned().collect(),
        Err(_) => vec![value.clone()],
    }
}

fn string_values(structure: &gst::StructureRef, field: &str) -> Vec<String> {
    field_values(structure, field)
        .into_iter()
        .filter_map(|v| v.get::<String>().ok())
        .collect()
}

fn int_values(structure: &gst::StructureRef, field: &str) -> Vec<u32> {
    field_values(structure, field)
        .into_iter()
        .filter_map(|v| v.get::<i32>().ok())
        .filter_map(|v| u32::try_from(v).ok())
        .collect()
}

fn framerate_values(structure: &gst::StructureRef) -> Vec<Framerate> {
    field_values(structure, "framerate")
        .into_iter()
        .filter_map(|value| {
            if let Ok(f) = value.get::<gst::Fraction>() {
                Some(Framerate::Fixed {
                    num: f.numer(),
                    den: f.denom(),
                })
            } else if let Ok(range) = value.get::<gst::FractionRange>() {
                Some(Framerate::Range {
                    min: (range.min().numer(), range.min().denom()),
                    max: (range.max().numer(), range.max().denom()),
                })
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn modes(caps: &str) -> Vec<Mode> {
        gst::init().unwrap();
        modes_from_caps(&gst::Caps::from_str(caps).unwrap())
    }

    #[test]
    fn expands_lists_and_sorts() {
        let modes = modes(
            "video/x-raw, format=(string)YUY2, width=(int)1280, height=(int)720, framerate=(fraction)60/1; \
             image/jpeg, width=(int)1920, height=(int)1080, framerate=(fraction){ 50/1, 60/1 }; \
             video/x-raw, format=(string)YUY2, width=(int)1920, height=(int)1080, framerate=(fraction)30000/1001",
        );
        let summary: Vec<String> = modes
            .iter()
            .map(|m| format!("{} {}x{}@{}", m.format, m.width, m.height, m.framerate))
            .collect();
        assert_eq!(
            summary,
            [
                "MJPEG 1920x1080@60",
                "MJPEG 1920x1080@50",
                "YUY2 1920x1080@29.97",
                "YUY2 1280x720@60",
            ]
        );
        assert!(modes[0].is_full_hd_50_or_60());
        assert!(!modes[2].is_full_hd_50_or_60());
    }

    #[test]
    fn handles_format_lists_and_ranges() {
        let modes = modes(
            "video/x-raw, format=(string){ I420, YUY2 }, width=(int)1920, height=(int)1080, \
             framerate=(fraction)[ 1/1, 60/1 ]; \
             video/x-raw, format=(string)NV12, width=(int)[ 1, 4096 ], height=(int)[ 1, 2160 ]",
        );
        assert_eq!(modes.len(), 2);
        assert_eq!(modes[0].format, PixelFormat::Raw("YUY2".into()));
        assert_eq!(modes[0].framerate.to_string(), "1–60");
        assert!(modes[0].is_full_hd_50_or_60());
        assert!(modes[0].format.is_supported());
        assert!(!modes[1].format.is_supported());
    }

    #[test]
    fn removes_duplicates() {
        let modes = modes(
            "image/jpeg, width=(int)1920, height=(int)1080, framerate=(fraction)50/1; \
             image/jpeg, width=(int)1920, height=(int)1080, framerate=(fraction)50/1",
        );
        assert_eq!(modes.len(), 1);
    }
}
