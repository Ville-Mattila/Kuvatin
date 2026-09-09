//! GStreamer-backed video engine for Kuvatin. The GUI talks only to `Project`
//! (GES editing timeline + composited preview + render); all GStreamer details
//! stay inside this crate.

/// One decoded RGBA video frame handed to the GUI (`width * height * 4` bytes).
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub mod project;
pub mod sequence;
pub use project::{
    normalize_render_size, thumbnail, thumbnail_uri, warm_asset, warm_asset_uri, ClipGeom, ClipId,
    ClipInfo, ExportSettings, Layout, Project, RenderStatus, VideoCodec, CANVAS_H, CANVAS_W,
};
pub use sequence::{
    convert_exr_sequence, detect_sequence, is_frame_file, parse_frame_path, render_to_mp4,
    sweep_sequence_cache, Cancelled, RenderProgress, SequenceSpec, CACHE_MAX_AGE, CACHE_MAX_BYTES,
    FRAME_EXTENSIONS, VIDEO_EXTENSIONS,
};
