# Kuvatin — Editor Features 1: Clip Edits Design

> **For agentic workers:** This is the validated design (spec) for the first of
> the three editor-feature specs the undo design named (backlog item
> `ui-editor`). The next step is the `writing-plans` skill. Everything here is
> an edit to a clip that is already on the timeline, plus one view that rides
> along. Spec 2 (track controls) and Spec 3 (media features) are separate
> documents and own the things listed under "Out of scope". Every edit here
> records through the undo history built in
> `docs/superpowers/specs/2026-09-13-undo-design.md`; its "Contract for later
> edits" (lines 287–294) is binding.

## Goal

Five things the timeline cannot do today:

1. **Split at the playhead** cuts the selected clip in two where the playhead
   stands, and undo puts it back as one.
2. **Frame stepping and shuttle keys** give the transport the keys every editor
   has: `,` and `.` for one frame, `J` / `K` / `L` for the speed of playback.
3. **Clip scale above 100 percent** lets a clip be zoomed past the canvas, which
   the engine already allows and the interface alone forbids.
4. **Clip speed** plays a clip at 0.25× to 4×, saved with the project and
   undoable like any other clip edit.
5. **The audio waveform** is drawn inside the clip block, so a cut can be placed
   by looking at the sound rather than by guessing.

The fifth shares nothing with the other four: it records no undo step, adds
nothing to the project file, touches no engine edit path, and is a view rather
than an edit. It is here because it is small and because it wants the same
worker-thread shape as the thumbnails. **If this spec proves too large to plan
as one piece, the waveform is the seam to cut along** — lift it into a spec of
its own and the remaining four still stand together.

## Background — current state

### The undo contract

- Every edit records through `Recorder::before()` and `Recorder::record()`
  (`crates/kuvatin/src/gui/video/undo.rs:359-401`): read the clip records
  before the engine edit, run the edit, read them again, keep the difference.
- `diff` (`undo.rs:78-90`) compares `ClipRecord`s **exactly**, by
  `PartialEq`, keyed by `ClipId`. State that is not in a `ClipRecord` or in the
  track rows is invisible to the history, and a step made only of such state is
  dropped as empty (`TimelineStep::is_empty`, `undo.rs:192-194`).
- `plan` (`undo.rs:222-237`) already turns a diff into removals, writes and
  restores. A clip whose record changed becomes a write; a clip present on only
  one side becomes a restore or a removal. **Split needs no new shape here**:
  one record changes and one appears.
- `StepKind` (`undo.rs:24-33`) only decides the hint text and whether
  consecutive steps merge (`StepKind::merges`, `undo.rs:35-42`). Adding a kind
  is free; adding state outside `ClipRecord` is not.
- `rows_after` and `place` (`undo.rs:261-297`) rebuild the timeline rows from
  the applied records. `place` writes exactly four fields: track, start,
  duration, in-point. **Any new field on `TimelineClip` that depends on a
  record must be written there too**, or it goes stale after every undo.

### The engine

- `ClipRecord` (`crates/kuvatin-video/src/document.rs:61-77`) is the saved and
  compared shape of a clip: `uri`, `name`, `track`, `start`, `inpoint`,
  `duration`, `layout`, `sequence`.
- `Project` keeps its clips in a `HashMap<String, ges::Clip>` keyed by the GES
  name, which is the `ClipId`. Anything that creates a clip must insert it
  there (`add_clip_uri`, `project.rs:931-959`; `restore_clip`,
  `project.rs:1225-1246`).
- `trim_clip` (`project.rs:1059-1100`) reads the source's `max-duration`
  (`project.rs:1069-1071`) and hands it to the pure `trim_right_math`
  (`project.rs:279-285`); `trim_left_math` (`project.rs:269-274`) moves start
  and in-point by the same delta. Both assume one second of timeline is one
  second of source. `MIN_TRIM_NS` is 0.2 s (`project.rs:261`).
- `set_clip_records` (`project.rs:1129-1216`) writes records back exactly,
  parking clips on spare layers so GES never sees an overlap, and reads every
  clip back with `clip_placed_as` (`project.rs:337-342`), which compares start,
  in-point, duration and track.
- `clip_layout` (`project.rs:1538-1571`) deliberately has **no upper clamp on
  scale**; the comment at `project.rs:1554-1562` records that a 1.0 ceiling
  there silently snapped zoomed clips back whenever the inspector refreshed.
  `set_clip_layout` (`project.rs:1575-1596`) multiplies the fit size by the
  scale with no cap either.
- Transport: `play`, `pause`, `seek` (KEY_UNIT), `seek_accurate` (ACCURATE),
  `position`, `duration` (`project.rs:1655-1756`). Both seeks go through
  `seek_simple`, which always plays at rate 1.0. **There is no rate anywhere in
  the engine.**
- The preview's video track has restriction caps for width and height only
  (`project.rs:803-811`); no framerate is pinned, so there is no constant the
  app can call "one frame".

### The project file

`FORMAT_VERSION` is 1 (`document.rs:20`), every saved file is built through
`ProjectFile::new`, which stamps the current version unconditionally
(`document.rs:100`), and `load` refuses anything higher (`document.rs:130`).
Bumping the version therefore makes every new file unopenable by 2.12 and
earlier, including files that use none of the new feature. Optional fields carrying `#[serde(default)]` are the compatible route,
and there is precedent in this very struct: `name` at `document.rs:65` and
`sequence` at `document.rs:75`.

### The interface

- `TimelineClip` (`crates/kuvatin/ui/app.slint:25-35`) has one image field,
  `thumb`, filled asynchronously by `spawn_thumbnails`
  (`crates/kuvatin/src/gui/video/project_file.rs:284-327`): a worker decodes,
  `slint::invoke_from_event_loop` drops the image into the row whose `id`
  matches, and into the media-bin row whose `name` matches.
- The clip block is drawn at `app.slint:1863-1947`: a 22 px rectangle, a
  gradient by kind, the thumbnail over it with `image-fit: cover`, a left
  scrim, the name, two white edge grips, and a delete square on the selected
  clip.
- Keys: `clip-keys` (`app.slint:373-412`) walks and edits the selected clip;
  `video-keys` (`app.slint:413-448`) handles zoom, Ctrl+S / Ctrl+O, Space,
  Delete, then falls through to `clip-keys`. `J`, `K`, `L`, `,`, `.` and plain
  `S` are all unbound. The comment at `app.slint:370-372` records why
  `clip-keys` is a separate function: a long `if … return` chain overflows the
  Slint compiler's stack.
- The scale cap lives in exactly two interface places the brief named —
  `app.slint:1661` (`InspSlider { label: "Scale"; min: 10; max: 100; … }`) and
  `crates/kuvatin/src/gui/video/timeline.rs:66`
  (`ui.set_insp_scale(((l.scale * 100.0) as f32).clamp(10.0, 100.0))`) — **and
  in a third that the brief did not**: the preview bounding box's resize drag
  clamps to `Math.clamp(…, 10, 100)` at `app.slint:1540`. The box's own
  geometry (`app.slint:1476-1521`) is derived arithmetic with no clamp; only the
  drag handler clamps.
- The timeline toolbar (`app.slint:1698-1738`) holds the Undo and Redo chips
  and the three zoom chips, all `TimelineChip`s with an `enabled` flag and a
  `hint` shown as a tooltip.

### What ships in the installer

`crates/kuvatin/wix/bundle-gstreamer.ps1:64-90` is the plugin allow-list.
`gstvideorate` (line 72) and `gstsoundtouch` (line 69, which provides `pitch`)
are **already bundled**, and both appear in `crates/kuvatin/wix/gstreamer.wxs`
(lines 360 and 330). Clip speed therefore needs no new runtime files and no
change to the installer.

## Guiding decisions

1. **Nothing leaves the undo contract.** Split adds a `StepKind` for its hint
   and nothing else. Speed adds one field to `ClipRecord`, which is exactly
   what the contract anticipated. The waveform records no step at all, because
   it changes nothing about the work.
2. **The project file stays at version 1.** Speed arrives as an optional
   `rate` field with a serde default of 1.0, skipped when it is 1.0, so a
   project that never uses speed is byte-identical to one written today, and
   every existing file still loads. An older build opening a file that does use
   speed sees the clip at its saved place and length but plays it at 1× — wrong
   playback, right geometry. That is the accepted price of not bumping.
3. **Reverse playback is in no spec, including this one.** There is no reverse,
   no negative rate and no rate at all in the engine today, and the bindings
   offer nothing purpose-built: reverse means either negative-rate seeking,
   which needs every element in the chain to handle it and which GES's
   composition and the app's appsink have never been asked to do, or a
   reversing element inside the source bin, which the engine does not build.
   Both are a piece of work that has to be priced on its own before it can be
   written into a spec that must ship. Because of that, `J` is **not** reverse
   shuttle here; see "Frame stepping and shuttle keys".
4. **Speed is chosen from a fixed list, not dragged.** Every rate change
   rebuilds GES effects and re-times the clip. A slider firing `changed` on
   every pixel would do that dozens of times a second, and the transform timer's
   coalescing trick does not apply, because a rate change also moves the clip's
   end. A discrete control makes each change one step with no merging.
5. **Scale keeps one source of truth.** The new ceiling is a Rust constant
   pushed into the window as a property, so the slider, the drag clamp and the
   inspector read-back cannot drift apart again — which is how the three places
   above came to disagree with the engine in the first place.
6. **The waveform is a view.** It is derived from the source, cached by URI,
   never saved, never recorded, and always safe to throw away and decode again.

## Architecture

### 1. Split at the playhead

**Engine.** One new method on `Project`:

```rust
/// Cut the clip in two at timeline position `at`. The clip keeps its start
/// and in-point and ends at `at`; the returned clip begins there with the
/// in-point that follows. Refused unless both halves would be at least
/// `MIN_TRIM_NS` long.
pub fn split_clip(&mut self, id: &ClipId, at: Duration) -> Result<(ClipId, ClipGeom, ClipGeom)>;
```

- Refuses while rendering, as every other edit does.
- Bounds first, in nanoseconds: `at` must satisfy
  `start + MIN_TRIM_NS <= at <= start + duration - MIN_TRIM_NS`. Outside that,
  `bail!` with a message naming the minimum.
- `clip.split_full(at_ns)` (`gstreamer-editing-services` 0.23,
  `auto/clip.rs:402`; gated on `v1_18`, which the crate's `v1_20` + `v1_24`
  features in `crates/kuvatin-video/Cargo.toml:20` turn on). `split_full`
  rather than `split` (`auto/clip.rs:389`) because it returns a `glib::Error`
  with a reason instead of a bare boolean failure. `Ok(None)` means GES chose
  not to split and is treated as an error.
- The new clip's GES name is read the way `add_clip_uri` reads it
  (`project.rs:952-956`) — a nameless clip is an error, not a silent collision
  on the `""` key — and inserted into `self.clips`.
- **The engine then writes the parent's layout onto the new clip explicitly**
  with `set_clip_layout`. Whether `ges_clip_split` copies child properties is
  not something this design wants to depend on; writing them makes the outcome
  the same either way, and the engine test below pins it.
- `commit()`, `dirty.set(true)`, return both halves' geometry.

**Undo.** `StepKind::Split`, describing itself as `"splitting {name}"`, not in
`merges()`. The diff after a split is: the left clip's `duration` changed, and a
clip appeared with a new ID. `plan` turns undo into one write (the left clip's
old record) and one removal (the right half), and redo into one write and one
`restore_clip`. **Redo does not split again** — it rebuilds the right half from
its record, which is a `UriClip` on the same source with a different in-point,
exactly what `restore_clip` already makes. No new engine operation for undo.

**Interface.** A new callback `timeline-split()`, reached two ways:

- a `TimelineChip { label: "Split"; wide: true; hint: … }` in the timeline
  toolbar, next to Undo and Redo (`app.slint:1705-1720`), enabled on
  `root.timeline-selected >= 0 && !root.modal-open() && !root.video-engine-down`;
- the plain `S` key in `video-keys`. `Ctrl+S` is taken by "save project"
  (`app.slint:436-439`); unmodified `S` is free.

The handler (a new block in `crates/kuvatin/src/gui/video/timeline.rs`, beside
the trim handler) reads the selected row and `ui.get_playhead()`, calls
`Recorder::before`, calls `split_clip`, updates the selected row's duration,
**pushes a new row for the right half** copying the left row's `name`, `kind`,
`thumb` and `wave` and taking its times from the returned geometry, sets
`timeline-duration`, and only then calls `Recorder::record(…, StepKind::Split,
Some(left_id), before)` — the recorder's doc (`undo.rs:365-367`) requires an
added clip's row to exist before the step is recorded, because the step keeps
the row.

The selection stays on the left half, whose row index does not move. The
playhead is then exactly at the right half's start, so pressing `S` again is
refused by the minimum-length rule rather than producing a zero-length clip.

**Not the whole track.** Only the selected clip is split, never every clip the
playhead crosses. The hint names one clip, the selection is how every other clip
edit in this app is aimed, and keyboard users can already reach any clip with
the arrow keys.

### 2. Frame stepping and shuttle keys

Neither is an edit. Nothing is recorded, nothing is marked dirty, and the
history is untouched.

**How long is a frame.** The preview pins no framerate
(`project.rs:803-811`), so the engine learns it instead: `emit_sample`
(`project.rs:165-171`) stashes the buffer's duration in an
`Arc<AtomicU64>` on `Project`, and

```rust
/// How long one composited preview frame lasts, from the last frame that
/// arrived; 1/25 s until one has.
pub fn frame_secs(&self) -> f64;
```

reports it. A buffer with no duration leaves the last good value in place. This
is a measurement of what the preview is actually producing, which is what the
user sees stepping past.

**Stepping.** `,` and `.` pause if playing, then set `pending_seek` to
`(playhead ∓ frame_secs, true)` clamped to `[0, duration]`. That is the existing
coalesced path (`crates/kuvatin/src/gui/video/mod.rs:185-195` and the UI tick at
`:316-323`), which already lands ACCURATE and already moves the playhead and the
scrubber. No engine work beyond `frame_secs`, and both directions behave
identically — which a GStreamer `Step` event could not offer, since it only goes
forward.

**Shuttle.** One new engine method:

```rust
/// Play forward at `rate` (1.0 = normal). Refuses a rate that is not
/// finite and greater than zero: this engine has no reverse (see the spec's
/// "Out of scope"), and a negative rate here would fail deep inside GES
/// with nothing to show the user.
pub fn set_rate(&self, rate: f64) -> Result<()>;
```

It issues a full `Element::seek(rate, FLUSH | ACCURATE, Set, position, End,
ClockTime::NONE)` — `seek_simple` cannot carry a rate — and is inert while
rendering, like `play` and `pause`.

The ladder is `1× → 2× → 4× → 8×`:

| Key | Playing | Paused |
| --- | --- | --- |
| `L` | next rate up the ladder, capped at 8× | play at 1× |
| `J` | next rate down; at 1× it pauses | step one frame back (as `,`) |
| `K` | pause, rate back to 1× | play at 1× |

`J` is not reverse play, and this is the one place where Kuvatin's keys do not
mean what they mean in other editors. Decision 3 says why. The rate is shown so
nobody has to infer it: a new `in property <float> shuttle-rate` on the window,
rendered beside the play button as `2×`, `4×`, `8×` and hidden at 1×.

**Sound while shuttling.** Above 1× the preview's audio pitches up. The app
mutes it: `set_master_volume(0.0)` when the rate leaves 1.0 and the transport
volume restored when it returns. The transport slider's own value is untouched,
so the user's setting survives.

**Where the keys live.** A new `transport-keys(event)` function in
`app.slint`, called from `video-keys` before its fall-through to `clip-keys`,
for the same reason `clip-keys` exists at all (`app.slint:370-372`): six more
branches on the end of `video-keys` risk the Slint compiler's stack. It holds
`,`, `.`, `J`, `K` and `L`, and refuses everything while `root.exporting`, as
`video-keys` already does at `app.slint:421`.

### 3. Clip scale above 100 percent

The engine needs no change; `clip_layout`'s comment at `project.rs:1554-1562`
says so in as many words. Three interface places cap it and one doc comment
lies about it:

| Place | Now | Becomes |
| --- | --- | --- |
| `crates/kuvatin/src/gui/video/timeline.rs:66` | `.clamp(10.0, 100.0)` | `.clamp(MIN_SCALE_PCT, MAX_SCALE_PCT)` |
| `app.slint:1661` (Scale slider) | `min: 10; max: 100` | `min: root.insp-scale-min; max: root.insp-scale-max` |
| `app.slint:1540` (preview box resize drag) | `Math.clamp(…, 10, 100)` | `Math.clamp(…, root.insp-scale-min, root.insp-scale-max)` |
| `project.rs:649-651` (`Layout` doc) | "`scale` is 0..1" | says 1.0 is fit-to-canvas and larger zooms in |

`MIN_SCALE_PCT = 10.0` and `MAX_SCALE_PCT = 400.0` are `pub(super)` constants in
`crates/kuvatin/src/gui/video/mod.rs`, written into the window once at start-up
through two new `in property <float>`s. 400 % because it is the ceiling other
editors use and because the position sliders already run to ±canvas, so a clip
zoomed that far can still be placed anywhere on it.

The slider's arrow keys step a hundredth of its range
(`crates/kuvatin/ui/widgets.slint:584-600`), so widening the range from 90 to
390 points makes one arrow press about 3.9 % instead of 0.9 %. Accepted: the
preview box's corner drag is the fine control, and the slider is the coarse one.

No undo work: `scale` is already inside `LayoutRecord`, so every step kind that
carries a layout carries it. No project-file work: the field is already an
`f64` and `apply_document` already writes the layout back
(`project.rs:1510`).

### 4. Clip speed

**The record.** One new field, and two tiny free functions beside it in
`document.rs`:

```rust
pub struct ClipRecord {
    // …
    /// Playback rate: 1.0 is normal, 2.0 twice as fast. Optional and
    /// defaulted so a project that never changes a clip's speed is written
    /// exactly as before, and every file from 2.12 and earlier still loads.
    #[serde(default = "unit_rate", skip_serializing_if = "is_unit_rate")]
    pub rate: f64,
}

fn unit_rate() -> f64 { 1.0 }
fn is_unit_rate(r: &f64) -> bool { *r == 1.0 }
```

`#[serde(default)]` alone would be wrong: it yields `0.0`, not `1.0`. The field
is compared exactly, like the `f64` times beside it, so `diff` sees a speed
change without any change to `undo.rs`.

**How the rate is made.** GES has no speed property; a rate is a pair of *time
effects* on the clip:

- video: `Effect::new("videorate rate=R")`;
- audio: `Effect::new("pitch rate=R")` — `pitch` comes from
  `gstsoundtouch`, already in the installer's allow-list
  (`bundle-gstreamer.ps1:69`).

Each is added with `Clip::add_top_effect(&effect, -1)` and each has its rate
child property registered with `BaseEffect::register_time_property`, without
which GES does not treat the effect as time-changing and the clip's timing
maths stay wrong. `BaseEffect::is_time_effect()` is asserted straight after, so
a runtime that disagrees fails loudly at the first speed change rather than
producing a clip whose length no longer matches its sound.

**Rate 1.0 means no effect.** Setting a clip back to 1× removes both effects
entirely, so an untouched project carries no effects at all and behaves exactly
as it does today.

**Engine API.**

```rust
/// The clip's playback rate: the rate of its time effects, or 1.0 when it
/// has none.
pub fn clip_rate(&self, id: &ClipId) -> f64;

/// Set the clip's playback rate, clamped to [RATE_MIN, RATE_MAX]. The clip
/// keeps its start and in-point; its duration becomes the same span of
/// source at the new rate, clamped to the trim minimum, to what the source
/// can still supply, and to the gap before the next clip on its track.
pub fn set_clip_rate(&mut self, id: &ClipId, rate: f64) -> Option<ClipGeom>;
```

with `RATE_MIN = 0.25` and `RATE_MAX = 4.0`.

`clip_rate` reads `Clip::top_effects()`, keeps the ones that downcast to
`BaseEffect` and answer `is_time_effect()`, and reads the `rate` child property
off the first. That is also how `set_clip_rate` finds the effects to remove:
no bookkeeping map, no name to keep in sync, nothing to lose across an undo.

**Order matters, and in the same way it already does for trims.**
`ges_clip_add_top_effect` fails when the effect would push the clip's
`duration-limit` below its current duration, and setting a duration that the
source cannot supply at the current rate fails too. So, exactly as
`set_clip_times` (`project.rs:344-356`) applies the shrinking property first:

- **speeding up** (more source per timeline second): write the shorter duration
  first, then swap the effects in;
- **slowing down**: swap the effects in first, then write the longer duration.

**The trim maths need the rate.** With a time effect, one second of timeline is
`rate` seconds of source, and both pure helpers currently assume 1:1:

- `trim_right_math(inpoint, dur, delta, max_ns)` (`project.rs:279-285`) caps
  with `nd.min(m - inpoint)`. It gains a `rate: f64` and caps with
  `nd.min(((m - inpoint) as f64 / rate) as i128)` — at 2× a 10-second source
  starting at its head can only fill 5 seconds of timeline.
- `trim_left_math(start, inpoint, dur, delta)` (`project.rs:269-274`) moves
  start and in-point by the same `d`. It gains a `rate: f64`: the in-point
  moves by `(d as f64 * rate) as i128`, and the "never before the source
  origin" guard becomes `d.max(-((inpoint as f64 / rate) as i128))`.

`trim_clip` (`project.rs:1059-1100`) reads the clip's rate once with
`clip_rate` and passes it to both. Both helpers stay pure and stay unit-tested,
which is why this design computes the maths rather than reading GES's
`duration-limit` property: `duration-limit` would be a second, GES-side answer
to a question the repo already answers in a tested pure function, and it cannot
be exercised without an engine. It is used in the engine test below as the
cross-check instead.

**The rest of the record paths.** `clip_records` (`project.rs:1425-1469`)
fills `rate` from `clip_rate`. `set_clip_records` (`project.rs:1129-1216`)
applies the rate on the same shrink-first rule, before or after the times as
the direction demands, and `clip_placed_as` (`project.rs:337-342`) gains a rate
comparison so a rate GES refused is reported as a failed write like any other.
`restore_clip` (`project.rs:1225-1246`) applies the rate after the layout.
`apply_document` (`project.rs:1502-1512`) applies it after `set_clip_layout`.

**Undo.** A new `StepKind::Speed`, describing itself as
`"changing the speed of {name}"`, not in `merges()`. It is a hint, not state:
the state is the `rate` field in `ClipRecord`, which is what the contract asks
for. Because the duration changes with the rate, undoing a speed change
restores both from the same record in one write.

**Interface.** A "Speed" row in the inspector, between Volume
(`app.slint:1663`) and the stills' Duration block (`app.slint:1666-1680`),
shown only when `insp-has-rate` — true
for `ClipKind::Video` and `ClipKind::Sequence`, false for stills, which have no
source time to stretch. It is a `ComboBox` of `0.25×, 0.5×, 1×, 1.5×, 2×, 4×`
and a new callback `inspector-speed-changed(float)`, handled next to
`on_inspector_duration_changed` in `timeline.rs`: `Recorder::before`,
`set_clip_rate`, write the row's `duration` and `rate`, update
`timeline-duration`, `Recorder::record(…, StepKind::Speed, Some(id), before)`.

### 5. The audio waveform

The odd one out, as said above.

**Decoding.** A new function in `kuvatin-video`, beside `thumbnail_uri`
(`project.rs:182`), which it copies in shape — its own throwaway pipeline, safe
off the UI thread, `None` on any failure:

```rust
/// Peak-per-column audio waveform for a source, rasterised RGBA, plus the
/// source's duration in seconds. None when the source has no audio.
pub fn waveform_uri(uri: &str, width: u32, height: u32) -> Option<(Frame, f64)>;
```

`uridecodebin ! audioconvert ! audioresample ! audio/x-raw,format=S16LE,
channels=1,rate=8000 ! appsink`, pulled to the end, accumulating the peak
magnitude per output column, then drawn as a symmetric bar per column into an
RGBA `Frame`. 8 kHz mono is far more than a 22 px block can show and keeps a
long source cheap. The duration comes back with it because the image spans the
**whole source**, and the clip only shows a window into it.

**Cache and worker.** `spawn_waveforms(ui_weak, records)` in
`project_file.rs`, modelled line for line on `spawn_thumbnails`
(`project_file.rs:284-327`): one named worker thread, one
`slint::invoke_from_event_loop` per finished source, rows matched by clip id.
`VideoState` gains

```rust
/// Waveform per source URI: the rasterised image and the seconds it spans.
/// A view, never saved; dropped and decoded again whenever it is missing.
pub(super) waves: Rc<RefCell<HashMap<String, (slint::Image, f32)>>>,
```

so a second clip of the same file, a split half, and a clip that comes back
from an undo all get their waveform without decoding. It is started from the
three places `spawn_thumbnails` is started from: opening a project
(`project_file.rs:279`), `add_to_timeline` and `add_sequence_to_timeline`.

**The row.** `TimelineClip` (`app.slint:25-35`) gains three fields:

```slint
wave: image,        // the whole source's waveform, or empty
wave-secs: float,   // how many seconds of source that image spans
rate: float,        // the clip's playback rate; 1 unless speed changed it
```

`rate` is here because the waveform window depends on it — a clip at 2× shows
twice the source in the same block — and because the clip block can then say
so. **`place` (`undo.rs:291-297`) must write `rate` from the record**, or a
sped-up clip's waveform goes wrong after every undo; `wave` and `wave-secs`
must not be written there, because they belong to the source, not the record,
and the row already carries them.

**Drawing.** Inside `vis` (`app.slint:1888-1947`), between the thumbnail
(`:1922`) and the scrim (`:1923`), in the bottom 40 % of the block:

```slint
if clip.wave.width > 0 : Image {
    source: clip.wave;
    image-fit: fill;
    y: parent.height * 0.6; height: parent.height * 0.4; width: 100%;
    source-clip-x: clip.wave-secs > 0
        ? Math.round(clip.inpoint / clip.wave-secs * clip.wave.width) : 0;
    source-clip-width: clip.wave-secs > 0
        ? Math.max(1, Math.round(clip.duration * clip.rate / clip.wave-secs * clip.wave.width))
        : clip.wave.width;
}
```

Slint's `Image` is `ClippedImage` and carries `source-clip-x` /
`source-clip-width` (verified in `i-slint-compiler-1.16.1/builtins.slint:59-63`),
so trimming and speed move the window with no work in Rust and no re-decode. No
`rate` field on the row and this expression would be a lie for every sped-up
clip.

## Failure handling

| What happens | What the user sees | State afterwards |
| --- | --- | --- |
| Split with nothing selected | The Split chip is disabled; `S` does nothing | Unchanged |
| Split with the playhead outside the clip, or within 0.2 s of an edge | Error naming the 0.2 s minimum | Unchanged, no step recorded |
| `split_full` fails or returns nothing | The `glib::Error` text, through `show_error` | Unchanged, no step recorded |
| Shuttle or step with no engine | Nothing; the keys are inert, as Space already is | Unchanged |
| A rate GES refuses (`add_top_effect` fails) | "Could not change the speed of \<name\>" | Clip left at its old rate and duration, no step recorded |
| `pitch` or `videorate` missing from the runtime | The same message, once, the first time speed is used | As above |
| A speed change that would overlap the next clip | The clip takes the gap and stops there | Recorded as the step it actually was |
| A waveform that cannot be decoded, or a silent source | No waveform on that clip | Nothing else changes; never an error dialog |

A waveform failure is deliberately silent. It is a view: a modal about a missing
picture of the sound would be worse than the missing picture.

## Testing

**Pure, no engine (`kuvatin-video`):**

- `trim_right_math` with a rate: at 1× the existing cases still hold; at 2× a
  10 s source from its head caps the duration at 5 s; at 0.5× at 20 s; the
  minimum still wins where they conflict, as `project.rs:276-278` says.
- `trim_left_math` with a rate: the in-point moves by `delta × rate`; the
  never-before-the-source-origin guard stops at `inpoint / rate`; a trim that
  would go negative still comes back non-negative, which is the bug
  `project.rs:263-268` records.
- `ClipRecord` round-trips through TOML with no `rate` key (reads 1.0), with
  `rate = 2.0`, and a record at 1.0 writes no `rate` key at all.
- A `ProjectFile` written by this build still carries `version = 1`, and a file
  written before this change still loads.

**Pure, no engine (`kuvatin`):**

- `diff` sees a rate-only change as one changed clip, and `plan` turns it into
  one write.
- A split's diff — one record changed, one appeared — turns into one write plus
  one restore on undo, and one write plus one removal on redo.
- `StepKind::Split` and `StepKind::Speed` describe themselves as "splitting
  intro.mp4" and "changing the speed of intro.mp4", and neither merges with a
  second of its kind.
- `place` writes `rate` from the record, and leaves `wave` and `wave-secs`
  alone.
- The shuttle ladder as a pure function: `L` from paused starts at 1×, climbs
  1→2→4→8 and stops; `J` descends and pauses at 1×; `K` pauses from any rate;
  `J` and `L` while paused ask for one frame back and forward.

**Engine, real GStreamer, single-threaded, alongside
`a_timeline_survives_being_saved_and_reopened`:**

- Split a generated still's clip in the middle: two clips, touching exactly, the
  right one's in-point equal to the left one's in-point plus its duration, both
  with the parent's layout, and the new clip present in `clip_records`.
- Split refused within 0.2 s of either edge, and outside the clip; nothing
  changes.
- Split, then undo: one clip again, exactly as it was, with the left clip's
  original ID; then redo: two again, the right one under the ID the step holds.
- Scale a clip to 250 % and read it back: `clip_layout` returns 2.5, not 1.0 —
  the regression `project.rs:1554-1562` describes, pinned by a test at last.
  Then through `to_document` / `apply_document` and back.
- `set_clip_rate(2.0)`: the clip's duration halves, `clip_rate` reads 2.0, the
  clip has two top effects and both answer `is_time_effect()`. Back to 1.0: the
  duration returns, and `top_effects()` is empty.
- With the rate at 2.0, the clip's GES `duration-limit` agrees with what
  `trim_right_math` allows — the cross-check that keeps the pure maths honest
  about what GES will actually accept.
- A speed change and its undo through `set_clip_records`: rate and duration both
  come back, in both directions, which is what pins the shrink-first ordering.
- Speeding a clip up against a neighbour 1 s away: the duration stops at the gap
  and GES accepts it.
- `waveform_uri` on the live-media fixture returns an image and a duration that
  matches the source; on a generated still it returns `None`. This one gates in
  the live-media step, like the two undo tests that need real media, because the
  self-contained step runs before the fixtures exist.

**By hand in the running app:** split at the playhead with `S` and undo it;
hold `.` through a cut; `L` up to 8× and `K` back; a clip at 300 % dragged by
its corner in the preview; a clip set to 0.5× then trimmed from both edges;
a waveform appearing on a long clip while the timeline stays responsive.

## Risks and open questions

- **Whether `ges_clip_split` copies child properties** is not settled from the
  bindings, which carry no documentation (`auto/clip.rs:388-413`). The design
  sidesteps it by writing the layout onto the new clip itself, and the engine
  test pins the result either way. If GES turns out to copy them, the extra
  write is harmless.
- **The exact child-property name to register as a time property** is not
  settled from the code: `register_time_property` takes a child-property name
  (`auto/base_effect.rs:45-52`), and whether GES wants `"rate"` or the
  qualified `"GstVideoRate::rate"` — and whether it already knows `videorate`
  and `pitch` without being told — has to be found by running it. The design
  asserts `is_time_effect()` immediately afterwards precisely so that this
  question is answered loudly at the first speed change rather than quietly in
  a wrong-length export. **This is the first thing to try when planning the
  speed work.**
- **`pitch` and rate direction.** `pitch` has both `rate` and `tempo`; `rate`
  changes speed and pitch together, `tempo` speed alone. This design says
  `rate`, to match what `videorate` does to the picture, but a build that
  prefers chipmunk-free audio would use `tempo`. Worth one experiment before it
  is written into the plan.
- **Audio while shuttling** is muted rather than time-stretched. A `scaletempo`
  in the preview's audio path would be better, but the preview is GES's own
  `playsink` and inserting a filter into it is a piece of work of its own.
- **The frame length is a measurement, not a contract.** A source whose buffers
  carry no duration leaves `frame_secs` at its fallback of 1/25 s, and stepping
  is then approximate. Pinning a framerate into the preview's restriction caps
  would make it exact, but it would also change what the preview composites for
  every existing project, so it is not done here.
- **A split clip's halves share one thumbnail**, which is the parent's frame,
  so the right half shows a picture from before its own start. Accepted: the
  thumbnail is a label, not a frame-accurate preview, and decoding a second one
  per split is not worth the wait. The waveform, by contrast, is correct
  immediately, because it is windowed by in-point.
- **Speed and export.** The render path builds its own encoding profile with a
  pinned framerate (`project.rs:596-612`); a time effect inside the composition
  should be invisible to it, but no test covers exporting a sped-up clip today.
  Adding one would need the live-media fixture and a longer pipeline run; it is
  listed here as a gap rather than hidden.
- **Speed and the waveform** stay consistent only because the row carries
  `rate`. Anything later that writes a `TimelineClip` and forgets that field
  will look right until someone speeds a clip up.
- **Five features, one spec.** The four edits share the undo path, the record
  shape and the trim maths, and changing any of them separately would mean
  touching `ClipRecord` and `trim_*_math` more than once. The waveform shares
  none of that. Cutting it out is the only clean cut this document has.

## Out of scope

- **Per-track mute, solo, lock and rename.** Spec 2, which owns the question of
  what identifies a track when tracks can be reordered and pruned.
- **Cross-dissolves and text overlays.** Spec 3, which owns everything about
  clips that are not `UriClip`s — transitions, title clips, and whatever
  `ClipRecord` needs to describe them.
- **Reverse playback.** Deliberately in no spec. The bindings offer no reverse
  and no negative rate; doing it means either negative-rate seeking through
  GES's composition and the app's appsink, or a reversing element inside the
  source bin, and this engine builds neither. It needs pricing on its own
  before it is written into a spec that has to ship alone. Decision 3 is why
  `J` shuttles down instead of playing backwards.
- Splitting every clip under the playhead, or splitting a selection of clips.
- A rate typed in as a free number, and rates outside 0.25×–4×.
- Speed on still images, which have no source time.
- Zoom on the waveform's vertical scale, a waveform in the media bin, and a
  waveform for the whole timeline under the ruler.
- Bumping `FORMAT_VERSION`, and anything that would need it.
