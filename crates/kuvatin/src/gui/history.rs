//! Undo and redo, as a pair of stacks that knows nothing about what a step
//! does. Each mode supplies its own step type (the timeline's clip records,
//! Images mode's files and crops) and applies a step itself; this decides what
//! a new step merges into, when the oldest is dropped, and what the buttons say.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Steps kept per history. Enough to walk back through an evening's editing,
/// and bounded, because a step can hold thumbnails.
pub(super) const MAX_STEPS: usize = 200;

/// How close together two changes of the same kind to the same thing must be
/// to count as one gesture: a slider drag, a mouse drag, a held arrow key.
pub(super) const MERGE_WINDOW: Duration = Duration::from_secs(1);

/// What a history needs from a step.
pub(super) trait Step {
    /// A noun phrase for the buttons: "trim of intro.mp4", "adding 3 files".
    fn describe(&self) -> String;
    /// Whether `newer`, recorded straight after this step, continues the same
    /// gesture. The history adds the timing condition itself.
    fn merges_with(&self, newer: &Self) -> bool;
    /// Fold `newer` into this step: keep this step's "before", take `newer`'s
    /// "after".
    fn absorb(&mut self, newer: Self);
    /// A step that changed nothing. The history ignores it.
    fn is_empty(&self) -> bool;
}

/// One mode's undo and redo stacks.
pub(super) struct History<S> {
    /// Oldest first, each step with the time it last changed.
    undo: VecDeque<(S, Instant)>,
    redo: Vec<S>,
    /// Set by an undo, a redo, [`Self::seal`], or a merge that emptied the top
    /// step; cleared by the next non-empty record. A change made right after
    /// an undo is a new step, never part of the one before it.
    sealed: bool,
}

impl<S: Step> Default for History<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Step> History<S> {
    pub(super) fn new() -> Self {
        History {
            undo: VecDeque::new(),
            redo: Vec::new(),
            sealed: false,
        }
    }

    /// Add a step the caller has just applied. It merges into the step on top
    /// when that step continues the same gesture within [`MERGE_WINDOW`];
    /// otherwise it is pushed, dropping the oldest past [`MAX_STEPS`]. Any
    /// non-empty record clears redo. A step that merges back to nothing is
    /// removed, and the next change starts a step of its own.
    pub(super) fn record(&mut self, step: S, now: Instant) {
        if step.is_empty() {
            return;
        }
        self.redo.clear();
        let sealed = std::mem::replace(&mut self.sealed, false);
        if let Some((top, last)) = self.undo.back_mut() {
            if !sealed
                && now.saturating_duration_since(*last) < MERGE_WINDOW
                && top.merges_with(&step)
            {
                top.absorb(step);
                *last = now;
                // A drag that ends where it began changed nothing after all.
                // It still separates what came before from what comes next,
                // so the next change must not merge across it.
                let emptied = top.is_empty();
                if emptied {
                    self.undo.pop_back();
                    self.sealed = true;
                }
                return;
            }
        }
        self.push(step, now);
    }

    fn push(&mut self, step: S, now: Instant) {
        self.undo.push_back((step, now));
        if self.undo.len() > MAX_STEPS {
            self.undo.pop_front();
        }
    }

    /// The step an undo would apply. Apply it, then call [`Self::commit_undo`];
    /// if applying failed, do not, and the step stays where it is.
    pub(super) fn peek_undo(&self) -> Option<&S> {
        self.undo.back().map(|(step, _)| step)
    }

    /// The step a redo would apply; the same two phases as undo.
    pub(super) fn peek_redo(&self) -> Option<&S> {
        self.redo.last()
    }

    /// The step from [`Self::peek_undo`] was applied: move it to redo.
    pub(super) fn commit_undo(&mut self) {
        if let Some((step, _)) = self.undo.pop_back() {
            self.redo.push(step);
        }
        self.sealed = true;
    }

    /// The step from [`Self::peek_redo`] was applied: move it back to undo.
    pub(super) fn commit_redo(&mut self, now: Instant) {
        if let Some(step) = self.redo.pop() {
            self.push(step, now);
        }
        self.sealed = true;
    }

    pub(super) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub(super) fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// "Undo trim of intro.mp4", or "Nothing to undo".
    pub(super) fn undo_hint(&self) -> String {
        match self.peek_undo() {
            Some(step) => format!("Undo {}", step.describe()),
            None => "Nothing to undo".into(),
        }
    }

    /// "Redo trim of intro.mp4", or "Nothing to redo".
    pub(super) fn redo_hint(&self) -> String {
        match self.peek_redo() {
            Some(step) => format!("Redo {}", step.describe()),
            None => "Nothing to redo".into(),
        }
    }

    pub(super) fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.sealed = false;
    }

    /// Make the next change a step of its own, whatever it is. For a caller
    /// whose undo or redo failed partway: the step on top no longer matches
    /// what is on screen, so nothing may merge into it.
    pub(super) fn seal(&mut self) {
        self.sealed = true;
    }

    /// Every step on both stacks, for rewriting a handle that changed (a clip
    /// restored under a new ID).
    pub(super) fn steps_mut(&mut self) -> impl Iterator<Item = &mut S> {
        self.undo
            .iter_mut()
            .map(|(step, _)| step)
            .chain(self.redo.iter_mut())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A step that changes the value named `what` from `before` to `after`.
    #[derive(Debug, Clone, PartialEq)]
    struct Set {
        what: &'static str,
        before: i32,
        after: i32,
        merges: bool,
    }

    fn set(what: &'static str, before: i32, after: i32) -> Set {
        Set {
            what,
            before,
            after,
            merges: true,
        }
    }

    impl Step for Set {
        fn describe(&self) -> String {
            format!("change of {}", self.what)
        }
        fn merges_with(&self, newer: &Self) -> bool {
            self.merges && newer.merges && self.what == newer.what
        }
        fn absorb(&mut self, newer: Self) {
            self.after = newer.after;
        }
        fn is_empty(&self) -> bool {
            self.before == self.after
        }
    }

    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    #[test]
    fn undo_and_redo_walk_the_steps_in_order() {
        let t0 = Instant::now();
        let mut h = History::new();
        h.record(set("a", 0, 1), at(t0, 0));
        h.record(set("b", 0, 1), at(t0, 5000));
        assert_eq!(h.peek_undo().map(|s| s.what), Some("b"));
        h.commit_undo();
        assert_eq!(h.peek_undo().map(|s| s.what), Some("a"));
        assert_eq!(h.peek_redo().map(|s| s.what), Some("b"));
        h.commit_redo(at(t0, 6000));
        assert_eq!(h.peek_undo().map(|s| s.what), Some("b"));
        assert!(!h.can_redo());
    }

    #[test]
    fn a_new_step_clears_redo() {
        let t0 = Instant::now();
        let mut h = History::new();
        h.record(set("a", 0, 1), at(t0, 0));
        h.commit_undo();
        assert!(h.can_redo());
        h.record(set("b", 0, 1), at(t0, 100));
        assert!(!h.can_redo());
    }

    #[test]
    fn changes_to_the_same_thing_within_a_second_are_one_step() {
        let t0 = Instant::now();
        let mut h = History::new();
        h.record(set("a", 0, 1), at(t0, 0));
        h.record(set("a", 1, 2), at(t0, 500));
        assert_eq!(h.peek_undo(), Some(&set("a", 0, 2)));
        h.commit_undo();
        assert!(!h.can_undo(), "one step, not two");
    }

    #[test]
    fn a_change_at_exactly_one_second_is_a_new_step() {
        let t0 = Instant::now();
        let mut h = History::new();
        h.record(set("a", 0, 1), at(t0, 0));
        h.record(set("a", 1, 2), at(t0, 1000));
        h.commit_undo();
        assert!(h.can_undo(), "two steps");
    }

    /// A slow, steady drag is one gesture: the window runs from the last change.
    #[test]
    fn the_window_runs_from_the_last_change_not_the_first() {
        let t0 = Instant::now();
        let mut h = History::new();
        h.record(set("a", 0, 1), at(t0, 0));
        h.record(set("a", 1, 2), at(t0, 900));
        h.record(set("a", 2, 3), at(t0, 1800));
        assert_eq!(h.peek_undo(), Some(&set("a", 0, 3)));
    }

    /// A drag that ends where it began changed nothing, so it leaves no step.
    #[test]
    fn a_gesture_that_ends_where_it_began_leaves_no_step() {
        let t0 = Instant::now();
        let mut h = History::new();
        h.record(set("a", 0, 1), at(t0, 0));
        h.record(set("a", 1, 0), at(t0, 400));
        assert!(!h.can_undo());
    }

    #[test]
    fn changes_to_different_things_never_merge() {
        let t0 = Instant::now();
        let mut h = History::new();
        h.record(set("a", 0, 1), at(t0, 0));
        h.record(set("b", 0, 1), at(t0, 100));
        h.commit_undo();
        assert!(h.can_undo(), "two steps");
    }

    /// Undo during a gesture, then carry on: the next change must not fold
    /// into the step beneath the undone one, even inside the merge window.
    #[test]
    fn a_change_right_after_an_undo_is_its_own_step() {
        let t0 = Instant::now();
        let mut h = History::new();
        h.record(set("a", 0, 1), at(t0, 0));
        h.record(set("b", 0, 1), at(t0, 100));
        h.commit_undo();
        h.record(set("a", 1, 2), at(t0, 200));
        assert_eq!(h.peek_undo(), Some(&set("a", 1, 2)));
        h.commit_undo();
        assert_eq!(h.peek_undo(), Some(&set("a", 0, 1)));
    }

    #[test]
    fn the_oldest_step_goes_when_the_cap_is_reached() {
        let t0 = Instant::now();
        let mut h = History::new();
        for i in 0..=(MAX_STEPS as i32) {
            let mut s = set("a", i, i + 1);
            s.merges = false;
            h.record(s, at(t0, i as u64));
        }
        let mut undone = Vec::new();
        while let Some(s) = h.peek_undo() {
            undone.push(s.before);
            h.commit_undo();
        }
        assert_eq!(undone.len(), MAX_STEPS);
        assert_eq!(*undone.last().unwrap(), 1, "step 0 was dropped");
    }

    #[test]
    fn a_step_that_changed_nothing_is_ignored() {
        let t0 = Instant::now();
        let mut h = History::new();
        h.record(set("a", 3, 3), at(t0, 0));
        assert!(!h.can_undo());
        // ...and it leaves redo alone: re-applying a crop that is already
        // there must not throw away what could be redone.
        h.record(set("a", 0, 1), at(t0, 100));
        h.commit_undo();
        h.record(set("a", 0, 0), at(t0, 200));
        assert!(h.can_redo());
    }

    /// Undo is two-phase: until the caller says the step was applied, it stays.
    #[test]
    fn an_undo_that_was_never_committed_leaves_the_step_in_place() {
        let mut h = History::new();
        h.record(set("a", 0, 1), Instant::now());
        let _ = h.peek_undo();
        assert!(h.can_undo());
        assert!(!h.can_redo());
    }

    #[test]
    fn the_hints_name_the_step() {
        let mut h: History<Set> = History::new();
        assert_eq!(h.undo_hint(), "Nothing to undo");
        assert_eq!(h.redo_hint(), "Nothing to redo");
        h.record(set("volume", 0, 1), Instant::now());
        assert_eq!(h.undo_hint(), "Undo change of volume");
        h.commit_undo();
        assert_eq!(h.redo_hint(), "Redo change of volume");
    }

    #[test]
    fn clear_empties_both_stacks() {
        let t0 = Instant::now();
        let mut h = History::new();
        h.record(set("a", 0, 1), at(t0, 0));
        h.record(set("b", 0, 1), at(t0, 2000));
        h.commit_undo();
        h.clear();
        assert!(!h.can_undo() && !h.can_redo());
    }

    #[test]
    fn every_step_on_both_stacks_can_be_rewritten() {
        let t0 = Instant::now();
        let mut h = History::new();
        h.record(set("a", 0, 1), at(t0, 0));
        h.record(set("b", 0, 1), at(t0, 5000));
        h.commit_undo();
        for s in h.steps_mut() {
            s.what = "c";
        }
        assert_eq!(h.peek_undo().map(|s| s.what), Some("c"));
        assert_eq!(h.peek_redo().map(|s| s.what), Some("c"));
    }

    /// A gesture that cancels itself out between an undo and the next change
    /// must not let that change merge into the step beneath: the seal holds.
    #[test]
    fn a_cancelled_gesture_keeps_the_seal() {
        let t0 = Instant::now();
        let mut h = History::new();
        h.record(set("a", 0, 1), at(t0, 0));
        h.record(set("b", 0, 1), at(t0, 100));
        h.commit_undo();
        h.record(set("c", 0, 1), at(t0, 200));
        h.record(set("c", 1, 0), at(t0, 300));
        h.record(set("a", 1, 2), at(t0, 400));
        assert_eq!(h.peek_undo(), Some(&set("a", 1, 2)));
        h.commit_undo();
        assert_eq!(h.peek_undo(), Some(&set("a", 0, 1)));
    }

    #[test]
    fn a_sealed_history_starts_a_new_step() {
        let t0 = Instant::now();
        let mut h = History::new();
        h.record(set("a", 0, 1), at(t0, 0));
        h.seal();
        h.record(set("a", 1, 2), at(t0, 100));
        h.commit_undo();
        assert!(h.can_undo(), "two steps");
    }
}
