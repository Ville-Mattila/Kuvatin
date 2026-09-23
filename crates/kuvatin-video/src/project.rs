//! GES-backed editing project: a timeline of layers + clips with a composited
//! preview. The GUI's timeline UI drives this; GES handles compositing,
//! transforms, trims, and seeking (and, later, render-to-file for export).

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use ges::prelude::*;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSinkCallbacks};
use gstreamer_editing_services as ges;
use gstreamer_pbutils as gst_pbutils;

use crate::Frame;

/// A handle to a clip on the timeline (its GES clip name).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ClipId(pub String);

/// Where a clip ended up on the timeline (returned after placing it).
#[derive(Clone, Debug)]
pub struct ClipInfo {
    pub id: ClipId,
    pub track: usize,
    pub start: Duration,
    pub duration: Duration,
}

/// A clip's current timeline geometry (returned after a slide/trim clamps it).
#[derive(Clone, Copy, Debug)]
pub struct ClipGeom {
    pub start: Duration,
    pub inpoint: Duration,
    pub duration: Duration,
}

/// Pre-load (discover) a media file into the GES asset cache. Safe to call off
/// the UI thread; a subsequent `add_clip`/`append_clip` then hits the warm cache
/// and returns immediately instead of blocking the UI on discovery.
pub fn warm_asset(path: &Path) -> Result<()> {
    let uri = gst::glib::filename_to_uri(path, None)?;
    warm_asset_uri(&uri)
}

/// URI form of [`warm_asset`], for sources that aren't a single file (image
/// sequences use the `imagesequence://` scheme).
pub fn warm_asset_uri(uri: &str) -> Result<()> {
    gst::init()?;
    ges::init()?;
    ensure_encoder_ranks();
    ensure_discovery_timeout();
    let _ = ges::UriClipAsset::request_sync(uri)?;
    Ok(())
}

/// The first video encoder in `bin`, by element name. GStreamer's klass
/// strings mark it: "Codec/Encoder/Video" (some add "/Hardware").
fn encoder_in(bin: &gst::Bin) -> Option<String> {
    for e in bin.iterate_elements().into_iter().flatten() {
        if let Some(f) = e.factory() {
            let klass = f.klass().to_string();
            if klass.contains("Encoder") && klass.contains("Video") {
                return Some(f.name().to_string());
            }
        }
        if let Some(b) = e.dynamic_cast_ref::<gst::Bin>() {
            if let Some(found) = encoder_in(b) {
                return Some(found);
            }
        }
    }
    None
}

/// Recursively find the first element in `bin` created by the named factory.
fn find_by_factory(bin: &gst::Bin, factory: &str) -> Option<gst::Element> {
    for e in bin.iterate_elements().into_iter().flatten() {
        if e.factory().map(|f| f.name().to_string()).as_deref() == Some(factory) {
            return Some(e);
        }
        if let Some(b) = e.dynamic_cast_ref::<gst::Bin>() {
            if let Some(found) = find_by_factory(b, factory) {
                return Some(found);
            }
        }
    }
    None
}

/// A borrowed RGBA video frame straight from the mapped GStreamer buffer.
/// Rows are `stride` bytes apart (GPU-download paths pad them; VideoMeta),
/// of which the first `width * 4` are pixels. The preview callback receives
/// this so the GUI copies the pixels ONCE, into the buffer it will display —
/// a 4K canvas is 33 MB per frame, and the old owned `Frame` cost a second
/// copy of that on the UI thread.
pub struct FrameView<'a> {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub data: &'a [u8],
}

impl FrameView<'_> {
    /// Copy the pixels, tightly packed, into `dst` (`width * height * 4`
    /// bytes; extra bytes are left alone).
    pub fn copy_packed_into(&self, dst: &mut [u8]) {
        let row_bytes = self.width as usize * 4;
        let rows = self.height as usize;
        if self.stride == row_bytes {
            let n = row_bytes * rows;
            dst[..n].copy_from_slice(&self.data[..n]);
        } else {
            for row in 0..rows {
                let src = row * self.stride;
                dst[row * row_bytes..(row + 1) * row_bytes]
                    .copy_from_slice(&self.data[src..src + row_bytes]);
            }
        }
    }

    /// An owned, tightly packed copy.
    pub fn to_frame(&self) -> Frame {
        let mut rgba = vec![0u8; self.width as usize * self.height as usize * 4];
        self.copy_packed_into(&mut rgba);
        Frame {
            width: self.width,
            height: self.height,
            rgba,
        }
    }
}

/// Map a sample's video buffer and hand it to `f` as a [`FrameView`].
fn with_frame_view<R>(sample: &gst::Sample, f: impl FnOnce(FrameView<'_>) -> R) -> Option<R> {
    use gstreamer_video::VideoFrameExt;
    let caps = sample.caps()?;
    let info = gstreamer_video::VideoInfo::from_caps(caps).ok()?;
    let buffer = sample.buffer_owned()?;
    let vframe = gstreamer_video::VideoFrame::from_buffer_readable(buffer, &info).ok()?;
    let stride = vframe.plane_stride()[0] as usize;
    let data = vframe.plane_data(0).ok()?;
    let (width, height) = (info.width(), info.height());
    if data.len() < stride * (height as usize - 1) + width as usize * 4 {
        return None;
    }
    Some(f(FrameView {
        width,
        height,
        stride,
        data,
    }))
}

/// Extract an owned RGBA `Frame` from a GStreamer sample (thumbnails).
fn sample_to_frame(sample: &gst::Sample) -> Option<Frame> {
    with_frame_view(sample, |v| v.to_frame())
}

/// A frame's length until the preview has shown one: 1/25 s.
const DEFAULT_FRAME_NS: u64 = 40_000_000;

/// The shortest buffer taken as a frame. The composited preview stamps a few
/// buffers with a duration of 1 ns around a flushing seek (measured, M10); a
/// "frame" that short would make a frame step invisible.
const MIN_FRAME_NS: u64 = 1_000_000;

/// The frame length to keep once a buffer lasting `buffer_ns` has arrived: a
/// buffer with no duration, or one too short to be a frame, leaves the last
/// good length in place.
fn next_frame_ns(last_ns: u64, buffer_ns: Option<u64>) -> u64 {
    match buffer_ns {
        Some(ns) if ns >= MIN_FRAME_NS => ns,
        _ => last_ns,
    }
}

/// Push one RGBA video sample to the frame callback, noting how long it lasts.
fn emit_sample(
    sample: &gst::Sample,
    cb: &(dyn Fn(FrameView<'_>) + Send + Sync),
    frame_ns: &AtomicU64,
) -> std::result::Result<gst::FlowSuccess, gst::FlowError> {
    let lasts = sample
        .buffer()
        .and_then(|b| b.duration())
        .map(|d| d.nseconds());
    frame_ns.store(
        next_frame_ns(frame_ns.load(Ordering::Relaxed), lasts),
        Ordering::Relaxed,
    );
    with_frame_view(sample, cb).ok_or(gst::FlowError::Error)?;
    Ok(gst::FlowSuccess::Ok)
}

/// Grab a representative thumbnail frame (RGBA, ~`width`px wide at the source's
/// natural aspect) for a media file. Safe to call off the UI thread; None on
/// failure. Seeks a little in to skip black intro frames.
pub fn thumbnail(path: &Path, width: u32) -> Option<Frame> {
    let uri = gst::glib::filename_to_uri(path, None).ok()?;
    thumbnail_uri(&uri, width)
}

/// URI form of [`thumbnail`] (image sequences use `imagesequence://`).
pub fn thumbnail_uri(uri: &str, width: u32) -> Option<Frame> {
    gst::init().ok()?;
    ensure_encoder_ranks();
    let pipeline = gst::Pipeline::new();
    let src = gst::ElementFactory::make("uridecodebin")
        .property("uri", uri)
        .build()
        .ok()?;
    let convert = gst::ElementFactory::make("videoconvert").build().ok()?;
    let scale = gst::ElementFactory::make("videoscale").build().ok()?;
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGBA")
        .field("width", width as i32)
        .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
        .build();
    let sink = AppSink::builder().caps(&caps).build();
    pipeline
        .add_many([&src, &convert, &scale, sink.upcast_ref::<gst::Element>()])
        .ok()?;
    gst::Element::link_many([&convert, &scale, sink.upcast_ref::<gst::Element>()]).ok()?;
    // uridecodebin exposes decoded pads dynamically — link only the video one.
    let convert_weak = convert.downgrade();
    src.connect_pad_added(move |_, pad| {
        let Some(convert) = convert_weak.upgrade() else {
            return;
        };
        let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
        let is_video = caps
            .structure(0)
            .map(|s| s.name().starts_with("video/"))
            .unwrap_or(false);
        if !is_video {
            return;
        }
        if let Some(sinkpad) = convert.static_pad("sink") {
            if !sinkpad.is_linked() {
                let _ = pad.link(&sinkpad);
            }
        }
    });
    if pipeline.set_state(gst::State::Paused).is_err() {
        // Partially-started elements/threads would outlive the call otherwise.
        let _ = pipeline.set_state(gst::State::Null);
        return None;
    }
    // A timed-out state change comes back as Ok(Async), not Err — treating it
    // as success used to fall through to an UNBOUNDED pull_preroll, which
    // stalled the import worker (and every import behind it) forever.
    let settled = |timeout: u64| {
        matches!(
            pipeline.state(gst::ClockTime::from_seconds(timeout)).0,
            Ok(gst::StateChangeSuccess::Success) | Ok(gst::StateChangeSuccess::NoPreroll)
        )
    };
    if !settled(5) {
        let _ = pipeline.set_state(gst::State::Null);
        return None;
    }
    if let Some(dur) = pipeline.query_duration::<gst::ClockTime>() {
        let target = (dur.nseconds() / 2).min(gst::ClockTime::from_seconds(1).nseconds());
        if target > 0 {
            let _ = pipeline.seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                gst::ClockTime::from_nseconds(target),
            );
            if !settled(3) {
                let _ = pipeline.set_state(gst::State::Null);
                return None;
            }
        }
    }
    let frame = sink
        .try_pull_preroll(gst::ClockTime::from_seconds(5))
        .and_then(|s| sample_to_frame(&s));
    let _ = pipeline.set_state(gst::State::Null);
    frame
}

/// Clips may never be shorter than this (0.2 s) via edge-trimming.
const MIN_TRIM_NS: i128 = 200_000_000;

/// Left-edge trim math: the clip's END stays fixed; start moves by the
/// (clamped) delta, the in-point by `rate` times as much (one second of
/// timeline is `rate` seconds of source), and the duration shrinks or grows
/// to match. All results are guaranteed non-negative — the previous version
/// applied the min-duration override AFTER the >=0 clamps, so a clip already
/// shorter than the minimum could push inpoint/start negative and wrap
/// through the u64 cast into a ~10^5-hour ClockTime. Precedence here:
/// never-negative beats min-duration. The in-point is rounded down, so the
/// source position of the fixed end never moves past where it was.
fn trim_left_math(
    start: i128,
    inpoint: i128,
    dur: i128,
    delta: i128,
    rate: f64,
) -> (i128, i128, i128) {
    let mut d = delta;
    d = d.min(dur - MIN_TRIM_NS); // keep at least the minimum (may go negative)
                                  // Never before the source origin (in timeline time), nor the timeline's.
    d = d.max(-((inpoint as f64 / rate) as i128)).max(-start);
    let di = (d as f64 * rate).floor() as i128;
    ((start + d).max(0), (inpoint + di).max(0), (dur - d).max(0))
}

/// Right-edge trim math: only the duration changes. Clamped to the minimum,
/// then capped by what the source can still supply from `inpoint` at `rate`
/// (which wins over the minimum when the two conflict, e.g. `inpoint` near
/// the end of the media), floored at 0.
fn trim_right_math(inpoint: i128, dur: i128, delta: i128, max_ns: Option<i128>, rate: f64) -> i128 {
    let mut nd = (dur + delta).max(MIN_TRIM_NS);
    if let Some(m) = max_ns {
        nd = nd.min(((m - inpoint) as f64 / rate) as i128);
    }
    nd.max(0)
}

/// Whether a clip at `start` lasting `dur` can be cut at `at`, all in
/// nanoseconds: only where both halves keep at least [`MIN_TRIM_NS`].
fn split_fits(start: i128, dur: i128, at: i128) -> bool {
    at >= start + MIN_TRIM_NS && at <= start + dur - MIN_TRIM_NS
}

/// The slowest and the fastest a clip plays (see [`Project::set_clip_rate`]).
pub const RATE_MIN: f64 = 0.25;
pub const RATE_MAX: f64 = 4.0;

/// Which of `pitch`'s properties carries a clip's speed. `rate` changes speed
/// and pitch together, as `videorate` does the picture; `tempo` would keep
/// the pitch. GES knows both as time properties (M1, M11), so choosing the
/// other is this one line.
const PITCH_PROPERTY: &str = "rate";

/// A rate as the engine keeps it: clamped to [`RATE_MIN`, `RATE_MAX`] and
/// rounded to the nearest `f32`. `pitch` stores its rate as a float (M3), and
/// undo compares records exactly, so a rate that did not survive that trip
/// would never read back as the record it came from.
fn snap_rate(rate: f64) -> f64 {
    f64::from(rate.clamp(RATE_MIN, RATE_MAX) as f32)
}

/// How long a clip lasts at `new_rate`, from `dur` at `old_rate`: the same
/// span of source, played at the new rate. Then, as a right-edge trim would
/// be: at least the trim minimum, never more than the source can still supply
/// from `inpoint` at the new rate (which wins over the minimum where they
/// conflict), and never more than `room`, the space before the next clip.
fn rate_change_math(
    inpoint: i128,
    dur: i128,
    old_rate: f64,
    new_rate: f64,
    max_ns: Option<i128>,
    room: Option<i128>,
) -> i128 {
    let mut nd = ((dur as f64 * old_rate / new_rate).round() as i128).max(MIN_TRIM_NS);
    if let Some(m) = max_ns {
        nd = nd.min(((m - inpoint) as f64 / new_rate) as i128);
    }
    if let Some(r) = room {
        nd = nd.min(r);
    }
    nd.max(0)
}

/// The room a clip at `start` lasting `dur` has to grow into: from its start
/// to the nearest neighbour that begins at or after its end. `neighbours` are
/// the other clips on its layer as `(start, end)`. None when nothing follows.
fn room_after(start: i128, dur: i128, neighbours: &[(i128, i128)]) -> Option<i128> {
    let end = start + dur;
    neighbours
        .iter()
        .filter(|&&(n_start, _)| n_start >= end)
        .map(|&(n_start, _)| n_start - start)
        .min()
}

/// Read a GES clip's current timeline geometry.
/// Where a slid clip may actually land on its layer.
///
/// GES stacks whatever it is told to stack: drop one clip onto another on the
/// same layer and the later one simply hides the earlier, with nothing on
/// screen to say so. A clip therefore stays inside the gap it already occupies
/// — it can butt up against a neighbour on either side, and no further.
///
/// `neighbours` is every OTHER clip on the layer as `(start, end)` in
/// nanoseconds, in any order. A clip that is already overlapping something
/// (a project made before this rule) is not frozen in place: it just gets the
/// old clamp at zero, so it can be dragged out of the mess.
fn slide_within_gap(start: i128, dur: i128, delta: i128, neighbours: &[(i128, i128)]) -> i128 {
    let desired = (start + delta).max(0);
    let end = start + dur;
    // The gap around the clip's current position: the nearest neighbour edge
    // on each side. Anything already overlapping it is not a boundary — it is
    // the mess the user is trying to drag out of.
    let mut floor = 0i128;
    let mut ceil = i128::MAX;
    for &(n_start, n_end) in neighbours {
        if n_end <= start {
            floor = floor.max(n_end);
        } else if n_start >= end {
            ceil = ceil.min(n_start);
        }
    }
    if ceil == i128::MAX {
        return desired.max(floor);
    }
    // A gap too small to hold the clip leaves it exactly where it was.
    desired.clamp(floor, (ceil - dur).max(floor))
}

fn clip_geom(clip: &ges::Clip) -> ClipGeom {
    ClipGeom {
        start: Duration::from_nanos(clip.start().nseconds()),
        inpoint: Duration::from_nanos(clip.inpoint().nseconds()),
        duration: Duration::from_nanos(clip.duration().nseconds()),
    }
}

/// Seconds as a record stores them, back to the nanoseconds they came from.
/// Exact for any timeline shorter than about 26 days.
fn clock_time(secs: f64) -> gst::ClockTime {
    gst::ClockTime::from_nseconds((secs.max(0.0) * 1e9).round() as u64)
}

/// Whether a clip already sits where `record` puts it: its times to the
/// nanosecond, its track, and its speed.
fn clip_placed_as(clip: &ges::Clip, record: &crate::document::ClipRecord) -> bool {
    clip.start() == clock_time(record.start)
        && clip.inpoint() == clock_time(record.inpoint)
        && clip.duration() == clock_time(record.duration)
        && clip.layer().map(|l| l.priority() as usize) == Some(record.track)
        && clip_rate_of(clip) == record.rate
}

/// A clip's time effects: its speed change, when it has one.
fn time_effects(clip: &ges::Clip) -> Vec<ges::BaseEffect> {
    clip.top_effects()
        .into_iter()
        .filter_map(|e| e.downcast::<ges::BaseEffect>().ok())
        .filter(|e| e.is_time_effect())
        .collect()
}

/// A clip's playback rate: what its time effects play at, or 1.0 when it has
/// none. `videorate` keeps its rate as a double and `pitch` as a float (M3),
/// so either is read.
fn clip_rate_of(clip: &ges::Clip) -> f64 {
    time_effects(clip)
        .iter()
        .find_map(|e| {
            let name = if e.track_type() == ges::TrackType::AUDIO {
                PITCH_PROPERTY
            } else {
                "rate"
            };
            let value = TimelineElementExt::child_property(e, name)?;
            value
                .get::<f64>()
                .ok()
                .or_else(|| value.get::<f32>().ok().map(f64::from))
        })
        .unwrap_or(1.0)
}

/// Replace a clip's time effects with ones playing at `rate`, or with none at
/// 1.0, so a clip at normal speed carries no effects at all. The picture and
/// the sound get one each, but only a stream the clip has: an audio effect on
/// a clip with no sound fails inside GES without an error, which the bindings
/// turn into a panic in a debug build (M5). Refused for a still, which has no
/// source time to stretch.
fn apply_rate(clip: &ges::Clip, rate: f64) -> Result<()> {
    if rate != 1.0
        && clip
            .property::<Option<gst::ClockTime>>("max-duration")
            .is_none()
    {
        anyhow::bail!("a still has no source time to play faster or slower");
    }
    for effect in time_effects(clip) {
        clip.remove_top_effect(&effect)?;
    }
    if rate == 1.0 {
        return Ok(());
    }
    let has = |kind: gst::glib::Type| clip.find_track_element(None::<&ges::Track>, kind).is_some();
    let mut wanted = Vec::new();
    if has(ges::VideoSource::static_type()) {
        wanted.push(format!("videorate rate={rate}"));
    }
    if has(ges::AudioSource::static_type()) {
        wanted.push(format!("pitch {PITCH_PROPERTY}={rate}"));
    }
    for description in wanted {
        let effect = ges::Effect::new(&description)?;
        // A runtime that does not re-time the clip for this effect would give
        // a clip whose length no longer matches its sound: stop here instead.
        if !effect.is_time_effect() {
            anyhow::bail!("this GStreamer does not treat {description:?} as a speed change");
        }
        clip.add_top_effect(&effect, -1)?;
    }
    Ok(())
}

/// Set a clip's times and speed so that, at every step on the way, the source
/// is never asked for more than it has, which GES refuses: the duration goes
/// down first if it is going down at all; then the speed and the in-point,
/// whichever lowers the source used first; then the final duration; then the
/// start. With the speed unchanged this is the order `trim_clip` uses.
fn set_clip_placement(
    clip: &ges::Clip,
    start: gst::ClockTime,
    inpoint: gst::ClockTime,
    duration: gst::ClockTime,
    rate: f64,
) {
    let current = clip_rate_of(clip);
    clip.set_duration(duration.min(clip.duration()));
    if rate < current {
        let _ = apply_rate(clip, rate);
        clip.set_inpoint(inpoint);
    } else {
        clip.set_inpoint(inpoint);
        if rate != current {
            let _ = apply_rate(clip, rate);
        }
    }
    clip.set_duration(duration);
    clip.set_start(start);
}

/// Where a two-step pipeline operation has got to. `Pending` means the state
/// change is still running; the caller should look again on its next tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Pending,
    Ready,
}

/// Ask the pipeline for its state with no wait at all. A timed-out state
/// change reports as `Async`, which is exactly "not there yet"; a failed one
/// reports as an error, and there is nothing to wait for then either.
fn settled(pipeline: &ges::Pipeline) -> Step {
    match pipeline.state(gst::ClockTime::ZERO).0 {
        Ok(gst::StateChangeSuccess::Success) | Ok(gst::StateChangeSuccess::NoPreroll) => {
            Step::Ready
        }
        Err(_) => Step::Ready,
        _ => Step::Pending,
    }
}

/// Fixed composited canvas size. Pinning it gives the inspector's position/scale
/// controls a known frame to work against.
pub const CANVAS_W: i32 = 1280;
pub const CANVAS_H: i32 = 720;

/// Video codec for export. The codec implies the container/muxer and audio codec
/// (H.264 → MP4/AAC, VP8/VP9 → WebM/Opus).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoCodec {
    /// H.264 in MP4 (hardware NVENC when available, else software x264).
    H264,
    /// VP9 in WebM.
    Vp9,
    /// VP8 in WebM.
    Vp8,
}

/// Export/render settings: codec (→ container), output resolution, frame rate,
/// and target video bitrate in kbit/s. A `bitrate_kbps` of 0 means "let the
/// encoder decide".
#[derive(Clone, Copy, Debug)]
pub struct ExportSettings {
    pub codec: VideoCodec,
    pub width: i32,
    pub height: i32,
    /// Output frames per second. The encoding profile pins the framerate (see
    /// `encoding_profile`), so this decides the constant output rate.
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// Hardware, software, or let the machine decide (and fall back).
    pub encoder: Encoder,
}

/// Hardware H.264 (NVENC) refuses tiny frames (its floor is around 145×49)
/// with an opaque "stream error", and the encoder rank is fixed at init so
/// there is no software fallback; NV12 also needs even dimensions. Every
/// render — GUI export and headless sequence alike — goes through
/// [`normalize_render_size`] so neither caller can hit that failure.
pub const MIN_RENDER_W: i32 = 160;
pub const MIN_RENDER_H: i32 = 96;

/// The frame size actually encoded for a requested `w`×`h`: as requested,
/// except sizes below the encoder floor are scaled UP (aspect kept), then
/// rounded to even and clamped to the canvas range.
pub fn normalize_render_size(w: i32, h: i32) -> (i32, i32) {
    let (wf, hf) = (w.max(1) as f64, h.max(1) as f64);
    let scale = (MIN_RENDER_W as f64 / wf)
        .max(MIN_RENDER_H as f64 / hf)
        .max(1.0);
    let w = ((wf * scale).round() as i32 & !1).clamp(16, 7680);
    let h = ((hf * scale).round() as i32 & !1).clamp(16, 4320);
    (w, h)
}

impl ExportSettings {
    /// These settings with the size normalized (see [`normalize_render_size`])
    /// and the frame rate clamped to 1..=240.
    pub fn normalized(mut self) -> Self {
        let (w, h) = normalize_render_size(self.width, self.height);
        self.width = w;
        self.height = h;
        self.fps = self.fps.clamp(1, 240);
        self
    }
}

/// Progress of an export/render.
#[derive(Clone, Debug)]
pub enum RenderStatus {
    Rendering(f32), // 0..1
    Done,
    Failed(String),
}

/// Encoder ranks for export, applied programmatically to the registry right
/// after `gst::init()` (call at every init site, before any pipeline exists):
///
/// - `nvautogpuh264enc` → 512 — prefer hardware NVENC H.264 (fast). On
///   non-NVIDIA machines the factory doesn't exist, so encodebin falls back.
/// - `x264enc` → 256 — software H.264 fallback.
/// - `mfaacenc` → NONE — **critical**: the Media Foundation AAC encoder ties
///   `voaacenc` at rank 128, and when encodebin picks it for the MP4 audio
///   track it spins up a D3D11 device that corrupts the GPU state, making
///   NVENC's `NvEncOpenEncodeSessionEx` fail `NV_ENC_ERR_INVALID_VERSION`
///   ("Could not encode stream" → 0-byte file). Forcing software `voaacenc`
///   makes hardware H.264 export reliable, even after the preview used the GPU.
///
/// Registry API instead of the GST_PLUGIN_FEATURE_RANK env var: the env-var
/// approach silently deactivated whenever the user's environment already set
/// that variable (re-breaking export with zero diagnostics), and writing env
/// vars from worker threads is a thread-safety hazard. Setting ranks on the
/// registry features directly merges with any user configuration — an explicit
/// env override still wins because the registry parses it at init, after which
/// we only *adjust* the specific features below. Idempotent; cheap after the
/// first call.
/// How long discovery may spend on one file before giving up.
///
/// GES discovers with no deadline at all by default, so a file behind an
/// unresponsive network share never returns: the import worker stops there and
/// every import queued behind it stops too, for the rest of the session, and
/// Cancel cannot free it because nothing is polling. Thumbnailing already
/// learned this (see [`thumbnail_uri`]); discovery had not.
///
/// Twenty seconds is generous for a slow share and short enough that a user
/// who picked the wrong file gets their app back.
pub(crate) const DISCOVERY_TIMEOUT_SECS: u64 = 20;

/// Apply [`DISCOVERY_TIMEOUT_SECS`] to the process-wide discoverer. Idempotent;
/// call after `ges::init()` on any path that may discover an asset.
pub(crate) fn ensure_discovery_timeout() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        ges::DiscovererManager::default()
            .set_timeout(gst::ClockTime::from_seconds(DISCOVERY_TIMEOUT_SECS));
    });
}

pub(crate) fn ensure_encoder_ranks() {
    use gst::prelude::PluginFeatureExtManual;
    let registry = gst::Registry::get();
    for (name, rank) in [
        (HARDWARE_H264, gst::Rank::from(512)),
        (SOFTWARE_H264, gst::Rank::from(256)),
        ("mfaacenc", gst::Rank::NONE),
    ] {
        if let Some(feature) = registry.lookup_feature(name) {
            feature.set_rank(rank);
        }
    }
}

/// The hardware H.264 encoder, when the machine has one.
pub(crate) const HARDWARE_H264: &str = "nvautogpuh264enc";
/// The software H.264 encoder, which ships with the bundled runtime.
pub(crate) const SOFTWARE_H264: &str = "x264enc";

/// Which encoder an export should use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Encoder {
    /// Hardware where the machine has it, software where it does not — and
    /// software anyway if the hardware attempt fails. The default, because
    /// what a user wants is a file, not an encoder.
    #[default]
    Auto,
    /// Hardware only. A failure is reported rather than worked around, which
    /// is what someone debugging their machine wants.
    Hardware,
    /// Software only: slower, and the one that always works.
    Software,
}

/// True if `name` is an encoder that runs on the GPU. Names rather than caps:
/// the vendor prefixes are stable and the klass string is not ("Hardware" is
/// absent from some elements that very much are).
pub fn is_hardware_encoder(name: &str) -> bool {
    ["nv", "qsv", "amf", "d3d11", "va", "mf"]
        .iter()
        .any(|p| name.starts_with(p))
}

/// Does this machine have a hardware H.264 encoder at all?
pub fn hardware_encoding_available() -> bool {
    let _ = gst::init();
    gst::Registry::get().lookup_feature(HARDWARE_H264).is_some()
}

/// The encoder factory a choice pins, if it pins one. `Auto` pins nothing and
/// takes whatever the ranks set at start-up prefer.
fn pinned_encoder(choice: Encoder, codec: VideoCodec) -> Option<&'static str> {
    match (choice, codec) {
        (Encoder::Auto, _) => None,
        (Encoder::Hardware, VideoCodec::H264) => Some(HARDWARE_H264),
        // No hardware VP8/VP9 encoder ships with the bundled runtime, so
        // "hardware" for those is the software one either way.
        (Encoder::Hardware, _) => None,
        (Encoder::Software, VideoCodec::H264) => Some(SOFTWARE_H264),
        (Encoder::Software, VideoCodec::Vp8) => Some("vp8enc"),
        (Encoder::Software, VideoCodec::Vp9) => Some("vp9enc"),
    }
}

/// The GES encoding profile for export settings: container + video + audio caps,
/// plus the output resolution (as a video-profile restriction so encodebin scales
/// to it) and the target bitrate (set on whichever encoder encodebin picks).
fn encoding_profile(s: ExportSettings) -> gst_pbutils::EncodingContainerProfile {
    let s = s.normalized();
    let (container, video_caps, audio_caps) = match s.codec {
        VideoCodec::H264 => (
            gst::Caps::builder("video/quicktime")
                .field("variant", "iso")
                .build(),
            // qtmux needs H.264 in avc stream-format; be explicit so encodebin
            // inserts h264parse (generic caps let it pick byte-stream and fail).
            gst::Caps::builder("video/x-h264")
                .field("stream-format", "avc")
                .build(),
            gst::Caps::builder("audio/mpeg")
                .field("mpegversion", 4i32)
                .build(),
        ),
        VideoCodec::Vp9 => (
            gst::Caps::builder("video/webm").build(),
            gst::Caps::builder("video/x-vp9").build(),
            gst::Caps::builder("audio/x-opus").build(),
        ),
        VideoCodec::Vp8 => (
            gst::Caps::builder("video/webm").build(),
            gst::Caps::builder("video/x-vp8").build(),
            gst::Caps::builder("audio/x-opus").build(),
        ),
    };

    // Output resolution + a FULLY PINNED raw format. Pinning format & framerate (not
    // just size) means encodebin converts every composition segment to one constant
    // caps before the encoder. Without this, gap fillers and still-image overlays
    // renegotiate caps mid-stream — software x264 tolerates that, hardware NVENC
    // errors out ("Internal data stream error" → truncated file with no moov atom).
    // The pixel format is per-codec: NV12 is NVENC's native layout; VP8/9 take I420.
    // Kept alive here so the builder's borrow outlives the profile build.
    let pixfmt = match s.codec {
        VideoCodec::H264 => "NV12",
        VideoCodec::Vp9 | VideoCodec::Vp8 => "I420",
    };
    let restriction = gst::Caps::builder("video/x-raw")
        .field("format", pixfmt)
        .field("width", s.width)
        .field("height", s.height)
        .field("framerate", gst::Fraction::new(s.fps as i32, 1))
        .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
        .build();
    let mut vb = gst_pbutils::EncodingVideoProfile::builder(&video_caps).restriction(&restriction);
    // Pinning the encoder. Element ranks cannot do this: encodebin builds its
    // candidate list once and caches it, so a rank changed later is invisible
    // to it (measured — a render with the hardware encoder at rank NONE still
    // chose it). A profile's preset name is matched against the factory name,
    // and a factory that does not match is skipped.
    if let Some(factory) = pinned_encoder(s.encoder, s.codec) {
        vb = vb.preset_name(factory);
    }
    // Target bitrate. Property name and unit differ by encoder: x264enc/NVENC use
    // "bitrate" in kbit/s (guint); vp8enc/vp9enc use "target-bitrate" in bit/s (gint).
    // ElementProperties applies to whichever matching encoder encodebin instantiates.
    if s.bitrate_kbps > 0 {
        let props = match s.codec {
            VideoCodec::H264 => gst_pbutils::ElementProperties::builder_general()
                .field("bitrate", s.bitrate_kbps)
                .build(),
            VideoCodec::Vp9 | VideoCodec::Vp8 => gst_pbutils::ElementProperties::builder_general()
                .field(
                    "target-bitrate",
                    (s.bitrate_kbps.saturating_mul(1000)) as i32,
                )
                .build(),
        };
        vb = vb.element_properties(props);
    }
    let video = vb.build();

    let audio = gst_pbutils::EncodingAudioProfile::builder(&audio_caps).build();
    gst_pbutils::EncodingContainerProfile::builder(&container)
        .add_profile(video)
        .add_profile(audio)
        .build()
}

/// A clip's transform + audio level for the inspector. `scale` is relative to
/// the largest size that fits the canvas WITHOUT distorting the source, so a
/// non-16:9 clip keeps its aspect ratio: 1.0 fits, above 1.0 zooms past the
/// canvas edges, and nothing in the engine caps it.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub posx: i32,
    pub posy: i32,
    pub scale: f64,
    pub alpha: f64,
    pub volume: f64,
}

/// Largest (width, height) a `nat_w`x`nat_h` source fits into a `cw`x`ch` canvas
/// without distortion (letter/pillar-boxed). Falls back to the full canvas if
/// the source size is unknown.
fn fit_size(nat_w: u32, nat_h: u32, cw: i32, ch: i32) -> (f64, f64) {
    if nat_w == 0 || nat_h == 0 {
        return (cw as f64, ch as f64);
    }
    let aspect = nat_w as f64 / nat_h as f64;
    let canvas_aspect = cw as f64 / ch as f64;
    if aspect > canvas_aspect {
        (cw as f64, cw as f64 / aspect)
    } else {
        (ch as f64 * aspect, ch as f64)
    }
}

/// The clip's source video dimensions, for aspect-correct scaling (None until
/// the source has negotiated caps, or for audio-only clips).
fn clip_natural_size(clip: &ges::Clip) -> Option<(u32, u32)> {
    let el = clip.find_track_element(None::<&ges::Track>, ges::VideoSource::static_type())?;
    let src = el.downcast::<ges::VideoSource>().ok()?;
    let (w, h) = src.natural_size()?;
    (w > 0 && h > 0).then_some((w as u32, h as u32))
}

/// The capsfilter GES pairs with a clip's frame positioner: the element
/// whose caps the positioner rewrites whenever its `width` or `height` is
/// set. Found from the positioner, which is what answers for `width`, as the
/// one capsfilter in the bin around it. None until the clip has a video
/// track element, when there is nothing to guard either.
fn positioner_capsfilter(clip: &ges::Clip) -> Option<gst::Element> {
    let (positioner, _) = clip.lookup_child("width")?;
    let bin = positioner
        .downcast::<gst::Element>()
        .ok()?
        .parent()?
        .downcast::<gst::Bin>()
        .ok()?;
    bin.children()
        .into_iter()
        .find(|e| e.factory().is_some_and(|f| f.name() == "capsfilter"))
}

/// Set a clip's frame: its position and size on the canvas.
///
/// The size goes to the GES frame positioner, whose setter rewrites the caps
/// of the capsfilter behind it while still holding the positioner's own
/// object lock. GStreamer answers any property change by walking up the
/// element's parents, taking each bin's lock in turn, so that walk runs
/// under the positioner's lock. The composition's thread takes the same
/// locks the other way round, the bin's first and then every child's,
/// whenever it adds the clip's source to its stack or changes the source's
/// state: while a clip that has just been added comes up, and while a stack
/// is torn down for a commit, a seek or the end of a clip. Where the two
/// meet, each waits for the other for good: GES itself is deadlocked, and
/// the app with it. Transforming a clip that was still prerolling hung 5
/// runs in 25 under load, every one on this pair of stacks.
///
/// Freezing the capsfilter's notifications for the duration holds the parent
/// walk back until the guard drops, after the positioner's setter has
/// returned and let go of its lock. Nothing else the setter does under that
/// lock waits on another thread.
fn set_clip_frame(clip: &ges::Clip, posx: i32, posy: i32, width: i32, height: i32) {
    let _deferred_notify = positioner_capsfilter(clip).map(|f| f.freeze_notify());
    let _ = clip.set_child_property("posx", &posx.to_value());
    let _ = clip.set_child_property("posy", &posy.to_value());
    let _ = clip.set_child_property("width", &width.to_value());
    let _ = clip.set_child_property("height", &height.to_value());
}

/// A GES-backed editing project: one timeline, one preview pipeline. Layers are
/// visual tracks, index 0 = bottom (top layers composite over lower ones).
pub struct Project {
    timeline: ges::Timeline,
    layers: Vec<ges::Layer>,
    pipeline: ges::Pipeline,
    /// Clips by ID, so the GUI can edit them (slide/trim/transform). An ID is
    /// the GES name a clip was placed under, or, for a restored clip, the ID it
    /// had before: GES names every new clip afresh.
    clips: HashMap<String, ges::Clip>,
    /// Set by edits, cleared by `refresh_preview` — coalesces repaints.
    dirty: std::cell::Cell<bool>,
    /// Set by edits, cleared only by saving or loading. Unlike `dirty`, which
    /// is a repaint-pending flag the preview timer clears, this answers "would
    /// closing now lose work".
    unsaved: std::cell::Cell<bool>,
    /// The preview video sink; kept so we can restore preview mode after a render.
    appsink: AppSink,
    /// Composited canvas ("viewport") size in px. Configurable via
    /// `set_canvas_size`; drives fit/layout and the video track restriction caps.
    canvas_w: i32,
    canvas_h: i32,
    /// True between `begin_render` and `end_render`. Transport and edit
    /// methods are inert while set: a Space/Delete key during an export used
    /// to pause the RENDER pipeline or commit a removal mid-render.
    rendering: std::cell::Cell<bool>,
    /// The encoder element the current or last render used (see
    /// [`Project::render_encoder`]).
    last_encoder: std::cell::RefCell<Option<String>>,
    /// Clips taken off the timeline that the preview may still be using, each
    /// with the number of commits the engine had asked for when its removal
    /// was committed (see [`Project::remove_clip`]).
    removed: std::cell::RefCell<Vec<(ges::Clip, u64)>>,
    /// How many commits the engine has asked the timeline for. Every one goes
    /// through [`Project::commit`], which counts it here; only this thread
    /// touches it.
    commits: std::cell::Cell<u64>,
    /// Per track, how many of those commits its composition has finished,
    /// counted from the track's `commited`. That signal arrives on the
    /// composition's own thread, never this one, so the counts are atomics
    /// and nothing else is shared with the handler.
    track_commits: Vec<Arc<AtomicU64>>,
    /// How long the last composited preview frame lasted, in nanoseconds
    /// (see [`Project::frame_secs`]). Written on the appsink's streaming
    /// thread, read on the interface's.
    frame_ns: Arc<AtomicU64>,
    /// The rate the preview plays at: 1.0, except after [`Project::set_rate`]
    /// until the next ordinary seek, which plays at normal speed again.
    rate: std::cell::Cell<f64>,
}

impl Project {
    /// Build an empty project whose preview pushes RGBA frames to `on_frame`
    /// (called from a GStreamer thread with the buffer mapped — copy what you
    /// need and hop to the UI thread; the view does not outlive the call).
    pub fn new(on_frame: impl Fn(FrameView<'_>) + Send + Sync + 'static) -> Result<Self> {
        gst::init()?;
        ges::init()?;
        ensure_encoder_ranks();
        ensure_discovery_timeout();

        let timeline = ges::Timeline::new_audio_video();
        // One counter per track, filled in from the track's own `commited`.
        // The timeline's `commited` cannot do this job: it fires once per
        // batch (see `release_removed`). These are the timeline's only
        // tracks — the engine adds layers later, never tracks.
        let track_commits: Vec<Arc<AtomicU64>> = timeline
            .tracks()
            .iter()
            .map(|track| {
                let done = Arc::new(AtomicU64::new(0));
                let counter = done.clone();
                track.connect_commited(move |_| {
                    counter.fetch_add(1, Ordering::SeqCst);
                });
                done
            })
            .collect();
        let layer = timeline.append_layer();
        let pipeline = ges::Pipeline::new();
        pipeline.set_timeline(&timeline)?;

        // Pin the composited video size so transforms have a fixed canvas.
        let restriction = gst::Caps::builder("video/x-raw")
            .field("width", CANVAS_W)
            .field("height", CANVAS_H)
            .build();
        for track in timeline.tracks() {
            if track.track_type() == ges::TrackType::VIDEO {
                track.set_restriction_caps(&restriction);
            }
        }

        let appsink = AppSink::builder()
            .caps(
                &gst::Caps::builder("video/x-raw")
                    .field("format", "RGBA")
                    .build(),
            )
            .max_buffers(2)
            .drop(true)
            .build();

        let frame_ns = Arc::new(AtomicU64::new(DEFAULT_FRAME_NS));
        let cb: Arc<dyn Fn(FrameView<'_>) + Send + Sync> = Arc::new(on_frame);
        let cb_sample = cb.clone();
        let cb_preroll = cb;
        let ns_sample = frame_ns.clone();
        let ns_preroll = frame_ns.clone();
        appsink.set_callbacks(
            AppSinkCallbacks::builder()
                // Playing: frames arrive as samples.
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    emit_sample(&sample, &*cb_sample, &ns_sample)
                })
                // Paused / after a seek: the current frame arrives as a preroll
                // buffer, so deliver it too — otherwise edits don't repaint while
                // the timeline is paused.
                .new_preroll(move |sink| {
                    let sample = sink.pull_preroll().map_err(|_| gst::FlowError::Eos)?;
                    emit_sample(&sample, &*cb_preroll, &ns_preroll)
                })
                .build(),
        );
        pipeline.preview_set_video_sink(Some(appsink.upcast_ref::<gst::Element>()));
        pipeline.set_mode(ges::PipelineFlags::FULL_PREVIEW)?;

        Ok(Self {
            timeline,
            layers: vec![layer],
            pipeline,
            clips: HashMap::new(),
            dirty: std::cell::Cell::new(false),
            unsaved: std::cell::Cell::new(false),
            appsink,
            canvas_w: CANVAS_W,
            canvas_h: CANVAS_H,
            rendering: std::cell::Cell::new(false),
            last_encoder: std::cell::RefCell::new(None),
            removed: std::cell::RefCell::new(Vec::new()),
            commits: std::cell::Cell::new(0),
            track_commits,
            frame_ns,
            rate: std::cell::Cell::new(1.0),
        })
    }

    /// Whether an export/render is in progress (edits and transport are inert).
    pub fn is_rendering(&self) -> bool {
        self.rendering.get()
    }

    /// An edit happened: repaint, and remember that the file on disk is behind.
    fn touched(&self) {
        self.dirty.set(true);
        self.unsaved.set(true);
    }

    /// Would closing now lose work? Unlike the repaint flag, this survives
    /// every tick of the preview timer and clears only on a save or a load,
    /// so the window can ask before it closes itself for an update.
    pub fn has_unsaved_work(&self) -> bool {
        self.unsaved.get()
    }

    /// The project now matches a file on disk.
    pub fn mark_saved(&self) {
        self.unsaved.set(false);
    }

    /// Current composited canvas ("viewport") size in px.
    pub fn canvas_size(&self) -> (i32, i32) {
        (self.canvas_w, self.canvas_h)
    }

    /// Change the composited canvas ("viewport") size. Updates the video track
    /// restriction caps (so the preview + render output become this size) and
    /// repaints. Existing clip transforms keep their pixel coordinates, so set
    /// this before laying out clips for the cleanest result.
    pub fn set_canvas_size(&mut self, w: i32, h: i32) {
        if self.rendering.get() {
            return;
        }
        let w = w.clamp(16, 7680);
        let h = h.clamp(16, 4320);
        self.canvas_w = w;
        self.canvas_h = h;
        let restriction = gst::Caps::builder("video/x-raw")
            .field("width", w)
            .field("height", h)
            .build();
        for track in self.timeline.tracks() {
            if track.track_type() == ges::TrackType::VIDEO {
                track.set_restriction_caps(&restriction);
            }
        }
        self.commit();
        self.touched();
    }

    /// Ensure at least `index + 1` layers exist; return the layer at `index`.
    fn layer(&mut self, index: usize) -> ges::Layer {
        while self.layers.len() <= index {
            self.layers.push(self.timeline.append_layer());
        }
        self.layers[index].clone()
    }

    /// Ask the timeline to apply the edits made so far, and count the asking.
    /// EVERY commit in the engine goes through here: each track's composition
    /// reports one `commited` per commit, in order, which is how
    /// [`Self::release_removed`] knows a removal has run. Async, as ever —
    /// `commit_sync` deadlocks the caller mid state change.
    fn commit(&self) {
        self.commits.set(self.commits.get() + 1);
        self.timeline.commit();
    }

    /// Add `path` as a clip on track `track` at timeline position `start`,
    /// showing the source range `[inpoint, inpoint + duration)`.
    pub fn add_clip(
        &mut self,
        path: &Path,
        track: usize,
        start: Duration,
        inpoint: Duration,
        duration: Duration,
    ) -> Result<ClipId> {
        let uri = gst::glib::filename_to_uri(path, None)?;
        self.add_clip_uri(&uri, track, start, inpoint, duration)
    }

    /// URI form of [`Self::add_clip`], for sources that aren't a single file
    /// (image sequences, and anything a saved project hands back).
    pub fn add_clip_uri(
        &mut self,
        uri: &str,
        track: usize,
        start: Duration,
        inpoint: Duration,
        duration: Duration,
    ) -> Result<ClipId> {
        if self.rendering.get() {
            anyhow::bail!("a render is in progress");
        }
        let clip = ges::UriClip::new(uri)?;
        clip.set_start(gst::ClockTime::from_nseconds(start.as_nanos() as u64));
        clip.set_inpoint(gst::ClockTime::from_nseconds(inpoint.as_nanos() as u64));
        clip.set_duration(gst::ClockTime::from_nseconds(duration.as_nanos() as u64));
        self.layer(track).add_clip(&clip)?;
        // Async commit (see append_clip): commit_sync() can deadlock during an
        // async pipeline state-change, so never block on the commit here.
        self.commit();
        // GES auto-names clips; a missing name would silently collide on the
        // "" key and orphan the previous clip — treat it as the error it is.
        let name = clip
            .name()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("GES returned an unnamed clip"))?;
        self.clips.insert(name.clone(), clip.clone().upcast());
        Ok(ClipId(name))
    }

    /// Append `path` to the end of track `track`, using its natural duration
    /// (videos) or `image_dur` (still images, which have no intrinsic length).
    pub fn append_clip(
        &mut self,
        path: &Path,
        track: usize,
        image_dur: Option<Duration>,
    ) -> Result<ClipInfo> {
        let uri = gst::glib::filename_to_uri(path, None)?;
        self.append_clip_uri(&uri, track, image_dur)
    }

    /// URI form of [`Self::append_clip`], for sources that aren't a single file.
    /// An `imagesequence://` URI has an intrinsic duration (frames ÷ fps), so
    /// sequences pass `image_dur: None` like videos do.
    pub fn append_clip_uri(
        &mut self,
        uri: &str,
        track: usize,
        image_dur: Option<Duration>,
    ) -> Result<ClipInfo> {
        if self.rendering.get() {
            anyhow::bail!("a render is in progress");
        }
        let asset = ges::UriClipAsset::request_sync(uri)?;
        let dur_ct = match image_dur {
            Some(d) => gst::ClockTime::from_nseconds(d.as_nanos() as u64),
            None => asset.duration().unwrap_or(gst::ClockTime::from_seconds(5)),
        };
        let start_ct = self.track_end(track);
        let layer = self.layer(track);
        let clip = layer.add_asset(
            &asset,
            start_ct,
            gst::ClockTime::ZERO,
            dur_ct,
            ges::TrackType::UNKNOWN,
        )?;
        // Async commit: never block the caller. commit_sync() deadlocks if the
        // pipeline is mid async state-change (e.g. a second clip added right
        // after play()), because the commit ack can't arrive until preroll ends.
        self.commit();
        let name = clip
            .name()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("GES returned an unnamed clip"))?;
        self.clips.insert(name.clone(), clip.clone());
        Ok(ClipInfo {
            id: ClipId(name),
            track,
            start: Duration::from_nanos(start_ct.nseconds()),
            duration: Duration::from_nanos(dur_ct.nseconds()),
        })
    }

    /// Slide a clip along its track by `delta_secs` (may be negative); start is
    /// clamped to >= 0 and to the gap the clip occupies on its layer, so a drag
    /// cannot bury one clip under another (see [`slide_within_gap`]). Returns
    /// the resulting geometry, or None for an unknown id.
    pub fn slide_clip(&mut self, id: &ClipId, delta_secs: f64) -> Option<ClipGeom> {
        if self.rendering.get() {
            return None;
        }
        let clip = self.clips.get(&id.0)?.clone();
        let start = clip.start().nseconds() as i128;
        let dur = clip.duration().nseconds() as i128;
        let delta = (delta_secs * 1e9) as i128;
        let new_start = slide_within_gap(start, dur, delta, &self.layer_neighbours(id)) as u64;
        clip.set_start(gst::ClockTime::from_nseconds(new_start));
        self.commit();
        self.touched();
        Some(clip_geom(&clip))
    }

    /// Every other clip sharing a layer with `id`, as `(start, end)` in
    /// nanoseconds. Clips on other layers are composited over each other on
    /// purpose and are none of this method's business.
    fn layer_neighbours(&self, id: &ClipId) -> Vec<(i128, i128)> {
        let Some(me) = self.clips.get(&id.0) else {
            return Vec::new();
        };
        let my_layer = me.layer().map(|l| l.priority());
        self.clips
            .iter()
            .filter(|(name, _)| *name != &id.0)
            .filter(|(_, c)| c.layer().map(|l| l.priority()) == my_layer)
            .map(|(_, c)| {
                let start = c.start().nseconds() as i128;
                (start, start + c.duration().nseconds() as i128)
            })
            .collect()
    }

    /// Trim a clip by dragging an edge. `edge < 0` = left edge (keeps the right
    /// end fixed by moving start+inpoint and shrinking duration); `edge > 0` =
    /// right edge (adjusts duration only). Clamped to the source bounds and a
    /// 0.2 s minimum. Returns the resulting geometry.
    pub fn trim_clip(&mut self, id: &ClipId, edge: i32, delta_secs: f64) -> Option<ClipGeom> {
        if self.rendering.get() {
            return None;
        }
        let clip = self.clips.get(&id.0)?.clone();
        let start = clip.start().nseconds() as i128;
        let inpoint = clip.inpoint().nseconds() as i128;
        let dur = clip.duration().nseconds() as i128;
        // max-duration is GST_CLOCK_TIME_NONE (→ None) for stills; a finite length
        // for real media, which caps how far the right edge can extend.
        let max_ns = clip
            .property::<Option<gst::ClockTime>>("max-duration")
            .map(|m| m.nseconds() as i128);
        let delta = (delta_secs * 1e9) as i128;
        let rate = clip_rate_of(&clip);

        if edge < 0 {
            let (ns, ni, nd) = trim_left_math(start, inpoint, dur, delta, rate);
            let new_start = gst::ClockTime::from_nseconds(ns as u64);
            let new_inp = gst::ClockTime::from_nseconds(ni as u64);
            let new_dur = gst::ClockTime::from_nseconds(nd as u64);
            // Apply the shrinking property first so inpoint + duration never
            // transiently exceeds max-duration (which GES refuses).
            if nd <= dur {
                clip.set_duration(new_dur);
                clip.set_inpoint(new_inp);
            } else {
                clip.set_inpoint(new_inp);
                clip.set_duration(new_dur);
            }
            clip.set_start(new_start);
        } else {
            let nd = trim_right_math(inpoint, dur, delta, max_ns, rate);
            clip.set_duration(gst::ClockTime::from_nseconds(nd as u64));
        }
        self.commit();
        self.touched();
        Some(clip_geom(&clip))
    }

    /// Set a clip's duration outright (the inspector's Duration field for
    /// stills, whose length is otherwise only reachable by edge-trimming).
    /// Clamped like a right-edge trim: never below the trim minimum, never
    /// past the end of a real media source. Returns the resulting geometry.
    pub fn set_clip_duration(&mut self, id: &ClipId, secs: f64) -> Option<ClipGeom> {
        if self.rendering.get() || !secs.is_finite() {
            return None;
        }
        let current = self.clips.get(&id.0)?.duration().nseconds() as f64 / 1e9;
        self.trim_clip(id, 1, secs - current)
    }

    /// Cut the clip in two at timeline position `at`. The clip keeps its ID,
    /// start and in-point and ends at `at`; the returned clip begins there,
    /// with the in-point that follows (GES translates it through any speed
    /// change). Refused unless both halves keep at least 0.2 s. Returns the
    /// new clip's ID and both halves' geometry, left first.
    pub fn split_clip(
        &mut self,
        id: &ClipId,
        at: Duration,
    ) -> Result<(ClipId, ClipGeom, ClipGeom)> {
        if self.rendering.get() {
            anyhow::bail!("a render is in progress");
        }
        let clip = self
            .clips
            .get(&id.0)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("clip {} is not on the timeline", id.0))?;
        let start = clip.start().nseconds() as i128;
        let dur = clip.duration().nseconds() as i128;
        let at_ns = at.as_nanos() as i128;
        if !split_fits(start, dur, at_ns) {
            anyhow::bail!(
                "a split leaves at least {:.1} s of the clip on each side of the playhead",
                MIN_TRIM_NS as f64 / 1e9
            );
        }
        // GES copies the transform to the new half (measured, M6), but this
        // does not lean on it: the right half gets it written explicitly.
        // Only once the clip has one: a clip never laid out still has GES's
        // stretch-to-fill, and writing back the read-back of that would turn
        // it into a fitted frame in the corner.
        let laid_out = clip
            .child_property("width")
            .and_then(|v| v.get::<i32>().ok())
            .unwrap_or(0)
            > 0;
        let layout = if laid_out { self.clip_layout(id) } else { None };
        let right = clip
            .split_full(at_ns as u64)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .ok_or_else(|| anyhow::anyhow!("GES did not split the clip"))?;
        self.commit();
        // Named the way `add_clip_uri` names a clip: a nameless one would
        // collide on the "" key and orphan whatever was there.
        let name = right
            .name()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("GES returned an unnamed clip"))?;
        self.clips.insert(name.clone(), right.clone());
        let right_id = ClipId(name);
        if let Some(l) = layout {
            self.set_clip_layout(&right_id, l);
        }
        // GES copies the time effects to the new half and translates its
        // in-point through them (M6). Were a runtime not to, the right half
        // would play at normal speed and run past its source.
        let rate = clip_rate_of(&clip);
        if clip_rate_of(&right) != rate {
            let _ = apply_rate(&right, rate);
            // A change the timeline has not committed is a change the preview
            // never shows: route it through the same commit as every other edit.
            self.commit();
        }
        self.touched();
        Ok((right_id, clip_geom(&clip), clip_geom(&right)))
    }

    /// The clip's playback rate: what its time effects play at, or 1.0 when
    /// it has none or is not on the timeline.
    pub fn clip_rate(&self, id: &ClipId) -> f64 {
        self.clips.get(&id.0).map(clip_rate_of).unwrap_or(1.0)
    }

    /// Set the clip's playback rate, clamped to [`RATE_MIN`, `RATE_MAX`]. The
    /// clip keeps its start and in-point; its duration becomes the same span
    /// of source at the new rate, clamped to the trim minimum, to what the
    /// source can still supply, and to the gap before the next clip on its
    /// track (see [`rate_change_math`]). Returns the resulting geometry;
    /// None for an unknown clip, a still, a rate that is not a number, or a
    /// change GES refused, which leaves the clip as it was.
    pub fn set_clip_rate(&mut self, id: &ClipId, rate: f64) -> Option<ClipGeom> {
        if self.rendering.get() || !rate.is_finite() {
            return None;
        }
        let clip = self.clips.get(&id.0)?.clone();
        // A still has no source length and no source time to stretch.
        let max_ns = clip
            .property::<Option<gst::ClockTime>>("max-duration")?
            .nseconds() as i128;
        let rate = snap_rate(rate);
        let old_rate = clip_rate_of(&clip);
        if rate == old_rate {
            return Some(clip_geom(&clip));
        }
        let start = clip.start().nseconds() as i128;
        let dur = clip.duration().nseconds() as i128;
        let room = room_after(start, dur, &self.layer_neighbours(id));
        let nd = rate_change_math(
            clip.inpoint().nseconds() as i128,
            dur,
            old_rate,
            rate,
            Some(max_ns),
            room,
        );
        let new_dur = gst::ClockTime::from_nseconds(nd as u64);
        let old_dur = clip.duration();
        let set_len = |d: gst::ClockTime| clip.duration() == d || clip.set_duration(d);
        // Whichever change lowers the clip's duration-limit goes first, the
        // way `trim_clip` orders in-point and duration. Faster: the
        // shorter duration, then the effects. Slower: the effects, then the
        // longer duration, capped at the limit GES now reports, so a
        // nanosecond of rounding cannot get it refused.
        let applied = if rate > old_rate {
            set_len(new_dur) && apply_rate(&clip, rate).is_ok()
        } else {
            apply_rate(&clip, rate).is_ok() && {
                let limit = clip.property::<Option<gst::ClockTime>>("duration-limit");
                set_len(limit.map_or(new_dur, |l| new_dur.min(l)))
            }
        };
        if !applied || clip_rate_of(&clip) != rate {
            // Back as it was: the old effects, then the old length.
            let _ = apply_rate(&clip, old_rate);
            set_len(old_dur);
            self.commit();
            return None;
        }
        self.commit();
        self.touched();
        Some(clip_geom(&clip))
    }

    /// Write clips' start, in-point, duration, speed, track and transform
    /// exactly as their records give them. Nothing is clamped: this puts back
    /// a state the engine already accepted, which is what undo needs — a
    /// slide or trim replayed in reverse would be clamped again and land
    /// somewhere else.
    ///
    /// GES refuses, without an error, any moment where one clip sits fully on
    /// top of another, even when the end state is fine, so clips that trade
    /// tracks or places would collide halfway. Every clip whose place or times
    /// change is therefore parked alone on a new layer below the timeline
    /// first, then set and moved to its track, and the parking layers are
    /// removed. Every clip is read back. One that did not land goes back as it
    /// was or, if a clip that landed has taken its place, stays parked on the
    /// first free parking layer: a refused write changes nothing else, and
    /// trying it again adds no tracks.
    ///
    /// Returns the IDs of the clips that did not land where their record says
    /// (unknown clips included): empty when every write landed. Each ID may
    /// appear at most once. While rendering, nothing is written and every ID is
    /// returned.
    pub fn set_clip_records(
        &mut self,
        writes: &[(ClipId, crate::document::ClipRecord)],
    ) -> Vec<ClipId> {
        if self.rendering.get() {
            return writes.iter().map(|(id, _)| id.clone()).collect();
        }
        let mut failed = Vec::new();
        let mut found = Vec::with_capacity(writes.len());
        for (id, record) in writes {
            match self.clips.get(&id.0) {
                Some(clip) => found.push((id, record, clip.clone())),
                None => failed.push(id.clone()),
            }
        }
        // Parking layers go below every track a record names, so none of them
        // is also a destination.
        let parking = found
            .iter()
            .map(|(_, record, _)| record.track + 1)
            .fold(self.layers.len(), usize::max);
        let moving: Vec<_> = found
            .iter()
            .filter(|(_, record, clip)| !clip_placed_as(clip, record))
            .collect();
        // Where each moving clip was, so one that cannot land can go back.
        let origins: Vec<_> = moving
            .iter()
            .map(|(_, _, clip)| {
                (
                    clip.layer(),
                    clip.start(),
                    clip.inpoint(),
                    clip.duration(),
                    clip_rate_of(clip),
                )
            })
            .collect();
        for (k, (_, _, clip)) in moving.iter().enumerate() {
            let layer = self.layer(parking + k);
            // If parking fails, the writes below are still tried and the
            // read-back reports the clip.
            let _ = clip.move_to_layer(&layer);
        }
        for (_, record, clip) in &moving {
            set_clip_placement(
                clip,
                clock_time(record.start),
                clock_time(record.inpoint),
                clock_time(record.duration),
                record.rate,
            );
            let target = self.layer(record.track);
            let _ = clip.move_to_layer(&target);
        }
        for (id, record, clip) in &found {
            if !clip_placed_as(clip, record) {
                failed.push((*id).clone());
            }
        }
        // A clip that did not land goes back as it was. If a clip that did
        // land has taken its old place, it stays parked, packed onto the first
        // parking layers so no empty track is left above it.
        let mut stuck = 0;
        for (k, ((_, record, clip), (layer, start, inpoint, duration, rate))) in
            moving.iter().zip(origins).enumerate()
        {
            if clip_placed_as(clip, record) {
                continue;
            }
            let _ = clip.move_to_layer(&self.layers[parking + k]);
            set_clip_placement(clip, start, inpoint, duration, rate);
            if layer.is_some_and(|l| clip.move_to_layer(&l).is_ok()) {
                continue;
            }
            let _ = clip.move_to_layer(&self.layers[parking + stuck]);
            stuck += 1;
        }
        // Empty again, unless a clip could not go back to its place.
        while self.layers.len() > parking {
            if self.layers.last().is_some_and(|l| !l.clips().is_empty()) {
                break;
            }
            if let Some(last) = self.layers.pop() {
                let _ = self.timeline.remove_layer(&last);
            }
        }
        // A refused clip is back as it was, transform included.
        for (id, record, _) in found.iter().filter(|(id, _, _)| !failed.contains(*id)) {
            self.set_clip_layout(id, record.layout.into());
        }
        if !found.is_empty() {
            self.commit();
            self.touched();
        }
        failed
    }

    /// Put back a clip that was removed, as `record` describes it, under the
    /// ID it had, which the interface and the undo history still hold. GES
    /// replaces a name in its own `uriclipN` pattern with its next one, so a
    /// removed clip's name cannot be asked back; the engine keeps the old ID
    /// as its handle for the clip instead. GES never gives out a name twice in
    /// a process, so no later clip can arrive under that ID. Returns `id`;
    /// fails if a clip already has it.
    pub fn restore_clip(
        &mut self,
        id: &ClipId,
        record: &crate::document::ClipRecord,
    ) -> Result<ClipId> {
        if self.rendering.get() {
            anyhow::bail!("a render is in progress");
        }
        if self.clips.contains_key(&id.0) {
            anyhow::bail!("clip {} is already on the timeline", id.0);
        }
        let clip = ges::UriClip::new(&record.uri)?;
        clip.set_start(clock_time(record.start));
        clip.set_inpoint(clock_time(record.inpoint));
        // Slower than normal, a clip can be longer than its source lasts at
        // 1×, and it goes onto the layer at 1×: at the length the source
        // allows, until its speed is back on.
        let mut length = clock_time(record.duration);
        if record.rate < 1.0 {
            if let Some(max) = clip.max_duration() {
                length = length.min(max.saturating_sub(clock_time(record.inpoint)));
            }
        }
        clip.set_duration(length);
        self.layer(record.track).add_clip(&clip)?;
        self.commit();
        self.clips.insert(id.0.clone(), clip.upcast());
        self.set_clip_layout(id, record.layout.into());
        if record.rate != 1.0 {
            self.put_rate_back(id, record.rate, record.duration);
        }
        self.touched();
        Ok(id.clone())
    }

    /// The last step of restoring or loading a clip whose speed was changed:
    /// its time effects go on, then it takes its length, which at a slow
    /// speed only fits once they are on (M4). A refusal shows in the
    /// read-back, as any other write's does.
    fn put_rate_back(&mut self, id: &ClipId, rate: f64, duration: f64) {
        let Some(clip) = self.clips.get(&id.0).cloned() else {
            return;
        };
        let length = clock_time(duration);
        if apply_rate(&clip, rate).is_ok() && clip.duration() != length {
            clip.set_duration(length);
        }
        self.commit();
    }

    /// Whether the source at `uri` is there to bring back: the check undo
    /// makes before restoring a deleted clip, so a source deleted since is
    /// named instead of failing at preview with an error that names nothing.
    /// Undo only restores sources this session has discovered, and GES
    /// answers for those from its cache, which outlives the files. So a file
    /// is looked for on disk, and anything else (an image sequence) is
    /// discovered again, bounded by the discovery timeout.
    pub fn source_available(&self, uri: &str) -> bool {
        match crate::document::path_from_uri(uri) {
            Some(path) => path.is_file(),
            None => {
                let _ = ges::Asset::needs_reload(ges::UriClip::static_type(), Some(uri));
                ges::UriClipAsset::request_sync(uri).is_ok()
            }
        }
    }

    /// Remove empty tracks from the bottom while more than `keep` remain,
    /// stopping at the first track with a clip on it (an empty track above a
    /// clip stays, so no clip changes track) and never going below one. Does
    /// nothing while rendering. Undo and redo call this with the track count
    /// of the side they go to, so undoing a move onto a new bottom track takes
    /// that track away again.
    pub fn prune_tracks(&mut self, keep: usize) {
        if self.rendering.get() {
            return;
        }
        let mut changed = false;
        while self.layers.len() > keep.max(1) {
            if !self.layers[self.layers.len() - 1].clips().is_empty() {
                break;
            }
            if let Some(last) = self.layers.pop() {
                let _ = self.timeline.remove_layer(&last);
                changed = true;
            }
        }
        if changed {
            self.commit();
            self.touched();
        }
    }

    /// Number of tracks (GES layers, 0 = top) in the timeline.
    pub fn track_count(&self) -> usize {
        self.layers.len()
    }

    /// Move a clip to `track`, creating the layer if `track` is one past the last
    /// (a new bottom track). Returns the resulting track index.
    pub fn move_clip_to_track(&mut self, id: &ClipId, track: usize) -> Option<usize> {
        if self.rendering.get() {
            return None;
        }
        let clip = self.clips.get(&id.0)?.clone();
        let target = self.layer(track);
        clip.move_to_layer(&target).ok()?;
        self.commit();
        self.touched();
        Some(track)
    }

    /// The clip's current track index (its layer's priority, 0 = top).
    pub fn clip_track(&self, id: &ClipId) -> Option<usize> {
        self.clips
            .get(&id.0)?
            .layer()
            .map(|l| l.priority() as usize)
    }

    /// Remove a clip from the timeline entirely. Returns whether it existed.
    /// Empty TRAILING layers are pruned (never populated or middle ones, so
    /// remaining track indices stay stable); at least one layer always remains.
    ///
    /// The clip leaves the timeline, and every record, at once, but the
    /// engine keeps hold of it until GES has finished with it. GES only
    /// queues a source's removal: the composition carries on with whatever
    /// it was doing — prerolling a stack the clip is in, or building one from
    /// the commit that added it — and meanwhile the source's streaming
    /// threads call back into the clip's track elements. Letting the clip go
    /// at once freed those: STATUS_ACCESS_VIOLATION on a Delete or an undo
    /// straight after an add. See [`Self::release_removed`] for when it goes.
    pub fn remove_clip(&mut self, id: &ClipId) -> bool {
        if self.rendering.get() {
            return false;
        }
        let Some(clip) = self.clips.remove(&id.0) else {
            return false;
        };
        if let Some(layer) = clip.layer() {
            let _ = layer.remove_clip(&clip);
        }
        // Prune empty trailing layers so "drag down for a new track" mistakes
        // don't accumulate dead rows forever.
        while self.layers.len() > 1 {
            let last = self.layers.last().unwrap();
            if !last.clips().is_empty() {
                break;
            }
            let last = self.layers.pop().unwrap();
            let _ = self.timeline.remove_layer(&last);
        }
        self.commit();
        self.touched();
        // Stamped with the commits asked for so far, this removal's own
        // included: the clip goes once every track has finished that many.
        let stamp = self.commits.get();
        self.removed.borrow_mut().push((clip, stamp));
        self.release_removed();
        true
    }

    /// Let go of the removed clips GES has finished with: those whose
    /// removal's commit EVERY track has completed.
    ///
    /// A track's composition does what it is given in order — the removal,
    /// then the commit asked for after it — and reports one `commited` per
    /// commit, so a track that has finished as many commits as a clip's stamp
    /// has run that clip's removal. The timeline's own `commited` cannot
    /// answer this: it fires once per batch, as soon as every track has
    /// reported since the last commit was asked for, which the completion of
    /// an OLDER commit satisfies while the removal still sits in the queue.
    /// Reading it as "this removal has run" freed clips the compositions were
    /// still about to bring up.
    ///
    /// Never blocks; the preview timer calls it every tick, and so does every
    /// removal. At NULL nothing is playing and no commit runs until the
    /// pipeline starts again, which is also the moment the queue is made good
    /// from the start, so everything held goes at once: a commit dropped by a
    /// state change cannot strand a clip for the rest of the session. That
    /// is the one place the rule is bypassed, and it is safe even for a clip
    /// whose removal no track ever ran: reaching NULL has joined the
    /// streaming threads and torn the stacks down, so nothing is left to
    /// read the clip's source.
    fn release_removed(&self) {
        if self.removed.borrow().is_empty() {
            return;
        }
        if self.pipeline.current_state() == gst::State::Null
            && self.pipeline.pending_state() == gst::State::VoidPending
        {
            self.removed.borrow_mut().clear();
            return;
        }
        let done = self.commits_done();
        self.removed.borrow_mut().retain(|(_, stamp)| *stamp > done);
    }

    /// How many commits every track has finished: the lowest of the per-track
    /// counts, so a clip stamped at or below it has had its removal run
    /// everywhere. A timeline with no tracks answers 0, which holds every
    /// clip instead of releasing the lot — the harmless way round.
    fn commits_done(&self) -> u64 {
        self.track_commits
            .iter()
            .map(|done| done.load(Ordering::SeqCst))
            .min()
            .unwrap_or(0)
    }

    /// Describe the whole timeline in the form that goes in a file: every
    /// clip's source, place, trim and transform, plus the canvas. Ordered by
    /// track and then by start time, so a saved file reads top-to-bottom the
    /// way the timeline looks.
    pub fn to_document(&self) -> crate::document::ProjectFile {
        let (w, h) = self.canvas_size();
        let clips = self
            .clip_records()
            .into_iter()
            .map(|(_, rec)| rec)
            .collect();
        crate::document::ProjectFile::new(w, h, clips)
    }

    /// The same records, each with the handle of the clip it came from. The
    /// interface rebuilds its timeline rows from this after opening a project,
    /// where it needs the handle to address the clip for later edits.
    pub fn clip_records(&self) -> Vec<(ClipId, crate::document::ClipRecord)> {
        let mut clips: Vec<(ClipId, crate::document::ClipRecord)> = self
            .clips
            .iter()
            .filter_map(|(name, clip)| {
                let uri = clip.downcast_ref::<ges::UriClip>()?.uri().to_string();
                let secs = |t: gst::ClockTime| t.nseconds() as f64 / 1e9;
                Some((
                    ClipId(name.clone()),
                    crate::document::ClipRecord {
                        name: uri
                            .rsplit(['/', '\\'])
                            .next()
                            .map(|s| s.split('?').next().unwrap_or(s).to_string())
                            .unwrap_or_default(),
                        uri,
                        track: clip.layer().map(|l| l.priority() as usize).unwrap_or(0),
                        start: secs(clip.start()),
                        inpoint: secs(clip.inpoint()),
                        duration: secs(clip.duration()),
                        rate: clip_rate_of(clip),
                        layout: self
                            .clip_layout(&ClipId(name.clone()))
                            .map(Into::into)
                            .unwrap_or(crate::document::LayoutRecord {
                                posx: 0,
                                posy: 0,
                                scale: 1.0,
                                alpha: 1.0,
                                volume: 1.0,
                            }),
                        // Filled in by the caller, which is the only side that
                        // knows how a sequence clip was described when it arrived.
                        sequence: None,
                    },
                ))
            })
            .collect();
        clips.sort_by(|(_, a), (_, b)| {
            a.track
                .cmp(&b.track)
                .then(a.start.total_cmp(&b.start))
                .then(a.uri.cmp(&b.uri))
        });
        clips
    }

    /// Replace the timeline with what `doc` describes.
    ///
    /// Returns the sources it could not open, by name, rather than failing the
    /// whole project: media moves, and a project that refuses to open at all
    /// because one clip is missing is a project you cannot rescue.
    pub fn apply_document(&mut self, doc: &crate::document::ProjectFile) -> Result<Vec<String>> {
        if self.rendering.get() {
            anyhow::bail!("a render is in progress");
        }
        for id in self.clips.keys().cloned().collect::<Vec<_>>() {
            self.remove_clip(&ClipId(id));
        }
        self.set_canvas_size(doc.canvas_w, doc.canvas_h);
        let mut missing = Vec::new();
        for rec in &doc.clips {
            // GES will happily build a clip around a URI that points at
            // nothing and only fail later, at preroll, as a bus error with no
            // file name in it. Discovery is the honest check — and it warms the
            // asset the clip is about to use. (It is bounded: see
            // `ensure_discovery_timeout`.)
            let name = || {
                if rec.name.is_empty() {
                    rec.uri.clone()
                } else {
                    rec.name.clone()
                }
            };
            let asset = match ges::UriClipAsset::request_sync(&rec.uri) {
                Ok(asset) => asset,
                Err(_) => {
                    missing.push(name());
                    continue;
                }
            };
            // Slower than normal, a clip can be longer than its source lasts
            // at 1×: it goes on at the length the source allows, and takes
            // the rest once its speed is back on.
            let mut length = Duration::from_secs_f64(rec.duration.max(0.0));
            if rec.rate < 1.0 {
                if let Some(source) = asset.duration() {
                    let fits = source.saturating_sub(clock_time(rec.inpoint)).nseconds();
                    length = length.min(Duration::from_nanos(fits));
                }
            }
            let placed = self.add_clip_uri(
                &rec.uri,
                rec.track,
                Duration::from_secs_f64(rec.start.max(0.0)),
                Duration::from_secs_f64(rec.inpoint.max(0.0)),
                length,
            );
            match placed {
                Ok(id) => {
                    self.set_clip_layout(&id, rec.layout.into());
                    if rec.rate != 1.0 {
                        self.put_rate_back(&id, rec.rate, rec.duration);
                    }
                }
                Err(_) => missing.push(name()),
            }
        }
        self.commit();
        self.touched();
        // Just loaded: this is exactly what is on disk. The edits above set the
        // flag on their way through, so this has to come last.
        self.unsaved.set(false);
        Ok(missing)
    }

    /// Reorder tracks: move the track at `from` to position `to` (0 = top).
    pub fn move_track(&mut self, from: usize, to: usize) {
        if self.rendering.get()
            || from >= self.layers.len()
            || to >= self.layers.len()
            || from == to
        {
            return;
        }
        let layer = self.layers[from].clone();
        let _ = self.timeline.move_layer(&layer, to as u32);
        // Resync our layer vec to the new priority order.
        self.layers = self.timeline.layers();
        self.commit();
        self.touched();
    }

    /// Read a clip's current layout (position, aspect-preserving scale, opacity,
    /// volume) from its GES child properties.
    pub fn clip_layout(&self, id: &ClipId) -> Option<Layout> {
        let clip = self.clips.get(&id.0)?;
        let geti = |n: &str, d: i32| {
            clip.child_property(n)
                .and_then(|v| v.get::<i32>().ok())
                .unwrap_or(d)
        };
        let getf = |n: &str, d: f64| {
            clip.child_property(n)
                .and_then(|v| v.get::<f64>().ok())
                .unwrap_or(d)
        };
        let (fit_w, _) = match clip_natural_size(clip) {
            Some((w, h)) => fit_size(w, h, self.canvas_w, self.canvas_h),
            None => (self.canvas_w as f64, self.canvas_h as f64),
        };
        let width = geti("width", 0);
        // No upper clamp: set_clip_layout accepts scale > 1.0 (zoom-in), so the
        // read-back must round-trip it — a 1.0 ceiling here silently snapped
        // zoomed clips back to 100% whenever the inspector refreshed.
        let scale = if width > 0 && fit_w > 0.0 {
            (width as f64 / fit_w).max(0.0)
        } else {
            1.0
        };
        Some(Layout {
            posx: geti("posx", 0),
            posy: geti("posy", 0),
            scale,
            alpha: getf("alpha", 1.0),
            volume: getf("volume", 1.0),
        })
    }

    /// Apply a layout to a clip, live. `scale` maps to aspect-correct width/height
    /// derived from the source size, so the clip is never distorted. Missing child
    /// properties (e.g. volume on a still image) are ignored.
    pub fn set_clip_layout(&mut self, id: &ClipId, l: Layout) {
        if self.rendering.get() {
            return;
        }
        let Some(clip) = self.clips.get(&id.0) else {
            return;
        };
        let (fit_w, fit_h) = match clip_natural_size(clip) {
            Some((w, h)) => fit_size(w, h, self.canvas_w, self.canvas_h),
            None => (self.canvas_w as f64, self.canvas_h as f64),
        };
        let width = (l.scale * fit_w).round().max(1.0) as i32;
        let height = (l.scale * fit_h).round().max(1.0) as i32;
        set_clip_frame(clip, l.posx, l.posy, width, height);
        let _ = clip.set_child_property("alpha", &l.alpha.to_value());
        let _ = clip.set_child_property("volume", &l.volume.to_value());
        self.commit();
        self.touched();
    }

    /// The clip's aspect-fit size in canvas px (largest undistorted size), used to
    /// size the preview bounding box. None for audio-only / not-yet-prerolled clips.
    pub fn clip_fit_size(&self, id: &ClipId) -> Option<(u32, u32)> {
        let clip = self.clips.get(&id.0)?;
        let (nw, nh) = clip_natural_size(clip)?;
        let (fw, fh) = fit_size(nw, nh, self.canvas_w, self.canvas_h);
        Some((fw.round() as u32, fh.round() as u32))
    }

    /// The first time a clip is edited, replace GES's stretch-to-fill default with
    /// an aspect-correct, centered layout. No-op once the clip has been laid out
    /// (width child prop non-zero) or if the source size isn't known yet.
    pub fn ensure_laid_out(&mut self, id: &ClipId) {
        if self.rendering.get() {
            return;
        }
        let Some(clip) = self.clips.get(&id.0) else {
            return;
        };
        let cur_w = clip
            .child_property("width")
            .and_then(|v| v.get::<i32>().ok())
            .unwrap_or(0);
        if cur_w > 0 {
            return;
        }
        let Some((nw, nh)) = clip_natural_size(clip) else {
            return;
        };
        let (fw, fh) = fit_size(nw, nh, self.canvas_w, self.canvas_h);
        let posx = ((self.canvas_w as f64 - fw) / 2.0).round() as i32;
        let posy = ((self.canvas_h as f64 - fh) / 2.0).round() as i32;
        set_clip_frame(clip, posx, posy, fw.round() as i32, fh.round() as i32);
        self.commit();
        self.touched();
    }

    /// End time (start + duration) of the last clip on `track`, or zero.
    fn track_end(&self, track: usize) -> gst::ClockTime {
        self.layers
            .get(track)
            .map(|l| {
                l.clips()
                    .iter()
                    .map(|c| c.start() + c.duration())
                    .max()
                    .unwrap_or(gst::ClockTime::ZERO)
            })
            .unwrap_or(gst::ClockTime::ZERO)
    }

    /// Master output volume (0..1) for the whole preview, set on the internal
    /// playsink. This is the transport volume; per-clip volume is a child prop.
    pub fn set_master_volume(&self, v: f64) {
        if let Some(ps) = find_by_factory(self.pipeline.upcast_ref::<gst::Bin>(), "playsink") {
            ps.set_property("volume", v.clamp(0.0, 1.0));
        }
    }

    /// Start playback. Inert during a render (the pipeline is the encoder's).
    pub fn play(&self) -> Result<()> {
        if self.rendering.get() {
            return Ok(());
        }
        self.pipeline.set_state(gst::State::Playing)?;
        Ok(())
    }

    /// Pause playback. Inert during a render — pausing the render pipeline
    /// would freeze the export until the watchdog calls it stuck.
    pub fn pause(&self) -> Result<()> {
        if self.rendering.get() {
            return Ok(());
        }
        self.pipeline.set_state(gst::State::Paused)?;
        Ok(())
    }

    /// Fast seek (snaps to the nearest keyframe). Use DURING a scrub drag,
    /// where responsiveness beats precision; land with [`Self::seek_accurate`].
    pub fn seek(&self, pos: Duration) -> Result<()> {
        if self.rendering.get() {
            return Ok(());
        }
        self.pipeline.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            gst::ClockTime::from_nseconds(pos.as_nanos() as u64),
        )?;
        // seek_simple always plays at 1.0.
        self.rate.set(1.0);
        Ok(())
    }

    /// Frame-accurate seek — the displayed frame matches the requested time
    /// exactly (decodes from the previous keyframe). Use when a scrub drag
    /// ends, so the playhead and the picture agree.
    pub fn seek_accurate(&self, pos: Duration) -> Result<()> {
        if self.rendering.get() {
            return Ok(());
        }
        self.pipeline.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::ClockTime::from_nseconds(pos.as_nanos() as u64),
        )?;
        // seek_simple always plays at 1.0.
        self.rate.set(1.0);
        Ok(())
    }

    /// How long one composited preview frame lasts, in seconds, from the last
    /// frame that arrived; 1/25 s until one has. A measurement of what the
    /// preview produces, which is what a frame step walks past.
    pub fn frame_secs(&self) -> f64 {
        self.frame_ns.load(Ordering::Relaxed) as f64 / 1e9
    }

    /// Play forward at `rate` (1.0 = normal) from where the playhead is.
    /// Refuses a rate that is not finite and greater than zero: this engine
    /// has no reverse, and a negative rate would fail deep inside GES with
    /// nothing to show the user. Inert while rendering, like `play`.
    pub fn set_rate(&self, rate: f64) -> Result<()> {
        if !(rate.is_finite() && rate > 0.0) {
            anyhow::bail!("a playback rate must be above zero, not {rate}");
        }
        if self.rendering.get() {
            return Ok(());
        }
        let pos = self.position().unwrap_or(Duration::ZERO);
        // A full seek: seek_simple cannot carry a rate.
        self.pipeline.seek(
            rate,
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::SeekType::Set,
            gst::ClockTime::from_nseconds(pos.as_nanos() as u64),
            gst::SeekType::None,
            gst::ClockTime::NONE,
        )?;
        self.rate.set(rate);
        Ok(())
    }

    /// The rate the preview plays at: 1.0 unless [`Self::set_rate`] changed
    /// it since the last ordinary seek.
    pub fn rate(&self) -> f64 {
        self.rate.get()
    }

    /// Repaint the preview if an edit marked the timeline dirty since the last
    /// call. MUST be driven from a UI timer, never from the edit path: a slider
    /// drag fires dozens of edits/second, and one flush seek per edit floods the
    /// pipeline and freezes the app. Coalescing to the timer caps it to one seek
    /// per tick. No-op while actively playing (frames already flow). Every
    /// call also lets go of removed clips GES has finished with.
    pub fn refresh_preview(&self) {
        self.release_removed();
        if self.rendering.get() || !self.dirty.replace(false) {
            return;
        }
        let playing = self.pipeline.current_state() == gst::State::Playing;
        let at_end = matches!(
            (self.position(), self.duration()),
            (Some(p), Some(d)) if p + Duration::from_millis(60) >= d
        );
        if playing && !at_end {
            return;
        }
        let pos = self.position().unwrap_or(Duration::ZERO);
        // ACCURATE: this repaints the paused frame after an edit — snapping to
        // the nearest keyframe (KEY_UNIT) showed a frame that could be seconds
        // away from the displayed playhead time on long-GOP media.
        if self
            .pipeline
            .seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                gst::ClockTime::from_nseconds(pos.as_nanos() as u64),
            )
            .is_ok()
        {
            self.rate.set(1.0);
        }
    }

    /// Non-blocking check for a preview-pipeline ERROR (decoder death, missing
    /// plugin, sink failure). Drive from the UI timer: the preview otherwise
    /// just freezes silently — bus messages queue unread outside render mode.
    /// Returns a displayable message once per error.
    pub fn poll_preview_error(&self) -> Option<String> {
        let bus = self.pipeline.bus()?;
        while let Some(msg) = bus.pop_filtered(&[gst::MessageType::Error]) {
            if let gst::MessageView::Error(err) = msg.view() {
                return Some(format!(
                    "{} [{}]",
                    err.error(),
                    err.src().map(|s| s.name().to_string()).unwrap_or_default()
                ));
            }
        }
        None
    }

    pub fn position(&self) -> Option<Duration> {
        self.pipeline
            .query_position::<gst::ClockTime>()
            .map(|t| Duration::from_nanos(t.nseconds()))
    }

    pub fn duration(&self) -> Option<Duration> {
        let d = self.timeline.duration();
        (d.nseconds() > 0).then(|| Duration::from_nanos(d.nseconds()))
    }

    /// Start rendering the whole timeline to `path`. Switches the pipeline into
    /// render mode, so the live preview is unavailable until `end_render`.
    ///
    /// On ANY failure the preview is restored before returning — the sink is
    /// detached before the fallible steps, and leaving it detached used to kill
    /// the preview (black screen) for the rest of the session.
    /// Discard whatever the preview left on the pipeline's bus.
    ///
    /// [`Self::render_status`] reports the first end-of-stream or error it
    /// pops, so a preview that ran to the end of the timeline — or failed —
    /// would have its message read as the *next* render's outcome: the export
    /// dialog closes over a file that has barely started. Toggling the
    /// flushing flag drops every queued message; clearing it again is what
    /// lets the render post its own.
    pub(crate) fn flush_bus(&self) {
        if let Some(bus) = self.pipeline.bus() {
            bus.set_flushing(true);
            bus.set_flushing(false);
        }
    }

    pub fn begin_render(&self, path: &Path, settings: ExportSettings) -> Result<()> {
        // The blocking form of prepare_render + start_render, for callers with
        // no event loop to poll from (the tests, and the headless sequence
        // render). The interface uses the two-step form.
        self.prepare_render()?;
        let _ = self.pipeline.state(gst::ClockTime::from_seconds(3));
        self.start_render(path, settings)
    }

    /// Abort a running render as gracefully as possible: send EOS so the muxer
    /// can finalize (a hard Null teardown leaves an unplayable file with no
    /// moov atom), wait briefly, then restore the preview. Deletes the partial
    /// output when `delete_partial` is set. Also correct to call on a FAILED
    /// render for cleanup.
    pub fn cancel_render(&self, output: &Path, delete_partial: bool) -> Result<()> {
        // Only a file that is KEPT needs its moov atom: a partial that is
        // deleted on the next line isn't worth a 2 s UI freeze.
        if !delete_partial {
            let _ = self.pipeline.send_event(gst::event::Eos::new());
            if let Some(bus) = self.pipeline.bus() {
                // Give the muxer up to 2 s to flush and post EOS.
                let _ = bus.timed_pop_filtered(
                    gst::ClockTime::from_seconds(2),
                    &[gst::MessageType::Eos, gst::MessageType::Error],
                );
            }
        }
        let restore = self.end_render();
        if delete_partial {
            let _ = std::fs::remove_file(output);
        }
        restore
    }

    /// Step one of starting a render: tear the preview down and ask the
    /// pipeline for NULL, then RETURN. Coming straight from a playing preview
    /// the GPU/CUDA context is not released synchronously, so NVENC fails its
    /// session init ("Could not encode stream" → a 0-byte file) unless the NULL
    /// transition has completed — but waiting for it blocks the caller, and
    /// the caller is the interface thread. Poll [`render_ready`](Self::render_ready)
    /// from a timer instead, then call [`start_render`](Self::start_render).
    ///
    /// Transport and edits are inert from here until the preview is restored.
    pub fn prepare_render(&self) -> Result<()> {
        self.pipeline.set_state(gst::State::Null)?;
        self.rendering.set(true);
        Ok(())
    }

    /// Has the teardown from [`prepare_render`](Self::prepare_render) finished?
    pub fn render_ready(&self) -> Step {
        settled(&self.pipeline)
    }

    /// Step two: route the timeline to the encoder and roll. Only call this
    /// once [`render_ready`](Self::render_ready) is `Ready`.
    pub fn start_render(&self, path: &Path, settings: ExportSettings) -> Result<()> {
        // Drop the custom preview sink so render mode can route to encodebin.
        self.pipeline.preview_set_video_sink(None::<&gst::Element>);
        // Start from a clean bus: anything the preview or the teardown left
        // queued would otherwise be read as this render's result.
        self.flush_bus();
        self.rendering.set(true);
        *self.last_encoder.borrow_mut() = None;
        let attempt = (|| -> Result<()> {
            let uri = gst::glib::filename_to_uri(path, None)?;
            let profile = encoding_profile(settings);
            self.pipeline.set_render_settings(uri.as_str(), &profile)?;
            self.pipeline.set_mode(ges::PipelineFlags::RENDER)?;
            self.pipeline.set_state(gst::State::Playing)?;
            Ok(())
        })();
        if attempt.is_err() {
            let _ = self.end_render();
        }
        attempt
    }

    /// Start restoring the live preview and RETURN; poll
    /// [`restore_ready`](Self::restore_ready) for when it has prerolled. Until
    /// it has, transport and edits stay inert: an edit landing while the
    /// composition is mid-transition made GES dereference a freed source asset
    /// (STATUS_ACCESS_VIOLATION).
    pub fn begin_restore(&self) -> Result<()> {
        self.pipeline.set_state(gst::State::Null)?;
        self.pipeline
            .preview_set_video_sink(Some(self.appsink.upcast_ref::<gst::Element>()));
        self.pipeline.set_mode(ges::PipelineFlags::FULL_PREVIEW)?;
        self.pipeline.set_state(gst::State::Paused)?;
        // The pipeline went through NULL: it plays at 1.0 again.
        self.rate.set(1.0);
        Ok(())
    }

    /// Has the preview prerolled? Hands the timeline back (transport and edits
    /// live again) the first time it says `Ready`.
    pub fn restore_ready(&self) -> Step {
        let step = settled(&self.pipeline);
        if step == Step::Ready && self.rendering.get() {
            self.rendering.set(false);
            self.touched();
        }
        step
    }

    /// Poll render progress — drive this from a UI timer. Consumes EOS/ERROR bus
    /// messages, so once it returns Done/Failed the render is finished.
    pub fn render_status(&self) -> RenderStatus {
        // encodebin builds its chain as the pipeline rolls, so the encoder is
        // not there the instant the render starts; catch it on the first poll
        // that finds it and keep it for the report at the end.
        if self.last_encoder.borrow().is_none() {
            if let Some(name) = encoder_in(self.pipeline.upcast_ref::<gst::Bin>()) {
                *self.last_encoder.borrow_mut() = Some(name);
            }
        }
        if let Some(bus) = self.pipeline.bus() {
            while let Some(msg) =
                bus.pop_filtered(&[gst::MessageType::Eos, gst::MessageType::Error])
            {
                match msg.view() {
                    gst::MessageView::Eos(_) => return RenderStatus::Done,
                    gst::MessageView::Error(err) => {
                        return RenderStatus::Failed(format!(
                            "{} [{}] ({})",
                            err.error(),
                            err.src().map(|s| s.name().to_string()).unwrap_or_default(),
                            err.debug().unwrap_or_default()
                        ))
                    }
                    _ => {}
                }
            }
        }
        let frac = match (self.position(), self.duration()) {
            (Some(p), Some(d)) if d.as_secs_f32() > 0.0 => {
                (p.as_secs_f32() / d.as_secs_f32()).clamp(0.0, 1.0)
            }
            _ => 0.0,
        };
        RenderStatus::Rendering(frac)
    }

    /// The encoder the running (or last finished) render used, by element name
    /// — "nvautogpuh264enc", "x264enc", "vp9enc". None before one has rolled.
    pub fn render_encoder(&self) -> Option<String> {
        self.last_encoder.borrow().clone()
    }

    /// Stop rendering and restore the live preview (re-attaching the appsink).
    pub fn end_render(&self) -> Result<()> {
        // The blocking form of begin_restore + restore_ready, for callers with
        // no event loop to poll from.
        self.begin_restore()?;
        let _ = self.pipeline.state(gst::ClockTime::from_seconds(5));
        self.rendering.set(false);
        self.touched();
        Ok(())
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    /// One second, in nanoseconds — the unit every timeline number here is in.
    const S: i128 = 1_000_000_000;

    /// Discovery blocks forever by default, so one unreachable network file
    /// used to stall the import worker - and every import queued behind it -
    /// for the rest of the session, with cancel unable to free it.
    /// Thumbnailing learned this lesson long ago; discovery had not.
    #[test]
    fn discovery_gives_up_instead_of_blocking_forever() {
        gst::init().unwrap();
        ges::init().unwrap();
        ensure_discovery_timeout();
        assert_eq!(
            ges::DiscovererManager::default().timeout(),
            Some(gst::ClockTime::from_seconds(DISCOVERY_TIMEOUT_SECS))
        );
    }

    #[test]
    fn a_clip_on_an_empty_layer_slides_freely() {
        assert_eq!(slide_within_gap(2 * S, S, 3 * S, &[]), 5 * S);
        assert_eq!(slide_within_gap(2 * S, S, -S, &[]), S);
    }

    #[test]
    fn sliding_past_the_start_of_the_timeline_stops_at_zero() {
        assert_eq!(slide_within_gap(2 * S, S, -5 * S, &[]), 0);
    }

    /// GES stacks whatever it is told to stack: the later clip simply hides the
    /// earlier one, with nothing on screen to say so.
    #[test]
    fn a_clip_stops_against_the_neighbour_on_its_right() {
        // [2,3) sliding right into a neighbour at [5,8).
        let neighbours = [(5 * S, 8 * S)];
        assert_eq!(slide_within_gap(2 * S, S, 10 * S, &neighbours), 4 * S);
    }

    #[test]
    fn a_clip_stops_against_the_neighbour_on_its_left() {
        // [6,7) sliding left into a neighbour at [1,4).
        let neighbours = [(S, 4 * S)];
        assert_eq!(slide_within_gap(6 * S, S, -10 * S, &neighbours), 4 * S);
    }

    #[test]
    fn a_clip_is_confined_to_the_gap_it_is_already_in() {
        // [4,6) between [0,4) and [6,9): it cannot move at all.
        let neighbours = [(0, 4 * S), (6 * S, 9 * S)];
        assert_eq!(slide_within_gap(4 * S, 2 * S, 3 * S, &neighbours), 4 * S);
        assert_eq!(slide_within_gap(4 * S, 2 * S, -3 * S, &neighbours), 4 * S);
    }

    /// Clips that already overlap (a project from before this rule) must not be
    /// frozen in place: the move is clamped to zero and nothing else.
    #[test]
    fn an_already_overlapping_clip_can_still_be_moved() {
        let neighbours = [(0, 10 * S)];
        assert_eq!(slide_within_gap(2 * S, S, 3 * S, &neighbours), 5 * S);
    }

    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Scratch space under the workspace's target dir — not `%TEMP%`, whose
    /// 8.3 short path on the hosted runner trips GStreamer's URI opener — and
    /// unique per process, so parallel `cargo test` invocations don't collide
    /// on output names.
    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!("{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// Pure trim math: no input may produce a negative (u64-wrapping) value.
    /// These are the exact wraps the audit found reachable via short clips.
    #[test]
    fn trim_math_never_goes_negative() {
        let s = 1_000_000_000i128; // 1 s in ns

        // Left edge, clip already SHORTER than the 0.2 s minimum at the origin:
        // the min-duration override used to push inpoint/start negative.
        let (ns, ni, nd) = trim_left_math(0, 0, 100_000_000, 50_000_000, 1.0);
        assert!(ns >= 0 && ni >= 0 && nd >= 0, "wrapped: {ns} {ni} {nd}");

        // Left edge, ordinary trim: end stays fixed.
        let (ns, ni, nd) = trim_left_math(2 * s, s, 5 * s, s, 1.0);
        assert_eq!((ns, ni, nd), (3 * s, 2 * s, 4 * s));
        assert_eq!(ns + nd, 2 * s + 5 * s, "right end moved");

        // Left edge can't trim before the source origin.
        let (ns, ni, nd) = trim_left_math(3 * s, s, 5 * s, -2 * s, 1.0);
        assert_eq!(ni, 0, "inpoint clamped to source start");
        assert!(ns >= 0 && nd >= 0);

        // Right edge: inpoint at/past max-duration used to underflow.
        let nd = trim_right_math(10 * s, 5 * s, s, Some(8 * s), 1.0);
        assert!(nd >= 0, "wrapped: {nd}");

        // Right edge respects the minimum when there's room.
        let nd = trim_right_math(0, s, -10 * s, Some(100 * s), 1.0);
        assert_eq!(nd, MIN_TRIM_NS);
    }

    #[test]
    fn trim_math_follows_the_rate() {
        // Right edge at 2x: a 10 s source from its head fills 5 s of timeline.
        assert_eq!(trim_right_math(0, 2 * S, 20 * S, Some(10 * S), 2.0), 5 * S);
        // At 0.5x the same source fills 20 s.
        assert_eq!(trim_right_math(0, 2 * S, 30 * S, Some(10 * S), 0.5), 20 * S);
        // The source still wins over the minimum: 0.1 s of source left is
        // 0.05 s at 2x.
        assert_eq!(
            trim_right_math(9_900_000_000, 50_000_000, -S, Some(10 * S), 2.0),
            50_000_000
        );
        // Left edge at 2x: the in-point moves twice as far as the start.
        assert_eq!(
            trim_left_math(2 * S, S, 5 * S, S, 2.0),
            (3 * S, 3 * S, 4 * S)
        );
        // Out past the source origin: it stops where the in-point reaches
        // zero, half a second of timeline for one second of source.
        assert_eq!(
            trim_left_math(3 * S, S, 5 * S, -2 * S, 2.0),
            (3 * S - S / 2, 0, 5 * S + S / 2)
        );
        // Never negative at any rate.
        let (ns, ni, nd) = trim_left_math(0, 0, 100_000_000, 50_000_000, 2.0);
        assert!(ns >= 0 && ni >= 0 && nd >= 0, "wrapped: {ns} {ni} {nd}");
        // At 1x nothing changed.
        assert_eq!(
            trim_left_math(2 * S, S, 5 * S, S, 1.0),
            (3 * S, 2 * S, 4 * S)
        );
    }

    #[test]
    fn split_needs_the_minimum_on_both_sides() {
        // A clip at [1 s, 3 s).
        assert!(!split_fits(S, 2 * S, S), "at its start");
        assert!(
            !split_fits(S, 2 * S, S + MIN_TRIM_NS - 1),
            "a nanosecond too close to the start"
        );
        assert!(
            split_fits(S, 2 * S, S + MIN_TRIM_NS),
            "the minimum from the start"
        );
        assert!(split_fits(S, 2 * S, 2 * S), "the middle");
        assert!(
            split_fits(S, 2 * S, 3 * S - MIN_TRIM_NS),
            "the minimum from the end"
        );
        assert!(
            !split_fits(S, 2 * S, 3 * S - MIN_TRIM_NS + 1),
            "a nanosecond too close to the end"
        );
        assert!(!split_fits(S, 2 * S, 5 * S), "outside it");
        // 0.3 s cannot leave 0.2 s on both sides.
        assert!(!split_fits(0, 300_000_000, 150_000_000));
    }

    /// remove_clip deletes from GES + the map, prunes empty trailing layers,
    /// Both ends of an export used to freeze the window for seconds: starting
    /// waits for the preview pipeline to reach NULL (the NVENC context is not
    /// released synchronously) and finishing waits for the preview to preroll
    /// again, and both waits ran on whichever thread called them — the
    /// interface thread. Each is now a step plus a poll, so the caller can wait
    /// from a timer and stay responsive. Needs only GStreamer: the still is
    /// generated here.
    #[test]
    fn a_render_starts_and_finishes_without_blocking_the_caller() {
        let dir = scratch("two-step");
        let png = dir.join("still.png");
        image::RgbaImage::from_pixel(160, 120, image::Rgba([40, 160, 200, 255]))
            .save(&png)
            .expect("write still");
        let out = dir.join("out.mp4");
        let mut project = Project::new(|_f| {}).expect("project");
        let info = project.append_clip(&png, 0, None).expect("append still");
        project.set_clip_duration(&info.id, 1.0).expect("1 s still");

        // Step one returns at once; the poll says when the teardown is done.
        project.prepare_render().expect("prepare");
        let settled = |what: &str, poll: &dyn Fn() -> Step| {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while poll() == Step::Pending && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(poll(), Step::Ready, "{what} never settled");
        };
        settled("the teardown", &|| project.render_ready());

        project
            .start_render(
                &out,
                ExportSettings {
                    codec: VideoCodec::H264,
                    width: 320,
                    height: 240,
                    fps: 24,
                    bitrate_kbps: 2000,
                    encoder: Encoder::Auto,
                },
            )
            .expect("start_render");
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            match project.render_status() {
                RenderStatus::Done => break,
                RenderStatus::Failed(e) => panic!("render failed: {e}"),
                RenderStatus::Rendering(_) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "render never finished"
                    );
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }

        // ...and the preview comes back the same way.
        project.begin_restore().expect("begin_restore");
        settled("the preview", &|| project.restore_ready());
        assert!(
            out.metadata().map(|m| m.len()).unwrap_or(0) > 1000,
            "the render wrote nothing usable"
        );
        // The timeline is live again: transport works and edits are accepted.
        project.play().expect("play after restore");
        project.pause().expect("pause after restore");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unsaved_work_survives_a_repaint_unlike_the_repaint_flag() {
        let mut project = Project::new(|_f| {}).expect("project");
        assert!(
            !project.has_unsaved_work(),
            "a new project has nothing to lose"
        );

        project.set_canvas_size(1280, 720);
        assert!(project.has_unsaved_work(), "an edit is unsaved work");

        // The repaint flag clears on a timer. Unsaved work must not.
        project.refresh_preview();
        assert!(
            project.has_unsaved_work(),
            "a repaint is not a save: this is the bug this flag exists to fix"
        );
    }

    #[test]
    fn saving_and_loading_both_clear_unsaved_work() {
        let mut project = Project::new(|_f| {}).expect("project");
        project.set_canvas_size(1600, 900);
        assert!(project.has_unsaved_work());

        let doc = project.to_document();
        project.mark_saved();
        assert!(!project.has_unsaved_work(), "saving clears it");

        project.set_canvas_size(1280, 720);
        assert!(project.has_unsaved_work());
        project.apply_document(&doc).expect("apply");
        assert!(
            !project.has_unsaved_work(),
            "a project just loaded from a file matches that file"
        );
    }

    /// A saved project has to come back as the same timeline: same sources, on
    /// the same tracks, at the same times, with the same transforms. The whole
    /// point is that an evening's arranging survives closing the window.
    #[test]
    fn a_timeline_survives_being_saved_and_reopened() {
        let dir = scratch("save-load");
        let png = dir.join("still.png");
        image::RgbaImage::from_pixel(120, 90, image::Rgba([10, 120, 200, 255]))
            .save(&png)
            .expect("write still");
        let file = dir.join("cut.kuvatin");

        let mut project = Project::new(|_f| {}).expect("project");
        project.set_canvas_size(1280, 720);
        let a = project.append_clip(&png, 0, None).expect("clip a");
        project.set_clip_duration(&a.id, 3.0).expect("3 s");
        // A second clip on its own track, offset, with a transform of its own.
        let b = project
            .add_clip(
                &png,
                1,
                Duration::from_millis(1500),
                Duration::ZERO,
                Duration::from_secs(2),
            )
            .expect("clip b");
        project.set_clip_layout(
            &b,
            Layout {
                posx: 40,
                posy: -20,
                scale: 0.5,
                alpha: 0.75,
                volume: 1.0,
            },
        );
        let before = project.to_document();
        assert_eq!(before.clips.len(), 2, "both clips are in the document");
        before.save(&file).expect("save");

        // A fresh engine, as if the window had been closed and reopened.
        let mut reopened = Project::new(|_f| {}).expect("project");
        let doc = crate::document::ProjectFile::load(&file).expect("load");
        reopened.apply_document(&doc).expect("apply");
        let after = reopened.to_document();

        assert_eq!(after.canvas_w, 1280);
        assert_eq!(after.canvas_h, 720);
        assert_eq!(after.clips.len(), 2);
        for (was, now) in before.clips.iter().zip(after.clips.iter()) {
            assert_eq!(was.uri, now.uri, "same source");
            assert_eq!(was.track, now.track, "same track");
            assert!((was.start - now.start).abs() < 1e-6, "{was:?} vs {now:?}");
            assert!(
                (was.duration - now.duration).abs() < 1e-6,
                "{was:?} vs {now:?}"
            );
            assert!(
                (was.inpoint - now.inpoint).abs() < 1e-6,
                "{was:?} vs {now:?}"
            );
            assert!(
                (was.layout.scale - now.layout.scale).abs() < 1e-3
                    && (was.layout.alpha - now.layout.alpha).abs() < 1e-3
                    && was.layout.posx == now.layout.posx,
                "transform: {:?} vs {:?}",
                was.layout,
                now.layout
            );
        }
        // Applying a document REPLACES the timeline rather than adding to it.
        reopened.apply_document(&doc).expect("apply again");
        assert_eq!(reopened.to_document().clips.len(), 2, "not four");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A project whose media has moved says which file it could not find, and
    /// still opens with everything that is still there.
    #[test]
    fn a_missing_source_is_named_and_the_rest_still_opens() {
        let dir = scratch("save-missing");
        let png = dir.join("still.png");
        image::RgbaImage::from_pixel(64, 64, image::Rgba([200, 40, 40, 255]))
            .save(&png)
            .expect("write still");
        let mut project = Project::new(|_f| {}).expect("project");
        project.append_clip(&png, 0, None).expect("clip");
        let mut doc = project.to_document();
        // Point a second clip at something that was never there.
        let mut ghost = doc.clips[0].clone();
        ghost.uri = ghost.uri.replace("still.png", "gone.png");
        ghost.name = "gone.png".into();
        ghost.track = 1;
        doc.clips.push(ghost);

        let mut reopened = Project::new(|_f| {}).expect("project");
        let missing = reopened.apply_document(&doc).expect("apply");
        assert_eq!(missing.len(), 1, "one source could not be opened");
        assert!(missing[0].contains("gone.png"), "{missing:?}");
        assert_eq!(
            reopened.to_document().clips.len(),
            1,
            "the clip that exists is still there"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The choice is carried out by pinning an encoder factory in the profile.
    /// Ranks cannot do it: encodebin builds its candidate list once and caches
    /// it, so a rank set afterwards is invisible — measured, with a render that
    /// used the hardware encoder while its rank was NONE.
    #[test]
    fn the_encoder_choice_pins_a_factory() {
        assert_eq!(pinned_encoder(Encoder::Auto, VideoCodec::H264), None);
        assert_eq!(
            pinned_encoder(Encoder::Software, VideoCodec::H264),
            Some(SOFTWARE_H264)
        );
        assert_eq!(
            pinned_encoder(Encoder::Hardware, VideoCodec::H264),
            Some(HARDWARE_H264)
        );
        assert_eq!(
            pinned_encoder(Encoder::Software, VideoCodec::Vp9),
            Some("vp9enc")
        );
        // There is no hardware VP8/VP9 encoder in the bundled runtime, so
        // "hardware" there means "whatever Auto would have taken".
        assert_eq!(pinned_encoder(Encoder::Hardware, VideoCodec::Vp9), None);
    }

    #[test]
    fn a_hardware_encoder_is_recognised_by_name() {
        assert!(is_hardware_encoder("nvautogpuh264enc"));
        assert!(is_hardware_encoder("qsvh264enc"));
        assert!(is_hardware_encoder("amfh264enc"));
        assert!(!is_hardware_encoder("x264enc"));
        assert!(!is_hardware_encoder("vp9enc"));
        assert!(!is_hardware_encoder("openh264enc"));
    }

    /// "Which encoder did that use?" was unanswerable: the export either
    /// worked or failed with a raw engine message. A finished render now knows.
    #[test]
    fn a_render_reports_the_encoder_it_used() {
        let dir = scratch("encoder-name");
        let png = dir.join("still.png");
        image::RgbaImage::from_pixel(160, 120, image::Rgba([90, 160, 60, 255]))
            .save(&png)
            .expect("write still");
        let out = dir.join("out.mp4");
        let mut project = Project::new(|_f| {}).expect("project");
        let info = project.append_clip(&png, 0, None).expect("append still");
        project.set_clip_duration(&info.id, 1.0).expect("1 s");
        project
            .begin_render(
                &out,
                ExportSettings {
                    codec: VideoCodec::H264,
                    width: 320,
                    height: 240,
                    fps: 24,
                    bitrate_kbps: 2000,
                    // Software: the only choice every machine (and every CI
                    // runner) can actually satisfy.
                    encoder: Encoder::Software,
                },
            )
            .expect("begin_render");
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            match project.render_status() {
                RenderStatus::Done => break,
                RenderStatus::Failed(e) => panic!("render failed: {e}"),
                RenderStatus::Rendering(_) => {
                    assert!(std::time::Instant::now() < deadline, "never finished");
                    std::thread::sleep(Duration::from_millis(40));
                }
            }
        }
        let used = project.render_encoder().expect("an encoder was recorded");
        assert!(
            used.contains("264"),
            "expected an H.264 encoder, got {used}"
        );
        assert!(
            !is_hardware_encoder(&used),
            "{used} is not the software one"
        );
        let _ = project.end_render();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A still's length is a free choice: `set_clip_duration` applies it
    /// exactly, clamps a silly value up to the trim minimum, and reports
    /// what it applied. Needs only GStreamer (the still is generated here).
    #[test]
    fn set_clip_duration_sets_and_clamps_a_still() {
        let dir = scratch("set-dur");
        let png = dir.join("still.png");
        image::RgbaImage::from_pixel(64, 48, image::Rgba([200, 80, 40, 255]))
            .save(&png)
            .expect("write still");
        let mut project = Project::new(|_f| {}).expect("project");
        let info = project.append_clip(&png, 0, None).expect("append still");
        let geom = project.set_clip_duration(&info.id, 8.0).expect("set 8 s");
        assert!(
            (geom.duration.as_secs_f64() - 8.0).abs() < 1e-6,
            "got {:?}",
            geom.duration
        );
        assert_eq!(project.duration(), Some(geom.duration), "timeline follows");
        let tiny = project
            .set_clip_duration(&info.id, 0.001)
            .expect("set tiny");
        assert_eq!(tiny.duration.as_nanos() as i128, MIN_TRIM_NS, "clamped up");
        assert!(project.set_clip_duration(&info.id, f64::NAN).is_none());
        assert!(project
            .set_clip_duration(&ClipId("nope".into()), 3.0)
            .is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// and the remaining clips keep working. Self-skips without `GST_TEST_FILE`.
    #[test]
    fn removes_clips_and_prunes_trailing_layers() {
        let Some(path) = std::env::var_os("GST_TEST_FILE") else {
            eprintln!("skipping removes_clips_and_prunes_trailing_layers: set GST_TEST_FILE");
            return;
        };
        let path = std::path::PathBuf::from(path);
        let mut project = Project::new(|_f| {}).expect("project");
        let a = project.append_clip(&path, 0, None).expect("a");
        let b = project.append_clip(&path, 1, None).expect("b");
        let c = project.append_clip(&path, 2, None).expect("c");
        assert_eq!(project.track_count(), 3);

        // Removing the middle clip must not shift the other tracks.
        assert!(project.remove_clip(&b.id));
        assert_eq!(project.track_count(), 3, "middle layer kept (not trailing)");
        assert_eq!(project.clip_track(&c.id), Some(2));

        // Removing the bottom clip prunes the now-empty trailing layers (2 and 1).
        assert!(project.remove_clip(&c.id));
        assert_eq!(project.track_count(), 1, "empty trailing layers pruned");
        assert_eq!(project.clip_track(&a.id), Some(0));

        // Unknown id is a no-op; the survivor still slides fine.
        assert!(!project.remove_clip(&ClipId("nope".into())));
        assert!(project.slide_clip(&a.id, 1.0).is_some());
    }

    /// Adds a clip to a project and asserts the preview produces frames. Self-
    /// skips without `GST_TEST_FILE`; needs the GStreamer `bin` on PATH.
    #[test]
    fn previews_a_clip() {
        let Some(path) = std::env::var_os("GST_TEST_FILE") else {
            eprintln!("skipping previews_a_clip: set GST_TEST_FILE");
            return;
        };
        let count = Arc::new(AtomicU32::new(0));
        let c2 = count.clone();
        let mut project = Project::new(move |f| {
            assert!(f.data.len() as u32 >= f.width * f.height * 4);
            c2.fetch_add(1, Ordering::SeqCst);
        })
        .expect("project");
        project
            .add_clip(
                Path::new(&path),
                0,
                Duration::ZERO,
                Duration::ZERO,
                Duration::from_secs(2),
            )
            .expect("add_clip");
        project.play().expect("play");
        std::thread::sleep(Duration::from_millis(1200));
        assert!(count.load(Ordering::SeqCst) > 0, "no preview frames");
        assert!(project.duration().unwrap() > Duration::ZERO);
    }

    /// Reproduces the drag-two-files freeze: append a clip, start playing, then
    /// append a second clip (which commits the timeline while the pipeline is
    /// playing). Self-skips without `GST_TEST_FILE`; needs GStreamer on PATH.
    #[test]
    fn append_two_while_playing() {
        let Some(path) = std::env::var_os("GST_TEST_FILE") else {
            eprintln!("skipping append_two_while_playing: set GST_TEST_FILE");
            return;
        };
        let path = std::path::PathBuf::from(path);
        let mut project = Project::new(|_f| {}).expect("project");
        project.append_clip(&path, 0, None).expect("append1");
        project.play().expect("play");
        // No sleep: append #2 must not block while the pipeline is still
        // prerolling. With commit_sync() this deadlocked the calling thread.
        let info2 = project.append_clip(&path, 0, None).expect("append2");
        assert!(
            info2.start > Duration::ZERO,
            "second clip should start after the first"
        );
    }

    /// Adding a clip starts the preview (`add_to_timeline` plays), and a
    /// Delete or a Ctrl+Z straight after took the clip away while the
    /// composition was still bringing its source up. GES queues the source's
    /// removal behind that preroll, but the engine let go of the clip at
    /// once, which freed the GES track element the source's streaming threads
    /// still call back into: STATUS_ACCESS_VIOLATION, every time. This is the
    /// interface's sequence — Delete's `remove_timeline_clip` and undo's
    /// `apply_step` both come down to `remove_clip` — down to the 100 ms
    /// timer that keeps ticking over what is left.
    #[test]
    fn removing_a_clip_straight_after_adding_it_does_not_crash() {
        let dir = scratch("remove-at-once");
        let png = dir.join("still.png");
        image::RgbaImage::from_pixel(120, 90, image::Rgba([10, 120, 200, 255]))
            .save(&png)
            .expect("write still");
        let mut project = Project::new(|_f| {}).expect("project");
        // add_to_timeline: undo reads the records around the add, then it plays.
        let _ = project.clip_records();
        let added = project
            .append_clip(&png, 0, Some(Duration::from_secs(5)))
            .expect("append");
        let _ = project.clip_records();
        project.play().expect("play");
        let _ = project.duration();
        let clip = project.clips[&added.id.0].downgrade();
        // No wait: the removal lands while the preview is still starting.
        let _ = project.clip_records();
        assert!(project.remove_clip(&added.id));
        assert!(
            project.clip_records().is_empty(),
            "the model loses it at once"
        );
        let _ = project.duration();
        // Ask for the release as hard as the interface ever will, while the
        // composition is still bringing the source up: a rule that lets go
        // too early lets go here, and the crash follows. That is this test's
        // job — it reproduces the crash, it does not gate the rule. Nothing
        // older is in flight in this shape, so the wrong rule's window (an
        // older commit answering for this removal) never opens; the tests
        // below, with commits still in flight, are what hold the rule.
        let spin = std::time::Instant::now() + Duration::from_millis(300);
        while std::time::Instant::now() < spin {
            project.release_removed();
        }
        for tick in 0..50 {
            project.refresh_preview();
            let _ = project.poll_preview_error();
            let _ = project.position();
            std::thread::sleep(Duration::from_millis(100));
            if tick >= 19 && clip.upgrade().is_none() {
                break;
            }
        }
        assert!(
            clip.upgrade().is_none(),
            "the engine lets the clip go once the pipeline has"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same crash one step later: with an image playing, a video dropped
    /// onto the empty track under it and taken straight back out. Its source
    /// had no parent and no state yet when it went — the composition had it
    /// queued but no stack built around it — and the update the add's commit
    /// had started brought it up after the engine had let the clip go. So
    /// "not in a stack" is not "done with": every track's own `commited` for
    /// the removal's commit is.
    ///
    /// The transform before each removal is applied while the clip is still
    /// prerolling, on purpose: that is the sequence that deadlocked GES (a
    /// hang, not a crash) until `set_clip_frame` held the frame positioner's
    /// deep-notify walk back. Run it under CPU load to see either fault.
    #[test]
    fn removing_a_clip_added_while_playing_does_not_crash() {
        let dir = scratch("remove-while-playing");
        let png = dir.join("still.png");
        image::RgbaImage::from_pixel(120, 90, image::Rgba([10, 120, 200, 255]))
            .save(&png)
            .expect("write still");
        let mut project = Project::new(|_f| {}).expect("project");
        let first = project
            .append_clip(&png, 0, Some(Duration::from_secs(5)))
            .expect("first")
            .id;
        project.play().expect("play");
        // Playing for a while, as when the user reaches for a second file.
        let end = std::time::Instant::now() + Duration::from_secs(10);
        while settled(&project.pipeline) == Step::Pending && std::time::Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut removed = Vec::new();
        for _ in 0..5 {
            // add_to_timeline for a video (a still stands in): the base track, then play.
            let added = project
                .append_clip(&png, 1, Some(Duration::from_secs(5)))
                .expect("second");
            project.play().expect("play");
            // The inspector's transform, as a slider drag leaves behind, and
            // straight away: the clip is still coming up, which is when a
            // width or height write met the composition's thread head-on and
            // deadlocked GES for good (see `set_clip_frame`; 5 hangs in 25
            // under load before the guard). It also leaves more commits in
            // flight when the removal lands, so an older one's `commited`
            // must not be read as this removal's.
            for i in 0..3 {
                project.set_clip_layout(
                    &added.id,
                    Layout {
                        posx: i * 4,
                        posy: 0,
                        scale: 0.5,
                        alpha: 1.0,
                        volume: 1.0,
                    },
                );
            }
            removed.push(project.clips[&added.id.0].downgrade());
            assert!(project.remove_clip(&added.id));
            // Tight poll, with older commits still in flight: the clip stays
            // until every track has finished the removal's own commit.
            let stamp = project.removed.borrow().last().expect("held").1;
            let spin = std::time::Instant::now() + Duration::from_millis(100);
            while std::time::Instant::now() < spin {
                project.release_removed();
                if project.commits_done() < stamp {
                    assert!(
                        project.removed.borrow().iter().any(|(_, s)| *s == stamp),
                        "the clip went before every track had finished its removal's commit"
                    );
                }
            }
            for _ in 0..3 {
                project.refresh_preview();
                let _ = project.poll_preview_error();
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        let records = project.clip_records();
        assert_eq!(records.len(), 1, "only the first clip is left");
        assert_eq!(records[0].0, first);
        for _ in 0..50 {
            if removed.iter().all(|c| c.upgrade().is_none()) {
                break;
            }
            project.refresh_preview();
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            removed.iter().all(|c| c.upgrade().is_none()),
            "the engine lets every removed clip go once the pipeline has"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undo takes a whole step back at once: `apply_step` loops over its
    /// removals, and two quick Deletes do the same. The second removal is
    /// what freed the first clip, because `remove_clip` releases as it goes
    /// and nothing waited for the timeline in between. Two clips added under
    /// a playing one and both taken away back to back, with only the
    /// interface's 100 ms tick afterwards.
    #[test]
    fn removing_two_clips_back_to_back_while_playing_does_not_crash() {
        let dir = scratch("remove-two-at-once");
        let png = dir.join("still.png");
        image::RgbaImage::from_pixel(120, 90, image::Rgba([10, 120, 200, 255]))
            .save(&png)
            .expect("write still");
        let mut project = Project::new(|_f| {}).expect("project");
        let first = project
            .append_clip(&png, 0, Some(Duration::from_secs(5)))
            .expect("first")
            .id;
        project.play().expect("play");
        let end = std::time::Instant::now() + Duration::from_secs(10);
        while settled(&project.pipeline) == Step::Pending && std::time::Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut removed = Vec::new();
        for _ in 0..3 {
            // Two adds, each playing as add_to_timeline does.
            let a = project
                .append_clip(&png, 1, Some(Duration::from_secs(5)))
                .expect("a");
            project.play().expect("play");
            let b = project
                .append_clip(&png, 2, Some(Duration::from_secs(5)))
                .expect("b");
            project.play().expect("play");
            removed.push(project.clips[&a.id.0].downgrade());
            removed.push(project.clips[&b.id.0].downgrade());
            // Back to back, as one undo step: no tick in between.
            assert!(project.remove_clip(&a.id));
            assert!(project.remove_clip(&b.id));
            // The second removal must not take the first clip with it: both
            // stay until every track has finished the later removal's commit.
            let stamp = project.removed.borrow().last().expect("held").1;
            let spin = std::time::Instant::now() + Duration::from_millis(200);
            while std::time::Instant::now() < spin {
                project.release_removed();
                if project.commits_done() < stamp {
                    assert!(
                        project.removed.borrow().iter().any(|(_, s)| *s == stamp),
                        "a clip went before every track had finished its removal's commit"
                    );
                }
            }
            for _ in 0..5 {
                project.refresh_preview();
                let _ = project.poll_preview_error();
                let _ = project.position();
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        let records = project.clip_records();
        assert_eq!(records.len(), 1, "only the first clip is left");
        assert_eq!(records[0].0, first);
        for _ in 0..50 {
            if removed.iter().all(|c| c.upgrade().is_none()) {
                break;
            }
            project.refresh_preview();
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            removed.iter().all(|c| c.upgrade().is_none()),
            "the engine lets every removed clip go once the pipeline has"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Reproduces the "freeze when editing the overlay scale": append a clip,
    /// play, then apply transforms repeatedly (as a slider drag would). If an
    /// edit blocks, this hangs. Self-skips without `GST_TEST_FILE`.
    #[test]
    fn edit_transform_while_playing() {
        let Some(path) = std::env::var_os("GST_TEST_FILE") else {
            eprintln!("skipping edit_transform_while_playing: set GST_TEST_FILE");
            return;
        };
        let path = std::path::PathBuf::from(path);
        let mut project = Project::new(|_f| {}).expect("project");
        let info = project.append_clip(&path, 0, None).expect("append");
        project.play().expect("play");
        std::thread::sleep(Duration::from_millis(300));
        for i in 0..8 {
            let scale = 0.5 + (i as f64) * 0.05;
            project.set_clip_layout(
                &info.id,
                Layout {
                    posx: 0,
                    posy: 0,
                    scale,
                    alpha: 1.0,
                    volume: 1.0,
                },
            );
            project.refresh_preview();
        }
    }

    /// Renders a one-clip timeline to an MP4 and checks the file is written and
    /// non-trivial. Self-skips without `GST_TEST_FILE`.
    #[test]
    fn renders_to_mp4() {
        let Some(path) = std::env::var_os("GST_TEST_FILE") else {
            eprintln!("skipping renders_to_mp4: set GST_TEST_FILE");
            return;
        };
        let src = std::path::PathBuf::from(path);
        // WebM (VP9/Opus) is deterministic — no hardware H.264 encoder to fight
        // with under the suite's rapid pipeline churn. Exercises the render path.
        let out = scratch("render").join("out.webm");
        let _ = std::fs::remove_file(&out);
        let mut project = Project::new(|_f| {}).expect("project");
        project.append_clip(&src, 1, None).expect("clip");
        // VP9/WebM at a forced 640x360 with a target bitrate — exercises the codec,
        // the resolution restriction, and the bitrate element-property path.
        project
            .begin_render(
                &out,
                ExportSettings {
                    codec: VideoCodec::Vp9,
                    width: 640,
                    height: 360,
                    fps: 30,
                    bitrate_kbps: 1500,
                    encoder: Encoder::Auto,
                },
            )
            .expect("begin_render");
        let mut done = false;
        for _ in 0..300 {
            match project.render_status() {
                RenderStatus::Done => {
                    done = true;
                    break;
                }
                RenderStatus::Failed(e) => panic!("render failed: {e}"),
                RenderStatus::Rendering(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        assert!(done, "render did not finish in time");
        project.end_render().expect("end_render");
        let len = std::fs::metadata(&out).expect("output file").len();
        assert!(len > 1000, "output file too small: {len} bytes");
        let _ = std::fs::remove_file(&out);
    }

    /// A preview that reached the end of the timeline leaves an end-of-stream
    /// message sitting on the pipeline's bus. Nothing else drains it, so the
    /// render that follows must, or its very first status poll consumes the
    /// stale one and reports a finished export.
    #[test]
    fn flushing_the_bus_drops_a_stale_end_of_stream() {
        let project = Project::new(|_f| {}).expect("project");
        let bus = project.pipeline.bus().expect("bus");

        bus.post(gst::message::Eos::builder().build())
            .expect("post");
        assert!(
            bus.pop_filtered(&[gst::MessageType::Eos]).is_some(),
            "the fixture itself must queue a message"
        );

        bus.post(gst::message::Eos::builder().build())
            .expect("post");
        project.flush_bus();
        assert!(
            bus.pop_filtered(&[gst::MessageType::Eos]).is_none(),
            "a stale end-of-stream survived the flush"
        );
    }

    /// The same hazard through the real entry point: with a stale message
    /// queued, the first poll of a freshly started render must still say it is
    /// rendering. Needs only a generated still, so it gates the build.
    #[test]
    fn a_stale_end_of_stream_does_not_finish_the_next_render() {
        let dir = scratch("stale-eos");
        let png = dir.join("still.png");
        image::RgbaImage::from_pixel(160, 120, image::Rgba([30, 90, 160, 255]))
            .save(&png)
            .expect("write still");
        let out = dir.join("out.mp4");

        let mut project = Project::new(|_f| {}).expect("project");
        let info = project.append_clip(&png, 0, None).expect("append still");
        // Long enough that a genuine end-of-stream cannot arrive in the
        // microseconds between starting the render and the first poll.
        project
            .set_clip_duration(&info.id, 60.0)
            .expect("stretch the still");

        project
            .pipeline
            .bus()
            .expect("bus")
            .post(gst::message::Eos::builder().build())
            .expect("post");

        project
            .begin_render(
                &out,
                ExportSettings {
                    codec: VideoCodec::H264,
                    width: 160,
                    height: 120,
                    fps: 24,
                    bitrate_kbps: 1000,
                    encoder: Encoder::Auto,
                },
            )
            .expect("begin_render");
        let first = project.render_status();
        let _ = project.cancel_render(&out, true);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            !matches!(first, RenderStatus::Done),
            "the render consumed a stale end-of-stream: {first:?}"
        );
    }

    /// Regression for the 0-byte H.264 export: play the preview (as the GUI does),
    /// then render to MP4 in-process. The original failure was the Media Foundation
    /// AAC encoder (mfaacenc) winning a rank tie and breaking NVENC's session init;
    /// deranking it (see `ensure_encoder_ranks`) makes hardware H.264 export work even
    /// after a preview. Self-skips without `GST_TEST_FILE`.
    #[test]
    fn renders_after_preview_eos() {
        let Some(path) = std::env::var_os("GST_TEST_FILE") else {
            eprintln!("skipping renders_after_preview_eos: set GST_TEST_FILE");
            return;
        };
        let src = std::path::PathBuf::from(path);
        let out = scratch("eos").join("out.mp4");
        let _ = std::fs::remove_file(&out);
        let mut project = Project::new(|_f| {}).expect("project");
        project.append_clip(&src, 1, None).expect("clip");
        // Play the whole clip so the pipeline posts EOS onto the bus.
        project.play().expect("play");
        std::thread::sleep(Duration::from_secs(6));
        project.pause().expect("pause");
        project
            .begin_render(
                &out,
                ExportSettings {
                    codec: VideoCodec::H264,
                    width: 1280,
                    height: 720,
                    fps: 30,
                    bitrate_kbps: 8000,
                    encoder: Encoder::Auto,
                },
            )
            .expect("begin_render");
        // If the first poll returns Done, a stale EOS was consumed (the bug).
        let first = project.render_status();
        eprintln!("first render_status after preview-EOS: {first:?}");
        let mut done = matches!(first, RenderStatus::Done);
        if !done {
            for _ in 0..300 {
                match project.render_status() {
                    RenderStatus::Done => {
                        done = true;
                        break;
                    }
                    RenderStatus::Failed(e) => panic!("render failed: {e}"),
                    RenderStatus::Rendering(_) => std::thread::sleep(Duration::from_millis(100)),
                }
            }
        }
        let _ = project.end_render();
        let len = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
        eprintln!("output bytes after preview-EOS render: {len} (done={done})");
        assert!(len > 1000, "0-byte/tiny export reproduced: {len} bytes");
        let _ = std::fs::remove_file(&out);
    }

    /// Regression for the 111KB/no-moov export: a REALISTIC timeline (a leading gap
    /// and a still-image overlay track) must render to H.264 MP4. Gap/overlay
    /// boundaries renegotiate caps mid-stream, which hardware NVENC chokes on unless
    /// the profile pins one constant format. Self-skips without `GST_TEST_FILE`
    /// (also needs `GST_TEST_IMAGE` for the overlay; skipped if unset).
    #[test]
    fn renders_gapped_overlay_timeline() {
        let Some(path) = std::env::var_os("GST_TEST_FILE") else {
            eprintln!("skipping renders_gapped_overlay_timeline: set GST_TEST_FILE");
            return;
        };
        let Some(img) = std::env::var_os("GST_TEST_IMAGE") else {
            eprintln!("skipping renders_gapped_overlay_timeline: set GST_TEST_IMAGE");
            return;
        };
        let src = std::path::PathBuf::from(path);
        let img = std::path::PathBuf::from(img);
        let out = scratch("gapped").join("out.mp4");
        let _ = std::fs::remove_file(&out);
        let mut project = Project::new(|_f| {}).expect("project");
        // Video on track 1 with a 2s leading gap; image overlay on track 0.
        project
            .add_clip(
                &src,
                1,
                Duration::from_secs(2),
                Duration::ZERO,
                Duration::from_secs(2),
            )
            .expect("video clip");
        project
            .add_clip(
                &img,
                0,
                Duration::ZERO,
                Duration::ZERO,
                Duration::from_secs(3),
            )
            .expect("image clip");
        project
            .begin_render(
                &out,
                ExportSettings {
                    codec: VideoCodec::H264,
                    width: 1280,
                    height: 720,
                    fps: 30,
                    bitrate_kbps: 8000,
                    encoder: Encoder::Auto,
                },
            )
            .expect("begin_render");
        let mut done = false;
        for _ in 0..600 {
            match project.render_status() {
                RenderStatus::Done => {
                    done = true;
                    break;
                }
                RenderStatus::Failed(e) => panic!("gapped/overlay render failed: {e}"),
                RenderStatus::Rendering(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        assert!(done, "render did not finish in time");
        project.end_render().expect("end_render");
        let len = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
        eprintln!("gapped/overlay render bytes: {len}");
        assert!(len > 10_000, "render produced {len} bytes");
        let _ = std::fs::remove_file(&out);
    }

    /// End-to-end image-sequence path: generate PNG frames (in a dir with
    /// non-ASCII + a space, like real user paths), detect the sequence, append
    /// it via its `imagesequence://` URI, and check the timeline duration is
    /// frames ÷ fps, the preview produces frames, and a WebM render succeeds.
    /// Needs GStreamer on PATH; self-skips if the pipeline can't be built.
    #[test]
    fn previews_and_renders_an_image_sequence() {
        let mut project = match Project::new({
            let count = Arc::new(AtomicU32::new(0));
            let c2 = count.clone();
            move |_f| {
                c2.fetch_add(1, Ordering::SeqCst);
            }
        }) {
            Ok(p) => p,
            Err(_) => {
                eprintln!("skipping previews_and_renders_an_image_sequence: no GStreamer");
                return;
            }
        };
        let dir = scratch("säq test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        for i in 1..=10u32 {
            let img =
                image::RgbaImage::from_pixel(64, 36, image::Rgba([(i * 20) as u8, 90, 200, 255]));
            img.save(dir.join(format!("frame_{i:04}.png")))
                .expect("write frame");
        }
        let mut spec =
            crate::sequence::detect_sequence(&dir.join("frame_0001.png")).expect("detect");
        assert_eq!(spec.count, 10);
        spec.fps = 25;
        let uri = spec.uri().expect("uri");

        let info = project
            .append_clip_uri(&uri, 0, None)
            .expect("append sequence");
        // 10 frames at 25 fps = 0.4 s, discovered as the clip's natural length.
        assert_eq!(info.duration, Duration::from_millis(400));
        assert_eq!(project.duration(), Some(Duration::from_millis(400)));
        project.play().expect("play");
        std::thread::sleep(Duration::from_millis(800));

        let out = dir.join("out.webm");
        let _ = std::fs::remove_file(&out);
        project
            .begin_render(
                &out,
                ExportSettings {
                    codec: VideoCodec::Vp8,
                    width: 64,
                    height: 36,
                    fps: 25,
                    bitrate_kbps: 0,
                    encoder: Encoder::Auto,
                },
            )
            .expect("begin_render");
        let mut done = false;
        for _ in 0..300 {
            match project.render_status() {
                RenderStatus::Done => {
                    done = true;
                    break;
                }
                RenderStatus::Failed(e) => panic!("sequence render failed: {e}"),
                RenderStatus::Rendering(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        assert!(done, "sequence render did not finish in time");
        project.end_render().expect("end_render");
        let len = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
        assert!(len > 500, "sequence render produced only {len} bytes");
        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Sizes below the NVENC floor scale up (aspect kept); everything rounds
    /// to even; the GUI's odd 1281x721 becomes 1280x720.
    #[test]
    fn export_settings_normalize_size_and_fps() {
        assert_eq!(normalize_render_size(1920, 1080), (1920, 1080));
        assert_eq!(normalize_render_size(1281, 721), (1280, 720));
        assert_eq!(normalize_render_size(64, 36), (170, 96));
        assert_eq!(normalize_render_size(100, 300), (160, 480));
        let s = ExportSettings {
            codec: VideoCodec::H264,
            width: 17,
            height: 9,
            fps: 999,
            bitrate_kbps: 0,
            encoder: Encoder::Auto,
        }
        .normalized();
        // 17x9: scale = max(160/17, 96/9) = 10.67 → 181x96 → even 180x96.
        assert_eq!((s.width, s.height, s.fps), (180, 96, 240));
    }

    /// While a render runs, transport and edits are inert: pause() doesn't
    /// stall the encoder and remove_clip() refuses; both work again after
    /// end_render. Self-skips without GStreamer.
    #[test]
    fn transport_and_edits_are_inert_while_rendering() {
        let mut project = match Project::new(|_f| {}) {
            Ok(p) => p,
            Err(_) => {
                eprintln!("skipping transport_and_edits_are_inert_while_rendering: no GStreamer");
                return;
            }
        };
        let dir = scratch("inert");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        for i in 1..=12u32 {
            image::RgbaImage::from_pixel(320, 180, image::Rgba([(i * 20) as u8, 60, 160, 255]))
                .save(dir.join(format!("f_{i:03}.png")))
                .expect("frame");
        }
        let spec = crate::sequence::detect_sequence(&dir.join("f_001.png")).expect("detect");
        let info = project
            .append_clip_uri(&spec.uri().unwrap(), 0, None)
            .expect("append");
        let out = dir.join("out.webm");
        project
            .begin_render(
                &out,
                ExportSettings {
                    codec: VideoCodec::Vp8,
                    width: 320,
                    height: 180,
                    fps: 30,
                    bitrate_kbps: 0,
                    encoder: Encoder::Auto,
                },
            )
            .expect("begin_render");
        assert!(project.is_rendering());
        // Inert, not errors: the keyboard handler calls these blindly.
        project.pause().expect("pause is a no-op while rendering");
        assert!(
            !project.remove_clip(&info.id),
            "edits refused while rendering"
        );
        assert!(project
            .append_clip_uri(&spec.uri().unwrap(), 1, None)
            .is_err());
        let mut done = false;
        for _ in 0..300 {
            match project.render_status() {
                RenderStatus::Done => {
                    done = true;
                    break;
                }
                RenderStatus::Failed(e) => panic!("render failed: {e}"),
                RenderStatus::Rendering(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        assert!(done, "the paused-while-rendering export still finished");
        project.end_render().expect("end_render");
        assert!(!project.is_rendering());
        assert!(
            project.remove_clip(&info.id),
            "edits work again after the render"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The composited canvas size is configurable and clamped. Needs GStreamer on
    /// PATH (no media file), self-skips if the pipeline can't be built.
    #[test]
    fn sets_canvas_size() {
        let mut project = match Project::new(|_f| {}) {
            Ok(p) => p,
            Err(_) => {
                eprintln!("skipping sets_canvas_size: no GStreamer");
                return;
            }
        };
        assert_eq!(project.canvas_size(), (CANVAS_W, CANVAS_H));
        project.set_canvas_size(1920, 1080);
        assert_eq!(project.canvas_size(), (1920, 1080));
        // Absurd values are clamped to a sane range, not accepted verbatim.
        project.set_canvas_size(0, 999_999);
        let (w, h) = project.canvas_size();
        assert!((16..=7680).contains(&w) && (16..=4320).contains(&h));
    }

    /// Grabs a thumbnail frame off the UI thread and checks it's a sane RGBA image.
    #[test]
    fn grabs_a_thumbnail() {
        let Some(path) = std::env::var_os("GST_TEST_FILE") else {
            eprintln!("skipping grabs_a_thumbnail: set GST_TEST_FILE");
            return;
        };
        let path = std::path::PathBuf::from(path);
        let frame = std::thread::spawn(move || thumbnail(&path, 160))
            .join()
            .unwrap()
            .expect("thumbnail");
        assert_eq!(frame.width, 160, "thumbnail width");
        assert!(frame.height > 0);
        assert_eq!(frame.rgba.len() as u32, frame.width * frame.height * 4);
    }

    /// Verifies asset discovery works on a worker thread (so imports can happen
    /// off the UI thread) and warms the cache for a fast follow-up add.
    #[test]
    fn warms_asset_off_thread() {
        let Some(path) = std::env::var_os("GST_TEST_FILE") else {
            eprintln!("skipping warms_asset_off_thread: set GST_TEST_FILE");
            return;
        };
        let path = std::path::PathBuf::from(path);
        gst::init().unwrap();
        ges::init().unwrap();
        ensure_encoder_ranks();
        let p2 = path.clone();
        let ok = std::thread::spawn(move || warm_asset(&p2).is_ok())
            .join()
            .unwrap();
        assert!(ok, "warm_asset failed on a worker thread");
        // After warming, adding the clip should succeed (cache hit, no block).
        let mut project = Project::new(|_f| {}).expect("project");
        project
            .append_clip(&path, 0, None)
            .expect("append after warm");
    }

    /// Runtime-checks the track structural ops: move a clip between tracks, onto
    /// a new track, and reorder tracks. Must not hang or panic. Needs GST_TEST_FILE.
    #[test]
    fn moves_clips_and_tracks() {
        let Some(path) = std::env::var_os("GST_TEST_FILE") else {
            eprintln!("skipping moves_clips_and_tracks: set GST_TEST_FILE");
            return;
        };
        let path = std::path::PathBuf::from(path);
        let mut project = Project::new(|_f| {}).expect("project");
        let a = project.append_clip(&path, 0, None).expect("a"); // track 0
        let b = project.append_clip(&path, 1, None).expect("b"); // track 1
        project.play().expect("play");
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(project.clip_track(&a.id), Some(0));
        assert_eq!(project.clip_track(&b.id), Some(1));
        // Move a onto a new bottom track (index 2).
        assert_eq!(project.move_clip_to_track(&a.id, 2), Some(2));
        assert_eq!(project.track_count(), 3);
        assert_eq!(project.clip_track(&a.id), Some(2));
        // Reorder: move track 2 to the top (0); a follows its layer.
        project.move_track(2, 0);
        assert_eq!(project.clip_track(&a.id), Some(0));
    }

    /// The faithful repro: a video base + an IMAGE overlay, then edit the image's
    /// transform (scale) repeatedly. Needs `GST_TEST_FILE` (video) + `GST_TEST_IMAGE`.
    #[test]
    fn edit_image_transform() {
        let (Some(vid), Some(img)) = (
            std::env::var_os("GST_TEST_FILE"),
            std::env::var_os("GST_TEST_IMAGE"),
        ) else {
            eprintln!("skipping edit_image_transform: set GST_TEST_FILE + GST_TEST_IMAGE");
            return;
        };
        let vid = std::path::PathBuf::from(vid);
        let img = std::path::PathBuf::from(img);
        let mut project = Project::new(|_f| {}).expect("project");
        project.append_clip(&vid, 1, None).expect("video");
        let image = project
            .append_clip(&img, 0, Some(Duration::from_secs(5)))
            .expect("image");
        project.play().expect("play");
        std::thread::sleep(Duration::from_millis(300));
        for i in 0..8 {
            let scale = 0.5 + (i as f64) * 0.05;
            project.set_clip_layout(
                &image.id,
                Layout {
                    posx: 0,
                    posy: 0,
                    scale,
                    alpha: 1.0,
                    volume: 1.0,
                },
            );
            project.refresh_preview();
        }
    }

    /// A still image for the undo tests, and a project to put it in.
    fn undo_fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, Project) {
        let dir = scratch(tag);
        let png = dir.join("still.png");
        image::RgbaImage::from_pixel(120, 90, image::Rgba([10, 120, 200, 255]))
            .save(&png)
            .expect("write still");
        (dir, png, Project::new(|_f| {}).expect("project"))
    }

    fn record_of(project: &Project, id: &ClipId) -> crate::document::ClipRecord {
        project
            .clip_records()
            .into_iter()
            .find(|(i, _)| i == id)
            .map(|(_, r)| r)
            .expect("the clip is on the timeline")
    }

    /// Exactly: undo compares records with `==`, so a clip written back must
    /// read back identical, transform included.
    fn assert_same_record(a: &crate::document::ClipRecord, b: &crate::document::ClipRecord) {
        assert_eq!(a, b);
    }

    fn secs(n: f64) -> Duration {
        Duration::from_secs_f64(n)
    }

    /// Write records back through the batch, as undo does; true if all landed.
    fn write_back(
        project: &mut Project,
        writes: &[(&ClipId, &crate::document::ClipRecord)],
    ) -> bool {
        let owned: Vec<(ClipId, crate::document::ClipRecord)> = writes
            .iter()
            .map(|(id, record)| ((*id).clone(), (*record).clone()))
            .collect();
        project.set_clip_records(&owned).is_empty()
    }

    /// The slide was clamped against a neighbour; reversing the amount would
    /// not put the clip back, writing the old record does.
    #[test]
    fn undo_writes_a_clamped_slide_back_exactly() {
        let (dir, png, mut project) = undo_fixture("undo-slide");
        project
            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(2.0))
            .expect("a");
        let b = project
            .add_clip(&png, 0, secs(3.0), Duration::ZERO, secs(2.0))
            .expect("b");
        let before = record_of(&project, &b);
        let geom = project.slide_clip(&b, -10.0).expect("slide");
        assert_eq!(geom.start, secs(2.0), "stopped against a");
        assert!(write_back(&mut project, &[(&b, &before)]));
        assert_same_record(&record_of(&project, &b), &before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_writes_a_clamped_trim_back_exactly() {
        let (dir, png, mut project) = undo_fixture("undo-trim");
        let a = project
            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(3.0))
            .expect("a");
        let before = record_of(&project, &a);
        let geom = project.trim_clip(&a, 1, -10.0).expect("trim");
        assert!(geom.duration < secs(1.0), "clamped at the minimum");
        assert!(write_back(&mut project, &[(&a, &before)]));
        assert_same_record(&record_of(&project, &a), &before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_writes_a_track_and_a_transform_back() {
        let (dir, png, mut project) = undo_fixture("undo-move");
        let a = project
            .add_clip(&png, 0, secs(1.0), Duration::ZERO, secs(2.0))
            .expect("a");
        project.set_clip_layout(
            &a,
            Layout {
                posx: 40,
                posy: -20,
                scale: 0.5,
                alpha: 0.75,
                volume: 1.0,
            },
        );
        let before = record_of(&project, &a);
        project.move_clip_to_track(&a, 1).expect("move");
        project.set_clip_layout(
            &a,
            Layout {
                posx: 0,
                posy: 0,
                scale: 1.0,
                alpha: 1.0,
                volume: 1.0,
            },
        );
        assert!(write_back(&mut project, &[(&a, &before)]));
        assert_same_record(&record_of(&project, &a), &before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A reorder moves layers, not clips; writing each clip's record puts
    /// every clip back on its old track. The two clips trade layers at the
    /// same time, which GES refuses one write at a time.
    #[test]
    fn undo_puts_clips_back_after_a_track_reorder() {
        let (dir, png, mut project) = undo_fixture("undo-reorder");
        let a = project
            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(2.0))
            .expect("a");
        let b = project
            .add_clip(&png, 1, secs(0.0), Duration::ZERO, secs(2.0))
            .expect("b");
        let (ra, rb) = (record_of(&project, &a), record_of(&project, &b));
        project.move_track(0, 1);
        assert_eq!(project.clip_track(&a), Some(1), "the reorder moved a");
        assert!(write_back(&mut project, &[(&a, &ra), (&b, &rb)]));
        assert_same_record(&record_of(&project, &a), &ra);
        assert_same_record(&record_of(&project, &b), &rb);
        assert_eq!(project.track_count(), 2, "no parking layer is left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_lets_two_clips_trade_places_on_a_track() {
        let (dir, png, mut project) = undo_fixture("undo-trade");
        let a = project
            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(2.0))
            .expect("a");
        let b = project
            .add_clip(&png, 0, secs(2.0), Duration::ZERO, secs(2.0))
            .expect("b");
        let mut to_a = record_of(&project, &a);
        let mut to_b = record_of(&project, &b);
        to_a.start = 2.0;
        to_b.start = 0.0;
        assert!(write_back(&mut project, &[(&a, &to_a), (&b, &to_b)]));
        assert_same_record(&record_of(&project, &a), &to_a);
        assert_same_record(&record_of(&project, &b), &to_b);
        assert_eq!(project.track_count(), 1, "no parking layer is left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GES refuses a clip on top of another without an error; the batch must
    /// say so, put the clip back as it was, and leave no track behind however
    /// often the write is tried.
    #[test]
    fn undo_reports_a_write_the_engine_refused() {
        let (dir, png, mut project) = undo_fixture("undo-refused");
        let a = project
            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(2.0))
            .expect("a");
        let _b = project
            .add_clip(&png, 0, secs(2.0), Duration::ZERO, secs(2.0))
            .expect("b");
        let before = record_of(&project, &a);
        let mut onto_b = before.clone();
        onto_b.start = 2.0;
        for _try in 0..2 {
            assert_eq!(
                project.set_clip_records(&[(a.clone(), onto_b.clone())]),
                vec![a.clone()]
            );
            assert_eq!(record_of(&project, &a), before, "a is back as it was");
            assert_eq!(project.track_count(), 1, "no parking layer is left behind");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// b cannot land (c is there) and cannot go back (a took its place): it
    /// stays parked, on the first track below the timeline, with its old times.
    #[test]
    fn undo_parks_a_clip_that_can_go_neither_way_on_one_track() {
        let (dir, png, mut project) = undo_fixture("undo-neither");
        let a = project
            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(2.0))
            .expect("a");
        let b = project
            .add_clip(&png, 0, secs(2.0), Duration::ZERO, secs(2.0))
            .expect("b");
        project
            .add_clip(&png, 0, secs(4.0), Duration::ZERO, secs(2.0))
            .expect("c");
        let (mut to_a, rb) = (record_of(&project, &a), record_of(&project, &b));
        let mut onto_c = rb.clone();
        to_a.start = 2.0;
        onto_c.start = 4.0;
        assert_eq!(
            project.set_clip_records(&[(a.clone(), to_a.clone()), (b.clone(), onto_c)]),
            vec![b.clone()]
        );
        assert_eq!(record_of(&project, &a), to_a, "a landed");
        let now_b = record_of(&project, &b);
        assert_eq!(
            (now_b.track, now_b.start),
            (1, rb.start),
            "b parked with its old start"
        );
        assert_eq!(project.track_count(), 2, "one parking track, none empty");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undoing a left trim grows the duration back while the in-point shrinks;
    /// in the wrong order in-point plus duration passes the source's length and
    /// GES refuses. Stills have no length, so this needs real media.
    #[test]
    fn undo_writes_a_left_trim_of_real_media_back() {
        let Some(path) = std::env::var_os("GST_TEST_FILE") else {
            eprintln!("skipping undo_writes_a_left_trim_of_real_media_back: set GST_TEST_FILE");
            return;
        };
        let mut project = Project::new(|_f| {}).expect("project");
        let a = project
            .append_clip(std::path::Path::new(&path), 0, None)
            .expect("clip")
            .id;
        let full = record_of(&project, &a);
        project.trim_clip(&a, -1, 0.5).expect("trim");
        let trimmed = record_of(&project, &a);
        assert!(write_back(&mut project, &[(&a, &full)]), "undo the trim");
        assert_eq!(record_of(&project, &a), full);
        assert!(write_back(&mut project, &[(&a, &trimmed)]), "redo it");
        assert_eq!(record_of(&project, &a), trimmed);
    }

    /// The interface and the undo history both hold clip IDs. A deleted clip
    /// that came back under a new name would orphan every one of them.
    #[test]
    fn undo_restores_a_deleted_clip_under_its_old_id() {
        let (dir, png, mut project) = undo_fixture("undo-restore");
        let a = project
            .add_clip(&png, 0, secs(1.5), Duration::ZERO, secs(2.0))
            .expect("a");
        project.set_clip_layout(
            &a,
            Layout {
                posx: 12,
                posy: 34,
                scale: 0.6,
                alpha: 0.5,
                volume: 1.0,
            },
        );
        let before = record_of(&project, &a);
        assert!(project.remove_clip(&a));
        let restored = project.restore_clip(&a, &before).expect("restore");
        assert_eq!(restored, a, "same ID");
        assert_same_record(&record_of(&project, &a), &before);
        // Somewhere free, so it is the ID that refuses it and not an overlap.
        let mut elsewhere = before.clone();
        elsewhere.start += 5.0;
        assert!(project.restore_clip(&a, &elsewhere).is_err(), "not twice");
        assert_eq!(project.clip_records().len(), 1, "one clip, under one ID");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Deleting the only clip on the bottom track prunes that track; bringing
    /// the clip back brings the track back.
    #[test]
    fn undo_restores_a_clip_onto_a_track_that_was_pruned() {
        let (dir, png, mut project) = undo_fixture("undo-restore-track");
        project
            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(2.0))
            .expect("a");
        let tracks = project.track_count();
        let b = project
            .add_clip(&png, tracks, secs(0.0), Duration::ZERO, secs(2.0))
            .expect("b");
        let with_b = project.track_count();
        let before = record_of(&project, &b);
        assert!(project.remove_clip(&b));
        assert!(
            project.track_count() < with_b,
            "the empty bottom track went"
        );
        let restored = project.restore_clip(&b, &before).expect("restore");
        assert_eq!(project.clip_track(&restored), Some(tracks));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_knows_a_source_that_is_not_there() {
        let (dir, png, project) = undo_fixture("undo-source");
        let here = gst::glib::filename_to_uri(&png, None).expect("uri");
        let gone = gst::glib::filename_to_uri(dir.join("never-there.png"), None).expect("uri");
        assert!(project.source_available(&here));
        assert!(!project.source_available(&gone));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undo only brings back a source this session has discovered, and GES
    /// answers for that from its cache even after the file is deleted.
    #[test]
    fn undo_knows_a_source_deleted_since_it_was_used() {
        let (dir, png, mut project) = undo_fixture("undo-source-deleted");
        let uri = gst::glib::filename_to_uri(&png, None).expect("uri");
        let a = project
            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(2.0))
            .expect("a");
        assert!(project.remove_clip(&a));
        std::fs::remove_file(&png).expect("delete the still");
        assert!(!project.source_available(&uri));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same for an image sequence, which is not one file to look for; and
    /// once its frames are back, it is available and can be added again.
    #[test]
    fn undo_knows_a_sequence_deleted_since_it_was_used() {
        let dir = scratch("undo-sequence-deleted");
        let frames: Vec<_> = (1..=3u32)
            .map(|i| dir.join(format!("frame_{i:04}.png")))
            .collect();
        let write = || {
            for f in &frames {
                image::RgbaImage::from_pixel(64, 36, image::Rgba([10, 120, 200, 255]))
                    .save(f)
                    .expect("write a frame");
            }
        };
        write();
        let uri = crate::sequence::detect_sequence(&frames[0])
            .expect("detect")
            .uri()
            .expect("uri");
        let mut project = Project::new(|_f| {}).expect("project");
        let a = project.append_clip_uri(&uri, 0, None).expect("append").id;
        assert!(project.remove_clip(&a));
        for f in &frames {
            std::fs::remove_file(f).expect("delete a frame");
        }
        assert!(!project.source_available(&uri), "frames gone");
        write();
        assert!(project.source_available(&uri), "frames back");
        project
            .append_clip_uri(&uri, 0, None)
            .expect("the sequence can be added again");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Stills have no in-point; real media does, and a restore must put it back.
    #[test]
    fn undo_restores_a_trimmed_clip_of_real_media() {
        let Some(path) = std::env::var_os("GST_TEST_FILE") else {
            eprintln!("skipping undo_restores_a_trimmed_clip_of_real_media: set GST_TEST_FILE");
            return;
        };
        let mut project = Project::new(|_f| {}).expect("project");
        let a = project
            .append_clip(Path::new(&path), 0, None)
            .expect("clip")
            .id;
        project.trim_clip(&a, -1, 0.5).expect("trim");
        let before = record_of(&project, &a);
        assert!(before.inpoint > 0.0, "the trim moved the in-point");
        assert!(project.remove_clip(&a));
        project.restore_clip(&a, &before).expect("restore");
        assert_same_record(&record_of(&project, &a), &before);
    }

    /// A drag onto a new bottom track creates that track; undoing the move
    /// must take it away again.
    #[test]
    fn undo_removes_the_track_a_move_created() {
        let (dir, png, mut project) = undo_fixture("undo-prune");
        let a = project
            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(2.0))
            .expect("a");
        let tracks = project.track_count();
        let before = record_of(&project, &a);
        project
            .move_clip_to_track(&a, tracks)
            .expect("move to a new track");
        assert_eq!(project.track_count(), tracks + 1);
        assert!(write_back(&mut project, &[(&a, &before)]));
        project.prune_tracks(tracks);
        assert_eq!(project.track_count(), tracks);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_pruning_never_removes_a_track_with_clips() {
        let (dir, png, mut project) = undo_fixture("undo-prune-keep");
        project
            .add_clip(&png, 2, secs(0.0), Duration::ZERO, secs(2.0))
            .expect("a");
        let tracks = project.track_count();
        project.prune_tracks(0);
        assert_eq!(project.track_count(), tracks, "the bottom track has a clip");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Several empty tracks at the bottom all go, down to the count and no
    /// further; an empty track above a clip stays, so no clip changes track;
    /// and GES drops the same layers the engine does, so a track made later
    /// lands at its index.
    #[test]
    fn undo_pruning_takes_every_empty_bottom_track_down_to_the_count() {
        let (dir, png, mut project) = undo_fixture("undo-prune-many");
        let a = project
            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(2.0))
            .expect("a");
        let b = project
            .add_clip(&png, 2, secs(0.0), Duration::ZERO, secs(2.0))
            .expect("b");
        project.layer(5); // empty tracks 3 to 5, as a merged drag leaves them
        project.prune_tracks(4);
        assert_eq!(project.track_count(), 4, "down to the count, no further");
        project.prune_tracks(1);
        assert_eq!(
            project.track_count(),
            3,
            "stops at b; the empty track 1 stays"
        );
        assert_eq!(
            (project.clip_track(&a), project.clip_track(&b)),
            (Some(0), Some(2))
        );
        assert_eq!(project.timeline.layers().len(), project.track_count());
        project
            .move_clip_to_track(&a, 3)
            .expect("a new bottom track");
        assert_eq!(
            project.clip_track(&a),
            Some(3),
            "the new track is at its index"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Nothing on the timeline and a count of zero still leaves one track:
    /// the engine is never without a layer.
    #[test]
    fn undo_pruning_leaves_one_track_on_an_empty_timeline() {
        let (dir, _png, mut project) = undo_fixture("undo-prune-empty");
        project.layer(2);
        project.prune_tracks(0);
        assert_eq!(project.track_count(), 1);
        assert_eq!(project.timeline.layers().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The engine zooms a clip past the canvas and the read-back must say so:
    /// a 1.0 ceiling in `clip_layout` once snapped every zoomed clip back to
    /// fit whenever the inspector refreshed. Through a save and a load too.
    #[test]
    fn a_zoomed_clip_keeps_its_scale() {
        let (dir, png, mut project) = undo_fixture("zoomed");
        let a = project
            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(2.0))
            .expect("a");
        project.set_clip_layout(
            &a,
            Layout {
                posx: -320,
                posy: -180,
                scale: 2.5,
                alpha: 1.0,
                volume: 1.0,
            },
        );
        let read = project.clip_layout(&a).expect("a layout");
        assert!((read.scale - 2.5).abs() < 1e-3, "read back {}", read.scale);
        let doc = project.to_document();
        let mut reopened = Project::new(|_f| {}).expect("project");
        reopened.apply_document(&doc).expect("apply");
        let again = &reopened.to_document().clips[0];
        assert!(
            (again.layout.scale - 2.5).abs() < 1e-3,
            "after a load {}",
            again.layout.scale
        );
        assert_eq!((again.layout.posx, again.layout.posy), (-320, -180));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn split_cuts_a_clip_in_two_where_asked() {
        let (dir, png, mut project) = undo_fixture("split-middle");
        let a = project
            .add_clip(&png, 0, secs(1.0), Duration::ZERO, secs(4.0))
            .expect("a");
        project.set_clip_layout(
            &a,
            Layout {
                posx: 12,
                posy: 34,
                scale: 0.6,
                alpha: 0.5,
                volume: 1.0,
            },
        );
        let before = record_of(&project, &a);
        let (b, left, right) = project.split_clip(&a, secs(2.5)).expect("split");
        assert_ne!(b, a, "the right half is a new clip");
        assert_eq!((left.start, left.duration), (secs(1.0), secs(1.5)));
        assert_eq!((right.start, right.duration), (secs(2.5), secs(2.5)));
        assert_eq!(
            left.start + left.duration,
            right.start,
            "they touch exactly"
        );
        assert_eq!(
            right.inpoint,
            left.inpoint + left.duration,
            "the right half carries on where the left stops"
        );
        let (ra, rb) = (record_of(&project, &a), record_of(&project, &b));
        assert_eq!(ra.layout, before.layout, "the left keeps its transform");
        assert_eq!(rb.layout, before.layout, "and the right half has it too");
        assert_eq!((ra.track, rb.track), (0, 0));
        assert_eq!(project.clip_records().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn split_refuses_within_the_minimum_of_either_edge() {
        let (dir, png, mut project) = undo_fixture("split-edges");
        // A clip at [1 s, 3 s).
        let a = project
            .add_clip(&png, 0, secs(1.0), Duration::ZERO, secs(2.0))
            .expect("a");
        let before = record_of(&project, &a);
        for at in [0.5, 1.0, 1.1, 2.9, 3.0, 4.0] {
            let err = project.split_clip(&a, secs(at)).expect_err("refused");
            assert!(format!("{err:#}").contains("0.2 s"), "at {at}: {err:#}");
        }
        assert_eq!(project.clip_records().len(), 1, "nothing was cut");
        assert_same_record(&record_of(&project, &a), &before);
        // Exactly the minimum is allowed.
        let (_, left, right) = project
            .split_clip(&a, Duration::from_millis(1200))
            .expect("at 1.2 s");
        assert_eq!(
            (left.duration, right.duration),
            (Duration::from_millis(200), Duration::from_millis(1800))
        );
        // The playhead now sits on the cut, where cutting again is refused.
        assert!(project.split_clip(&a, Duration::from_millis(1200)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undo writes the left half's old record and removes the right half;
    /// redo writes the left half again and restores the right half under the
    /// ID the step holds. No split is replayed: these are the operations
    /// `undo::plan` already produces.
    #[test]
    fn undo_puts_a_split_clip_back_as_one_and_redo_cuts_it_again() {
        let (dir, png, mut project) = undo_fixture("undo-split");
        let a = project
            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(4.0))
            .expect("a");
        let whole = record_of(&project, &a);
        let (b, _, _) = project.split_clip(&a, secs(1.5)).expect("split");
        let (left, right) = (record_of(&project, &a), record_of(&project, &b));
        assert!(project.remove_clip(&b));
        assert!(write_back(&mut project, &[(&a, &whole)]));
        assert_eq!(project.clip_records().len(), 1, "one clip again");
        assert_same_record(&record_of(&project, &a), &whole);
        assert!(write_back(&mut project, &[(&a, &left)]));
        assert_eq!(project.restore_clip(&b, &right).expect("restore"), b);
        assert_same_record(&record_of(&project, &a), &left);
        assert_same_record(&record_of(&project, &b), &right);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A generated image sequence: real source time, unlike a still, and no
    /// sound. `frames` PNGs at `fps`, so it lasts `frames / fps` seconds.
    fn sequence_fixture(tag: &str, frames: u32, fps: u32) -> (std::path::PathBuf, String) {
        let dir = scratch(tag);
        for i in 1..=frames {
            image::RgbaImage::from_pixel(64, 36, image::Rgba([(i * 7 % 255) as u8, 90, 200, 255]))
                .save(dir.join(format!("frame_{i:04}.png")))
                .expect("write a frame");
        }
        let mut spec =
            crate::sequence::detect_sequence(&dir.join("frame_0001.png")).expect("detect");
        spec.fps = fps;
        let uri = spec.uri().expect("uri");
        (dir, uri)
    }

    #[test]
    fn frame_length_ignores_buffers_too_short_to_be_frames() {
        assert_eq!(next_frame_ns(40_000_000, Some(33_333_333)), 33_333_333);
        assert_eq!(
            next_frame_ns(33_333_333, None),
            33_333_333,
            "no duration keeps the last"
        );
        assert_eq!(
            next_frame_ns(33_333_333, Some(1)),
            33_333_333,
            "the 1 ns buffers a flushing seek leaves"
        );
        assert_eq!(next_frame_ns(33_333_333, Some(100_000_000)), 100_000_000);
    }

    /// The composited preview runs at the source's rate (measured, M10), so a
    /// 10 fps sequence shows frames a tenth of a second long.
    #[test]
    fn frame_length_follows_the_preview() {
        let (dir, uri) = sequence_fixture("frame-length", 30, 10);
        let mut project = Project::new(|_f| {}).expect("project");
        assert_eq!(
            project.frame_secs(),
            0.04,
            "1/25 s until a frame has arrived"
        );
        project.append_clip_uri(&uri, 0, None).expect("append");
        project.play().expect("play");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while project.frame_secs() == 0.04 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(project.frame_secs(), 0.1);
        let _ = project.pause();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shuttle_plays_faster_and_refuses_reverse() {
        let (dir, png, mut project) = undo_fixture("shuttle");
        project
            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(20.0))
            .expect("a");
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(project.set_rate(bad).is_err(), "{bad} must be refused");
        }
        assert_eq!(project.rate(), 1.0, "a refused rate changes nothing");
        project.play().expect("play");
        let end = std::time::Instant::now() + Duration::from_secs(10);
        while settled(&project.pipeline) == Step::Pending && std::time::Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        project.set_rate(4.0).expect("4x");
        assert_eq!(project.rate(), 4.0);
        let _ = project.pipeline.state(gst::ClockTime::from_seconds(3));
        let from = project.position().expect("a position");
        std::thread::sleep(Duration::from_millis(1000));
        let to = project.position().expect("a position");
        // Measured at 3.96 s of timeline per second on a still (M9). Anything
        // clearly faster than normal proves the rate reached the pipeline.
        assert!(
            to > from + Duration::from_millis(2000),
            "{from:?} -> {to:?}"
        );
        project.seek(secs(1.0)).expect("seek");
        assert_eq!(
            project.rate(),
            1.0,
            "an ordinary seek plays at normal speed again"
        );
        let _ = project.pause();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Not a gate: a measurement of what this GStreamer does with a speed
    /// change, which the speed work was built on (see the Amendments of
    /// `docs/superpowers/plans/2026-09-23-editor-clip-edits.md`). Run it by
    /// hand after a GStreamer upgrade, with `GST_TEST_FILE` naming a video
    /// with sound:
    /// `cargo test -p kuvatin-video -- --ignored --nocapture --test-threads=1 speed_measure_the_runtime`
    #[test]
    #[ignore]
    fn speed_measure_the_runtime() {
        gst::init().expect("gst");
        ges::init().expect("ges");
        println!("M0 {}", gst::version_string());

        for desc in [
            "videorate",
            "pitch",
            "videorate rate=2",
            "pitch rate=2",
            "pitch tempo=2",
        ] {
            let e = ges::Effect::new(desc).expect("effect");
            println!("M1 {desc:?}: is_time_effect = {}", e.is_time_effect());
        }
        for (desc, name) in [
            ("videorate", "rate"),
            ("videorate", "GstVideoRate::rate"),
            ("pitch", "rate"),
            ("pitch", "GstPitch::rate"),
            ("pitch", "tempo"),
        ] {
            let e = ges::Effect::new(desc).expect("effect");
            let took = e.register_time_property(name);
            println!(
                "M2 {desc} register_time_property({name:?}) = {took}, is_time_effect = {}",
                e.is_time_effect()
            );
        }

        let (dir, seq_uri) = sequence_fixture("speed-measure", 20, 10);
        let png = dir.join("still.png");
        image::RgbaImage::from_pixel(64, 36, image::Rgba([10, 120, 200, 255]))
            .save(&png)
            .expect("still");
        let mut project = Project::new(|_f| {}).expect("project");
        let seq = project
            .append_clip_uri(&seq_uri, 0, None)
            .expect("sequence")
            .id;
        let still = project
            .append_clip(&png, 1, Some(Duration::from_secs(4)))
            .expect("still")
            .id;
        let seq_clip = project.clips[&seq.0].clone();
        let still_clip = project.clips[&still.0].clone();
        let limit = |c: &ges::Clip| c.property::<Option<gst::ClockTime>>("duration-limit");

        let caught =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| still_clip.duration_limit()));
        println!(
            "M7 still: duration_limit() panics = {}; the property reads {:?}",
            caught.is_err(),
            limit(&still_clip)
        );

        let pitch = ges::Effect::new("pitch rate=2").expect("pitch");
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            seq_clip.add_top_effect(&pitch, -1)
        }));
        let said = match &caught {
            Ok(Ok(())) => "Ok(())".to_string(),
            Ok(Err(e)) => format!("Err({e})"),
            Err(_) => "panicked: GES returned FALSE without a GError".to_string(),
        };
        println!("M5 pitch on a sequence, which has no sound: {said}");

        let video = ges::Effect::new("videorate rate=2").expect("videorate");
        println!(
            "M8 videorate on a still: {:?}",
            still_clip.add_top_effect(&video, -1)
        );

        let Some(path) = std::env::var_os("GST_TEST_FILE") else {
            println!("M3, M4, M6 and M11 need GST_TEST_FILE: a video with sound");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        let a = project
            .append_clip(Path::new(&path), 2, None)
            .expect("media")
            .id;
        let clip = project.clips[&a.0].clone();
        println!(
            "M4 full length {} limit {:?}",
            clip.duration(),
            limit(&clip)
        );
        let v = ges::Effect::new("videorate rate=2").expect("videorate");
        let added = clip.add_top_effect(&v, -1);
        println!(
            "M4 videorate=2 on the full-length clip: {added:?}; duration now {} limit {:?}",
            clip.duration(),
            limit(&clip)
        );
        let grew = clip.set_duration(clip.duration() + gst::ClockTime::from_seconds(1));
        println!("M4 growing past the limit: set_duration = {grew}");
        let p = ges::Effect::new("pitch rate=2").expect("pitch");
        println!("M4 pitch=2: {:?}", clip.add_top_effect(&p, -1));
        for e in clip.top_effects() {
            println!(
                "M3 {:?} {:?}: rate = {:?}, tempo = {:?}",
                e.track_type(),
                TimelineElementExt::name(&e),
                TimelineElementExt::child_property(&e, "rate"),
                TimelineElementExt::child_property(&e, "tempo")
            );
        }

        project.set_clip_layout(
            &a,
            Layout {
                posx: 40,
                posy: 0,
                scale: 0.5,
                alpha: 0.5,
                volume: 0.25,
            },
        );
        let right = clip
            .split_full(clip.start().nseconds() + 1_000_000_000)
            .expect("split")
            .expect("a new clip");
        println!(
            "M6 right half: start {} inpoint {} duration {} effects {} posx {:?} width {:?} alpha {:?} volume {:?}",
            right.start(),
            right.inpoint(),
            right.duration(),
            right.top_effects().len(),
            right.child_property("posx"),
            right.child_property("width"),
            right.child_property("alpha"),
            right.child_property("volume")
        );
        println!(
            "M6 split at the clip's own start: {:?}",
            clip.split_full(clip.start().nseconds())
                .map(|c| c.is_some())
        );

        let b = project
            .append_clip(Path::new(&path), 3, None)
            .expect("media")
            .id;
        let bc = project.clips[&b.0].clone();
        bc.set_duration(gst::ClockTime::from_nseconds(bc.duration().nseconds() / 2));
        let t = ges::Effect::new("pitch tempo=2").expect("tempo");
        println!(
            "M11 pitch tempo=2 on a half-length clip: {:?}, limit {:?}",
            bc.add_top_effect(&t, -1),
            limit(&bc)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn speed_rates_snap_to_what_the_engine_can_hold() {
        for r in [0.25, 0.5, 1.0, 1.5, 2.0, 4.0] {
            assert_eq!(
                snap_rate(r),
                r,
                "every rate the inspector offers is kept exactly"
            );
        }
        assert_eq!(snap_rate(0.1), RATE_MIN);
        assert_eq!(snap_rate(9.0), RATE_MAX);
        assert_eq!(
            snap_rate(1.1),
            f64::from(1.1f32),
            "rounded to what pitch can store"
        );
    }

    #[test]
    fn speed_change_keeps_the_span_of_source() {
        // Four seconds at 1x are two at 2x, and back.
        assert_eq!(
            rate_change_math(0, 4 * S, 1.0, 2.0, Some(10 * S), None),
            2 * S
        );
        assert_eq!(
            rate_change_math(0, 2 * S, 2.0, 1.0, Some(10 * S), None),
            4 * S
        );
        // A source with no length has nothing to run out of.
        assert_eq!(rate_change_math(0, 4 * S, 1.0, 0.5, None, None), 8 * S);
        // Slowing down stops at the next clip.
        assert_eq!(
            rate_change_math(0, 2 * S, 1.0, 0.5, Some(10 * S), Some(3 * S)),
            3 * S
        );
        // The minimum holds a very fast clip up...
        assert_eq!(
            rate_change_math(0, 400_000_000, 1.0, 4.0, Some(10 * S), None),
            MIN_TRIM_NS
        );
        // ...but the source wins where they conflict: 0.5 s of source left
        // is 0.125 s at 4x.
        assert_eq!(
            rate_change_math(9_500_000_000, 400_000_000, 1.0, 4.0, Some(10 * S), None),
            125_000_000
        );
    }

    #[test]
    fn speed_room_is_the_gap_before_the_next_clip() {
        // A clip at [2 s, 3 s).
        assert_eq!(room_after(2 * S, S, &[]), None, "nothing after it");
        assert_eq!(room_after(2 * S, S, &[(5 * S, 8 * S)]), Some(3 * S));
        assert_eq!(
            room_after(2 * S, S, &[(0, S), (9 * S, 10 * S), (5 * S, 8 * S)]),
            Some(3 * S),
            "the nearest, in any order; a clip before it does not count"
        );
        assert_eq!(
            room_after(2 * S, S, &[(3 * S, 4 * S)]),
            Some(S),
            "a clip butting up against it leaves no room to grow"
        );
    }

    #[test]
    fn speed_halves_a_sequence_and_brings_it_back() {
        // Forty frames at 10 fps: four seconds of source.
        let (dir, uri) = sequence_fixture("speed-seq", 40, 10);
        let mut project = Project::new(|_f| {}).expect("project");
        let id = project.append_clip_uri(&uri, 0, None).expect("append").id;
        assert_eq!(project.clip_rate(&id), 1.0);
        let geom = project.set_clip_rate(&id, 2.0).expect("2x");
        assert_eq!(
            geom.duration,
            secs(2.0),
            "the same four seconds, twice as fast"
        );
        assert_eq!(project.clip_rate(&id), 2.0);
        let clip = project.clips[&id.0].clone();
        let effects = time_effects(&clip);
        assert_eq!(
            effects.len(),
            1,
            "the picture only: a sequence has no sound"
        );
        assert!(effects.iter().all(|e| e.is_time_effect()));
        let back = project.set_clip_rate(&id, 1.0).expect("1x");
        assert_eq!(back.duration, secs(4.0));
        assert!(
            clip.top_effects().is_empty(),
            "normal speed carries no effects at all"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The spec puts this test on a clip sped up; speeding up shortens a
    /// clip, so it is slowing down that runs into a neighbour.
    #[test]
    fn speed_stops_at_the_next_clip() {
        // Twenty frames at 10 fps: two seconds, with a neighbour 1 s after it.
        let (dir, uri) = sequence_fixture("speed-gap", 20, 10);
        let mut project = Project::new(|_f| {}).expect("project");
        let a = project
            .add_clip_uri(&uri, 0, secs(0.0), Duration::ZERO, secs(2.0))
            .expect("a");
        project
            .add_clip_uri(&uri, 0, secs(3.0), Duration::ZERO, secs(2.0))
            .expect("b");
        let geom = project.set_clip_rate(&a, 0.5).expect("0.5x");
        assert_eq!(
            geom.duration,
            secs(3.0),
            "four seconds wanted, three of room"
        );
        assert_eq!(project.clip_rate(&a), 0.5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn speed_is_refused_for_a_still() {
        let (dir, png, mut project) = undo_fixture("speed-still");
        let a = project
            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(2.0))
            .expect("a");
        assert!(project.set_clip_rate(&a, 2.0).is_none());
        assert_eq!(project.clip_rate(&a), 1.0);
        assert!(project.clips[&a.0].top_effects().is_empty());
        assert!(project.set_clip_rate(&a, f64::NAN).is_none());
        assert!(project.set_clip_rate(&ClipId("nope".into()), 2.0).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn speed_trims_a_slowed_clip_from_both_edges() {
        // Four seconds of source at 0.5x: eight on the timeline, from 2 s.
        let (dir, uri) = sequence_fixture("speed-trim", 40, 10);
        let mut project = Project::new(|_f| {}).expect("project");
        let a = project
            .add_clip_uri(&uri, 0, secs(2.0), Duration::ZERO, secs(4.0))
            .expect("a");
        project.set_clip_rate(&a, 0.5).expect("0.5x");
        // In by a second of timeline: half a second of source.
        let g = project.trim_clip(&a, -1, 1.0).expect("left in");
        assert_eq!(
            (g.start, g.inpoint, g.duration),
            (secs(3.0), secs(0.5), secs(7.0))
        );
        // Out as far as it goes: 3.5 s of source left is 7 s at 0.5x.
        let g = project.trim_clip(&a, 1, 30.0).expect("right out");
        assert_eq!(g.duration, secs(7.0), "the source is used up exactly");
        // Back out past the start of the source: it stops there.
        let g = project.trim_clip(&a, -1, -5.0).expect("left out");
        assert_eq!(
            (g.start, g.inpoint, g.duration),
            (secs(2.0), Duration::ZERO, secs(8.0))
        );
        // The pure maths and GES agree on how long the clip may be.
        let clip = project.clips[&a.0].clone();
        let limit = clip
            .property::<Option<gst::ClockTime>>("duration-limit")
            .expect("a sequence has a length");
        let max = clip
            .property::<Option<gst::ClockTime>>("max-duration")
            .expect("a sequence has a length");
        let ours = trim_right_math(
            clip.inpoint().nseconds() as i128,
            clip.duration().nseconds() as i128,
            60 * S,
            Some(max.nseconds() as i128),
            0.5,
        );
        assert_eq!(ours, limit.nseconds() as i128);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_writes_a_speed_change_back_both_ways() {
        let (dir, uri) = sequence_fixture("undo-speed", 40, 10);
        let mut project = Project::new(|_f| {}).expect("project");
        let a = project.append_clip_uri(&uri, 0, None).expect("append").id;
        let normal = record_of(&project, &a);
        project.set_clip_rate(&a, 2.0).expect("2x");
        let fast = record_of(&project, &a);
        assert_eq!((fast.rate, fast.duration), (2.0, 2.0));
        assert!(
            write_back(&mut project, &[(&a, &normal)]),
            "undo: slower, longer"
        );
        assert_same_record(&record_of(&project, &a), &normal);
        assert!(
            write_back(&mut project, &[(&a, &fast)]),
            "redo: faster, shorter"
        );
        assert_same_record(&record_of(&project, &a), &fast);
        // A slow clip is longer than its source: the other order entirely.
        project.set_clip_rate(&a, 0.5).expect("0.5x");
        let slow = record_of(&project, &a);
        assert_eq!((slow.rate, slow.duration), (0.5, 8.0));
        assert!(write_back(&mut project, &[(&a, &fast)]));
        assert_same_record(&record_of(&project, &a), &fast);
        assert!(write_back(&mut project, &[(&a, &slow)]));
        assert_same_record(&record_of(&project, &a), &slow);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_restores_a_slowed_clip_longer_than_its_source() {
        // Two seconds of source at 0.5x: four on the timeline.
        let (dir, uri) = sequence_fixture("undo-restore-speed", 20, 10);
        let mut project = Project::new(|_f| {}).expect("project");
        let a = project.append_clip_uri(&uri, 0, None).expect("append").id;
        project.set_clip_rate(&a, 0.5).expect("0.5x");
        let before = record_of(&project, &a);
        assert_eq!(before.duration, 4.0, "twice as long as the source");
        assert!(project.remove_clip(&a));
        project.restore_clip(&a, &before).expect("restore");
        assert_same_record(&record_of(&project, &a), &before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn speed_survives_a_save_and_a_load() {
        let (dir, uri) = sequence_fixture("speed-save", 20, 10);
        let file = dir.join("cut.kuvatin");
        let mut project = Project::new(|_f| {}).expect("project");
        let slow = project.append_clip_uri(&uri, 0, None).expect("slow").id;
        project.set_clip_rate(&slow, 0.5).expect("0.5x");
        let fast = project
            .add_clip_uri(&uri, 1, secs(0.0), Duration::ZERO, secs(2.0))
            .expect("fast");
        project.set_clip_rate(&fast, 4.0).expect("4x");
        project.to_document().save(&file).expect("save");
        let mut reopened = Project::new(|_f| {}).expect("project");
        let doc = crate::document::ProjectFile::load(&file).expect("load");
        reopened.apply_document(&doc).expect("apply");
        let got: Vec<(f64, f64)> = reopened
            .to_document()
            .clips
            .iter()
            .map(|c| (c.rate, c.duration))
            .collect();
        assert_eq!(got, vec![(0.5, 4.0), (4.0, 0.5)]);
        assert!(
            !reopened.has_unsaved_work(),
            "putting the speed back is part of the load, not an edit"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn split_keeps_the_speed_on_both_halves() {
        // Four seconds of source at 2x: two on the timeline.
        let (dir, uri) = sequence_fixture("split-speed", 40, 10);
        let mut project = Project::new(|_f| {}).expect("project");
        let a = project.append_clip_uri(&uri, 0, None).expect("append").id;
        project.set_clip_rate(&a, 2.0).expect("2x");
        let (b, left, right) = project.split_clip(&a, secs(0.5)).expect("split");
        assert_eq!((project.clip_rate(&a), project.clip_rate(&b)), (2.0, 2.0));
        assert_eq!(
            right.inpoint,
            secs(1.0),
            "half a second at 2x is a second of source"
        );
        assert_eq!(left.duration + right.duration, secs(2.0));
        assert_eq!(
            time_effects(&project.clips[&b.0]).len(),
            1,
            "one effect on the new half, not two"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Real media has picture and sound, so a speed change is two time
    /// effects, and both must be ones GES re-times the clip for. Self-skips
    /// without `GST_TEST_FILE`.
    #[test]
    fn a_sped_up_video_changes_picture_and_sound() {
        let Some(path) = std::env::var_os("GST_TEST_FILE") else {
            eprintln!("skipping a_sped_up_video_changes_picture_and_sound: set GST_TEST_FILE");
            return;
        };
        let mut project = Project::new(|_f| {}).expect("project");
        let a = project
            .append_clip(Path::new(&path), 0, None)
            .expect("clip")
            .id;
        let full = record_of(&project, &a);
        let geom = project.set_clip_rate(&a, 2.0).expect("2x");
        let effects = time_effects(&project.clips[&a.0]);
        let kinds: Vec<ges::TrackType> = effects.iter().map(|e| e.track_type()).collect();
        assert_eq!(effects.len(), 2, "{kinds:?}");
        assert!(
            kinds.contains(&ges::TrackType::VIDEO) && kinds.contains(&ges::TrackType::AUDIO),
            "{kinds:?}"
        );
        assert!((geom.duration.as_secs_f64() - full.duration / 2.0).abs() < 1e-6);
        // Pulled right as far as it goes: the source runs out twice as fast.
        let long = project.trim_clip(&a, 1, 60.0).expect("trim");
        assert!((long.duration.as_secs_f64() - full.duration / 2.0).abs() < 1e-6);
        assert!(write_back(&mut project, &[(&a, &full)]), "undo the speed");
        assert_same_record(&record_of(&project, &a), &full);
        assert!(project.clips[&a.0].top_effects().is_empty());
    }
}
