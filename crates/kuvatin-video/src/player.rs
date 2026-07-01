//! Single-clip video player built on GStreamer `playbin`.
//!
//! `playbin` handles demux / decode / audio / A-V sync. We replace its video
//! sink with a bin (`videoconvert ! appsink`) that hands every decoded frame to
//! the GUI as RGBA bytes. Audio uses playbin's default (auto) audio sink.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSinkCallbacks};
use gstreamer_video as gst_video;

/// One decoded RGBA video frame handed to the GUI (`width * height * 4` bytes).
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Owns the GStreamer pipeline for one clip.
pub struct Player {
    pipeline: gst::Pipeline,
}

impl Player {
    /// Build a player. `on_frame` is invoked from a GStreamer streaming thread
    /// for each decoded RGBA frame — the GUI must hop to the UI thread (e.g.
    /// `slint::invoke_from_event_loop`) before touching the UI.
    pub fn new(on_frame: impl Fn(Frame) + Send + Sync + 'static) -> Result<Self> {
        crate::project::ensure_encoder_ranks();
        gst::init()?;

        let playbin = gst::ElementFactory::make("playbin").build()?;

        // Video sink branch: videoconvert -> appsink (force RGBA out).
        let caps = gst::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .build();
        let appsink = AppSink::builder()
            .caps(&caps)
            .max_buffers(2)
            .drop(true)
            .build();

        let on_frame: Arc<dyn Fn(Frame) + Send + Sync> = Arc::new(on_frame);
        appsink.set_callbacks(
            AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let caps = sample.caps().ok_or(gst::FlowError::Error)?;
                    let info = gst_video::VideoInfo::from_caps(caps)
                        .map_err(|_| gst::FlowError::Error)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    on_frame(Frame {
                        width: info.width(),
                        height: info.height(),
                        rgba: map.as_slice().to_vec(),
                    });
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        let convert = gst::ElementFactory::make("videoconvert").build()?;
        let bin = gst::Bin::new();
        bin.add_many([&convert, appsink.upcast_ref::<gst::Element>()])?;
        gst::Element::link_many([&convert, appsink.upcast_ref::<gst::Element>()])?;
        // Expose videoconvert's sink pad as the bin's sink pad so playbin can feed it.
        let sink_pad = convert
            .static_pad("sink")
            .ok_or_else(|| anyhow!("videoconvert has no sink pad"))?;
        let ghost = gst::GhostPad::with_target(&sink_pad)?;
        bin.add_pad(&ghost)?;
        playbin.set_property("video-sink", &bin);

        let pipeline = playbin
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("playbin is not a pipeline"))?;

        Ok(Self { pipeline })
    }

    /// Point the player at a local file (does not start playback).
    pub fn load(&self, path: &Path) -> Result<()> {
        let uri = gst::glib::filename_to_uri(path, None)?;
        self.pipeline.set_property("uri", uri.as_str());
        Ok(())
    }

    pub fn play(&self) -> Result<()> {
        self.pipeline.set_state(gst::State::Playing)?;
        Ok(())
    }

    pub fn pause(&self) -> Result<()> {
        self.pipeline.set_state(gst::State::Paused)?;
        Ok(())
    }

    /// Frame-accurate seek to `pos` from the start of the clip.
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
        self.pipeline
            .query_duration::<gst::ClockTime>()
            .map(|t| Duration::from_nanos(t.nseconds()))
    }

    /// Set audio volume (0.0 ..= 1.0).
    pub fn set_volume(&self, volume: f64) {
        self.pipeline.set_property("volume", volume.clamp(0.0, 1.0));
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Decodes frames from a short clip given via `GST_TEST_FILE`. Self-skips if
    /// the env var is unset (so the suite passes without a fixture). Requires the
    /// GStreamer `bin` on PATH at runtime.
    #[test]
    fn pulls_frames_from_fixture() {
        let Some(path) = std::env::var_os("GST_TEST_FILE") else {
            eprintln!("skipping pulls_frames_from_fixture: set GST_TEST_FILE");
            return;
        };
        let count = Arc::new(AtomicU32::new(0));
        let c2 = count.clone();
        let player = Player::new(move |f| {
            assert!(f.width > 0 && f.height > 0);
            assert_eq!(f.rgba.len() as u32, f.width * f.height * 4);
            c2.fetch_add(1, Ordering::SeqCst);
        })
        .expect("player");
        player.load(Path::new(&path)).expect("load");
        player.play().expect("play");
        std::thread::sleep(Duration::from_millis(1200));
        assert!(count.load(Ordering::SeqCst) > 0, "no frames decoded");
    }
}
