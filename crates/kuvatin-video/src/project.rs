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

/// A clip's video transform + audio level, via GES child properties.
#[derive(Clone, Copy, Debug)]
pub struct Transform {
    pub posx: i32,
    pub posy: i32,
    pub width: i32,
    pub height: i32,
    pub alpha: f64,
    pub volume: f64,
}

/// A GES-backed editing project: one timeline, one preview pipeline. Layers are
/// visual tracks, index 0 = bottom (top layers composite over lower ones).
pub struct Project {
    timeline: ges::Timeline,
    layers: Vec<ges::Layer>,
    pipeline: ges::Pipeline,
    /// Clips by GES name, so the GUI can edit them by id (slide/trim/transform).
    clips: HashMap<String, ges::Clip>,
}

impl Project {
    /// Build an empty project whose preview pushes RGBA frames to `on_frame`
    /// (called from a GStreamer thread — the GUI must hop to the UI thread).
    pub fn new(on_frame: impl Fn(Frame) + Send + Sync + 'static) -> Result<Self> {
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
        appsink.set_callbacks(
            AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let caps = sample.caps().ok_or(gst::FlowError::Error)?;
                    let info = gstreamer_video::VideoInfo::from_caps(caps)
                        .map_err(|_| gst::FlowError::Error)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    cb(Frame {
                        width: info.width(),
                        height: info.height(),
                        rgba: map.as_slice().to_vec(),
                    });
                    Ok(gst::FlowSuccess::Ok)
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
        })
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
        Some(clip_geom(&clip))
    }

    /// Read a clip's current transform (video child props + audio volume).
    pub fn clip_transform(&self, id: &ClipId) -> Option<Transform> {
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
        Some(Transform {
            posx: geti("posx", 0),
            posy: geti("posy", 0),
            width: geti("width", CANVAS_W),
            height: geti("height", CANVAS_H),
            alpha: getf("alpha", 1.0),
            volume: getf("volume", 1.0),
        })
    }

    /// Apply a transform to a clip, live. Missing child properties (e.g. volume
    /// on a still image, which has no audio) are ignored.
    pub fn set_clip_transform(&mut self, id: &ClipId, t: Transform) {
        let Some(clip) = self.clips.get(&id.0) else {
            return;
        };
        let _ = clip.set_child_property("posx", &t.posx.to_value());
        let _ = clip.set_child_property("posy", &t.posy.to_value());
        let _ = clip.set_child_property("width", &t.width.to_value());
        let _ = clip.set_child_property("height", &t.height.to_value());
        let _ = clip.set_child_property("alpha", &t.alpha.to_value());
        let _ = clip.set_child_property("volume", &t.volume.to_value());
        self.timeline.commit();
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

    pub fn position(&self) -> Option<Duration> {
        self.pipeline
            .query_position::<gst::ClockTime>()
            .map(|t| Duration::from_nanos(t.nseconds()))
    }

    pub fn duration(&self) -> Option<Duration> {
        let d = self.timeline.duration();
        (d.nseconds() > 0).then(|| Duration::from_nanos(d.nseconds()))
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
}
