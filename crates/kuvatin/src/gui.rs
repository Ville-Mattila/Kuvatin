use crate::collect::collect_images;
use anyhow::{anyhow, Result};
use kuvatin_core::batch::run_batch;
use kuvatin_core::preset::PresetStore;
use slint::{Model, ModelRc, SharedString, VecModel};
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

    let files: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(collect_images(&initial_paths)));
    let rows = Rc::new(VecModel::from(rows_from(&files.lock().unwrap())));
    ui.set_files(ModelRc::from(rows.clone()));

    {
        let files = files.clone();
        let rows = rows.clone();
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
            ui.set_running(true);
            ui.set_progress(0.0);

            let ui_weak2 = ui_weak.clone();
            let total = inputs.len();
            std::thread::spawn(move || {
                let ui_for_progress = ui_weak2.clone();
                let rows_paths = inputs.clone();
                run_batch(&inputs, &preset.job, &preset.name, move |p| {
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
        })
        .collect()
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
