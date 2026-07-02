//! GES-backed editing project: a timeline of layers + clips with a composited
//! preview. The GUI's timeline UI drives this; GES handles compositing,
//! transforms, trims, and seeking (and, later, render-to-file for export).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSinkCallbacks};
use gstreamer_editing_services as ges;
use ges::prelude::*;
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
    ensure_encoder_ranks();
    gst::init()?;
    ges::init()?;
    let uri = gst::glib::filename_to_uri(path, None)?;
    let _ = ges::UriClipAsset::request_sync(&uri)?;
    Ok(())
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

/// Extract an RGBA `Frame` from a GStreamer sample.
fn sample_to_frame(sample: &gst::Sample) -> Option<Frame> {
    let caps = sample.caps()?;
    let info = gstreamer_video::VideoInfo::from_caps(caps).ok()?;
    let buffer = sample.buffer()?;
    let map = buffer.map_readable().ok()?;
    Some(Frame {
        width: info.width(),
        height: info.height(),
        rgba: map.as_slice().to_vec(),
    })
}

/// Push one RGBA video sample to the frame callback.
fn emit_sample(
    sample: &gst::Sample,
    cb: &(dyn Fn(Frame) + Send + Sync),
) -> std::result::Result<gst::FlowSuccess, gst::FlowError> {
    let frame = sample_to_frame(sample).ok_or(gst::FlowError::Error)?;
    cb(frame);
    Ok(gst::FlowSuccess::Ok)
}

/// Grab a representative thumbnail frame (RGBA, ~`width`px wide at the source's
/// natural aspect) for a media file. Safe to call off the UI thread; None on
/// failure. Seeks a little in to skip black intro frames.
pub fn thumbnail(path: &Path, width: u32) -> Option<Frame> {
    ensure_encoder_ranks();
    gst::init().ok()?;
    let uri = gst::glib::filename_to_uri(path, None).ok()?;
    let pipeline = gst::Pipeline::new();
    let src = gst::ElementFactory::make("uridecodebin")
        .property("uri", &uri)
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
        return None;
    }
    if pipeline.state(gst::ClockTime::from_seconds(5)).0.is_err() {
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
            let _ = pipeline.state(gst::ClockTime::from_seconds(3));
        }
    }
    let frame = sink.pull_preroll().ok().and_then(|s| sample_to_frame(&s));
    let _ = pipeline.set_state(gst::State::Null);
    frame
}

/// Read a GES clip's current timeline geometry.
fn clip_geom(clip: &ges::Clip) -> ClipGeom {
    ClipGeom {
        start: Duration::from_nanos(clip.start().nseconds()),
        inpoint: Duration::from_nanos(clip.inpoint().nseconds()),
        duration: Duration::from_nanos(clip.duration().nseconds()),
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

/// Export/render settings: codec (→ container), output resolution, and target
/// video bitrate in kbit/s. A `bitrate_kbps` of 0 means "let the encoder decide".
#[derive(Clone, Copy, Debug)]
pub struct ExportSettings {
    pub codec: VideoCodec,
    pub width: i32,
    pub height: i32,
    pub bitrate_kbps: u32,
}

/// Progress of an export/render.
#[derive(Clone, Debug)]
pub enum RenderStatus {
    Rendering(f32), // 0..1
    Done,
    Failed(String),
}

/// Encoder ranks for export (must be set before the first gst init — read once at
/// registry load — hence it runs at every gst-init path).
///
/// - `nvautogpuh264enc:512` — prefer hardware NVENC H.264 (fast). On non-NVIDIA
///   machines the factory isn't registered, so encodebin falls back to x264enc.
/// - `x264enc:256` — software H.264 fallback.
/// - `mfaacenc:0` — **critical**: derank the Media Foundation AAC encoder. It ties
///   `voaacenc` at rank 128, and when encodebin picks it for the MP4 audio track it
///   spins up a D3D11 device that corrupts the GPU state, making NVENC's
///   `NvEncOpenEncodeSessionEx` fail `NV_ENC_ERR_INVALID_VERSION` ("Could not encode
///   stream" → 0-byte file). Forcing the software `voaacenc` makes hardware H.264
///   export reliable, even after the live preview has used the GPU.
pub(crate) fn ensure_encoder_ranks() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if std::env::var_os("GST_PLUGIN_FEATURE_RANK").is_none() {
            std::env::set_var(
                "GST_PLUGIN_FEATURE_RANK",
                "nvautogpuh264enc:512,x264enc:256,mfaacenc:0",
            );
        }
    });
}

/// The GES encoding profile for export settings: container + video + audio caps,
/// plus the output resolution (as a video-profile restriction so encodebin scales
/// to it) and the target bitrate (set on whichever encoder encodebin picks).
fn encoding_profile(s: ExportSettings) -> gst_pbutils::EncodingContainerProfile {
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
        .field("width", s.width.max(2))
        .field("height", s.height.max(2))
        .field("framerate", gst::Fraction::new(30, 1))
        .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
        .build();
    let mut vb = gst_pbutils::EncodingVideoProfile::builder(&video_caps).restriction(&restriction);
    // Target bitrate. Property name and unit differ by encoder: x264enc/NVENC use
    // "bitrate" in kbit/s (guint); vp8enc/vp9enc use "target-bitrate" in bit/s (gint).
    // ElementProperties applies to whichever matching encoder encodebin instantiates.
    if s.bitrate_kbps > 0 {
        let props = match s.codec {
            VideoCodec::H264 => gst_pbutils::ElementProperties::builder_general()
                .field("bitrate", s.bitrate_kbps)
                .build(),
            VideoCodec::Vp9 | VideoCodec::Vp8 => gst_pbutils::ElementProperties::builder_general()
                .field("target-bitrate", (s.bitrate_kbps.saturating_mul(1000)) as i32)
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

/// A clip's transform + audio level for the inspector. `scale` is 0..1 relative
/// to the largest size that fits the canvas WITHOUT distorting the source, so a
/// non-16:9 clip keeps its aspect ratio.
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

/// A GES-backed editing project: one timeline, one preview pipeline. Layers are
/// visual tracks, index 0 = bottom (top layers composite over lower ones).
pub struct Project {
    timeline: ges::Timeline,
    layers: Vec<ges::Layer>,
    pipeline: ges::Pipeline,
    /// Clips by GES name, so the GUI can edit them by id (slide/trim/transform).
    clips: HashMap<String, ges::Clip>,
    /// Set by edits, cleared by `refresh_preview` — coalesces repaints.
    dirty: std::cell::Cell<bool>,
    /// The preview video sink; kept so we can restore preview mode after a render.
    appsink: AppSink,
    /// Composited canvas ("viewport") size in px. Configurable via
    /// `set_canvas_size`; drives fit/layout and the video track restriction caps.
    canvas_w: i32,
    canvas_h: i32,
}

impl Project {
    /// Build an empty project whose preview pushes RGBA frames to `on_frame`
    /// (called from a GStreamer thread — the GUI must hop to the UI thread).
    pub fn new(on_frame: impl Fn(Frame) + Send + Sync + 'static) -> Result<Self> {
        ensure_encoder_ranks();
        gst::init()?;
        ges::init()?;

        let timeline = ges::Timeline::new_audio_video();
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

        let cb: Arc<dyn Fn(Frame) + Send + Sync> = Arc::new(on_frame);
        let cb_sample = cb.clone();
        let cb_preroll = cb;
        appsink.set_callbacks(
            AppSinkCallbacks::builder()
                // Playing: frames arrive as samples.
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    emit_sample(&sample, &*cb_sample)
                })
                // Paused / after a seek: the current frame arrives as a preroll
                // buffer, so deliver it too — otherwise edits don't repaint while
                // the timeline is paused.
                .new_preroll(move |sink| {
                    let sample = sink.pull_preroll().map_err(|_| gst::FlowError::Eos)?;
                    emit_sample(&sample, &*cb_preroll)
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
            appsink,
            canvas_w: CANVAS_W,
            canvas_h: CANVAS_H,
        })
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
        self.timeline.commit();
        self.dirty.set(true);
    }

    /// Ensure at least `index + 1` layers exist; return the layer at `index`.
    fn layer(&mut self, index: usize) -> ges::Layer {
        while self.layers.len() <= index {
            self.layers.push(self.timeline.append_layer());
        }
        self.layers[index].clone()
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
        let clip = ges::UriClip::new(&uri)?;
        clip.set_start(gst::ClockTime::from_nseconds(start.as_nanos() as u64));
        clip.set_inpoint(gst::ClockTime::from_nseconds(inpoint.as_nanos() as u64));
        clip.set_duration(gst::ClockTime::from_nseconds(duration.as_nanos() as u64));
        self.layer(track).add_clip(&clip)?;
        // Async commit (see append_clip): commit_sync() can deadlock during an
        // async pipeline state-change, so never block on the commit here.
        self.timeline.commit();
        let name = clip.name().map(|s| s.to_string()).unwrap_or_default();
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
        let asset = ges::UriClipAsset::request_sync(&uri)?;
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
        self.timeline.commit();
        let name = clip.name().map(|s| s.to_string()).unwrap_or_default();
        self.clips.insert(name.clone(), clip.clone());
        Ok(ClipInfo {
            id: ClipId(name),
            track,
            start: Duration::from_nanos(start_ct.nseconds()),
            duration: Duration::from_nanos(dur_ct.nseconds()),
        })
    }

    /// Slide a clip along its track by `delta_secs` (may be negative); start is
    /// clamped to >= 0. Returns the resulting geometry, or None for an unknown id.
    pub fn slide_clip(&mut self, id: &ClipId, delta_secs: f64) -> Option<ClipGeom> {
        let clip = self.clips.get(&id.0)?.clone();
        let start = clip.start().nseconds() as i128;
        let delta = (delta_secs * 1e9) as i128;
        let new_start = (start + delta).max(0) as u64;
        clip.set_start(gst::ClockTime::from_nseconds(new_start));
        self.timeline.commit();
        self.dirty.set(true);
        Some(clip_geom(&clip))
    }

    /// Trim a clip by dragging an edge. `edge < 0` = left edge (keeps the right
    /// end fixed by moving start+inpoint and shrinking duration); `edge > 0` =
    /// right edge (adjusts duration only). Clamped to the source bounds and a
    /// 0.2 s minimum. Returns the resulting geometry.
    pub fn trim_clip(&mut self, id: &ClipId, edge: i32, delta_secs: f64) -> Option<ClipGeom> {
        let clip = self.clips.get(&id.0)?.clone();
        let start = clip.start().nseconds() as i128;
        let inpoint = clip.inpoint().nseconds() as i128;
        let dur = clip.duration().nseconds() as i128;
        // max-duration is GST_CLOCK_TIME_NONE (→ None) for stills; a finite length
        // for real media, which caps how far the right edge can extend.
        let max_ns = clip
            .property::<Option<gst::ClockTime>>("max-duration")
            .map(|m| m.nseconds() as i128);
        let min_dur = 200_000_000i128; // 0.2 s
        let delta = (delta_secs * 1e9) as i128;

        if edge < 0 {
            // Left edge: end (start + duration) stays fixed; inpoint moves with start.
            let mut d = delta;
            if inpoint + d < 0 {
                d = -inpoint;
            }
            if start + d < 0 {
                d = -start;
            }
            if dur - d < min_dur {
                d = dur - min_dur;
            }
            let new_start = gst::ClockTime::from_nseconds((start + d) as u64);
            let new_inp = gst::ClockTime::from_nseconds((inpoint + d) as u64);
            let new_dur = gst::ClockTime::from_nseconds((dur - d) as u64);
            // Apply the shrinking property first so inpoint + duration never
            // transiently exceeds max-duration (which GES would clamp).
            if d >= 0 {
                clip.set_duration(new_dur);
                clip.set_inpoint(new_inp);
            } else {
                clip.set_inpoint(new_inp);
                clip.set_duration(new_dur);
            }
            clip.set_start(new_start);
        } else {
            // Right edge: only the duration changes.
            let mut new_dur = dur + delta;
            if new_dur < min_dur {
                new_dur = min_dur;
            }
            if let Some(m) = max_ns {
                if inpoint + new_dur > m {
                    new_dur = m - inpoint;
                }
            }
            clip.set_duration(gst::ClockTime::from_nseconds(new_dur as u64));
        }
        self.timeline.commit();
        self.dirty.set(true);
        Some(clip_geom(&clip))
    }

    /// Number of tracks (GES layers, 0 = top) in the timeline.
    pub fn track_count(&self) -> usize {
        self.layers.len()
    }

    /// Move a clip to `track`, creating the layer if `track` is one past the last
    /// (a new bottom track). Returns the resulting track index.
    pub fn move_clip_to_track(&mut self, id: &ClipId, track: usize) -> Option<usize> {
        let clip = self.clips.get(&id.0)?.clone();
        let target = self.layer(track);
        clip.move_to_layer(&target).ok()?;
        self.timeline.commit();
        self.dirty.set(true);
        Some(track)
    }

    /// The clip's current track index (its layer's priority, 0 = top).
    pub fn clip_track(&self, id: &ClipId) -> Option<usize> {
        self.clips.get(&id.0)?.layer().map(|l| l.priority() as usize)
    }

    /// Reorder tracks: move the track at `from` to position `to` (0 = top).
    pub fn move_track(&mut self, from: usize, to: usize) {
        if from >= self.layers.len() || to >= self.layers.len() || from == to {
            return;
        }
        let layer = self.layers[from].clone();
        let _ = self.timeline.move_layer(&layer, to as u32);
        // Resync our layer vec to the new priority order.
        self.layers = self.timeline.layers();
        self.timeline.commit();
        self.dirty.set(true);
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
        let scale = if width > 0 && fit_w > 0.0 {
            (width as f64 / fit_w).clamp(0.0, 1.0)
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
        let Some(clip) = self.clips.get(&id.0) else {
            return;
        };
        let (fit_w, fit_h) = match clip_natural_size(clip) {
            Some((w, h)) => fit_size(w, h, self.canvas_w, self.canvas_h),
            None => (self.canvas_w as f64, self.canvas_h as f64),
        };
        let width = (l.scale * fit_w).round().max(1.0) as i32;
        let height = (l.scale * fit_h).round().max(1.0) as i32;
        let _ = clip.set_child_property("posx", &l.posx.to_value());
        let _ = clip.set_child_property("posy", &l.posy.to_value());
        let _ = clip.set_child_property("width", &width.to_value());
        let _ = clip.set_child_property("height", &height.to_value());
        let _ = clip.set_child_property("alpha", &l.alpha.to_value());
        let _ = clip.set_child_property("volume", &l.volume.to_value());
        self.timeline.commit();
        self.dirty.set(true);
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
        let _ = clip.set_child_property("posx", &posx.to_value());
        let _ = clip.set_child_property("posy", &posy.to_value());
        let _ = clip.set_child_property("width", &(fw.round() as i32).to_value());
        let _ = clip.set_child_property("height", &(fh.round() as i32).to_value());
        self.timeline.commit();
        self.dirty.set(true);
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

    pub fn play(&self) -> Result<()> {
        self.pipeline.set_state(gst::State::Playing)?;
        Ok(())
    }

    pub fn pause(&self) -> Result<()> {
        self.pipeline.set_state(gst::State::Paused)?;
        Ok(())
    }

    pub fn seek(&self, pos: Duration) -> Result<()> {
        self.pipeline.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            gst::ClockTime::from_nseconds(pos.as_nanos() as u64),
        )?;
        Ok(())
    }

    /// Repaint the preview if an edit marked the timeline dirty since the last
    /// call. MUST be driven from a UI timer, never from the edit path: a slider
    /// drag fires dozens of edits/second, and one flush seek per edit floods the
    /// pipeline and freezes the app. Coalescing to the timer caps it to one seek
    /// per tick. No-op while actively playing (frames already flow).
    pub fn refresh_preview(&self) {
        if !self.dirty.replace(false) {
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
        let _ = self.pipeline.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            gst::ClockTime::from_nseconds(pos.as_nanos() as u64),
        );
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
    pub fn begin_render(&self, path: &Path, settings: ExportSettings) -> Result<()> {
        // Fully tear the preview down and WAIT for NULL before switching to render
        // mode. Coming straight from a playing preview, the GPU/CUDA context is not
        // released synchronously, so the NVENC encoder fails gst_nv_encoder_init_
        // session ("Could not encode stream" → 0-byte file). Waiting for the NULL
        // transition to complete releases it.
        self.pipeline.set_state(gst::State::Null)?;
        let _ = self.pipeline.state(gst::ClockTime::from_seconds(3));
        // Drop the custom preview sink so render mode can route to encodebin.
        self.pipeline
            .preview_set_video_sink(None::<&gst::Element>);
        let uri = gst::glib::filename_to_uri(path, None)?;
        let profile = encoding_profile(settings);
        self.pipeline.set_render_settings(uri.as_str(), &profile)?;
        self.pipeline.set_mode(ges::PipelineFlags::RENDER)?;
        self.pipeline.set_state(gst::State::Playing)?;
        Ok(())
    }

    /// Poll render progress — drive this from a UI timer. Consumes EOS/ERROR bus
    /// messages, so once it returns Done/Failed the render is finished.
    pub fn render_status(&self) -> RenderStatus {
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

    /// Stop rendering and restore the live preview (re-attaching the appsink).
    pub fn end_render(&self) -> Result<()> {
        self.pipeline.set_state(gst::State::Null)?;
        self.pipeline
            .preview_set_video_sink(Some(self.appsink.upcast_ref::<gst::Element>()));
        self.pipeline.set_mode(ges::PipelineFlags::FULL_PREVIEW)?;
        self.pipeline.set_state(gst::State::Paused)?;
        self.dirty.set(true);
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
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

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
            assert_eq!(f.rgba.len() as u32, f.width * f.height * 4);
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
        assert!(info2.start > Duration::ZERO, "second clip should start after the first");
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
        let out = std::env::temp_dir().join("kuvatin_render_test.webm");
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
                    bitrate_kbps: 1500,
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
        let out = std::env::temp_dir().join("kuvatin_eos_diag.mp4");
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
                    bitrate_kbps: 8000,
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

    /// Regression for the 111KB/no-moov export: a REALISTIC timeline — leading gap
    /// + a still-image overlay track — must render to H.264 MP4. Gap/overlay
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
        let out = std::env::temp_dir().join("kuvatin_gapped_overlay.mp4");
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
            .add_clip(&img, 0, Duration::ZERO, Duration::ZERO, Duration::from_secs(3))
            .expect("image clip");
        project
            .begin_render(
                &out,
                ExportSettings {
                    codec: VideoCodec::H264,
                    width: 1280,
                    height: 720,
                    bitrate_kbps: 8000,
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
        ensure_encoder_ranks();
        gst::init().unwrap();
        ges::init().unwrap();
        let p2 = path.clone();
        let ok = std::thread::spawn(move || warm_asset(&p2).is_ok())
            .join()
            .unwrap();
        assert!(ok, "warm_asset failed on a worker thread");
        // After warming, adding the clip should succeed (cache hit, no block).
        let mut project = Project::new(|_f| {}).expect("project");
        project.append_clip(&path, 0, None).expect("append after warm");
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
}
