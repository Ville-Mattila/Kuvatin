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
    // Export encoder ranks (NVENC first, x264 fallback, mfaacenc disabled) are
    // applied programmatically right after every gst::init() — see
    // kuvatin-video's ensure_encoder_ranks(). The old GST_PLUGIN_FEATURE_RANK
    // env-var approach silently deactivated whenever the user's environment
    // already set that variable.
}

fn main() -> anyhow::Result<()> {
    configure_bundled_gstreamer();
    match Cli::parse().into_mode() {
        Mode::Register => shell::register()?,
        Mode::Unregister => shell::unregister()?,
        Mode::QuickRun { preset, paths } => {
            match quickrun::run(&preset, &paths) {
                Ok(report) if report.failure_count() > 0 => {
                    let mut msg = format!(
                        "{} of {} file(s) could not be processed:\n\n",
                        report.failure_count(),
                        report.total
                    );
                    for (path, err) in report.failures.iter().take(10) {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string());
                        msg.push_str(&format!("\u{2022} {name}: {err}\n"));
                    }
                    if report.failures.len() > 10 {
                        msg.push_str(&format!("\u{2026}and {} more.\n", report.failures.len() - 10));
                    }
                    shell::notify_error("Kuvatin \u{2014} some files failed", &msg);
                    std::process::exit(1);
                }
                Ok(_) => {}
                Err(e) => {
                    // Windowed release build has no stderr, so the returned Err
                    // would be silent — surface it before propagating.
                    shell::notify_error("Kuvatin \u{2014} quick run failed", &e.to_string());
                    return Err(e);
                }
            }
        }
        Mode::Gui { paths } => gui::run(paths)?,
    }
    Ok(())
}
