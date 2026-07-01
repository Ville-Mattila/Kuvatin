#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod cli;
mod collect;
mod gui;
mod preview;
mod quickrun;
mod shell;

use clap::Parser;
use cli::{Cli, Mode};

/// In an installed build the GStreamer **plugins** are bundled next to the exe
/// (the core DLLs sit alongside the exe so the loader finds them at startup;
/// plugins load later, at `gst::init`, via this path). In a dev build the
/// directory is absent and GStreamer uses the system install on PATH. Must run
/// before any GStreamer init.
fn configure_bundled_gstreamer() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent() else {
        return;
    };
    let plugins = dir.join("gstreamer-plugins");
    if plugins.is_dir() {
        std::env::set_var("GST_PLUGIN_PATH", &plugins);
        // Don't also scan a differently-versioned system GStreamer.
        std::env::set_var("GST_PLUGIN_SYSTEM_PATH", "");
    }
    // H.264 export encoder ranks (must be set before any gst init). Prefer NVENC
    // auto-GPU mode (fast, works with GES output); most other hardware encoders
    // fail to init, so x264enc (software) sits above them as the fallback.
    if std::env::var_os("GST_PLUGIN_FEATURE_RANK").is_none() {
        std::env::set_var("GST_PLUGIN_FEATURE_RANK", "nvautogpuh264enc:512,x264enc:300");
    }
}

fn main() -> anyhow::Result<()> {
    configure_bundled_gstreamer();
    let mode = Cli::parse().into_mode();
    match mode {
        Mode::Register => shell::register()?,
        Mode::Unregister => shell::unregister()?,
        Mode::QuickRun { preset, paths } => {
            let failures = quickrun::run(&preset, &paths)?;
            if failures > 0 {
                std::process::exit(1);
            }
        }
        Mode::Gui { paths } => gui::run(paths)?,
    }
    Ok(())
}
