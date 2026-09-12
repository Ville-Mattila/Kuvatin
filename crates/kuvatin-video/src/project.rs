//! GES-backed editing project: a timeline of layers + clips with a composited
//! preview. The GUI's timeline UI drives this; GES handles compositing,
//! transforms, trims, and seeking (and, later, render-to-file for export).

use std::collections::HashMap;
use std::path::Path;
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

/// Push one RGBA video sample to the frame callback.
fn emit_sample(
    sample: &gst::Sample,
    cb: &(dyn Fn(FrameView<'_>) + Send + Sync),
) -> std::result::Result<gst::FlowSuccess, gst::FlowError> {
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

/// Left-edge trim math: the clip's END stays fixed; start and inpoint move by
/// the (clamped) delta and the duration shrinks/grows to match. All results are
/// guaranteed non-negative — the previous version applied the min-duration
/// override AFTER the >=0 clamps, so a clip already shorter than the minimum
/// could push inpoint/start negative and wrap through the u64 cast into a
/// ~10^5-hour ClockTime. Precedence here: never-negative beats min-duration.
fn trim_left_math(start: i128, inpoint: i128, dur: i128, delta: i128) -> (i128, i128, i128) {
    let mut d = delta;
    d = d.min(dur - MIN_TRIM_NS); // keep at least the minimum (may go negative)
    d = d.max(-inpoint).max(-start); // never trim before the source/timeline origin
    ((start + d).max(0), (inpoint + d).max(0), (dur - d).max(0))
}

/// Right-edge trim math: only the duration changes. Clamped to the minimum,
/// then capped by the source's max-duration (which wins over the minimum when
/// the two conflict, e.g. `inpoint` near the end of the media), floored at 0.
fn trim_right_math(inpoint: i128, dur: i128, delta: i128, max_ns: Option<i128>) -> i128 {
    let mut nd = (dur + delta).max(MIN_TRIM_NS);
    if let Some(m) = max_ns {
        nd = nd.min(m - inpoint);
    }
    nd.max(0)
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
    /// True between `begin_render` and `end_render`. Transport and edit
    /// methods are inert while set: a Space/Delete key during an export used
    /// to pause the RENDER pipeline or commit a removal mid-render.
    rendering: std::cell::Cell<bool>,
    /// The encoder element the current or last render used (see
    /// [`Project::render_encoder`]).
    last_encoder: std::cell::RefCell<Option<String>>,
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

        let cb: Arc<dyn Fn(FrameView<'_>) + Send + Sync> = Arc::new(on_frame);
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
            rendering: std::cell::Cell::new(false),
            last_encoder: std::cell::RefCell::new(None),
        })
    }

    /// Whether an export/render is in progress (edits and transport are inert).
    pub fn is_rendering(&self) -> bool {
        self.rendering.get()
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
        self.timeline.commit();
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
        self.timeline.commit();
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
        self.timeline.commit();
        self.dirty.set(true);
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

        if edge < 0 {
            let (ns, ni, nd) = trim_left_math(start, inpoint, dur, delta);
            let new_start = gst::ClockTime::from_nseconds(ns as u64);
            let new_inp = gst::ClockTime::from_nseconds(ni as u64);
            let new_dur = gst::ClockTime::from_nseconds(nd as u64);
            // Apply the shrinking property first so inpoint + duration never
            // transiently exceeds max-duration (which GES would clamp).
            if nd <= dur {
                clip.set_duration(new_dur);
                clip.set_inpoint(new_inp);
            } else {
                clip.set_inpoint(new_inp);
                clip.set_duration(new_dur);
            }
            clip.set_start(new_start);
        } else {
            let nd = trim_right_math(inpoint, dur, delta, max_ns);
            clip.set_duration(gst::ClockTime::from_nseconds(nd as u64));
        }
        self.timeline.commit();
        self.dirty.set(true);
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
        self.timeline.commit();
        self.dirty.set(true);
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
        self.timeline.commit();
        self.dirty.set(true);
        true
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
            if ges::UriClipAsset::request_sync(&rec.uri).is_err() {
                missing.push(name());
                continue;
            }
            let placed = self.add_clip_uri(
                &rec.uri,
                rec.track,
                Duration::from_secs_f64(rec.start.max(0.0)),
                Duration::from_secs_f64(rec.inpoint.max(0.0)),
                Duration::from_secs_f64(rec.duration.max(0.0)),
            );
            match placed {
                Ok(id) => self.set_clip_layout(&id, rec.layout.into()),
                Err(_) => missing.push(name()),
            }
        }
        self.timeline.commit();
        self.dirty.set(true);
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
        Ok(())
    }

    /// Repaint the preview if an edit marked the timeline dirty since the last
    /// call. MUST be driven from a UI timer, never from the edit path: a slider
    /// drag fires dozens of edits/second, and one flush seek per edit floods the
    /// pipeline and freezes the app. Coalescing to the timer caps it to one seek
    /// per tick. No-op while actively playing (frames already flow).
    pub fn refresh_preview(&self) {
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
        let _ = self.pipeline.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::ClockTime::from_nseconds(pos.as_nanos() as u64),
        );
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
        Ok(())
    }

    /// Has the preview prerolled? Hands the timeline back (transport and edits
    /// live again) the first time it says `Ready`.
    pub fn restore_ready(&self) -> Step {
        let step = settled(&self.pipeline);
        if step == Step::Ready && self.rendering.get() {
            self.rendering.set(false);
            self.dirty.set(true);
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
        let (ns, ni, nd) = trim_left_math(0, 0, 100_000_000, 50_000_000);
        assert!(ns >= 0 && ni >= 0 && nd >= 0, "wrapped: {ns} {ni} {nd}");

        // Left edge, ordinary trim: end stays fixed.
        let (ns, ni, nd) = trim_left_math(2 * s, s, 5 * s, s);
        assert_eq!((ns, ni, nd), (3 * s, 2 * s, 4 * s));
        assert_eq!(ns + nd, 2 * s + 5 * s, "right end moved");

        // Left edge can't trim before the source origin.
        let (ns, ni, nd) = trim_left_math(3 * s, s, 5 * s, -2 * s);
        assert_eq!(ni, 0, "inpoint clamped to source start");
        assert!(ns >= 0 && nd >= 0);

        // Right edge: inpoint at/past max-duration used to underflow.
        let nd = trim_right_math(10 * s, 5 * s, s, Some(8 * s));
        assert!(nd >= 0, "wrapped: {nd}");

        // Right edge respects the minimum when there's room.
        let nd = trim_right_math(0, s, -10 * s, Some(100 * s));
        assert_eq!(nd, MIN_TRIM_NS);
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
}
