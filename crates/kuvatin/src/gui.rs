use crate::collect::collect_images;
use anyhow::{anyhow, Result};
use kuvatin_core::batch::run_jobs;
use kuvatin_core::crop::CropMode;
use kuvatin_core::format::OutputFormat;
use kuvatin_core::pipeline::{Job, PngOptimize};
use kuvatin_core::preset::PresetStore;
use slint::{Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

slint::include_modules!();

pub fn run(initial_paths: Vec<PathBuf>) -> Result<()> {
    let store_path = PresetStore::default_path().ok_or_else(|| anyhow!("no config dir"))?;
    let store = Arc::new(Mutex::new(PresetStore::load_or_init(&store_path)?));

    let ui = AppWindow::new()?;

    // Initialize the preset-names model and the format/quality controls from the
    // first preset so they reflect (and can override) what will actually be applied.
    refresh_presets(&ui, &store.lock().unwrap(), 0);

    let files: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(collect_images(&initial_paths)));
    let rows = Rc::new(VecModel::from(rows_from(&files.lock().unwrap())));
    ui.set_files(ModelRc::from(rows.clone()));
    spawn_thumbnails(ui.as_weak(), files.clone(), files.lock().unwrap().clone());

    // Per-file crops in ABSOLUTE pixels (x, y, w, h) keyed by input path. Files
    // not present here are converted with the base job (no crop override).
    type CropMap = HashMap<PathBuf, (u32, u32, u32, u32)>;
    let crops: Arc<Mutex<CropMap>> = Arc::new(Mutex::new(HashMap::new()));
    // The in-progress crop edit: the file being cropped and its ORIGINAL (w, h).
    let edit: Arc<Mutex<Option<(PathBuf, u32, u32)>>> = Arc::new(Mutex::new(None));

    // Selecting a preset syncs the format/quality controls to that preset's job.
    {
        let store = store.clone();
        let ui_weak = ui.as_weak();
        ui.on_preset_changed(move |idx| {
            let store = store.lock().unwrap();
            if let (Some(ui), Some(p)) = (ui_weak.upgrade(), store.presets.get(idx as usize)) {
                ui.set_format(format_combo_str(p.job.format).into());
                ui.set_quality(p.job.quality as i32);
                ui.set_png_mode(png_mode_to_idx(p.job.png));
                ui.set_preset_name(p.name.clone().into());
            }
        });
    }

    {
        let files = files.clone();
        let rows = rows.clone();
        let ui_weak = ui.as_weak();
        ui.on_add_files(move || {
            if let Some(picked) = rfd::FileDialog::new()
                .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp", "tiff", "gif"])
                .pick_files()
            {
                add_paths(picked, &files, &rows, &ui_weak);
            }
        });
    }

    // Windows Explorer drag-and-drop: enable WM_DROPFILES on the native window
    // and drain dropped paths into the same add-files pipeline. The native HWND
    // is only available after the window is shown, so we wire it up from a
    // single-shot timer that fires once the event loop is running.
    #[cfg(windows)]
    {
        let files = files.clone();
        let rows = rows.clone();
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
        std::mem::forget(setup_timer);

        // Drain dropped paths on the UI thread and feed them into add_paths.
        let drain_timer = slint::Timer::default();
        drain_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(150),
            move || {
                let dropped = win_drop::take_dropped();
                if !dropped.is_empty() {
                    add_paths(dropped, &files, &rows, &ui_weak);
                }
            },
        );
        std::mem::forget(drain_timer);
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
        ui.on_win_drag(|| {
            #[cfg(windows)]
            win_drop::drag();
        });
    }

    {
        let files = files.clone();
        let rows = rows.clone();
        let crops = crops.clone();
        ui.on_clear_files(move || {
            let mut guard = files.lock().unwrap();
            guard.clear();
            crops.lock().unwrap().clear();
            refresh(&rows, &guard);
        });
    }

    // Save (upsert) the current settings as a named preset.
    {
        let store = store.clone();
        let store_path = store_path.clone();
        let ui_weak = ui.as_weak();
        ui.on_save_preset(move |name| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let mut store = store.lock().unwrap();
            // Trim the requested name; fall back to the selected preset's name if
            // the field is empty so a bare "Save" still overwrites the current one.
            let mut name = name.trim().to_string();
            if name.is_empty() {
                let idx = ui.get_current_preset() as usize;
                match store.presets.get(idx) {
                    Some(p) => name = p.name.clone(),
                    None => return,
                }
            }

            let job = current_job(&ui, &store);

            // Upsert: overwrite an existing preset's job, or push a new one.
            if let Some(existing) = store.presets.iter_mut().find(|p| p.name == name) {
                existing.job = job;
            } else {
                store.presets.push(kuvatin_core::preset::Preset {
                    name: name.clone(),
                    job,
                });
            }

            if let Err(e) = store.save(&store_path) {
                eprintln!("failed to save presets: {e}");
            }

            let idx = store
                .presets
                .iter()
                .position(|p| p.name == name)
                .unwrap_or(0);
            refresh_presets(&ui, &store, idx);
            ui.set_preset_name(name.into());
        });
    }

    // Delete the currently selected preset (keeping at least one).
    {
        let store = store.clone();
        let store_path = store_path.clone();
        let ui_weak = ui.as_weak();
        ui.on_delete_preset(move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let mut store = store.lock().unwrap();
            // Keep at least one preset: deleting the last one would leave the app
            // with no selectable preset and no usable job.
            if store.presets.len() <= 1 {
                return;
            }
            let idx = (ui.get_current_preset() as usize).min(store.presets.len() - 1);
            store.presets.remove(idx);

            if let Err(e) = store.save(&store_path) {
                eprintln!("failed to save presets: {e}");
            }

            let select = idx.min(store.presets.len() - 1);
            refresh_presets(&ui, &store, select);
            if let Some(p) = store.presets.get(select) {
                ui.set_preset_name(p.name.clone().into());
            }
        });
    }

    // Open the crop editor for a queued file.
    {
        let files = files.clone();
        let crops = crops.clone();
        let edit = edit.clone();
        let ui_weak = ui.as_weak();
        ui.on_start_crop(move |index| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let path = match files.lock().unwrap().get(index as usize) {
                Some(p) => p.clone(),
                None => return,
            };
            let Ok(img) = image::open(&path) else {
                return;
            };
            let (ow, oh) = (img.width(), img.height());
            if ow == 0 || oh == 0 {
                return;
            }

            // Downscale the decoded image for display; normalized coords keep the
            // crop math independent of the preview size.
            let preview = img.thumbnail(900, 620).to_rgba8();
            let (pw, ph) = (preview.width(), preview.height());
            let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(preview.as_raw(), pw, ph);
            ui.set_crop_image(Image::from_rgba8(buf));

            // Preview BOX size that matches the original aspect within a max area.
            let (max_w, max_h) = (560.0_f32, 420.0_f32);
            let scale = (max_w / ow as f32).min(max_h / oh as f32).min(1.0);
            ui.set_crop_box_w((ow as f32 * scale).max(1.0));
            ui.set_crop_box_h((oh as f32 * scale).max(1.0));

            // Expose the original dimensions so the numeric X/Y/W/H crop fields
            // can show and edit absolute pixels.
            ui.set_crop_img_w(ow as i32);
            ui.set_crop_img_h(oh as i32);

            // Initialize the rect from any existing crop (normalized back to 0..1),
            // else the full image.
            if let Some(&(x, y, w, h)) = crops.lock().unwrap().get(&path) {
                ui.set_crop_x(x as f32 / ow as f32);
                ui.set_crop_y(y as f32 / oh as f32);
                ui.set_crop_w(w as f32 / ow as f32);
                ui.set_crop_h(h as f32 / oh as f32);
            } else {
                ui.set_crop_x(0.0);
                ui.set_crop_y(0.0);
                ui.set_crop_w(1.0);
                ui.set_crop_h(1.0);
            }

            *edit.lock().unwrap() = Some((path, ow, oh));
            ui.set_cropping(true);
        });
    }

    // Apply the current crop rectangle: normalized → absolute pixels.
    {
        let crops = crops.clone();
        let edit = edit.clone();
        let files = files.clone();
        let ui_weak = ui.as_weak();
        ui.on_apply_crop(move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let Some((path, ow, oh)) = edit.lock().unwrap().clone() else {
                ui.set_cropping(false);
                return;
            };
            let cx = ui.get_crop_x().clamp(0.0, 1.0);
            let cy = ui.get_crop_y().clamp(0.0, 1.0);
            let cw = ui.get_crop_w().clamp(0.0, 1.0);
            let ch = ui.get_crop_h().clamp(0.0, 1.0);

            let mut x = (cx * ow as f32).round() as u32;
            let mut y = (cy * oh as f32).round() as u32;
            let mut w = (cw * ow as f32).round().max(1.0) as u32;
            let mut h = (ch * oh as f32).round().max(1.0) as u32;
            // Clamp so the rect stays inside the image.
            x = x.min(ow.saturating_sub(1));
            y = y.min(oh.saturating_sub(1));
            w = w.min(ow - x).max(1);
            h = h.min(oh - y).max(1);

            crops.lock().unwrap().insert(path.clone(), (x, y, w, h));

            // Mark the row (path-matched via the files list) as cropped.
            if let Some(i) = files.lock().unwrap().iter().position(|p| *p == path) {
                let model = ui.get_files();
                if let Some(mut row) = model.row_data(i) {
                    row.status = "cropped".into();
                    model.set_row_data(i, row);
                }
            }

            ui.set_cropping(false);
        });
    }

    // Cancel the crop edit: discard, keep any previously applied crop.
    {
        let ui_weak = ui.as_weak();
        ui.on_cancel_crop(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_cropping(false);
            }
        });
    }

    // Clear the crop for the file currently being edited.
    {
        let crops = crops.clone();
        let edit = edit.clone();
        let files = files.clone();
        let ui_weak = ui.as_weak();
        ui.on_clear_crop(move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            if let Some((path, _, _)) = edit.lock().unwrap().clone() {
                crops.lock().unwrap().remove(&path);
                // Reset the row status back to queued.
                if let Some(i) = files.lock().unwrap().iter().position(|p| *p == path) {
                    let model = ui.get_files();
                    if let Some(mut row) = model.row_data(i) {
                        if row.status == "cropped" {
                            row.status = "queued".into();
                            model.set_row_data(i, row);
                        }
                    }
                }
            }
            ui.set_cropping(false);
        });
    }

    {
        let files = files.clone();
        let store = store.clone();
        let crops = crops.clone();
        let ui_weak = ui.as_weak();
        ui.on_convert(move || {
            let inputs = files.lock().unwrap().clone();
            if inputs.is_empty() {
                return;
            }
            let ui = match ui_weak.upgrade() {
                Some(u) => u,
                None => return,
            };
            let preset_idx = ui.get_current_preset() as usize;
            let preset = match store.lock().unwrap().presets.get(preset_idx) {
                Some(p) => p.clone(),
                None => return,
            };
            // Apply the live format/quality controls on top of the preset's job.
            let mut job = preset.job.clone();
            job.format = format_combo_to_format(&ui.get_format());
            job.quality = ui.get_quality().clamp(0, 100) as u8;
            job.png = png_mode_from(ui.get_png_mode());

            // Explicit output-resolution override from the Settings fields. When
            // either dimension is set (> 0) it replaces the preset's resize for
            // this run; a 0 dimension is left unconstrained. With both at 0 the
            // preset's resize is used unchanged. The core pipeline crops first
            // and resizes second, so a per-file crop + this resolution combine
            // correctly (crop the region, then scale it to the resolution).
            let rw = ui.get_res_w().max(0) as u32;
            let rh = ui.get_res_h().max(0) as u32;
            if rw > 0 || rh > 0 {
                job.resize = kuvatin_core::resize::ResizeMode::Pixels {
                    width: if rw > 0 { Some(rw) } else { None },
                    height: if rh > 0 { Some(rh) } else { None },
                    keep_aspect: ui.get_res_lock(),
                };
            }

            let preset_name = preset.name.clone();

            // Build a per-file job list: files with a stored crop get a
            // CropMode::Rect override; the rest use the base job unchanged. Clone
            // the crop data out now so we don't hold the lock across the thread.
            let crop_map = crops.lock().unwrap().clone();
            let items: Vec<(PathBuf, Job)> = inputs
                .iter()
                .map(|p| {
                    let mut j = job.clone();
                    if let Some(&(x, y, width, height)) = crop_map.get(p) {
                        j.crop = CropMode::Rect { x, y, width, height };
                    }
                    (p.clone(), j)
                })
                .collect();

            ui.set_running(true);
            ui.set_progress(0.0);

            let ui_weak2 = ui_weak.clone();
            let total = inputs.len();
            std::thread::spawn(move || {
                let ui_for_progress = ui_weak2.clone();
                let rows_paths = inputs.clone();
                run_jobs(&items, &preset_name, move |p| {
                    let frac = p.done as f32 / total as f32;
                    let idx = rows_paths.iter().position(|x| *x == p.last.input);
                    let ok = p.last.outcome.is_ok();
                    let ui3 = ui_for_progress.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui3.upgrade() {
                            ui.set_progress(frac);
                            if let Some(i) = idx {
                                let model = ui.get_files();
                                if let Some(mut row) = model.row_data(i) {
                                    row.status = if ok { "done".into() } else { "error".into() };
                                    model.set_row_data(i, row);
                                }
                            }
                        }
                    });
                });
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak2.upgrade() {
                        ui.set_running(false);
                        ui.set_progress(1.0);
                    }
                });
            });
        });
    }

    ui.run()?;
    Ok(())
}

/// (Re)build the preset-names model, select `select` (clamped to a valid index),
/// and sync the format/quality controls to the selected preset's job.
fn refresh_presets(ui: &AppWindow, store: &PresetStore, select: usize) {
    let names: Vec<SharedString> = store.presets.iter().map(|p| p.name.clone().into()).collect();
    ui.set_preset_names(ModelRc::new(VecModel::from(names)));
    let idx = select.min(store.presets.len().saturating_sub(1));
    ui.set_current_preset(idx as i32);
    if let Some(p) = store.presets.get(idx) {
        ui.set_format(format_combo_str(p.job.format).into());
        ui.set_quality(p.job.quality as i32);
        ui.set_png_mode(png_mode_to_idx(p.job.png));
    }
}

/// Build the job described by the live UI: start from the selected preset's job
/// (to preserve resize/crop/output), then override format + quality from the
/// controls — the same recipe `on_convert` applies before running a batch.
fn current_job(ui: &AppWindow, store: &PresetStore) -> Job {
    let idx = ui.get_current_preset() as usize;
    let mut job = store
        .presets
        .get(idx)
        .map(|p| p.job.clone())
        .unwrap_or_default();
    job.format = format_combo_to_format(&ui.get_format());
    job.quality = ui.get_quality().clamp(0, 100) as u8;
    job.png = png_mode_from(ui.get_png_mode());
    job
}

/// Map the PNG-optimization combo index to the core enum.
fn png_mode_from(idx: i32) -> PngOptimize {
    match idx {
        1 => PngOptimize::Lossless,
        2 => PngOptimize::Lossy,
        _ => PngOptimize::None,
    }
}

/// Map the core PNG-optimization enum back to its combo index.
fn png_mode_to_idx(mode: PngOptimize) -> i32 {
    match mode {
        PngOptimize::None => 0,
        PngOptimize::Lossless => 1,
        PngOptimize::Lossy => 2,
    }
}

fn rows_from(paths: &[PathBuf]) -> Vec<FileRow> {
    paths
        .iter()
        .map(|p| FileRow {
            name: p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
                .into(),
            status: "queued".into(),
            thumb: Default::default(),
            dims: "".into(),
        })
        .collect()
}

/// Decode thumbnails for the current `files` on a background thread and post
/// each one back to the matching row on the UI thread. The decoded thumbnail is
/// matched by full path against the *current* `files` snapshot when posting, so
/// an add/clear that happens mid-decode can't write a thumbnail onto the wrong
/// row — a path that is no longer present is simply dropped. The same path-based
/// lookup the convert progress callback uses keeps model indices honest.
fn spawn_thumbnails(
    ui_weak: slint::Weak<AppWindow>,
    files: Arc<Mutex<Vec<PathBuf>>>,
    paths: Vec<PathBuf>,
) {
    std::thread::spawn(move || {
        for path in paths {
            let Ok(img) = image::open(&path) else {
                continue;
            };
            let (ow, oh) = (img.width(), img.height());
            let thumb = img.thumbnail(40, 40).to_rgba8();
            let (tw, th) = (thumb.width(), thumb.height());
            // `SharedPixelBuffer` is `Send`; `slint::Image` is not, so we ship the
            // buffer across the event-loop boundary and build the `Image` on the
            // UI thread.
            let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(thumb.as_raw(), tw, th);
            let dims: SharedString = format!("{ow}×{oh}").into();

            let ui_weak = ui_weak.clone();
            let files = files.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_weak.upgrade() {
                    // Resolve this path to its current row index. If the list
                    // changed and the path is gone, skip silently.
                    let idx = files.lock().unwrap().iter().position(|p| *p == path);
                    if let Some(i) = idx {
                        let model = ui.get_files();
                        if let Some(mut row) = model.row_data(i) {
                            row.thumb = Image::from_rgba8(buf);
                            row.dims = dims;
                            model.set_row_data(i, row);
                        }
                    }
                }
            });
        }
    });
}

/// The combo-box string for a format (matches the model in app.slint).
fn format_combo_str(format: OutputFormat) -> &'static str {
    match format {
        OutputFormat::Png => "png",
        OutputFormat::Jpeg => "jpeg",
        OutputFormat::Webp => "webp",
        OutputFormat::Bmp => "bmp",
        OutputFormat::Tiff => "tiff",
        OutputFormat::Gif => "gif",
    }
}

/// Parse a combo-box string back into a format; unknown values fall back to PNG.
fn format_combo_to_format(s: &str) -> OutputFormat {
    match s {
        "jpeg" => OutputFormat::Jpeg,
        "webp" => OutputFormat::Webp,
        "bmp" => OutputFormat::Bmp,
        "tiff" => OutputFormat::Tiff,
        "gif" => OutputFormat::Gif,
        _ => OutputFormat::Png,
    }
}

fn refresh(rows: &Rc<VecModel<FileRow>>, paths: &[PathBuf]) {
    let new = rows_from(paths);
    while rows.row_count() > 0 {
        rows.remove(0);
    }
    for r in new {
        rows.push(r);
    }
}

/// Add `picked` paths to the queue: filter/expand to image files, merge with the
/// existing set (sorted + deduped), refresh the visible rows, and kick off
/// thumbnail decoding. Shared by the Add files… button and the drag-and-drop
/// drain timer so both paths behave identically.
fn add_paths(
    picked: Vec<PathBuf>,
    files: &Arc<Mutex<Vec<PathBuf>>>,
    rows: &Rc<VecModel<FileRow>>,
    ui_weak: &slint::Weak<AppWindow>,
) {
    let mut guard = files.lock().unwrap();
    guard.extend(collect_images(&picked));
    guard.sort();
    guard.dedup();
    refresh(rows, &guard);
    spawn_thumbnails(ui_weak.clone(), files.clone(), guard.clone());
}

/// Native Windows Explorer drag-and-drop support via `WM_DROPFILES`.
///
/// Slint 1.16 does not expose OS file-drop events, so we obtain the window's
/// `HWND` (through the `raw-window-handle-06` slint feature), call
/// `DragAcceptFiles`, and subclass the window proc to intercept `WM_DROPFILES`.
/// Dropped paths are pushed into a process-global inbox that the UI thread
/// drains on a repeating timer — this avoids passing Rust closures through the
/// C callback boundary.
#[cfg(windows)]
mod win_drop {
    use super::AppWindow;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::ComponentHandle;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows::Win32::UI::Shell::{
        DefSubclassProc, DragAcceptFiles, DragFinish, DragQueryFileW, SetWindowSubclass, HDROP,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetWindowRect, IsZoomed, PostMessageW, SendMessageW, ShowWindow, HTBOTTOM,
        HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT,
        HTTOPRIGHT, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, WM_CLOSE, WM_DROPFILES, WM_NCLBUTTONDOWN,
    };
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };
    use windows::Win32::UI::HiDpi::GetDpiForWindow;

    /// Width of the invisible edge zone (in physical px) used for resize hit-testing.
    const RESIZE_BORDER: i32 = 6;

    /// Inbox of paths dropped onto the window, awaiting drain by the UI thread.
    static INBOX: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();
    /// Guards against subclassing the window more than once.
    static INSTALLED: OnceLock<()> = OnceLock::new();
    /// The native window handle, captured in `enable()` so the win-* callbacks
    /// can reach it without re-deriving it from the Slint window each time.
    static HWND_RAW: OnceLock<isize> = OnceLock::new();

    fn inbox() -> &'static Mutex<Vec<PathBuf>> {
        INBOX.get_or_init(|| Mutex::new(Vec::new()))
    }

    /// The captured HWND, if `enable()` has run.
    fn hwnd() -> Option<HWND> {
        HWND_RAW
            .get()
            .map(|raw| HWND(*raw as *mut std::ffi::c_void))
    }

    /// Begin the native window move loop. Wired to the title-bar drag region:
    /// release the implicit mouse capture, then tell Windows the user grabbed the
    /// "caption" so it runs its own move loop (including edge snapping).
    pub fn drag() {
        if let Some(hwnd) = hwnd() {
            // SAFETY: hwnd is valid and we run on the UI thread that owns it.
            unsafe {
                let _ = ReleaseCapture();
                SendMessageW(
                    hwnd,
                    WM_NCLBUTTONDOWN,
                    WPARAM(HTCAPTION as usize),
                    LPARAM(0),
                );
            }
        }
    }

    /// Minimize the window.
    pub fn minimize() {
        if let Some(hwnd) = hwnd() {
            unsafe {
                let _ = ShowWindow(hwnd, SW_MINIMIZE);
            }
        }
    }

    /// Toggle maximize/restore.
    pub fn maximize() {
        if let Some(hwnd) = hwnd() {
            unsafe {
                if IsZoomed(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                } else {
                    let _ = ShowWindow(hwnd, SW_MAXIMIZE);
                }
            }
        }
    }

    /// Request a clean close (lets Slint tear down via the normal WM_CLOSE path).
    pub fn close() {
        if let Some(hwnd) = hwnd() {
            unsafe {
                let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }

    /// Drain all queued dropped paths. Called by the UI-thread timer.
    pub fn take_dropped() -> Vec<PathBuf> {
        let mut guard = inbox().lock().unwrap();
        std::mem::take(&mut *guard)
    }

    /// Enable Explorer drag-and-drop on the given window. Idempotent: only the
    /// first call installs the subclass. Must run after the window is shown so
    /// the native HWND exists.
    pub fn enable(ui: &AppWindow) {
        if INSTALLED.get().is_some() {
            return;
        }
        let Some(hwnd) = hwnd_of(ui) else {
            return;
        };
        // Stash the raw handle so the win-* callbacks can use it later.
        let _ = HWND_RAW.set(hwnd.0 as isize);
        // Windows 11: round the frameless window's outer corners via DWM.
        unsafe {
            let pref = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const core::ffi::c_void,
                std::mem::size_of_val(&pref) as u32,
            );
        }
        // SAFETY: hwnd is a valid window handle obtained from the shown window,
        // and we run on the UI/event-loop thread that owns it.
        unsafe {
            DragAcceptFiles(hwnd, true);
            // Subclass id 1, no per-instance refdata (we use a global inbox).
            if SetWindowSubclass(hwnd, Some(subclass_proc), 1, 0).as_bool() {
                let _ = INSTALLED.set(());
            }
        }
    }

    /// Extract the Win32 HWND from a shown Slint window.
    fn hwnd_of(ui: &AppWindow) -> Option<HWND> {
        let handle = ui.window().window_handle();
        match handle.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(h) => Some(HWND(isize::from(h.hwnd) as *mut std::ffi::c_void)),
            _ => None,
        }
    }

    /// Window subclass proc. Runs on the UI thread (same thread as the Slint
    /// event loop). On `WM_DROPFILES` it reads the dropped paths and queues them
    /// in the inbox; everything else is forwarded to the default chain.
    unsafe extern "system" fn subclass_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _uid: usize,
        _refdata: usize,
    ) -> LRESULT {
        use windows::Win32::UI::WindowsAndMessaging::WM_NCHITTEST;
        if msg == WM_NCHITTEST {
            // The frameless window has no native border, so we synthesize resize
            // grips: if the cursor is within RESIZE_BORDER px of an edge, return
            // the matching hit code so Windows runs its native resize loop.
            // The lparam packs screen coords as signed 16-bit lo/hi words;
            // GetCursorPos avoids sign/monitor pitfalls and gives the same point.
            let mut pt = POINT { x: 0, y: 0 };
            if GetCursorPos(&mut pt).is_err() {
                // Fall back to the lparam-packed coords.
                pt.x = (lparam.0 & 0xFFFF) as i16 as i32;
                pt.y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            }
            let mut rc = RECT::default();
            if GetWindowRect(hwnd, &mut rc).is_ok() {
                let b = RESIZE_BORDER;
                let left = pt.x < rc.left + b;
                let right = pt.x >= rc.right - b;
                let top = pt.y < rc.top + b;
                let bottom = pt.y >= rc.bottom - b;

                let hit = if top && left {
                    Some(HTTOPLEFT)
                } else if top && right {
                    Some(HTTOPRIGHT)
                } else if bottom && left {
                    Some(HTBOTTOMLEFT)
                } else if bottom && right {
                    Some(HTBOTTOMRIGHT)
                } else if left {
                    Some(HTLEFT)
                } else if right {
                    Some(HTRIGHT)
                } else if top {
                    Some(HTTOP)
                } else if bottom {
                    Some(HTBOTTOM)
                } else {
                    None
                };

                if let Some(code) = hit {
                    return LRESULT(code as isize);
                }

                // Title-bar band (excluding the right-side window buttons) acts
                // as the caption, so Windows drags the window natively. This
                // replaces firing WM_NCLBUTTONDOWN from inside Slint's pointer
                // handler, which nested a modal move loop and broke client input.
                let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
                let titlebar_h = (36.0 * scale) as i32;
                let buttons_w = (3.0 * 46.0 * scale) as i32;
                if pt.y < rc.top + titlebar_h && pt.x < rc.right - buttons_w {
                    return LRESULT(HTCAPTION as isize);
                }
            }
            // Everything else: let the default proc classify it (HTCLIENT, etc.)
            // so winit/Slint receive normal mouse input.
            return DefSubclassProc(hwnd, msg, wparam, lparam);
        }
        if msg == WM_DROPFILES {
            let hdrop = HDROP(wparam.0 as *mut std::ffi::c_void);
            let mut dropped = Vec::new();
            // Passing 0xFFFFFFFF as the index returns the file count.
            let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, None);
            for i in 0..count {
                // First query the required length (excluding NUL).
                let len = DragQueryFileW(hdrop, i, None);
                if len == 0 {
                    continue;
                }
                let mut buf = vec![0u16; len as usize + 1];
                let written = DragQueryFileW(hdrop, i, Some(&mut buf));
                if written > 0 {
                    let s = String::from_utf16_lossy(&buf[..written as usize]);
                    dropped.push(PathBuf::from(s));
                }
            }
            DragFinish(hdrop);
            if !dropped.is_empty() {
                inbox().lock().unwrap().extend(dropped);
            }
            return LRESULT(0);
        }
        DefSubclassProc(hwnd, msg, wparam, lparam)
    }
}
