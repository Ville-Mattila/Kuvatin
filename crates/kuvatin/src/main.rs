#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod cli;
mod collect;
mod gui;
mod preview;
mod quickrun;
mod rendezvous;
mod sequence_render;
mod shell;

use clap::Parser;
use cli::{Cli, Mode};
use std::path::PathBuf;

/// Fold the processes Explorer launches for a multi-item selection (one per
/// item) into a single batch: the leader gets every path, followers get
/// `None` and should exit silently. `None` also when another leader already
/// claimed this process' paths.
fn coalesce(group: &str, paths: Vec<PathBuf>) -> Option<Vec<PathBuf>> {
    match rendezvous::gather(group, &paths, rendezvous::QUIET) {
        rendezvous::Role::Follower => None,
        rendezvous::Role::Leader(all) if all.is_empty() => None,
        rendezvous::Role::Leader(all) => Some(all),
    }
}

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

/// Show a headless run's per-file failures (capped at ten) and exit non-zero.
fn fail_and_exit(title: &str, intro: String, failures: &[(PathBuf, String)]) -> ! {
    let mut msg = intro;
    msg.push_str("\n\n");
    for (path, err) in failures.iter().take(10) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        msg.push_str(&format!("\u{2022} {name}: {err}\n"));
    }
    if failures.len() > 10 {
        msg.push_str(&format!("\u{2026}and {} more.\n", failures.len() - 10));
    }
    shell::notify_error(title, &msg);
    std::process::exit(1);
}

fn main() -> anyhow::Result<()> {
    configure_bundled_gstreamer();
    match Cli::parse().into_mode() {
        Mode::Register => shell::register()?,
        Mode::Unregister => shell::unregister()?,
        Mode::QuickRun { preset, paths } => {
            // One group per preset, so two different presets never merge.
            let Some(paths) = coalesce(&format!("preset:{preset}"), paths) else {
                return Ok(());
            };
            match quickrun::run(&preset, &paths) {
                Ok(report) if report.failure_count() > 0 => fail_and_exit(
                    "Kuvatin \u{2014} some files failed",
                    format!(
                        "{} of {} file(s) could not be processed:",
                        report.failure_count(),
                        report.total
                    ),
                    &report.failures,
                ),
                Ok(_) => {}
                Err(e) => {
                    // Windowed release build has no stderr, so the returned Err
                    // would be silent — surface it before propagating.
                    shell::notify_error("Kuvatin \u{2014} quick run failed", &e.to_string());
                    return Err(e);
                }
            }
        }
        Mode::SequenceMp4 { paths, fps } => {
            // Selecting several frames of one run launches one process per
            // frame; coalesced, they resolve to that single sequence.
            let Some(paths) = coalesce("sequence-mp4", paths) else {
                return Ok(());
            };
            match sequence_render::run(&paths, fps) {
                Ok(report) if !report.failures.is_empty() => fail_and_exit(
                    "Kuvatin \u{2014} sequence render",
                    format!(
                        "{} rendered, {} could not be rendered:",
                        report.rendered.len(),
                        report.failures.len()
                    ),
                    &report.failures,
                ),
                Ok(_) => {}
                Err(e) => {
                    shell::notify_error("Kuvatin \u{2014} sequence render failed", &e.to_string());
                    return Err(e);
                }
            }
        }
        Mode::Gui { paths } => {
            // "Open in Kuvatin…" on N items must open ONE window with all of
            // them, not N windows. A plain launch (no paths) skips the wait.
            let paths = if paths.is_empty() {
                paths
            } else {
                match coalesce("open", paths) {
                    Some(all) => all,
                    None => return Ok(()),
                }
            };
            gui::run(paths)?
        }
    }
    Ok(())
}
