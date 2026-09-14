//! Undo and redo for the timeline. A step is what an edit changed, clip by
//! clip: each affected clip's record before and after, read from the engine
//! around the edit (`Project::clip_records`, about 0.2 ms at 50 clips). Undo
//! writes the old records back exactly, so a clip returns where it was, with
//! the same ID, transform and thumbnail.

use super::project_file::kind_of;
use crate::gui::history::Step;
use crate::gui::TimelineClip;
use kuvatin_video::ClipRecord;
use slint::Image;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

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
}

impl StepKind {
    /// The kinds a continuous gesture produces. Only these merge.
    fn merges(self) -> bool {
        matches!(
            self,
            StepKind::Move | StepKind::Trim | StepKind::Transform | StepKind::Duration
        )
    }
}

/// Every clip's record, by ID, and the number of track rows, at one moment.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Capture {
    pub(super) records: BTreeMap<String, ClipRecord>,
    pub(super) tracks: usize,
}

impl Capture {
    /// Read the timeline as it is now. No project yet means no clips.
    pub(super) fn of(project: Option<&kuvatin_video::Project>, tracks: usize) -> Self {
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
    /// The clip the step is about, when it is about one.
    pub(super) subject: Option<String>,
    /// That clip's display name, for the hint.
    pub(super) name: String,
    pub(super) changes: Vec<ClipChange>,
    /// Thumbnails of the clips that exist on only one side, by ID, so a
    /// restored row gets its picture back without decoding.
    pub(super) thumbs: HashMap<String, Image>,
    pub(super) tracks_before: usize,
    pub(super) tracks_after: usize,
}

impl TimelineStep {
    pub(super) fn new(
        kind: StepKind,
        subject: Option<&str>,
        name: &str,
        before: &Capture,
        after: &Capture,
        thumbs: HashMap<String, Image>,
    ) -> Self {
        let changes = diff(before, after);
        let one_sided: HashSet<&str> = changes
            .iter()
            .filter(|c| c.before.is_none() != c.after.is_none())
            .map(|c| c.id.as_str())
            .collect();
        let thumbs = thumbs
            .into_iter()
            .filter(|(id, _)| one_sided.contains(id.as_str()))
            .collect();
        TimelineStep {
            kind,
            subject: subject.map(str::to_string),
            name: name.to_string(),
            changes,
            thumbs,
            tracks_before: before.tracks,
            tracks_after: after.tracks,
        }
    }

    /// Replace a clip's ID everywhere in the step: the engine restored it
    /// under a new one.
    pub(super) fn rename_clip(&mut self, old: &str, new: &str) {
        if self.subject.as_deref() == Some(old) {
            self.subject = Some(new.to_string());
        }
        for change in &mut self.changes {
            if change.id == old {
                change.id = new.to_string();
            }
        }
        if let Some(thumb) = self.thumbs.remove(old) {
            self.thumbs.insert(new.to_string(), thumb);
        }
    }
}

impl Step for TimelineStep {
    fn describe(&self) -> String {
        let name = &self.name;
        match self.kind {
            StepKind::Move => format!("move of {name}"),
            StepKind::Trim => format!("trim of {name}"),
            StepKind::Transform => format!("transform of {name}"),
            StepKind::Duration => format!("duration of {name}"),
            StepKind::Add => format!("adding {name}"),
            StepKind::Delete => format!("deleting {name}"),
            StepKind::ReorderTracks => "track reorder".into(),
            StepKind::AddTrack => "new track".into(),
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
        self.thumbs.extend(newer.thumbs);
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

/// One engine operation an undo or redo needs.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Write {
    /// The clip must not exist.
    Remove(String),
    /// The clip is gone and comes back with this record.
    Restore(String, ClipRecord),
    /// The clip exists and takes this record.
    Set(String, ClipRecord),
}

/// The engine operations that take the timeline to one side of `step`, in the
/// order they must run: removals, then restores, then writes. Removing first
/// frees the places and names the restored clips come back into.
pub(super) fn plan(step: &TimelineStep, dir: Direction) -> Vec<Write> {
    let (mut removes, mut restores, mut sets) = (Vec::new(), Vec::new(), Vec::new());
    for change in &step.changes {
        let (from, to) = match dir {
            Direction::Undo => (&change.after, &change.before),
            Direction::Redo => (&change.before, &change.after),
        };
        match (from, to) {
            (Some(_), None) => removes.push(Write::Remove(change.id.clone())),
            (None, Some(record)) => {
                restores.push(Write::Restore(change.id.clone(), record.clone()))
            }
            (Some(_), Some(record)) => sets.push(Write::Set(change.id.clone(), record.clone())),
            (None, None) => {}
        }
    }
    removes.into_iter().chain(restores).chain(sets).collect()
}

/// How many track rows the timeline shows on the `dir` side of `step`.
pub(super) fn target_tracks(step: &TimelineStep, dir: Direction) -> usize {
    match dir {
        Direction::Undo => step.tracks_before,
        Direction::Redo => step.tracks_after,
    }
}

/// What one write did, as the rows need it: the clip's ID in the step, the ID
/// it has now (a restore can hand back a new one), and its record, or `None`
/// if the clip is gone.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Applied {
    pub(super) id: String,
    pub(super) now_id: String,
    pub(super) record: Option<ClipRecord>,
}

/// The timeline rows after `applied`. A gone clip loses its row; a changed
/// clip keeps its row (name, thumbnail, selection) with the new geometry; a
/// restored clip gets a row back, with the thumbnail the step kept.
pub(super) fn rows_after(
    rows: &[TimelineClip],
    applied: &[Applied],
    thumbs: &HashMap<String, Image>,
) -> Vec<TimelineClip> {
    let mut out = rows.to_vec();
    for a in applied {
        let at = out.iter().position(|r| r.id.as_str() == a.id);
        match (&a.record, at) {
            (None, Some(i)) => {
                out.remove(i);
            }
            (None, None) => {}
            (Some(record), Some(i)) => {
                let row = &mut out[i];
                row.id = a.now_id.as_str().into();
                row.track = record.track as i32;
                row.start = record.start as f32;
                row.duration = record.duration as f32;
                row.inpoint = record.inpoint as f32;
            }
            (Some(record), None) => out.push(TimelineClip {
                id: a.now_id.as_str().into(),
                track: record.track as i32,
                start: record.start as f32,
                duration: record.duration as f32,
                inpoint: record.inpoint as f32,
                name: record.name.as_str().into(),
                kind: kind_of(&record.uri),
                selected: false,
                thumb: thumbs.get(&a.id).cloned().unwrap_or_default(),
            }),
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::ClipKind;
    use kuvatin_video::LayoutRecord;

    fn rec(track: usize, start: f64, duration: f64) -> ClipRecord {
        ClipRecord {
            uri: "file:///C:/media/intro.mp4".into(),
            name: "intro.mp4".into(),
            track,
            start,
            inpoint: 0.0,
            duration,
            layout: LayoutRecord {
                posx: 0,
                posy: 0,
                scale: 1.0,
                alpha: 1.0,
                volume: 1.0,
            },
            sequence: None,
        }
    }

    fn cap(clips: &[(&str, ClipRecord)], tracks: usize) -> Capture {
        Capture {
            records: clips
                .iter()
                .map(|(id, r)| (id.to_string(), r.clone()))
                .collect(),
            tracks,
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
        }
    }

    fn step(kind: StepKind, subject: &str, before: &Capture, after: &Capture) -> TimelineStep {
        TimelineStep::new(
            kind,
            Some(subject),
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
    fn a_step_keeps_thumbnails_only_for_clips_that_come_and_go() {
        let before = cap(&[("a", rec(0, 0.0, 2.0)), ("b", rec(1, 0.0, 2.0))], 2);
        let after = cap(&[("a", rec(0, 1.0, 2.0))], 2);
        let thumbs: HashMap<String, Image> = [("a", Image::default()), ("b", Image::default())]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let s = TimelineStep::new(
            StepKind::Delete,
            Some("b"),
            "intro.mp4",
            &before,
            &after,
            thumbs,
        );
        assert!(
            s.thumbs.contains_key("b"),
            "b disappears, so its picture is kept"
        );
        assert!(!s.thumbs.contains_key("a"), "a only moved");
    }

    #[test]
    fn moves_of_the_same_clip_merge_and_nothing_else_does() {
        let (c0, c1) = (
            cap(&[("a", rec(0, 0.0, 2.0))], 2),
            cap(&[("a", rec(0, 1.0, 2.0))], 2),
        );
        let mv = step(StepKind::Move, "a", &c0, &c1);
        assert!(mv.merges_with(&step(StepKind::Move, "a", &c1, &c0)));
        assert!(
            !mv.merges_with(&step(StepKind::Trim, "a", &c1, &c0)),
            "another kind"
        );
        assert!(
            !mv.merges_with(&step(StepKind::Move, "b", &c1, &c0)),
            "another clip"
        );
        let del = step(StepKind::Delete, "a", &c0, &c1);
        assert!(
            !del.merges_with(&step(StepKind::Delete, "a", &c0, &c1)),
            "deletes never merge"
        );
    }

    #[test]
    fn absorbing_keeps_the_first_before_and_the_last_after() {
        let c0 = cap(&[("a", rec(0, 0.0, 2.0))], 2);
        let c1 = cap(&[("a", rec(0, 1.0, 2.0))], 2);
        let c2 = cap(&[("a", rec(0, 3.0, 2.0))], 3);
        let mut first = step(StepKind::Move, "a", &c0, &c1);
        first.absorb(step(StepKind::Move, "a", &c1, &c2));
        assert_eq!(first.changes.len(), 1);
        assert_eq!(first.changes[0].before, Some(rec(0, 0.0, 2.0)));
        assert_eq!(first.changes[0].after, Some(rec(0, 3.0, 2.0)));
        assert_eq!((first.tracks_before, first.tracks_after), (2, 3));
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
    fn undo_removes_then_restores_then_writes() {
        let before = cap(&[("a", rec(0, 0.0, 2.0)), ("b", rec(1, 0.0, 2.0))], 2);
        let after = cap(&[("a", rec(0, 1.0, 2.0)), ("c", rec(1, 4.0, 1.0))], 2);
        let s = step(StepKind::Move, "a", &before, &after);
        assert_eq!(
            plan(&s, Direction::Undo),
            vec![
                Write::Remove("c".into()),
                Write::Restore("b".into(), rec(1, 0.0, 2.0)),
                Write::Set("a".into(), rec(0, 0.0, 2.0)),
            ]
        );
        assert_eq!(target_tracks(&s, Direction::Undo), 2);
    }

    #[test]
    fn redo_is_the_same_plan_the_other_way() {
        let before = cap(&[("b", rec(1, 0.0, 2.0))], 2);
        let after = cap(&[("c", rec(1, 4.0, 1.0))], 3);
        let s = step(StepKind::Delete, "b", &before, &after);
        assert_eq!(
            plan(&s, Direction::Redo),
            vec![
                Write::Remove("b".into()),
                Write::Restore("c".into(), rec(1, 4.0, 1.0)),
            ]
        );
        assert_eq!(target_tracks(&s, Direction::Redo), 3);
    }

    #[test]
    fn renaming_a_clip_rewrites_every_mention_of_it() {
        let before = cap(&[("old", rec(0, 0.0, 2.0))], 2);
        let after = cap(&[], 2);
        let thumbs: HashMap<String, Image> = [("old".to_string(), Image::default())]
            .into_iter()
            .collect();
        let mut s = TimelineStep::new(
            StepKind::Delete,
            Some("old"),
            "intro.mp4",
            &before,
            &after,
            thumbs,
        );
        s.rename_clip("old", "new");
        assert_eq!(s.subject.as_deref(), Some("new"));
        assert_eq!(s.changes[0].id, "new");
        assert!(s.thumbs.contains_key("new") && !s.thumbs.contains_key("old"));
    }

    #[test]
    fn rows_follow_what_was_applied() {
        let mut kept = row("a", &rec(0, 0.0, 2.0));
        kept.selected = true;
        kept.name = "renamed in the bin".into();
        let rows = vec![kept, row("b", &rec(1, 0.0, 2.0))];
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
                id: "a".into(),
                now_id: "a".into(),
                record: Some(rec(0, 3.0, 2.0)),
            },
        ];
        let out = rows_after(&rows, &applied, &HashMap::new());
        assert_eq!(out.len(), 2, "b gone, c back");
        assert_eq!(out[0].id.as_str(), "a");
        assert_eq!(out[0].start, 3.0);
        assert!(out[0].selected, "an updated row keeps its selection");
        assert_eq!(out[0].name.as_str(), "renamed in the bin", "and its name");
        assert_eq!(
            out[1].id.as_str(),
            "c2",
            "a restored clip gets the ID it has now"
        );
        assert_eq!(out[1].name.as_str(), "intro.mp4");
        assert_eq!(out[1].kind, ClipKind::Video);
        assert_eq!((out[1].track, out[1].start, out[1].duration), (1, 4.0, 1.0));
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
            TimelineStep::new(kind, Some("a"), "intro.mp4", &c, &c, HashMap::new()).describe()
        };
        assert_eq!(d(StepKind::Move), "move of intro.mp4");
        assert_eq!(d(StepKind::Trim), "trim of intro.mp4");
        assert_eq!(d(StepKind::Transform), "transform of intro.mp4");
        assert_eq!(d(StepKind::Duration), "duration of intro.mp4");
        assert_eq!(d(StepKind::Add), "adding intro.mp4");
        assert_eq!(d(StepKind::Delete), "deleting intro.mp4");
        assert_eq!(d(StepKind::ReorderTracks), "track reorder");
        assert_eq!(d(StepKind::AddTrack), "new track");
    }
}
