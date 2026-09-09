//! Timeline editing: selection + inspector, slide / trim / move-to-track,
//! magnetic snapping, track rows and clip removal.

use super::VideoState;
use crate::gui::{AppWindow, TimelineClip};
use slint::{ComponentHandle, Model, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

/// Wire the timeline callbacks.
pub(super) fn wire(ui: &AppWindow, st: &VideoState) {
    let ui_weak = ui.as_weak();
    let project_slot = &st.project;
    let tl_clips = &st.tl_clips;
    let video_tl = &st.tl_clips;
    let video_tracks = &st.tracks;
    let sel_idx = &st.sel_idx;
    let pending_xform = &st.pending_xform;
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
