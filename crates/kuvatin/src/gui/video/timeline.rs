//! Timeline editing: selection + inspector, slide / trim / move-to-track,
//! magnetic snapping, track rows and clip removal.

use super::undo::{Recorder, StepKind};
use super::{VideoState, MAX_SCALE_PCT, MIN_SCALE_PCT, SPEEDS};
use crate::gui::{show_error, AppWindow, ClipKind, TimelineClip};
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
    let rec = st.recorder(ui);
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
            // The keyboard verbs need to know what is selected too.
            ui.set_timeline_selected(i);
            let mut name = SharedString::new();
            let mut sel_id = SharedString::new();
            let mut sel_kind = ClipKind::Video;
            let mut sel_dur = 0.0f32;
            for idx in 0..tl_clips.row_count() {
                if let Some(mut c) = tl_clips.row_data(idx) {
                    c.selected = idx as i32 == i;
                    if c.selected {
                        name = c.name.clone();
                        sel_id = c.id.clone();
                        sel_kind = c.kind;
                        sel_dur = c.duration;
                    }
                    tl_clips.set_row_data(idx, c);
                }
            }
            ui.set_inspector_name(name);
            // Only real videos carry audio — stills and image sequences don't.
            ui.set_insp_has_audio(sel_kind == ClipKind::Video);
            // Stills get a free Duration field; real media is trimmed instead.
            ui.set_insp_is_still(sel_kind == ClipKind::Image);
            ui.set_insp_duration_s(sel_dur.round().max(1.0) as i32);
            // Speed is for clips with source time to stretch.
            ui.set_insp_has_rate(matches!(sel_kind, ClipKind::Video | ClipKind::Sequence));
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
                        ui.set_insp_scale(scale_percent(l.scale));
                        ui.set_insp_alpha((l.alpha as f32 * 100.0).clamp(0.0, 100.0));
                        ui.set_insp_volume((l.volume as f32 * 100.0).clamp(0.0, 100.0));
                    }
                    ui.set_insp_rate_index(speed_index(p.clip_rate(&cid)));
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
        let rec = rec.clone();
        ui.on_timeline_clip_dropped(move |i, delta_secs, delta_rows| {
            let Some(mut row) = tl_clips.row_data(i as usize) else {
                return;
            };
            let cid = kuvatin_video::ClipId(row.id.to_string());
            let mut slot = project_slot.borrow_mut();
            let Some(p) = slot.as_mut() else {
                return;
            };
            let before = rec.before(Some(&*p));
            // Horizontal: slide along the track.
            if let Some(geom) = p.slide_clip(&cid, delta_secs as f64) {
                row.start = geom.start.as_secs_f32();
                row.inpoint = geom.inpoint.as_secs_f32();
                row.duration = geom.duration.as_secs_f32();
            }
            // Vertical: move to another track, or a new bottom track.
            if delta_rows != 0 {
                let target =
                    drop_target_track(row.track, delta_rows, p.track_count(), tracks.row_count());
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
            rec.record(Some(&*p), StepKind::Move, Some(row.id.as_str()), before);
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
            if i < 0 {
                return dx_s;
            }
            let i = i as usize;
            let Some(dragged) = tl_clips.row_data(i) else {
                return dx_s;
            };
            // Every OTHER clip, in model order: the order decides a tie.
            let others: Vec<(f32, f32)> = (0..tl_clips.row_count())
                .filter(|&j| j != i)
                .filter_map(|j| tl_clips.row_data(j))
                .map(|c| (c.start, c.duration))
                .collect();
            snap_slide(dragged.start, dragged.duration, &others, dx_s, pps)
        });
    }

    // Click "+ New track": append an empty visual track row. The GES layer is
    // created lazily when a clip first lands there (exactly how the built-in
    // empty "Track 2" works), so an added-but-empty track can never be taken
    // away by remove_clip's trailing-layer pruning. Capped so the timeline
    // band can't grow to eat the whole viewer.
    {
        let tracks = video_tracks.clone();
        let rec = rec.clone();
        ui.on_add_track(move || {
            if tracks.row_count() >= 8 {
                return;
            }
            // No clip changes, so no project is needed to record it.
            let before = rec.before(None);
            let n = tracks.row_count() + 1;
            tracks.push(SharedString::from(format!("Track {n}")));
            rec.record(None, StepKind::AddTrack, None, before);
        });
    }

    // Reorder tracks by dragging a header: move the GES layer, then resync
    // every clip's track from GES (a reorder shifts several layers' indices).
    {
        let project_slot = project_slot.clone();
        let tl_clips = tl_clips.clone();
        let rec = rec.clone();
        ui.on_track_reordered(move |from, to| {
            if from == to {
                return;
            }
            let mut slot = project_slot.borrow_mut();
            let Some(p) = slot.as_mut() else {
                return;
            };
            let before = rec.before(Some(&*p));
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
            rec.record(Some(&*p), StepKind::ReorderTracks, None, before);
        });
    }

    // Trim a clip by dragging an edge (edge: -1 left, +1 right).
    {
        let ui_weak = ui_weak.clone();
        let project_slot = project_slot.clone();
        let tl_clips = tl_clips.clone();
        let rec = rec.clone();
        ui.on_timeline_clip_trimmed(move |i, edge, delta| {
            let Some(mut row) = tl_clips.row_data(i as usize) else {
                return;
            };
            let geom = project_slot.borrow_mut().as_mut().and_then(|p| {
                let before = rec.before(Some(&*p));
                let geom = p.trim_clip(
                    &kuvatin_video::ClipId(row.id.to_string()),
                    edge,
                    delta as f64,
                );
                rec.record(Some(&*p), StepKind::Trim, Some(row.id.as_str()), before);
                geom
            });
            let Some(geom) = geom else {
                return;
            };
            row.start = geom.start.as_secs_f32();
            row.inpoint = geom.inpoint.as_secs_f32();
            row.duration = geom.duration.as_secs_f32();
            let selected = row.selected;
            tl_clips.set_row_data(i as usize, row);
            if let Some(ui) = ui_weak.upgrade() {
                if let Some(d) = project_slot.borrow().as_ref().and_then(|p| p.duration()) {
                    ui.set_timeline_duration(d.as_secs_f32());
                }
                if selected {
                    ui.set_insp_duration_s(geom.duration.as_secs_f32().round().max(1.0) as i32);
                }
            }
        });
    }
    // Split the selected clip where the playhead stands (the Split chip, S).
    {
        let ui_weak = ui_weak.clone();
        let project_slot = project_slot.clone();
        let tl_clips = tl_clips.clone();
        let sel_idx = sel_idx.clone();
        let rec = rec.clone();
        ui.on_timeline_split(move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let i = sel_idx.get();
            if i < 0 {
                return;
            }
            let Some(mut left) = tl_clips.row_data(i as usize) else {
                return;
            };
            let at = std::time::Duration::from_secs_f64(f64::from(ui.get_playhead().max(0.0)));
            let mut slot = project_slot.borrow_mut();
            let Some(p) = slot.as_mut() else {
                return;
            };
            let before = rec.before(Some(&*p));
            let cid = kuvatin_video::ClipId(left.id.to_string());
            match p.split_clip(&cid, at) {
                Ok((right_id, lg, rg)) => {
                    left.duration = lg.duration.as_secs_f32();
                    // The right half is the same source further on: its row
                    // is the left's, name and pictures and all, at its place.
                    let mut right = left.clone();
                    right.id = right_id.0.clone().into();
                    right.start = rg.start.as_secs_f32();
                    right.inpoint = rg.inpoint.as_secs_f32();
                    right.duration = rg.duration.as_secs_f32();
                    right.selected = false;
                    tl_clips.set_row_data(i as usize, left.clone());
                    tl_clips.push(right);
                    // After the push: the step keeps the row of a clip it adds.
                    rec.record(Some(&*p), StepKind::Split, Some(left.id.as_str()), before);
                    let length = p.duration();
                    drop(slot);
                    ui.set_timeline_duration(length.map(|d| d.as_secs_f32()).unwrap_or(0.0));
                    ui.set_insp_duration_s(lg.duration.as_secs_f32().round().max(1.0) as i32);
                }
                Err(e) => {
                    drop(slot);
                    show_error(
                        &ui,
                        &format!("Could not split {}", left.name),
                        format!("{e:#}"),
                    );
                }
            }
        });
    }
    // Inspector Duration field (stills): set the selected clip's length outright.
    {
        let ui_weak = ui_weak.clone();
        let project_slot = project_slot.clone();
        let tl_clips = tl_clips.clone();
        let sel_idx = sel_idx.clone();
        let rec = rec.clone();
        ui.on_inspector_duration_changed(move |secs| {
            let i = sel_idx.get();
            if i < 0 {
                return;
            }
            let Some(mut row) = tl_clips.row_data(i as usize) else {
                return;
            };
            let geom = project_slot.borrow_mut().as_mut().and_then(|p| {
                let before = rec.before(Some(&*p));
                let geom =
                    p.set_clip_duration(&kuvatin_video::ClipId(row.id.to_string()), secs as f64);
                rec.record(Some(&*p), StepKind::Duration, Some(row.id.as_str()), before);
                geom
            });
            let Some(geom) = geom else {
                return;
            };
            row.duration = geom.duration.as_secs_f32();
            tl_clips.set_row_data(i as usize, row);
            if let Some(ui) = ui_weak.upgrade() {
                if let Some(d) = project_slot.borrow().as_ref().and_then(|p| p.duration()) {
                    ui.set_timeline_duration(d.as_secs_f32());
                }
                // The engine may have clamped; show what it actually applied.
                ui.set_insp_duration_s(geom.duration.as_secs_f32().round().max(1.0) as i32);
            }
        });
    }
    // Inspector Speed: play the selected clip faster or slower.
    {
        let ui_weak = ui_weak.clone();
        let project_slot = project_slot.clone();
        let tl_clips = tl_clips.clone();
        let sel_idx = sel_idx.clone();
        let rec = rec.clone();
        ui.on_inspector_speed_changed(move |index| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let Some(&rate) = usize::try_from(index).ok().and_then(|i| SPEEDS.get(i)) else {
                return;
            };
            let i = sel_idx.get();
            if i < 0 {
                return;
            }
            let Some(mut row) = tl_clips.row_data(i as usize) else {
                return;
            };
            let cid = kuvatin_video::ClipId(row.id.to_string());
            let done = project_slot.borrow_mut().as_mut().and_then(|p| {
                let before = rec.before(Some(&*p));
                let geom = p.set_clip_rate(&cid, rate)?;
                rec.record(Some(&*p), StepKind::Speed, Some(row.id.as_str()), before);
                Some((geom, p.clip_rate(&cid), p.duration()))
            });
            match done {
                Some((geom, now, length)) => {
                    row.start = geom.start.as_secs_f32();
                    row.inpoint = geom.inpoint.as_secs_f32();
                    row.duration = geom.duration.as_secs_f32();
                    row.rate = now as f32;
                    tl_clips.set_row_data(i as usize, row);
                    ui.set_timeline_duration(length.map(|d| d.as_secs_f32()).unwrap_or(0.0));
                    ui.set_insp_rate_index(speed_index(now));
                }
                None => {
                    // The clip plays as it did: show that.
                    ui.set_insp_rate_index(speed_index(f64::from(row.rate)));
                    show_error(
                        &ui,
                        "Could not change the speed",
                        format!(
                            "Could not change the speed of {}. It plays as it did.",
                            row.name
                        ),
                    );
                }
            }
        });
    }
    // Delete a timeline clip: the × on the selected clip.
    {
        let ui_weak = ui_weak.clone();
        let project_slot = project_slot.clone();
        let tl_clips = video_tl.clone();
        let sel_idx = sel_idx.clone();
        let rec = rec.clone();
        ui.on_timeline_clip_removed(move |i| {
            remove_timeline_clip(i, &ui_weak, &project_slot, &tl_clips, &sel_idx, &rec);
        });
    }
    // Delete key → remove whatever clip is selected.
    {
        let ui_weak = ui_weak.clone();
        let project_slot = project_slot.clone();
        let tl_clips = video_tl.clone();
        let sel_idx = sel_idx.clone();
        let rec = rec.clone();
        ui.on_delete_selected_clip(move || {
            remove_timeline_clip(
                sel_idx.get(),
                &ui_weak,
                &project_slot,
                &tl_clips,
                &sel_idx,
                &rec,
            );
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
    rec: &Recorder,
) {
    if i < 0 || (i as usize) >= tl_clips.row_count() {
        return;
    }
    let mut duration = None;
    if let Some(row) = tl_clips.row_data(i as usize) {
        if let Some(p) = project_slot.borrow_mut().as_mut() {
            let before = rec.before(Some(&*p));
            p.remove_clip(&kuvatin_video::ClipId(row.id.to_string()));
            // Recorded before the row goes, so the step keeps the row.
            rec.record(Some(&*p), StepKind::Delete, Some(row.id.as_str()), before);
            duration = Some(p.duration());
        }
    }
    tl_clips.remove(i as usize);
    if let (Some(ui), Some(d)) = (ui_weak.upgrade(), duration) {
        // Deleting the last clip used to leave the lane and scrollbar at the
        // old length.
        ui.set_timeline_duration(d.map(|d| d.as_secs_f32()).unwrap_or(0.0));
    }
    // Keep the selection on the same clip, or clear it if that clip is gone.
    let sel = sel_idx.get();
    let next = selection_after_removal(sel, i);
    if next != sel {
        sel_idx.set(next);
        if let Some(ui) = ui_weak.upgrade() {
            if next < 0 {
                ui.set_inspector_name("".into());
            }
            ui.set_timeline_selected(next);
        }
    }
}

/// Magnetic snap for a clip at `start` lasting `duration`, being slid by
/// `dx_s` seconds. If either edge comes within 8 px of the timeline origin or
/// of an edge of one of `others` (each `(start, duration)`, the dragged clip
/// left out), the slide is nudged so that edge lands exactly there, taking the
/// smallest nudge; on a tie the earlier target wins, the origin first. The
/// start never goes before zero. `pps` is the zoom in pixels per second, and
/// with none yet the drag comes back untouched.
fn snap_slide(start: f32, duration: f32, others: &[(f32, f32)], dx_s: f32, pps: f32) -> f32 {
    if pps <= 0.0 {
        return dx_s;
    }
    let prop_start = start + dx_s;
    let prop_end = start + duration + dx_s;
    let targets = std::iter::once(0.0).chain(others.iter().flat_map(|&(s, d)| [s, s + d]));
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
    if start + snapped < 0.0 {
        -start
    } else {
        snapped
    }
}

/// The track a clip dropped `delta_rows` rows away from track `current` lands
/// on: clamped to the rows there are, where one past the last means a new
/// track. A row counts whether or not the engine has a track for it yet — a
/// track added with "+ New track" gets its layer only when a clip first lands
/// there, and `layer()` creates any in between on demand, so a drop onto any
/// visible row lands exactly on it.
fn drop_target_track(
    current: i32,
    delta_rows: i32,
    engine_tracks: usize,
    visible_rows: usize,
) -> i32 {
    let count = (engine_tracks as i32).max(visible_rows as i32);
    (current + delta_rows).clamp(0, count)
}

/// Which row is selected after row `removed` is deleted: none if it was the
/// selected one, one fewer if the selection sat after it (those rows move up),
/// otherwise the same.
fn selection_after_removal(selected: i32, removed: i32) -> i32 {
    if selected == removed {
        -1
    } else if selected > removed {
        selected - 1
    } else {
        selected
    }
}

/// The inspector's Scale reading, in percent, for an engine scale (1.0 is the
/// size that fits the canvas). Clamped to the range the slider and the
/// preview box can reach, and to nothing tighter.
fn scale_percent(scale: f64) -> f32 {
    ((scale * 100.0) as f32).clamp(MIN_SCALE_PCT, MAX_SCALE_PCT)
}

/// The Speed list entry nearest `rate`, so a rate from outside the list (a
/// file edited by hand) still shows the closest one. On a tie, the slower.
fn speed_index(rate: f64) -> i32 {
    SPEEDS
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| (*a - rate).abs().total_cmp(&(*b - rate).abs()))
        .map(|(i, _)| i as i32)
        .unwrap_or(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Close enough for positions in seconds that went through f32 maths.
    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    // ---- magnetic snap ------------------------------------------------------

    /// Away from every edge, a drag stays exactly where the pointer put it.
    #[test]
    fn a_drag_far_from_every_edge_is_not_nudged() {
        let others = [(0.0, 5.0), (20.0, 5.0)];
        assert_eq!(snap_slide(10.0, 2.0, &others, 1.0, 100.0), 1.0);
    }

    /// The start edge lands exactly on the end of the clip before it.
    #[test]
    fn a_start_edge_near_a_neighbours_end_lands_on_it() {
        // The neighbour ends at 5.0; this puts the start at 5.04, 4 px away.
        let dx = snap_slide(7.0, 2.0, &[(0.0, 5.0)], -1.96, 100.0);
        assert!(near(7.0 + dx, 5.0), "start landed at {}", 7.0 + dx);
    }

    /// The end edge snaps too: dragged up against the next clip.
    #[test]
    fn an_end_edge_near_a_neighbours_start_lands_on_it() {
        // The next clip starts at 10.0; this puts the end at 9.95.
        let dx = snap_slide(4.0, 2.0, &[(10.0, 3.0)], 3.95, 100.0);
        assert!(near(6.0 + dx, 10.0), "end landed at {}", 6.0 + dx);
    }

    /// Two edges in range: the one needing the smaller nudge wins.
    #[test]
    fn the_nearer_of_two_edges_in_range_wins() {
        // The start would sit 0.05 past an end at 5.0, the end 0.03 short of a
        // start at 8.0. The end is nearer.
        let others = [(0.0, 5.0), (8.0, 1.0)];
        let dx = snap_slide(6.0, 2.92, &others, -0.95, 100.0);
        assert!(near(8.92 + dx, 8.0), "end landed at {}", 8.92 + dx);
    }

    /// Just outside the window nothing happens: the snap must not reach
    /// further than it looks like it does.
    #[test]
    fn an_edge_nine_pixels_away_does_not_snap() {
        assert_eq!(snap_slide(7.0, 2.0, &[(0.0, 5.0)], -1.91, 100.0), -1.91);
    }

    /// The window is eight pixels, so zooming out widens it in seconds.
    #[test]
    fn the_window_is_eight_pixels_at_any_zoom() {
        // Half a second away: 50 px at 100 px/s, 5 px at 10 px/s.
        assert_eq!(snap_slide(7.0, 2.0, &[(0.0, 5.0)], -1.5, 100.0), -1.5);
        let dx = snap_slide(7.0, 2.0, &[(0.0, 5.0)], -1.5, 10.0);
        assert!(near(7.0 + dx, 5.0), "start landed at {}", 7.0 + dx);
    }

    /// The timeline origin is an edge even with no clip there.
    #[test]
    fn the_timeline_origin_is_an_edge() {
        let dx = snap_slide(3.0, 2.0, &[], -2.95, 100.0);
        assert!(near(3.0 + dx, 0.0), "start landed at {}", 3.0 + dx);
    }

    /// However far left the drag goes, the start stops at zero.
    #[test]
    fn a_clip_never_slides_before_the_timeline_start() {
        assert_eq!(snap_slide(3.0, 2.0, &[], -10.0, 100.0), -3.0);
    }

    /// No zoom yet is a layout that hasn't happened; leave the drag alone
    /// rather than divide by it.
    #[test]
    fn no_zoom_yet_means_no_snap() {
        assert_eq!(snap_slide(7.0, 2.0, &[(0.0, 5.0)], -1.96, 0.0), -1.96);
    }

    // ---- where a dropped clip lands -----------------------------------------

    #[test]
    fn dragging_above_the_top_track_stops_at_the_top() {
        assert_eq!(drop_target_track(1, -5, 3, 3), 0);
    }

    /// One past the last row is a new track, and so is anything further: a
    /// long drag makes one track, not a gap.
    #[test]
    fn dragging_below_the_last_track_makes_one_new_track() {
        assert_eq!(drop_target_track(1, 1, 2, 2), 2);
        assert_eq!(drop_target_track(1, 9, 2, 2), 2);
    }

    /// A track added with "+ New track" has a row but no engine layer until a
    /// clip lands on it. It is still somewhere a clip can be dropped.
    #[test]
    fn a_row_the_engine_has_no_track_for_yet_is_still_a_target() {
        assert_eq!(drop_target_track(0, 3, 2, 4), 3);
        assert_eq!(drop_target_track(0, 9, 2, 4), 4);
    }

    // ---- selection after a clip is removed ----------------------------------

    #[test]
    fn removing_the_selected_clip_clears_the_selection() {
        assert_eq!(selection_after_removal(2, 2), -1);
    }

    /// The rows after the removed one shift down, so the index follows them.
    #[test]
    fn removing_an_earlier_clip_keeps_the_same_clip_selected() {
        assert_eq!(selection_after_removal(3, 1), 2);
    }

    #[test]
    fn removing_a_later_clip_changes_nothing() {
        assert_eq!(selection_after_removal(1, 3), 1);
    }

    #[test]
    fn with_nothing_selected_nothing_becomes_selected() {
        assert_eq!(selection_after_removal(-1, 0), -1);
    }

    // ---- the inspector's scale reading --------------------------------------

    /// The engine zooms a clip past the canvas. The read-back used to clamp at
    /// 100 % and snap a zoomed clip back to fit every time it was selected.
    #[test]
    fn the_scale_reading_reaches_past_the_canvas() {
        assert_eq!(scale_percent(1.0), 100.0);
        assert_eq!(scale_percent(2.5), 250.0);
        assert_eq!(
            scale_percent(9.0),
            MAX_SCALE_PCT,
            "capped at the slider's end"
        );
        assert_eq!(scale_percent(0.01), MIN_SCALE_PCT, "and at its start");
    }

    // ---- the Speed list -----------------------------------------------------

    #[test]
    fn each_speed_finds_its_own_entry_and_others_the_nearest() {
        for (i, &r) in SPEEDS.iter().enumerate() {
            assert_eq!(speed_index(r), i as i32, "{r}");
        }
        assert_eq!(
            speed_index(3.0),
            4,
            "between 2x and 4x: the first of the two"
        );
        assert_eq!(speed_index(0.1), 0);
        assert_eq!(speed_index(9.0), 5);
    }

    #[test]
    fn every_speed_offered_is_one_the_engine_keeps_exactly() {
        use kuvatin_video::project::{RATE_MAX, RATE_MIN};
        for r in SPEEDS {
            assert!((RATE_MIN..=RATE_MAX).contains(&r), "{r}");
            assert_eq!(
                f64::from(r as f32),
                r,
                "{r} survives the engine's f32 rounding"
            );
        }
    }
}
