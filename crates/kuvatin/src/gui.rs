use crate::collect::collect_images;
use anyhow::{anyhow, Result};
use kuvatin_core::batch::run_batch;
use kuvatin_core::format::OutputFormat;
use kuvatin_core::preset::PresetStore;
use slint::{Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

slint::include_modules!();

pub fn run(initial_paths: Vec<PathBuf>) -> Result<()> {
    let store_path = PresetStore::default_path().ok_or_else(|| anyhow!("no config dir"))?;
    let store = Arc::new(PresetStore::load_or_init(&store_path)?);

    let ui = AppWindow::new()?;

    let names: Vec<SharedString> = store.presets.iter().map(|p| p.name.clone().into()).collect();
    ui.set_preset_names(ModelRc::new(VecModel::from(names)));

    // Initialize the format/quality controls from the first preset so they reflect
    // (and can override) what will actually be applied.
    if let Some(first) = store.presets.first() {
        ui.set_format(format_combo_str(first.job.format).into());
        ui.set_quality(first.job.quality as i32);
    }

    let files: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(collect_images(&initial_paths)));
    let rows = Rc::new(VecModel::from(rows_from(&files.lock().unwrap())));
    ui.set_files(ModelRc::from(rows.clone()));
    spawn_thumbnails(ui.as_weak(), files.clone(), files.lock().unwrap().clone());

    // Selecting a preset syncs the format/quality controls to that preset's job.
    {
        let store = store.clone();
        let ui_weak = ui.as_weak();
        ui.on_preset_changed(move |idx| {
            if let (Some(ui), Some(p)) = (ui_weak.upgrade(), store.presets.get(idx as usize)) {
                ui.set_format(format_combo_str(p.job.format).into());
                ui.set_quality(p.job.quality as i32);
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
                let mut guard = files.lock().unwrap();
                guard.extend(collect_images(&picked));
                guard.sort();
                guard.dedup();
                refresh(&rows, &guard);
                spawn_thumbnails(ui_weak.clone(), files.clone(), guard.clone());
            }
        });
    }

    {
        let files = files.clone();
        let rows = rows.clone();
        ui.on_clear_files(move || {
            let mut guard = files.lock().unwrap();
            guard.clear();
            refresh(&rows, &guard);
        });
    }

    {
        let files = files.clone();
        let store = store.clone();
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
            let preset = match store.presets.get(preset_idx) {
                Some(p) => p.clone(),
                None => return,
            };
            // Apply the live format/quality controls on top of the preset's job.
            let mut job = preset.job.clone();
            job.format = format_combo_to_format(&ui.get_format());
            job.quality = ui.get_quality().clamp(0, 100) as u8;
            let preset_name = preset.name.clone();

            ui.set_running(true);
            ui.set_progress(0.0);

            let ui_weak2 = ui_weak.clone();
            let total = inputs.len();
            std::thread::spawn(move || {
                let ui_for_progress = ui_weak2.clone();
                let rows_paths = inputs.clone();
                run_batch(&inputs, &job, &preset_name, move |p| {
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
