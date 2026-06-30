# Video Editor — Layered Timeline (Plan 3) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or executing-plans. Steps use checkbox (`- [ ]`) syntax. The GES + compositor spikes already PASSED (see the spec + memory); this builds the real editor on GES.

**Goal:** Turn Videos mode into a layered timeline editor — arrange multiple clips/images on layers, slide them in time, drag edges to trim, with a live composited preview. (Export is Plan 4; GES also does that later.)

**Architecture:** Built on **GStreamer Editing Services (GES)**. `kuvatin-video` gains a `Project` controller wrapping a `GESTimeline` + `GESPipeline`; the pipeline previews into an `appsink` that pushes RGBA frames to Slint (same path as the `Player`). The timeline UI is a thin view over a Rust timeline model that mirrors GES (`GESLayer` per track, `GESUriClip` per item with `start` = timeline position, `inpoint`/`duration` = trim). GES handles compositing, transforms (clip child props), and seeking. The single-clip `Player` (Plan 2) stays for quick playback; the editor uses `Project`.

**Tech Stack:** Rust, `gstreamer` + `gstreamer-editing-services` 0.23, Slint 1.8, GStreamer 1.26 (GES/NLE already installed + bundled).

**Build/run env:** unchanged from Plan 2 (`.cargo/config.toml` for building; GStreamer `bin` on PATH to run; installer already bundles `ges`/`nle`).

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/kuvatin-video/Cargo.toml` | add `gstreamer-editing-services` | Modify |
| `crates/kuvatin-video/src/project.rs` | GES-backed `Project`: timeline + pipeline + frames + edits | Create |
| `crates/kuvatin-video/src/lib.rs` | re-export `Project`, `ClipId`, `LayerKind` | Modify |
| `crates/kuvatin/src/video_mode.rs` | timeline model + orchestration of `Project` (keep gui.rs lean) | Create |
| `crates/kuvatin/src/gui.rs` | wire video mode to `Project`/`video_mode` | Modify |
| `crates/kuvatin/ui/app.slint` | timeline band UI (layers, clip blocks, slide, trim handles), inspector | Modify |

---

## Task 1: GES `Project` controller (the editor engine)

**Files:** `crates/kuvatin-video/Cargo.toml`, `crates/kuvatin-video/src/project.rs`, `lib.rs`.

The spike proved: `GESTimeline` + `GESUriClip` previews into an appsink and seeks. Wrap it.

- [ ] **Step 1:** add `gstreamer-editing-services = "0.23"` to `kuvatin-video/Cargo.toml`.

- [ ] **Step 2:** create `project.rs` with this API (fill bodies per the validated spike):

```rust
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use anyhow::Result;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;
use gstreamer_editing_services as ges;
use ges::prelude::*;
use crate::Frame; // reuse Plan 2's Frame { width, height, rgba }

/// A handle to a clip on the timeline (its GES clip name).
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ClipId(String);

/// A GES-backed editing project: one timeline, one preview pipeline.
pub struct Project {
    timeline: ges::Timeline,
    layers: Vec<ges::Layer>,        // index = visual track, 0 = bottom
    pipeline: ges::Pipeline,
}

impl Project {
    /// Build an empty project whose preview pushes RGBA frames to `on_frame`
    /// (called from a GStreamer thread; GUI must hop to the UI thread).
    pub fn new(on_frame: impl Fn(Frame) + Send + Sync + 'static) -> Result<Self> {
        gst::init()?;
        ges::init()?;
        let timeline = ges::Timeline::new_audio_video();
        let layer = timeline.append_layer();
        let pipeline = ges::Pipeline::new();
        pipeline.set_timeline(&timeline)?;

        let appsink = AppSink::builder()
            .caps(&gst::Caps::builder("video/x-raw").field("format", "RGBA").build())
            .max_buffers(2).drop(true)
            .build();
        let cb: Arc<dyn Fn(Frame) + Send + Sync> = Arc::new(on_frame);
        appsink.set_callbacks(gstreamer_app::AppSinkCallbacks::builder()
            .new_sample(move |s| {
                let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let caps = sample.caps().ok_or(gst::FlowError::Error)?;
                let info = gstreamer_video::VideoInfo::from_caps(caps).map_err(|_| gst::FlowError::Error)?;
                let buf = sample.buffer().ok_or(gst::FlowError::Error)?;
                let map = buf.map_readable().map_err(|_| gst::FlowError::Error)?;
                cb(Frame { width: info.width(), height: info.height(), rgba: map.as_slice().to_vec() });
                Ok(gst::FlowSuccess::Ok)
            }).build());
        pipeline.preview_set_video_sink(Some(appsink.upcast_ref::<gst::Element>()));
        pipeline.set_mode(ges::PipelineFlags::FULL_PREVIEW)?;

        Ok(Self { timeline, layers: vec![layer], pipeline })
    }

    /// Ensure `index` layers exist (0 = bottom). Returns the layer.
    fn layer(&mut self, index: usize) -> ges::Layer {
        while self.layers.len() <= index {
            self.layers.push(self.timeline.append_layer());
        }
        self.layers[index].clone()
    }

    /// Add a file as a clip on `track` at timeline position `start`, showing the
    /// source range [inpoint, inpoint+duration). Returns its id.
    pub fn add_clip(&mut self, path: &Path, track: usize, start: Duration, inpoint: Duration, duration: Duration) -> Result<ClipId> {
        let uri = gst::glib::filename_to_uri(path, None)?;
        let clip = ges::UriClip::new(&uri)?;
        clip.set_start(gst::ClockTime::from_nseconds(start.as_nanos() as u64));
        clip.set_inpoint(gst::ClockTime::from_nseconds(inpoint.as_nanos() as u64));
        clip.set_duration(gst::ClockTime::from_nseconds(duration.as_nanos() as u64));
        self.layer(track).add_clip(&clip)?;
        self.timeline.commit_sync();
        Ok(ClipId(clip.name().map(|s| s.to_string()).unwrap_or_default()))
    }

    pub fn play(&self) -> Result<()> { self.pipeline.set_state(gst::State::Playing)?; Ok(()) }
    pub fn pause(&self) -> Result<()> { self.pipeline.set_state(gst::State::Paused)?; Ok(()) }
    pub fn seek(&self, pos: Duration) -> Result<()> {
        self.pipeline.seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            gst::ClockTime::from_nseconds(pos.as_nanos() as u64))?;
        Ok(())
    }
    pub fn position(&self) -> Option<Duration> {
        self.pipeline.query_position::<gst::ClockTime>().map(|t| Duration::from_nanos(t.nseconds()))
    }
    pub fn duration(&self) -> Option<Duration> {
        Some(Duration::from_nanos(self.timeline.duration().nseconds()))
    }
}

impl Drop for Project {
    fn drop(&mut self) { let _ = self.pipeline.set_state(gst::State::Null); }
}
```

- [ ] **Step 3:** `lib.rs` — add `pub mod project; pub use project::{Project, ClipId};`.

- [ ] **Step 4:** build: `cargo build -p kuvatin-video` (iterate on GES 0.23 API names — the spike confirmed the core path).

- [ ] **Step 5:** add a gated integration test mirroring the spike (`GST_TEST_FILE` → add a clip, play, assert frames + duration > 0). Run with the GStreamer `bin` on PATH and a fixture. Commit `feat(video): GES-backed Project controller`.

---

## Task 2: Timeline model + band UI (read-only render)

**Files:** `app.slint`, `crates/kuvatin/src/video_mode.rs`, `gui.rs`.

- [ ] A Rust timeline model: `Vec<Track>` of `Clip { id, name, track, start, duration, inpoint }`, mirrored to a Slint model (`timeline-clips: [{ track, x, w, name }]` computed from a pixels-per-second scale).
- [ ] Replace the single-clip video preview wiring with the `Project`: opening media adds a clip to track 0 and the Viewer shows the GES composited preview.
- [ ] Render a **timeline band** at the bottom of Videos mode: a time ruler + one row per track + clip blocks positioned by `x = start * pps`, width `= duration * pps`. A playhead from `Project::position`.
- [ ] Build + commit `feat(video): timeline band rendering over a GES project`.

---

## Task 3: Add + slide clips

- [ ] Opening media adds it to a chosen track at the playhead (or appended). Multiple clips/tracks supported.
- [ ] Drag a clip block horizontally → update its `start` (Rust model + `clip.set_start` + `timeline.commit`). Snap to a small grid. Clamp >= 0.
- [ ] Commit `feat(video): add and slide clips on the timeline`.

## Task 4: Edge-trim

- [ ] Drag a clip's left edge → adjust `inpoint` + `start` + `duration`; right edge → adjust `duration`. Reflect in GES (`set_inpoint`/`set_duration`) + the model; clamp to the source length.
- [ ] Commit `feat(video): trim clips by dragging their edges`.

## Task 5: Image overlays + per-layer transform + inspector

- [ ] Image files become clips with a chosen duration on an upper track (overlay).
- [ ] The right Inspector edits the selected clip's transform (posx/posy/width/height/alpha via GES clip child properties) + volume; changes reflect live in the preview.
- [ ] Commit `feat(video): image overlays and per-clip transform inspector`.

---

## Self-Review

**Spec coverage (Stage = compositor/timeline editor):** GES project + preview (Task 1); layered timeline render (Task 2); arrange/slide (Task 3); edge-trim (Task 4); overlays + transforms (Task 5). Export is correctly out of scope (Plan 4, also GES).

**Risks:** (1) GES 0.23 API specifics (clip child-property names for transform) — iterate against the compiler; the core timeline/preview/seek path is spike-proven. (2) live edits while playing — `commit`/`commit_sync` after each change; pause-edit-resume if glitchy. (3) frame flooding — appsink `max-buffers=2 drop`. (4) the single-clip `Player` vs `Project` overlap — `Project` becomes the video-mode engine; keep `Player` only if a simple-playback path is still wanted.

**Type consistency:** `Project::new(on_frame)`, `add_clip(path, track, start, inpoint, duration) -> ClipId`, `play/pause/seek/position/duration`, `Frame { width, height, rgba }` — consistent with Plan 2's crate.
