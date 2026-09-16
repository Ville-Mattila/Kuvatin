# Kuvatin — Editor Track Controls Design

> **For agentic workers:** This is the validated design (spec) for per-track
> mute, solo, lock and rename in Kuvatin's Videos timeline. It is the second of
> the three specs the undo design names (backlog item `ui-editor`); Spec 1 is
> the quick wins and Spec 3 the media features, and neither is covered here.
> **It cannot be built until the undo design is amended** — see "The amendment
> the undo design needs", which gives the exact wording. The next step after
> that amendment is the `writing-plans` skill.

## Goal

A track is a thing you can name, silence, listen to on its own, and protect. A
track called "Dialogue" stays called "Dialogue" after an undo, after a reorder
and after the project is closed and reopened. A muted track is silent in the
preview and in the export. A soloed track is the only one you hear while you
are listening. A locked track refuses every edit that would change what is on
it, and says so rather than failing quietly.

Underneath all four sits one change: **a track becomes a thing with state,
instead of an index with a generated label.**

## Background — current state

**There is no track. There is a layer index.**

- `Project.layers` is a `Vec<ges::Layer>` (`crates/kuvatin-video/src/project.rs:735`),
  index 0 = top. `Project::layer(index)` (`:898-903`) appends layers on demand,
  so an index that has no clip on it may have no layer either. A clip's track is
  read back as its layer's priority (`clip_track`, `:1311-1316`). There is no
  track struct, no track id, and no per-track state of any kind.
- `move_track(from, to)` (`:1520-1534`) asks GES to move the layer and then
  re-reads `self.layers` from the timeline. `prune_tracks(keep)` (`:1271-1289`)
  drops trailing empty layers, never below one. `track_count()` (`:1292-1294`)
  is `layers.len()`.
- `Timeline::new_audio_video()` (`:781`) gives the timeline exactly one audio
  track and one video track. They are the only GES tracks the engine ever has;
  layers are added later, tracks never are.

**Track names exist only in the interface, and only as generated strings.**

- `VideoState.tracks` is an `Rc<VecModel<SharedString>>` seeded with "Track 1"
  and "Track 2" (`crates/kuvatin/src/gui/video/mod.rs:30-32`, `:53-57`).
- It is regenerated from the index in two places: `set_track_rows`
  (`gui/video/undo.rs:338-346`) after every undo and redo, and `restore_models`
  (`gui/video/project_file.rs:231-235`) on every open. A rename today would be
  silently overwritten by either.
- The model is also the authority on **how many** tracks there are. `on_add_track`
  (`gui/video/timeline.rs:185-203`) pushes a row and creates no layer — by
  design, so that `remove_clip`'s trailing-layer pruning cannot take an
  added-but-empty track away. The engine's `track_count()` can therefore be
  smaller than the number of rows, and `drop_target_track`
  (`gui/video/timeline.rs:424-435`) takes the max of the two.

**The saved format has no track table.**

- `ProjectFile` is `{ version, canvas_w, canvas_h, clips }`
  (`crates/kuvatin-video/src/document.rs:88-96`), `FORMAT_VERSION` 1 (`:20`).
- On load the track count is inferred as the highest clip's track plus one,
  floored at two (`gui/video/project_file.rs:225-230`).
- The save path already patches the document with state only the interface
  holds: it fills each sequence clip's `sequence` spec after `to_document()`
  returns (`gui/video/project_file.rs:40-54`). That precedent matters below.

**The undo capture holds a bare count.**

- `Capture { records: BTreeMap<String, ClipRecord>, tracks: usize }`
  (`gui/video/undo.rs:47-50`), read by `Capture::of` (`:54-64`).
- `TimelineStep` keeps `tracks_before` / `tracks_after` as `usize` (`:104-105`),
  compared in `is_empty` (`:192-194`), carried by `absorb` (`:189`), read by
  `target_tracks` (`:240-245`), and consumed in `apply_step` at `:479` (the
  no-engine path), `:579` (`prune_tracks`), `:607` (`tracks.max(p.track_count())`)
  and `:611` (`set_track_rows`).
- A step's `subject` is `Option<String>`, always a `ClipId` (`:93-106`,
  `:142-156`, `:172-177`).

**The gutter header has no room.**

- `crates/kuvatin/ui/app.slint:1772-1804`: a 94 px column (`:1773`) of rows
  `Theme.track-h` tall — 30 px (`ui/theme.slint:32`) — each holding a `≡` grip
  and an elided label, all inside one `TouchArea` (`:1786-1802`) whose only job
  is drag-to-reorder and which takes keyboard focus on pointer-down (`:1791`).
- `timeline-track-labels: [string]` (`:108`) is read at `:1691` (band height),
  `:1775` (the headers), `:1794` (the reorder clamp), `:1856` (the lane's row
  stripes), `:2058-2059` (the new-track drop zone) and `:2101-2102` (band
  height again).
- The "+ New track" row (`:2057-2090`) calls `add-track()` (`:188`), capped at
  eight rows (`gui/video/timeline.rs:194`).
- `Tooltip` / `TooltipLayer` (`ui/widgets.slint:730`, `ui/app.slint:2423`) and
  the `Gesture.held` global (`ui/widgets.slint:765-767`) already exist, from the
  undo work.

**Not every edit enters through `timeline.rs`.** Two do not, and both need a
lock guard: the inspector transform, applied by the preview tick
(`gui/video/mod.rs:306-315`), and adding a clip from the media bin
(`add_to_timeline`, `gui/video/mod.rs:460`, which lands images on track 0
and videos on track 1 at `:482-484`, and `add_sequence_to_timeline`, `:532`).

## Guiding decisions (locked during brainstorming)

1. **A track is its position, not an id.** Per-track state is a vector kept in
   lockstep with `Project.layers` and with the interface's track rows. Giving a
   track a stable id would only pay off if a clip named its track by that id,
   and `ClipRecord.track` is a `usize` index (`document.rs:67`) that every edit
   path, the saved format and `drop_target_track` already treat as positional.
   Changing that is a format break and a rewrite of every edit path, for no gain
   this spec needs. Rejected, and written down here so the next spec does not
   re-open it cheaply.
2. **The interface owns the track table; the engine is told mute and nothing
   else.** The track model is already the only thing that knows how many tracks
   there are, because a track can exist without a layer. Moving that knowledge
   into `Project` would mean giving it a track count that is not `layers.len()`,
   which is exactly what the lazy-layer rule (`timeline.rs:185-189`) exists to
   avoid. Name and lock mean nothing to GES; only mute does.
3. **The undo capture's `tracks` becomes a table, not a count.** See "Why the
   capture, and not a step kind of its own".
4. **Solo is a monitoring state: not saved, not undone, and dropped before a
   render.** See "Solo".
5. **Mute silences audio only.** `set_active_for_tracks` is passed the
   timeline's audio track alone. Passing the video track too would make the
   clips vanish from the picture, which is "hide", not "mute", and hiding is not
   in the backlog entry.
6. **The row height does not change.** `Theme.track-h` is the timeline's
   vertical unit; the gutter widens instead. See "Interface".

## Architecture

### The track record and the track row

Two types, one for what is saved and undone, one for what is drawn.

In `crates/kuvatin-video/src/document.rs`, beside `ClipRecord` (`:61-77`):

```rust
/// One track, as stored. The index in `ProjectFile::tracks` is the track: 0 is
/// the top track, the same numbering `ClipRecord::track` uses.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackRecord {
    /// What the user called it. Empty means "call it Track N", so a project
    /// nobody renamed carries no names at all.
    #[serde(default)]
    pub name: String,
    /// Silent: the layer is inactive for the timeline's audio track.
    #[serde(default)]
    pub muted: bool,
    /// Refuses edits to the clips on it.
    #[serde(default)]
    pub locked: bool,
}
```

Solo is deliberately absent. In `crates/kuvatin/ui/app.slint`, beside
`TimelineClip` (`:25-35`):

```slint
// One timeline track row: the gutter header and the lane stripe behind it.
export struct TimelineTrack {
    name: string,      // "" → the header shows "Track {t+1}"
    muted: bool,
    soloed: bool,      // transient: never saved, never undone
    locked: bool,
}
```

`TimelineTrack` is `TrackRecord` plus the one transient flag. The conversions
both ways, the effective-mute derivation and the lock predicates live in a new
pure module, `crates/kuvatin/src/gui/video/tracks.rs`, which also wires the four
new callbacks. `mod.rs` gains `mod tracks;` beside its siblings (`:5-9`) and
calls `tracks::wire(ui, st)`.

### Interface state changes

`VideoState.tracks` (`gui/video/mod.rs:30-32`) becomes
`Rc<VecModel<TimelineTrack>>`, seeded with two default rows (`:53-57`), and
`ui.set_timeline_tracks(...)` replaces `set_timeline_track_labels`. The same
type change lands on `Recorder.tracks` (`gui/video/undo.rs:355`) and
`VideoHandles.tracks` (`gui/video/project_file.rs:353`). Call sites that only
use `row_count()` — `timeline.rs:138`, `:146-150`, `:194`, `:199-200` — keep
working; the two that push a `SharedString` (`timeline.rs:148`, `:200`) push a
default `TimelineTrack` instead, which is the correct behaviour anyway: a track
created by a drop or by "+ New track" starts unnamed, audible, unsoloed and
unlocked.

### Engine additions (`kuvatin-video`, `Project`)

One field and two methods. Nothing else in the engine learns about tracks.

```rust
/// Per track, top first, whether its audio is silenced. Kept the same length
/// as, and in the same order as, `layers`; an index past the end is audible.
/// Remembered rather than only applied, so a layer created later for a muted
/// index arrives silent.
mutes: Vec<bool>,

/// Silence the audio of the tracks whose flag is set, and remember the whole
/// vector. Positional, top track first. Inert while rendering, like every
/// other mutator.
pub fn set_track_mutes(&mut self, mutes: &[bool]);

/// Whether the layer at `track` is silent right now, read back from GES. False
/// for a track that has no layer yet.
pub fn track_muted(&self, track: usize) -> bool;
```

- Applying one flag is
  `layer.set_active_for_tracks(!muted, &audio_tracks)`
  (`gstreamer-editing-services` 0.23.5, `src/auto/layer.rs:227`; behind feature
  `v1_18`, which `v1_20` pulls in, and the crate enables `v1_20` and `v1_24`).
  `audio_tracks` is `self.timeline.tracks()` filtered to
  `TrackType::AUDIO` — exactly one, from `new_audio_video()` (`:781`). The video
  track is never passed, per guiding decision 5.
- `track_muted` is `!layer.is_active_for_track(&audio)`
  (`src/auto/layer.rs:139`).
- `set_track_mutes` ends with `self.commit()` and sets `dirty` the way
  `prune_tracks` does (`project.rs:1286-1287`): a mute is saved state, so the
  project has changed.
- **`layer(index)` (`:898-903`) applies `mutes.get(i)` to every layer it
  appends.** Without this, dropping a clip onto a muted-but-empty track would
  silently unmute it.
- **`move_track` (`:1520-1534`) permutes `mutes` with the layers**:
  `let m = mutes.remove(from); mutes.insert(to, m);`, alongside the existing
  `self.layers = self.timeline.layers();`. The claim that this matches what GES
  does to the layer order is pinned by a test.
- **`prune_tracks` (`:1271-1289`) truncates `mutes` to `layers.len()`** after
  its loop.

Keeping the engine internally consistent this way matters because `layer()`
runs inside `move_clip_to_track` (`:1298-1308`), before the interface has a
chance to push a fresh vector.

### The effective mute, and where it is pushed

The interface's rows are the truth; the engine's vector is a projection of them.
In `tracks.rs`:

```rust
/// What the engine should silence, given the rows as they are. Explicit mute
/// wins over solo: a track that is both soloed and muted stays silent.
pub(super) fn effective_mutes(rows: &[TimelineTrack], solo_allowed: bool) -> Vec<bool> {
    let any_solo = solo_allowed && rows.iter().any(|r| r.soloed);
    rows.iter()
        .map(|r| r.muted || (any_solo && !r.soloed))
        .collect()
}

/// Read the rows and push the result to the engine. Idempotent; call it after
/// anything that changes a mute, a solo, the track order or the track count.
pub(super) fn push_mutes(
    project: &mut kuvatin_video::Project,
    tracks: &VecModel<TimelineTrack>,
);
```

`push_mutes` is called from: the mute and solo callbacks; `on_track_reordered`
(`timeline.rs:205-233`); `on_timeline_clip_dropped` when the drop grew the
track count (`timeline.rs:144-149`); `restore_models` after `apply_document`
(`project_file.rs:192`); and `apply_step` after the track rows are set
(`undo.rs:611`). Because it rewrites the whole vector, a missed call can leave
the engine stale but can never leave it half-applied, and the next call fixes
it.

### Why the capture, and not a step kind of its own

Clause 4 of the undo design's "Contract for later edits"
(`2026-09-13-undo-design.md:287-294`) makes this a gate: an edit whose state
cannot be expressed as a `ClipRecord` or the track rows needs a new step kind,
added to that design before it is built. Three facts decide which way it goes.

1. **The track table has to be in every step's before/after anyway.** Undoing a
   reorder must put the names back in the right order. Undoing "+ New track"
   must take away the row *and* whatever was done to it. Undoing a drop onto a
   new bottom track must take that track's row away too. None of that works if
   the table lives outside the step.
2. **A step kind carrying "track 2's mute went false → true" would be a
   hand-written inverse**, which guiding decision 4 of the undo design
   (`:78-82`) rejected on purpose: the whole model is before/after state, not
   reversed operations.
3. **The state is genuinely part of "the track rows"** the contract already
   names. The rows are simply a count today; widening them to a table is the
   change the contract anticipated, not an escape from it.

So: **`Capture.tracks` becomes a vector of track records**, and the four
controls get new step *kinds* only for their labels and their merge behaviour —
no new step *shape*. Solo gets none of either, because it is not in the table.

### What moves in the undo module

**`Capture`** (`undo.rs:47-50`), and `Capture::of` (`:54-64`), whose second
parameter becomes the table:

```rust
pub(super) struct Capture {
    pub(super) records: BTreeMap<String, ClipRecord>,
    pub(super) tracks: Vec<TrackRecord>,
}

pub(super) fn of(
    project: Option<&kuvatin_video::Project>,
    tracks: Vec<TrackRecord>,
) -> Self;
```

`Recorder::before` (`:361-363`) reads the table out of the model instead of
calling `row_count()`.

**`TimelineStep`** (`:93-106`): `tracks_before` and `tracks_after` become
`Vec<TrackRecord>` (`:104-105`), filled the same way in `new` (`:135-136`).

**`StepKind`** (`:24-33`) gains `MuteTrack`, `LockTrack`, `RenameTrack`. Solo
adds nothing. `StepKind::merges()` (`:35-42`) gains `RenameTrack` and only
that: a rename is typed, so its keystrokes merge into one step; a mute or a lock
is a single toggle, and muting then unmuting inside a second should read as two
steps, not as nothing.

**`subject`** (`:96`) becomes `Option<Subject>`:

```rust
/// What a step is about, when it is about one thing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Subject {
    Clip(String),
    /// A track by its index. Safe as a merge key: the only kind that merges is
    /// RenameTrack, and any edit that could renumber a track is a different
    /// kind, so `merges_with` already refuses across it.
    Track(usize),
}
```

Two `Option` fields that must never both be `Some` would be the smaller diff and
the worse invariant. The consequences are mechanical:

- `TimelineStep::new` (`:111-137`) and `Recorder::record` / `record_captures`
  (`:368-401`) take `Option<Subject>`. `record_captures` looks a clip subject's
  display name up in the rows as it does today (`:387-393`), and a track
  subject's in the table (see `label` below).
- `rename_clip` (`:142-156`) rewrites only `Subject::Clip`.
- `merges_with` (`:172-177`) is unchanged in text and works on the enum.
- `apply_step` (`:458-473`, `:614-621`) narrows the subject to a clip id before
  `selection_after` (`:302-314`): a track step selects no clip.
- The seven call sites that pass `Some(row.id.as_str())` pass
  `Some(Subject::Clip(row.id.to_string()))`: `timeline.rs:151` (Move), `:252`
  (Trim), `:292` (Duration), `:360` (Delete), `mod.rs:309-314` (Transform),
  `mod.rs:507-512` (Add) and the same call in `add_sequence_to_timeline`
  (`mod.rs:567-572`). The two that pass `None` (`timeline.rs:201` for Add track,
  `:231` for Reorder tracks) are unchanged.

**`describe`** (`:158-170`) gains, using the track's label on the step's *before*
side, which a merge keeps stable:

| Kind | Phrase |
| --- | --- |
| `MuteTrack` | "muting Dialogue" / "unmuting Dialogue" |
| `LockTrack` | "locking Dialogue" / "unlocking Dialogue" |
| `RenameTrack` | "renaming Track 2" |

Which way a toggle went is read off the step's own two tables, so no extra field
is needed. The label is

```rust
/// What to call the track at `i`: what it was named, or "Track {i+1}".
pub(super) fn label(table: &[TrackRecord], i: usize) -> String;
```

**`absorb`** (`:179-190`): line `:189` (`self.tracks_after = newer.tracks_after;`)
is unchanged in text and now moves a vector. Nothing else in `absorb` changes —
a rename records no clip changes, so the loop at `:180-185` runs zero times and
the `retain` at `:187` is a no-op.

**`is_empty`** (`:192-194`): unchanged in text, changed in meaning. It becomes a
vector comparison, which is exactly what makes a rename-only or mute-only edit
recordable at all; today such an edit compares `usize == usize`, is judged
empty, and is silently dropped by `History::record` (`gui/history.rs:62`). This
one line is the reason the capture route is not optional.

**`target_tracks`** (`:240-245`): returns `Vec<TrackRecord>` instead of `usize`,
cloning the chosen side. The name stays.

**`set_track_rows`** (`:338-346`): its whole job changes, from "make there be
`count` rows, named from the index" to "make the rows be exactly this table".

```rust
/// Make the timeline's track rows match `want` exactly: its length, and every
/// row's name, mute and lock. Solo is the row's own, and is carried across
/// rather than overwritten — undo does not touch what you are listening to.
/// Rows that already match are not rewritten, so Slint does not repaint them.
pub(super) fn set_track_rows(tracks: &VecModel<TimelineTrack>, want: &[TrackRecord]);
```

This is the line where "a rename is silently overwritten by any undo" dies.

**`apply_step`**: `:479` (the no-engine path) and `:611` pass the table.
`:579` (`p.prune_tracks(tracks)`) passes `tracks.len()`. `:607`
(`let track_rows = tracks.max(p.track_count());`) becomes: take the step's
table, and if the engine still has more layers than the table has rows —
because a prune was refused — extend it with default records up to
`p.track_count()`. A `push_mutes` call follows `:611`.

### Saving and opening

`ProjectFile` (`document.rs:88-96`) gains one field:

```rust
/// One per track, top first. Empty means "no table was saved": the track count
/// is inferred from the clips, as it was before this field existed.
#[serde(default)]
pub tracks: Vec<TrackRecord>,
```

`ProjectFile::new` (`:98-106`) fills it with `Vec::new()`, so `to_document()`
(`project.rs:1412-1420`) is unchanged: the engine does not hold names or locks
and must not pretend to. The save path patches it in, exactly as it already
patches each sequence clip's `sequence` (`project_file.rs:40-54`):

```rust
doc.tracks = tracks::records(&st.tracks);   // name, muted, locked; solo dropped
```

On load, `restore_models` (`project_file.rs:192`) replaces the inference at
`:225-235` with:

```rust
let needed = doc.tracks.len()
    .max(records.iter().map(|(_, r)| r.track + 1).max().unwrap_or(0))
    .max(2);
```

then builds the rows from `doc.tracks`, padding with defaults to `needed`,
clearing every `soloed`, and calling `push_mutes`. Taking the max matters: a
file whose table is shorter than its deepest clip would otherwise lose a track.

**`FORMAT_VERSION` stays 1.** The field is additive and `#[serde(default)]`, so
an old file opens unchanged. The cost is the other direction: `ProjectFile` does
not set `deny_unknown_fields`, so a build older than this change opens a newer
project, ignores the table, and shows every track unnamed, audible and
unlocked. Bumping to 2 would instead make every project saved after this lands
refuse to open in 2.12.0 and earlier (`document.rs:130-137`), which is worse for
a field this small. Written down in Risks rather than hidden.

### Solo

Solo is derived: it is "mute every other track", which `effective_mutes` above
computes. It needs no engine API of its own. Two decisions go with it.

**Solo is not persisted, and not undoable.** It is not in `TrackRecord`, so it
is not in `ProjectFile`, not in `Capture.tracks`, and invisible to the history.
A saved solo would reopen a project with tracks inaudible for a reason the user
had forgotten, and would give the file two overlapping truths about what is
silent. An undoable solo would let Ctrl+Z change what you are listening to,
which is not an edit to the work. It lives only in `TimelineTrack.soloed`, is
cleared by `restore_models` on open, and lasts the session otherwise.

**Solo does not reach an export.** It is cleared when a render starts, before
`prepare_render` / `begin_render` in `gui/video/export.rs`: the solo flags are
set false, `push_mutes` runs (with `set_track_mutes` still able to write,
because `Project::rendering` is not yet set), and the buttons visibly pop off.
The alternative — save the solo set, push a solo-free vector, restore
afterwards — needs three hooks (start, finish, cancel), and a forgotten solo
that silently ruins a long export is the failure worth spending a visible
button-clearing on. Explicit mutes are untouched and do reach the export.

### Lock

Lock involves no engine call. It is a guard, and it must sit on **every** path
that changes a clip, which is not the same as every callback in `timeline.rs`.
The predicates are pure and live in `tracks.rs`:

```rust
/// Whether the track at `t` refuses edits. Out-of-range is not locked.
pub(super) fn locked(rows: &[TimelineTrack], t: i32) -> bool;

/// Whether a drop must be refused outright: either the clip's track or the
/// track it would land on is locked.
pub(super) fn drop_refused(rows: &[TimelineTrack], from: i32, to: i32) -> bool;
```

What refuses, and on what test:

| Entry point | Refuses when |
| --- | --- |
| `on_timeline_clip_dropped` (`timeline.rs:119-158`) | the clip's current track **or** the computed target is locked. `drop_target_track` (`:424-435`) is pure and is called first, so the whole drop — slide and track change together — is refused as one thing and the engine is never touched. A drop onto the new bottom track only checks the source, because a new track is never locked. |
| `on_timeline_clip_trimmed` (`timeline.rs:241-272`) | the clip's track is locked |
| `on_inspector_duration_changed` (`timeline.rs:274-307`) | the selected clip's track is locked |
| `remove_timeline_clip` (`timeline.rs:343-381`) | the clip's track is locked. One guard covers both the × button (`:316`) and the Delete key (`:327`). |
| `on_track_reordered` (`timeline.rs:205-233`) | the **dragged** track is locked |
| the transform timer (`mod.rs:306-315`) | the pending clip's track is locked; the pending value is dropped, not applied |
| `add_to_timeline` (`mod.rs:460`) and `add_sequence_to_timeline` (`:532`) | the track it would land on — 0 for images, 1 for videos (`:482-484`) — is locked |

Three things deliberately do **not** refuse:

- **`on_add_track` (`timeline.rs:185-203`).** Adding a track changes no locked
  track.
- **A reorder someone else starts.** A locked track can still be shifted by
  another track's reorder, because its contents do not change, only its
  position. Refusing that would make one locked track freeze the whole gutter.
- **Undo and redo (`undo.rs:414-442`).** A lock guards against new mistakes; it
  is not a reason to strand a step that is already in the history and can never
  be applied. The two-phase apply would fail forever otherwise. Undoing an edit
  made before the lock went on can therefore change a locked track's contents.

A refusal is visible, not silent: the guards that a click can reach — drop,
trim, duration, delete, add — show the existing `show_error` with
"Track N is locked" and "Unlock the track to change what is on it." A locked
track's clips also lose their affordances in the interface (below), so a
refusal message should be the rare case, not the normal one.

`on_timeline_snap_dx` (`timeline.rs:167-183`) is pure and read-only and needs no
guard; the drag it feeds is stopped at the source instead.

### The amendment the undo design needs

`docs/superpowers/specs/2026-09-13-undo-design.md` must be amended **before any
of this is built**, per clause 4 of its own contract. Four edits, with the
wording proposed:

**1. In "Timeline steps (app crate)", replace the bullet at `:174`**
("the timeline's track-row count before and after.") with:

> - the timeline's **track table** before and after: one record per track row,
>   in order, holding its name, whether it is muted and whether it is locked. A
>   step that changes nothing but the table — a rename, a mute, a lock — is
>   still a step; a step that moves tracks carries the table permuted with them;
>   and a step that adds or removes a track row carries that row's record on the
>   side it exists. Solo is not in the table: it is a monitoring state, neither
>   saved nor undone.

**2. In the same section, extend the step kinds at `:166-168`** to read:

> - its kind (Move, Trim, Transform, Duration, Add, Delete, Reorder tracks, Add
>   track, Mute track, Lock track, Rename track), the clip **or track** it is
>   about if it is about one of them, and that clip's display name or that
>   track's label, which its description uses;

**3. In "Merging", replace the last sentence of `:121-122`**
("Add, Delete, Reorder tracks and Add track never merge, and nothing in Images
mode merges.") with:

> Add, Delete, Reorder tracks, Add track, Mute track and Lock track never merge,
> and nothing in Images mode merges. Rename track merges, so the keystrokes of
> one renaming are one step.

**4. In "How steps describe themselves" (`:281-283`), add to the Videos list:**

> "muting Dialogue", "unmuting Dialogue", "locking Dialogue", "unlocking
> Dialogue", "renaming Track 2".

The "Recording" table at `:184-193` gains three rows — the mute, lock and rename
callbacks against their kinds — and the "Contract for later edits" itself needs
no change: the track controls are expressible in the track rows, which is what
the contract already requires.

## Interface

### The gutter header

`crates/kuvatin/ui/app.slint:1772-1804` becomes a 168 px column of 30 px rows:

```
┌────────────────────────────────────────────────┐
│ ≡  Dialogue                       [M] [S] [L]  │  30px
└────────────────────────────────────────────────┘
        ← reorder area →            ← buttons →
```

- **Width 94 px → 168 px** (`:1773`). Nothing else in the interface assumes 94;
  the lane beside it is `horizontal-stretch: 1` and simply gets narrower.
- **`Theme.track-h` stays 30 px** (`ui/theme.slint:32`). It is the timeline's
  vertical unit: the lane stripes (`:1856`), the clip blocks (`:1875`), the
  drag-to-row arithmetic (`:1900`, `:1958`, `:1967`), the new-track zone
  (`:2059`) and the band height (`:1691`, `:2102`) are all built on it. Raising
  it to fit two lines of header would make eight tracks 434 px of band instead
  of 322 px and re-tune the whole timeline to solve a gutter problem.
- **Layout**: `HorizontalLayout { padding-left: 9px; padding-right: 8px;
  spacing: 6px; }` — the `≡` grip, then the name (`horizontal-stretch: 1`,
  `overflow: elide`), then an inner `HorizontalLayout { spacing: 2px; }` of
  three 20 × 20 buttons. Fixed width is about 90 px, leaving roughly 78 px for
  the name: about fifteen characters at the existing 9 px font, enough for
  "Dialogue" or "Music bed".
- **Buttons**: "M", "S", "L" in a 20 × 20 rounded rectangle. Idle is the
  header's own `#5e6675` glyph on transparent; on is `Theme.on-accent` on
  `Theme.accent2`. Each writes a `Tooltip` hint on hover — "Mute Dialogue",
  "Solo Dialogue", "Lock Dialogue", and the un- forms when on — reusing the
  global the undo work added (`ui/widgets.slint:730`), and uses the same string
  as its `accessible-label` with `accessible-checked` set.
  20 px is the largest square a 30 px row allows with breathing room; it is a
  small target and is named in Risks.

### What happens to the reorder gesture

Today `hta` (`:1786-1802`) is one `TouchArea` over the whole row. **It shrinks
to the left part of the row** — the grip and the name — rather than being
layered under three buttons. The buttons sit to its right in the same
`HorizontalLayout` and never overlap it, so there is no z-order to get wrong and
no click to disambiguate. The cost is that the right-hand 60 px of the header no
longer starts a reorder; the grip is the affordance and it stays where it is.

Each button's own `TouchArea` must call `kbd.focus()` on pointer-down and
maintain `Gesture.held`, as `hta` does at `:1790-1791`, or a click in the gutter
drops keyboard routing on the floor.

### Rename

Double-click the name (`TouchArea`'s `double-clicked`, present in Slint 1.16.1)
to edit it in place. A root property `track-renaming: int` (-1 = none) says
which row is editing; that row draws a `LineEdit` where the `Text` was, seeded
with the current name.

- `accepted` commits and clears `track-renaming`; Escape reverts (the modal
  `Esc` chain at `:327-344` must let a renaming row have it first); losing focus
  commits, so clicking away is not a silent loss.
- An empty name is allowed and means "back to Track N" — the generated label is
  what the header falls back to, so there is no way to end up with a nameless,
  unlabelled track.
- Committing calls `track-renamed(t, text)`. A commit to the same name records
  nothing, because `is_empty` compares the tables.
- **Key routing comes free.** `undo-keys` already refuses all three of its keys
  while `TextInputInterface.text-input-focused` is set (`:353`), which a focused
  `LineEdit` sets, so Ctrl+Z in the rename field is the field's own text undo.
  `video-keys` has no such guard (`:445-446`), but a focused `LineEdit` consumes
  Space and Delete itself, so they never bubble to play/pause and
  delete-selected-clip. That is the existing arrangement for the inspector's
  fields; it must be confirmed by hand for this one rather than assumed.

### Elsewhere in the timeline

- `timeline-track-labels: [string]` (`:108`) becomes
  `timeline-tracks: [TimelineTrack]`. Every `.length` use (`:1691`, `:1794`,
  `:2058`, `:2059`, `:2101`, `:2102`) is unchanged; the two `for lbl[t]` loops
  (`:1775`, `:1856`) bind `trk` and read `trk.name`.
- **The lane shows the state too**, so it reads without looking at the gutter:
  the row stripe at `:1856` darkens for a muted track and takes a flatter,
  greyer tint for a locked one.
- **A locked track's clips lose their affordances**: the clip block's drag and
  trim touch areas are disabled and its × is hidden when
  `timeline-tracks[clip.track].locked`, so the refusal messages above are the
  keyboard path's safety net rather than the normal experience.
- New root callbacks: `track-muted(int, bool)`, `track-soloed(int, bool)`,
  `track-locked(int, bool)`, `track-renamed(int, string)`, beside `add-track()`
  (`:188`).

### The eight-track cap

**It stays at eight** (`gui/video/timeline.rs:194`). Nothing here makes more
tracks cheaper — each row now carries three buttons, so a row costs more gutter
than it did — and eight rows is already 322 px of timeline band. It is a cap on
the "+ New track" button only: a project file with more tracks still opens with
all of them, which is today's behaviour and is left alone.

## Testing

**Pure, in `tracks.rs`:**

- `effective_mutes`: no solo gives the stored mutes back unchanged; one solo
  silences every other track including ones already muted; every track soloed is
  the same as none soloed; a track both soloed and muted stays silent;
  `solo_allowed: false` ignores solo entirely (the render path).
- `locked` for in-range, out-of-range and negative indices.
- `drop_refused`: locked source, locked target, both, neither, and a drop onto a
  track index past the end (the new bottom track), which is refused only for a
  locked source.
- `label`: a named track, an unnamed one ("Track 3" for index 2).
- `records` drops solo and keeps name, mute and lock.

**Pure, in `undo.rs`:**

- `set_track_rows` against a table: growing, shrinking, renaming a row in place,
  turning a mute on and off, and — the regression this design exists for — a
  table whose names are not "Track N" arriving intact rather than regenerated.
  The existing test at `:1113-1116` is rewritten for the new signature.
- `set_track_rows` carries each row's `soloed` across rather than clearing it.
- `is_empty`: a step whose only difference is one track's name, one track's
  mute, or one track's lock is **not** empty; identical tables with identical
  clip records are.
- `absorb`: two rename steps on the same track within a second become one step
  keeping the first "before" and the last "after"; renaming back to the original
  name leaves an empty step, which the history drops and which seals it;
  renames of two different tracks do not merge; a mute and an immediately
  following unmute are two steps.
- `describe` for each new kind, both directions of each toggle, and a rename
  whose track had no name ("renaming Track 2").
- `target_tracks` returns the before table for an undo and the after table for a
  redo.

**Pure, in `document.rs` (no engine, as that module's tests already are):**

- A `ProjectFile` with a track table round-trips through TOML.
- A file with no `tracks` key loads with an empty table.
- A `TrackRecord` with default fields serialises to nothing, so a project nobody
  renamed gains no noise in its file.

**Pure, in `project_file.rs`:**

- The track count on load is the max of the table's length, the deepest clip's
  track plus one, and two — one test per term winning.
- Solo is cleared by a load even if a saved file somehow carries one.

**Engine, real GStreamer, generated stills, alongside the existing video tests:**

- `set_track_mutes` makes `is_active_for_track` false for the timeline's audio
  track and leaves the video track active, so the picture survives a mute.
- `track_muted` reads back what was set, and is false for a track with no layer.
- A clip dropped onto a muted-but-empty track index lands on a layer that is
  already silent (this is `layer()`'s new line).
- `move_track` moves the mute with the layer: mute track 0, move it to 2, and
  the layer that is now at 2 is the silent one. This is the test that pins the
  assumption that GES's `move_layer` permutes like remove-then-insert.
- `prune_tracks` truncates the mute vector, and a track added back afterwards is
  audible.
- A mute survives `begin_render` / `end_render`, and `set_track_mutes` is inert
  while rendering.

These join the video tests CI gates on in `.github/workflows/release.yml`. None
of them needs real media, so they all belong in the self-contained step.

**By hand in the running app**, because none of it is a pure function:

- Rename a track, make an unrelated edit, Ctrl+Z, and the name is still there.
- Rename a track, save, open, and the name is still there; open the same file in
  a build without this change and it opens with the names gone but nothing else
  wrong.
- Mute a track and hear the preview go quiet; export and hear the file go quiet.
- Solo one track, start an export, and watch the solo buttons clear before the
  render starts.
- Lock a track and try all seven refusals, including the Delete key and the
  inspector sliders.
- Drag a header by the grip and reorder it; confirm the buttons do not reorder.
- Ctrl+Z inside the rename field undoes the typing, not the timeline.

## Risks and open questions

- **The brief this spec was written from says every edit enters through the
  timeline callbacks in `timeline.rs`. It does not.** The inspector transform
  enters through the preview tick (`mod.rs:306-315`) and adding a clip enters
  through `add_to_timeline` / `add_sequence_to_timeline` (`mod.rs:460`, `:532`).
  Both are in the refusal table above. A plan that guards only `timeline.rs`
  ships a lock that a slider can walk straight past.
- **Older builds silently drop the track table.** `ProjectFile` does not set
  `deny_unknown_fields` and the version gate only refuses *newer* format
  numbers, so 2.12.0 opens a project saved by this change with its tracks
  unnamed, audible and unlocked. Accepted over bumping `FORMAT_VERSION` to 2,
  which would refuse the file outright. The one that stings is mute: a track
  silenced on purpose plays in an older build.
- **Positional track state is only as good as the permutations.** Three places
  must move or truncate the state in lockstep with the layers —
  `Project::move_track`, `Project::prune_tracks` and `set_track_rows` — and a
  fourth, `layer()`, must apply it to a layer it creates. The engine tests above
  exist for exactly this. If a fifth place ever reorders layers, it inherits the
  obligation, which is the price of guiding decision 1.
- **Undo can change a locked track**, by design (see Lock). Whether that
  surprises anyone is unknown until it is used.
- **A reorder shifts locked tracks.** A locked track can be pushed up or down by
  another track's reorder; only dragging the locked track itself is refused.
  Open question: is "locked" expected to mean "this track does not move either"?
  If so, the refusal belongs in `on_track_reordered`'s clamp as well, and the
  whole gutter freezes around one locked track — which is why it is not the
  default here.
- **20 px buttons in a 30 px row are a small target**, three of them per row,
  and they are the only new hit areas in the timeline. If they prove fiddly the
  next move is a 36 px row height, which is a re-tune of the whole timeline
  band; do not reach for it without the complaint.
- **Solo is cleared by a render rather than restored after it.** The user gets
  it back with one click, but they do have to notice. Open question: should the
  export confirmation say "solo will be turned off" when solo is on, or is the
  visible clearing enough?
- **The clip block's disabled drag on a locked track is untested by anything
  pure.** It lives entirely in `app.slint` and is only checked by hand.
- **The rename field's key routing is inferred, not measured.** `undo-keys`
  guards on `TextInputInterface.text-input-focused` (`:353`) but `video-keys`
  does not (`:445-446`); the reasoning that a `LineEdit` eats Space and Delete
  itself matches the inspector's fields, but this row is inside a `TouchArea`
  that takes focus on pointer-down, which the inspector's fields are not.
- **The eight-track cap is a button guard, not an invariant.** A project file
  with twelve tracks opens with twelve rows and a dead "+ New track". That is
  true today; this spec neither fixes nor worsens it.
- **This spec is written against `master`, where `Project` has only the `dirty`
  repaint flag.** The unreleased in-app-update branch adds a second flag,
  `unsaved`, and a `touched()` helper that sets both. If that branch lands
  first, `set_track_mutes` should call `touched()` rather than
  `self.dirty.set(true)`, so that muting a track counts as work the user would
  lose by closing. Whoever plans this should check which flag exists before
  writing the line.

## Out of scope

- **Spec 1's quick wins**: split at the playhead, frame stepping and the shuttle
  keys, scale above 100 percent, clip speed, and the audio waveform. The
  waveform is per clip, not per track: it records no undo step and needs no
  saved state, so it is Spec 1's despite the word "audio".
- **Spec 3's media features**: cross-dissolves and text overlays.
- **Reverse playback**, which is in no spec because the bindings have no API for
  it.
- **Per-track hide** (the video half of `set_active_for_tracks`), per-track
  volume or gain, and per-clip mute (`TrackElement::set_active`). None is in the
  backlog entry; mute here is a track's audio, on or off.
- **Giving a track a stable id**, and with it a clip that names its track by id
  rather than by index. Rejected in guiding decision 1; it would be a format
  break.
- **Locking a clip rather than a track**, and any lock on the Images mode file
  list.
- **Undo for solo**, and saving solo in a project file.
- **Raising or removing the eight-track cap.**
