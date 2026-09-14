# Kuvatin — Undo Design

> **For agentic workers:** This is the validated design (spec) for undo in
> Kuvatin's Videos timeline and Images mode (backlog item `ui-undo`). The next
> step is the `writing-plans` skill. The editor features (backlog item
> `ui-editor`) come after this as three separate specs, and every edit they add
> must be recordable by this history: see "Contract for later edits".

## Goal

Every change to the work can be taken back and put back. In Videos mode that
is every change to the timeline; in Images mode it is crops and the file list.
Undo is visible (buttons whose hint names the step), fast (no rebuild of the
timeline), and exact (a clip comes back where it was, with the same identity,
transform and thumbnail).

## Background — current state

- **Timeline edits commit straight to the engine with no history.** They enter
  through the callbacks in `crates/kuvatin/src/gui/video/timeline.rs` (drop,
  trim, duration, track reorder, add track, remove), the transform timer in
  `gui/video/mod.rs` (`pending_xform` → `set_clip_layout`), and
  `add_to_timeline` / `add_sequence_to_timeline`. A mis-aimed Delete destroys a
  clip silently.
- **The engine moves clips by amounts and clamps.** `Project::slide_clip` stays
  inside the gap between neighbours; `trim_clip` keeps a 0.2 s minimum and the
  source's `max-duration`. `remove_clip` prunes empty trailing layers and
  `move_clip_to_track` creates layers on demand. A `ClipId` is the name GES
  assigns when the clip is added.
- **Project records exist.** `Project::clip_records()` and
  `to_document()` / `apply_document()` arrived with project files in 2.10.0.
  `apply_document` removes every clip, checks each source, re-adds it (with a
  new `ClipId`), and the interface then rebuilds rows, tracks, the media bin and
  every thumbnail, clears the selection and moves the playhead to zero.
- **Images mode.** The file list is always sorted and deduplicated
  (`add_paths`). Crops are kept per path in absolute pixels and are written only
  by `apply-crop`; there is no way to reset a crop. Clearing the list drops every
  path, crop and cached thumbnail (`ThumbCache` holds raw RGBA `ThumbData`);
  removing a file drops its path and crop. Both are refused while a batch runs.
- **No unsaved-changes tracking.** The engine's `dirty` flag means "the preview
  needs repainting" and `refresh_preview` resets it. Opening a project asks for
  confirmation whenever the timeline has clips.
- **Keyboard.** One `FocusScope` routes keys to `modal-keys`, then `video-keys`
  or `image-keys`. A focused `LineEdit` consumes its own keys. Ctrl+arrow nudges
  and Shift+arrow trims call the same callbacks as mouse drops and trims.
  Inspector sliders fire `changed` continuously, and the 100 ms timer applies
  only the latest transform; there is no release event.
- **Toolbars.** The timeline toolbar uses `TimelineChip { label, hint }` for the
  zoom controls; `hint` is only the chip's accessible label, and the app has no
  visible tooltip anywhere. The Images toolbar is a `SecondaryButton` row ("Add
  files…", "Clear", then a stretch spacer).

## Measurements

Release build, 2026-09-13, alternating a generated still and the CI fixture
video across three tracks:

| Clips | Read every clip's record (`clip_records`) | Full restore (`apply_document`, best of 3) |
|------:|------------------------------------------:|-------------------------------------------:|
| 10    | 0.037 ms                                  | 18 ms                                      |
| 50    | 0.18 ms                                   | 369 ms                                     |

The restore figures exclude the interface rebuild. Reading records is cheap
enough to do before and after every edit; restoring the whole project on every
Ctrl+Z is not.

## Guiding decisions (locked during brainstorming)

1. **Undo first.** The editor features follow as three later specs: quick wins
   (split at the playhead, frame stepping and J/K/L, scale above 100%), track
   controls (mute, solo, lock, rename), and media features (cross-dissolves,
   speed and reverse, text overlays, waveform).
2. **Scope.** The Videos timeline, and Images mode crops and file list. Two
   separate histories. The settings panel is not undoable.
3. **Visible.** Ctrl+Z, and Ctrl+Y or Ctrl+Shift+Z, plus Undo and Redo buttons
   in both modes whose hover hint names the step.
4. **Timeline approach: record each edit's changes.** Capture clip records
   before and after an edit and keep what changed. Not whole-project snapshots
   (too slow, loses identity and selection) and not hand-written inverse edits
   (clamping makes a reversed amount inexact, and every later feature would need
   its own inverse).

## Architecture

### History core

A new interface-free module in the app crate, `crates/kuvatin/src/gui/history.rs`,
used by both modes:

- Two stacks, undo and redo, capped at **200 steps** per history; the oldest
  step is dropped when the cap is reached.
- **Recording** a step: an empty step (nothing changed) is ignored and leaves
  the redo stack alone. Otherwise it either merges into the step on top of the
  undo stack (see Merging) or is pushed, and the redo stack is cleared.
- **Undo and redo are two-phase.** The caller looks at the top step, applies it,
  and only then tells the history it succeeded, which moves the step to the
  other stack. A failed apply leaves the step where it was, and seals the
  history (see Merging).
- Each step **describes** itself as a noun phrase ("trim of intro.mp4"), which
  the button hints complete: "Undo trim of intro.mp4", "Redo trim of
  intro.mp4", "Nothing to undo".
- The current time is passed in rather than read, so tests use a fake clock.
- `clear()` empties both stacks.

### Merging

A newly recorded step merges into the step on top of the undo stack when all of
these hold:

- both are the same kind, and that kind is **Move**, **Trim**, **Transform** or
  **Duration**;
- both are about the same clip;
- less than **one second** has passed since the top step last changed;
- the history is not **sealed**. An undo, a redo or a failed apply seals it, and
  the next change that is not empty unseals it, so a change made right after an
  undo always starts a step of its own.

The merged step keeps the older "before" and takes the newer "after", and its
time becomes the newer one. One slider drag, one mouse drag, or a held
Ctrl+arrow therefore becomes one step. Add, Delete, Reorder tracks and Add track
never merge, and nothing in Images mode merges.

A merged step that ends up changing nothing (a drag back to where it began) is
removed, and that seals the history too, so the next change cannot merge into
the step beneath it.

### Engine additions (`kuvatin-video`, `Project`)

- **`set_clip_records(writes)`** writes clips' start, in-point, duration, track
  and transform exactly as given, with no clamping, because it restores a state
  the engine already accepted. GES refuses any moment where one clip sits fully
  on top of another (see Risks), so clips that trade tracks or places would
  collide halfway: every clip whose place or times change is first parked alone
  on a new layer below the timeline, then set and moved to its track, and the
  parking layers are removed. When shrinking, in-point and duration are written
  in the order `trim_clip` uses, so in-point plus duration never transiently
  exceeds `max-duration`. Every clip is read back, and the ones that did not
  land are returned. A clip that did not land goes back as it was, or stays
  parked on the first free parking layer if a clip that landed took its place,
  so a refused write changes nothing else and retrying it adds no tracks.
- **`restore_clip(id, record)`** re-adds a removed clip as its record describes
  it, under its old `ClipId`. GES replaces a name in its own `uriclipN` pattern
  with its next one (see Risks), so a removed clip's name cannot be asked
  back; but the engine never looks clips up by GES name, so it keeps the
  restored clip under the ID the interface and the history still hold. GES
  never gives out a name twice in a process, so no later clip can arrive
  under it.
- **`source_available(uri)`** answers whether a source is still there. A file is
  looked for on disk, because GES answers from a cache that outlives it, and
  undo only restores sources the session has used; anything else, an image
  sequence, is dropped from the cache and discovered again, bounded by the
  discovery timeout.
- **Pruning to a track count.** After an undo or redo, empty trailing layers
  beyond the step's recorded track count are removed, so undoing a move onto a
  new bottom track also removes that track.
- `remove_clip` and `clip_records` are used as they are.

### Timeline steps (app crate)

A `TimelineStep` holds:

- its kind (Move, Trim, Transform, Duration, Add, Delete, Reorder tracks, Add
  track), the clip it is about if it is about one clip, and that clip's display
  name, which its description uses ("trim of intro.mp4");
- for each affected clip, its record **before** and **after**, where "none"
  means the clip did not exist on that side;
- the timeline row of every clip that exists on only one side, so a clip that
  comes back gets its row as it was, without decoding: its name (the engine's
  record spells one from the URI), kind and thumbnail;
- the timeline's track-row count before and after.

**Comparing.** A pure function takes the records before and after an edit,
keyed by `ClipId`, and returns the clips whose record changed, appeared or
disappeared. Records are compared exactly: an unchanged clip reads back
identical values. No differing clips and an unchanged track-row count means no
step.

**Recording.** One helper wraps every edit: read the records and the track-row
count, run the edit, read them again, compare, and record the step. It is
called from:

| Where | Step kind |
|-------|-----------|
| `on_timeline_clip_dropped` | Move (also covers moving to another track) |
| `on_timeline_clip_trimmed` | Trim |
| `on_inspector_duration_changed` | Duration |
| the transform timer's `set_clip_layout` | Transform |
| `on_track_reordered` | Reorder tracks |
| `on_add_track` | Add track (track rows only) |
| `remove_timeline_clip` (× button and Delete key) | Delete |
| `add_to_timeline`, `add_sequence_to_timeline` | Add |

Keyboard nudges and trims reach the same callbacks, so they need no hook of
their own.

**Undoing a timeline step:**

1. Check `source_available` for every clip the undo brings back. If any is
   missing, change nothing, show an error naming the file, and keep the step.
2. For each affected clip: remove it if it did not exist before; restore it if
   it existed before but not after; otherwise write its "before" record.
   Removals are applied first, then all writes at once with
   `set_clip_records`, then restores. After the removals and writes every clip
   is where the "before" side has it, so a restored clip never lands on one
   that has yet to move away.
3. Prune layers to the step's "before" track count, and set the timeline's
   track rows to that count.
4. Update only the affected timeline rows. This is a pure function from the
   current rows and the applied records to the new rows, so it is unit-tested.
   Then update the timeline duration and repaint the preview.
5. The selection stays on its clip if that clip still exists and is cleared
   otherwise. A step about exactly one clip selects that clip, so the change is
   visible.
6. Tell the history the undo succeeded.

**Redo** is the same with "before" and "after" swapped.

If the engine refuses a write partway through, show the error, rebuild the
affected rows from `clip_records()` so the screen matches the engine, and keep
the step. Retrying it skips the clips its earlier try already brought back, so
none comes back twice.

### Images steps (app crate)

| Step | Keeps | Undo does |
|------|-------|-----------|
| Add files | the paths that were actually new | removes those paths; redo adds them back, with thumbnails from the cache or decoded again |
| Remove a file | its path and crop (or none); its thumbnail stays in the cache | adds both back; selects the file |
| Clear the list | every path, crop and thumbnail | restores the whole list |
| Apply a crop | the file, its previous crop (or none), the new crop | writes the previous crop, updates the row's cropped mark and, if the file is selected, the crop outline |

Undo and redo go through the functions the buttons already use (`add_paths`,
`sync_rows`, the crop map), so rows, sorting, selection and thumbnails behave
exactly as they do today. The sorted list puts a restored file back in its old
place without any stored position. A path that no longer exists on disk is
skipped, and the user is told how many files could not come back.

### Lifetime and refusals

- The timeline history is cleared when a project is opened. The engine is
  created once per session, before anything can be recorded, so a new engine
  always starts with an empty history. The Images history lasts the session.
- Undo and redo are refused, and both buttons are disabled:
  - during an export, including while it is starting;
  - while an Images batch is running;
  - while a dialog is open;
  - while the video engine is down.

## Interface

- **Tooltip.** Slint has no tooltip element. A `Tooltip` global holds the text
  and position of the one tooltip in the window. A control with a `hint` writes
  its hint text and its own `absolute-position` into that global while it is
  hovered, and a `TooltipLayer`, the window's last child, draws the tooltip
  above everything. The existing zoom chips get visible hints from this too.
- **Videos mode.** Two `TimelineChip`s, "Undo" and "Redo", in the timeline
  toolbar, left of the zoom chips. Their hint ("Undo trim of intro.mp4", or
  "Nothing to undo" / "Nothing to redo") shows on hover. `TimelineChip` gains
  an `enabled` property and they are greyed out when unavailable; the hint
  still shows on a greyed chip.
- **Images mode.** Two `SecondaryButton`s, "Undo" and "Redo", at the right end
  of the "Add files… / Clear" row. `SecondaryButton` gains an optional `hint`,
  shown on hover and used as its accessible description.
- **Shortcuts.** Ctrl+Z undoes; Ctrl+Y and Ctrl+Shift+Z redo. They are handled
  in `video-keys` and `image-keys` and act on the current mode's history. A
  focused text field keeps Ctrl+Z for its own text. `modal-keys` runs first,
  and undo keys do nothing while a dialog is open.
- **Accessibility.** Each button's hint is also its accessible description.
- **How steps describe themselves** (the hint prefixes "Undo " or "Redo "):
  - Videos: "move of intro.mp4", "trim of intro.mp4", "transform of
    intro.mp4", "duration of still.png", "deleting intro.mp4", "adding
    intro.mp4", "track reorder", "new track".
  - Images: "adding 12 files", "removing photo.jpg", "clearing the list (40
    files)", "crop of photo.jpg".

## Contract for later edits

Every later timeline edit (split, speed, text overlays, track mute, and so on)
records through the same helper. Its state must be part of `ClipRecord` or the
track rows, so the before/after comparison sees it. That is the same state a
saved project needs, so it costs nothing extra. An edit whose state cannot be
expressed that way needs a new step kind, added to this design before it is
built.

## Testing

- **History core (pure).** Undo and redo; a new step clearing redo, and an
  empty one ignored without touching it; the merge rule's conditions against a
  fake clock, including a change at exactly one second, which does not merge;
  the seal after an undo, after an explicit seal and after a gesture that
  merged back to nothing; the 200-step cap dropping the oldest; the two-phase
  undo leaving a failed step in place; the hint text.
- **Comparing records (pure).** Changed, added and removed clips; identical
  records produce no step.
- **Engine (real GStreamer, generated stills, like
  `a_timeline_survives_being_saved_and_reopened`).**
  - Exact write-back after a slide that was clamped against a neighbour, and
    after a trim clamped at the minimum.
  - Delete, then restore: the clip comes back exactly, transform and (for real
    media) in-point included, under its old `ClipId`, and not twice.
  - A source deleted after its clip was used is noticed, for a still and for an
    image sequence.
  - A move onto a new bottom track, then undo: the track is gone again; several
    empty bottom tracks go down to a count, never past a clip or below one
    track, and GES drops the same layers the engine does.
  - A track reorder and its undo.
  - Two clips trading places on a track, in one batch; a write the engine
    refuses is reported and put back, however often it is retried; a clip that
    can go neither way stays parked on one track.
  - A left trim of real media undone and redone, which pins the order of
    in-point and duration (advisory, like the other live-media tests).
  - A missing source: named, and nothing changed.

  These join the video tests CI gates on in `.github/workflows/release.yml`.
- **Images history (pure, over paths, crops and thumbnails).** Add, remove,
  clear and crop round-trips; adding an already present file records nothing; a
  file missing on restore is skipped and counted.
- **Interface.** The row update after an undo is a pure function with its own
  tests. Then, by hand in the running app: a slider drag undone as one step, a
  delete undone with Ctrl+Z, Ctrl+Z inside a text field, and clearing the Images
  list then undoing.

## Risks / open questions

- **GES names every new clip afresh.** Measured: `set_name` with a removed
  clip's name (GES's own `uriclipN` pattern) succeeds and the clip still gets
  the next name, before or after it joins a layer. The engine keeps a restored
  clip under its old ID itself; the history's rename path stays as a safety
  net and does not run.
- **GES caches discovered sources.** A deleted file still discovers from the
  cache, and a clip restored from it fails at preview with an error that names
  nothing, so `source_available` looks for files on disk and rediscovers the
  rest.
- **Exact writes without clamping** are correct only because steps are undone
  strictly in order, which a single linear history guarantees.
- **GES refuses overlaps without an error.** One clip fully on top of another,
  or three clips overlapping, is refused even for a moment: measured,
  `move_to_layer` returns an error, `set_start` returns false and leaves the
  clip where it was, and `add_clip` fails. Writes therefore run as one parked
  batch and are read back, and restores come after them.
- **Merging depends on timing.** A slow drag that pauses for more than a second
  becomes more than one step. This is accepted.
- **Memory.** A Clear step for a 1,000-file list keeps about 16 MB of
  thumbnails while it is in the history, bounded by the 200-step cap.

## Out of scope

- Undo for the settings panel, presets, export settings and canvas size.
- Opening a project as an undoable step.
- Keeping the history across sessions or in project files.
- An unsaved-changes indicator or prompt built on the history.
- A visible history list.
