//! Per-track state in the Videos timeline: names, mutes, solo and locks.
//!
//! A track is its position (guiding decision 1 of the track controls
//! design). The interface's rows are the truth; the engine is told what to
//! silence and nothing else, and the undo history keeps the rows as
//! [`TrackRecord`]s. Solo is the one flag a record leaves out: it is a way of
//! listening, not an edit, so it is neither saved nor undone.

use super::undo::{Recorder, StepKind, Subject};
use crate::gui::{AppWindow, TimelineTrack};
use kuvatin_video::TrackRecord;
use slint::{Model, VecModel};

/// Wire the track header's controls: mute, solo, lock and rename.
pub(super) fn wire(ui: &AppWindow, st: &super::VideoState) {
    let rec = st.recorder(ui);
    {
        let project = st.project.clone();
        let rec = rec.clone();
        ui.on_track_muted(move |t, on| {
            edit_row(&project, &rec, t, StepKind::MuteTrack, |r| r.muted = on);
        });
    }
    {
        let project = st.project.clone();
        let rec = rec.clone();
        ui.on_track_locked(move |t, on| {
            edit_row(&project, &rec, t, StepKind::LockTrack, |r| r.locked = on);
        });
    }
    {
        let project = st.project.clone();
        let rec = rec.clone();
        ui.on_track_renamed(move |t, name| {
            // Blank or spaces only: back to "Track N".
            let name = name.trim().to_string();
            edit_row(&project, &rec, t, StepKind::RenameTrack, |r| {
                r.name = name.as_str().into()
            });
        });
    }
    // Solo is for listening: it changes what the engine silences and nothing
    // else. Not the history, not the file, not unsaved work.
    {
        let project = st.project.clone();
        let rows = st.tracks.clone();
        ui.on_track_soloed(move |t, on| {
            let Some(i) = usize::try_from(t).ok() else {
                return;
            };
            let Some(mut row) = rows.row_data(i) else {
                return;
            };
            row.soloed = on;
            rows.set_row_data(i, row);
            if let Some(p) = project.borrow_mut().as_mut() {
                push_mutes(p, &rows);
            }
        });
    }
}

/// Change the track row at `t` as an edit: recorded as a step of `kind`,
/// pushed to the engine, and counted as unsaved work. A change that leaves
/// the row as it was does nothing at all. Works before the engine exists,
/// as adding a track does: the step needs no clips.
fn edit_row(
    project: &super::ProjectSlot,
    rec: &Recorder,
    t: i32,
    kind: StepKind,
    change: impl FnOnce(&mut TimelineTrack),
) {
    let Some(i) = usize::try_from(t).ok() else {
        return;
    };
    let Some(was) = rec.tracks.row_data(i) else {
        return;
    };
    let mut row = was.clone();
    change(&mut row);
    if row == was {
        return;
    }
    let mut slot = project.borrow_mut();
    let before = rec.before(slot.as_ref());
    rec.tracks.set_row_data(i, row);
    if let Some(p) = slot.as_mut() {
        push_mutes(p, &rec.tracks);
        // The engine cannot tell a name or a lock changed; a mute it does
        // not count, because solo drives the same call.
        p.mark_unsaved();
    }
    rec.record(slot.as_ref(), kind, Some(Subject::Track(i)), before);
}

/// What the engine should silence, given the rows as they are. While any
/// track is soloed every other one is silent; an explicit mute wins over
/// solo, so a track both soloed and muted stays silent.
pub(super) fn effective_mutes(rows: &[TimelineTrack]) -> Vec<bool> {
    let any_solo = rows.iter().any(|r| r.soloed);
    rows.iter()
        .map(|r| r.muted || (any_solo && !r.soloed))
        .collect()
}

/// Whether the track at `t` refuses edits. A track past the end, or no track
/// at all (-1), is not locked: a new bottom track never is.
pub(super) fn locked(rows: &[TimelineTrack], t: i32) -> bool {
    usize::try_from(t)
        .ok()
        .and_then(|t| rows.get(t))
        .is_some_and(|r| r.locked)
}

/// The first of `touched` that is locked: the track an edit touching those
/// tracks must be refused for. A drop touches the track the clip is on and
/// the one it would land on.
pub(super) fn first_locked(rows: &[TimelineTrack], touched: &[i32]) -> Option<usize> {
    touched
        .iter()
        .copied()
        .find(|&t| locked(rows, t))
        .map(|t| t as usize)
}

/// What an edit refused on the locked track `t` says.
pub(super) fn refusal(rows: &[TimelineTrack], t: usize) -> (String, String) {
    (
        format!("{} is locked", label(&records(rows), t)),
        "Unlock the track to change what is on it.".into(),
    )
}

/// Refuse an edit that touches a locked track, saying which track and how to
/// get past it. True when the edit must not go ahead. A locked track's clips
/// lose their handles on screen, so this is for the keyboard, and for a
/// click that got there first.
pub(super) fn refuse_locked(ui: &AppWindow, rows: &[TimelineTrack], touched: &[i32]) -> bool {
    let Some(t) = first_locked(rows, touched) else {
        return false;
    };
    let (title, detail) = refusal(rows, t);
    crate::gui::show_error(ui, &title, detail);
    true
}

/// The rows as they are stored and undone: name, mute and lock. Solo is left
/// out.
pub(super) fn records(rows: &[TimelineTrack]) -> Vec<TrackRecord> {
    rows.iter()
        .map(|r| TrackRecord {
            name: r.name.to_string(),
            muted: r.muted,
            locked: r.locked,
        })
        .collect()
}

/// What to call the track at `i`: what it was named, or "Track {i+1}".
pub(super) fn label(table: &[TrackRecord], i: usize) -> String {
    match table.get(i) {
        Some(t) if !t.name.is_empty() => t.name.clone(),
        _ => format!("Track {}", i + 1),
    }
}

/// The row a record draws as, carrying the solo flag it is given.
pub(super) fn row(record: &TrackRecord, soloed: bool) -> TimelineTrack {
    TimelineTrack {
        name: record.name.as_str().into(),
        muted: record.muted,
        soloed,
        locked: record.locked,
    }
}

/// A model's rows, as a vector.
pub(super) fn rows_of(tracks: &VecModel<TimelineTrack>) -> Vec<TimelineTrack> {
    tracks.iter().collect()
}

/// Tell the engine what to silence, from the rows as they are. Idempotent:
/// call it after anything that changes a mute, a solo, the order of the
/// tracks or how many there are. It rewrites the engine's whole vector, so a
/// missed call leaves the engine behind but never half-applied, and the next
/// call catches it up.
pub(super) fn push_mutes(project: &mut kuvatin_video::Project, tracks: &VecModel<TimelineTrack>) {
    project.set_track_mutes(&effective_mutes(&rows_of(tracks)));
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A row, for tests here and in the other timeline modules.
    pub(in crate::gui::video) fn trk(
        name: &str,
        muted: bool,
        soloed: bool,
        locked: bool,
    ) -> TimelineTrack {
        TimelineTrack {
            name: name.into(),
            muted,
            soloed,
            locked,
        }
    }

    #[test]
    fn with_no_solo_the_mutes_are_the_rows_own() {
        let rows = [
            trk("", false, false, false),
            trk("", true, false, false),
            trk("", false, false, true),
        ];
        assert_eq!(effective_mutes(&rows), vec![false, true, false]);
    }

    /// Solo silences every other track, the ones already muted included.
    #[test]
    fn one_solo_silences_every_other_track() {
        let rows = [
            trk("", false, false, false),
            trk("", false, true, false),
            trk("", true, false, false),
        ];
        assert_eq!(effective_mutes(&rows), vec![true, false, true]);
    }

    /// With every track soloed there is no other track to silence.
    #[test]
    fn every_track_soloed_is_the_same_as_none() {
        let rows = [trk("", false, true, false), trk("", true, true, false)];
        assert_eq!(effective_mutes(&rows), vec![false, true]);
    }

    #[test]
    fn a_muted_track_stays_silent_when_it_is_soloed() {
        let rows = [trk("", true, true, false), trk("", false, false, false)];
        assert_eq!(effective_mutes(&rows), vec![true, true]);
    }

    #[test]
    fn a_record_keeps_name_mute_and_lock_and_drops_solo() {
        let rows = [
            trk("Dialogue", true, true, false),
            trk("", false, false, true),
        ];
        let got = records(&rows);
        assert_eq!(
            got,
            vec![
                TrackRecord {
                    name: "Dialogue".into(),
                    muted: true,
                    locked: false,
                },
                TrackRecord {
                    name: String::new(),
                    muted: false,
                    locked: true,
                },
            ]
        );
        assert_eq!(row(&got[0], true), rows[0], "and a row comes back from it");
        assert!(!row(&got[0], false).soloed);
    }

    #[test]
    fn only_a_row_that_exists_can_be_locked() {
        let rows = [trk("", false, false, true), trk("", false, false, false)];
        assert!(locked(&rows, 0));
        assert!(!locked(&rows, 1));
        assert!(!locked(&rows, 2), "a new bottom track");
        assert!(!locked(&rows, -1), "no track");
    }

    /// A drop is refused if the clip's track or the one it lands on is
    /// locked, and the message names the one it found first.
    #[test]
    fn an_edit_is_refused_for_the_first_locked_track_it_touches() {
        let rows = [
            trk("", false, false, false),
            trk("Music", false, false, true),
            trk("", false, false, true),
        ];
        assert_eq!(first_locked(&rows, &[0, 0]), None, "neither");
        assert_eq!(first_locked(&rows, &[1, 0]), Some(1), "the source");
        assert_eq!(first_locked(&rows, &[0, 2]), Some(2), "the target");
        assert_eq!(first_locked(&rows, &[2, 1]), Some(2), "both: the source");
        assert_eq!(first_locked(&rows, &[0, 3]), None, "a new bottom track");
        assert_eq!(
            refusal(&rows, 1),
            (
                "Music is locked".to_string(),
                "Unlock the track to change what is on it.".to_string()
            )
        );
        assert_eq!(refusal(&rows, 2).0, "Track 3 is locked");
    }

    #[test]
    fn a_track_is_called_by_its_name_or_its_number() {
        let table = records(&[
            trk("Dialogue", false, false, false),
            trk("", false, false, false),
        ]);
        assert_eq!(label(&table, 0), "Dialogue");
        assert_eq!(label(&table, 1), "Track 2");
        assert_eq!(label(&table, 5), "Track 6", "past the end");
    }

    /// Two unnamed tracks and no engine, as the window starts.
    fn recorder() -> (super::super::ProjectSlot, Recorder) {
        let rec = Recorder {
            history: std::rc::Rc::new(std::cell::RefCell::new(crate::gui::history::History::new())),
            tl_clips: std::rc::Rc::new(VecModel::from(Vec::new())),
            tracks: std::rc::Rc::new(VecModel::from(vec![TimelineTrack::default(); 2])),
            ui: slint::Weak::default(),
        };
        (std::rc::Rc::new(std::cell::RefCell::new(None)), rec)
    }

    /// A mute is an edit, recorded with the track's label, and needs no
    /// engine: a track can be muted before any clip is on the timeline.
    #[test]
    fn muting_a_row_is_a_step_named_for_its_track() {
        use crate::gui::history::Step;
        let (project, rec) = recorder();
        edit_row(&project, &rec, 1, StepKind::MuteTrack, |r| r.muted = true);
        assert!(rec.tracks.row_data(1).unwrap().muted);
        let history = rec.history.borrow();
        let step = history.peek_undo().expect("a step");
        assert_eq!(step.describe(), "muting Track 2");
        assert_eq!(step.subject, Some(Subject::Track(1)));
    }

    #[test]
    fn a_change_that_changes_nothing_records_nothing() {
        let (project, rec) = recorder();
        edit_row(&project, &rec, 0, StepKind::LockTrack, |r| r.locked = false);
        edit_row(&project, &rec, 9, StepKind::LockTrack, |r| r.locked = true);
        assert!(!rec.history.borrow().can_undo());
    }
}
