//! Single-clip player. The real playbin + appsink implementation lands in
//! Task 2; this skeleton compiles and links GStreamer so the build env is
//! validated end to end.

/// One decoded RGBA video frame handed to the GUI (`width * height * 4` bytes).
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Owns the GStreamer pipeline. Placeholder API until Task 2.
pub struct Player;

impl Player {
    /// Initialize GStreamer. (References the GStreamer libs so the linker pulls
    /// them in — proving the SDK is wired up correctly.)
    pub fn new() -> anyhow::Result<Self> {
        gstreamer::init()?;
        Ok(Player)
    }
}
