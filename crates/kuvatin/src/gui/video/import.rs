//! Media import: files and folders dropped or opened are discovered on a
//! worker thread (warming the GES asset cache) and drained onto the bin +
//! timeline by a UI timer; image sequences take a confirm dialog and an
//! optional EXR conversion first. Also the media bin's add/remove.

use super::{add_sequence_to_timeline, add_to_bin, add_to_timeline, frame_to_image, VideoState};
use crate::collect::collect_media;
use crate::gui::{name_list, show_error, AppWindow, VideoAsset};
use slint::ComponentHandle;
use slint::Model;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// (generation, path, thumbnail, Some(error) if discovery failed → not addable).
type ImportItem = (u64, PathBuf, Option<kuvatin_video::Frame>, Option<String>);

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

/// The import machinery: the queue (worker thread), its ready items, and the
/// image-sequence import in flight (at most one).
pub(crate) struct ImportState {
    pub(crate) q: Rc<ImportQueue>,
    ready: Arc<Mutex<VecDeque<ImportItem>>>,
    /// The detected sequence while its confirm dialog is open.
    pending_seq: Rc<RefCell<Option<kuvatin_video::SequenceSpec>>>,
    /// Media-bin entry (keyed by the sequence's FIRST frame) → its import-ready
    /// spec, so a bin click re-adds the sequence, not a single still.
    pub(super) seq_by_path: Rc<RefCell<HashMap<PathBuf, kuvatin_video::SequenceSpec>>>,
    seq_ready: Arc<Mutex<VecDeque<SeqResult>>>,
    /// EXR-conversion progress (done, total) — the import timer mirrors it
    /// into the import modal while a sequence import is in flight.
    seq_progress: Arc<(AtomicU32, AtomicU32)>,
    seq_active: Rc<Cell<bool>>,
    seq_cancel: Arc<AtomicBool>,
}

impl ImportState {
    /// Start the discovery worker: it pulls `(generation, path)` off the
    /// channel, warms the asset and thumbnails it off the UI thread, and
    /// parks the result for the drain timer.
    pub(crate) fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<(u64, PathBuf)>();
        let ready: Arc<Mutex<VecDeque<ImportItem>>> = Arc::new(Mutex::new(VecDeque::new()));
        let q = Rc::new(ImportQueue {
            tx,
            total: Cell::new(0),
            done: Cell::new(0),
            gen: Arc::new(AtomicU64::new(0)),
            seen: RefCell::new(SeenSet::default()),
            scans: Arc::new(Expansions::default()),
        });
        {
            let ready = ready.clone();
            let gen = q.gen.clone();
            std::thread::spawn(move || {
                for (item_gen, path) in rx {
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
        Self {
            q,
            ready,
            pending_seq: Rc::new(RefCell::new(None)),
            seq_by_path: Rc::new(RefCell::new(HashMap::new())),
            seq_ready: Arc::new(Mutex::new(VecDeque::new())),
            seq_progress: Arc::new(Default::default()),
            seq_active: Rc::new(Cell::new(false)),
            seq_cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// Wire the import callbacks, the drain timer and the media-bin actions.
pub(super) fn wire(
    ui: &AppWindow,
    st: &VideoState,
    im: &ImportState,
    timers: &mut Vec<slint::Timer>,
) {
    let ui_weak = ui.as_weak();
    let project_slot = &st.project;
    let assets = &st.assets;
    let video_assets = &st.assets;
    let bin_paths = &st.bin_paths;
    let tl_clips = &st.tl_clips;
    let import_q = &im.q;
    let import_ready = &im.ready;
    let pending_seq = &im.pending_seq;
    let seq_by_path = &im.seq_by_path;
    let seq_ready = &im.seq_ready;
    let seq_progress = &im.seq_progress;
    let seq_active = &im.seq_active;
    let seq_cancel = &im.seq_cancel;
    // Open media via the file dialog → the same import queue as drag-and-drop.
    {
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
            import_q.enqueue(paths);
        });
    }

    // Import an image sequence: pick its FIRST frame, detect the numbered
    // run in the same directory, and open the confirm dialog (frame count +
    // frame rate) — the actual import happens on seq-confirm.
    {
        let ui_weak = ui_weak.clone();
        let pending_seq = pending_seq.clone();
        // The timer that waits for a detection running off the UI thread. It
        // lives here so it survives the callback that starts it; a second pick
        // replaces it, abandoning the first detection's result.
        let detect_timer: Rc<RefCell<Option<slint::Timer>>> = Rc::new(RefCell::new(None));
        ui.on_video_open_sequence(move || {
            if ui_weak.upgrade().is_none() {
                return; // window already gone: don't open a dialog for it
            }
            let Some(path) = rfd::FileDialog::new()
                .add_filter("First frame of a sequence", kuvatin_video::FRAME_EXTENSIONS)
                .pick_file()
            else {
                return;
            };
            // Detecting a run stats every frame up to the limit, which on a
            // real render directory is thousands of files: off the UI thread,
            // or the window freezes with nothing to show for it.
            type Detected = Result<kuvatin_video::SequenceSpec, String>;
            let slot: Arc<Mutex<Option<Detected>>> = Arc::new(Mutex::new(None));
            let worker = slot.clone();
            let file = path.clone();
            let detect = move || {
                let found = kuvatin_video::detect_sequence(&file).map_err(|e| format!("{e:#}"));
                if let Ok(mut s) = worker.lock() {
                    *s = Some(found);
                }
            };
            if let Err(e) = std::thread::Builder::new()
                .name("kuvatin-seq-detect".into())
                .spawn(detect)
            {
                // No thread: detect here, as it always used to.
                crate::applog::log(&format!("sequence detect thread failed to start: {e}"));
                let found = kuvatin_video::detect_sequence(&path).map_err(|e| format!("{e:#}"));
                if let Ok(mut s) = slot.lock() {
                    *s = Some(found);
                }
            }
            let timer = slint::Timer::default();
            let ui_weak = ui_weak.clone();
            let pending_seq = pending_seq.clone();
            let holder = detect_timer.clone();
            timer.start(
                slint::TimerMode::Repeated,
                std::time::Duration::from_millis(60),
                move || {
                    let Some(found) = slot.lock().ok().and_then(|mut s| s.take()) else {
                        return;
                    };
                    if let Some(t) = holder.borrow().as_ref() {
                        t.stop();
                    }
                    let Some(ui) = ui_weak.upgrade() else {
                        return;
                    };
                    match found {
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
                        Err(e) => show_error(&ui, "Not an image sequence", e),
                    }
                },
            );
            *detect_timer.borrow_mut() = Some(timer);
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
            // One conversion at a time. Confirming the dialog again mid-run
            // used to start a second worker sharing this one's progress
            // counters and cancel flag, and whichever finished first closed
            // the dialog over the other.
            if seq_active.get() {
                show_error(
                    &ui,
                    "A sequence is already being converted",
                    "Wait for the current import to finish, or cancel it, then import this one.",
                );
                return;
            }
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
                                seq_progress
                                    .1
                                    .store(total.min(u32::MAX as u64) as u32, Ordering::Relaxed);
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
                // Idle (no file import, no sequence import): nothing can
                // be waiting — two Cell reads instead of two mutex locks
                // sixteen times a second for the life of the window.
                if import_q.total.get() == 0 && !seq_active.get() && !import_q.scanning() {
                    return;
                }
                // Folder scans that finished off-thread: queue what they found.
                if let Some(ui) = ui_weak.upgrade() {
                    import_q.drain_scans(&ui);
                }
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
        timers.push(timer);
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
                add_sequence_to_timeline(&spec, &name, &ui_weak, &project_slot, &tl_clips, thumb);
                return;
            }
            add_to_timeline(&path, &ui_weak, &project_slot, &tl_clips, thumb);
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

/// Folder expansions running off the UI thread, and the results waiting for
/// it. Expanding a dropped folder walks it; on a frames directory or a network
/// share that is thousands of stat calls, and it used to happen on the UI
/// thread inside the drop timer — the window froze before any dialog appeared.
///
/// The drain timer sleeps while nothing is in flight, so [`busy`](Self::busy)
/// must stay true from the moment a scan starts until its result has been
/// collected: `finish` therefore publishes the result BEFORE it decrements the
/// counter, and never the other way round.
#[derive(Default)]
struct Expansions {
    in_flight: AtomicU64,
    done: Mutex<Vec<(Vec<PathBuf>, Vec<PathBuf>)>>,
}

impl Expansions {
    fn start(&self) {
        self.in_flight.fetch_add(1, Ordering::SeqCst);
    }

    fn finish(&self, result: (Vec<PathBuf>, Vec<PathBuf>)) {
        if let Ok(mut done) = self.done.lock() {
            done.push(result);
        }
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }

    fn busy(&self) -> bool {
        self.in_flight.load(Ordering::SeqCst) > 0
            || self.done.lock().map(|d| !d.is_empty()).unwrap_or(false)
    }

    fn take(&self) -> Vec<(Vec<PathBuf>, Vec<PathBuf>)> {
        self.done
            .lock()
            .map(|mut d| std::mem::take(&mut *d))
            .unwrap_or_default()
    }
}

/// The media import queue. Dropped/opened paths go to a worker thread
/// (discovery off the UI thread); every batch is stamped with `gen`, and a
/// cancel bumps it, so the worker and the drain discard anything queued before
/// — no cancelled stragglers revived by the next drop, no double-counted files.
pub(crate) struct ImportQueue {
    tx: std::sync::mpsc::Sender<(u64, PathBuf)>,
    /// Files in the current import (drives the progress modal); 0 = idle.
    total: Cell<usize>,
    done: Cell<usize>,
    gen: Arc<AtomicU64>,
    seen: RefCell<SeenSet>,
    /// Folder expansion, off the UI thread (see [`Expansions`]).
    scans: Arc<Expansions>,
}

impl ImportQueue {
    pub(crate) fn current_gen(&self) -> u64 {
        self.gen.load(Ordering::Relaxed)
    }

    /// Expand folders and keep the media, off the UI thread; the drain timer
    /// picks the result up and queues it (see [`drain_scans`](Self::drain_scans)).
    ///
    /// The walk itself is the slow part — a dropped frames folder is thousands
    /// of directory entries — and it used to run inside the drop timer, which
    /// froze the window before anything appeared on screen.
    pub(crate) fn enqueue(&self, picked: Vec<PathBuf>) {
        let scans = self.scans.clone();
        scans.start();
        let mine = picked.clone();
        let spawned = std::thread::Builder::new()
            .name("kuvatin-import-scan".into())
            .spawn(move || scans.finish(collect_media(&mine)));
        if let Err(e) = spawned {
            // No thread to be had: walk it here rather than lose the files.
            // The window stalls, as it always used to; the count started
            // above is settled by the same shared counter.
            crate::applog::log(&format!("import scan thread failed to start: {e}"));
            self.scans.finish(collect_media(&picked));
        }
    }

    /// Collect finished scans and queue what they found. Called by the drain
    /// timer on the UI thread, where the bookkeeping lives.
    pub(crate) fn drain_scans(&self, ui: &AppWindow) {
        for (media, frames_only) in self.scans.take() {
            self.queue_collected(ui, media, frames_only);
        }
    }

    /// True while a scan is running or its result is waiting to be collected,
    /// so the drain timer stays awake for it.
    pub(crate) fn scanning(&self) -> bool {
        self.scans.busy()
    }

    /// Queue what a scan found and open the progress modal. Explicitly dropped
    /// EXR frames get a pointer at "Import sequence…" instead of a slow
    /// GES failure.
    fn queue_collected(&self, ui: &AppWindow, media: Vec<PathBuf>, frames_only: Vec<PathBuf>) {
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

    /// Forget what has been imported and start again from `in_bin` — what a
    /// reopened project put there. Without this, a source the new project uses
    /// would be refused as "already imported" from the project before it.
    pub(crate) fn reseed(&self, in_bin: &[PathBuf]) {
        self.seen.borrow_mut().reseed(in_bin.iter());
    }

    /// Abandon everything queued: later arrivals of the old generation are
    /// dropped, and "seen" is re-seeded from what actually reached the bin so
    /// the discarded files can be imported again.
    pub(crate) fn cancel<'a>(&self, in_bin: impl IntoIterator<Item = &'a PathBuf>) {
        self.gen.fetch_add(1, Ordering::Relaxed);
        self.total.set(0);
        self.done.set(0);
        self.seen.borrow_mut().reseed(in_bin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    /// The drain timer sleeps while nothing is in flight, so a scan that has
    /// started — or has finished but not been collected — must keep it awake.
    /// If `busy` ever dipped false with a result pending, the files would sit
    /// in the queue until something else woke the timer.
    #[test]
    fn a_scan_keeps_the_drain_awake_until_its_result_is_collected() {
        let ex = Expansions::default();
        assert!(!ex.busy());
        ex.start();
        assert!(ex.busy(), "a scan is running");
        ex.finish((paths(&["a.mp4"]), Vec::new()));
        assert!(ex.busy(), "the result is waiting to be collected");
        let taken = ex.take();
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].0, paths(&["a.mp4"]));
        assert!(!ex.busy(), "nothing left");
    }

    #[test]
    fn results_are_handed_over_exactly_once() {
        let ex = Expansions::default();
        ex.start();
        ex.finish((paths(&["a.mp4"]), paths(&["f.exr"])));
        assert_eq!(ex.take().len(), 1);
        assert!(ex.take().is_empty(), "a second drain finds nothing");
    }

    /// Two drops in quick succession: both scans are accounted for, and the
    /// drain gets both results whether or not they finished together.
    #[test]
    fn two_scans_are_both_accounted_for() {
        let ex = Expansions::default();
        ex.start();
        ex.start();
        ex.finish((paths(&["a.mp4"]), Vec::new()));
        assert!(ex.busy());
        assert_eq!(ex.take().len(), 1);
        assert!(ex.busy(), "the second scan is still running");
        ex.finish((paths(&["b.mp4"]), Vec::new()));
        assert_eq!(ex.take().len(), 1);
        assert!(!ex.busy());
    }

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
}
