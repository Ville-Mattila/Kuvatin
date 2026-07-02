//! GStreamer-backed video playback for Kuvatin. The GUI talks only to `Player`;
//! all GStreamer details stay inside this crate.

pub mod player;
pub use player::{Frame, Player};

pub mod project;
pub use project::{
    thumbnail, warm_asset, ClipGeom, ClipId, ClipInfo, ExportSettings, Layout, Project,
    RenderStatus, VideoCodec, CANVAS_H, CANVAS_W,
};
