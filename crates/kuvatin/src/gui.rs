use crate::collect::{collect_images, collect_media};
use anyhow::{anyhow, Result};
use kuvatin_core::batch::run_jobs_to;
use kuvatin_core::crop::CropMode;
use kuvatin_core::format::OutputFormat;
use kuvatin_core::naming::{output_file_name, subfolder_name};
use kuvatin_core::pipeline::{decode_oriented, plan_unique_outputs, Job, PngOptimize};
use kuvatin_core::preset::PresetStore;
use kuvatin_core::resize::ResizeMode;
use slint::{Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

slint::include_modules!();

/// Per-file crops in ABSOLUTE pixels (x, y, w, h) keyed by input path. Files
/// not present here are converted with the base job (no crop override).
type CropMap = HashMap<PathBuf, (u32, u32, u32, u32)>;

/// Show the app's error dialog with `title` + `detail`. The one place failures
/// become visible — in the release windowed build there is no stderr.
fn show_error(ui: &AppWindow, title: &str, detail: impl AsRef<str>) {
    ui.set_error_title(title.into());
    ui.set_error_detail(detail.as_ref().into());
    ui.set_error_visible(true);
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

/// Paths already queued or in the media bin, keyed by their canonical form so
/// `C:\x.mp4` and `c:\x.mp4` (or a `..`-relative spelling) are one file.
#[derive(Default)]
struct SeenSet(std::collections::HashSet<PathBuf>);

impl SeenSet {
    fn key(p: &Path) -> PathBuf {
        std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
    }
    /// Whether `p` was new.
    fn insert(&mut self, p: &Path) -> bool {
        self.0.insert(Self::key(p))
    }
    #[cfg(test)]
    fn contains(&self, p: &Path) -> bool {
        self.0.contains(&Self::key(p))
    }
    fn remove(&mut self, p: &Path) {
        self.0.remove(&Self::key(p));
    }
    fn reseed<'a>(&mut self, keep: impl IntoIterator<Item = &'a PathBuf>) {
        self.0 = keep.into_iter().map(|p| Self::key(p)).collect();
    }
}

/// The media import queue. Dropped/opened paths go to a worker thread
/// (discovery off the UI thread); every batch is stamped with `gen`, and a
/// cancel bumps it, so the worker and the drain discard anything queued before
/// — no cancelled stragglers revived by the next drop, no double-counted files.
struct ImportQueue {
    tx: std::sync::mpsc::Sender<(u64, PathBuf)>,
    /// Files in the current import (drives the progress modal); 0 = idle.
    total: Cell<usize>,
    done: Cell<usize>,
    gen: Arc<AtomicU64>,
    seen: RefCell<SeenSet>,
}

impl ImportQueue {
    fn current_gen(&self) -> u64 {
        self.gen.load(Ordering::Relaxed)
    }

    /// Expand folders, keep media, queue what's new and open the progress
    /// modal. Explicitly dropped EXR frames get a pointer at "Import sequence…"
    /// instead of a slow GES failure.
    fn enqueue(&self, ui: &AppWindow, picked: Vec<PathBuf>) {
        let (media, frames_only) = collect_media(&picked);
        let gen = self.current_gen();
        let mut queued = 0;
        for path in media {
            if !self.seen.borrow_mut().insert(&path) {
                continue; // already queued or in the bin
            }
            let _ = self.tx.send((gen, path));
            self.total.set(self.total.get() + 1);
            queued += 1;
        }
        if queued > 0 {
            ui.set_importing(true);
            ui.set_import_total(self.total.get() as i32);
        }
        if !frames_only.is_empty() {
            show_error(
                ui,
                "EXR frames import as a sequence",
                format!(
                    "{} EXR file(s) were skipped. Use Import sequence\u{2026} and pick the first frame of the run.",
                    frames_only.len()
                ),
            );
        }
    }

    /// Abandon everything queued: later arrivals of the old generation are
    /// dropped, and "seen" is re-seeded from what actually reached the bin so
    /// the discarded files can be imported again.
    fn cancel<'a>(&self, in_bin: impl IntoIterator<Item = &'a PathBuf>) {
        self.gen.fetch_add(1, Ordering::Relaxed);
        self.total.set(0);
        self.done.set(0);
        self.seen.borrow_mut().reseed(in_bin);
    }
}

/// Remove the timeline clip at model index `i` from GES, the model, and fix up
/// the selection and the timeline length (shared by the clip's × button and
/// the Delete key).
fn remove_timeline_clip(
    i: i32,
    ui_weak: &slint::Weak<AppWindow>,
    project_slot: &Rc<RefCell<Option<kuvatin_video::Project>>>,
    tl_clips: &Rc<VecModel<TimelineClip>>,
    sel_idx: &std::rc::Rc<std::cell::Cell<i32>>,
) {
    if i < 0 || (i as usize) >= tl_clips.row_count() {
        return;
    }
    let mut duration = None;
    if let Some(row) = tl_clips.row_data(i as usize) {
        if let Some(p) = project_slot.borrow_mut().as_mut() {
            p.remove_clip(&kuvatin_video::ClipId(row.id.to_string()));
            duration = Some(p.duration());
        }
    }
    tl_clips.remove(i as usize);
    if let (Some(ui), Some(d)) = (ui_weak.upgrade(), duration) {
        // Deleting the last clip used to leave the lane and scrollbar at the
        // old length.
        ui.set_timeline_duration(d.map(|d| d.as_secs_f32()).unwrap_or(0.0));
    }
    // Keep selection consistent: the removed clip is gone; rows above it shift down.
    let sel = sel_idx.get();
    if sel == i {
        sel_idx.set(-1);
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_inspector_name("".into());
        }
    } else if sel > i {
        sel_idx.set(sel - 1);
    }
}

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
            // Without this, every bin click was a silent no-op for the rest of
            // the session (the windowed build has no stderr).
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_video_engine_down(true);
                show_error(
                    &ui,
                    "Video engine unavailable",
                    format!(
                        "{e:#}\n\nThe video editor needs the bundled GStreamer runtime next to kuvatin.exe; reinstalling Kuvatin restores it."
                    ),
                );
            }
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
    let is_img = kuvatin_core::format::is_input_extension(&ext);
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
        Err(e) => {
            if let Some(ui) = ui_weak.upgrade() {
                show_error(&ui, "Could not add clip", format!("{e:#}"));
            }
        }
    }
}

/// Append an image sequence to the timeline as a clip. The `imagesequence://`
/// URI carries an intrinsic duration (frames ÷ fps), so GES places it like a
/// video; `kind: 2` gives it the sequence styling in the timeline.
fn add_sequence_to_timeline(
    spec: &kuvatin_video::SequenceSpec,
    clip_name: &str,
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
    let added = spec
        .uri()
        // Sequences are footage, not overlays: the base video track (GES
        // composites lower layer indices on top, so videos live on 1).
        .and_then(|uri| project.append_clip_uri(&uri, 1, None));
    match added {
        Ok(info) => {
            tl_clips.push(TimelineClip {
                id: info.id.0.clone().into(),
                track: info.track as i32,
                start: info.start.as_secs_f32(),
                duration: info.duration.as_secs_f32(),
                inpoint: 0.0,
                name: clip_name.into(),
                kind: 2,
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
        Err(e) => {
            if let Some(ui) = ui_weak.upgrade() {
                show_error(&ui, "Could not add sequence", format!("{e:#}"));
            }
        }
    }
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

    // Initialize the preset-names model and the format/quality controls from the
    // first preset so they reflect (and can override) what will actually be applied.
    refresh_presets(&ui, &store.lock().unwrap(), 0);

    // If the preset file was corrupt/partial, load_or_init recovered to built-ins
    // and left a note — surface it instead of pretending nothing happened.
    if let Some(warning) = store.lock().unwrap().last_load_warning.clone() {
        show_error(&ui, "Presets reset", warning);
    }

    let files: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(collect_images(&initial_paths)));

    // Per-file crops in ABSOLUTE pixels (x, y, w, h) keyed by input path. Files
    // not present here are converted with the base job (no crop override).

    let crops: Arc<Mutex<CropMap>> = Arc::new(Mutex::new(HashMap::new()));

    // Path-keyed thumbnail cache (see `ThumbCache`): lets row rebuilds restore
    // thumbnails and keeps re-adds from re-decoding files already seen.
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
    // The in-progress crop edit: the file being cropped and its ORIGINAL (w, h).
    let edit: Arc<Mutex<Option<(PathBuf, u32, u32)>>> = Arc::new(Mutex::new(None));

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
    // (generation, path, thumbnail, Some(error) if discovery failed → not addable).
    type ImportItem = (u64, PathBuf, Option<kuvatin_video::Frame>, Option<String>);
    let (import_tx, import_rx) = std::sync::mpsc::channel::<(u64, PathBuf)>();
    let import_ready: Arc<Mutex<std::collections::VecDeque<ImportItem>>> =
        Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let import_q = Rc::new(ImportQueue {
        tx: import_tx,
        total: Cell::new(0),
        done: Cell::new(0),
        gen: Arc::new(AtomicU64::new(0)),
        seen: RefCell::new(SeenSet::default()),
    });
    {
        let ready = import_ready.clone();
        let gen = import_q.gen.clone();
        std::thread::spawn(move || {
            for (item_gen, path) in import_rx {
                // A cancelled batch drains without the expensive discovery.
                if item_gen != gen.load(Ordering::Relaxed) {
                    continue;
                }
                let warm = kuvatin_video::warm_asset(&path);
                let err = warm.err().map(|e| e.to_string());
                let thumb = if err.is_none() {
                    kuvatin_video::thumbnail(&path, 160)
                } else {
                    None
                };
                ready
                    .lock()
                    .unwrap()
                    .push_back((item_gen, path, thumb, err));
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
        std::mem::forget(setup_timer);

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
                    import_q.enqueue(&ui, dropped);
                } else {
                    // Ignore image drops while a batch is running — adding rows
                    // would desync the progress callback's snapshot indices.
                    if !ui.get_running() {
                        add_paths(dropped, &files, &rows, &crops, &thumbs, &ui_weak);
                    }
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
        // True between "Export…" and the deferred `begin_render` (one tick
        // later, so the modal paints before the engine blocks the UI thread).
        let export_pending = Rc::new(std::cell::Cell::new(false));
        // The output path of the running export, so Cancel/failure can delete
        // the partial file.
        let export_path: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
        // Latest scrub target (seconds, frame-accurate?) awaiting the UI tick:
        // one seek per tick instead of one per pointer event, and an ACCURATE
        // landing on release so the picture matches the playhead.
        let pending_seek: Rc<Cell<Option<(f32, bool)>>> = Rc::new(Cell::new(None));

        // Image-sequence import state. `pending_seq` holds the detected sequence
        // while its confirm dialog is open; `seq_by_path` maps a media-bin entry
        // (keyed by the sequence's FIRST frame) to its import-ready spec so a
        // bin click re-adds the sequence, not a single still.
        let pending_seq: Rc<RefCell<Option<kuvatin_video::SequenceSpec>>> =
            Rc::new(RefCell::new(None));
        let seq_by_path: Rc<RefCell<HashMap<PathBuf, kuvatin_video::SequenceSpec>>> =
            Rc::new(RefCell::new(HashMap::new()));
        /// Worker → UI handoff for a finished sequence import.
        struct SeqResult {
            /// The original sequence's first frame (identity in bin/"seen").
            first: PathBuf,
            /// Media-bin label (original pattern + frame count).
            bin_name: String,
            /// Timeline-clip label (original pattern).
            clip_name: String,
            /// The import-ready spec (post-EXR-conversion); None on failure.
            spec: Option<kuvatin_video::SequenceSpec>,
            thumb: Option<kuvatin_video::Frame>,
            err: Option<String>,
            /// The user cancelled mid-import — drop silently, no error dialog.
            cancelled: bool,
        }
        let seq_ready: Arc<Mutex<std::collections::VecDeque<SeqResult>>> =
            Arc::new(Mutex::new(std::collections::VecDeque::new()));
        // EXR-conversion progress (done, total) — the import timer mirrors it
        // into the import modal while a sequence import is in flight.
        let seq_progress: Arc<(std::sync::atomic::AtomicU32, std::sync::atomic::AtomicU32)> =
            Arc::new(Default::default());
        let seq_active = Rc::new(std::cell::Cell::new(false));
        let seq_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));

        // Open media via the file dialog → the same import queue as drag-and-drop.
        {
            let ui_weak = ui_weak.clone();
            let import_q = import_q.clone();
            ui.on_video_open(move || {
                // Videos plus every image input (stills become overlays).
                let mut media: Vec<&str> = kuvatin_video::VIDEO_EXTENSIONS.to_vec();
                media.extend_from_slice(kuvatin_core::format::INPUT_EXTENSIONS);
                let Some(paths) = rfd::FileDialog::new()
                    .add_filter("Media", &media)
                    .pick_files()
                else {
                    return;
                };
                if let Some(ui) = ui_weak.upgrade() {
                    import_q.enqueue(&ui, paths);
                }
            });
        }

        // Import an image sequence: pick its FIRST frame, detect the numbered
        // run in the same directory, and open the confirm dialog (frame count +
        // frame rate) — the actual import happens on seq-confirm.
        {
            let ui_weak = ui_weak.clone();
            let pending_seq = pending_seq.clone();
            ui.on_video_open_sequence(move || {
                let Some(ui) = ui_weak.upgrade() else {
                    return;
                };
                let Some(path) = rfd::FileDialog::new()
                    .add_filter("First frame of a sequence", kuvatin_video::FRAME_EXTENSIONS)
                    .pick_file()
                else {
                    return;
                };
                match kuvatin_video::detect_sequence(&path) {
                    Ok(spec) => {
                        ui.set_seq_range(
                            format!(
                                "{} → {}",
                                spec.frame_file_name(spec.start),
                                spec.frame_file_name(spec.start + spec.count - 1)
                            )
                            .into(),
                        );
                        ui.set_seq_count(spec.count.min(i32::MAX as u64) as i32);
                        *pending_seq.borrow_mut() = Some(spec);
                        ui.set_seq_config(true);
                    }
                    Err(e) => show_error(&ui, "Not an image sequence", format!("{e:#}")),
                }
            });
        }

        // Sequence confirmed: import on a worker thread — EXR frames convert to
        // PNG first (GStreamer has no EXR decoder; progress feeds the import
        // modal), then the `imagesequence://` asset is discovered and thumbed.
        // The import timer drains the result onto the bin + timeline.
        {
            let ui_weak = ui_weak.clone();
            let pending_seq = pending_seq.clone();
            let import_q = import_q.clone();
            let seq_ready = seq_ready.clone();
            let seq_progress = seq_progress.clone();
            let seq_active = seq_active.clone();
            let seq_cancel = seq_cancel.clone();
            ui.on_seq_confirm(move || {
                let Some(ui) = ui_weak.upgrade() else {
                    return;
                };
                let Some(mut spec) = pending_seq.borrow_mut().take() else {
                    return;
                };
                spec.fps = ui.get_seq_fps().clamp(1, 240) as u32;
                let first = spec.first_path();
                if !import_q.seen.borrow_mut().insert(&first) {
                    show_error(
                        &ui,
                        "Already imported",
                        "That sequence is already in the media bin.",
                    );
                    return;
                }
                seq_cancel.store(false, Ordering::Relaxed);
                seq_progress.0.store(0, Ordering::Relaxed);
                seq_progress.1.store(0, Ordering::Relaxed);
                seq_active.set(true);
                ui.set_importing(true);
                ui.set_import_done(0);
                ui.set_import_total(1);
                let bin_name = format!("{} · {}f", spec.pattern_name(), spec.count);
                let clip_name = spec.pattern_name();
                let seq_ready = seq_ready.clone();
                let seq_progress = seq_progress.clone();
                let seq_cancel = seq_cancel.clone();
                std::thread::spawn(move || {
                    type Ready = (kuvatin_video::SequenceSpec, Option<kuvatin_video::Frame>);
                    let result = (|| -> std::result::Result<Ready, String> {
                        let spec = if spec.is_exr() {
                            kuvatin_video::convert_exr_sequence(
                                &spec,
                                |done, total| {
                                    seq_progress
                                        .0
                                        .store(done.min(u32::MAX as u64) as u32, Ordering::Relaxed);
                                    seq_progress.1.store(
                                        total.min(u32::MAX as u64) as u32,
                                        Ordering::Relaxed,
                                    );
                                },
                                &seq_cancel,
                            )
                            .map_err(|e| format!("{e:#}"))?
                        } else {
                            spec
                        };
                        let uri = spec.uri().map_err(|e| format!("{e:#}"))?;
                        kuvatin_video::warm_asset_uri(&uri).map_err(|e| format!("{e:#}"))?;
                        let thumb = kuvatin_video::thumbnail_uri(&uri, 160);
                        Ok((spec, thumb))
                    })();
                    let cancelled = seq_cancel.load(Ordering::Relaxed);
                    let item = match result {
                        Ok((spec, thumb)) => SeqResult {
                            first,
                            bin_name,
                            clip_name,
                            spec: Some(spec),
                            thumb,
                            err: None,
                            cancelled,
                        },
                        Err(e) => SeqResult {
                            first,
                            bin_name,
                            clip_name,
                            spec: None,
                            thumb: None,
                            err: Some(e),
                            cancelled,
                        },
                    };
                    seq_ready.lock().unwrap().push_back(item);
                });
            });
        }

        // Cancel an in-flight import: stop the modal and discard the queue. The
        // worker keeps draining in the background; everything of the old
        // generation — including the file whose discovery was mid-flight — is
        // dropped by the drain, so nothing sneaks into the bin after Cancel.
        {
            let ui_weak = ui_weak.clone();
            let import_q = import_q.clone();
            let import_ready = import_ready.clone();
            let bin_paths = bin_paths.clone();
            let seq_cancel = seq_cancel.clone();
            let seq_ready = seq_ready.clone();
            let seq_active = seq_active.clone();
            ui.on_import_cancel(move || {
                import_ready.lock().unwrap().clear();
                // Abort a running sequence import too (the EXR conversion
                // checks the flag per frame and cleans up its partial cache).
                seq_cancel.store(true, Ordering::Relaxed);
                seq_ready.lock().unwrap().clear();
                seq_active.set(false);
                import_q.cancel(bin_paths.borrow().iter());
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_importing(false);
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
            let import_q = import_q.clone();
            let seq_ready = seq_ready.clone();
            let seq_progress = seq_progress.clone();
            let seq_active = seq_active.clone();
            let seq_by_path = seq_by_path.clone();
            // Names of files that failed discovery this import, for one summary.
            let import_failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
            let timer = slint::Timer::default();
            timer.start(
                slint::TimerMode::Repeated,
                std::time::Duration::from_millis(60),
                move || {
                    loop {
                        let next = ready.lock().unwrap().pop_front();
                        let Some((item_gen, path, thumb_frame, err)) = next else {
                            break;
                        };
                        if item_gen != import_q.current_gen() {
                            continue; // cancelled batch: neither counted nor added
                        }
                        import_q.done.set(import_q.done.get() + 1);
                        if let Some(_e) = err {
                            // Unreadable/undiscoverable → don't add a dead bin entry.
                            import_q.seen.borrow_mut().remove(&path);
                            import_failures.borrow_mut().push(
                                path.file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| path.display().to_string()),
                            );
                            continue;
                        }
                        let thumb = frame_to_image(thumb_frame);
                        add_to_bin(&assets, &path, thumb.clone());
                        bin_paths.borrow_mut().push(path.clone());
                        // Only the first file (when the timeline is empty) goes on
                        // the timeline; the rest wait in the bin for the user.
                        if tl_clips.row_count() == 0 {
                            add_to_timeline(&path, &ui_weak, &project_slot, &tl_clips, thumb);
                        }
                    }
                    // Finished sequence imports (at most one in flight): land the
                    // clip on the bin + timeline, or surface the failure.
                    loop {
                        let item = seq_ready.lock().unwrap().pop_front();
                        let Some(res) = item else {
                            break;
                        };
                        seq_active.set(false);
                        let Some(ui) = ui_weak.upgrade() else {
                            continue;
                        };
                        // Close the modal unless a file import still drives it.
                        if import_q.total.get() == 0 {
                            ui.set_importing(false);
                        }
                        match (res.spec, res.err, res.cancelled) {
                            (_, _, true) => {
                                // User cancelled: forget it silently, allow re-import.
                                import_q.seen.borrow_mut().remove(&res.first);
                            }
                            (Some(spec), None, false) => {
                                let thumb = frame_to_image(res.thumb);
                                assets.push(VideoAsset {
                                    name: res.bin_name.into(),
                                    thumb: thumb.clone(),
                                });
                                bin_paths.borrow_mut().push(res.first.clone());
                                seq_by_path.borrow_mut().insert(res.first, spec.clone());
                                add_sequence_to_timeline(
                                    &spec,
                                    &res.clip_name,
                                    &ui_weak,
                                    &project_slot,
                                    &tl_clips,
                                    thumb,
                                );
                            }
                            (_, err, false) => {
                                import_q.seen.borrow_mut().remove(&res.first);
                                show_error(
                                    &ui,
                                    "Could not import sequence",
                                    err.unwrap_or_else(|| "unknown error".into()),
                                );
                            }
                        }
                    }
                    if let Some(ui) = ui_weak.upgrade() {
                        if import_q.total.get() > 0 {
                            ui.set_import_done(import_q.done.get() as i32);
                            if import_q.done.get() >= import_q.total.get() {
                                // A sequence import still in flight keeps the
                                // modal (it shows that one's progress next).
                                if !seq_active.get() {
                                    ui.set_importing(false);
                                }
                                import_q.total.set(0);
                                import_q.done.set(0);
                                let failures = std::mem::take(&mut *import_failures.borrow_mut());
                                if !failures.is_empty() {
                                    show_error(
                                        &ui,
                                        "Some files could not be imported",
                                        format!(
                                            "{} file(s) couldn't be read as media:\n{}",
                                            failures.len(),
                                            name_list(&failures)
                                        ),
                                    );
                                }
                            }
                        } else if seq_active.get() {
                            // Mirror the EXR-conversion progress (file imports
                            // take display precedence over it).
                            let total = seq_progress.1.load(Ordering::Relaxed);
                            if total > 0 {
                                ui.set_import_done(seq_progress.0.load(Ordering::Relaxed) as i32);
                                ui.set_import_total(total as i32);
                            }
                        }
                    }
                },
            );
            std::mem::forget(timer);
        }

        // Media-bin item "add": append that file to the timeline as a new clip.
        {
            let ui_weak = ui_weak.clone();
            let project_slot = project_slot.clone();
            let tl_clips = tl_clips.clone();
            let assets = assets.clone();
            let bin_paths = bin_paths.clone();
            let seq_by_path = seq_by_path.clone();
            ui.on_video_add(move |i| {
                let Some(path) = bin_paths.borrow().get(i as usize).cloned() else {
                    return;
                };
                let thumb = assets
                    .row_data(i as usize)
                    .map(|a| a.thumb)
                    .unwrap_or_default();
                // A bin entry backed by an image sequence re-adds the whole
                // sequence, not the single first-frame file.
                if let Some(spec) = seq_by_path.borrow().get(&path).cloned() {
                    let name = spec.pattern_name();
                    add_sequence_to_timeline(
                        &spec,
                        &name,
                        &ui_weak,
                        &project_slot,
                        &tl_clips,
                        thumb,
                    );
                    return;
                }
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
                let mut sel_kind = 0;
                for idx in 0..tl_clips.row_count() {
                    if let Some(mut c) = tl_clips.row_data(idx) {
                        c.selected = idx as i32 == i;
                        if c.selected {
                            name = c.name.clone();
                            sel_id = c.id.clone();
                            sel_kind = c.kind;
                        }
                        tl_clips.set_row_data(idx, c);
                    }
                }
                ui.set_inspector_name(name);
                // Only real videos carry audio — stills and image sequences don't.
                ui.set_insp_has_audio(sel_kind == 0);
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
                // Visible label rows count too — a click-added track may not have
                // a GES layer yet, and layer() creates any intermediates on
                // demand, so dropping onto ANY visible row lands exactly there.
                if delta_rows != 0 {
                    let count = (p.track_count() as i32).max(tracks.row_count() as i32);
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

        // Click "+ New track": append an empty visual track row. The GES layer is
        // created lazily when a clip first lands there (exactly how the built-in
        // empty "Track 2" works), so an added-but-empty track can never be taken
        // away by remove_clip's trailing-layer pruning. Capped so the timeline
        // band can't grow to eat the whole viewer.
        {
            let tracks = video_tracks.clone();
            ui.on_add_track(move || {
                if tracks.row_count() >= 8 {
                    return;
                }
                let n = tracks.row_count() + 1;
                tracks.push(SharedString::from(format!("Track {n}")));
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
                let geom = project_slot.borrow_mut().as_mut().and_then(|p| {
                    p.trim_clip(
                        &kuvatin_video::ClipId(row.id.to_string()),
                        edge,
                        delta as f64,
                    )
                });
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
                    // Play from a finished timeline restarts it instead of
                    // pausing again on the same last frame.
                    if let (Some(p), Some(d)) = (project.position(), project.duration()) {
                        if p + std::time::Duration::from_millis(120) >= d {
                            let _ = project.seek(std::time::Duration::ZERO);
                        }
                    }
                    let _ = project.play();
                    ui.set_video_playing(true);
                }
            });
        }

        // Seek to a fraction of the timeline (transport scrubber). The playhead
        // moves at once; the pipeline seek is coalesced onto the UI tick.
        {
            let ui_weak = ui_weak.clone();
            let project_slot = project_slot.clone();
            let pending_seek = pending_seek.clone();
            ui.on_video_seek(move |frac| {
                let dur = project_slot
                    .borrow()
                    .as_ref()
                    .and_then(|p| p.duration())
                    .map(|d| d.as_secs_f32())
                    .unwrap_or(0.0);
                if dur <= 0.0 {
                    return;
                }
                let secs = dur * frac.clamp(0.0, 1.0);
                pending_seek.set(Some((secs, false)));
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_playhead(secs);
                    ui.set_video_position(frac.clamp(0.0, 1.0));
                }
            });
        }

        // Scrub: drag/click the timeline lane to move the playhead + seek.
        {
            let ui_weak = ui_weak.clone();
            let project_slot = project_slot.clone();
            let pending_seek = pending_seek.clone();
            ui.on_timeline_seek_time(move |secs| {
                let dur = project_slot
                    .borrow()
                    .as_ref()
                    .and_then(|p| p.duration())
                    .map(|d| d.as_secs_f32())
                    .unwrap_or(0.0);
                let secs = if dur > 0.0 {
                    secs.clamp(0.0, dur)
                } else {
                    secs.max(0.0)
                };
                pending_seek.set(Some((secs, false)));
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_playhead(secs);
                    if dur > 0.0 {
                        ui.set_video_position((secs / dur).clamp(0.0, 1.0));
                    }
                }
            });
        }

        // Scrub released: land frame-accurately where the playhead shows, so
        // the paused picture can't sit a keyframe interval away from it.
        {
            let ui_weak = ui_weak.clone();
            let pending_seek = pending_seek.clone();
            ui.on_seek_done(move || {
                if let Some(ui) = ui_weak.upgrade() {
                    pending_seek.set(Some((ui.get_playhead(), true)));
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
            let export_pending = export_pending.clone();
            let export_path = export_path.clone();
            ui.on_video_export(move || {
                if export_active.get() || export_pending.get() {
                    return;
                }
                let Some(ui) = ui_weak.upgrade() else {
                    return;
                };
                if project_slot.borrow().is_none() {
                    show_error(
                        &ui,
                        "Nothing to export",
                        "Add a clip to the timeline first.",
                    );
                    return;
                }
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
                    fps: ui.get_export_fps().clamp(1, 240) as u32,
                    bitrate_kbps: ui.get_export_bitrate().max(0) as u32,
                };
                let Some(path) = rfd::FileDialog::new()
                    .add_filter(
                        if ext == "mp4" {
                            "MP4 video"
                        } else {
                            "WebM video"
                        },
                        &[ext],
                    )
                    .set_file_name(default_name)
                    .save_file()
                else {
                    return;
                };
                // Show the modal NOW and start the render on the next tick:
                // begin_render tears the preview down and waits for NULL (up
                // to 3 s) on this thread, which used to freeze the window with
                // no feedback before anything appeared.
                *export_path.borrow_mut() = Some(path.clone());
                export_pending.set(true);
                ui.set_exporting(true);
                ui.set_export_progress(0.0);
                ui.set_export_status("Starting\u{2026}".into());
                let ui_weak = ui_weak.clone();
                let project_slot = project_slot.clone();
                let export_active = export_active.clone();
                let export_pending = export_pending.clone();
                let export_path = export_path.clone();
                slint::Timer::single_shot(std::time::Duration::from_millis(60), move || {
                    if !export_pending.get() {
                        return; // cancelled before it started
                    }
                    export_pending.set(false);
                    let result = project_slot
                        .borrow()
                        .as_ref()
                        .map(|p| p.begin_render(&path, settings));
                    match result {
                        Some(Ok(())) => export_active.set(true),
                        Some(Err(e)) => {
                            let _ = std::fs::remove_file(&path);
                            export_path.borrow_mut().take();
                            if let Some(ui) = ui_weak.upgrade() {
                                ui.set_exporting(false);
                                show_error(&ui, "Export failed to start", e.to_string());
                            }
                        }
                        None => {
                            export_path.borrow_mut().take();
                            if let Some(ui) = ui_weak.upgrade() {
                                ui.set_exporting(false);
                            }
                        }
                    }
                });
            });
        }

        // Cancel a running export: tear the render down and delete the partial
        // file (no EOS wait — the file is discarded anyway).
        {
            let ui_weak = ui_weak.clone();
            let project_slot = project_slot.clone();
            let export_active = export_active.clone();
            let export_pending = export_pending.clone();
            let export_path = export_path.clone();
            ui.on_export_cancel(move || {
                if export_pending.get() {
                    // Not started yet: the deferred start sees the flag and bails.
                    export_pending.set(false);
                    export_path.borrow_mut().take();
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_exporting(false);
                    }
                    return;
                }
                if !export_active.get() {
                    return;
                }
                let path = export_path.borrow_mut().take();
                if let (Some(p), Some(path)) = (project_slot.borrow().as_ref(), path) {
                    let _ = p.cancel_render(&path, true);
                }
                export_active.set(false);
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_exporting(false);
                }
            });
        }

        // Export progress: poll the render; finish or fail restores the preview.
        {
            let ui_weak = ui_weak.clone();
            let project_slot = project_slot.clone();
            let export_active = export_active.clone();
            let export_path = export_path.clone();
            // Watchdog: if progress hasn't advanced for this many ticks (200ms
            // each → 20s), tell the user the render looks stuck (Cancel is right
            // there). Some pipeline stalls never post EOS/Error. The baseline
            // is the last fraction that actually ADVANCED — comparing against
            // the previous tick called a slow-but-healthy render stuck.
            let stall = Rc::new(std::cell::Cell::new((0.0f32, 0u32)));
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
                            let (last, ticks) = stall.get();
                            let (last, ticks) = if f > last { (f, 0) } else { (last, ticks + 1) };
                            stall.set((last, ticks));
                            if let Some(ui) = ui_weak.upgrade() {
                                ui.set_export_progress(f);
                                ui.set_export_status(if ticks >= 100 {
                                    "Export appears stuck — you can cancel.".into()
                                } else {
                                    slint::format!("{:.0}%", f * 100.0)
                                });
                            }
                        }
                        kuvatin_video::RenderStatus::Done => {
                            // A preview that fails to come back is otherwise a
                            // silently black viewer for the rest of the session.
                            if let Err(e) = p.end_render() {
                                if let Some(ui) = ui_weak.upgrade() {
                                    show_error(&ui, "Preview could not be restored", e.to_string());
                                }
                            }
                            drop(slot);
                            export_active.set(false);
                            export_path.borrow_mut().take();
                            stall.set((0.0, 0));
                            if let Some(ui) = ui_weak.upgrade() {
                                ui.set_exporting(false);
                            }
                        }
                        kuvatin_video::RenderStatus::Failed(e) => {
                            if let Err(restore) = p.end_render() {
                                if let Some(ui) = ui_weak.upgrade() {
                                    show_error(
                                        &ui,
                                        "Preview could not be restored",
                                        restore.to_string(),
                                    );
                                }
                            }
                            drop(slot);
                            export_active.set(false);
                            stall.set((0.0, 0));
                            // Delete the truncated output and show the real reason.
                            if let Some(path) = export_path.borrow_mut().take() {
                                let _ = std::fs::remove_file(path);
                            }
                            if let Some(ui) = ui_weak.upgrade() {
                                ui.set_exporting(false);
                                show_error(&ui, "Export failed", e);
                            }
                        }
                    }
                },
            );
            std::mem::forget(timer);
        }

        // Delete a timeline clip: the × on the selected clip.
        {
            let ui_weak = ui_weak.clone();
            let project_slot = project_slot.clone();
            let tl_clips = video_tl.clone();
            let sel_idx = sel_idx.clone();
            ui.on_timeline_clip_removed(move |i| {
                remove_timeline_clip(i, &ui_weak, &project_slot, &tl_clips, &sel_idx);
            });
        }
        // Delete key → remove whatever clip is selected.
        {
            let ui_weak = ui_weak.clone();
            let project_slot = project_slot.clone();
            let tl_clips = video_tl.clone();
            let sel_idx = sel_idx.clone();
            ui.on_delete_selected_clip(move || {
                remove_timeline_clip(sel_idx.get(), &ui_weak, &project_slot, &tl_clips, &sel_idx);
            });
        }
        // Remove a media-bin entry (× on hover). Keeps bin_paths in lockstep and
        // frees the path from "seen" so it can be re-imported later.
        {
            let video_assets = video_assets.clone();
            let bin_paths = bin_paths.clone();
            let import_q = import_q.clone();
            let seq_by_path = seq_by_path.clone();
            ui.on_video_bin_removed(move |i| {
                if i < 0 {
                    return;
                }
                let i = i as usize;
                if i < video_assets.row_count() {
                    video_assets.remove(i);
                }
                let mut bp = bin_paths.borrow_mut();
                if i < bp.len() {
                    let removed = bp.remove(i);
                    import_q.seen.borrow_mut().remove(&removed);
                    seq_by_path.borrow_mut().remove(&removed);
                }
            });
        }
        // Mode switch: pause the video project when leaving Videos mode so audio
        // stops and the pipeline goes quiet (the playhead timer also gates on mode).
        {
            let ui_weak = ui_weak.clone();
            let project_slot = project_slot.clone();
            ui.on_video_mode_changed(move |mode| {
                if mode != 1 {
                    if let Some(p) = project_slot.borrow().as_ref() {
                        let _ = p.pause();
                    }
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_video_playing(false);
                    }
                }
            });
        }

        // Advance the playhead + scrubber + time, and apply coalesced edits.
        {
            let ui_weak = ui_weak.clone();
            let project_slot = project_slot.clone();
            let pending_xform = pending_xform.clone();
            let pending_seek = pending_seek.clone();
            let export_active = export_active.clone();
            let export_pending = export_pending.clone();
            let timer = slint::Timer::default();
            timer.start(
                slint::TimerMode::Repeated,
                std::time::Duration::from_millis(100),
                move || {
                    // Never touch the pipeline while a render is in progress
                    // (or about to start).
                    if export_active.get() || export_pending.get() {
                        return;
                    }
                    let Some(ui) = ui_weak.upgrade() else {
                        return;
                    };
                    // Idle when not in Videos mode — nothing to preview.
                    if ui.get_app_mode() != 1 {
                        return;
                    }
                    let mut slot = project_slot.borrow_mut();
                    let Some(project) = slot.as_mut() else {
                        return;
                    };
                    // Apply the latest inspector transform (if any) then repaint,
                    // both coalesced to one commit + one seek per tick.
                    if let Some((id, l)) = pending_xform.borrow_mut().take() {
                        project.set_clip_layout(&kuvatin_video::ClipId(id), l);
                    }
                    // Scrub target: one (keyframe) seek per tick during a drag,
                    // a frame-accurate one on release.
                    if let Some((secs, accurate)) = pending_seek.take() {
                        let t = std::time::Duration::from_secs_f32(secs.max(0.0));
                        let _ = if accurate {
                            project.seek_accurate(t)
                        } else {
                            project.seek(t)
                        };
                    }
                    project.refresh_preview();
                    // Surface a dead preview pipeline instead of freezing silently.
                    if let Some(err) = project.poll_preview_error() {
                        let _ = project.pause();
                        ui.set_video_playing(false);
                        show_error(&ui, "Playback error", err);
                        return;
                    }
                    let pos = project.position().unwrap_or_default();
                    let dur = project.duration().unwrap_or_default();
                    // End of the timeline: loop when repeat is on, otherwise
                    // reflect the stop — the pause icon used to stick forever.
                    let at_end =
                        dur.as_secs_f32() > 0.1 && pos.as_secs_f32() + 0.12 >= dur.as_secs_f32();
                    if at_end && ui.get_video_playing() {
                        if ui.get_video_repeat() {
                            let _ = project.seek(std::time::Duration::ZERO);
                        } else {
                            let _ = project.pause();
                            ui.set_video_playing(false);
                        }
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
/// and sync every control to the selected preset's job.
fn refresh_presets(ui: &AppWindow, store: &PresetStore, select: usize) {
    let names: Vec<SharedString> = store
        .presets
        .iter()
        .map(|p| p.name.clone().into())
        .collect();
    ui.set_preset_names(ModelRc::new(VecModel::from(names)));
    let idx = select.min(store.presets.len().saturating_sub(1));
    ui.set_current_preset(idx as i32);
    if let Some(p) = store.presets.get(idx) {
        sync_controls(ui, &p.job);
    }
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
fn current_job(ui: &AppWindow, store: &PresetStore) -> Job {
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

/// A decoded thumbnail, kept as raw RGBA so it is `Send` (a `slint::Image` is
/// not) and can live in the shared cache. Rebuilt into an `Image` on the UI
/// thread when a row is (re)created.
#[derive(Clone)]
struct ThumbData {
    rgba: Vec<u8>,
    w: u32,
    h: u32,
    dims: String,
}

/// Path-keyed cache of decoded thumbnails. Populated once per file by the
/// thumbnail worker; read when rebuilding rows so an add no longer re-decodes
/// every file already in the list (and existing thumbnails survive the rebuild).
type ThumbCache = Arc<Mutex<HashMap<PathBuf, ThumbData>>>;

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

/// Add `picked` paths to the queue: filter/expand to image files, merge with the
/// existing set (sorted + deduped), update the visible rows, and kick off
/// thumbnail decoding. The selection follows its FILE across the re-sort (the
/// highlighted row used to become a different file than the viewer and the
/// crop state). Shared by the Add files… button and the drag-and-drop drain
/// timer so both paths behave identically.
fn add_paths(
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

    /// Different spellings of one file are one "seen" entry (Windows paths
    /// are case-insensitive; a `..` segment is the same file too).
    #[cfg(windows)]
    #[test]
    fn seen_set_canonicalises_case_and_dots() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Clip.mp4");
        std::fs::write(&file, b"x").unwrap();
        let mut seen = SeenSet::default();
        assert!(seen.insert(&file));
        let lower = dir.path().join("clip.MP4");
        assert!(seen.contains(&lower), "case-insensitive");
        let dotted = dir.path().join("sub").join("..").join("Clip.mp4");
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        assert!(!seen.insert(&dotted), "`..` spelling is the same file");
        seen.remove(&lower);
        assert!(!seen.contains(&file));
        // A path that doesn't exist still works (by its literal form).
        let ghost = dir.path().join("missing.mp4");
        assert!(seen.insert(&ghost) && seen.contains(&ghost));
    }

    #[test]
    fn dialog_name_lists_are_capped() {
        let names: Vec<String> = (1..=12).map(|i| format!("f{i}.png")).collect();
        let s = name_list(&names);
        assert_eq!(s.lines().count(), 11);
        assert!(s.ends_with("\u{2026}and 2 more"));
        assert_eq!(name_list(&names[..3]), "f1.png\nf2.png\nf3.png");
    }
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
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };
    use windows::Win32::System::Ole::RevokeDragDrop;
    use windows::Win32::UI::HiDpi::GetDpiForWindow;
    use windows::Win32::UI::Shell::{
        DefSubclassProc, DragAcceptFiles, DragFinish, DragQueryFileW, SetWindowSubclass, HDROP,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetWindowRect, IsZoomed, PostMessageW, ShowWindow, HTBOTTOM, HTBOTTOMLEFT,
        HTBOTTOMRIGHT, HTCAPTION, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, SW_MAXIMIZE,
        SW_MINIMIZE, SW_RESTORE, WM_CLOSE, WM_DROPFILES,
    };

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
