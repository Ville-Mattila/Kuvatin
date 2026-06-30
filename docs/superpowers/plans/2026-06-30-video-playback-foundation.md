# Video Playback Foundation (Plan 2) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax. The GStreamer integration spike already PASSED — see the spec and the build-env recipe below; this plan builds the real feature on that.

**Goal:** Open a video in Kuvatin's Videos mode and watch it play — with synced audio, a scrubber, and frame-accurate seeking — single clip, no timeline yet.

**Architecture:** A new `kuvatin-video` crate wraps GStreamer (`gstreamer-rs`) behind a `Player` controller: `playbin` does demux/decode/audio/sync; an `appsink` delivers decoded **RGBA frames** to a callback. The GUI turns each frame into a `slint::Image` (same `SharedPixelBuffer<Rgba8Pixel>` path the image preview uses) on the UI thread via `slint::invoke_from_event_loop`. `kuvatin-core` stays image-only.

**Tech Stack:** Rust, `gstreamer`/`gstreamer-app`/`gstreamer-video` 0.23, Slint 1.8, GStreamer 1.26 MSVC SDK.

**Build env (validated by the spike).** GStreamer 1.26.11 MSVC dev SDK installed at `C:\Program Files\gstreamer\1.0\msvc_x86_64\` (the SDK bundles `bin\pkg-config.exe`, not on PATH). To build `gstreamer-rs`, these must be set (Task 1 makes them automatic via `.cargo/config.toml`):
```
GSTREAMER_1_0_ROOT_MSVC_X86_64 = C:\Program Files\gstreamer\1.0\msvc_x86_64\
PKG_CONFIG      = <root>\bin\pkg-config.exe
PKG_CONFIG_PATH = <root>\lib\pkgconfig
```
To **run** (decode), the SDK's `bin` must be on `PATH` (the GStreamer DLLs). For dev/test runs, prepend it. For end users, the installer (Task 6) bundles the DLLs next to the exe.

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `.cargo/config.toml` | Build env for GStreamer | Create |
| `Cargo.toml` (workspace) | Add member | Modify |
| `crates/kuvatin-video/Cargo.toml` | New crate manifest | Create |
| `crates/kuvatin-video/src/lib.rs` | `Player` API: load/play/pause/seek/position/duration + frame callback | Create |
| `crates/kuvatin-video/src/player.rs` | GStreamer `playbin` + `appsink` impl | Create |
| `crates/kuvatin/Cargo.toml` | Depend on `kuvatin-video` | Modify |
| `crates/kuvatin/ui/app.slint` | Video-mode UI: frame Image, transport, media list | Modify |
| `crates/kuvatin/src/gui.rs` | Wire Player ↔ Slint (frames, controls) | Modify |
| `crates/kuvatin/src/video_mode.rs` | Video-mode state + Player orchestration (keep gui.rs focused) | Create |
| `.github/workflows/release.yml` | Install GStreamer in the Windows job | Modify |
| `crates/kuvatin/wix/main.wxs` + `docs/.../gstreamer.md` | Bundle runtime DLLs; document build env | Modify/Create |

---

## Task 1: Build-env config + `kuvatin-video` crate skeleton

**Files:** Create `.cargo/config.toml`, `crates/kuvatin-video/Cargo.toml`, `crates/kuvatin-video/src/lib.rs`; modify root `Cargo.toml`.

- [ ] **Step 1: Create `.cargo/config.toml`** so every cargo build finds GStreamer:

```toml
# GStreamer (MSVC) build env. The SDK installs here by default and bundles its
# own pkg-config; gstreamer-rs needs these to locate the libraries. To RUN the
# app/tests, also put <root>\bin on PATH (the runtime DLLs); the installer
# bundles those for end users.
[env]
GSTREAMER_1_0_ROOT_MSVC_X86_64 = "C:\\Program Files\\gstreamer\\1.0\\msvc_x86_64\\"
PKG_CONFIG = "C:\\Program Files\\gstreamer\\1.0\\msvc_x86_64\\bin\\pkg-config.exe"
PKG_CONFIG_PATH = "C:\\Program Files\\gstreamer\\1.0\\msvc_x86_64\\lib\\pkgconfig"
```

- [ ] **Step 2: Create `crates/kuvatin-video/Cargo.toml`:**

```toml
[package]
name = "kuvatin-video"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
gstreamer = "0.23"
gstreamer-app = "0.23"
gstreamer-video = "0.23"
anyhow = { workspace = true }
```

- [ ] **Step 3: Create `crates/kuvatin-video/src/lib.rs`** with a minimal compiling stub:

```rust
//! GStreamer-backed video playback for Kuvatin. The GUI talks only to `Player`.
pub mod player;
pub use player::{Frame, Player};
```

And `crates/kuvatin-video/src/player.rs`:

```rust
//! Single-clip player. Filled in Task 2.

/// One decoded RGBA video frame handed to the GUI.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>, // width*height*4
}

/// Placeholder so the crate builds before Task 2.
pub struct Player;

impl Player {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Player)
    }
}
```

- [ ] **Step 4: Add to the workspace** — in root `Cargo.toml`, change `members = ["crates/kuvatin-core", "crates/kuvatin"]` to include `"crates/kuvatin-video"`.

- [ ] **Step 5: Build the whole workspace** (the config.toml supplies the env):

Run: `cargo build`
Expected: builds clean, including `kuvatin-video` linking GStreamer. If pkg-config errors, confirm the SDK path in `.cargo/config.toml` matches the install.

- [ ] **Step 6: Commit**
```bash
git add .cargo/config.toml Cargo.toml crates/kuvatin-video
git commit -m "feat(video): build-env config + kuvatin-video crate skeleton"
```

---

## Task 2: `Player` — playbin + appsink, controls, frame callback

**Files:** `crates/kuvatin-video/src/player.rs`; test in the same file.

The spike validated `appsink` pulling RGBA frames. Build the real player:
- `playbin` element with `uri` set from a file path (`gstreamer::filename_to_uri`).
- A video sink branch ending in `appsink` with caps `video/x-raw,format=RGBA`; `playbin`'s `video-sink` property set to a bin wrapping `videoconvert ! appsink`.
- `appsink` `new-sample` callback (set `emit_signals(true)` or a callbacks struct) pulls the sample, maps the buffer, and invokes the user frame callback with a `Frame`.
- Audio: leave `playbin` to its default audio sink (auto, synced).
- Controls: `play`/`pause` set pipeline state; `seek(position)` via `seek_simple(SeekFlags::FLUSH | KEY_UNIT, ClockTime)`; `position()`/`duration()` query the pipeline.

- [ ] **Step 1: Implement `Player`** with this public API (fill bodies per the spike + `gstreamer-app` `AppSinkCallbacks`):

```rust
use std::sync::{Arc, Mutex};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSinkCallbacks};
use gstreamer_video as gst_video;

pub struct Frame { pub width: u32, pub height: u32, pub rgba: Vec<u8> }

pub struct Player {
    pipeline: gst::Pipeline,
}

impl Player {
    /// `on_frame` is called from a GStreamer streaming thread for each decoded
    /// RGBA frame. The GUI must hop to the UI thread (invoke_from_event_loop).
    pub fn new(on_frame: impl Fn(Frame) + Send + Sync + 'static) -> anyhow::Result<Self> {
        gst::init()?;
        let playbin = gst::ElementFactory::make("playbin").build()?;
        // video-sink = bin: appsink with RGBA caps
        let appsink = gst::ElementFactory::make("appsink").build()?.downcast::<AppSink>().unwrap();
        appsink.set_caps(Some(&gst::Caps::builder("video/x-raw").field("format", "RGBA").build()));
        let cb = Arc::new(on_frame);
        appsink.set_callbacks(AppSinkCallbacks::builder()
            .new_sample(move |s| {
                if let Ok(sample) = s.pull_sample() {
                    if let (Some(caps), Some(buf)) = (sample.caps(), sample.buffer()) {
                        if let Ok(info) = gst_video::VideoInfo::from_caps(caps) {
                            if let Ok(map) = buf.map_readable() {
                                cb(Frame { width: info.width(), height: info.height(), rgba: map.to_vec() });
                            }
                        }
                    }
                    Ok(gst::FlowSuccess::Ok)
                } else { Err(gst::FlowError::Eos) }
            }).build());
        // Wrap appsink in a bin with videoconvert so any decoded format -> RGBA.
        let convert = gst::ElementFactory::make("videoconvert").build()?;
        let bin = gst::Bin::new();
        bin.add_many([&convert, appsink.upcast_ref()])?;
        gst::Element::link_many([&convert, appsink.upcast_ref()])?;
        let pad = convert.static_pad("sink").unwrap();
        bin.add_pad(&gst::GhostPad::with_target(&pad)?)?;
        playbin.set_property("video-sink", &bin);
        let pipeline = playbin.downcast::<gst::Pipeline>()
            .map_err(|_| anyhow::anyhow!("playbin is not a pipeline"))?;
        Ok(Self { pipeline })
    }

    pub fn load(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let uri = gst::filename_to_uri(path, None)?;
        self.pipeline.set_property("uri", uri.as_str());
        Ok(())
    }
    pub fn play(&self)  -> anyhow::Result<()> { self.pipeline.set_state(gst::State::Playing)?; Ok(()) }
    pub fn pause(&self) -> anyhow::Result<()> { self.pipeline.set_state(gst::State::Paused)?; Ok(()) }
    pub fn seek(&self, pos: std::time::Duration) -> anyhow::Result<()> {
        self.pipeline.seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            gst::ClockTime::from_nseconds(pos.as_nanos() as u64))?;
        Ok(())
    }
    pub fn position(&self) -> Option<std::time::Duration> {
        self.pipeline.query_position::<gst::ClockTime>().map(|t| std::time::Duration::from_nanos(t.nseconds()))
    }
    pub fn duration(&self) -> Option<std::time::Duration> {
        self.pipeline.query_duration::<gst::ClockTime>().map(|t| std::time::Duration::from_nanos(t.nseconds()))
    }
}

impl Drop for Player {
    fn drop(&mut self) { let _ = self.pipeline.set_state(gst::State::Null); }
}
```

> The exact `gstreamer-rs` 0.23 method names (e.g. `with_target`, `add_pad`, error types) may need small adjustments — iterate against `cargo build`. The spike confirmed the crate set + the appsink pull pattern compile and run.

- [ ] **Step 2: Add an integration test** gated behind a `GST_TEST_FILE` env var (CI/dev provides a short fixture clip), since playback needs the runtime DLLs on PATH and a media file:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[test]
    fn pulls_frames_from_fixture() {
        let Some(path) = std::env::var_os("GST_TEST_FILE") else {
            eprintln!("skipping: set GST_TEST_FILE to a short video"); return;
        };
        let count = Arc::new(AtomicU32::new(0));
        let c2 = count.clone();
        let player = Player::new(move |f| { assert!(f.width > 0 && f.rgba.len() as u32 == f.width*f.height*4); c2.fetch_add(1, Ordering::SeqCst); }).unwrap();
        player.load(std::path::Path::new(&path)).unwrap();
        player.play().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(800));
        assert!(count.load(Ordering::SeqCst) > 0, "no frames decoded");
    }
}
```

- [ ] **Step 3: Build + test** (with `bin` on PATH):
Run (PowerShell): set `$env:Path` to include the GStreamer `bin`, then `cargo test -p kuvatin-video`. Without `GST_TEST_FILE` the test self-skips; with a short clip it must decode frames.

- [ ] **Step 4: Commit** `feat(video): playbin+appsink Player with controls`.

---

## Task 3: Frame → Viewer bridge in Videos mode

**Files:** `crates/kuvatin/Cargo.toml`, `crates/kuvatin/src/video_mode.rs` (new), `crates/kuvatin/src/gui.rs`, `crates/kuvatin/ui/app.slint`.

- [ ] **Step 1:** `kuvatin/Cargo.toml` — add `kuvatin-video = { path = "../kuvatin-video" }`.
- [ ] **Step 2:** In `app.slint`, replace the Videos-mode placeholder (`if root.app-mode == 1: Rectangle { ... "coming soon" }`) with the video layout: the shared Viewer card showing `in property <image> video-frame;` (an `Image { source: root.video-frame; image-fit: contain; }`), with a media list on the left and a transport row below (added in Task 4). Add `callback video-open(); in-out property <bool> video-playing;` etc.
- [ ] **Step 3:** `video_mode.rs` — create the `Player` with a frame callback that converts `Frame` → `slint::Image` (`SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&f.rgba, f.width, f.height)`) and pushes it to the UI via `slint::invoke_from_event_loop(move || ui.set_video_frame(img))` (use a `Weak` handle; throttle/replace-latest so the UI isn't flooded).
- [ ] **Step 4:** Wire an "Open video…" (rfd file dialog, video extensions) that loads+plays the chosen file; confirm a frame shows in the Viewer.
- [ ] **Step 5:** Build, run manually (PATH has GStreamer bin), confirm playback renders. Commit `feat(video): render player frames in the Viewer`.

> Verification note: as with Plan 1, agents can `cargo build` but not watch the GUI — final visual confirmation is the user's via `cargo run` with the GStreamer `bin` on PATH.

---

## Task 4: Media panel + transport controls

**Files:** `app.slint`, `gui.rs`, `video_mode.rs`.

- [ ] Media panel (left, in Videos mode): an "Open video(s)…" button + a list of opened clips; clicking one loads+plays it.
- [ ] Transport row under the Viewer: play/pause (toggles `Player::play/pause`), a scrubber bound to `position/duration` (drag → `Player::seek`), a `mm:ss / mm:ss` time label, a volume control (`playbin` `volume` property 0..1), and a fullscreen/frame-step affordance if cheap. A repeating `slint::Timer` polls `position()` to advance the scrubber.
- [ ] Build + commit `feat(video): media panel and transport controls`.

---

## Task 5: CI — install GStreamer in the Windows job

**Files:** `.github/workflows/release.yml`.

- [ ] In the `windows` job, before the build, add a step that downloads + silently installs the GStreamer **runtime + devel** MSIs (1.26.x, MSVC x86_64) to the default path via `msiexec /qn ADDLOCAL=ALL`, and prepends `<root>\bin` to `$GITHUB_PATH`. The `.cargo/config.toml` then supplies the build env. Cache the MSIs if practical.
- [ ] Commit `ci(video): install GStreamer in the Windows build job`.

---

## Task 6: Installer bundles the GStreamer runtime + docs

**Files:** `crates/kuvatin/wix/main.wxs`, `docs/superpowers/...` build note (or `crates/kuvatin/wix/README.md`).

- [ ] Bundle the GStreamer **runtime** DLLs/plugins the app needs next to `kuvatin.exe` (or under the install dir, with the app adding them to its DLL search path at startup). Pragmatic first cut: harvest the required `bin\*.dll` + the `lib\gstreamer-1.0` plugins for playback (`coreelements`, `playback`, `typefind`, `videoconvert`, codecs, audio sinks) into the WiX component set. Expect the installer to grow to ~50–80 MB. Verify a clean install plays a video on a machine without GStreamer.
- [ ] Document the GStreamer dev setup (mirror `wix/README.md`).
- [ ] Commit `feat(video): bundle GStreamer runtime in the installer`.

---

## Self-Review

**Spec coverage (Stage = video playback foundation):** Player with audio/seek (Tasks 1–2); frames in the shared Viewer (Task 3); media panel + transport (Task 4); single clip, no timeline (correctly out of scope → Plan 3); installer/CI carry GStreamer (Tasks 5–6).

**Risks:** (1) exact `gstreamer-rs` 0.23 API names — iterate against the compiler (spike proved the crate set works). (2) runtime DLL discovery — handled by PATH (dev) / bundling (Task 6). (3) frame delivery flooding the UI — throttle/replace-latest in Task 3. (4) installer DLL harvest completeness — verify on a clean machine in Task 6.

**Type consistency:** `Player::new(on_frame)`, `Frame { width, height, rgba }`, `load/play/pause/seek/position/duration`, `video-frame` Slint property — used consistently across tasks.
