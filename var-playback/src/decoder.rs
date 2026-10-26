//! H.264 decoder for one camera: `appsrc ─ h264parse ─ decodebin ─ NV12 ─ appsink`.
//!
//! `decodebin` picks the hardware decoder with the highest rank (D3D12/D3D11 on
//! Windows, VideoToolbox on macOS, VA-API on Linux) and falls back to software.

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::VideoFrameExt;
use var_core::color::YuvColorimetry;

use crate::PlaybackError;

/// A decoded NV12 frame. The planes are copied out of the decoder's buffers, so
/// frames can be kept in the frame cache without holding buffers the decoder needs
/// (its buffer pool is small).
pub struct DecodedFrame {
    width: u32,
    height: u32,
    planes: [Vec<u8>; 2],
    strides: [u32; 2],
    colorimetry: YuvColorimetry,
    /// The decoder's output caps (NV12), e.g. to hand the frame on to an encoder.
    caps: gst::Caps,
    /// Timestamp given when the frame was pushed, ns.
    pub pts_ns: u64,
}

impl DecodedFrame {
    pub fn caps(&self) -> &gst::Caps {
        &self.caps
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Data of plane `index` (0: Y, 1: CbCr), rows `stride(index)` bytes apart.
    pub fn plane(&self, index: u32) -> &[u8] {
        &self.planes[index as usize]
    }

    pub fn stride(&self, index: u32) -> u32 {
        self.strides[index as usize]
    }

    pub fn colorimetry(&self) -> YuvColorimetry {
        self.colorimetry
    }
}

pub struct Decoder {
    pipeline: gst::Pipeline,
    source: gst_app::AppSrc,
    sink: gst_app::AppSink,
}

impl Decoder {
    pub fn new() -> Result<Self, PlaybackError> {
        Self::with_notify(|| {})
    }

    /// A decoder that calls `notify` (on a GStreamer thread) whenever a decoded frame
    /// is ready to [`pull`](Self::pull), so the caller can sleep until then instead of
    /// polling with timeouts, which are coarse on Windows.
    pub fn with_notify(notify: impl Fn() + Send + Sync + 'static) -> Result<Self, PlaybackError> {
        let err = |e: &dyn std::fmt::Display| PlaybackError::Decoder(e.to_string());
        let pipeline = gst::parse::launch(
            "appsrc name=source format=time stream-type=stream is-live=false block=false \
               max-bytes=0 caps=video/x-h264,stream-format=byte-stream,alignment=au \
             ! h264parse ! decodebin ! videoconvert ! video/x-raw,format=NV12 \
             ! appsink name=sink sync=false max-buffers=16",
        )
        .map_err(|e| err(&e))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| PlaybackError::Decoder("not a pipeline".into()))?;
        let source = pipeline
            .by_name("source")
            .and_then(|e| e.downcast::<gst_app::AppSrc>().ok())
            .ok_or_else(|| PlaybackError::Decoder("no appsrc".into()))?;
        let sink = pipeline
            .by_name("sink")
            .and_then(|e| e.downcast::<gst_app::AppSink>().ok())
            .ok_or_else(|| PlaybackError::Decoder("no appsink".into()))?;
        // The sample stays queued in the appsink until pulled.
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |_| {
                    notify();
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| err(&e))?;
        Ok(Decoder {
            pipeline,
            source,
            sink,
        })
    }

    /// Feeds one access unit. `discont`: first frame after a flush.
    pub fn push(&self, data: Vec<u8>, pts_ns: u64, keyframe: bool, discont: bool) {
        let mut buffer = gst::Buffer::from_mut_slice(data);
        {
            let buffer = buffer.get_mut().unwrap();
            buffer.set_pts(gst::ClockTime::from_nseconds(pts_ns));
            buffer.set_dts(gst::ClockTime::from_nseconds(pts_ns));
            let mut flags = gst::BufferFlags::empty();
            if !keyframe {
                flags |= gst::BufferFlags::DELTA_UNIT;
            }
            if discont {
                flags |= gst::BufferFlags::DISCONT;
            }
            buffer.set_flags(flags);
        }
        let _ = self.source.push_buffer(buffer);
    }

    /// The next decoded frame, waiting at most `timeout`.
    pub fn pull(&self, timeout: gst::ClockTime) -> Option<DecodedFrame> {
        let sample = self.sink.try_pull_sample(timeout)?;
        let caps = sample.caps_owned()?;
        let info = gst_video::VideoInfo::from_caps(&caps).ok()?;
        let buffer = sample.buffer_owned()?;
        let pts_ns = buffer.pts()?.nseconds();
        let colorimetry = var_capture::yuv_colorimetry(&info);
        let frame = gst_video::VideoFrame::from_buffer_readable(buffer, &info).ok()?;
        let (width, height) = (frame.width(), frame.height());
        let rows = [height as usize, height.div_ceil(2) as usize];
        let mut planes = [Vec::new(), Vec::new()];
        let mut strides = [0; 2];
        for plane in 0..2 {
            let stride = frame.plane_stride()[plane] as usize;
            let data = frame.plane_data(plane as u32).ok()?;
            // Exactly the rows of the plane, not the padding after it.
            planes[plane] = data[..(stride * rows[plane]).min(data.len())].to_vec();
            strides[plane] = stride as u32;
        }
        Some(DecodedFrame {
            width,
            height,
            planes,
            strides,
            colorimetry,
            caps,
            pts_ns,
        })
    }

    /// Decodes a complete sequence of frames, starting with a keyframe, and returns all
    /// decoded frames. Ends the stream so the decoder outputs every frame, then resets
    /// the decoder for the next call. `packets`: (data, pts in ns, keyframe).
    pub fn decode_all(&self, packets: Vec<(Vec<u8>, u64, bool)>) -> Vec<DecodedFrame> {
        self.flush();
        for (i, (data, pts_ns, keyframe)) in packets.into_iter().enumerate() {
            self.push(data, pts_ns, keyframe, i == 0);
        }
        let _ = self.source.end_of_stream();
        let mut frames = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            match self.pull(gst::ClockTime::from_mseconds(20)) {
                Some(frame) => frames.push(frame),
                None if self.sink.is_eos() => break,
                None => {}
            }
        }
        self.flush();
        frames
    }

    /// Discards everything queued or being decoded (before a jump).
    pub fn flush(&self) {
        self.source.send_event(gst::event::FlushStart::new());
        self.source.send_event(gst::event::FlushStop::new(false));
    }

    /// The first error reported by the decoder pipeline, if any.
    pub fn error(&self) -> Option<String> {
        let bus = self.pipeline.bus()?;
        let message = bus.pop_filtered(&[gst::MessageType::Error])?;
        match message.view() {
            gst::MessageView::Error(e) => {
                let source = e
                    .src()
                    .map(|s| s.path_string().to_string())
                    .unwrap_or_default();
                Some(match e.debug() {
                    Some(debug) => format!("{} ({source}: {debug})", e.error()),
                    None => format!("{} ({source})", e.error()),
                })
            }
            _ => None,
        }
    }

    /// Name of the decoder element that `decodebin` chose.
    pub fn decoder_name(&self) -> Option<String> {
        self.pipeline
            .iterate_recurse()
            .into_iter()
            .flatten()
            .find_map(|e| {
                // decodebin is classified as decoder too; the element inside counts.
                if e.is::<gst::Bin>() {
                    return None;
                }
                let factory = e.factory()?;
                factory
                    .has_type(gst::ElementFactoryType::DECODER)
                    .then(|| factory.name().to_string())
            })
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
