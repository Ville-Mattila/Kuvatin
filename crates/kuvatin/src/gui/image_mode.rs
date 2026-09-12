//! Images mode: the file queue (add / drop / remove / clear), the viewer with
//! its inline crop editor, thumbnails, and the Convert batch.

use super::presets::current_job;
use super::{name_list, show_error, show_info, show_info_at, AppWindow, FileRow};
use crate::collect::collect_images;
use kuvatin_core::batch::{file_result_line, run_jobs_to_until, summarize, BatchSummary};
use kuvatin_core::crop::CropMode;
use kuvatin_core::naming::{output_file_name, subfolder_name};
use kuvatin_core::pipeline::{decode_oriented, plan_unique_outputs, Job};
use kuvatin_core::preset::PresetStore;
use slint::{
    ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// How long a selection must stand before its preview is decoded. Long enough
/// to swallow a key repeat (a held arrow fires every ~30 ms), short enough that
/// a deliberate click feels immediate — the row highlights at once either way.
const PREVIEW_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(120);

/// Per-file crops in ABSOLUTE pixels (x, y, w, h) keyed by input path. Files
/// not present here are converted with the base job (no crop override).
pub(super) type CropMap = HashMap<PathBuf, (u32, u32, u32, u32)>;

/// Everything the Images mode owns. The handlers share it through cheap
/// clones of these handles; the batch worker and the thumbnail decoder reach
/// the `Arc`s from their threads.
pub(super) struct ImageState {
    /// The queue, sorted and de-duplicated; row `i` of the list is `files[i]`.
    pub(super) files: Arc<Mutex<Vec<PathBuf>>>,
    pub(super) crops: Arc<Mutex<CropMap>>,
    /// Path-keyed thumbnail cache (see `ThumbCache`): lets row rebuilds restore
    /// thumbnails and keeps re-adds from re-decoding files already seen.
    pub(super) thumbs: ThumbCache,
    pub(super) rows: Rc<VecModel<FileRow>>,
    /// The in-progress crop edit: the file being cropped and its ORIGINAL (w, h).
    pub(super) edit: Arc<Mutex<Option<(PathBuf, u32, u32)>>>,
    /// Raised by Cancel, cleared when a run starts. The batch runner checks it
    /// before every file, so a cancel stops the queue without killing the file
    /// being written at that moment.
    pub(super) cancel: Arc<AtomicBool>,
}

impl ImageState {
    /// Seed the queue from the paths the app was launched with and start
    /// decoding their thumbnails.
    pub(super) fn new(ui: &AppWindow, initial_paths: &[PathBuf]) -> Self {
        let files: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(collect_images(initial_paths)));
        let crops: Arc<Mutex<CropMap>> = Arc::new(Mutex::new(HashMap::new()));
        let thumbs: ThumbCache = Arc::new(Mutex::new(HashMap::new()));
        let rows = Rc::new(VecModel::from(rows_from(
            &files.lock().unwrap(),
            &crops.lock().unwrap(),
            &thumbs,
        )));
        ui.set_files(ModelRc::from(rows.clone()));
        spawn_thumbnails(
            ui.as_weak(),
            files.clone(),
            thumbs.clone(),
            files.lock().unwrap().clone(),
        );
        Self {
            files,
            crops,
            thumbs,
            rows,
            edit: Arc::new(Mutex::new(None)),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// Wire the Images-mode callbacks.
pub(super) fn wire(ui: &AppWindow, st: &ImageState, store: &Arc<Mutex<PresetStore>>) {
    let ImageState {
        files,
        crops,
        thumbs,
        rows,
        edit,
        cancel,
    } = st;
    {
        let files = files.clone();
        let rows = rows.clone();
        let crops = crops.clone();
        let thumbs = thumbs.clone();
        let ui_weak = ui.as_weak();
        ui.on_add_files(move || {
            // Don't let the list change under a running batch: the progress
            // callback addresses model rows by their snapshot index.
            if ui_weak.upgrade().map(|u| u.get_running()).unwrap_or(false) {
                return;
            }
            if let Some(picked) = rfd::FileDialog::new()
                .add_filter("Images", kuvatin_core::format::INPUT_EXTENSIONS)
                .pick_files()
            {
                add_paths(picked, &files, &rows, &crops, &thumbs, &ui_weak);
            }
        });
    }
    {
        let files = files.clone();
        let rows = rows.clone();
        let crops = crops.clone();
        let thumbs = thumbs.clone();
        let edit = edit.clone();
        let ui_weak = ui.as_weak();
        ui.on_clear_files(move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            if ui.get_running() {
                return; // don't clear the list a running batch is iterating
            }
            let mut guard = files.lock().unwrap();
            let old = std::mem::take(&mut *guard);
            let mut crops_guard = crops.lock().unwrap();
            crops_guard.clear();
            thumbs.lock().unwrap().clear();
            sync_rows(&rows, &old, &guard, &crops_guard, &thumbs);
            drop(crops_guard);
            ui.set_selected_index(-1);
            ui.set_viewer_image(Image::default());
            ui.set_cropping(false);
            *edit.lock().unwrap() = None;
        });
    }

    // Remove one file from the queue (the × on its row). Selection and the
    // crop state follow the file, not the index.
    {
        let files = files.clone();
        let rows = rows.clone();
        let crops = crops.clone();
        let thumbs = thumbs.clone();
        let edit = edit.clone();
        let ui_weak = ui.as_weak();
        ui.on_remove_file(move |i| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            if ui.get_running() || i < 0 {
                return;
            }
            let mut guard = files.lock().unwrap();
            if (i as usize) >= guard.len() {
                return;
            }
            let old = guard.clone();
            let removed = guard.remove(i as usize);
            let mut crops_guard = crops.lock().unwrap();
            crops_guard.remove(&removed);
            sync_rows(&rows, &old, &guard, &crops_guard, &thumbs);
            drop(crops_guard);
            drop(guard);
            let sel = ui.get_selected_index();
            if sel == i {
                ui.set_selected_index(-1);
                ui.set_viewer_image(Image::default());
                ui.set_cropping(false);
                *edit.lock().unwrap() = None;
            } else if sel > i {
                ui.set_selected_index(sel - 1);
            }
        });
    }

    // Selecting a file shows it large in the viewer (and prepares crop state).
    // The decode runs off the UI thread so picking a big image can't freeze the
    // app; a monotonic generation guards against a slow decode landing after the
    // user has already moved on to a different file.
    {
        let files = files.clone();
        let crops = crops.clone();
        let edit = edit.clone();
        let ui_weak = ui.as_weak();
        let select_gen = Arc::new(std::sync::atomic::AtomicU64::new(0));
        // Walking the list with the arrow keys used to start a full decode per
        // keystroke — a hundred-file list, held Down, meant a hundred threads
        // each decoding a photo whose result was then thrown away. The decode
        // waits out a keypress instead; restarting this single-shot timer is
        // what makes only the selection you stop on cost anything.
        let preview_timer = Rc::new(slint::Timer::default());
        ui.on_select_file(move |index| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let path = match files.lock().unwrap().get(index as usize) {
                Some(p) => p.clone(),
                None => return,
            };
            // Highlight is instant; the preview arrives when the decode finishes.
            ui.set_selected_index(index);
            let generation = select_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;

            let ui_weak = ui_weak.clone();
            let crops = crops.clone();
            let edit = edit.clone();
            let select_gen = select_gen.clone();
            let files = files.clone();
            preview_timer.start(slint::TimerMode::SingleShot, PREVIEW_DEBOUNCE, move || {
                let (ui_weak, crops, edit, select_gen, files, path) = (
                    ui_weak.clone(),
                    crops.clone(),
                    edit.clone(),
                    select_gen.clone(),
                    files.clone(),
                    path.clone(),
                );
                std::thread::spawn(move || {
                    // Decode exactly as the conversion will (EXIF orientation applied,
                    // animated GIFs refused), so the crop the user draws lands on the
                    // same pixels the pipeline crops.
                    let decoded = decode_oriented(&path)
                        .ok()
                        .filter(|i| i.width() > 0 && i.height() > 0);
                    let Some(img) = decoded else {
                        // Unreadable: don't leave the viewer and Crop aimed at the
                        // PREVIOUS file — clear them and say so in the row.
                        let _ = slint::invoke_from_event_loop(move || {
                            if select_gen.load(std::sync::atomic::Ordering::SeqCst) != generation {
                                return;
                            }
                            let Some(ui) = ui_weak.upgrade() else {
                                return;
                            };
                            ui.set_viewer_image(Image::default());
                            ui.set_cropping(false);
                            *edit.lock().unwrap() = None;
                            if let Some(i) = files.lock().unwrap().iter().position(|p| *p == path) {
                                let model = ui.get_files();
                                if let Some(mut row) = model.row_data(i) {
                                    row.dims = "unreadable".into();
                                    model.set_row_data(i, row);
                                }
                            }
                        });
                        return;
                    };
                    let (ow, oh) = (img.width(), img.height());
                    // Decode a display-sized preview; normalized crop coords stay
                    // size-independent. Ship raw pixels (Send) to the UI thread.
                    let preview = img.thumbnail(1280, 1280).to_rgba8();
                    let (pw, ph) = (preview.width(), preview.height());
                    let raw = preview.into_raw();
                    let _ = slint::invoke_from_event_loop(move || {
                        // Superseded by a newer selection? then drop this result.
                        if select_gen.load(std::sync::atomic::Ordering::SeqCst) != generation {
                            return;
                        }
                        let Some(ui) = ui_weak.upgrade() else {
                            return;
                        };
                        let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&raw, pw, ph);
                        ui.set_viewer_image(Image::from_rgba8(buf));

                        // Seed crop state for this file (used when the viewer enters
                        // Crop mode later). The crop box itself is sized by the
                        // Slint side from the surface and this aspect ratio.
                        ui.set_crop_img_w(ow as i32);
                        ui.set_crop_img_h(oh as i32);
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
                    });
                });
            });
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
                    row.cropped = true;
                    model.set_row_data(i, row);
                }
            }

            // Exit crop mode; the inline View-button sets cropping=false itself
            // afterward, so this is a harmless no-op on that path.
            ui.set_cropping(false);
        });
    }

    {
        let files = files.clone();
        let store = store.clone();
        let crops = crops.clone();
        let cancel = cancel.clone();
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
            if ui.get_running() {
                return; // a batch is already running; ignore re-entrant Convert
            }
            let job = {
                let store = store.lock().unwrap();
                if store
                    .presets
                    .get(ui.get_current_preset().max(0) as usize)
                    .is_none()
                {
                    return;
                }
                // The same recipe "Save preset" stores, so what runs is what
                // gets saved.
                current_job(&ui, &store)
            };

            // Build a per-file job list: files with a stored crop get a
            // CropMode::Rect override; the rest use the base job unchanged. Clone
            // the crop data out now so we don't hold the lock across the thread.
            let crop_map = crops.lock().unwrap().clone();
            let items: Vec<(PathBuf, Job)> = inputs
                .iter()
                .map(|p| {
                    let mut j = job.clone();
                    if let Some(&(x, y, width, height)) = crop_map.get(p) {
                        j.crop = CropMode::Rect {
                            x,
                            y,
                            width,
                            height,
                        };
                    }
                    (p.clone(), j)
                })
                .collect();

            // Ask where to save. A single file with no subfolder -> a Save dialog
            // with the suffixed name pre-filled. Otherwise -> a folder picker;
            // when "save to a subfolder" is on, outputs nest into a folder named
            // after the suffix. Each output is `<stem><suffix>.<ext>`, de-duplicated
            // on collision. Cancelling either dialog aborts the run.
            let suffix = ui.get_suffix().to_string();
            let subfolder = ui.get_save_subfolder();
            let ext = job.format.extension();
            let stem_of = |p: &std::path::Path| {
                p.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("image")
                    .to_string()
            };
            let items_to: Vec<(PathBuf, Job, PathBuf)> = if items.len() == 1 && !subfolder {
                let (input, j) = items[0].clone();
                // The suffix is user text: sanitized (no separators) like the
                // core pipeline does, never interpolated raw into a path.
                let mut dlg = rfd::FileDialog::new()
                    .set_file_name(output_file_name(
                        &stem_of(input.as_path()),
                        &suffix,
                        job.format,
                    ))
                    .add_filter(ext, &[ext]);
                if let Some(dir) = input.parent() {
                    dlg = dlg.set_directory(dir);
                }
                match dlg.save_file() {
                    Some(out) => vec![(input, j, out)],
                    None => return,
                }
            } else {
                let mut dlg = rfd::FileDialog::new();
                if let Some(dir) = items[0].0.parent() {
                    dlg = dlg.set_directory(dir);
                }
                let base = match dlg.pick_folder() {
                    Some(f) => f,
                    None => return,
                };
                let dir = if subfolder {
                    base.join(subfolder_name(&suffix))
                } else {
                    base
                };
                // Plan every target up front: same-stem inputs from different
                // folders would otherwise all plan the same name and the batch
                // would overwrite its own results (a filesystem check alone
                // can't see the other targets in this batch).
                let targets: Vec<PathBuf> = items
                    .iter()
                    .map(|(input, _)| {
                        dir.join(output_file_name(
                            &stem_of(input.as_path()),
                            &suffix,
                            job.format,
                        ))
                    })
                    .collect();
                items
                    .iter()
                    .zip(plan_unique_outputs(targets))
                    .map(|((input, j), out)| (input.clone(), j.clone(), out))
                    .collect()
            };

            // A previous run's Cancel must not stop this one.
            cancel.store(false, Ordering::Relaxed);
            ui.set_running(true);
            ui.set_progress(0.0);
            // A fresh run: every row goes back to "queued" (a previous run's
            // "done" / sizes used to linger on rows the new run hadn't reached).
            {
                let model = ui.get_files();
                for i in 0..model.row_count() {
                    if let Some(mut row) = model.row_data(i) {
                        row.status = "queued".into();
                        row.result = "".into();
                        model.set_row_data(i, row);
                    }
                }
            }

            let ui_weak2 = ui_weak.clone();
            let total = items_to.len();
            let rows_paths = inputs.clone();
            let cancel_flag = cancel.clone();
            std::thread::spawn(move || {
                let ui_for_progress = ui_weak2.clone();
                let results = run_jobs_to_until(
                    &items_to,
                    move |p| {
                        let frac = p.done as f32 / total as f32;
                        let idx = rows_paths.iter().position(|x| *x == p.last.input);
                        let ok = p.last.outcome.is_ok();
                        // "410 KB (-66%)" for the row, read here on the worker so
                        // the UI thread never touches the filesystem.
                        let result = match &p.last.outcome {
                            Ok(out) => file_result_line(&p.last.input, out),
                            Err(_) => String::new(),
                        };
                        let ui3 = ui_for_progress.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui3.upgrade() {
                                ui.set_progress(frac);
                                if let Some(i) = idx {
                                    let model = ui.get_files();
                                    if let Some(mut row) = model.row_data(i) {
                                        row.status =
                                            if ok { "done".into() } else { "error".into() };
                                        row.result = result.into();
                                        model.set_row_data(i, row);
                                    }
                                }
                            }
                        });
                    },
                    // Checked before each file: the one being written finishes,
                    // the rest of the queue is reported as cancelled.
                    move || cancel_flag.load(Ordering::Relaxed),
                );
                // The summary: counts, bytes in vs out, and which files failed.
                let summary = summarize(&results);
                // Any one output is enough to open the folder they landed in.
                let written = results
                    .iter()
                    .find_map(|r| r.outcome.as_ref().ok().cloned());
                let failed: Vec<String> = results
                    .iter()
                    .filter_map(|r| match &r.outcome {
                        Err(e) if e != kuvatin_core::batch::CANCELLED => Some(format!(
                            "{}: {e}",
                            r.input
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| r.input.display().to_string())
                        )),
                        _ => None,
                    })
                    .collect();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak2.upgrade() {
                        ui.set_running(false);
                        ui.set_progress(1.0);
                        let title = run_title(&summary);
                        let mut detail = summary.size_line();
                        if !failed.is_empty() {
                            if !detail.is_empty() {
                                detail.push_str("\n\n");
                            }
                            detail.push_str(&format!(
                                "{} file{} failed:\n{}",
                                failed.len(),
                                if failed.len() == 1 { "" } else { "s" },
                                name_list(&failed)
                            ));
                        }
                        match (summary.failed, &written) {
                            (0, Some(path)) => show_info_at(&ui, &title, detail, path),
                            (0, None) => show_info(&ui, &title, detail),
                            _ => show_error(&ui, &title, detail),
                        }
                    }
                });
            });
        });
    }

    {
        // Cancel: the button the Convert button turns into while a run is on.
        // Raising the flag is all it does — the worker notices before it starts
        // the next file, and the run ends through its normal summary path.
        let cancel = cancel.clone();
        ui.on_convert_cancel(move || cancel.store(true, Ordering::Relaxed));
    }
}

/// The heading for the dialog a finished run puts up. A cancelled run says so
/// rather than reporting the files it did get through as the whole job.
fn run_title(s: &BatchSummary) -> String {
    if s.cancelled > 0 {
        return format!("Cancelled after {} of {} files", s.ok, s.total());
    }
    if s.failed == 0 {
        return format!(
            "Converted {} file{}",
            s.ok,
            if s.ok == 1 { "" } else { "s" }
        );
    }
    format!("Converted {} of {} files", s.ok, s.total())
}

/// A decoded thumbnail, kept as raw RGBA so it is `Send` (a `slint::Image` is
/// not) and can live in the shared cache. Rebuilt into an `Image` on the UI
/// thread when a row is (re)created.
#[derive(Clone)]
pub(super) struct ThumbData {
    rgba: Vec<u8>,
    w: u32,
    h: u32,
    dims: String,
}

/// Path-keyed cache of decoded thumbnails. Populated once per file by the
/// thumbnail worker; read when rebuilding rows so an add no longer re-decodes
/// every file already in the list (and existing thumbnails survive the rebuild).
pub(super) type ThumbCache = Arc<Mutex<HashMap<PathBuf, ThumbData>>>;

/// A fresh row for `p` (thumbnail from the cache when already decoded).
fn row_for(p: &Path, crops: &CropMap, cache: &HashMap<PathBuf, ThumbData>) -> FileRow {
    let (thumb, dims) = match cache.get(p) {
        Some(d) => (
            Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                &d.rgba, d.w, d.h,
            )),
            d.dims.clone().into(),
        ),
        None => (Image::default(), SharedString::new()),
    };
    FileRow {
        name: p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
            .into(),
        status: "queued".into(),
        result: "".into(),
        thumb,
        dims,
        cropped: crops.contains_key(p),
    }
}

fn rows_from(paths: &[PathBuf], crops: &CropMap, thumbs: &ThumbCache) -> Vec<FileRow> {
    let cache = thumbs.lock().unwrap();
    paths.iter().map(|p| row_for(p, crops, &cache)).collect()
}

/// Bring `rows` (which mirror `old`) in line with `new` by inserting and
/// removing only what changed. Both lists are sorted and de-duplicated. Rows
/// that stay keep their status, thumbnail and dimensions — a rebuild used to
/// reset every row to "queued" and replay the fade-in on the whole list.
fn sync_rows(
    rows: &Rc<VecModel<FileRow>>,
    old: &[PathBuf],
    new: &[PathBuf],
    crops: &CropMap,
    thumbs: &ThumbCache,
) {
    debug_assert_eq!(rows.row_count(), old.len(), "rows must mirror `old`");
    let cache = thumbs.lock().unwrap();
    let mut i = 0; // position in `rows`
    let mut oi = 0; // position in `old`
    for p in new {
        while oi < old.len() && old[oi] < *p {
            rows.remove(i);
            oi += 1;
        }
        if oi < old.len() && old[oi] == *p {
            if let Some(mut r) = rows.row_data(i) {
                let cropped = crops.contains_key(p);
                if r.cropped != cropped {
                    r.cropped = cropped;
                    rows.set_row_data(i, r);
                }
            }
            oi += 1;
        } else {
            rows.insert(i, row_for(p, crops, &cache));
        }
        i += 1;
    }
    while oi < old.len() {
        rows.remove(i);
        oi += 1;
    }
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
    thumbs: ThumbCache,
    paths: Vec<PathBuf>,
) {
    std::thread::spawn(move || {
        for path in paths {
            // Same decode as the conversion (orientation applied) — and an
            // unreadable file says so in its row instead of staying blank.
            let Ok(img) = decode_oriented(&path) else {
                let ui_weak = ui_weak.clone();
                let files = files.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak.upgrade() else {
                        return;
                    };
                    if let Some(i) = files.lock().unwrap().iter().position(|p| *p == path) {
                        let model = ui.get_files();
                        if let Some(mut row) = model.row_data(i) {
                            row.dims = "unreadable".into();
                            model.set_row_data(i, row);
                        }
                    }
                });
                continue;
            };
            let (ow, oh) = (img.width(), img.height());
            let thumb = img.thumbnail(40, 40).to_rgba8();
            let (tw, th) = (thumb.width(), thumb.height());
            let data = ThumbData {
                rgba: thumb.into_raw(),
                w: tw,
                h: th,
                dims: format!("{ow}×{oh}"),
            };
            // Cache first (raw RGBA is `Send`), so a later row rebuild can restore
            // this thumbnail without re-decoding.
            thumbs.lock().unwrap().insert(path.clone(), data.clone());

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
                            // `slint::Image` is not `Send`, so build it here on the
                            // UI thread from the cached raw pixels.
                            let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                                &data.rgba, data.w, data.h,
                            );
                            row.thumb = Image::from_rgba8(buf);
                            row.dims = data.dims.clone().into();
                            model.set_row_data(i, row);
                        }
                    }
                }
            });
        }
    });
}

/// Add `picked` paths to the queue: filter/expand to image files, merge with the
/// existing set (sorted + deduped), update the visible rows, and kick off
/// thumbnail decoding. The selection follows its FILE across the re-sort (the
/// highlighted row used to become a different file than the viewer and the
/// crop state). Shared by the Add files… button and the drag-and-drop drain
/// timer so both paths behave identically.
pub(super) fn add_paths(
    picked: Vec<PathBuf>,
    files: &Arc<Mutex<Vec<PathBuf>>>,
    rows: &Rc<VecModel<FileRow>>,
    crops: &Arc<Mutex<CropMap>>,
    thumbs: &ThumbCache,
    ui_weak: &slint::Weak<AppWindow>,
) {
    let mut guard = files.lock().unwrap();
    let old = guard.clone();
    let selected = ui_weak
        .upgrade()
        .map(|ui| ui.get_selected_index())
        .filter(|&i| i >= 0)
        .and_then(|i| old.get(i as usize).cloned());
    guard.extend(collect_images(&picked));
    guard.sort();
    guard.dedup();
    let crops_guard = crops.lock().unwrap();
    sync_rows(rows, &old, &guard, &crops_guard, thumbs);
    drop(crops_guard);
    if let (Some(ui), Some(sel)) = (ui_weak.upgrade(), selected) {
        let idx = guard
            .iter()
            .position(|p| *p == sel)
            .map(|i| i as i32)
            .unwrap_or(-1);
        ui.set_selected_index(idx);
    }
    // Decode only files we don't already have a thumbnail for — an add no longer
    // re-decodes the whole list, and cached rows kept their thumbnail above.
    let missing: Vec<PathBuf> = {
        let cache = thumbs.lock().unwrap();
        guard
            .iter()
            .filter(|p| !cache.contains_key(*p))
            .cloned()
            .collect()
    };
    drop(guard);
    if !missing.is_empty() {
        spawn_thumbnails(ui_weak.clone(), files.clone(), thumbs.clone(), missing);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kuvatin_core::batch::BatchSummary;

    #[test]
    fn a_clean_run_says_how_many_files_it_converted() {
        let s = BatchSummary {
            ok: 3,
            ..Default::default()
        };
        assert_eq!(run_title(&s), "Converted 3 files");
        let one = BatchSummary {
            ok: 1,
            ..Default::default()
        };
        assert_eq!(run_title(&one), "Converted 1 file");
    }

    #[test]
    fn failures_are_counted_against_the_total() {
        let s = BatchSummary {
            ok: 2,
            failed: 1,
            ..Default::default()
        };
        assert_eq!(run_title(&s), "Converted 2 of 3 files");
    }

    #[test]
    fn a_cancelled_run_says_so_rather_than_claiming_success() {
        let s = BatchSummary {
            ok: 2,
            cancelled: 3,
            ..Default::default()
        };
        assert_eq!(run_title(&s), "Cancelled after 2 of 5 files");
    }
}
