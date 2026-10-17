//! Synchronised review of the running recording (AD-6).
//!
//! The [`ReviewPlayer`] has one review position in wall-clock time. Every camera has
//! a [`CameraPlayer`] thread that reads and decodes towards that position and shows
//! the latest frame captured at or before it. Because cameras are matched by capture
//! time, not frame number, cameras with dropped frames, restarts or different frame
//! rates stay in sync.
//!
//! Each camera keeps the last decoded frames in a frame cache, so stepping backwards
//! is immediate within the cache; beyond it, the camera decodes again from the
//! preceding keyframe (AD-4), which refills the cache with that GOP.
//!
//! The players only read the segment files; they have no connection to the recording
//! pipelines, so review can't slow down or interrupt the recording (REV-010).

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gstreamer as gst;
use tracing::{debug, info, warn};
use var_core::color::YuvColorimetry;
use var_core::index::LiveIndex;

use crate::decoder::{DecodedFrame, Decoder};
use crate::source::{CameraSource, FrameRef};

/// Frames are decoded this far ahead of the review position (in recording time), so
/// the decoder has enough input to output the frame at the position, also at 2×.
const LOOKAHEAD_US: i64 = 400_000;
/// At most this many frames are in the decoder at a time.
const MAX_IN_FLIGHT: usize = 16;
/// Decoded frames at or before the position kept for stepping backwards and reverse
/// playback: `PREFETCH_MARGIN` plus one GOP (0.5 s) at 60 fps, plus margin.
const CACHE_FRAMES: usize = 96;
/// Decoded frames after the position kept: the lookahead (24 frames at 60 fps) plus
/// margin. In reverse playback, frames the position has passed are dropped beyond
/// this.
const CACHE_FRAMES_AFTER: usize = 40;
/// When the position is this few frames from the oldest cached frame, the GOP before
/// it is decoded in the background, so stepping backwards and reverse playback don't
/// wait for decoding. About 1.5 GOPs at 60 fps: with the video engine busy encoding,
/// decoding a GOP took up to 1 s on the development machine, while reverse playback
/// at 0.5× uses up 45 frames in 1.5 s.
const PREFETCH_MARGIN: usize = 45;
/// A frame older than this before the review position is not shown (gap in the
/// recording, e.g. a camera failure).
const MAX_FRAME_AGE_US: i64 = 500_000;
/// If the next frame to decode is this far behind the review position, decoding
/// jumps ahead instead of catching up.
const JUMP_US: i64 = 1_000_000;

/// Playback speeds (REV-003).
pub const SPEEDS: [f64; 5] = [2.0, 1.0, 0.5, 0.25, 0.1];
/// Fastest reverse speed (REV-005): reverse playback decodes GOP by GOP backwards.
pub const MAX_REVERSE_SPEED: f64 = 0.5;
/// While playing, the position waits when a camera is behind it (still decoding, e.g.
/// the GOP before the cache in reverse), so frames aren't skipped and the cameras stay
/// together; but at most this long, so a camera that can't keep up doesn't stop
/// playback.
const MAX_WAIT_FOR_CAMERAS: Duration = Duration::from_secs(1);
/// Up to this much of a wait is made up afterwards (less than a frame), so the short
/// waits for a camera to show the next frame don't slow down playback.
const WAIT_MADE_UP: Duration = Duration::from_millis(15);

/// A decoded frame with its capture time.
pub struct ReviewFrame {
    pub decoded: DecodedFrame,
    /// Capture time, µs since the Unix epoch.
    pub wall_us: i64,
}

impl ReviewFrame {
    pub fn width(&self) -> u32 {
        self.decoded.width()
    }
    pub fn height(&self) -> u32 {
        self.decoded.height()
    }
    pub fn plane(&self, index: u32) -> &[u8] {
        self.decoded.plane(index)
    }
    pub fn stride(&self, index: u32) -> u32 {
        self.decoded.stride(index)
    }
    pub fn colorimetry(&self) -> YuvColorimetry {
        self.decoded.colorimetry()
    }
}

/// What a camera shows at the review position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CameraState {
    /// Decoding towards the position.
    Loading,
    Showing,
    /// No video at the position: before the camera's recording started, or a gap
    /// (camera failure, moved segment).
    NoVideo,
    Failed(String),
}

#[derive(Debug, Clone, Copy)]
struct Target {
    wall_us: i64,
    /// Changes on every seek; the player then restarts from a keyframe.
    epoch: u64,
}

/// Wakes the player thread: a counter that changes on every event (new target,
/// decoded frame ready, prefetched GOP ready, stop), so no event is missed between
/// checking for work and going to sleep.
#[derive(Default)]
struct Wake {
    events: Mutex<u64>,
    cond: Condvar,
}

impl Wake {
    fn signal(&self) {
        *self.events.lock().unwrap() += 1;
        self.cond.notify_all();
    }

    fn events(&self) -> u64 {
        *self.events.lock().unwrap()
    }

    /// Sleeps until an event after `seen` (from [`events`](Self::events)), at most
    /// `timeout`.
    fn wait(&self, seen: u64, timeout: Duration) {
        let events = self.events.lock().unwrap();
        let _ = self
            .cond
            .wait_timeout_while(events, timeout, |events| *events == seen);
    }
}

struct Shared {
    target: Mutex<Target>,
    wake: Wake,
    frame: Mutex<Option<Arc<ReviewFrame>>>,
    state: Mutex<CameraState>,
    decoder_name: Mutex<Option<String>>,
    stop: AtomicBool,
    notify: Box<dyn Fn() + Send + Sync>,
}

/// Decodes one camera's recording towards the review position on its own thread.
pub struct CameraPlayer {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    index: Arc<LiveIndex>,
}

impl CameraPlayer {
    pub fn new(
        index: Arc<LiveIndex>,
        wall_us: i64,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let shared = Arc::new(Shared {
            target: Mutex::new(Target { wall_us, epoch: 0 }),
            wake: Wake::default(),
            frame: Mutex::new(None),
            state: Mutex::new(CameraState::Loading),
            decoder_name: Mutex::new(None),
            stop: AtomicBool::new(false),
            notify: Box::new(notify),
        });
        let thread = std::thread::Builder::new()
            .name("var-review-camera".into())
            .spawn({
                let shared = Arc::clone(&shared);
                let source = CameraSource::new(Arc::clone(&index));
                move || run(source, shared)
            })
            .expect("failed to spawn review thread");
        CameraPlayer {
            shared,
            thread: Some(thread),
            index,
        }
    }

    fn set_target(&self, wall_us: i64, epoch: u64) {
        *self.shared.target.lock().unwrap() = Target { wall_us, epoch };
        self.shared.wake.signal();
    }

    /// The frame shown at the review position, if any.
    pub fn frame(&self) -> Option<Arc<ReviewFrame>> {
        self.shared.frame.lock().unwrap().clone()
    }

    pub fn state(&self) -> CameraState {
        self.shared.state.lock().unwrap().clone()
    }

    /// The decoder element in use, once known.
    pub fn decoder_name(&self) -> Option<String> {
        self.shared.decoder_name.lock().unwrap().clone()
    }

    /// Wall-clock times of the first and last frame of this camera's recording.
    pub fn range(&self) -> Option<(i64, i64)> {
        self.source().range()
    }

    /// A reader for this camera's index (frame lookups).
    pub fn source(&self) -> CameraSource {
        CameraSource::new(Arc::clone(&self.index))
    }
}

impl Drop for CameraPlayer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.wake.signal();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn set_state(shared: &Shared, state: CameraState) {
    let mut current = shared.state.lock().unwrap();
    if *current != state {
        *current = state;
        drop(current);
        (shared.notify)();
    }
}

/// Decodes earlier GOPs with its own decoder on its own thread, for stepping
/// backwards and reverse playback without waiting.
struct Prefetcher {
    requests: Option<mpsc::Sender<PrefetchRequest>>,
    results: mpsc::Receiver<PrefetchResult>,
    thread: Option<JoinHandle<()>>,
    /// Cache generation of the request being decoded.
    pending: Option<u64>,
}

struct PrefetchRequest {
    generation: u64,
    /// (data, pts in ns, keyframe), starting with a keyframe.
    packets: Vec<(Vec<u8>, u64, bool)>,
    /// Capture time per timestamp.
    walls: BTreeMap<u64, i64>,
}

struct PrefetchResult {
    generation: u64,
    frames: Vec<Arc<ReviewFrame>>,
}

impl Prefetcher {
    /// `on_result` is called when a decoded GOP is ready in `results`.
    fn new(on_result: impl Fn() + Send + 'static) -> Self {
        let (request_tx, request_rx) = mpsc::channel::<PrefetchRequest>();
        let (result_tx, result_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("var-review-prefetch".into())
            .spawn(move || {
                let Ok(decoder) = Decoder::new() else {
                    warn!("cannot create prefetch decoder; stepping back decodes on demand");
                    return;
                };
                // Ends when the player drops the request sender.
                while let Ok(request) = request_rx.recv() {
                    let started = Instant::now();
                    let frames: Vec<Arc<ReviewFrame>> = decoder
                        .decode_all(request.packets)
                        .into_iter()
                        .filter_map(|decoded| {
                            let wall_us = *request.walls.get(&decoded.pts_ns)?;
                            Some(Arc::new(ReviewFrame { decoded, wall_us }))
                        })
                        .collect();
                    debug!(
                        frames = frames.len(),
                        ms = started.elapsed().as_millis(),
                        "GOP prefetched"
                    );
                    let result = PrefetchResult {
                        generation: request.generation,
                        frames,
                    };
                    if result_tx.send(result).is_err() {
                        break;
                    }
                    on_result();
                }
            })
            .expect("failed to spawn prefetch thread");
        Prefetcher {
            requests: Some(request_tx),
            results: result_rx,
            thread: Some(thread),
            pending: None,
        }
    }
}

impl Drop for Prefetcher {
    fn drop(&mut self) {
        self.requests = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The player thread: feed the decoder towards the target, collect decoded frames
/// into the cache, publish the frame at the target.
fn run(mut source: CameraSource, shared_arc: Arc<Shared>) {
    let shared = &*shared_arc;
    let wake = {
        let shared = Arc::clone(&shared_arc);
        move || shared.wake.signal()
    };
    // The decoder and the prefetcher wake the thread when they have frames.
    let mut decoder = match Decoder::with_notify(wake.clone()) {
        Ok(decoder) => decoder,
        Err(e) => {
            warn!(%e, "cannot create review decoder");
            set_state(shared, CameraState::Failed(e.to_string()));
            return;
        }
    };
    let frame_us = source.frame_us();
    let max_age = MAX_FRAME_AGE_US.max(2 * frame_us);
    let mut epoch = u64::MAX;
    // Next frame to feed, and the last frame fed (to continue when the index grows).
    let mut next: Option<FrameRef> = None;
    let mut last_fed: Option<FrameRef> = None;
    let mut discont = true;
    // Capture time per timestamp of the frames in the decoder.
    let mut in_flight: BTreeMap<u64, i64> = BTreeMap::new();
    // Frame cache: decoded frames in capture order.
    let mut cache: VecDeque<Arc<ReviewFrame>> = VecDeque::new();
    let mut shown: Option<i64> = None;
    // The frame a re-decode was started for, so a frame that can't be decoded doesn't
    // cause endless re-decoding.
    let mut redecoded_for: Option<FrameRef> = None;
    let mut prefetcher = Prefetcher::new(wake.clone());
    // Changes whenever the cache is cleared; prefetch results for an older cache are
    // discarded.
    let mut generation: u64 = 0;
    // Oldest cached frame for which prefetching was tried and gave nothing.
    let mut prefetch_failed_at: Option<i64> = None;
    // Decoding restarted from a keyframe and hasn't reached the position yet: the
    // frames on the way are not shown, so a seek doesn't flash the keyframe and run
    // up to the position.
    let mut catching_up = false;

    while !shared.stop.load(Ordering::Relaxed) {
        // Before reading the target: anything that happens from here on wakes the
        // wait at the end of the round.
        let events_seen = shared.wake.events();
        let target = *shared.target.lock().unwrap();
        let wanted = source.frame_at(target.wall_us);
        let wanted_wall = wanted.and_then(|f| source.record(f)).map(|r| r.wall_us);

        // Decode again from a keyframe: on a seek; when the position ran far ahead of
        // decoding; or when the frame at the position was already decoded but is no
        // longer in the cache (stepping or jumping backwards).
        let behind = next
            .and_then(|n| source.record(n))
            .is_some_and(|r| r.wall_us < target.wall_us - JUMP_US);
        let evicted = match (wanted, wanted_wall) {
            (Some(frame), Some(wall)) => {
                !source.is_missing(frame.segment)
                    && last_fed.is_some_and(|last| last >= frame)
                    && !cache.iter().any(|f| f.wall_us == wall)
                    && !in_flight.values().any(|&w| w == wall)
                    && redecoded_for != Some(frame)
                    // The GOP may be on its way from the prefetcher.
                    && prefetcher.pending != Some(generation)
            }
            _ => false,
        };
        if target.epoch != epoch || behind || evicted {
            epoch = target.epoch;
            source.close_file();
            decoder.flush();
            in_flight.clear();
            cache.clear();
            generation += 1;
            discont = true;
            next = wanted
                .map(|f| source.keyframe_for(f))
                .or_else(|| source.first());
            last_fed = None;
            redecoded_for = wanted;
            catching_up = true;
            debug!(?next, evicted, "review decodes from keyframe");
        }
        if next.is_none()
            && let Some(last) = last_fed
        {
            next = source.next(last);
        }

        // Feed.
        while in_flight.len() < MAX_IN_FLIGHT
            && let Some(frame) = next
        {
            let Some(record) = source.record(frame) else {
                break;
            };
            if record.wall_us > target.wall_us + LOOKAHEAD_US {
                break;
            }
            match source.read(frame) {
                Ok(packet) => {
                    let pts_ns = packet.record.pts90 * 100_000 / 9;
                    in_flight.insert(pts_ns, packet.record.wall_us);
                    decoder.push(packet.data, pts_ns, packet.record.keyframe, discont);
                    discont = false;
                    last_fed = Some(frame);
                    next = source.next(frame);
                }
                Err(e) => {
                    // Segment moved or unreadable: continue with the next one.
                    if !source.is_missing(frame.segment) {
                        warn!(%e, segment = frame.segment, "cannot read segment for review");
                    }
                    last_fed = None;
                    next = source.next_segment(frame);
                    discont = true;
                }
            }
        }

        // Collect decoded frames into the cache. No waiting here: the decoder wakes
        // the wait at the end of the round when it has a frame.
        let mut pulled = false;
        while let Some(frame) = decoder.pull(gst::ClockTime::ZERO) {
            pulled = true;
            if shared.decoder_name.lock().unwrap().is_none() {
                let name = decoder.decoder_name();
                if let Some(name) = &name {
                    info!(decoder = %name, "review decoder");
                }
                *shared.decoder_name.lock().unwrap() = name;
            }
            // Frames from before a flush are not in the map any more.
            let Some(wall_us) = in_flight.remove(&frame.pts_ns) else {
                continue;
            };
            // Older entries were dropped by the decoder.
            in_flight.retain(|&pts, _| pts > frame.pts_ns);
            cache.push_back(Arc::new(ReviewFrame {
                decoded: frame,
                wall_us,
            }));
        }
        if let Some(e) = decoder.error() {
            // A decoder pipeline that posted an error may not recover by flushing:
            // replace it, and decode again from a keyframe.
            warn!(%e, "review decoder error, decoder recreated");
            match Decoder::with_notify(wake.clone()) {
                Ok(new) => decoder = new,
                Err(e) => {
                    warn!(%e, "cannot recreate review decoder");
                    set_state(shared, CameraState::Failed(e.to_string()));
                    decoder.flush();
                }
            }
            epoch = u64::MAX;
            // Don't retry faster than this if the error repeats.
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }

        // Earlier GOP decoded in the background: in front of the cache.
        while let Ok(result) = prefetcher.results.try_recv() {
            prefetcher.pending = None;
            if result.generation != generation {
                continue;
            }
            let front = cache.front().map(|f| f.wall_us);
            if result.frames.is_empty() {
                prefetch_failed_at = front;
            }
            for frame in result.frames.into_iter().rev() {
                if front.is_none_or(|front| frame.wall_us < front) {
                    cache.push_front(frame);
                }
            }
        }

        // Keep at most CACHE_FRAMES frames at or before the position and
        // CACHE_FRAMES_AFTER after it. The target read at the top is used throughout:
        // a seek in between is handled in the next round, after the cache was cleared.
        let target = target.wall_us;
        let at_or_before = cache.partition_point(|f| f.wall_us <= target);
        for _ in CACHE_FRAMES..at_or_before {
            cache.pop_front();
        }
        let at_or_before = cache.partition_point(|f| f.wall_us <= target);
        cache.truncate(at_or_before + CACHE_FRAMES_AFTER);

        // Near the oldest cached frame: decode the GOP before it in the background.
        let at_or_before = cache.partition_point(|f| f.wall_us <= target);
        if prefetcher.pending.is_none()
            && at_or_before > 0
            && at_or_before <= PREFETCH_MARGIN
            && let Some(front) = cache.front().map(|f| f.wall_us)
            && prefetch_failed_at != Some(front)
            && let Some(request) = prefetch_request(&mut source, front, generation)
            && let Some(requests) = &prefetcher.requests
            && requests.send(request).is_ok()
        {
            debug!(at_or_before, "GOP before the cache requested");
            prefetcher.pending = Some(generation);
        }

        // Publish the latest cached frame at or before the position.
        let at_or_before = cache.partition_point(|f| f.wall_us <= target);
        let current = at_or_before.checked_sub(1).map(|i| Arc::clone(&cache[i]));
        let mut publish = true;
        if catching_up {
            // Reached: the frame at the position; or, if that can't be decoded, a
            // later one; or nothing more to decode up to the position (e.g. the
            // segment was moved away).
            let nothing_pending = in_flight.is_empty()
                && next
                    .and_then(|n| source.record(n))
                    .is_none_or(|r| r.wall_us > target);
            let reached = match wanted_wall {
                Some(wanted) => {
                    current.as_ref().is_some_and(|f| f.wall_us == wanted)
                        || cache.back().is_some_and(|f| f.wall_us > wanted)
                        || nothing_pending
                }
                None => true,
            };
            if reached {
                catching_up = false;
            } else {
                publish = false;
            }
        }
        match current {
            _ if !publish => {}
            Some(frame) if target - frame.wall_us <= max_age => {
                if shown != Some(frame.wall_us) {
                    shown = Some(frame.wall_us);
                    *shared.frame.lock().unwrap() = Some(frame);
                    set_state(shared, CameraState::Showing);
                    (shared.notify)();
                }
            }
            _ => {
                // Nothing at the position yet: still decoding towards it, or no video.
                let at_target = source.frame_at(target);
                let has_video = at_target
                    .and_then(|f| source.record(f))
                    .is_some_and(|r| target - r.wall_us <= max_age)
                    && at_target.is_some_and(|f| !source.is_missing(f.segment));
                if !has_video {
                    if shown.take().is_some() {
                        *shared.frame.lock().unwrap() = None;
                    }
                    set_state(shared, CameraState::NoVideo);
                } else if shown.is_none() {
                    set_state(shared, CameraState::Loading);
                }
            }
        }

        // Nothing came out of the decoder this round: sleep until something happens
        // (new position, decoded frame, prefetched GOP, stop); at most 10 ms, for
        // newly recorded frames at the live edge.
        if !pulled {
            shared.wake.wait(events_seen, Duration::from_millis(10));
        }
    }
}

/// Reads the GOP before the frame captured at `front_wall` for prefetching.
fn prefetch_request(
    source: &mut CameraSource,
    front_wall: i64,
    generation: u64,
) -> Option<PrefetchRequest> {
    let front = source.frame_at(front_wall)?;
    let last = source.prev(front)?;
    if source.is_missing(last.segment) {
        return None;
    }
    let mut frame = source.keyframe_for(last);
    let mut packets = Vec::new();
    let mut walls = BTreeMap::new();
    loop {
        let packet = source.read(frame).ok()?;
        let pts_ns = packet.record.pts90 * 100_000 / 9;
        walls.insert(pts_ns, packet.record.wall_us);
        packets.push((packet.data, pts_ns, packet.record.keyframe));
        if frame == last {
            break;
        }
        frame = source.next(frame)?;
    }
    Some(PrefetchRequest {
        generation,
        packets,
        walls,
    })
}

/// Review of the running recording: one position for all cameras.
pub struct ReviewPlayer {
    cameras: Vec<(usize, CameraPlayer)>,
    position_us: i64,
    playing: bool,
    /// Playing backwards (REV-005); only while playing.
    reverse: bool,
    speed: f64,
    epoch: u64,
    last_tick: Instant,
    /// Since when playback waits for a camera that is behind.
    waiting_since: Option<Instant>,
    /// Time spent waiting for cameras and number of waits (diagnostics).
    waited: (Duration, u32),
}

impl ReviewPlayer {
    /// Starts review of the cameras' recordings (`(slot, live index)`), paused at
    /// `start_us` (clamped to the recording).
    pub fn new(
        sources: Vec<(usize, Arc<LiveIndex>)>,
        start_us: i64,
        notify: impl Fn() + Clone + Send + Sync + 'static,
    ) -> Self {
        let cameras = sources
            .into_iter()
            .map(|(slot, index)| (slot, CameraPlayer::new(index, start_us, notify.clone())))
            .collect();
        let mut player = ReviewPlayer {
            cameras,
            position_us: start_us,
            playing: false,
            reverse: false,
            speed: 1.0,
            epoch: 0,
            last_tick: Instant::now(),
            waiting_since: None,
            waited: (Duration::ZERO, 0),
        };
        player.seek(start_us);
        player
    }

    /// Wall-clock range of the recording over all cameras: first frame to the latest
    /// frame available for review (the live edge).
    pub fn range(&self) -> Option<(i64, i64)> {
        self.cameras
            .iter()
            .filter_map(|(_, c)| c.range())
            .reduce(|(a0, a1), (b0, b1)| (a0.min(b0), a1.max(b1)))
    }

    pub fn position(&self) -> i64 {
        self.position_us
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Total time playback waited for cameras that were behind, and the number of
    /// waits, since the start (diagnostics).
    pub fn waited(&self) -> (Duration, u32) {
        self.waited
    }

    /// Playing backwards (REV-005).
    pub fn is_reverse(&self) -> bool {
        self.playing && self.reverse
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    /// Sets the playback speed; one of [`SPEEDS`] (REV-003). A speed above
    /// [`MAX_REVERSE_SPEED`] while playing backwards continues forwards.
    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed;
        if speed > MAX_REVERSE_SPEED {
            self.reverse = false;
        }
    }

    /// Next faster (`faster`) or slower speed of [`SPEEDS`]; in reverse at most
    /// [`MAX_REVERSE_SPEED`].
    pub fn change_speed(&mut self, faster: bool) {
        let index = SPEEDS.iter().position(|&s| s == self.speed).unwrap_or(1);
        let index = if faster {
            index.saturating_sub(1)
        } else {
            (index + 1).min(SPEEDS.len() - 1)
        };
        let speed = SPEEDS[index];
        if !(self.is_reverse() && speed > MAX_REVERSE_SPEED) {
            self.speed = speed;
        }
    }

    /// Jumps to a wall-clock time and restarts decoding there (scrub bar, markers).
    pub fn seek(&mut self, wall_us: i64) {
        self.position_us = self.clamp(wall_us);
        self.epoch += 1;
        self.update_targets();
    }

    /// Moves the position without restarting decoding: the cameras use their frame
    /// cache and decode from a keyframe only when needed (frame steps, short jumps).
    pub fn move_to(&mut self, wall_us: i64) {
        self.position_us = self.clamp(wall_us);
        self.update_targets();
    }

    /// Jumps forward (positive) or backward by `seconds` (REV-006).
    pub fn jump(&mut self, seconds: i64) {
        let target = self.position_us.saturating_add(seconds * 1_000_000);
        if seconds.abs() > 1 {
            self.seek(target);
        } else {
            self.move_to(target);
        }
    }

    /// Steps `frames` frames forward (positive) or backward, and pauses (REV-004).
    /// Steps follow the frames of `reference` (a camera slot; default: the first
    /// camera): the position lands exactly on that camera's frames, so all cameras
    /// with the same frame rate move by exactly one frame per step.
    pub fn step(&mut self, frames: i32, reference: Option<usize>) {
        self.pause();
        let camera = reference
            .and_then(|slot| self.camera(slot))
            .or_else(|| self.cameras.first().map(|(_, c)| c));
        let Some(camera) = camera else { return };
        let source = camera.source();
        let mut frame = match source.frame_at(self.position_us) {
            Some(frame) => frame,
            None if frames > 0 => match source.first() {
                // Before this camera's first frame: the first step lands on it.
                Some(first) => {
                    if let Some(record) = source.record(first) {
                        self.move_to(record.wall_us);
                    }
                    return;
                }
                None => return,
            },
            None => return,
        };
        for _ in 0..frames.unsigned_abs() {
            let moved = if frames > 0 {
                source.next(frame)
            } else {
                source.prev(frame)
            };
            match moved {
                Some(f) => frame = f,
                None => break,
            }
        }
        if let Some(record) = source.record(frame) {
            self.move_to(record.wall_us);
        }
    }

    pub fn play(&mut self) {
        self.playing = true;
        self.reverse = false;
        self.last_tick = Instant::now();
    }

    /// Plays backwards (REV-005), at the set speed but at most [`MAX_REVERSE_SPEED`].
    pub fn play_reverse(&mut self) {
        self.speed = self.speed.min(MAX_REVERSE_SPEED);
        self.playing = true;
        self.reverse = true;
        self.last_tick = Instant::now();
    }

    pub fn pause(&mut self) {
        self.playing = false;
        self.reverse = false;
    }

    /// Play forwards / pause. Playing backwards, it switches to forwards.
    pub fn toggle(&mut self) {
        if self.playing && !self.reverse {
            self.pause()
        } else {
            self.play()
        }
    }

    /// Play backwards / pause. Playing forwards, it switches to backwards.
    pub fn toggle_reverse(&mut self) {
        if self.is_reverse() {
            self.pause()
        } else {
            self.play_reverse()
        }
    }

    /// Advances the position while playing, at the set speed. At the live edge it
    /// stays at the latest available frame; playing backwards stops at the start of
    /// the recording. Call once per UI frame.
    pub fn tick(&mut self) {
        let now = Instant::now();
        if self.playing && self.cameras_behind() {
            let since = *self.waiting_since.get_or_insert_with(|| {
                for (slot, camera) in &self.cameras {
                    debug!(
                        slot,
                        state = ?camera.state(),
                        lag_ms = camera.frame().map(|f| (self.position_us - f.wall_us) / 1000),
                        "playback waits for cameras"
                    );
                }
                now
            });
            if now.duration_since(since) < MAX_WAIT_FOR_CAMERAS {
                // Only the last WAIT_MADE_UP of the wait counts as playing time.
                self.last_tick = self.last_tick.max(now - WAIT_MADE_UP);
                return;
            }
        } else if let Some(since) = self.waiting_since.take() {
            self.waited.0 += now.duration_since(since);
            self.waited.1 += 1;
        }
        let elapsed = now.duration_since(self.last_tick);
        self.last_tick = now;
        let advanced = if self.playing {
            let step = (elapsed.as_micros() as f64 * self.speed).round() as i64;
            if self.reverse {
                self.position_us.saturating_sub(step)
            } else {
                self.position_us.saturating_add(step)
            }
        } else {
            self.position_us
        };
        if self.reverse
            && let Some((start, _)) = self.range()
            && advanced <= start
        {
            self.pause();
        }
        // Also while paused: a position set before the first frame was available is
        // moved into the recording as soon as there is one.
        let position = self.clamp(advanced);
        if position != self.position_us {
            self.position_us = position;
            self.update_targets();
        }
    }

    /// Whether a camera is still decoding towards the position: no frame yet; playing
    /// backwards, its frame is after the position; playing forwards, its frame is
    /// more than two frames before the position (decoding normally lags about a
    /// frame). Cameras without video at the position (gaps) don't count.
    fn cameras_behind(&self) -> bool {
        self.cameras.iter().any(|(_, camera)| match camera.state() {
            CameraState::Loading => true,
            CameraState::Showing => camera.frame().is_some_and(|frame| {
                if self.reverse {
                    frame.wall_us > self.position_us
                } else {
                    self.position_us - frame.wall_us > 2 * camera.source().frame_us()
                }
            }),
            CameraState::NoVideo | CameraState::Failed(_) => false,
        })
    }

    fn update_targets(&self) {
        for (_, camera) in &self.cameras {
            camera.set_target(self.position_us, self.epoch);
        }
    }

    fn clamp(&self, wall_us: i64) -> i64 {
        match self.range() {
            Some((start, end)) => wall_us.clamp(start, end),
            None => wall_us,
        }
    }

    /// The player of a camera slot; `None` if the slot was not recorded.
    pub fn camera(&self, slot: usize) -> Option<&CameraPlayer> {
        self.cameras
            .iter()
            .find(|(s, _)| *s == slot)
            .map(|(_, c)| c)
    }
}
