//! Undo and redo for the timeline. A step is what an edit changed, clip by
//! clip: each affected clip's record before and after, read from the engine
//! around the edit (`Project::clip_records`, about 0.2 ms at 50 clips). Undo
//! writes the old records back exactly, so a clip returns where it was with its
//! transform, under the same ID where the engine allows it, and a clip that
//! comes back gets the row it had, name and thumbnail included.

use super::project_file::kind_of;
use super::tracks;
use crate::gui::history::{History, Step};
use crate::gui::{name_list, show_error, AppWindow, TimelineClip, TimelineTrack};
use kuvatin_video::{ClipRecord, TrackRecord};
use slint::{ComponentHandle, Model, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::time::Instant;

/// The timeline's undo history, shared by every handler that edits.
pub(super) type TimelineHistory = Rc<RefCell<History<TimelineStep>>>;

/// What kind of edit a step is. The kind decides whether consecutive steps
/// merge and how the step describes itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StepKind {
    Move,
    Trim,
    Transform,
    Duration,
    Add,
    Delete,
    ReorderTracks,
    AddTrack,
    Split,
    Speed,
    MuteTrack,
    LockTrack,
    RenameTrack,
    /// A title's text, font, colour or alignment. Its own kind so the hint
    /// does not call typing a transform.
    Text,
}

impl StepKind {
    /// The kinds a continuous gesture produces, and a rename or a title's
    /// text, whose keystrokes are one edit. Only these merge. A mute or a lock is one
    /// click: muting and unmuting inside a second is two steps, not nothing.
    fn merges(self) -> bool {
        matches!(
            self,
            StepKind::Move
                | StepKind::Trim
                | StepKind::Transform
                | StepKind::Duration
                | StepKind::RenameTrack
                | StepKind::Text
        )
    }
}

/// What a step is about, when it is about one thing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Subject {
    Clip(String),
    /// A track, by its index. Safe as a merge key: the only track kind that
    /// merges is RenameTrack, and every edit that could renumber a track is a
    /// kind of its own, which `merges_with` already refuses to merge across.
    Track(usize),
}

impl Subject {
    /// The clip this is about, if it is about a clip.
    pub(super) fn clip(&self) -> Option<&str> {
        match self {
            Subject::Clip(id) => Some(id),
            Subject::Track(_) => None,
        }
    }
}

/// Every clip's record, by ID, and the track table, at one moment.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Capture {
    pub(super) records: BTreeMap<String, ClipRecord>,
    /// One record per track row, top first: how many there are, and each
    /// one's name, mute and lock. Solo is not in it.
    pub(super) tracks: Vec<TrackRecord>,
}

impl Capture {
    /// Read the timeline as it is now. No project yet means no clips.
    pub(super) fn of(project: Option<&kuvatin_video::Project>, tracks: Vec<TrackRecord>) -> Self {
        let records = project
            .map(|p| {
                p.clip_records()
                    .into_iter()
                    .map(|(id, record)| (id.0, record))
                    .collect()
            })
            .unwrap_or_default();
        Capture { records, tracks }
    }
}

/// One clip's record on each side of a step. `None` means the clip did not
/// exist on that side.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ClipChange {
    pub(super) id: String,
    pub(super) before: Option<ClipRecord>,
    pub(super) after: Option<ClipRecord>,
}

/// The clips whose record differs between two captures, in ID order. Records
/// compare exactly: reading an untouched clip twice gives identical values.
pub(super) fn diff(before: &Capture, after: &Capture) -> Vec<ClipChange> {
    let ids: BTreeSet<&String> = before.records.keys().chain(after.records.keys()).collect();
    ids.into_iter()
        .filter_map(|id| {
            let (b, a) = (before.records.get(id), after.records.get(id));
            (b != a).then(|| ClipChange {
                id: id.clone(),
                before: b.cloned(),
                after: a.cloned(),
            })
        })
        .collect()
}

/// What one timeline edit changed.
pub(super) struct TimelineStep {
    pub(super) kind: StepKind,
    /// The clip or track the step is about, when it is about one.
    pub(super) subject: Option<Subject>,
    /// That clip's display name, or that track's label on the step's "before"
    /// side, for the hint.
    pub(super) name: String,
    pub(super) changes: Vec<ClipChange>,
    /// The timeline rows of the clips that exist on only one side, by ID, so a
    /// clip that comes back gets its row as it was: its name (the engine's
    /// record spells one from the URI), kind and thumbnail, without decoding.
    pub(super) kept_rows: HashMap<String, TimelineClip>,
    /// The track table on each side. A step that changes nothing but the
    /// table (a rename, a mute, a lock) is still a step.
    pub(super) tracks_before: Vec<TrackRecord>,
    pub(super) tracks_after: Vec<TrackRecord>,
}

impl TimelineStep {
    /// `rows` are the timeline's rows by ID when the edit is recorded; only
    /// those of clips that come or go are kept.
    pub(super) fn new(
        kind: StepKind,
        subject: Option<Subject>,
        name: &str,
        before: &Capture,
        after: &Capture,
        rows: HashMap<String, TimelineClip>,
    ) -> Self {
        let changes = diff(before, after);
        let one_sided: HashSet<&str> = changes
            .iter()
            .filter(|c| c.before.is_none() != c.after.is_none())
            .map(|c| c.id.as_str())
            .collect();
        let kept_rows = rows
            .into_iter()
            .filter(|(id, _)| one_sided.contains(id.as_str()))
            .collect();
        TimelineStep {
            kind,
            subject,
            name: name.to_string(),
            changes,
            kept_rows,
            tracks_before: before.tracks.clone(),
            tracks_after: after.tracks.clone(),
        }
    }

    /// Replace a clip's ID everywhere in the step: the engine restored it
    /// under a new one.
    pub(super) fn rename_clip(&mut self, old: &str, new: &str) {
        if self.subject.as_ref().and_then(Subject::clip) == Some(old) {
            self.subject = Some(Subject::Clip(new.to_string()));
        }
        for change in &mut self.changes {
            if change.id == old {
                change.id = new.to_string();
            }
        }
        if let Some(row) = self.kept_rows.remove(old) {
            self.kept_rows.insert(new.to_string(), row);
        }
    }

    /// Whether the step's track has `flag` set on its "after" side: which
    /// way a toggle went, read off the step's own tables.
    fn turned_on(&self, flag: impl Fn(&TrackRecord) -> bool) -> bool {
        match self.subject {
            Some(Subject::Track(t)) => self.tracks_after.get(t).is_some_and(flag),
            _ => false,
        }
    }
}

impl Step for TimelineStep {
    fn describe(&self) -> String {
        let name = &self.name;
        match self.kind {
            StepKind::Move => format!("moving {name}"),
            StepKind::Trim => format!("trimming {name}"),
            StepKind::Transform => format!("transforming {name}"),
            StepKind::Duration => format!("changing the duration of {name}"),
            StepKind::Add => format!("adding {name}"),
            StepKind::Delete => format!("deleting {name}"),
            StepKind::ReorderTracks => "reordering tracks".into(),
            StepKind::AddTrack => "adding a track".into(),
            StepKind::Split => format!("splitting {name}"),
            StepKind::Speed => format!("changing the speed of {name}"),
            StepKind::MuteTrack if self.turned_on(|t| t.muted) => format!("muting {name}"),
            StepKind::MuteTrack => format!("unmuting {name}"),
            StepKind::LockTrack if self.turned_on(|t| t.locked) => format!("locking {name}"),
            StepKind::LockTrack => format!("unlocking {name}"),
            StepKind::RenameTrack => format!("renaming {name}"),
            StepKind::Text => format!("editing the text of {name}"),
        }
    }

    fn merges_with(&self, newer: &Self) -> bool {
        self.kind == newer.kind
            && self.kind.merges()
            && self.subject.is_some()
            && self.subject == newer.subject
    }

    fn absorb(&mut self, newer: Self) {
        for change in newer.changes {
            match self.changes.iter_mut().find(|c| c.id == change.id) {
                Some(mine) => mine.after = change.after,
                None => self.changes.push(change),
            }
        }
        // A clip dragged back to where it started is no longer a change.
        self.changes.retain(|c| c.before != c.after);
        self.kept_rows.extend(newer.kept_rows);
        self.tracks_after = newer.tracks_after;
    }

    fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.tracks_before == self.tracks_after
    }
}

/// Which way a step is being applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Direction {
    Undo,
    Redo,
}

/// The engine operations that take the timeline to one side of a step, by the
/// IDs the step holds. They run in field order: removals, then writes, then
/// restores. Removing first frees the places and names the restored clips come
/// back into; writing next moves every remaining clip to where that side has
/// it, so a restored clip never lands on one that has yet to move away. The
/// writes run as one batch (`Project::set_clip_records`), because GES refuses a
/// clip on top of another even for a moment.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Plan {
    /// Clips that must not exist.
    pub(super) removes: Vec<String>,
    /// Clips that exist and take these records.
    pub(super) writes: Vec<(String, ClipRecord)>,
    /// Clips that are gone and come back with these records.
    pub(super) restores: Vec<(String, ClipRecord)>,
}

/// What it takes to bring the timeline to the `dir` side of `step`.
pub(super) fn plan(step: &TimelineStep, dir: Direction) -> Plan {
    let mut out = Plan::default();
    for change in &step.changes {
        let (from, to) = match dir {
            Direction::Undo => (&change.after, &change.before),
            Direction::Redo => (&change.before, &change.after),
        };
        match (from, to) {
            (Some(_), None) => out.removes.push(change.id.clone()),
            (None, Some(record)) => out.restores.push((change.id.clone(), record.clone())),
            (Some(_), Some(record)) => out.writes.push((change.id.clone(), record.clone())),
            (None, None) => {}
        }
    }
    out
}

/// The track table the timeline shows on the `dir` side of `step`.
pub(super) fn target_tracks(step: &TimelineStep, dir: Direction) -> Vec<TrackRecord> {
    match dir {
        Direction::Undo => step.tracks_before.clone(),
        Direction::Redo => step.tracks_after.clone(),
    }
}

/// What one engine operation did, as the rows need it: the clip's ID in the
/// step, the ID it has now (a restore can hand back a new one), and its
/// record, or `None` if the clip is gone.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Applied {
    pub(super) id: String,
    pub(super) now_id: String,
    pub(super) record: Option<ClipRecord>,
}

/// The timeline rows after `applied`. A gone clip loses its row; a changed
/// clip keeps its row (name, thumbnail, selection) in its new place; a clip
/// that comes back gets the row `kept` has for it, unselected, or else a row
/// built from its record.
pub(super) fn rows_after(
    rows: &[TimelineClip],
    applied: &[Applied],
    kept: &HashMap<String, TimelineClip>,
) -> Vec<TimelineClip> {
    let mut out = rows.to_vec();
    for a in applied {
        let at = out.iter().position(|r| r.id.as_str() == a.id);
        match (&a.record, at) {
            (None, Some(i)) => {
                out.remove(i);
            }
            (None, None) => {}
            (Some(record), Some(i)) => place(&mut out[i], &a.now_id, record),
            (Some(record), None) => {
                let mut row = kept.get(&a.id).cloned().unwrap_or_else(|| TimelineClip {
                    name: record.name.as_str().into(),
                    kind: kind_of(record),
                    ..Default::default()
                });
                row.selected = false;
                place(&mut row, &a.now_id, record);
                out.push(row);
            }
        }
    }
    out
}

/// The names of the clips among `restores` whose source is not there to
/// bring back, as `available` answers for a URI. A title has no source, so it
/// is never asked about: `available("")` would look for a file with no name.
fn missing_sources(
    restores: &[(String, ClipRecord)],
    kept: &HashMap<String, TimelineClip>,
    available: impl Fn(&str) -> bool,
) -> Vec<String> {
    restores
        .iter()
        .filter(|(_, record)| record.body.is_none() && !available(&record.uri))
        .map(|(id, record)| match kept.get(id) {
            Some(row) => row.name.to_string(),
            None => record.name.clone(),
        })
        .collect()
}

/// Put a row where `record` places its clip, under the ID the clip has now.
/// A title's name is its text, so it follows the record; any other clip keeps
/// the name its row has.
fn place(row: &mut TimelineClip, id: &str, record: &ClipRecord) {
    row.id = id.into();
    if record.body.is_some() {
        row.name = record.name.as_str().into();
    }
    row.track = record.track as i32;
    row.start = record.start as f32;
    row.duration = record.duration as f32;
    row.inpoint = record.inpoint as f32;
    row.rate = record.rate as f32;
}

/// The row to select after an undo or redo: the step's own clip if it is on
/// the timeline, so the change is visible; else the clip that was selected, if
/// it still exists; else none.
pub(super) fn selection_after(
    rows: &[TimelineClip],
    subject: Option<&str>,
    selected: Option<&str>,
) -> i32 {
    let find = |id: Option<&str>| id.and_then(|id| rows.iter().position(|r| r.id.as_str() == id));
    find(subject)
        .or_else(|| find(selected))
        .map(|i| i as i32)
        .unwrap_or(-1)
}

/// Writes that bring the rows in line with the engine's clips, whatever the
/// rows show now: every row whose clip is gone, then every clip as it is.
pub(super) fn applied_from_engine(
    rows: &[TimelineClip],
    records: Vec<(kuvatin_video::ClipId, ClipRecord)>,
) -> Vec<Applied> {
    let live: HashSet<String> = records.iter().map(|(id, _)| id.0.clone()).collect();
    let gone = rows
        .iter()
        .filter(|r| !live.contains(r.id.as_str()))
        .map(|r| Applied {
            id: r.id.to_string(),
            now_id: r.id.to_string(),
            record: None,
        });
    let present = records.into_iter().map(|(id, record)| Applied {
        id: id.0.clone(),
        now_id: id.0,
        record: Some(record),
    });
    gone.chain(present).collect()
}

/// Make the timeline's track rows match `want` exactly: how many there are,
/// and every row's name, mute and lock. Solo is the row's own and is carried
/// across rather than overwritten: undo does not change what you are
/// listening to. Rows that already match are left alone, so Slint does not
/// repaint them.
pub(super) fn set_track_rows(rows: &VecModel<TimelineTrack>, want: &[TrackRecord]) {
    while rows.row_count() > want.len() {
        rows.remove(rows.row_count() - 1);
    }
    for (i, record) in want.iter().enumerate() {
        match rows.row_data(i) {
            Some(have) => {
                let row = tracks::row(record, have.soloed);
                if row != have {
                    rows.set_row_data(i, row);
                }
            }
            None => rows.push(tracks::row(record, false)),
        }
    }
}

/// What recording a step needs: the history, the timeline rows (for a clip's
/// name, and the rows a step keeps) and the track rows. Cheap to clone into
/// each handler.
#[derive(Clone)]
pub(super) struct Recorder {
    pub(super) history: TimelineHistory,
    pub(super) tl_clips: Rc<VecModel<TimelineClip>>,
    pub(super) tracks: Rc<VecModel<TimelineTrack>>,
    pub(super) ui: slint::Weak<AppWindow>,
}

impl Recorder {
    /// Read the timeline just before an edit.
    pub(super) fn before(&self, project: Option<&kuvatin_video::Project>) -> Capture {
        Capture::of(project, self.table())
    }

    /// The track rows as the history keeps them.
    fn table(&self) -> Vec<TrackRecord> {
        tracks::records(&tracks::rows_of(&self.tracks))
    }

    /// Record what an edit changed. Call it after the engine edit and after an
    /// added clip's row is pushed, but before a deleted clip's row is removed:
    /// the step's name and the rows it keeps come from the rows.
    pub(super) fn record(
        &self,
        project: Option<&kuvatin_video::Project>,
        kind: StepKind,
        subject: Option<Subject>,
        before: Capture,
    ) {
        let after = Capture::of(project, self.table());
        self.record_captures(kind, subject, before, after);
    }

    pub(super) fn record_captures(
        &self,
        kind: StepKind,
        subject: Option<Subject>,
        before: Capture,
        after: Capture,
    ) {
        let rows: Vec<TimelineClip> = self.tl_clips.iter().collect();
        let name = match &subject {
            Some(Subject::Clip(id)) => {
                let from_rows = rows
                    .iter()
                    .find(|r| r.id.as_str() == id)
                    .map(|r| r.name.to_string());
                let from_records = before
                    .records
                    .get(id)
                    .or_else(|| after.records.get(id))
                    .map(|r| r.name.clone());
                from_rows.or(from_records).unwrap_or_default()
            }
            // As it was called before the step: "renaming Track 2".
            Some(Subject::Track(t)) => tracks::label(&before.tracks, *t),
            None => String::new(),
        };
        let kept = rows.into_iter().map(|r| (r.id.to_string(), r)).collect();
        let step = TimelineStep::new(kind, subject, &name, &before, &after, kept);
        let mut history = self.history.borrow_mut();
        history.record(step, Instant::now());
        if let Some(ui) = self.ui.upgrade() {
            refresh(&ui, &history);
        }
    }
}

/// Mirror the history into the Undo and Redo chips.
pub(super) fn refresh(ui: &AppWindow, history: &History<TimelineStep>) {
    ui.set_video_can_undo(history.can_undo());
    ui.set_video_can_redo(history.can_redo());
    ui.set_video_undo_hint(history.undo_hint().into());
    ui.set_video_redo_hint(history.redo_hint().into());
}

/// Wire the Undo and Redo chips and keys (both arrive as `video-undo` and
/// `video-redo`).
pub(super) fn wire(ui: &AppWindow, st: &super::VideoState, ex: &super::export::ExportState) {
    for dir in [Direction::Undo, Direction::Redo] {
        let ui_weak = ui.as_weak();
        let project = st.project.clone();
        let sel_idx = st.sel_idx.clone();
        let pending_xform = st.pending_xform.clone();
        let pending_title = st.pending_title.clone();
        let active = ex.active.clone();
        let pending = ex.pending.clone();
        let rec = st.recorder(ui);
        let waves = st.waves.clone();
        let run = move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            // The engine refuses edits during an export, and there is nothing
            // to undo on an engine that never started.
            if active.get() || pending.get() || ui.get_video_engine_down() {
                return;
            }
            // A transform or a title still waiting for the preview tick would
            // be applied, and recorded, after the undo.
            pending_xform.borrow_mut().take();
            pending_title.borrow_mut().take();
            apply_step(&ui, &project, &rec, &sel_idx, dir, &waves);
        };
        match dir {
            Direction::Undo => ui.on_video_undo(run),
            Direction::Redo => ui.on_video_redo(run),
        }
    }
}

/// Take the timeline to the other side of the newest undo (or redo) step.
fn apply_step(
    ui: &AppWindow,
    project: &Rc<RefCell<Option<kuvatin_video::Project>>>,
    rec: &Recorder,
    sel_idx: &Rc<Cell<i32>>,
    dir: Direction,
    waves: &super::waves::Waves,
) {
    let verb = match dir {
        Direction::Undo => "undo",
        Direction::Redo => "redo",
    };
    // Read what to do, then let go of the history: applying reads the rows,
    // and recording must never see it borrowed.
    let (ops, table, subject, kept) = {
        let history = rec.history.borrow();
        let step = match dir {
            Direction::Undo => history.peek_undo(),
            Direction::Redo => history.peek_redo(),
        };
        let Some(step) = step else {
            return;
        };
        (
            plan(step, dir),
            target_tracks(step, dir),
            step.subject.clone(),
            step.kept_rows.clone(),
        )
    };

    // A step that only changed track rows (a track added, renamed, muted or
    // locked before any clip was placed) needs no engine, which may not exist
    // yet.
    if project.borrow().is_none() {
        if ops == Plan::default() {
            set_track_rows(&rec.tracks, &table);
            let mut history = rec.history.borrow_mut();
            match dir {
                Direction::Undo => history.commit_undo(),
                Direction::Redo => history.commit_redo(Instant::now()),
            }
            refresh(ui, &history);
        }
        return;
    }
    let mut slot = project.borrow_mut();
    let Some(p) = slot.as_mut() else {
        return;
    };

    // A deleted clip whose file has gone since: say which, and change nothing.
    let missing = missing_sources(&ops.restores, &kept, |uri| p.source_available(uri));
    if !missing.is_empty() {
        drop(slot);
        show_error(
            ui,
            &format!("Could not {verb}"),
            format!(
                "{} no longer where {} was, so nothing was changed:\n{}",
                if missing.len() == 1 {
                    "This file is"
                } else {
                    "These files are"
                },
                if missing.len() == 1 { "it" } else { "they" },
                name_list(&missing)
            ),
        );
        return;
    }

    let mut applied = Vec::new();
    let mut failed: Option<String> = None;
    // The plan's order: removals, then every write as one batch (so clips
    // trading places never collide on the way), then restores.
    for id in &ops.removes {
        p.remove_clip(&kuvatin_video::ClipId(id.clone()));
        applied.push(Applied {
            id: id.clone(),
            now_id: id.clone(),
            record: None,
        });
    }
    let batch: Vec<(kuvatin_video::ClipId, ClipRecord)> = ops
        .writes
        .iter()
        .map(|(id, record)| (kuvatin_video::ClipId(id.clone()), record.clone()))
        .collect();
    let refused = p.set_clip_records(&batch);
    for (id, record) in batch {
        if refused.contains(&id) {
            if failed.is_none() {
                failed = Some(record.name);
            }
        } else {
            applied.push(Applied {
                id: id.0.clone(),
                now_id: id.0,
                record: Some(record),
            });
        }
    }
    if failed.is_none() {
        for (id, record) in &ops.restores {
            // A retry after a restore failed partway: the ones that worked are
            // back already, under the IDs every step now uses.
            if p.clip_track(&kuvatin_video::ClipId(id.clone())).is_some() {
                applied.push(Applied {
                    id: id.clone(),
                    now_id: id.clone(),
                    record: Some(record.clone()),
                });
                continue;
            }
            match p.restore_clip(&kuvatin_video::ClipId(id.clone()), record) {
                Ok(now) => applied.push(Applied {
                    id: id.clone(),
                    now_id: now.0,
                    record: Some(record.clone()),
                }),
                Err(e) => {
                    failed = Some(format!("{}: {e:#}", record.name));
                    break;
                }
            }
        }
    }
    p.prune_tracks(table.len());

    let rows: Vec<TimelineClip> = rec.tl_clips.iter().collect();
    let selected_id = usize::try_from(sel_idx.get())
        .ok()
        .and_then(|i| rows.get(i))
        .map(|r| r.id.to_string());
    // A restore keeps a clip's ID; should one ever hand back a new one,
    // every step must use it from now on.
    let renames: Vec<(String, String)> = applied
        .iter()
        .filter(|a| a.id != a.now_id)
        .map(|a| (a.id.clone(), a.now_id.clone()))
        .collect();
    // On a failure, the rows follow the engine rather than the plan, and the
    // engine knows a restored clip by the ID it has now.
    let (to_rows, kept) = if failed.is_some() {
        let mut kept = kept;
        for (old, new) in &renames {
            if let Some(row) = kept.remove(old) {
                kept.insert(new.clone(), row);
            }
        }
        (applied_from_engine(&rows, p.clip_records()), kept)
    } else {
        (applied, kept)
    };
    let new_rows = rows_after(&rows, &to_rows, &kept);
    // A prune the engine refused (a clip still on a track this side does not
    // have) leaves more layers than the table has rows: they stay on screen,
    // as rows of their own.
    let mut table = table;
    if table.len() < p.track_count() {
        table.resize(p.track_count(), TrackRecord::default());
    }
    let duration = p.duration();
    drop(slot);

    set_track_rows(&rec.tracks, &table);
    if let Some(p) = project.borrow_mut().as_mut() {
        tracks::push_mutes(p, &rec.tracks);
        // Every undo changes the work, and one that changes only the table (a
        // rename, a lock) touches nothing the engine would notice by itself.
        p.mark_unsaved();
    }
    ui.set_timeline_duration(duration.map(|d| d.as_secs_f32()).unwrap_or(0.0));

    // A track's step selects no clip; a clip's step selects its clip, under
    // the ID it has now.
    let subject_now = subject.as_ref().and_then(Subject::clip).map(|s| {
        renames
            .iter()
            .find(|(old, _)| old == s)
            .map(|(_, new)| new.clone())
            .unwrap_or_else(|| s.to_string())
    });
    let next = selection_after(&new_rows, subject_now.as_deref(), selected_id.as_deref());
    // A clip deleted before its thumbnail arrived comes back without one:
    // decode it again.
    let without_thumb: Vec<(kuvatin_video::ClipId, ClipRecord)> = ops
        .restores
        .iter()
        .filter(|(id, _)| {
            new_rows
                .iter()
                .any(|r| r.id.as_str() == id.as_str() && r.thumb.size().width == 0)
        })
        .map(|(id, record)| (kuvatin_video::ClipId(id.clone()), record.clone()))
        .collect();
    // The same for the waveform: from the cache, or decoded again.
    let without_wave: Vec<(SharedString, String)> = ops
        .restores
        .iter()
        .filter(|(id, _)| {
            new_rows
                .iter()
                .any(|r| r.id.as_str() == id.as_str() && r.wave.size().width == 0)
        })
        .map(|(id, record)| (id.as_str().into(), record.uri.clone()))
        .collect();
    rec.tl_clips.set_vec(new_rows);
    super::project_file::spawn_thumbnails(ui.as_weak(), without_thumb);
    waves.fill(ui.as_weak(), without_wave);

    {
        let mut history = rec.history.borrow_mut();
        if failed.is_none() {
            match dir {
                Direction::Undo => history.commit_undo(),
                Direction::Redo => history.commit_redo(Instant::now()),
            }
        } else {
            // The step stays to be tried again, but it no longer matches the
            // timeline, so the next edit must not merge into it.
            history.seal();
        }
        for (old, new) in &renames {
            for step in history.steps_mut() {
                step.rename_clip(old, new);
            }
        }
        refresh(ui, &history);
    }

    // Highlights the row and fills the inspector, or clears both for -1.
    ui.invoke_timeline_select(next);

    if let Some(what) = failed {
        show_error(
            ui,
            &format!("Could not {verb}"),
            format!(
                "The engine refused a change, so the timeline shows what it holds now and the step is still there to try again.\n{what}"
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::ClipKind;
    use kuvatin_video::LayoutRecord;
    use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

    fn rec(track: usize, start: f64, duration: f64) -> ClipRecord {
        ClipRecord {
            uri: "file:///C:/media/intro.mp4".into(),
            name: "intro.mp4".into(),
            track,
            start,
            inpoint: 0.0,
            duration,
            rate: 1.0,
            layout: LayoutRecord {
                posx: 0,
                posy: 0,
                scale: 1.0,
                alpha: 1.0,
                volume: 1.0,
            },
            sequence: None,
            body: None,
        }
    }

    /// Clips, on `tracks` unnamed, audible, unlocked tracks.
    fn cap(clips: &[(&str, ClipRecord)], tracks: usize) -> Capture {
        Capture {
            records: clips
                .iter()
                .map(|(id, r)| (id.to_string(), r.clone()))
                .collect(),
            tracks: vec![TrackRecord::default(); tracks],
        }
    }

    /// No clips, and this track table.
    fn table_only(tracks: &[TrackRecord]) -> Capture {
        Capture {
            records: BTreeMap::new(),
            tracks: tracks.to_vec(),
        }
    }

    fn named(name: &str) -> TrackRecord {
        TrackRecord {
            name: name.into(),
            ..TrackRecord::default()
        }
    }

    fn row(id: &str, r: &ClipRecord) -> TimelineClip {
        TimelineClip {
            id: id.into(),
            track: r.track as i32,
            start: r.start as f32,
            duration: r.duration as f32,
            inpoint: r.inpoint as f32,
            name: r.name.as_str().into(),
            kind: ClipKind::Video,
            selected: false,
            thumb: Image::default(),
            rate: r.rate as f32,
            wave: Image::default(),
            wave_secs: 0.0,
        }
    }

    fn picture(width: u32) -> Image {
        Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::new(width, 1))
    }

    fn step(kind: StepKind, subject: &str, before: &Capture, after: &Capture) -> TimelineStep {
        TimelineStep::new(
            kind,
            Some(Subject::Clip(subject.into())),
            "intro.mp4",
            before,
            after,
            HashMap::new(),
        )
    }

    #[test]
    fn the_comparison_finds_changed_added_and_removed_clips() {
        let before = cap(&[("a", rec(0, 0.0, 2.0)), ("b", rec(1, 0.0, 2.0))], 2);
        let after = cap(&[("a", rec(0, 1.0, 2.0)), ("c", rec(1, 4.0, 1.0))], 2);
        let changes = diff(&before, &after);
        assert_eq!(
            changes,
            vec![
                ClipChange {
                    id: "a".into(),
                    before: Some(rec(0, 0.0, 2.0)),
                    after: Some(rec(0, 1.0, 2.0)),
                },
                ClipChange {
                    id: "b".into(),
                    before: Some(rec(1, 0.0, 2.0)),
                    after: None,
                },
                ClipChange {
                    id: "c".into(),
                    before: None,
                    after: Some(rec(1, 4.0, 1.0)),
                },
            ]
        );
    }

    /// A drop that lands where the clip already was is not a step.
    #[test]
    fn an_edit_that_changed_nothing_makes_an_empty_step() {
        let same = cap(&[("a", rec(0, 0.0, 2.0))], 2);
        assert!(step(StepKind::Move, "a", &same, &same).is_empty());
        let one_more_track = cap(&[("a", rec(0, 0.0, 2.0))], 3);
        assert!(!TimelineStep::new(
            StepKind::AddTrack,
            None,
            "",
            &same,
            &one_more_track,
            HashMap::new()
        )
        .is_empty());
    }

    #[test]
    fn a_step_keeps_the_rows_of_clips_that_come_and_go() {
        let before = cap(&[("a", rec(0, 0.0, 2.0)), ("b", rec(1, 0.0, 2.0))], 2);
        let after = cap(&[("a", rec(0, 1.0, 2.0)), ("c", rec(1, 4.0, 1.0))], 2);
        let rows: HashMap<String, TimelineClip> = ["a", "b", "c"]
            .into_iter()
            .map(|id| (id.to_string(), row(id, &rec(0, 0.0, 2.0))))
            .collect();
        let s = TimelineStep::new(
            StepKind::Delete,
            Some(Subject::Clip("b".into())),
            "intro.mp4",
            &before,
            &after,
            rows,
        );
        assert!(
            s.kept_rows.contains_key("b"),
            "b goes, so its row is kept for an undo"
        );
        assert!(
            s.kept_rows.contains_key("c"),
            "c comes, so its row is kept for a redo"
        );
        assert!(!s.kept_rows.contains_key("a"), "a only moved");
    }

    #[test]
    fn gestures_on_the_same_clip_merge_and_nothing_else_does() {
        let (c0, c1) = (
            cap(&[("a", rec(0, 0.0, 2.0))], 2),
            cap(&[("a", rec(0, 1.0, 2.0))], 2),
        );
        for kind in [
            StepKind::Move,
            StepKind::Trim,
            StepKind::Transform,
            StepKind::Duration,
            StepKind::Text,
        ] {
            let first = step(kind, "a", &c0, &c1);
            assert!(first.merges_with(&step(kind, "a", &c1, &c0)), "{kind:?}");
            assert!(
                !first.merges_with(&step(kind, "b", &c1, &c0)),
                "{kind:?} of another clip"
            );
        }
        for kind in [
            StepKind::Add,
            StepKind::Delete,
            StepKind::ReorderTracks,
            StepKind::AddTrack,
            StepKind::Split,
            StepKind::Speed,
        ] {
            assert!(
                !step(kind, "a", &c0, &c1).merges_with(&step(kind, "a", &c1, &c0)),
                "{kind:?} never merges"
            );
        }
        assert!(
            !step(StepKind::Move, "a", &c0, &c1).merges_with(&step(StepKind::Trim, "a", &c1, &c0)),
            "another kind"
        );
    }

    #[test]
    fn absorbing_keeps_the_first_before_and_the_last_after() {
        let c0 = cap(&[("a", rec(0, 0.0, 2.0)), ("b", rec(1, 0.0, 2.0))], 2);
        let c1 = cap(&[("a", rec(0, 1.0, 2.0)), ("b", rec(1, 0.0, 2.0))], 2);
        let c2 = cap(&[("a", rec(0, 3.0, 2.0)), ("b", rec(1, 5.0, 2.0))], 3);
        let mut first = step(StepKind::Move, "a", &c0, &c1);
        first.absorb(step(StepKind::Move, "a", &c1, &c2));
        assert_eq!(
            first.changes,
            diff(&c0, &c2),
            "b, which only the newer step touched, is in too"
        );
        assert_eq!(
            (first.tracks_before.len(), first.tracks_after.len()),
            (2, 3)
        );
    }

    #[test]
    fn a_drag_back_to_where_it_started_absorbs_into_nothing() {
        let c0 = cap(&[("a", rec(0, 0.0, 2.0))], 2);
        let c1 = cap(&[("a", rec(0, 1.0, 2.0))], 2);
        let mut first = step(StepKind::Move, "a", &c0, &c1);
        first.absorb(step(StepKind::Move, "a", &c1, &c0));
        assert!(first.is_empty());
    }

    #[test]
    fn undo_plans_removals_writes_and_restores() {
        let before = cap(&[("a", rec(0, 0.0, 2.0)), ("b", rec(1, 0.0, 2.0))], 2);
        let after = cap(&[("a", rec(0, 1.0, 2.0)), ("c", rec(1, 4.0, 1.0))], 2);
        let s = step(StepKind::Move, "a", &before, &after);
        assert_eq!(
            plan(&s, Direction::Undo),
            Plan {
                removes: vec!["c".into()],
                writes: vec![("a".into(), rec(0, 0.0, 2.0))],
                restores: vec![("b".into(), rec(1, 0.0, 2.0))],
            }
        );
        assert_eq!(target_tracks(&s, Direction::Undo).len(), 2);
    }

    #[test]
    fn redo_is_the_same_plan_the_other_way() {
        let before = cap(&[("b", rec(1, 0.0, 2.0))], 2);
        let after = cap(&[("c", rec(1, 4.0, 1.0))], 3);
        let s = step(StepKind::Delete, "b", &before, &after);
        assert_eq!(
            plan(&s, Direction::Redo),
            Plan {
                removes: vec!["b".into()],
                writes: vec![],
                restores: vec![("c".into(), rec(1, 4.0, 1.0))],
            }
        );
        assert_eq!(target_tracks(&s, Direction::Redo).len(), 3);
    }

    /// Undo removes the right half and writes the left one back whole; redo
    /// writes the left half again and brings the right one back.
    #[test]
    fn a_split_undoes_to_one_clip_and_redoes_to_two() {
        let whole = rec(0, 0.0, 4.0);
        let left = rec(0, 0.0, 1.5);
        let mut right = rec(0, 1.5, 2.5);
        right.inpoint = 1.5;
        let before = cap(&[("a", whole.clone())], 2);
        let after = cap(&[("a", left.clone()), ("b", right.clone())], 2);
        let s = step(StepKind::Split, "a", &before, &after);
        assert_eq!(
            plan(&s, Direction::Undo),
            Plan {
                removes: vec!["b".into()],
                writes: vec![("a".into(), whole)],
                restores: vec![],
            }
        );
        assert_eq!(
            plan(&s, Direction::Redo),
            Plan {
                removes: vec![],
                writes: vec![("a".into(), left)],
                restores: vec![("b".into(), right)],
            }
        );
    }

    #[test]
    fn renaming_a_clip_rewrites_every_mention_of_it() {
        let before = cap(&[("old", rec(0, 0.0, 2.0)), ("other", rec(1, 0.0, 2.0))], 2);
        let after = cap(&[("other", rec(1, 1.0, 2.0))], 2);
        let rows: HashMap<String, TimelineClip> =
            [("old".to_string(), row("old", &rec(0, 0.0, 2.0)))]
                .into_iter()
                .collect();
        let mut s = TimelineStep::new(
            StepKind::Delete,
            Some(Subject::Clip("old".into())),
            "intro.mp4",
            &before,
            &after,
            rows,
        );
        s.rename_clip("old", "new");
        assert_eq!(s.subject, Some(Subject::Clip("new".into())));
        let ids: Vec<&str> = s.changes.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["new", "other"], "only the renamed clip's change");
        assert!(s.kept_rows.contains_key("new") && !s.kept_rows.contains_key("old"));
    }

    #[test]
    fn rows_follow_what_was_applied() {
        let mut shown = row("a", &rec(0, 0.0, 2.0));
        shown.selected = true;
        shown.name = "renamed in the bin".into();
        let rows = vec![shown, row("b", &rec(1, 0.0, 2.0))];
        let mut kept_c = row("c", &rec(0, 0.0, 1.0));
        kept_c.name = "frame_%04d.png".into();
        kept_c.kind = ClipKind::Sequence;
        kept_c.thumb = picture(2);
        kept_c.selected = true;
        let kept: HashMap<String, TimelineClip> = [("c".to_string(), kept_c)].into_iter().collect();
        let mut moved = rec(1, 3.0, 1.5);
        moved.inpoint = 0.5;
        let mut still = rec(2, 6.0, 1.0);
        still.uri = "file:///C:/media/still.png".into();
        still.name = "still.png".into();
        let applied = vec![
            Applied {
                id: "b".into(),
                now_id: "b".into(),
                record: None,
            },
            Applied {
                id: "c".into(),
                now_id: "c2".into(),
                record: Some(rec(1, 4.0, 1.0)),
            },
            Applied {
                id: "d".into(),
                now_id: "d".into(),
                record: Some(still),
            },
            Applied {
                id: "a".into(),
                now_id: "a".into(),
                record: Some(moved),
            },
        ];
        let out = rows_after(&rows, &applied, &kept);
        let ids: Vec<&str> = out.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["a", "c2", "d"],
            "b gone, c back under the ID it has now, d back"
        );
        let a = &out[0];
        assert_eq!(
            (a.track, a.start, a.duration, a.inpoint),
            (1, 3.0, 1.5, 0.5),
            "an updated row takes every part of the new place"
        );
        assert!(a.selected, "and keeps its selection");
        assert_eq!(a.name.as_str(), "renamed in the bin", "and its name");
        let c = &out[1];
        assert_eq!((c.track, c.start, c.duration), (1, 4.0, 1.0));
        assert_eq!(
            c.name.as_str(),
            "frame_%04d.png",
            "the kept row's name, not the record's"
        );
        assert_eq!(c.kind, ClipKind::Sequence);
        assert_eq!(c.thumb.size().width, 2, "and its picture");
        assert!(!c.selected, "the selection is decided afterwards");
        let d = &out[2];
        assert_eq!(
            d.name.as_str(),
            "still.png",
            "no kept row: built from the record"
        );
        assert_eq!(d.kind, ClipKind::Image);
        assert_eq!((d.track, d.start, d.duration), (2, 6.0, 1.0));
    }

    #[test]
    fn the_steps_own_clip_is_selected_first() {
        let rows = vec![row("a", &rec(0, 0.0, 2.0)), row("b", &rec(1, 0.0, 2.0))];
        assert_eq!(selection_after(&rows, Some("b"), Some("a")), 1);
        assert_eq!(
            selection_after(&rows, Some("gone"), Some("a")),
            0,
            "else the old selection"
        );
        assert_eq!(
            selection_after(&rows, None, Some("gone")),
            -1,
            "else nothing"
        );
    }

    #[test]
    fn each_kind_describes_itself() {
        let c = cap(&[], 2);
        let d = |kind| {
            TimelineStep::new(
                kind,
                Some(Subject::Clip("a".into())),
                "intro.mp4",
                &c,
                &c,
                HashMap::new(),
            )
            .describe()
        };
        assert_eq!(d(StepKind::Move), "moving intro.mp4");
        assert_eq!(d(StepKind::Trim), "trimming intro.mp4");
        assert_eq!(d(StepKind::Transform), "transforming intro.mp4");
        assert_eq!(d(StepKind::Duration), "changing the duration of intro.mp4");
        assert_eq!(d(StepKind::Add), "adding intro.mp4");
        assert_eq!(d(StepKind::Delete), "deleting intro.mp4");
        assert_eq!(d(StepKind::ReorderTracks), "reordering tracks");
        assert_eq!(d(StepKind::AddTrack), "adding a track");
        assert_eq!(d(StepKind::Split), "splitting intro.mp4");
        assert_eq!(d(StepKind::Speed), "changing the speed of intro.mp4");
        assert_eq!(d(StepKind::Text), "editing the text of intro.mp4");
    }

    /// A step about track `t`, going from one table to another, named as the
    /// recorder names it: by the track's label before the step.
    fn track_step(
        kind: StepKind,
        t: usize,
        before: &[TrackRecord],
        after: &[TrackRecord],
    ) -> TimelineStep {
        TimelineStep::new(
            kind,
            Some(Subject::Track(t)),
            &tracks::label(before, t),
            &table_only(before),
            &table_only(after),
            HashMap::new(),
        )
    }

    /// Which way a toggle went is read off the step's own tables, and the
    /// hint names the track as it was called before the step.
    #[test]
    fn track_steps_describe_which_way_they_went() {
        let plain = [TrackRecord::default(), named("Dialogue")];
        let mut muted = plain.clone();
        muted[1].muted = true;
        let mut locked = plain.clone();
        locked[1].locked = true;
        let mut renamed = plain.clone();
        renamed[0].name = "Music".into();
        let d =
            |kind, t, b: &[TrackRecord], a: &[TrackRecord]| track_step(kind, t, b, a).describe();
        assert_eq!(d(StepKind::MuteTrack, 1, &plain, &muted), "muting Dialogue");
        assert_eq!(
            d(StepKind::MuteTrack, 1, &muted, &plain),
            "unmuting Dialogue"
        );
        assert_eq!(
            d(StepKind::LockTrack, 1, &plain, &locked),
            "locking Dialogue"
        );
        assert_eq!(
            d(StepKind::LockTrack, 1, &locked, &plain),
            "unlocking Dialogue"
        );
        assert_eq!(
            d(StepKind::RenameTrack, 0, &plain, &renamed),
            "renaming Track 1"
        );
    }

    /// A rename is typed: its keystrokes are one step. Renames of different
    /// tracks are not, and neither is a mute or a lock, which is one click.
    #[test]
    fn renames_of_one_track_merge_and_nothing_else_about_tracks_does() {
        let t0 = [named("A"), TrackRecord::default()];
        let t1 = [named("Ab"), TrackRecord::default()];
        let t2 = [named("Abc"), TrackRecord::default()];
        let first = track_step(StepKind::RenameTrack, 0, &t0, &t1);
        assert!(first.merges_with(&track_step(StepKind::RenameTrack, 0, &t1, &t2)));
        assert!(!first.merges_with(&track_step(StepKind::RenameTrack, 1, &t1, &t2)));
        for kind in [StepKind::MuteTrack, StepKind::LockTrack] {
            assert!(
                !track_step(kind, 0, &t0, &t1).merges_with(&track_step(kind, 0, &t1, &t0)),
                "{kind:?} never merges"
            );
        }
    }

    /// Typing a name and then typing the old one back within a second is no
    /// rename at all: the history drops the step, and the next change starts
    /// one of its own rather than merging into whatever came before.
    #[test]
    fn a_rename_typed_back_to_the_old_name_leaves_nothing() {
        let t0 = [named("A")];
        let t1 = [named("B")];
        let now = Instant::now();
        let mut history = History::new();
        history.record(track_step(StepKind::RenameTrack, 0, &t0, &t1), now);
        history.record(
            track_step(StepKind::RenameTrack, 0, &t1, &t0),
            now + std::time::Duration::from_millis(300),
        );
        assert!(!history.can_undo(), "nothing changed after all");
    }

    /// Muting and unmuting straight away is two clicks and two steps.
    #[test]
    fn a_mute_and_an_unmute_are_two_steps() {
        let plain = [named("Dialogue")];
        let muted = [TrackRecord {
            muted: true,
            ..named("Dialogue")
        }];
        let now = Instant::now();
        let mut history = History::new();
        history.record(track_step(StepKind::MuteTrack, 0, &plain, &muted), now);
        history.record(
            track_step(StepKind::MuteTrack, 0, &muted, &plain),
            now + std::time::Duration::from_millis(100),
        );
        assert_eq!(history.undo_hint(), "Undo unmuting Dialogue");
        history.commit_undo();
        assert_eq!(history.undo_hint(), "Undo muting Dialogue");
    }

    /// Undoing a track's step selects no clip: the one selected stays.
    #[test]
    fn a_track_step_is_not_a_clip_to_select() {
        assert_eq!(Subject::Track(0).clip(), None);
        assert_eq!(Subject::Clip("a".into()).clip(), Some("a"));
    }

    fn recorder(rows: Vec<TimelineClip>, tracks: usize) -> Recorder {
        Recorder {
            history: Rc::new(RefCell::new(History::new())),
            tl_clips: Rc::new(VecModel::from(rows)),
            tracks: Rc::new(VecModel::from(vec![TimelineTrack::default(); tracks])),
            ui: slint::Weak::default(),
        }
    }

    /// A delete is recorded while the row is still there, so the step knows
    /// the clip's name and keeps its row for the undo.
    #[test]
    fn the_recorder_names_the_clip_and_keeps_its_row() {
        let mut shown = row("a", &rec(0, 0.0, 2.0));
        shown.name = "intro (bin name).mp4".into();
        let r = recorder(vec![shown], 2);
        let before = cap(&[("a", rec(0, 0.0, 2.0))], 2);
        let after = cap(&[], 2);
        r.record_captures(
            StepKind::Delete,
            Some(Subject::Clip("a".into())),
            before,
            after,
        );
        let history = r.history.borrow();
        let s = history.peek_undo().expect("a step");
        assert_eq!(s.describe(), "deleting intro (bin name).mp4");
        assert_eq!(s.kept_rows["a"].name.as_str(), "intro (bin name).mp4");
    }

    #[test]
    fn a_new_track_is_recorded_without_a_project() {
        let r = recorder(Vec::new(), 2);
        let before = r.before(None);
        r.tracks.push(TimelineTrack::default());
        r.record(None, StepKind::AddTrack, None, before);
        let history = r.history.borrow();
        let s = history.peek_undo().expect("a step");
        assert_eq!((s.tracks_before.len(), s.tracks_after.len()), (2, 3));
    }

    #[test]
    fn an_edit_that_changed_nothing_records_nothing() {
        let r = recorder(vec![row("a", &rec(0, 0.0, 2.0))], 2);
        let same = cap(&[("a", rec(0, 0.0, 2.0))], 2);
        r.record_captures(
            StepKind::Move,
            Some(Subject::Clip("a".into())),
            same.clone(),
            same,
        );
        assert!(!r.history.borrow().can_undo());
    }

    /// An add is recorded after its row is pushed, so the step keeps the row a
    /// redo brings back.
    #[test]
    fn the_recorder_keeps_the_row_of_an_added_clip() {
        let r = recorder(Vec::new(), 2);
        let before = cap(&[], 2);
        r.tl_clips.push(row("a", &rec(0, 0.0, 2.0)));
        let after = cap(&[("a", rec(0, 0.0, 2.0))], 2);
        r.record_captures(
            StepKind::Add,
            Some(Subject::Clip("a".into())),
            before,
            after,
        );
        let history = r.history.borrow();
        let s = history.peek_undo().expect("a step");
        assert_eq!(s.describe(), "adding intro.mp4");
        assert!(s.kept_rows.contains_key("a"), "the row a redo brings back");
    }

    /// After a write failed partway, the rows are rebuilt from what the engine
    /// actually holds, so the screen never disagrees with the edit.
    #[test]
    fn rows_can_be_resynced_from_the_engine() {
        let rows = vec![row("a", &rec(0, 0.0, 2.0)), row("gone", &rec(1, 0.0, 2.0))];
        let engine = vec![
            (kuvatin_video::ClipId("a".into()), rec(0, 5.0, 2.0)),
            (kuvatin_video::ClipId("new".into()), rec(1, 1.0, 1.0)),
        ];
        let applied = applied_from_engine(&rows, engine);
        let out = rows_after(&rows, &applied, &HashMap::new());
        let ids: Vec<&str> = out.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "new"]);
        assert_eq!(out[0].start, 5.0);
    }

    #[test]
    fn track_rows_grow_shrink_and_change_in_place() {
        let rows = VecModel::from(vec![TimelineTrack::default(); 2]);
        let mut want = vec![TrackRecord::default(); 4];
        want[1].muted = true;
        set_track_rows(&rows, &want);
        assert_eq!(tracks::records(&tracks::rows_of(&rows)), want);
        want.truncate(1);
        set_track_rows(&rows, &want);
        assert_eq!(rows.row_count(), 1);
        want[0].locked = true;
        set_track_rows(&rows, &want);
        assert!(rows.row_data(0).unwrap().locked, "changed in place");
    }

    /// The bug this table exists for: an undo regenerated every track's name
    /// from its index, so a rename was silently lost to the next Ctrl+Z.
    #[test]
    fn track_rows_take_their_names_from_the_table_not_the_index() {
        let rows = VecModel::from(vec![TimelineTrack::default(); 2]);
        set_track_rows(&rows, &[named("Music"), named("Dialogue")]);
        let names: Vec<String> = tracks::rows_of(&rows)
            .iter()
            .map(|r| r.name.to_string())
            .collect();
        assert_eq!(names, vec!["Music", "Dialogue"]);
    }

    /// Undo does not change what you are listening to.
    #[test]
    fn track_rows_keep_their_solo() {
        let rows = VecModel::from(vec![TimelineTrack {
            soloed: true,
            ..TimelineTrack::default()
        }]);
        set_track_rows(&rows, &[named("Dialogue")]);
        let row = rows.row_data(0).unwrap();
        assert!(row.soloed);
        assert_eq!(row.name.as_str(), "Dialogue");
    }

    /// A rename, a mute or a lock changes no clip, and is still a step. With
    /// a bare count on each side it compared equal and was thrown away.
    #[test]
    fn a_step_that_changes_only_the_track_table_is_not_empty() {
        let plain = [TrackRecord::default(), TrackRecord::default()];
        let mut muted = plain.clone();
        muted[1].muted = true;
        let mut locked = plain.clone();
        locked[0].locked = true;
        let mut renamed = plain.clone();
        renamed[1].name = "Dialogue".into();
        for other in [muted, locked, renamed] {
            let s = TimelineStep::new(
                StepKind::AddTrack,
                None,
                "",
                &table_only(&plain),
                &table_only(&other),
                HashMap::new(),
            );
            assert!(!s.is_empty(), "{other:?}");
        }
        let same = TimelineStep::new(
            StepKind::AddTrack,
            None,
            "",
            &table_only(&plain),
            &table_only(&plain),
            HashMap::new(),
        );
        assert!(same.is_empty(), "identical tables and no clips");
    }

    #[test]
    fn undo_goes_to_the_before_table_and_redo_to_the_after() {
        let s = TimelineStep::new(
            StepKind::AddTrack,
            None,
            "",
            &table_only(&[named("A")]),
            &table_only(&[named("B")]),
            HashMap::new(),
        );
        assert_eq!(target_tracks(&s, Direction::Undo), vec![named("A")]);
        assert_eq!(target_tracks(&s, Direction::Redo), vec![named("B")]);
    }

    /// The recorder reads the table from the rows, so a change to a row
    /// between the two reads is what the step holds.
    #[test]
    fn the_recorder_reads_the_track_table_from_the_rows() {
        let r = recorder(Vec::new(), 2);
        let before = r.before(None);
        let mut row = r.tracks.row_data(1).unwrap();
        row.name = "Dialogue".into();
        r.tracks.set_row_data(1, row);
        r.record(None, StepKind::AddTrack, None, before);
        let history = r.history.borrow();
        let s = history.peek_undo().expect("a step");
        assert_eq!(s.tracks_before[1].name, "");
        assert_eq!(s.tracks_after[1].name, "Dialogue");
    }

    #[test]
    fn a_row_takes_its_speed_from_the_record() {
        let mut shown = row("a", &rec(0, 0.0, 4.0));
        shown.thumb = picture(3);
        let mut fast = rec(0, 0.0, 2.0);
        fast.rate = 2.0;
        let applied = vec![Applied {
            id: "a".into(),
            now_id: "a".into(),
            record: Some(fast),
        }];
        let out = rows_after(&[shown], &applied, &HashMap::new());
        assert_eq!(out[0].rate, 2.0, "undo puts the speed back on the row");
        assert_eq!(out[0].duration, 2.0);
        assert_eq!(out[0].thumb.size().width, 3, "and leaves the picture alone");
    }

    /// The waveform belongs to the source, not the record: an undo moves the
    /// row and leaves its picture of the sound alone.
    #[test]
    fn a_row_keeps_its_waveform_through_an_undo() {
        let mut shown = row("a", &rec(0, 0.0, 4.0));
        shown.wave = picture(5);
        shown.wave_secs = 12.5;
        let applied = vec![Applied {
            id: "a".into(),
            now_id: "a".into(),
            record: Some(rec(1, 2.0, 3.0)),
        }];
        let out = rows_after(&[shown], &applied, &HashMap::new());
        assert_eq!((out[0].track, out[0].start), (1, 2.0));
        assert_eq!(out[0].wave.size().width, 5);
        assert_eq!(out[0].wave_secs, 12.5);
    }

    /// A rate-only change is one changed clip, and undoes as one write.
    #[test]
    fn a_speed_change_is_one_changed_clip() {
        let normal = rec(0, 0.0, 4.0);
        let mut other = normal.clone();
        other.rate = 2.0;
        let c0 = cap(&[("a", normal.clone())], 2);
        let c1 = cap(&[("a", other)], 2);
        assert_eq!(diff(&c0, &c1).len(), 1);
        assert_eq!(
            plan(&step(StepKind::Speed, "a", &c0, &c1), Direction::Undo),
            Plan {
                removes: vec![],
                writes: vec![("a".into(), normal)],
                restores: vec![],
            }
        );
    }

    fn title(text: &str, start: f64) -> ClipRecord {
        let body = kuvatin_video::TitleRecord {
            text: text.into(),
            ..Default::default()
        };
        ClipRecord {
            uri: String::new(),
            name: body.name(),
            body: Some(kuvatin_video::ClipBody::Title(body)),
            ..rec(0, start, 5.0)
        }
    }

    /// A title has no source: undo never asks whether one is there, and
    /// never names it as gone.
    #[test]
    fn title_restores_never_ask_for_a_source() {
        let restores = vec![
            ("t".to_string(), title("Hello", 0.0)),
            ("v".to_string(), rec(1, 0.0, 2.0)),
        ];
        let asked = RefCell::new(Vec::new());
        let missing = missing_sources(&restores, &HashMap::new(), |uri| {
            asked.borrow_mut().push(uri.to_string());
            false
        });
        assert_eq!(missing, vec!["intro.mp4".to_string()]);
        assert_eq!(
            *asked.borrow(),
            vec!["file:///C:/media/intro.mp4".to_string()]
        );
    }

    /// A title coming back is amber and named by its text; a title whose
    /// text changes takes the new name, which a media clip never does.
    #[test]
    fn title_rows_take_their_kind_and_name_from_the_record() {
        let rows = vec![row("v", &rec(1, 0.0, 2.0)), {
            let mut r = row("t", &title("Before", 0.0));
            r.kind = ClipKind::Title;
            r
        }];
        let mut renamed = rec(1, 0.0, 2.0);
        renamed.name = "other.mp4".into();
        let applied = vec![
            Applied {
                id: "t".into(),
                now_id: "t".into(),
                record: Some(title("After", 0.0)),
            },
            Applied {
                id: "v".into(),
                now_id: "v".into(),
                record: Some(renamed),
            },
            Applied {
                id: "back".into(),
                now_id: "back".into(),
                record: Some(title("Returned", 6.0)),
            },
        ];
        let out = rows_after(&rows, &applied, &HashMap::new());
        assert_eq!(
            out[0].name.as_str(),
            "intro.mp4",
            "a media clip keeps its name"
        );
        assert_eq!(out[1].name.as_str(), "After");
        assert_eq!(out[2].name.as_str(), "Returned");
        assert_eq!(out[2].kind, ClipKind::Title);
    }
}
