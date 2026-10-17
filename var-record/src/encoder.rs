//! H.264 encoder selection (AD-2): the first working hardware encoder from a per-OS
//! priority list, OpenH264 as software fallback.
//!
//! GPL encoders (x264) are never used, even if installed (PLT-007).

use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use tracing::{debug, info, warn};

use crate::RecordError;

/// Units of an encoder's `bitrate` property.
#[derive(Clone, Copy)]
enum BitrateUnit {
    Kbps,
    Bps,
}

/// How to configure one encoder element. Options the element doesn't have are skipped.
struct EncoderSpec {
    factory: &'static str,
    bitrate_unit: BitrateUnit,
    /// Property for the keyframe interval in frames.
    gop_property: &'static str,
    /// Further properties: constant bitrate, no B-frames (simpler decoding and
    /// frame stepping, lower latency), speed settings.
    options: &'static [(&'static str, &'static str)],
}

const fn hw(
    factory: &'static str,
    gop_property: &'static str,
    options: &'static [(&'static str, &'static str)],
) -> EncoderSpec {
    EncoderSpec {
        factory,
        bitrate_unit: BitrateUnit::Kbps,
        gop_property,
        options,
    }
}

// Option sets per vendor; not every OS uses every vendor.
#[allow(dead_code)]
const NVENC_OPTIONS: &[(&str, &str)] = &[("rc-mode", "cbr"), ("bframes", "0")];
#[allow(dead_code)]
const QSV_OPTIONS: &[(&str, &str)] = &[("rate-control", "cbr"), ("b-frames", "0")];
#[allow(dead_code)]
const AMF_OPTIONS: &[(&str, &str)] = &[("rate-control", "cbr"), ("b-frames", "0")];
#[allow(dead_code)]
const VA_OPTIONS: &[(&str, &str)] = &[("rate-control", "cbr"), ("b-frames", "0")];

#[cfg(target_os = "windows")]
const HARDWARE_ENCODERS: &[EncoderSpec] = &[
    hw("nvd3d11h264enc", "gop-size", NVENC_OPTIONS),
    hw("nvh264enc", "gop-size", NVENC_OPTIONS),
    hw("qsvh264enc", "gop-size", QSV_OPTIONS),
    hw("amfh264enc", "gop-size", AMF_OPTIONS),
    // Media Foundation: covers further vendors, e.g. Qualcomm.
    hw(
        "mfh264enc",
        "gop-size",
        &[("rc-mode", "cbr"), ("bframes", "0")],
    ),
];

#[cfg(target_os = "macos")]
const HARDWARE_ENCODERS: &[EncoderSpec] = &[
    hw(
        "vtenc_h264_hw",
        "max-keyframe-interval",
        &[("allow-frame-reordering", "false"), ("realtime", "true")],
    ),
    hw(
        "vtenc_h264",
        "max-keyframe-interval",
        &[("allow-frame-reordering", "false"), ("realtime", "true")],
    ),
];

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const HARDWARE_ENCODERS: &[EncoderSpec] = &[
    hw("nvh264enc", "gop-size", NVENC_OPTIONS),
    hw("vah264enc", "key-int-max", VA_OPTIONS),
    hw("vah264lpenc", "key-int-max", VA_OPTIONS),
    hw("qsvh264enc", "gop-size", QSV_OPTIONS),
    hw(
        "vaapih264enc",
        "keyframe-period",
        &[("rate-control", "cbr"), ("max-bframes", "0")],
    ),
];

const SOFTWARE_ENCODER: EncoderSpec = EncoderSpec {
    factory: "openh264enc",
    bitrate_unit: BitrateUnit::Bps,
    gop_property: "gop-size",
    options: &[
        ("rate-control", "bitrate"),
        ("complexity", "low"),
        ("usage-type", "camera"),
        ("enable-frame-skip", "false"),
        ("multi-thread", "4"),
        ("slice-mode", "auto"),
    ],
};

/// The encoder used for recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderInfo {
    /// GStreamer element name, e.g. `mfh264enc`.
    pub factory: String,
    /// Human-readable name of the element.
    pub description: String,
    pub hardware: bool,
}

/// Encoder settings for one camera mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodingSettings {
    pub bitrate_kbps: u32,
    /// Keyframe interval in frames.
    pub gop_frames: u32,
}

impl EncodingSettings {
    /// Reference bitrate: 15 Mbit/s for 1080p50 (REC-008).
    const REFERENCE_KBPS: f64 = 15_000.0;
    const REFERENCE_PIXEL_RATE: f64 = 1920.0 * 1080.0 * 50.0;
    const MIN_KBPS: f64 = 2_000.0;
    /// Keyframe interval: 0.5 s (AD-4).
    const GOP_SECONDS: f64 = 0.5;

    /// Bitrate proportional to the pixel rate, so 1080p50 gets 15 Mbit/s and 1080p60
    /// 18 Mbit/s (REC-008); keyframe every 0.5 s.
    pub fn for_mode(width: u32, height: u32, fps_num: i32, fps_den: i32) -> Self {
        let fps = if fps_den > 0 {
            f64::from(fps_num) / f64::from(fps_den)
        } else {
            0.0
        };
        let pixel_rate = f64::from(width) * f64::from(height) * fps;
        let kbps =
            (Self::REFERENCE_KBPS * pixel_rate / Self::REFERENCE_PIXEL_RATE).max(Self::MIN_KBPS);
        EncodingSettings {
            bitrate_kbps: kbps.round() as u32,
            gop_frames: ((fps * Self::GOP_SECONDS).round() as u32).max(1),
        }
    }
}

fn spec(factory: &str) -> Option<&'static EncoderSpec> {
    HARDWARE_ENCODERS
        .iter()
        .chain(std::iter::once(&SOFTWARE_ENCODER))
        .find(|s| s.factory == factory)
}

/// Selects the encoder: the first hardware encoder that is installed and passes a
/// short test encode, else OpenH264. `force_software` skips the hardware encoders
/// (debug option). Takes up to a few seconds; call it off the UI thread.
pub fn select_encoder(force_software: bool) -> Option<EncoderInfo> {
    let candidates: Vec<&EncoderSpec> = if force_software {
        info!("hardware encoders disabled by debug option");
        vec![&SOFTWARE_ENCODER]
    } else {
        HARDWARE_ENCODERS
            .iter()
            .chain(std::iter::once(&SOFTWARE_ENCODER))
            .collect()
    };

    for spec in candidates {
        let Some(factory) = gst::ElementFactory::find(spec.factory) else {
            debug!(encoder = spec.factory, "encoder not installed");
            continue;
        };
        let info = EncoderInfo {
            factory: spec.factory.to_owned(),
            description: factory
                .metadata(gst::ELEMENT_METADATA_LONGNAME)
                .unwrap_or(spec.factory)
                .to_owned(),
            hardware: spec.factory != SOFTWARE_ENCODER.factory,
        };
        match test_encode(&info) {
            Ok(()) => {
                info!(
                    encoder = %info.factory,
                    description = %info.description,
                    hardware = info.hardware,
                    "encoder selected"
                );
                return Some(info);
            }
            Err(err) => warn!(encoder = spec.factory, %err, "encoder failed the test encode"),
        }
    }
    warn!("no usable H.264 encoder found");
    None
}

/// Creates and configures the encoder element.
pub(crate) fn make_encoder(
    info: &EncoderInfo,
    settings: EncodingSettings,
) -> Result<gst::Element, RecordError> {
    let spec = spec(&info.factory)
        .ok_or_else(|| RecordError::Pipeline(format!("unknown encoder {}", info.factory)))?;
    let encoder = gst::ElementFactory::make(spec.factory)
        .build()
        .map_err(|_| RecordError::Pipeline(format!("encoder {} not available", spec.factory)))?;

    let bitrate = match spec.bitrate_unit {
        BitrateUnit::Kbps => settings.bitrate_kbps.to_string(),
        BitrateUnit::Bps => (u64::from(settings.bitrate_kbps) * 1000).to_string(),
    };
    set_option(&encoder, "bitrate", &bitrate);
    set_option(
        &encoder,
        spec.gop_property,
        &settings.gop_frames.to_string(),
    );
    for (name, value) in spec.options {
        set_option(&encoder, name, value);
    }
    Ok(encoder)
}

/// Sets a property from its string form, if the element has it and the value is valid.
fn set_option(element: &gst::Element, name: &str, value: &str) {
    let Some(pspec) = element.find_property(name) else {
        debug!(element = %element.name(), property = name, "property not available");
        return;
    };
    match gst::glib::Value::deserialize_with_pspec(value, &pspec) {
        Ok(v) => element.set_property_from_value(name, &v),
        Err(err) => {
            warn!(element = %element.name(), property = name, value, %err, "invalid property value")
        }
    }
}

/// Encodes a few 1080p60 frames to check that the encoder actually works on this
/// machine (an installed plugin doesn't guarantee usable hardware).
fn test_encode(info: &EncoderInfo) -> Result<(), RecordError> {
    let err = |e: &dyn std::fmt::Display| RecordError::Pipeline(e.to_string());

    let pipeline = gst::Pipeline::new();
    let source = gst::ElementFactory::make("videotestsrc")
        .property("num-buffers", 10i32)
        .build()
        .map_err(|e| err(&e))?;
    let caps = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("video/x-raw")
                .field("format", "NV12")
                .field("width", 1920i32)
                .field("height", 1080i32)
                .field("framerate", gst::Fraction::new(60, 1))
                .build(),
        )
        .build()
        .map_err(|e| err(&e))?;
    let convert = gst::ElementFactory::make("videoconvert")
        .build()
        .map_err(|e| err(&e))?;
    let encoder = make_encoder(info, EncodingSettings::for_mode(1920, 1080, 60, 1))?;
    let sink = gst::ElementFactory::make("fakesink")
        .build()
        .map_err(|e| err(&e))?;

    let chain = [&source, &caps, &convert, &encoder, &sink];
    pipeline.add_many(chain).map_err(|e| err(&e))?;
    gst::Element::link_many(chain).map_err(|e| err(&e))?;

    let encoded = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    encoder
        .static_pad("src")
        .unwrap()
        .add_probe(gst::PadProbeType::BUFFER, {
            let encoded = encoded.clone();
            move |_, _| {
                encoded.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                gst::PadProbeReturn::Ok
            }
        });

    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| err(&e))?;
    let message = pipeline.bus().unwrap().timed_pop_filtered(
        gst::ClockTime::from_seconds(10),
        &[gst::MessageType::Eos, gst::MessageType::Error],
    );
    let _ = pipeline.set_state(gst::State::Null);

    match message.as_ref().map(|m| m.view()) {
        Some(gst::MessageView::Eos(_))
            if encoded.load(std::sync::atomic::Ordering::Relaxed) > 0 =>
        {
            Ok(())
        }
        Some(gst::MessageView::Eos(_)) => Err(RecordError::Pipeline("no output".into())),
        Some(gst::MessageView::Error(e)) => Err(RecordError::Pipeline(e.error().to_string())),
        _ => Err(RecordError::Pipeline(format!(
            "timeout after {:?}",
            Duration::from_secs(10)
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrate_and_gop_per_mode() {
        let s = EncodingSettings::for_mode(1920, 1080, 50, 1);
        assert_eq!((s.bitrate_kbps, s.gop_frames), (15_000, 25));
        let s = EncodingSettings::for_mode(1920, 1080, 60, 1);
        assert_eq!((s.bitrate_kbps, s.gop_frames), (18_000, 30));
        let s = EncodingSettings::for_mode(1920, 1080, 60000, 1001);
        assert_eq!((s.bitrate_kbps, s.gop_frames), (17_982, 30));
        // Small modes get the minimum bitrate.
        let s = EncodingSettings::for_mode(640, 480, 30, 1);
        assert_eq!(s.bitrate_kbps, 2_000);
    }

    #[test]
    fn x264_is_never_a_candidate() {
        assert!(spec("x264enc").is_none());
    }
}
