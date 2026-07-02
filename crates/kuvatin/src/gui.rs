use crate::collect::collect_images;
use anyhow::{anyhow, Result};
use kuvatin_core::batch::run_jobs_to;
use kuvatin_core::naming::{ensure_unique, subfolder_name};
use kuvatin_core::crop::CropMode;
use kuvatin_core::format::OutputFormat;
use kuvatin_core::pipeline::{Job, PngOptimize};
use kuvatin_core::preset::PresetStore;
use slint::{Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

slint::include_modules!();

/// Create a video Player for `path`, start playback, and store it in `slot`
/// (dropping any previous player). Decoded RGBA frames are pushed to the UI's
/// `video-frame` from the GStreamer thread via `invoke_from_event_loop`.
/// Create a GES editing project whose composited preview frames are pushed to
/// the UI's `video-frame` (from a GStreamer thread, hopped to the UI thread).
fn make_project(ui_weak: &slint::Weak<AppWindow>) -> Option<kuvatin_video::Project> {
    let pending: Arc<Mutex<Option<kuvatin_video::Frame>>> = Arc::new(Mutex::new(None));
    let ui_for_frame = ui_weak.clone();
    match kuvatin_video::Project::new(move |frame| {
        *pending.lock().unwrap() = Some(frame);
        let ui_for_frame = ui_for_frame.clone();
        let pending = pending.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let (Some(ui), Some(f)) = (ui_for_frame.upgrade(), pending.lock().unwrap().take()) {
                let buf =
                    SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&f.rgba, f.width, f.height);
                ui.set_video_frame(Image::from_rgba8(buf));
            }
        });
    }) {
        Ok(project) => Some(project),
        Err(e) => {
            eprintln!("video project init error: {e:#}");
            None
        }
    }
}

/// Convert an optional RGBA frame into a Slint image (empty image if None).
fn frame_to_image(frame: Option<kuvatin_video::Frame>) -> Image {
    match frame {
        Some(f) if f.width > 0 && f.height > 0 => {
            let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&f.rgba, f.width, f.height);
            Image::from_rgba8(buf)
        }
        _ => Image::default(),
    }
}

/// Add a media file to the media bin (the library in the left panel).
fn add_to_bin(assets: &Rc<VecModel<VideoAsset>>, path: &std::path::Path, thumb: Image) {
    let name: SharedString = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .into();
    assets.push(VideoAsset { name, thumb });
}

/// Append a media file to the timeline as a clip: create the project if needed,
/// place it at its track's end, mirror it into the model, and play.
fn add_to_timeline(
    path: &std::path::Path,
    ui_weak: &slint::Weak<AppWindow>,
    project_slot: &Rc<RefCell<Option<kuvatin_video::Project>>>,
    tl_clips: &Rc<VecModel<TimelineClip>>,
    thumb: Image,
) {
    if project_slot.borrow().is_none() {
        *project_slot.borrow_mut() = make_project(ui_weak);
    }
    let mut slot = project_slot.borrow_mut();
    let Some(project) = slot.as_mut() else {
        return;
    };
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let is_img = matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif");
    let img_dur = is_img.then(|| std::time::Duration::from_secs(5));
    // GES composites lower layer indices ON TOP, so images (overlays) go on
    // layer 0 and videos on layer 1 (the base, underneath).
    let track = if is_img { 0 } else { 1 };
    match project.append_clip(path, track, img_dur) {
        Ok(info) => {
            let name: SharedString = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
                .into();
            tl_clips.push(TimelineClip {
                id: info.id.0.clone().into(),
                track: info.track as i32,
                start: info.start.as_secs_f32(),
                duration: info.duration.as_secs_f32(),
                inpoint: 0.0,
                name,
                kind: if is_img { 1 } else { 0 },
                selected: false,
                thumb,
            });
            let _ = project.play();
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_video_playing(true);
                if let Some(d) = project.duration() {
                    ui.set_timeline_duration(d.as_secs_f32());
                }
            }
        }
        Err(e) => eprintln!("append clip: {e:#}"),
    }
}

pub fn run(initial_paths: Vec<PathBuf>) -> Result<()> {
    // Self-heal the per-user Explorer context-menu registration: the MSI only
    // registers for the installing user, so other accounts (or a moved exe)
    // pick it up here on first launch. Best-effort, never blocks startup.
    crate::shell::ensure_registered();

    let store_path = PresetStore::default_path().ok_or_else(|| anyhow!("no config dir"))?;
    let store = Arc::new(Mutex::new(PresetStore::load_or_init(&store_path)?));

    let ui = AppWindow::new()?;

    // Initialize the preset-names model and the format/quality controls from the
    // first preset so they reflect (and can override) what will actually be applied.
    refresh_presets(&ui, &store.lock().unwrap(), 0);

    let files: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(collect_images(&initial_paths)));

    // Per-file crops in ABSOLUTE pixels (x, y, w, h) keyed by input path. Files
    // not present here are converted with the base job (no crop override).
    type CropMap = HashMap<PathBuf, (u32, u32, u32, u32)>;
    let crops: Arc<Mutex<CropMap>> = Arc::new(Mutex::new(HashMap::new()));

    let rows = Rc::new(VecModel::from(rows_from(&files.lock().unwrap(), &crops.lock().unwrap())));
    ui.set_files(ModelRc::from(rows.clone()));
    spawn_thumbnails(ui.as_weak(), files.clone(), files.lock().unwrap().clone());
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
                ui.set_suffix(p.job.output.suffix.clone().into());
                ui.set_save_subfolder(p.job.output.subfolder);
                ui.set_preset_name(p.name.clone().into());
            }
        });
    }

    {
        let files = files.clone();
        let rows = rows.clone();
        let crops = crops.clone();
        let ui_weak = ui.as_weak();
        ui.on_add_files(move || {
            if let Some(picked) = rfd::FileDialog::new()
                .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp", "tiff", "gif"])
                .pick_files()
            {
                add_paths(picked, &files, &rows, &crops, &ui_weak);
            }
        });
    }

    // Video editor shared state: the GES project and its timeline models. Created
    // here so both the drag-and-drop drain (below) and the video callbacks (later)
    // can reach the same project and reflect dropped/opened media onto the timeline.
    let video_project: Rc<RefCell<Option<kuvatin_video::Project>>> = Rc::new(RefCell::new(None));
    let video_assets = Rc::new(VecModel::<VideoAsset>::from(Vec::<VideoAsset>::new()));
    ui.set_video_clips(ModelRc::from(video_assets.clone()));
    // Source path of each media-bin entry (parallel to video_assets) so a bin
    // click can add that file to the timeline.
    let bin_paths: Rc<RefCell<Vec<PathBuf>>> = Rc::new(RefCell::new(Vec::new()));
    let video_tl = Rc::new(VecModel::<TimelineClip>::from(Vec::<TimelineClip>::new()));
    ui.set_timeline_clips(ModelRc::from(video_tl.clone()));
    // Timeline tracks (GES layers, top = index 0 = composited on top). Kept
    // mutable so dragging a clip onto a new track can grow the list.
    let video_tracks = Rc::new(VecModel::<SharedString>::from(vec![
        SharedString::from("Track 1"),
        SharedString::from("Track 2"),
    ]));
    ui.set_timeline_track_labels(ModelRc::from(video_tracks.clone()));

    // File import: a worker thread discovers dropped/opened media OFF the UI
    // thread (warming the GES asset cache); a UI timer then adds each cache-warm
    // clip quickly. So importing many files shows a progress modal instead of
    // freezing the app for the whole batch.
    type ImportItem = (PathBuf, Option<kuvatin_video::Frame>);
    let (import_tx, import_rx) = std::sync::mpsc::channel::<PathBuf>();
    let import_ready: Arc<Mutex<std::collections::VecDeque<ImportItem>>> =
        Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let import_total = Rc::new(std::cell::Cell::new(0usize));
    let import_done = Rc::new(std::cell::Cell::new(0usize));
    {
        let ready = import_ready.clone();
        std::thread::spawn(move || {
            for path in import_rx {
                let _ = kuvatin_video::warm_asset(&path);
                let thumb = kuvatin_video::thumbnail(&path, 160);
                ready.lock().unwrap().push_back((path, thumb));
            }
        });
    }

    // Windows Explorer drag-and-drop: enable WM_DROPFILES on the native window
    // and drain dropped paths into the right pipeline (images → file list,
    // videos → timeline). The native HWND is only available after the window is
    // shown, so we wire it up from a single-shot timer once the loop is running.
    #[cfg(windows)]
    {
        let files = files.clone();
        let rows = rows.clone();
        let crops = crops.clone();
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

        // Drain dropped paths on the UI thread. Images go to the file list;
        // videos are queued for the import worker (discovered off-thread, then
        // added by the import timer) so a big drop doesn't freeze the app.
        let import_tx = import_tx.clone();
        let import_total = import_total.clone();
        let drain_timer = slint::Timer::default();
        drain_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(150),
            move || {
                let dropped = win_drop::take_dropped();
                if dropped.is_empty() {
                    return;
                }
                let videos = ui_weak.upgrade().map(|ui| ui.get_app_mode() == 1).unwrap_or(false);
                if videos {
                    for path in dropped {
                        let _ = import_tx.send(path);
                        import_total.set(import_total.get() + 1);
                    }
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_importing(true);
                        ui.set_import_total(import_total.get() as i32);
                    }
                } else {
                    add_paths(dropped, &files, &rows, &crops, &ui_weak);
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
        let edit = edit.clone();
        let ui_weak = ui.as_weak();
        ui.on_clear_files(move || {
            let Some(ui) = ui_weak.upgrade() else { return; };
            let mut guard = files.lock().unwrap();
            guard.clear();
            let mut crops_guard = crops.lock().unwrap();
            crops_guard.clear();
            refresh(&rows, &guard, &crops_guard);
            drop(crops_guard);
            ui.set_selected_index(-1);
            ui.set_cropping(false);
            *edit.lock().unwrap() = None;
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

    // Selecting a file shows it large in the viewer (and prepares crop state).
    {
        let files = files.clone();
        let crops = crops.clone();
        let edit = edit.clone();
        let ui_weak = ui.as_weak();
        ui.on_select_file(move |index| {
            let Some(ui) = ui_weak.upgrade() else { return; };
            let path = match files.lock().unwrap().get(index as usize) {
                Some(p) => p.clone(),
                None => return,
            };
            let Ok(img) = image::open(&path) else { return; };
            let (ow, oh) = (img.width(), img.height());
            if ow == 0 || oh == 0 { return; }

            // Decode a display-sized preview; normalized crop coords stay size-independent.
            let preview = img.thumbnail(1280, 1280).to_rgba8();
            let (pw, ph) = (preview.width(), preview.height());
            let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(preview.as_raw(), pw, ph);
            ui.set_viewer_image(Image::from_rgba8(buf));
            ui.set_selected_index(index);

            // Seed crop state for this file (used when the viewer enters Crop mode later).
            ui.set_crop_img_w(ow as i32);
            ui.set_crop_img_h(oh as i32);
            let (bw, bh) = crate::preview::preview_box(ow, oh, 560.0, 420.0);
            ui.set_crop_box_w(bw);
            ui.set_crop_box_h(bh);
            if let Some(&(x, y, w, h)) = crops.lock().unwrap().get(&path) {
                ui.set_crop_x(x as f32 / ow as f32);
                ui.set_crop_y(y as f32 / oh as f32);
                ui.set_crop_w(w as f32 / ow as f32);
                ui.set_crop_h(h as f32 / oh as f32);
            } else {
                ui.set_crop_x(0.0); ui.set_crop_y(0.0);
                ui.set_crop_w(1.0); ui.set_crop_h(1.0);
            }
            *edit.lock().unwrap() = Some((path, ow, oh));
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

            // Ask where to save. A single file with no subfolder -> a Save dialog
            // with the suffixed name pre-filled. Otherwise -> a folder picker;
            // when "save to a subfolder" is on, outputs nest into a folder named
            // after the suffix. Each output is `<stem><suffix>.<ext>`, de-duplicated
            // on collision. Cancelling either dialog aborts the run.
            let suffix = ui.get_suffix().to_string();
            let subfolder = ui.get_save_subfolder();
            let ext = job.format.extension();
            let stem_of = |p: &std::path::Path| {
                p.file_stem().and_then(|s| s.to_str()).unwrap_or("image").to_string()
            };
            let items_to: Vec<(PathBuf, Job, PathBuf)> = if items.len() == 1 && !subfolder {
                let (input, j) = items[0].clone();
                let mut dlg = rfd::FileDialog::new()
                    .set_file_name(format!("{}{suffix}.{ext}", stem_of(input.as_path())))
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
                items
                    .iter()
                    .map(|(input, j)| {
                        let out = ensure_unique(dir.join(format!("{}{suffix}.{ext}", stem_of(input.as_path()))));
                        (input.clone(), j.clone(), out)
                    })
                    .collect()
            };

            ui.set_running(true);
            ui.set_progress(0.0);

            let ui_weak2 = ui_weak.clone();
            let total = items_to.len();
            let rows_paths = inputs.clone();
            std::thread::spawn(move || {
                let ui_for_progress = ui_weak2.clone();
                run_jobs_to(&items_to, move |p| {
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

    // Video editor: a GES Project drives the composited preview and the timeline.
    // Opening media appends a clip to track 0 and mirrors it into the timeline model.
    {
        let ui_weak = ui.as_weak();
        // Models + project are created earlier (shared with drag-and-drop); reuse them.
        let project_slot = video_project.clone();
        let assets = video_assets.clone();
        let tl_clips = video_tl.clone();
        // Index of the selected timeline clip (for the inspector), or -1.
        let sel_idx = Rc::new(std::cell::Cell::new(-1i32));
        // Latest inspector transform awaiting a coalesced apply on the UI timer.
        // Rapid slider drags only stash a value here; no GES work per event.
        let pending_xform: Rc<RefCell<Option<(String, kuvatin_video::Layout)>>> =
            Rc::new(RefCell::new(None));
        // True while an export/render is running (pauses the preview timer, whose
        // seeks/commits would corrupt the render).
        let export_active = Rc::new(std::cell::Cell::new(false));

        // Open media via the file dialog → the same import queue as drag-and-drop.
        {
            let ui_weak = ui_weak.clone();
            let import_tx = import_tx.clone();
            let import_total = import_total.clone();
            ui.on_video_open(move || {
                let Some(paths) = rfd::FileDialog::new()
                    .add_filter(
                        "Media",
                        &[
                            "mp4", "mov", "mkv", "webm", "avi", "m4v", "wmv", "png", "jpg", "jpeg",
                            "webp", "bmp", "gif",
                        ],
                    )
                    .pick_files()
                else {
                    return;
                };
                for path in paths {
                    let _ = import_tx.send(path);
                    import_total.set(import_total.get() + 1);
                }
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_importing(true);
                    ui.set_import_total(import_total.get() as i32);
                }
            });
        }

        // Import timer: pull discovered (cache-warm) files off the worker's ready
        // queue and add them quickly, advancing the progress modal.
        {
            let ui_weak = ui_weak.clone();
            let project_slot = project_slot.clone();
            let assets = assets.clone();
            let bin_paths = bin_paths.clone();
            let tl_clips = tl_clips.clone();
            let ready = import_ready.clone();
            let import_total = import_total.clone();
            let import_done = import_done.clone();
            let timer = slint::Timer::default();
            timer.start(
                slint::TimerMode::Repeated,
                std::time::Duration::from_millis(60),
                move || {
                    loop {
                        let next = ready.lock().unwrap().pop_front();
                        let Some((path, thumb_frame)) = next else {
                            break;
                        };
                        let thumb = frame_to_image(thumb_frame);
                        add_to_bin(&assets, &path, thumb.clone());
                        bin_paths.borrow_mut().push(path.clone());
                        // Only the first file (when the timeline is empty) goes on
                        // the timeline; the rest wait in the bin for the user.
                        if tl_clips.row_count() == 0 {
                            add_to_timeline(&path, &ui_weak, &project_slot, &tl_clips, thumb);
                        }
                        import_done.set(import_done.get() + 1);
                    }
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_import_done(import_done.get() as i32);
                        if import_total.get() > 0 && import_done.get() >= import_total.get() {
                            ui.set_importing(false);
                            import_total.set(0);
                            import_done.set(0);
                        }
                    }
                },
            );
            std::mem::forget(timer);
        }

        // Media-bin asset click: highlight it.
        {
            let ui_weak = ui_weak.clone();
            ui.on_video_select(move |i| {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_video_selected(i);
                }
            });
        }

        // Media-bin item "add": append that file to the timeline as a new clip.
        {
            let ui_weak = ui_weak.clone();
            let project_slot = project_slot.clone();
            let tl_clips = tl_clips.clone();
            let assets = assets.clone();
            let bin_paths = bin_paths.clone();
            ui.on_video_add(move |i| {
                let Some(path) = bin_paths.borrow().get(i as usize).cloned() else {
                    return;
                };
                let thumb = assets
                    .row_data(i as usize)
                    .map(|a| a.thumb)
                    .unwrap_or_default();
                add_to_timeline(&path, &ui_weak, &project_slot, &tl_clips, thumb);
            });
        }

        // Timeline clip click: select it (highlight) + populate the inspector.
        {
            let ui_weak = ui_weak.clone();
            let tl_clips = tl_clips.clone();
            let project_slot = project_slot.clone();
            let sel_idx = sel_idx.clone();
            ui.on_timeline_select(move |i| {
                let Some(ui) = ui_weak.upgrade() else {
                    return;
                };
                sel_idx.set(i);
                let mut name = SharedString::new();
                let mut sel_id = SharedString::new();
                let mut is_img = false;
                for idx in 0..tl_clips.row_count() {
                    if let Some(mut c) = tl_clips.row_data(idx) {
                        c.selected = idx as i32 == i;
                        if c.selected {
                            name = c.name.clone();
                            sel_id = c.id.clone();
                            is_img = c.kind == 1;
                        }
                        tl_clips.set_row_data(idx, c);
                    }
                }
                ui.set_inspector_name(name);
                ui.set_insp_has_audio(!is_img);
                // Give a fresh clip an aspect-correct default, then reflect its
                // current layout into the sliders.
                {
                    let mut slot = project_slot.borrow_mut();
                    if let Some(p) = slot.as_mut() {
                        let cid = kuvatin_video::ClipId(sel_id.to_string());
                        p.ensure_laid_out(&cid);
                        if let Some(l) = p.clip_layout(&cid) {
                            ui.set_insp_posx(l.posx as f32);
                            ui.set_insp_posy(l.posy as f32);
                            ui.set_insp_scale(((l.scale * 100.0) as f32).clamp(10.0, 100.0));
                            ui.set_insp_alpha((l.alpha as f32 * 100.0).clamp(0.0, 100.0));
                            ui.set_insp_volume((l.volume as f32 * 100.0).clamp(0.0, 100.0));
                        }
                        // Fit size drives the preview bounding box dimensions.
                        let (fw, fh) = p.clip_fit_size(&cid).unwrap_or((
                            kuvatin_video::CANVAS_W as u32,
                            kuvatin_video::CANVAS_H as u32,
                        ));
                        ui.set_sel_fit_w(fw as f32);
                        ui.set_sel_fit_h(fh as f32);
                    }
                }
            });
        }

        // Inspector slider moved: stash the transform; the UI timer applies it
        // (coalesced) so a fast drag never touches GES on the event itself.
        {
            let ui_weak = ui_weak.clone();
            let tl_clips = tl_clips.clone();
            let sel_idx = sel_idx.clone();
            let pending_xform = pending_xform.clone();
            ui.on_inspector_changed(move || {
                let Some(ui) = ui_weak.upgrade() else {
                    return;
                };
                let i = sel_idx.get();
                if i < 0 {
                    return;
                }
                let Some(row) = tl_clips.row_data(i as usize) else {
                    return;
                };
                let l = kuvatin_video::Layout {
                    posx: ui.get_insp_posx() as i32,
                    posy: ui.get_insp_posy() as i32,
                    scale: (ui.get_insp_scale() / 100.0) as f64,
                    alpha: (ui.get_insp_alpha() / 100.0) as f64,
                    volume: (ui.get_insp_volume() / 100.0) as f64,
                };
                *pending_xform.borrow_mut() = Some((row.id.to_string(), l));
            });
        }

        // Drop a clip: slide it (delta seconds) and/or move it to another/new
        // track (delta rows). Both are applied to GES and mirrored to the model.
        {
            let ui_weak = ui_weak.clone();
            let project_slot = project_slot.clone();
            let tl_clips = tl_clips.clone();
            let tracks = video_tracks.clone();
            ui.on_timeline_clip_dropped(move |i, delta_secs, delta_rows| {
                let Some(mut row) = tl_clips.row_data(i as usize) else {
                    return;
                };
                let cid = kuvatin_video::ClipId(row.id.to_string());
                let mut slot = project_slot.borrow_mut();
                let Some(p) = slot.as_mut() else {
                    return;
                };
                // Horizontal: slide along the track.
                if let Some(geom) = p.slide_clip(&cid, delta_secs as f64) {
                    row.start = geom.start.as_secs_f32();
                    row.inpoint = geom.inpoint.as_secs_f32();
                    row.duration = geom.duration.as_secs_f32();
                }
                // Vertical: move to another track (or a new bottom track). Clamp
                // to [0, count]; count means "one past the last" = a new track.
                if delta_rows != 0 {
                    let count = p.track_count() as i32;
                    let target = (row.track + delta_rows).clamp(0, count);
                    if target != row.track {
                        if let Some(t) = p.move_clip_to_track(&cid, target as usize) {
                            row.track = t as i32;
                        }
                    }
                    // Grow the gutter labels to match any newly created track.
                    let new_count = p.track_count();
                    while tracks.row_count() < new_count {
                        let n = tracks.row_count() + 1;
                        tracks.push(SharedString::from(format!("Track {n}")));
                    }
                }
                let dur = p.duration();
                drop(slot);
                tl_clips.set_row_data(i as usize, row);
                if let (Some(ui), Some(d)) = (ui_weak.upgrade(), dur) {
                    ui.set_timeline_duration(d.as_secs_f32());
                }
            });
        }

        // Magnetic snap: given the dragged clip's proposed slide (seconds), nudge
        // its nearest edge onto a neighbouring clip edge or the timeline start when
        // within ~8px. Pure/read-only — it drives the live drag binding AND the drop
        // commit, so what you see snapping is exactly where the clip lands.
        {
            let tl_clips = tl_clips.clone();
            ui.on_timeline_snap_dx(move |i, dx_s, pps| {
                if pps <= 0.0 || i < 0 {
                    return dx_s;
                }
                let i = i as usize;
                let n = tl_clips.row_count();
                let Some(dragged) = tl_clips.row_data(i) else {
                    return dx_s;
                };
                let start = dragged.start;
                let prop_start = start + dx_s;
                let prop_end = start + dragged.duration + dx_s;
                // Snap targets: timeline origin + every OTHER clip's start/end edge.
                let mut targets: Vec<f32> = Vec::with_capacity(2 * n + 1);
                targets.push(0.0);
                for j in 0..n {
                    if j == i {
                        continue;
                    }
                    if let Some(c) = tl_clips.row_data(j) {
                        targets.push(c.start);
                        targets.push(c.start + c.duration);
                    }
                }
                // Pick the target within threshold needing the smallest nudge,
                // measured against whichever edge (start/end) is closest to it.
                let threshold = 8.0 / pps; // 8 px expressed in seconds
                let mut best_adjust = 0.0f32;
                let mut best_dist = threshold;
                for t in targets {
                    for edge in [prop_start, prop_end] {
                        let a = t - edge;
                        if a.abs() < best_dist {
                            best_dist = a.abs();
                            best_adjust = a;
                        }
                    }
                }
                let snapped = dx_s + best_adjust;
                // Never slide a clip's start before the timeline origin.
                if start + snapped < 0.0 {
                    -start
                } else {
                    snapped
                }
            });
        }

        // Reorder tracks by dragging a header: move the GES layer, then resync
        // every clip's track from GES (a reorder shifts several layers' indices).
        {
            let project_slot = project_slot.clone();
            let tl_clips = tl_clips.clone();
            ui.on_track_reordered(move |from, to| {
                if from == to {
                    return;
                }
                let mut slot = project_slot.borrow_mut();
                let Some(p) = slot.as_mut() else {
                    return;
                };
                p.move_track(from as usize, to as usize);
                for idx in 0..tl_clips.row_count() {
                    if let Some(mut row) = tl_clips.row_data(idx) {
                        if let Some(t) = p.clip_track(&kuvatin_video::ClipId(row.id.to_string())) {
                            if row.track != t as i32 {
                                row.track = t as i32;
                                tl_clips.set_row_data(idx, row);
                            }
                        }
                    }
                }
            });
        }

        // Trim a clip by dragging an edge (edge: -1 left, +1 right).
        {
            let ui_weak = ui_weak.clone();
            let project_slot = project_slot.clone();
            let tl_clips = tl_clips.clone();
            ui.on_timeline_clip_trimmed(move |i, edge, delta| {
                let Some(mut row) = tl_clips.row_data(i as usize) else {
                    return;
                };
                let geom = project_slot
                    .borrow_mut()
                    .as_mut()
                    .and_then(|p| p.trim_clip(&kuvatin_video::ClipId(row.id.to_string()), edge, delta as f64));
                let Some(geom) = geom else {
                    return;
                };
                row.start = geom.start.as_secs_f32();
                row.inpoint = geom.inpoint.as_secs_f32();
                row.duration = geom.duration.as_secs_f32();
                tl_clips.set_row_data(i as usize, row);
                if let (Some(ui), Some(d)) = (
                    ui_weak.upgrade(),
                    project_slot.borrow().as_ref().and_then(|p| p.duration()),
                ) {
                    ui.set_timeline_duration(d.as_secs_f32());
                }
            });
        }

        // Play / pause.
        {
            let ui_weak = ui_weak.clone();
            let project_slot = project_slot.clone();
            ui.on_video_playpause(move || {
                let Some(ui) = ui_weak.upgrade() else {
                    return;
                };
                let slot = project_slot.borrow();
                let Some(project) = slot.as_ref() else {
                    return;
                };
                if ui.get_video_playing() {
                    let _ = project.pause();
                    ui.set_video_playing(false);
                } else {
                    let _ = project.play();
                    ui.set_video_playing(true);
                }
            });
        }

        // Seek to a fraction of the timeline.
        {
            let project_slot = project_slot.clone();
            ui.on_video_seek(move |frac| {
                let slot = project_slot.borrow();
                if let Some(project) = slot.as_ref() {
                    if let Some(dur) = project.duration() {
                        let _ = project.seek(dur.mul_f32(frac.clamp(0.0, 1.0)));
                    }
                }
            });
        }

        // Scrub: drag/click the timeline lane to move the playhead + seek.
        {
            let ui_weak = ui_weak.clone();
            let project_slot = project_slot.clone();
            ui.on_timeline_seek_time(move |secs| {
                let slot = project_slot.borrow();
                let Some(project) = slot.as_ref() else {
                    return;
                };
                let dur = project.duration().map(|d| d.as_secs_f32()).unwrap_or(0.0);
                let secs = if dur > 0.0 { secs.clamp(0.0, dur) } else { secs.max(0.0) };
                let _ = project.seek(std::time::Duration::from_secs_f32(secs));
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_playhead(secs);
                    if dur > 0.0 {
                        ui.set_video_position((secs / dur).clamp(0.0, 1.0));
                    }
                }
            });
        }

        // Transport volume: master output level for the whole preview.
        {
            let ui_weak = ui_weak.clone();
            let project_slot = project_slot.clone();
            ui.on_video_volume_changed(move |v| {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_video_volume(v);
                }
                if let Some(p) = project_slot.borrow().as_ref() {
                    p.set_master_volume(v as f64);
                }
            });
        }

        // Export: pick an output file + format, then start rendering.
        // Change the composited canvas ("viewport") size. Creates the project on
        // demand so a size chosen before importing still applies, then refreshes the
        // selected clip's fit size (which drives the preview bounding box).
        {
            let ui_weak = ui_weak.clone();
            let project_slot = project_slot.clone();
            let tl_clips = tl_clips.clone();
            let sel_idx = sel_idx.clone();
            ui.on_set_canvas_size(move |w, h| {
                let w = w.clamp(16, 7680);
                let h = h.clamp(16, 4320);
                if project_slot.borrow().is_none() {
                    *project_slot.borrow_mut() = make_project(&ui_weak);
                }
                if let Some(p) = project_slot.borrow_mut().as_mut() {
                    p.set_canvas_size(w, h);
                    let i = sel_idx.get();
                    if i >= 0 {
                        if let Some(row) = tl_clips.row_data(i as usize) {
                            let cid = kuvatin_video::ClipId(row.id.to_string());
                            if let (Some((fw, fh)), Some(ui)) =
                                (p.clip_fit_size(&cid), ui_weak.upgrade())
                            {
                                ui.set_sel_fit_w(fw as f32);
                                ui.set_sel_fit_h(fh as f32);
                            }
                        }
                    }
                }
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_canvas_w(w);
                    ui.set_canvas_h(h);
                }
            });
        }

        {
            let ui_weak = ui_weak.clone();
            let project_slot = project_slot.clone();
            let export_active = export_active.clone();
            ui.on_video_export(move || {
                if export_active.get() || project_slot.borrow().is_none() {
                    return;
                }
                let Some(ui) = ui_weak.upgrade() else {
                    return;
                };
                // Codec index → codec + container extension (chosen in the dialog).
                let (codec, ext, default_name) = match ui.get_export_codec() {
                    1 => (kuvatin_video::VideoCodec::Vp9, "webm", "export.webm"),
                    2 => (kuvatin_video::VideoCodec::Vp8, "webm", "export.webm"),
                    _ => (kuvatin_video::VideoCodec::H264, "mp4", "export.mp4"),
                };
                let settings = kuvatin_video::ExportSettings {
                    codec,
                    width: ui.get_export_w(),
                    height: ui.get_export_h(),
                    bitrate_kbps: ui.get_export_bitrate().max(0) as u32,
                };
                let Some(path) = rfd::FileDialog::new()
                    .add_filter(
                        if ext == "mp4" { "MP4 video" } else { "WebM video" },
                        &[ext],
                    )
                    .set_file_name(default_name)
                    .save_file()
                else {
                    return;
                };
                let started = project_slot
                    .borrow()
                    .as_ref()
                    .map(|p| p.begin_render(&path, settings).is_ok())
                    .unwrap_or(false);
                if started {
                    export_active.set(true);
                    ui.set_exporting(true);
                    ui.set_export_progress(0.0);
                    ui.set_export_status("Starting…".into());
                } else {
                    ui.set_export_status("Could not start export".into());
                }
            });
        }

        // Export progress: poll the render; finish or fail restores the preview.
        {
            let ui_weak = ui_weak.clone();
            let project_slot = project_slot.clone();
            let export_active = export_active.clone();
            let timer = slint::Timer::default();
            timer.start(
                slint::TimerMode::Repeated,
                std::time::Duration::from_millis(200),
                move || {
                    if !export_active.get() {
                        return;
                    }
                    let slot = project_slot.borrow();
                    let Some(p) = slot.as_ref() else {
                        return;
                    };
                    match p.render_status() {
                        kuvatin_video::RenderStatus::Rendering(f) => {
                            if let Some(ui) = ui_weak.upgrade() {
                                ui.set_export_progress(f);
                                ui.set_export_status(format!("{:.0}%", f * 100.0).into());
                            }
                        }
                        kuvatin_video::RenderStatus::Done => {
                            let _ = p.end_render();
                            drop(slot);
                            export_active.set(false);
                            if let Some(ui) = ui_weak.upgrade() {
                                ui.set_exporting(false);
                            }
                        }
                        kuvatin_video::RenderStatus::Failed(e) => {
                            let _ = p.end_render();
                            drop(slot);
                            export_active.set(false);
                            eprintln!("export failed: {e}");
                            if let Some(ui) = ui_weak.upgrade() {
                                ui.set_export_status("Export failed".into());
                                ui.set_exporting(false);
                            }
                        }
                    }
                },
            );
            std::mem::forget(timer);
        }

        // Advance the playhead + scrubber + time, and apply coalesced edits.
        {
            let ui_weak = ui_weak.clone();
            let project_slot = project_slot.clone();
            let pending_xform = pending_xform.clone();
            let export_active = export_active.clone();
            let timer = slint::Timer::default();
            timer.start(
                slint::TimerMode::Repeated,
                std::time::Duration::from_millis(100),
                move || {
                    // Never touch the pipeline while a render is in progress.
                    if export_active.get() {
                        return;
                    }
                    let Some(ui) = ui_weak.upgrade() else {
                        return;
                    };
                    let mut slot = project_slot.borrow_mut();
                    let Some(project) = slot.as_mut() else {
                        return;
                    };
                    // Apply the latest inspector transform (if any) then repaint,
                    // both coalesced to one commit + one seek per tick.
                    if let Some((id, l)) = pending_xform.borrow_mut().take() {
                        project.set_clip_layout(&kuvatin_video::ClipId(id), l);
                    }
                    project.refresh_preview();
                    let pos = project.position().unwrap_or_default();
                    let dur = project.duration().unwrap_or_default();
                    // Loop at the end when repeat is on.
                    if ui.get_video_repeat()
                        && ui.get_video_playing()
                        && dur.as_secs_f32() > 0.1
                        && pos.as_secs_f32() + 0.12 >= dur.as_secs_f32()
                    {
                        let _ = project.seek(std::time::Duration::ZERO);
                    }
                    ui.set_playhead(pos.as_secs_f32());
                    let frac = if dur.as_secs_f32() > 0.0 {
                        (pos.as_secs_f32() / dur.as_secs_f32()).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    ui.set_video_position(frac);
                    fn fmt(d: std::time::Duration) -> String {
                        let s = d.as_secs();
                        format!("{}:{:02}", s / 60, s % 60)
                    }
                    ui.set_video_time(format!("{} / {}", fmt(pos), fmt(dur)).into());
                },
            );
            std::mem::forget(timer);
        }
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
        ui.set_suffix(p.job.output.suffix.clone().into());
        ui.set_save_subfolder(p.job.output.subfolder);
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
    job.output.suffix = ui.get_suffix().to_string();
    job.output.subfolder = ui.get_save_subfolder();
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

fn rows_from(paths: &[PathBuf], crops: &HashMap<PathBuf, (u32, u32, u32, u32)>) -> Vec<FileRow> {
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
            cropped: crops.contains_key(p),
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

fn refresh(rows: &Rc<VecModel<FileRow>>, paths: &[PathBuf], crops: &HashMap<PathBuf, (u32, u32, u32, u32)>) {
    let new = rows_from(paths, crops);
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
    crops: &Arc<Mutex<HashMap<PathBuf, (u32, u32, u32, u32)>>>,
    ui_weak: &slint::Weak<AppWindow>,
) {
    let mut guard = files.lock().unwrap();
    guard.extend(collect_images(&picked));
    guard.sort();
    guard.dedup();
    let crops_guard = crops.lock().unwrap();
    refresh(rows, &guard, &crops_guard);
    drop(crops_guard);
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
    use windows::Win32::System::Ole::RevokeDragDrop;
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
            // Slint's winit backend registers its own OLE drop target on the
            // window (RegisterDragDrop). While that's in place our DragAcceptFiles
            // call silently fails (RegisterDragDrop returns ALREADYREGISTERED), so
            // WM_DROPFILES never arrives. Revoke winit's target first, then claim
            // the window for the classic shell drag-drop that posts WM_DROPFILES.
            let _ = RevokeDragDrop(hwnd);
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
