//! The Slint GUI. `run()` builds the window, the per-mode state and the
//! timers, wires each mode's callbacks from its own module, and tears down
//! in order. Nothing else here knows about GStreamer or image codecs.
//!
//! - `image_mode`: the file list, viewer/crop editor and Convert.
//! - `presets`: the Settings controls ↔ `PresetStore` round trip.
//! - `video`: the GES project, its preview, and (in submodules) media
//!   import, the timeline editor and export.
//! - `win_drop`: the Win32 glue (drag-and-drop, frameless window controls).

mod image_mode;
mod presets;
mod updates;
mod video;
#[cfg(windows)]
mod win_drop;

use anyhow::{anyhow, Result};
use image_mode::ImageState;
use kuvatin_core::preset::PresetStore;
use presets::refresh_presets;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use video::export::ExportState;
use video::import::ImportState;
use video::VideoState;

slint::include_modules!();

/// Show the app's error dialog with `title` + `detail`. The one place failures
/// become visible — in the release windowed build there is no stderr.
fn show_error(ui: &AppWindow, title: &str, detail: impl AsRef<str>) {
    ui.set_error_title(title.into());
    ui.set_error_detail(detail.as_ref().into());
    ui.set_dialog_info(false);
    ui.set_dialog_reveal("".into());
    ui.set_error_visible(true);
}

/// The same dialog in its neutral, informational form (a batch summary): no
/// red, a check mark instead of the exclamation.
fn show_info(ui: &AppWindow, title: &str, detail: impl AsRef<str>) {
    ui.set_error_title(title.into());
    ui.set_error_detail(detail.as_ref().into());
    ui.set_dialog_info(true);
    ui.set_dialog_reveal("".into());
    ui.set_error_visible(true);
}

/// [`show_info`] plus a "Show in folder" button for the file the message is
/// about — the difference between being told something was written and being
/// able to go and look at it.
fn show_info_at(ui: &AppWindow, title: &str, detail: impl AsRef<str>, path: &Path) {
    show_info(ui, title, detail);
    ui.set_dialog_reveal(path.to_string_lossy().as_ref().into());
}

/// Open the file's folder with the file selected. Explorer wants the path
/// verbatim after `/select,` and quoted, and it does not accept a separate
/// argument; anything that fails here fails silently, since this is a
/// convenience on top of a message that already names the file.
fn reveal_in_explorer(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
    #[cfg(not(windows))]
    let _ = path;
}

/// Join file names for a dialog, capped so twenty failures don't overflow it
/// (the headless path uses the same cap).
fn name_list(names: &[String]) -> String {
    let mut s = names
        .iter()
        .take(10)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    if names.len() > 10 {
        s.push_str(&format!("\n\u{2026}and {} more", names.len() - 10));
    }
    s
}

pub fn run(initial_paths: Vec<PathBuf>) -> Result<()> {
    // Self-heal the per-user Explorer context-menu registration: the MSI only
    // registers for the installing user, so other accounts (or a moved exe)
    // pick it up here on first launch. Best-effort, never blocks startup.
    crate::shell::ensure_registered();

    // Sweep stale EXR→PNG sequence-conversion cache entries (best-effort;
    // entries untouched for a week go — a reuse re-stamps its entry).
    std::thread::spawn(|| {
        kuvatin_video::sweep_sequence_cache(
            kuvatin_video::CACHE_MAX_AGE,
            kuvatin_video::CACHE_MAX_BYTES,
        );
    });

    let store_path = PresetStore::default_path().ok_or_else(|| anyhow!("no config dir"))?;
    let store = Arc::new(Mutex::new(PresetStore::load_or_init(&store_path)?));

    let ui = AppWindow::new()?;
    // Every repeating timer lives here, owned by run(): a forgotten timer kept
    // its closure — and the Rc<RefCell<Option<Project>>> inside — alive past
    // the window, so the project was never dropped and GStreamer threads were
    // still running at process exit.
    let mut timers: Vec<slint::Timer> = Vec::new();

    // Initialize the preset-names model and the format/quality controls from the
    // first preset so they reflect (and can override) what will actually be applied.
    refresh_presets(&ui, &store.lock().unwrap(), 0);

    // If the preset file was corrupt/partial, load_or_init recovered to built-ins
    // and left a note — surface it instead of pretending nothing happened.
    if let Some(warning) = store.lock().unwrap().last_load_warning.clone() {
        show_error(&ui, "Presets reset", warning);
    }

    // Images mode: the file queue, per-file crops, thumbnails, the viewer.
    let image = ImageState::new(&ui, &initial_paths);
    presets::wire(&ui, &store, &store_path);
    // "Show in folder" on any dialog that names a file it just wrote.
    ui.on_reveal_path(|p| reveal_in_explorer(Path::new(p.as_str())));
    updates::wire(&ui);
    image_mode::wire(&ui, &image, &store);

    // Videos mode: the GES project + timeline models, the media import queue
    // (worker thread + drain timer) and the export state. Created before the
    // drag-and-drop drain below, which feeds media into the import queue.
    let video = VideoState::new(&ui);
    let import = ImportState::new();
    let export = ExportState::default();

    #[cfg(windows)]
    let (files, rows, crops, thumbs) = (&image.files, &image.rows, &image.crops, &image.thumbs);
    #[cfg(windows)]
    let import_q = &import.q;
    #[cfg(windows)]
    let add_paths = image_mode::add_paths;
    // Windows Explorer drag-and-drop: enable WM_DROPFILES on the native window
    // and drain dropped paths into the right pipeline (images → file list,
    // videos → timeline). The native HWND is only available after the window is
    // shown, so we wire it up from a single-shot timer once the loop is running.
    #[cfg(windows)]
    {
        let files = files.clone();
        let rows = rows.clone();
        let crops = crops.clone();
        let thumbs = thumbs.clone();
        let ui_weak = ui.as_weak();
        let setup_weak = ui.as_weak();
        let setup_timer = slint::Timer::default();
        setup_timer.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(100),
            move || {
                if let Some(ui) = setup_weak.upgrade() {
                    win_drop::enable(&ui);
                }
            },
        );
        // Keep the timer alive for the lifetime of the window.
        timers.push(setup_timer);

        // Drain dropped paths on the UI thread. Images go to the file list;
        // media is queued for the import worker (discovered off-thread, then
        // added by the import timer) so a big drop doesn't freeze the app.
        let import_q = import_q.clone();
        let drain_timer = slint::Timer::default();
        drain_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(150),
            move || {
                let dropped = win_drop::take_dropped();
                if dropped.is_empty() {
                    return;
                }
                let Some(ui) = ui_weak.upgrade() else {
                    return;
                };
                if ui.get_app_mode() == 1 {
                    // Same expansion + filtering as the image mode (folders,
                    // hidden files, junk), then the shared queue.
                    import_q.enqueue(dropped);
                } else {
                    // Ignore image drops while a batch is running — adding rows
                    // would desync the progress callback's snapshot indices.
                    if !ui.get_running() {
                        add_paths(dropped, &files, &rows, &crops, &thumbs, &ui_weak);
                    }
                }
            },
        );
        timers.push(drain_timer);
    }

    // Custom window-frame controls. On Windows these drive the native move/
    // min/max/close via the win_drop module (the HWND is captured in enable()).
    // On other platforms they are harmless no-ops so the .slint compiles and
    // runs cross-platform.
    {
        ui.on_win_minimize(|| {
            #[cfg(windows)]
            win_drop::minimize();
        });
        ui.on_win_maximize(|| {
            #[cfg(windows)]
            win_drop::maximize();
        });
        ui.on_win_close(|| {
            #[cfg(windows)]
            win_drop::close();
        });
    }

    video::wire(&ui, &video, &import, &export, &mut timers);

    ui.run()?;
    // Orderly teardown: stop the timers (their closures hold the project), then
    // drop the project explicitly so its pipeline reaches NULL before exit.
    drop(timers);
    video.project.borrow_mut().take();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_name_lists_are_capped() {
        let names: Vec<String> = (1..=12).map(|i| format!("f{i}.png")).collect();
        let s = name_list(&names);
        assert_eq!(s.lines().count(), 11);
        assert!(s.ends_with("\u{2026}and 2 more"));
        assert_eq!(name_list(&names[..3]), "f1.png\nf2.png\nf3.png");
    }
}
