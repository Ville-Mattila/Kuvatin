//! Videos mode: the GES `Project` (composited preview + render), the media
//! bin and timeline models, transport and the preview tick. Import, timeline
//! editing and export live in the submodules.

pub(super) mod export;
pub(super) mod import;
mod timeline;

use super::{show_error, AppWindow, TimelineClip, VideoAsset};
use export::ExportState;
use import::ImportState;
use slint::{
    ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel,
};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

/// Everything the Videos mode owns that more than one handler touches.
pub(super) struct VideoState {
    /// The GES project, created on demand by the first clip (or canvas change).
    pub(super) project: Rc<RefCell<Option<kuvatin_video::Project>>>,
    /// Media-bin rows; `bin_paths[i]` is the source of `assets[i]`.
    pub(super) assets: Rc<VecModel<VideoAsset>>,
    pub(super) bin_paths: Rc<RefCell<Vec<PathBuf>>>,
    pub(super) tl_clips: Rc<VecModel<TimelineClip>>,
    /// Timeline tracks (GES layers, top = index 0 = composited on top). Kept
    /// mutable so dragging a clip onto a new track can grow the list.
    pub(super) tracks: Rc<VecModel<SharedString>>,
    /// Index of the selected timeline clip (for the inspector), or -1.
    pub(super) sel_idx: Rc<Cell<i32>>,
    /// Latest inspector transform awaiting a coalesced apply on the UI timer.
    /// Rapid slider drags only stash a value here; no GES work per event.
    pub(super) pending_xform: Rc<RefCell<Option<(String, kuvatin_video::Layout)>>>,
    /// Latest scrub target (seconds, frame-accurate?) awaiting the UI tick:
    /// one seek per tick instead of one per pointer event, and an ACCURATE
    /// landing on release so the picture matches the playhead.
    pub(super) pending_seek: Rc<Cell<Option<(f32, bool)>>>,
}

impl VideoState {
    pub(super) fn new(ui: &AppWindow) -> Self {
        let assets = Rc::new(VecModel::<VideoAsset>::from(Vec::<VideoAsset>::new()));
        ui.set_video_clips(ModelRc::from(assets.clone()));
        let tl_clips = Rc::new(VecModel::<TimelineClip>::from(Vec::<TimelineClip>::new()));
        ui.set_timeline_clips(ModelRc::from(tl_clips.clone()));
        let tracks = Rc::new(VecModel::<SharedString>::from(vec![
            SharedString::from("Track 1"),
            SharedString::from("Track 2"),
        ]));
        ui.set_timeline_track_labels(ModelRc::from(tracks.clone()));
        Self {
            project: Rc::new(RefCell::new(None)),
            assets,
            bin_paths: Rc::new(RefCell::new(Vec::new())),
            tl_clips,
            tracks,
            sel_idx: Rc::new(Cell::new(-1)),
            pending_xform: Rc::new(RefCell::new(None)),
            pending_seek: Rc::new(Cell::new(None)),
        }
    }
}

/// Wire every Videos-mode callback and timer.
pub(super) fn wire(
    ui: &AppWindow,
    st: &VideoState,
    im: &ImportState,
    ex: &ExportState,
    timers: &mut Vec<slint::Timer>,
) {
    import::wire(ui, st, im, timers);
    timeline::wire(ui, st);
    export::wire(ui, st, ex, timers);

    let ui_weak = ui.as_weak();
    let project_slot = &st.project;
    let tl_clips = &st.tl_clips;
    let sel_idx = &st.sel_idx;
    let pending_xform = &st.pending_xform;
    let pending_seek = &st.pending_seek;
    let export_active = &ex.active;
    let export_pending = &ex.pending;
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
        timers.push(timer);
    }
}

/// Create a GES editing project whose composited preview frames are pushed to
/// the UI's `video-frame` (from a GStreamer thread, hopped to the UI thread).
fn make_project(ui_weak: &slint::Weak<AppWindow>) -> Option<kuvatin_video::Project> {
    // The newest frame waiting for the UI thread; if the UI lags, a later
    // frame simply replaces it (no queue to drain).
    let pending: Arc<Mutex<Option<SharedPixelBuffer<Rgba8Pixel>>>> = Arc::new(Mutex::new(None));
    let ui_for_frame = ui_weak.clone();
    match kuvatin_video::Project::new(move |view| {
        // ONE copy, on the GStreamer thread, straight from the mapped buffer
        // into the pixel buffer the UI will display (SharedPixelBuffer is
        // Send; only slint::Image is not). The UI thread just wraps it.
        let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(view.width, view.height);
        view.copy_packed_into(buf.make_mut_bytes());
        *pending.lock().unwrap() = Some(buf);
        let ui_for_frame = ui_for_frame.clone();
        let pending = pending.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let (Some(ui), Some(buf)) = (ui_for_frame.upgrade(), pending.lock().unwrap().take())
            {
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
