#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod applog;
mod cli;
mod collect;
mod gui;
mod progress_ui;
mod quickrun;
mod rendezvous;
mod sequence_render;
mod settings;
mod shell;
mod update;

use clap::Parser;
use cli::{Cli, Mode};
use std::path::PathBuf;

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
    applog::log(&format!(
        "FAIL {title}: {} failure(s): {}",
        failures.len(),
        msg.replace('\n', " | ")
    ));
    shell::notify_error(title, &msg);
    std::process::exit(1);
}

/// Report a fatal error the way the current launch can show it (stderr with a
/// console, a dialog from Explorer, nothing under `--quiet`) and exit 1.
/// Every fatal path goes through here: a `?` out of `main` printed only to
/// stderr, which the windowed release build doesn't have — a failed
/// `--register`, a progress window that couldn't open, or a GUI that couldn't
/// start were all silent exits.
fn fail(title: &str, err: anyhow::Error) -> ! {
    applog::log(&format!("FAIL {title}: {err:#}"));
    shell::notify_error(title, &format!("{err:#}"));
    std::process::exit(1);
}

fn or_fail<T>(title: &str, result: anyhow::Result<T>) -> T {
    match result {
        Ok(v) => v,
        Err(e) => fail(title, e),
    }
}

/// Run a headless job under the progress window — or with no window under
/// `--quiet`, where nobody is watching and a window may not be creatable.
fn run_job<T: Send + 'static>(
    quiet: bool,
    heading: &str,
    work: impl FnOnce(&progress_ui::ProgressSink) -> T + Send + 'static,
) -> T {
    let outcome = if quiet {
        progress_ui::run_headless(work)
    } else {
        progress_ui::run_with_progress(heading, work)
    };
    or_fail("Kuvatin \u{2014} could not start", outcome)
}

fn main() {
    // A windowed exe run from a terminal joins that terminal's console, so
    // `--register` / errors print where the user is looking.
    shell::attach_parent_console();
    applog::install_panic_hook();
    configure_bundled_gstreamer();
    let args: Vec<std::ffi::OsString> = std::env::args_os()
        .map(|a| cli::repair_drive_root(&a))
        .collect();
    let cli = Cli::parse_from(args);
    let quiet = cli.quiet;
    if quiet {
        shell::set_quiet(true);
    }
    match cli.into_mode() {
        Mode::Register => {
            applog::log(&format!("register context menu ({})", applog::context()));
            or_fail(
                "Kuvatin \u{2014} could not register the context menu",
                shell::register(),
            );
            applog::log("register done");
        }
        Mode::Unregister => {
            applog::log(&format!("unregister context menu ({})", applog::context()));
            or_fail(
                "Kuvatin \u{2014} could not remove the context menu",
                shell::unregister(),
            );
            applog::log("unregister done");
        }
        Mode::Invalid(reason) => fail("Kuvatin", anyhow::anyhow!("{reason}")),
        Mode::PrintExtensions => {
            for ext in shell::menu_extensions() {
                println!(".{ext}");
            }
        }
        Mode::QuickRun { preset, paths } => {
            // One group per preset, so two different presets never merge.
            let Some(paths) = coalesce(&format!("preset:{preset}"), paths) else {
                return;
            };
            applog::log(&format!(
                "quick run: preset {preset:?}, {} input(s), quiet={quiet}",
                paths.len()
            ));
            let heading = preset.clone();
            let outcome = run_job(quiet, &heading, move |sink| {
                let store = quickrun::load_store()?;
                quickrun::run(&store, &preset, &paths, &|f, s| sink.set(f, s), &|| {
                    sink.cancelled()
                })
            });
            match outcome {
                // The user cancelled — no dialog, even if some files had
                // already failed before that.
                Ok(report) if report.cancelled > 0 => {}
                Ok(report) if report.failure_count() > 0 => fail_and_exit(
                    "Kuvatin \u{2014} some files failed",
                    format!(
                        "{} of {} file(s) could not be processed:",
                        report.failure_count(),
                        report.total
                    ),
                    &report.failures,
                ),
                Ok(report) => applog::log(&format!(
                    "quick run done: {} file(s) converted",
                    report.total - report.failure_count() - report.cancelled
                )),
                Err(e) => fail("Kuvatin \u{2014} quick run failed", e),
            }
        }
        Mode::SequenceMp4 { paths, fps } => {
            // Selecting several frames of one run launches one process per
            // frame; coalesced, they resolve to that single sequence.
            let Some(paths) = coalesce("sequence-mp4", paths) else {
                return;
            };
            applog::log(&format!(
                "sequence render: {} path(s), {fps} fps, quiet={quiet}",
                paths.len()
            ));
            let outcome = run_job(quiet, "Render image sequence to MP4", move |sink| {
                sequence_render::run(&paths, fps, &|f, s| sink.set(f, s), sink.cancel_flag())
            });
            // The EXR→PNG cache must be swept from this path too — a
            // right-click-only user never starts the GUI, whose startup sweep
            // used to be the only one.
            kuvatin_video::sweep_sequence_cache(
                kuvatin_video::CACHE_MAX_AGE,
                kuvatin_video::CACHE_MAX_BYTES,
            );
            match outcome {
                // A cancelled run is the user's choice — no dialog.
                Ok(report) if report.cancelled => {}
                Ok(report) if !report.failures.is_empty() => fail_and_exit(
                    "Kuvatin \u{2014} sequence render",
                    format!(
                        "{} rendered, {} could not be rendered:",
                        report.rendered.len(),
                        report.failures.len()
                    ),
                    &report.failures,
                ),
                Ok(report) => applog::log(&format!(
                    "sequence render done: {} MP4(s) written",
                    report.rendered.len()
                )),
                Err(e) => fail("Kuvatin \u{2014} sequence render failed", e),
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
                    None => return,
                }
            };
            or_fail("Kuvatin \u{2014} could not start", gui::run(paths))
        }
    }
}
