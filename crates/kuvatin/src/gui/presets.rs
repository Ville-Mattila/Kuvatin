//! Presets: the Settings controls are a view of one `Job`; `sync_controls`
//! and `current_job` are inverses, so a preset round-trips through the UI
//! unchanged, and "Convert" runs exactly what "Save" stores.

use super::{show_error, AppWindow};
use kuvatin_core::format::OutputFormat;
use kuvatin_core::pipeline::{Job, PngOptimize};
use kuvatin_core::preset::PresetStore;
use kuvatin_core::resize::ResizeMode;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Wire the preset callbacks: selection, save (upsert), rename, reorder and delete.
pub(super) fn wire(ui: &AppWindow, store: &Arc<Mutex<PresetStore>>, store_path: &Path) {
    let store_path = store_path.to_path_buf();
    // Selecting a preset syncs every control (format, quality, resolution,
    // naming) to that preset's job — a stale resolution override no longer
    // survives into "Original size".
    {
        let store = store.clone();
        let ui_weak = ui.as_weak();
        ui.on_preset_changed(move |idx| {
            let store = store.lock().unwrap();
            if let (Some(ui), Some(p)) = (ui_weak.upgrade(), store.presets.get(idx as usize)) {
                sync_controls(&ui, &p.job);
                ui.set_preset_name(p.name.clone().into());
            }
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
            // The name goes onto the Explorer menu's command line, so it must
            // be quotable there; refuse (with the reason) instead of saving a
            // preset that would silently never appear in the menu.
            if let Err(reason) = kuvatin_core::preset::validate_preset_name(&name) {
                show_error(&ui, "Can't use that preset name", reason);
                return;
            }

            let job = current_job(&ui, &store);

            // Upsert, matching names case-insensitively so "webp" can't sit
            // next to "WebP" as a second preset (and a second menu entry);
            // the stored spelling wins.
            let wanted = name.to_lowercase();
            if let Some(existing) = store
                .presets
                .iter_mut()
                .find(|p| p.name.to_lowercase() == wanted)
            {
                existing.job = job;
                name = existing.name.clone();
            } else {
                store.presets.push(kuvatin_core::preset::Preset {
                    name: name.clone(),
                    job,
                });
            }

            if let Err(e) = store.save(&store_path) {
                show_error(&ui, "Could not save presets", e.to_string());
            }

            let idx = store
                .presets
                .iter()
                .position(|p| p.name == name)
                .unwrap_or(0);
            refresh_presets(&ui, &store, idx);
            ui.set_preset_name(name.into());
            // The Explorer submenu mirrors the store.
            crate::shell::sync_menu();
        });
    }

    // Rename the selected preset to whatever the name field holds. Unlike
    // Save this never touches the preset's job, and it refuses a name another
    // preset already has (case-insensitively) instead of merging the two.
    {
        let store = store.clone();
        let store_path = store_path.clone();
        let ui_weak = ui.as_weak();
        ui.on_rename_preset(move |name| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let mut store = store.lock().unwrap();
            let cur = ui.get_current_preset();
            if cur < 0 {
                return;
            }
            match store.rename(cur as usize, &name) {
                Ok(stored) => {
                    if let Err(e) = store.save(&store_path) {
                        show_error(&ui, "Could not save presets", e.to_string());
                    }
                    refresh_names(&ui, &store, cur as usize);
                    ui.set_preset_name(stored.into());
                    crate::shell::sync_menu();
                }
                Err(reason) => show_error(&ui, "Can't rename the preset", reason),
            }
        });
    }

    // Move the selected preset up or down. The Explorer submenu lists presets
    // in store order, so this is also how the menu is arranged.
    {
        let store = store.clone();
        let store_path = store_path.clone();
        let ui_weak = ui.as_weak();
        ui.on_move_preset(move |delta| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let mut store = store.lock().unwrap();
            let cur = ui.get_current_preset();
            if cur < 0 {
                return;
            }
            let Some(new_idx) = store.move_by(cur as usize, delta) else {
                return;
            };
            if new_idx == cur as usize {
                return;
            }
            if let Err(e) = store.save(&store_path) {
                show_error(&ui, "Could not save presets", e.to_string());
            }
            // Names + selection only: the controls keep any unsaved edits.
            refresh_names(&ui, &store, new_idx);
            crate::shell::sync_menu();
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
            // With no selection (-1) the `as usize` wraps huge and .min() would
            // silently target the LAST preset — bail instead of deleting it.
            let cur = ui.get_current_preset();
            if cur < 0 {
                return;
            }
            let idx = (cur as usize).min(store.presets.len() - 1);
            store.presets.remove(idx);

            if let Err(e) = store.save(&store_path) {
                show_error(&ui, "Could not save presets", e.to_string());
            }

            let select = idx.min(store.presets.len() - 1);
            refresh_presets(&ui, &store, select);
            if let Some(p) = store.presets.get(select) {
                ui.set_preset_name(p.name.clone().into());
            }
            // A deleted preset leaves the menu too (no "unknown preset" ghosts).
            crate::shell::sync_menu();
        });
    }
}

/// (Re)build the preset-names model, select `select` (clamped to a valid index),
/// and sync every control to the selected preset's job.
pub(super) fn refresh_presets(ui: &AppWindow, store: &PresetStore, select: usize) {
    let idx = refresh_names(ui, store, select);
    if let Some(p) = store.presets.get(idx) {
        sync_controls(ui, &p.job);
    }
}

/// (Re)build the preset-names model and select `select` (clamped), leaving
/// the Settings controls alone. Returns the selected index.
fn refresh_names(ui: &AppWindow, store: &PresetStore, select: usize) -> usize {
    let names: Vec<SharedString> = store
        .presets
        .iter()
        .map(|p| p.name.clone().into())
        .collect();
    ui.set_preset_names(ModelRc::new(VecModel::from(names)));
    let idx = select.min(store.presets.len().saturating_sub(1));
    ui.set_current_preset(idx as i32);
    idx
}

/// Mirror a job into the Settings controls — the inverse of [`current_job`],
/// so a preset round-trips through the UI unchanged. A pixel resize shows in
/// the resolution fields; any other resize (percent, fit) leaves them at 0 =
/// "as the preset says".
fn sync_controls(ui: &AppWindow, job: &Job) {
    ui.set_format(format_combo_str(job.format).into());
    ui.set_quality(job.quality as i32);
    ui.set_png_mode(png_mode_to_idx(job.png));
    ui.set_suffix(job.output.suffix.clone().into());
    ui.set_save_subfolder(job.output.subfolder);
    let (w, h, lock) = match job.resize {
        ResizeMode::Pixels {
            width,
            height,
            keep_aspect,
        } => (width.unwrap_or(0), height.unwrap_or(0), keep_aspect),
        _ => (0, 0, true),
    };
    ui.set_res_w(w.min(i32::MAX as u32) as i32);
    ui.set_res_h(h.min(i32::MAX as u32) as i32);
    ui.set_res_lock(lock);
}

/// Build the job described by the live UI: start from the selected preset's job
/// (to preserve crop/other fields), then override format, quality, naming and
/// the resolution from the controls. ONE recipe for "Convert" and "Save
/// preset", so what runs is exactly what gets saved — the resolution override
/// used to apply to conversions but silently vanish from saved presets.
///
/// Resolution: when either field is set (> 0) it replaces the preset's resize;
/// a 0 dimension is left unconstrained; both at 0 keep the preset's resize.
/// The core pipeline crops first and resizes second, so a per-file crop and
/// this resolution combine correctly.
pub(super) fn current_job(ui: &AppWindow, store: &PresetStore) -> Job {
    let idx = ui.get_current_preset().max(0) as usize;
    let mut job = store
        .presets
        .get(idx)
        .map(|p| p.job.clone())
        .unwrap_or_default();
    job.format = format_combo_to_format(&ui.get_format());
    job.quality = ui.get_quality().clamp(0, 100) as u8;
    job.png = png_mode_from(ui.get_png_mode());
    job.output.suffix = ui.get_suffix().to_string();
    job.output.subfolder = ui.get_save_subfolder();
    let rw = ui.get_res_w().max(0) as u32;
    let rh = ui.get_res_h().max(0) as u32;
    if rw > 0 || rh > 0 {
        job.resize = ResizeMode::Pixels {
            width: (rw > 0).then_some(rw),
            height: (rh > 0).then_some(rh),
            keep_aspect: ui.get_res_lock(),
        };
    }
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
