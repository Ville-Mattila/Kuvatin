//! Per-track state in the Videos timeline: names, mutes, solo and locks.
//!
//! A track is its position (guiding decision 1 of the track controls
//! design). The interface's rows are the truth; the engine is told what to
//! silence and nothing else, and the undo history keeps the rows as
//! [`TrackRecord`]s. Solo is the one flag a record leaves out: it is a way of
//! listening, not an edit, so it is neither saved nor undone.

use crate::gui::TimelineTrack;
use kuvatin_video::TrackRecord;
use slint::{Model, VecModel};

/// What the engine should silence, given the rows as they are. While any
/// track is soloed every other one is silent; an explicit mute wins over
/// solo, so a track both soloed and muted stays silent.
pub(super) fn effective_mutes(rows: &[TimelineTrack]) -> Vec<bool> {
    let any_solo = rows.iter().any(|r| r.soloed);
    rows.iter()
        .map(|r| r.muted || (any_solo && !r.soloed))
        .collect()
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
}
