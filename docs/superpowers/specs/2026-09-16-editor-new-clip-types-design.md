# Kuvatin — Clips That Are Not Video

> **For agentic workers:** This is the validated design (spec) for the third
> slice of the editor-features backlog item (`ui-editor`): **cross-dissolves**
> and **text overlays**. The next step is the `writing-plans` skill. It builds
> on the undo design (`docs/superpowers/specs/2026-09-13-undo-design.md`) and
> must satisfy its "Contract for later edits" (lines 287–294). Two sibling
> specs cover the rest of `ui-editor`; see "Out of scope" for the split.

## Goal

The timeline can hold two things it cannot hold today: a **text overlay**, and
a **cross-dissolve** where two clips meet. Both survive everything the timeline
already promises — they save, they reopen, they undo and redo, they export —
because the same records, the same diff and the same history carry them.

A text overlay is a clip you add, select, place, scale, fade, retime and type
into. A cross-dissolve is the picture of two clips overlapping on one track.

## Background — current state

### The model only knows URI clips

This is the substance of the work; the two features are one spec because both
put a clip on a layer that is not a `ges::UriClip`, and everything that hurts
about that is shared.

- **`clip_records`** (`crates/kuvatin-video/src/project.rs:1425`) builds every
  record inside a `filter_map` whose first line is
  `let uri = clip.downcast_ref::<ges::UriClip>()?.uri().to_string();`
  (`:1429`). Any clip that is not a URI clip is dropped on the floor, silently.
  That means it is missing from `to_document` (`:1412`) and therefore from
  every saved file, and missing from the undo diff, because `diff`
  (`crates/kuvatin/src/gui/video/undo.rs:78`) compares `ClipRecord`s and
  `Capture::of` (`undo.rs:53`) is built from `clip_records`. A clip the diff
  cannot see is a clip the history cannot bring back.
- **`apply_document`** (`project.rs:1476`) opens a project by removing every
  clip and re-adding each record with `add_clip_uri` (`:1502`), after
  `ges::UriClipAsset::request_sync` (`:1498`) proves the source exists. A
  record that is not a URI clip would be reported as a missing source.
- **`restore_clip`** (`project.rs:1225`) — the one undo uses to bring a deleted
  clip back — builds `ges::UriClip::new(&record.uri)?` (`:1236`) and nothing
  else.
- **`ClipRecord`** (`crates/kuvatin-video/src/document.rs:61`) is
  `uri`, `name`, `track`, `start`, `inpoint`, `duration`, `layout`, `sequence`.
  There is no field that says what kind of clip it describes; `uri` is assumed
  to be the whole identity.

### The overlap rule

- **`slide_within_gap`** (`project.rs:288-319`) confines a slid clip to the gap
  it already occupies on its layer: it may butt up against a neighbour on
  either side and no further. Its doc comment gives the reason — GES stacks
  whatever it is told to stack, and one clip hiding another with nothing on
  screen to say so is a bug the user cannot see.
- **`set_clip_records`** (`project.rs:1129`) parks every moving clip alone on a
  scratch layer below the timeline before writing it (`:1144-1213`), because
  GES refuses, without an error, any moment where one clip sits fully on top of
  another.
- **`snap_slide`** (`crates/kuvatin/src/gui/video/timeline.rs:391`) magnets a
  dragged edge onto the timeline origin or a neighbouring clip edge within 8 px,
  and drives both the live drag and the drop commit
  (`timeline.rs:161-182`, `app.slint:1892-1896`).
- **Trimming does not use the rule at all.** `trim_clip` (`project.rs:1059`)
  clamps only against the source's `max-duration` and the 0.2 s minimum
  (`trim_left_math`, `trim_right_math`, `project.rs:268-286`). It never looks at
  a neighbour. So a right-edge drag can already push a clip over the next one
  today; only sliding is forbidden. The invariant is already inconsistent.

### What the engine offers, unused

`gstreamer-editing-services` 0.23.5 with features `v1_20` and `v1_24`
(`crates/kuvatin-video/Cargo.toml:19`), so every `v1_18`-gated API is present.
Nothing in the repository touches any of it.

- `TimelineExt::set_auto_transition` (`auto/timeline.rs:400`) and
  `LayerExt::set_auto_transition` (`auto/layer.rs:239`) make GES insert a
  transition wherever clips on a layer overlap.
- `TransitionClip::new(VideoStandardTransitionType)` (`auto/transition_clip.rs:30`,
  returns `Option`), `TransitionClip::for_nick` (`:37`),
  `VideoStandardTransitionType::Crossfade` (`auto/enums.rs:797`).
- `TitleClip::new()` (`auto/title_clip.rs:22`, returns `Option`). It extends
  `SourceClip`; the bindings generate **no** `TitleClipExt`, because GES
  deprecated the clip's own setters. The state lives on `TitleSource`
  (`auto/title_source.rs:34-165`): `text`, `font_desc`, `text_color`,
  `background_color`, `halignment`, `valignment`, `xpos`, `ypos` — and `text`
  and `font_desc` are themselves marked "Since 1.16" deprecated in favour of
  child properties.
- `TextOverlayClip::new()` (`auto/text_overlay_clip.rs:30`) with
  `set_text` / `set_font_desc` / `set_color` / `set_xpos` / `set_ypos` /
  `set_halign` / `set_valign` (`:99-165`). It extends `OverlayClip` →
  `OperationClip`: it is an operation over whatever is beneath it, not a
  source.

### Everything else that will have to move

- The video track's restriction caps pin width and height only, with no format
  (`project.rs:803-806`, and again in `set_canvas_size`, `:884-891`), so the
  compositor is free to negotiate a format with alpha.
- `clip_layout` / `set_clip_layout` (`project.rs:1538`, `:1575`) read and write
  `posx`, `posy`, `width`, `height`, `alpha`, `volume` as child properties;
  `clip_natural_size` (`:679`) is `None` for anything with no
  `ges::VideoSource` track element, and the callers then fit to the canvas.
- The interface's whole per-clip vocabulary is `TimelineClip`
  (`crates/kuvatin/ui/app.slint:25-35`) and `ClipKind` (`:21`). A clip's
  appearance is keyed on the kind at `:1913-1917`; the block is drawn at
  `:1863-2019`. The inspector (`:1604-1684`) offers canvas size, position,
  scale, opacity, volume and — for a still only — duration.
- `kind_of` (`crates/kuvatin/src/gui/video/project_file.rs:330`) decides a
  row's kind from its URI alone. Callers: `project_file.rs:216` (reopening a
  project) and `undo.rs:278` (a clip coming back).
- `add_to_timeline` (`crates/kuvatin/src/gui/video/mod.rs:460`) appends media
  to track 0 for images and track 1 for videos, because layer 0 composites on
  top.
- `FORMAT_VERSION = 1` (`document.rs:20`); `ProjectFile::new` stamps it
  (`:100`), `save` writes what `new` stamped (`:108`), and `load` refuses
  anything higher (`:130-137`). `#[serde(default)]` is the established
  compatible route: `name` (`:65`) and `sequence` (`:75`).

## Guiding decisions (locked during brainstorming)

1. **One discriminator, in the record, optional in the file.** `ClipRecord`
   gains a single `Option<ClipBody>`. `None` means a URI clip, which is every
   record any release so far has written, so old files read unchanged.
2. **Cross-dissolves are automatic; the overlap is deliberate.** GES inserts
   and maintains the transition; Kuvatin only decides where clips overlap. The
   transition is therefore never a record, never a history step and never a
   line in a project file — it is a picture of state that already round-trips.
3. **A text overlay is a `TitleClip`, not a `TextOverlayClip`.** A title is a
   source, so every rule Kuvatin already has for a clip applies to it unchanged.
4. **Overlap becomes legal, bounded by what GES itself refuses.** Neither clip
   may fully cover the other, order on the track may not change, and a clip may
   not reach past the neighbour beyond its neighbour. The same rule governs
   sliding *and* trimming, which today disagree.
5. **The file version says what a file needs, not what wrote it.** A project of
   only media clips is still version 1 and still opens in every shipped build.
   A project containing a text overlay is version 2 and an older build refuses
   it out loud, with the message it already has.

## Architecture

### The record learns what kind of clip it is

`crates/kuvatin-video/src/document.rs`:

```rust
/// What a record describes, when it is not a clip on a media source. Absent —
/// which is every record written before this existed — means a URI clip, and
/// `uri` is the whole of its identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipBody {
    /// A text overlay, built as a GES `TitleClip`.
    Title(TitleRecord),
}

/// A text overlay's own state: what a `TitleClip` needs beyond the place,
/// times and transform every clip has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TitleRecord {
    /// What it says. May be empty and may contain newlines; an empty title is
    /// a clip you can still see and select on the timeline, which is what a
    /// half-typed title has to be.
    pub text: String,
    /// A Pango font description, e.g. "Sans Bold 48". Written whole so a later
    /// version can widen what the interface offers without changing the file.
    #[serde(default = "default_font")]
    pub font: String,
    /// The text colour as `#rrggbb` or `#rrggbbaa`. A string, not a number,
    /// because a project file is a document someone may read — the same reason
    /// times are seconds.
    #[serde(default = "default_text_color")]
    pub color: String,
    #[serde(default)]
    pub halign: TitleHAlign, // Left, Center (default), Right
    #[serde(default)]
    pub valign: TitleVAlign, // Top, Center (default), Bottom
}
```

`ClipRecord` gains one field, last, after the two tables already there so TOML
serialisation keeps its scalars-before-tables order:

```rust
    /// What kind of clip this record describes. Absent means a URI clip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<ClipBody>,
```

A title record carries `uri: ""` and a `name` spelled from its text (the first
line, elided to 24 characters, or `"Text"` when empty), so the timeline row and
the media-bin-free parts of the interface have something to show.

Why an enum with one variant rather than a bare `Option<TitleRecord>`: the tag
is what costs something to add later. `[clips.body.title]` in a file today
makes `[clips.body.colour_card]` free tomorrow, and the alternative — one
`Option<…>` field per kind — allows states like "kind says title, payload is
absent" that this shape cannot express.

Two pure helpers, each with its own tests, because nothing else in the crate
parses colours:

```rust
/// `#rrggbb` or `#rrggbbaa` to the value GES takes for a title's text and
/// background colour. None for anything else.
pub fn parse_color(s: &str) -> Option<u32>;
/// The inverse, always eight digits when the alpha is not 0xff.
pub fn format_color(v: u32) -> String;
```

### `clip_records` — from a downcast to a match

`project.rs:1425-1470`. The `filter_map` body moves to a private method so the
sort and the `ClipId` plumbing stay where they are:

```rust
/// One clip as a record, or None for a clip the engine did not put on the
/// timeline itself — which, after this, is only an auto-inserted transition.
fn record_of(&self, id: &ClipId, clip: &ges::Clip) -> Option<crate::document::ClipRecord>;
```

It fills `track`, `start`, `inpoint`, `duration` and `layout` exactly as today
for every kind, then:

- `clip.downcast_ref::<ges::UriClip>()` → `uri` from the clip, `name` spelled
  from the URI as now, `body: None`.
- `clip.downcast_ref::<ges::TitleClip>()` → `uri: String::new()`,
  `body: Some(ClipBody::Title(self.title_of(id)?))`, `name` from the text.
- anything else → `None`.

The last arm stays, but nothing the engine adds can reach it: `add_clip_uri`,
`append_clip_uri`, `restore_clip` and the new `add_title_clip` are the only
writers of `self.clips`, and GES's auto-inserted transitions are never put
there (see below). It logs once per process rather than silently dropping, so a
future kind that forgets this method is noisy instead of lossy.

Reading a title back uses child properties, not `TitleSourceExt`, whose `text`
and `font_desc` are deprecated and whose getters need the track element:

```rust
/// A title clip's text and styling, or None for any other clip.
pub fn title_of(&self, id: &ClipId) -> Option<crate::document::TitleRecord>;
```
reading `"text"`, `"font-desc"`, `"color"`, `"halignment"`, `"valignment"`
through `clip.child_property(..)`, exactly as `clip_layout` reads `"alpha"`
(`project.rs:1538-1566`). The alignments come back as
`value.get::<ges::TextHAlign>()` (the bindings give them a `StaticType`,
`auto/enums.rs:482`), falling back to `Center` when the property is absent.

### `apply_document` and `restore_clip` — two builders, one shape

Both grow the same two-armed match on `rec.body`. The new builder:

```rust
/// Add a text overlay: a GES `TitleClip` on `track` at `start`, lasting
/// `duration`. It has no source, so nothing is discovered and nothing can be
/// missing.
pub fn add_title_clip(
    &mut self,
    title: &crate::document::TitleRecord,
    track: usize,
    start: Duration,
    duration: Duration,
) -> Result<ClipId>;

/// The same, at the end of `track`, for the "Add text" action.
pub fn append_title_clip(
    &mut self,
    title: &crate::document::TitleRecord,
    track: usize,
    duration: Duration,
) -> Result<ClipInfo>;

/// Rewrite a title clip's text, font, colour and alignment. Inert for a clip
/// that is not a title, and while rendering.
pub fn set_title(&mut self, id: &ClipId, title: &crate::document::TitleRecord);
```

`add_title_clip` builds `ges::TitleClip::new()` (an `Option`; `None` is an
error naming GES), sets start/inpoint/duration, applies `set_title`, adds it to
`self.layer(track)`, commits and registers it in `self.clips` under the GES
name — the same seven steps `add_clip_uri` takes (`project.rs:931-957`), so the
ID rules, the naming check and the async-commit rule are untouched.

`set_title` writes `"text"`, `"font-desc"`, `"color"`, `"halignment"`,
`"valignment"` and — once, at creation — `"background"` with alpha 0, so a
title composites over what is under it instead of hiding it. (See Risks: the
default background and the colour byte order are the two things the first task
of the plan must measure.)

In **`apply_document`** (`project.rs:1489-1512`) the discovery check and the
`missing` report stay for `body: None` and are skipped entirely for a title: a
title has no source, so it can never be missing and must never be named as
such.

In **`restore_clip`** (`project.rs:1225-1252`) the `ges::UriClip::new` line
becomes the match; everything else — the rendering guard, the duplicate-ID
refusal, the layer, the commit, the `self.clips` insert under the *old* ID and
the `set_clip_layout` — is unchanged, and for a title `set_title` runs beside
`set_clip_layout`.

The undo path's missing-source check (`undo.rs:498`,
`.filter(|(_, record)| !p.source_available(&record.uri))`) gains
`record.body.is_none() &&` in front of it. A title record must never be handed
to `source_available`, which would look for a file called `""` and refuse to
bring the clip back.

### `set_clip_records` also writes text

`set_clip_records` (`project.rs:1129`) today writes place, times and layout.
With `body` in the record, undoing a text edit would otherwise restore the
geometry and leave the new text. In the final loop that reapplies the layout to
every clip that landed (`project.rs:1207-1210`), a record with a title body
also gets `set_title`.

`clip_placed_as` (`project.rs:337`) is deliberately **not** extended to compare
text. It decides which clips are parked on scratch layers, and parking a clip
to change a string would be wasted work and a needless chance of refusal. Text
is written unconditionally for landed clips; geometry alone decides parking.

### Overlap: from a gap to an order

`slide_within_gap` becomes `slide_within_layer`, same signature and same place
(`project.rs:288-319`), still a pure function over `(start, dur, delta,
neighbours)` so its tests stay unit tests. `neighbours` stays every other clip
on the layer as `(start, end)`; the function now sorts them by start and picks
four: `P` (the last one starting before this clip), `PP` (the one before that),
`N` (the first starting after), `NN` (the one after that).

With `M = MIN_TRIM_NS` (0.2 s, `project.rs:261`) and `d` the clip's duration:

```
floor = max(0,
            P.start + M,          // P keeps its head: order cannot change
            P.end   + M - d,      // this clip is not swallowed by P
            PP.end)               // never reach past the neighbour's neighbour
ceil  = min(N.start     - M,      // this clip is not swallowed by N
            N.end   - M - d,      // N keeps its tail
            NN.start    - d)
new_start = desired.clamp(floor, max(ceil, floor))
```

Every absent neighbour drops its own term. With no neighbours at all this is
the old `desired.max(0)`. A clip already overlapping something keeps today's
escape hatch — the terms are computed from where its neighbours are, so it can
always be dragged apart. A gap too small still leaves the clip exactly where it
was, because `ceil` falling below `floor` collapses the clamp.

The rule is the engine's own refusal made explicit: GES rejects one clip fully
on top of another, and three clips overlapping at one instant. Everything this
allows, GES accepts.

**Trimming gets the same rule.** `trim_clip` (`project.rs:1059`) keeps
`trim_left_math` and `trim_right_math` untouched — they are CI-gated pure
functions — and composes a new one over them:

```rust
/// The bounds a trimmed edge may not cross, from the clip's neighbours on its
/// layer. Returns (earliest start, latest end) in nanoseconds.
fn trim_bounds(start: i128, neighbours: &[(i128, i128)]) -> (i128, i128);
```
`earliest start = max(0, P.start + M, PP.end)`;
`latest end = min(N.end - M, NN.start)`. A left trim clamps its new start up to
the first, a right trim clamps its new end down to the second. This is a new
restriction on behaviour that exists today — a right-edge drag can currently
bury the next clip, which GES then silently refuses — so it closes a hole
rather than opening one.

**Snapping does not change.** `snap_slide` (`timeline.rs:391-416`) magnets to
clip edges and the origin, which is exactly the "butt up, no dissolve"
position, within 8 px. A drag that goes further than 8 px past a neighbour's
edge is asking for overlap and now gets it. Adding a second magnet at "one
default dissolve of overlap" was considered and rejected: at any usable zoom
the two magnets are a few pixels apart and fight each other. The function, its
callers and its nine existing tests are untouched; only the doc comment's
claim about what happens past the magnet needs rewording.

`set_clip_records` is untouched by the rule, because it never clamps — it
writes a state the engine already accepted. Its parking dance is what makes
that safe, and legal overlaps go through it exactly as legal gaps do today.

### Cross-dissolves: automatic transitions over a deliberate overlap

`Project::new` sets `timeline.set_auto_transition(true)` on the timeline it
builds (`project.rs:781`), and `Project::layer` (`:898`) sets it on every layer
it appends, so a layer created later cannot be missed. GES then inserts a
`GESTransitionClip` (video crossfade, and an audio crossfade on the audio
track) wherever two clips on one layer overlap, resizes it as they move, and
removes it when they part.

Why automatic rather than an explicit `TransitionClip` Kuvatin owns:

- **The undo contract is satisfied with nothing added.** The undo design
  requires an edit's state to live in `ClipRecord` or the track rows. A
  dissolve's entire state is *where two clips are*, which is already `start`
  and `duration` on two records. Making a dissolve is a plain
  `StepKind::Move` — the existing kind, the existing recorder, the existing
  `set_clip_records` write-back. No new step kind, no new record field, no
  interaction between transitions and the parking dance.
- **It cannot go stale.** An explicit transition clip would need an invariant
  nothing in the model can express — "this clip is parented to those two" — and
  every move, trim and delete would have to maintain it or leave a dissolve
  hanging over nothing.
- **The file cost is zero.** Reopening a project re-creates the overlap from
  the two clips' records; the layer's auto-transition flag re-creates the
  crossfade. Cross-dissolves need no format change at all; the format change in
  this spec is entirely for text.
- **An auto-inserted clip is never ours.** `self.clips` is written only by the
  engine's own four builders, so a GES-inserted transition is not in it. It is
  therefore absent from `clip_records`, from `layer_neighbours`
  (`project.rs:1039`, which iterates `self.clips`) and from the diff — not
  *dropped* by them, which is the failure this spec exists to fix, but outside
  them by construction.

What is given up, stated plainly: the only transition is a cross-dissolve; its
length is the overlap and cannot differ from it; and it is not a thing you can
select, move or trim on its own. For a tool whose timeline has no ripple edit,
that is the right size.

**One consequence to fix.** Three places decide a layer is empty by asking GES
(`remove_clip`'s trailing prune, `project.rs:1341-1348`; `set_clip_records`'
parking sweep, `:1196-1204`; `prune_tracks`, `:1271-1290`). An auto transition
is a clip on the layer as far as `Layer::clips()` is concerned, so a layer
holding only a stranded transition would read as non-empty and stop the sweep,
leaving a dead track row. All three switch to a new private
`fn layer_is_empty(&self, layer: &ges::Layer) -> bool` that asks whether any
clip in `self.clips` is on it. Removing the layer takes any transition on it
with it.

**Making one.** The timeline toolbar (`app.slint:1700-1765`) gains a
`TimelineChip` beside Undo and Redo:

- Enabled when exactly one clip is selected, that clip has a clip before it on
  its own track, the engine is up and no modal is open — the same guard
  expression Undo and Redo already use.
- With no overlap yet it reads **"Dissolve"**, hint "Cross-dissolve with the
  clip before it (1.0 s)", and slides the selected clip left so it overlaps its
  predecessor by the default 1.0 s: `delta = -((S.start - P.end) + 1.0)`,
  through the existing `on_timeline_clip_dropped` path, so it is bounded by
  `slide_within_layer` and recorded as a `Move` like any drag. If the rule
  allows less, the dissolve is shorter and the hint says the real number
  afterwards.
- With an overlap already there it reads **"Remove dissolve"**, hint naming the
  current length, and slides right by the overlap so the clips butt up.
- A dissolve at a clip's right edge is the same object as one at the next
  clip's left edge, so one action covers both and there is no second chip.

Which clip is "before" it, and by how much, is a pure function over the rows
the interface already holds, so the engine grows no API for this at all:

```rust
// crates/kuvatin/src/gui/video/timeline.rs
/// The clip before `sel` on its own track and the slide that would give them a
/// `want` second dissolve — negative to make one, positive to take one away.
/// None when nothing precedes it on the track, or when the clip before it is
/// too short to give up 0.2 s.
fn dissolve_slide(rows: &[TimelineClip], sel: usize, want: f32) -> Option<(usize, f32)>;
```

### The project file

`FORMAT_VERSION` becomes `2` — the highest this build can *read*. What a file
is *stamped* with becomes the lowest version that can read it:

```rust
/// The format version a document needs: 2 once any clip is something a
/// version-1 reader has no shape for, 1 otherwise.
pub fn required_version(clips: &[ClipRecord]) -> u32;
```

`ProjectFile::new` (`document.rs:98-105`) stamps `required_version(&clips)`
instead of `FORMAT_VERSION`. Nothing else changes: `save` still writes what
`new` stamped, and `load`'s existing refusal (`:130-137`) does the rest.

So, concretely, what an older build does with a file containing a text overlay:
**it refuses the whole file, by name and by number**, with the message it
already has — "… was written by a newer version of Kuvatin (format 2, this
build reads 1)". It does not open the project with the title missing, and it
cannot be made to re-save it without the title. A project of only media clips
is still stamped 1 and still opens in every build ever shipped, and a file
written by an older build still opens here, with `body` defaulting to `None`.

The cost is named and accepted: the refusal is all-or-nothing, so one title
clip makes a project unopenable on an older build even to look at the rest.
Silently losing work is worse than loudly refusing it, and the loud refusal
already exists and already reads well.

The optional-field route was considered. It does not actually work here: `uri`
has no `serde` default, so an old build either fails to parse the file at all
or — with `uri = ""` present — opens it, reports the title as a missing source
and drops it, leaving the user one Ctrl+S from losing it. The version is the
honest place for this.

### Undo

- Adding, deleting, moving, trimming, retiming and transforming a title clip
  go through the existing kinds and the existing recorder with no change.
- Making or removing a dissolve is a `Move`.
- Editing text needs a kind of its own — not because the state is unexpressible
  (it is `ClipRecord::body`, which `diff` compares for free) but because
  `describe` (`undo.rs:158-172`) would otherwise call it a transform.
  `StepKind` gains `Text`, describing itself as `"editing the text of {name}"`,
  and it **merges** (`StepKind::merges`, `undo.rs:172-177`), so a typed
  sentence is one step under the history's one-second rule rather than one step
  per keystroke. Every other change in the undo design is untouched — this is
  the one addition the "Contract for later edits" asks to be written down
  before it is built, and this is it being written down.
- The text field records through the same coalescing path the inspector
  sliders use: the edit goes into a pending cell, the 100 ms UI timer applies
  it (`crates/kuvatin/src/gui/video/mod.rs:304-310`) and the recorder wraps the
  apply, exactly as `pending_xform` does.

## Interface

### The timeline

- `ClipKind` (`app.slint:21`) gains `title`. The block's background
  (`:1913-1917`) gains a fourth branch, amber so it reads as "not media"
  against blue video, teal image and violet sequence:
  `@linear-gradient(135deg, #f5a742 0%, #c07a1f 100%)`.
- A title has no thumbnail. The block already draws `clip.name` over
  `clip.thumb`, and an empty `image` renders nothing, so the text-derived name
  on the amber gradient is the whole block with no new element.
- `kind_of` (`project_file.rs:330`) takes a `&ClipRecord` instead of a `&str`
  and answers `ClipKind::Title` for a record with a title body before it looks
  at the URI. Both callers (`project_file.rs:216`, `undo.rs:278`) pass the
  record they already have.
- **A dissolve is drawn, not clicked.** A new struct and model:

  ```slint
  export struct TimelineOverlap { track: int, start: float, duration: float }
  in property <[TimelineOverlap]> timeline-overlaps;
  ```

  Each is a 22 px rectangle at the track's lane position, drawn **after** the
  clip blocks (`app.slint:1863-2019`) so it is visible over both of them, with no
  `TouchArea` — in Slint input goes only to touch areas, so it cannot steal a
  drag or a trim from the clips under it. Its fill is a horizontal
  `@linear-gradient(90deg, #ffffff00, #ffffff38 50%, #ffffff00)` with a 1 px
  `#ffffff66` top and bottom rule, which reads as a bow-tie without needing a
  hatch fill Slint does not have. `accessible-role: text`,
  `accessible-label: "Cross-dissolve, 1.2 s, track 2"`.

  It is **not selectable, not movable and not trimmable**. It changes by moving
  or trimming either clip, or by the Dissolve chip, and it disappears when they
  part.

  The model is filled by a pure function in the app crate, so it is unit-tested
  without a window:

  ```rust
  // crates/kuvatin/src/gui/video/timeline.rs
  /// Where clips on the same track overlap, in track and start order.
  pub(super) fn overlaps(rows: &[TimelineClip]) -> Vec<TimelineOverlap>;
  ```

  It is recomputed wherever `set_timeline_duration` is already called — the
  drop, trim and duration handlers (`timeline.rs:154-157`, `:263-266`,
  `:300-303`), the delete path (`:366-369`), `add_to_timeline`
  (`mod.rs:515-519`), `restore_models` (`project_file.rs:272`) and the undo
  and redo apply (`undo.rs:612`) — through one helper so no call site can
  drift.

- The toolbar gains the **Dissolve** chip described above and a **Text** chip,
  `label: "Text"`, `wide: true`, hint "Add a text overlay to the timeline",
  enabled when the engine is up. It appends a 5 s title to the end of track 0,
  matching `add_to_timeline`'s rule that overlays go on the top layer
  (`mod.rs:482-484`), selects it, and records a `StepKind::Add` step.

### The inspector

The inspector is where text is typed. When the selected clip is a title, the
LAYER section (`app.slint:1654-1683`) shows, **above** the existing sliders and
inside the same `if root.inspector-name != ""` block:

```slint
in property <bool> insp-is-title: false;
in-out property <string> insp-text: "";
in-out property <int>    insp-font-size: 48;
in-out property <bool>   insp-font-bold: true;
in-out property <string> insp-text-color: "#ffffff";
in-out property <int>    insp-halign: 1;   // 0 left, 1 centre, 2 right
in-out property <int>    insp-valign: 1;   // 0 top,  1 middle, 2 bottom
callback title-changed();                  // any of the above edited
```

- **The text** is a `TextEdit` from `std-widgets`, three lines tall, so a title
  can have more than one line. Its `edited` fires per keystroke and calls
  `title-changed()`, which the existing 100 ms coalescing timer absorbs — the
  same contract `inspector-changed()` already has, and the reason a typed
  sentence merges into one undo step.
- **Size** is a `NumberDropdown` (the widget exists, `widgets.slint`), 8 to 400,
  labelled "Size"; **Bold** is a checkbox beside it. The two build the Pango
  description `"Sans Bold 48"` or `"Sans 48"`. The family is fixed at `Sans`:
  enumerating installed fonts is a rabbit hole, the record stores the whole
  description string so a later version can widen it without a format change,
  and a fixed family cannot fail to resolve on another machine.
- **Colour** is a row of six 18 px swatches — `#ffffff`, `#000000`, `#ffd34d`,
  `#ff5f5f`, `#4dd2ff` and `Theme.accent` — each a `Rectangle` with a
  `TouchArea` and a ring when chosen. No new widget, no hex field, and no
  colour picker the app does not have.
- **Alignment** is two `SegToggle`s (`widgets.slint:364`), `["Left",
  "Centre", "Right"]` and `["Top", "Middle", "Bottom"]`.

The sliders below are unchanged and all of them work on a title, because a
`TitleClip` is a source and gets a frame positioner like any other: Position X
and Y move the title's frame, Scale scales it, Opacity fades it.
`insp-has-audio` is already `sel_kind == ClipKind::Video` (`timeline.rs:52`), so
a title gets no Volume slider without a change.

`insp-is-still` (`app.slint:217`) is renamed **`insp-free-duration`** and set
for a still *or* a title, so the Duration field (`:1666-1683`) appears for
both. A title has no `max-duration`, so `set_clip_duration`'s cap
(`project.rs:1102-1109`) never binds and the SpinBox's 1…3600 is the only
limit.

## Testing

**Pure, in `kuvatin-video`:**

- `slide_within_layer`: butting up still lands exactly on the edge; a half
  overlap is allowed; a slide that would swallow the previous clip stops 0.2 s
  short; a slide that would be swallowed by the next stops; a clip cannot reach
  past `PP` or `NN`; a clip already overlapping can still be dragged apart; the
  start is never negative; no neighbours means no clamp; a gap too small leaves
  the clip where it was. The four existing tests named `slid`, `neighbour`,
  `confined_to_the_gap` and `already_overlapping` are rewritten, not deleted —
  the CI filter (`.github/workflows/release.yml:252`) matches them by those
  substrings, so the names must survive.
- `trim_bounds`: a right trim stops 0.2 s short of covering the next clip; a
  left trim stops 0.2 s past the previous clip's start; neither reaches the
  clip beyond; no neighbours means no bound.
- `parse_color` / `format_color`: `#ffcc00`, `#ffcc0080`, upper case, a missing
  `#`, seven digits, empty, and a round trip through both.
- `required_version`: no clips → 1; only URI records → 1; one title record →
  2; and `load` still refuses 3.
- `ClipRecord` round trip through TOML with a title body, including a text with
  a newline in it and one with a `"` in it, and a record with no `body` key at
  all deserialising to `None`.

**Engine, real GStreamer, self-contained (generated stills, like
`a_timeline_survives_being_saved_and_reopened`). These join the CI gate at
`release.yml:244-255`; the filter is a list of substrings, so `title_` and
`dissolve_` are added to it.**

- `clip_records_does_not_drop_a_title` — the regression for `:1429`. A
  timeline of one still and one title gives two records, in track and start
  order.
- `title_round_trips_through_a_document` — build, `to_document`, `save`,
  `load`, `apply_document`, `clip_records`: text (multi-line), font, colour,
  both alignments, layout, track and all three times come back identical.
- `title_comes_back_after_being_removed` — `remove_clip` then `restore_clip`
  under the old ID, with the text intact and the clip not duplicated.
- `set_clip_records_writes_a_titles_text` — change the text on the engine, write
  the old record back, read it back; and the same when only the text differs,
  asserting the clip was never moved to a parking layer (its GES layer is the
  same object before and after).
- `a_title_is_never_a_missing_source` — `apply_document` on a document of one
  title returns an empty `missing`, and undo's restore check does not call
  `source_available` for it.
- `dissolve_overlapping_clips_are_still_two_records` — overlap two stills on a
  track, commit, and assert `clip_records()` returns exactly two and
  `to_document().clips.len() == 2`: a GES-inserted transition never reaches a
  file or the diff.
- `dissolve_appears_on_the_layer` — after the same overlap, a new
  `#[cfg(test)] fn layer_clip_count(&self, track: usize) -> usize` reports
  three, and two again after the clips are parted.
- `dissolve_survives_a_reopen` — save and reopen the overlapping pair and the
  count is three again, with no transition in the file.
- `an_empty_layer_holding_a_transition_is_still_pruned` — the
  `layer_is_empty` change: build an overlap on the bottom track, remove both
  clips, and the track goes.
- `a_title_scales_and_fades` — `set_clip_layout` then `clip_layout` round-trips
  posx/posy/scale/alpha on a title clip, proving the positioner is there.
- `a_title_composites_over_a_clip` — render one frame of a white still under a
  title whose text is off to one side and assert a pixel away from the text is
  white, not black. This is the test that pins the transparent background and
  the colour byte order; it is the first thing the plan's Task 1 runs.

No live-media test is needed: generated stills exercise every path here, so
nothing joins the `live-media regressions` step.

**Pure, in the app crate:**

- `overlaps`: two clips overlapping on a track give one wedge with the right
  start and length; clips on different tracks give none; butting up gives none;
  three in a row with two overlaps give two, in order.
- `dissolve_slide`: the slide to make a 1.0 s dissolve; the slide to remove an
  existing one; `None` with nothing before it on the track; `None` when the
  previous clip is shorter than 0.2 s plus the want.
- `kind_of` for a title record, and `rows_after` (`undo.rs:261-292`) giving a
  restored title its amber kind and its text-derived name.
- The `Text` step: it merges within a second and not across one, and describes
  itself as "editing the text of …".

**By hand in the running app:** add a title, type a sentence and undo it as one
step; save, reopen and see it; drag a clip onto its neighbour and watch the
wedge and the dissolve in the preview; remove the dissolve with the chip and
undo that; export and confirm the dissolve is in the file; open a version-2
project in a 2.12.0 build and read the refusal.

## Risks and open questions

- **`GESTitleSource`'s default background.** If it is opaque, a title clip
  hides everything beneath it until `"background"` is set with alpha 0. The fix
  is one line at creation; the risk is that it does not take. **Measurement,
  first task of the plan:** a white still on track 1, a title on track 0,
  render one frame, read a pixel away from the text. The same frame answers the
  second open question below.
- **Colour byte order.** GES takes `color` and `background` as a `guint32` and
  the underlying `textoverlay` documents big-endian ARGB, but the repository
  has never set either. `parse_color` maps `#rrggbbaa` to whatever the
  measurement says; the pure tests pin the mapping once it is known.
- **The fork if transparency does not work.** If a `TitleClip` cannot be made
  transparent against the current restriction caps (width and height only, no
  format, `project.rs:803-806`), the alternatives are to pin a format with
  alpha on the video track — which changes every render and every export, and
  is not acceptable — or to fall back to `TextOverlayClip`, an operation that
  composites over what is beneath it and needs no background at all. The cost
  of that fallback is real and is why it is not the first choice: a
  `TextOverlayClip` has no `ges::VideoSource`, so `clip_natural_size` returns
  `None`, `set_clip_frame`'s positioner lookup (`project.rs:692`) finds
  nothing, and the inspector's Position and Scale would silently do nothing on
  a title while working on everything else. It also renders nothing at all on a
  timeline with no clip beneath it, which is a confusing empty state. If the
  measurement forces it, the record shape, the format version, the undo work
  and the whole interface are unaffected — only `add_title_clip`, `set_title`,
  `title_of` and the inspector's Position and Scale rows change.
- **Auto transitions during the parking dance.** `set_clip_records` moves clips
  through scratch layers one at a time, so transient overlaps appear and GES
  builds and tears down transitions as it goes. The `layer_is_empty` change
  covers a transition left behind on a layer, but whether GES's insertion can
  make a `move_to_layer` or a `set_start` fail where it would otherwise succeed
  is not known. The read-back in `set_clip_records` already reports a clip that
  did not land, so the failure mode is a refused undo rather than a corrupt
  timeline — but if it happens at all it will happen under the undo tests
  first, which is why `dissolve_` tests and the existing `undo_` tests share
  the same CI gate.
- **Old projects gain dissolves.** `slide_within_gap`'s doc comment admits
  projects made before that rule can hold overlapping clips. Opening one now
  puts a crossfade where the overlap is. That is a change to how an existing
  file renders, and it is almost certainly an improvement over one clip
  silently hiding another, but it is a change and it should be in the
  changelog.
- **Snapping magnets across tracks.** `on_timeline_snap_dx`
  (`timeline.rs:167-182`) builds `others` from every row with no track filter,
  so a drag already magnets to edges on other tracks. Pre-existing and out of
  scope, but more visible now that a same-track edge means "no dissolve".
- **The `Project` struct's doc comment is wrong.** `project.rs:731-732` says
  "index 0 = bottom (top layers composite over lower ones)", while
  `add_to_timeline` (`mod.rs:482-483`), `app.slint:1874` and GES itself all
  have layer 0 on top. Nothing depends on the comment; it should be corrected
  when this work touches the file.
- **Font resolution.** The Pango description resolves on the machine that
  renders. Fixing the family at `Sans` keeps that safe; widening it later means
  a project can render differently elsewhere, which is a decision for the spec
  that widens it.
- **A dissolve's length is not settable as a number.** It is the overlap, and
  the only ways to change it are the chip's 1.0 s default and dragging. A
  per-edge length field in the inspector would be a `Move` like everything else
  and needs no new machinery, so it is a cheap addition later; it is left out
  now because the inspector would have to say *which* edge, and that is two
  more controls for a number most users will never set.

## Out of scope

Named because they belong to the sibling specs, and a plan built from this
document must not reach for them:

- **Spec 1 (quick wins):** split at the playhead, frame stepping and the
  shuttle keys, scale above 100 percent, clip speed, and the audio waveform.
- **Spec 2 (track controls):** per-track mute, solo, lock and rename.
- **Reverse playback**, which is in no spec at all: the bindings have no API
  for it.

Also out of scope here:

- Any transition but a cross-dissolve, and any way to choose one. GES offers
  the whole SMPTE set (`auto/enums.rs:653-797`); Kuvatin offers none of it.
- Transitions between clips on *different* tracks, which GES's auto-transition
  does not do and which would mean a compositing decision, not a timeline one.
- Font family choice, font enumeration, per-character styling, outlines,
  shadows and text animation.
- A colour or solid-card clip. The `ClipBody` enum is shaped so that adding one
  is a variant and no format change, but nothing here builds it.
- Rippling. Making a dissolve moves one clip and leaves a gap after it, exactly
  as dragging that clip by hand does today.
- Teaching the media bin about text. A title is not imported media and never
  appears there.
