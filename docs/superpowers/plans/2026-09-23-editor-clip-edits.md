# Editor Clip Edits Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Five things the timeline cannot do today: split the selected clip at the playhead, step frames and shuttle with J / K / L, zoom a clip past 100 %, play a clip at 0.25× to 4×, and draw a video's sound inside its clip block.

**Architecture:** Every edit records through the existing undo `Recorder`, and every piece of state an edit changes lives in `ClipRecord` or the timeline rows (the undo design's "Contract for later edits"). Split is `ges::Clip::split_full` plus a `StepKind`. Speed is a pair of GES *time effects* (`videorate`, `pitch`) and one optional `rate` field in `ClipRecord`, so the project file stays at version 1. Frame stepping and the shuttle are transport, not edits. The waveform is a view: decoded per source on a worker, cached by URI, never saved and never recorded.

**Tech Stack:** Rust, Slint 1.16, GStreamer 1.26 with GES through `gstreamer-editing-services` 0.23, TOML project files.

**Spec:** `docs/superpowers/specs/2026-09-16-editor-clip-edits-design.md`. Read it before Task 1. Its line numbers were taken before the in-app update merged; the table under "Where the spec's line numbers are now" gives the current ones, and this plan cites only current ones.

**Verified before review:** every code block below was applied, in task order, to an export of `dd515f0` outside the repository and run on GStreamer 1.26.11. `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings` were clean after each phase, and every test the plan adds passed, including the live-media ones against the fixture. The expected counts in each phase's last task are the ones that run produced.

---

## Three phases, each one shippable

| Phase | Tasks | What ships |
| --- | --- | --- |
| 1 | 1 to 8 | Scale to 400 %, split at the playhead, frame stepping, J / K / L shuttle |
| 2 | 9 to 17 | Clip speed, saved and undoable |
| 3 | 18 to 21 | The audio waveform |

Each phase ends with its own changelog and README task and a full gate run. The work can stop after Task 8 or Task 17 and the branch is still a release. Phase 3 depends on Phase 2 for one thing only: the `rate` field on the timeline row, which the waveform window reads.

## Before you start

- **Work in a worktree of its own on branch `clip-edits`** (superpowers:using-git-worktrees). Do not commit to `master`, and do not touch the main checkout at `C:\Työt\Koodaus\Kuvatin`. Run every command from the worktree root.
- **GStreamer must be on PATH for every cargo command** (Git Bash):
  `export PATH="/c/Program Files/gstreamer/1.0/msvc_x86_64/bin:$PATH"`.
- **The video tests run one at a time.** Every command that runs `kuvatin-video` tests passes `-- --test-threads=1`. Concurrent GES pipelines deadlock, and the failures that produces are not real.
- **Every new video test that gates behaviour goes into `.github/workflows/release.yml`**, into the step `Test (video engine, self-contained — gates the release)` or, for one that needs real media, `Test (live-media regressions — gates the release)`. The task that adds the test says which and shows the edit. A test left out of those lists never runs in CI.
- **`kuvatin` is a binary crate.** A `pub(super)` item with no production caller fails `clippy -D warnings`. The tasks below land each helper together with its caller, so no task needs a temporary `#[allow(dead_code)]`; if you split a task, add one with a comment naming the caller to come, and remove it when that caller lands.
- **Never run filesystem-wide searches** (`find /`). Crate sources live under `~/.cargo/registry/src/index.crates.io-*/`.
- **Heredocs in the Bash tool lose one level of backslash.** Make every file edit with the Edit or Write tool. `app.slint` holds `\u{…}` escapes and `\n` in strings that a heredoc would break.
- **Check the disk before build-heavy runs** (`df -h /c`). `target/` has filled the disk before; prune `target/debug/incremental` if it is over a day old.
- **Line numbers** in this plan are at `master` `dd515f0`, before any task. Earlier tasks shift them, so find each anchor by the function or text quoted beside it.
- **Every commit ends with the trailer.** Use two `-m` flags, so the trailer is its own paragraph:
  `git commit -m "<title>" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"`.
  Titles are plain sentences, as `git log --oneline -15` shows.
- **Gates before every commit:**
  ```bash
  cargo fmt --all
  cargo fmt --all --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test -p kuvatin
  ```
  plus, for a task that touches `kuvatin-video`, the video tests that task names, with `-- --test-threads=1`.
- **The live-media fixture** (Tasks 9, 14 and 18) is built the way the pipeline builds it, in a folder with an ASCII path:
  ```bash
  F="$(cygpath -m "$LOCALAPPDATA")/Temp/kuvatin-fixtures"
  mkdir -p "$F"
  gst-launch-1.0 -e videotestsrc num-buffers=150 ! "video/x-raw,width=320,height=180" ! vp8enc ! webmmux name=m ! filesink location="$F/fixture_av.webm" audiotestsrc num-buffers=300 ! audioconvert ! vorbisenc ! m.
  export GST_TEST_FILE="$F/fixture_av.webm"
  ```
  It is 6.965986394 s long, with a 30 fps picture and a sine tone (measured).

## Where the spec's line numbers are now

The spec was written before the in-app update merged. These are the places it cites that have moved; everything else it cites is still where it says.

Checked against the spec's own commit (`a427bca`) and against `dd515f0`. In `project.rs` everything after line 741 moved by 23 lines (the new `unsaved` field and three methods); in `app.slint` everything after line 290 moved by 17 (the update dialog's properties and its Escape line); in `mod.rs` and `project_file.rs` the merge added the unsaved-work accessor and a handle.

| Spec says | Now (at `dd515f0`) | What it is |
| --- | --- | --- |
| `project.rs:931-959` | `project.rs:954-982` | `add_clip_uri` |
| `project.rs:952-956` | `project.rs:975-979` | the GES name read in `add_clip_uri` |
| `project.rs:1059-1100` | `project.rs:1082-1119` | `trim_clip` |
| `project.rs:1069-1071` | `project.rs:1092-1094` | the `max-duration` read in `trim_clip` |
| `project.rs:1129-1216` | `project.rs:1152-1239` | `set_clip_records` |
| `project.rs:1225-1246` | `project.rs:1248-1269` | `restore_clip` |
| `project.rs:1425-1469` | `project.rs:1448-1492` | `clip_records` |
| `project.rs:1502-1512` | `project.rs:1525-1535` | the part of `apply_document` (1499-1543) that adds each clip and writes its layout |
| `project.rs:1510` | `project.rs:1533` | that layout write |
| `project.rs:1538-1571` | `project.rs:1564-1596` | `clip_layout` |
| `project.rs:1554-1562` | `project.rs:1580-1588` | the scale read-back; the "no upper clamp" comment is 1581-1583 |
| `project.rs:1575-1596` | `project.rs:1601-1619` | `set_clip_layout` |
| `project.rs:1655-1756` | `project.rs:1681-1782` | transport: `play` 1681, `pause` 1691, `seek` 1701, `seek_accurate` 1715, `refresh_preview` 1732, `position` 1773, `duration` 1779 (`set_master_volume` is just before, at 1674) |
| `project.rs:803-811` | `project.rs:806-815` | the preview's restriction caps |
| `project.rs:344-356` | `project.rs:344-361` | `set_clip_times`: the spec's range stopped short of its end |
| `mod.rs:185-195` | `mod.rs:204-214` | the scrub-release handler, which sets a frame-accurate `pending_seek` (line 211) |
| `mod.rs:316-323` | `mod.rs:335-344` | the tick's `pending_seek` apply |
| `project_file.rs:284-327` | `project_file.rs:289-332` | `spawn_thumbnails` |
| `project_file.rs:279` | `project_file.rs:284` | the `spawn_thumbnails` call when a project opens |
| `app.slint:370-372` | `app.slint:387-389` | why `clip-keys` is its own function (the comment starts at 386) |
| `app.slint:373-412` | `app.slint:390-429` | `clip-keys` |
| `app.slint:413-448` | `app.slint:430-465` | `video-keys` |
| `app.slint:421` | `app.slint:438` | `video-keys` refusing while exporting |
| `app.slint:436-439` | `app.slint:453-456` | Ctrl+S (its comment starts at 452) |
| `app.slint:1476-1521` | `app.slint:1493-1538` | the preview box; its derived geometry is 1499-1509 |
| `app.slint:1540` | `app.slint:1557` | the box's resize clamp |
| `app.slint:1661` | `app.slint:1678` | the Scale slider |
| `app.slint:1663` | `app.slint:1680` | the Volume slider |
| `app.slint:1666-1680` | `app.slint:1683-1697` | the stills' Duration block (its comment is 1681-1682) |
| `app.slint:1698-1738` | `app.slint:1715-1755` | the timeline toolbar's chips (the row runs on to the Export button, 1782) |
| `app.slint:1705-1720` | `app.slint:1722-1737` | the Undo and Redo chips |
| `app.slint:1863-1947` | `app.slint:1880-1964` | the clip block, `blk`, to the end of `vis` |
| `app.slint:1888-1947` | `app.slint:1905-1964` | `vis` |
| `app.slint:1922` | `app.slint:1939` | the thumbnail `Image` |
| `app.slint:1923` | `app.slint:1940` | the comment above the left scrim (the scrim is 1941) |

Still correct: `emit_sample` 165-171, `thumbnail_uri` 182, `MIN_TRIM_NS` 261, `trim_left_math` 269-274, `trim_right_math` 279-285, `clip_placed_as` 337-342, the `Layout` doc 649-651, the render profile 596-612, all of `undo.rs`, all of `document.rs`, `timeline.rs:66`, `widgets.slint:584-600`, `crates/kuvatin-video/Cargo.toml:20`, the GES binding lines, `i-slint-compiler-1.16.1/builtins.slint:59-63`, `bundle-gstreamer.ps1:64-90` (69 and 72), `gstreamer.wxs` 330 and 360, and the undo design's contract at 287-294.

## What running GES showed (read before Phase 2)

Measured while writing this plan, 2026-09-23, GStreamer 1.26.11 (MSVC x86_64), `gstreamer-editing-services` 0.23.5, with a probe crate outside the repository. Task 9 measures the speed facts again on your machine and records them under Amendments. Tasks cite these by number.

| # | Fact |
| --- | --- |
| M1 | `Effect::new("videorate")` and `Effect::new("pitch")` are time effects as created (`is_time_effect()` true), with or without a rate in the description, and `pitch tempo=…` is too. No registration is needed. |
| M2 | `register_time_property` returns `false` for `"rate"`, `"GstVideoRate::rate"`, `"GstPitch::rate"` and `"tempo"`: GES has registered them already. The spec's question of which name to register does not arise. |
| M3 | The rate reads as the unqualified child property `"rate"` on both effects. `videorate` holds it as a double (`f64`), `pitch` as a float (`f32`). |
| M4 | GES does **not** refuse a time effect that lowers the clip's duration-limit below its duration: it accepts it and shortens the clip to the new limit (6.966 s became 3.483 s at 2×). Raising the rate on a live effect does the same. Growing the duration past the limit is refused (`set_duration` returns `false`). |
| M5 | `add_top_effect` of a `pitch` effect on a clip with no sound (a still, an image sequence) makes GES return FALSE **without** a GError. The binding's `debug_assert` then panics in a debug build, and a release build reports `Ok(())`. Only add `pitch` where the clip has an `AudioSource`. |
| M6 | `split_full` copies the child properties (`posx`, `width`, `height`, `alpha`, `volume`) and the time effects to the new clip, and translates the new in-point through the rate (1 s of timeline at 2× gives an in-point of 2 s). At the clip's start, or outside it, it returns `Ok(None)`. |
| M7 | `ClipExt::duration_limit()` panics on a still: its limit is `GST_CLOCK_TIME_NONE` and the binding calls `expect`. Read the `"duration-limit"` property as `Option<gst::ClockTime>` instead. |
| M8 | `videorate` on a still is accepted. The engine refuses speed on stills anyway, as the spec says. |
| M9 | The GES pipeline honours a rate seek: 2× advanced 1.96 to 2.0 s per second, 4× 3.5 to 3.96, 8× 6.1 on a 1080p H.264 file (decode-bound, debug build). An ordinary `seek_simple` afterwards plays at 1× again. |
| M10 | Preview buffer durations follow the source's frame rate: 33.3 ms for 30 fps, 40 ms for a still, 100 ms for a 10 fps sequence. Some buffers around a flushing seek carry a duration of **1 ns**. |
| M11 | `pitch tempo=2` on a half-length clip is accepted and halves the duration-limit exactly as `pitch rate=2` does. The difference is only the sound. The spec's `rate` stays; switching is one constant. |
| M12 | Waveform decoding with `uridecodebin`, exposing video streams undecoded through `autoplug-select`: the 7 s fixture in 21 to 27 ms (377 ms when the picture was decoded too), a 2-minute 1080p H.264 file in 180 ms (410 ms). A still gives `None` in 58 ms once `no-more-pads` says no sound came. A hand-written 2 s WAV at 8 kHz spans exactly 2.0 s. |

## File structure

| File | Responsibility |
| --- | --- |
| `crates/kuvatin-video/src/project.rs` | `split_clip`, `frame_secs`, `set_rate` / `rate`, the speed engine (`set_clip_rate`, `clip_rate`, the time-effect helpers, the rate-aware trim maths and record paths), `clip_uri`, `waveform_uri` and `draw_waveform` |
| `crates/kuvatin-video/src/document.rs` | `ClipRecord::rate`, optional and defaulted |
| `crates/kuvatin-video/src/lib.rs` | Exports `waveform_uri` |
| `crates/kuvatin/src/gui/video/mod.rs` | The Scale range and the Speed list as constants, the new modules, the tick's shuttle sync, `add_to_timeline` asking for a waveform |
| `crates/kuvatin/src/gui/video/timeline.rs` | The scale read-back, the split handler, the Speed handler |
| `crates/kuvatin/src/gui/video/transport.rs` (new) | Frame stepping and the shuttle: the pure ladder, the key handlers, the readout |
| `crates/kuvatin/src/gui/video/undo.rs` | `StepKind::Split` and `StepKind::Speed`; `place` writes `rate`; undo gives a restored clip its waveform |
| `crates/kuvatin/src/gui/video/waves.rs` (new) | The waveform cache and its worker |
| `crates/kuvatin/src/gui/video/project_file.rs` | Rows carry `rate` and the waveform when a project opens |
| `crates/kuvatin/src/gui/video/import.rs` | Passes the waveform cache to `add_to_timeline` |
| `crates/kuvatin/ui/app.slint` | Properties, the Split chip, the keys, the shuttle readout, the Speed row, the waveform in the clip block |
| `.github/workflows/release.yml` | The new tests in the gate lists |
| `CHANGELOG.md`, `README.md` | One entry per phase |

---

# Phase 1: scale, split, frame stepping and shuttle

### Task 1: Scale reaches 400 %, from one constant

**Files:**
- Modify: `crates/kuvatin/src/gui/video/mod.rs` (constants after `has_unsaved_changes`, lines 26-33; two setters in `VideoState::new`, after line 70)
- Modify: `crates/kuvatin/src/gui/video/timeline.rs` (import at line 5; the read-back at line 66; a new function above `mod tests`)
- Modify: `crates/kuvatin/ui/app.slint` (properties after line 151; the Scale slider at 1678; the resize clamp at 1557)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module at the bottom of `crates/kuvatin/src/gui/video/timeline.rs`:

```rust
    // ---- the inspector's scale reading --------------------------------------

    /// The engine zooms a clip past the canvas. The read-back used to clamp at
    /// 100 % and snap a zoomed clip back to fit every time it was selected.
    #[test]
    fn the_scale_reading_reaches_past_the_canvas() {
        assert_eq!(scale_percent(1.0), 100.0);
        assert_eq!(scale_percent(2.5), 250.0);
        assert_eq!(scale_percent(9.0), MAX_SCALE_PCT, "capped at the slider's end");
        assert_eq!(scale_percent(0.01), MIN_SCALE_PCT, "and at its start");
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p kuvatin -- timeline::tests::the_scale_reading`
Expected: FAIL to compile, "cannot find function `scale_percent`" and "cannot find value `MAX_SCALE_PCT`".

- [ ] **Step 3: Add the constants**

In `crates/kuvatin/src/gui/video/mod.rs`, after the function `has_unsaved_changes` (ends line 33):

```rust
/// The inspector's Scale range, in percent of the size that fits the canvas.
/// One place for it: the slider, the preview box's corner drag and the
/// read-back when a clip is selected all take it from here, because three
/// copies of it once disagreed with the engine, which has no ceiling at all.
/// 400 % is where other editors stop, and the position sliders run a whole
/// canvas either way, so a clip zoomed that far can still be placed anywhere.
pub(super) const MIN_SCALE_PCT: f32 = 10.0;
pub(super) const MAX_SCALE_PCT: f32 = 400.0;
```

In `VideoState::new`, after `ui.set_timeline_track_labels(ModelRc::from(tracks.clone()));` (line 70):

```rust
        ui.set_insp_scale_min(MIN_SCALE_PCT);
        ui.set_insp_scale_max(MAX_SCALE_PCT);
```

- [ ] **Step 4: Add the window properties and use them**

In `crates/kuvatin/ui/app.slint`, after `in property <bool> insp-has-audio: true;   // hide volume for stills` (line 151):

```slint
    // The Scale range in percent. Set once at start-up from MIN_SCALE_PCT and
    // MAX_SCALE_PCT (gui/video/mod.rs): the slider, the preview box's corner
    // drag and the read-back on select all use the one range. Zero until then.
    in property <float> insp-scale-min;
    in property <float> insp-scale-max;
```

Replace the Scale slider (line 1678):

```slint
                                InspSlider { label: "Scale"; min: root.insp-scale-min; max: root.insp-scale-max; suffix: "%"; value <=> root.insp-scale; changed => { root.inspector-changed(); } }
```

In the preview box's `moved` handler (line 1557), replace the trailing `, 10, 100);` of the `root.insp-scale = Math.clamp(…)` line so it reads:

```slint
                                            root.insp-scale = Math.clamp(Math.abs((self.mouse-x - edit-layer.off-x) / edit-layer.ppc - eta.cen-x) * 2 / root.sel-fit-w * 100, root.insp-scale-min, root.insp-scale-max);
```

- [ ] **Step 5: Use the constants for the read-back**

In `crates/kuvatin/src/gui/video/timeline.rs`, change the import at line 5 from `use super::VideoState;` to:

```rust
use super::{VideoState, MAX_SCALE_PCT, MIN_SCALE_PCT};
```

Replace line 66, `ui.set_insp_scale(((l.scale * 100.0) as f32).clamp(10.0, 100.0));`, with:

```rust
                        ui.set_insp_scale(scale_percent(l.scale));
```

Add above `#[cfg(test)]`:

```rust
/// The inspector's Scale reading, in percent, for an engine scale (1.0 is the
/// size that fits the canvas). Clamped to the range the slider and the
/// preview box can reach, and to nothing tighter.
fn scale_percent(scale: f64) -> f32 {
    ((scale * 100.0) as f32).clamp(MIN_SCALE_PCT, MAX_SCALE_PCT)
}
```

- [ ] **Step 6: Run it and watch it pass**

Run: `cargo test -p kuvatin -- timeline::tests::the_scale_reading`
Expected: 1 passed.

- [ ] **Step 7: Look at it**

Run: `cargo run -p kuvatin`. In Videos mode, open a video, select its clip, drag Scale to the right end: it reads 400 %. Drag the preview box's corner outward past the canvas edge: the box keeps growing. Select another clip and back: the zoom stays. Close the window.

- [ ] **Step 8: Commit**

Gates (see Before you start), then:

```bash
git add -A
git commit -m "A clip can be zoomed to 400 percent, from one range in one place" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Pin the engine's zoom round trip

The engine already allows it (`clip_layout` has no ceiling, `project.rs:1581-1583`). This task pins that with a test at last, and corrects the doc comment that says otherwise.

**Files:**
- Modify: `crates/kuvatin-video/src/project.rs` (the `Layout` doc at 649-651; a test at the end of `mod tests`)
- Modify: `.github/workflows/release.yml`

- [ ] **Step 1: Write the test**

Add at the end of the `tests` module in `crates/kuvatin-video/src/project.rs`:

```rust
    /// The engine zooms a clip past the canvas and the read-back must say so:
    /// a 1.0 ceiling in `clip_layout` once snapped every zoomed clip back to
    /// fit whenever the inspector refreshed. Through a save and a load too.
    #[test]
    fn a_zoomed_clip_keeps_its_scale() {
        let (dir, png, mut project) = undo_fixture("zoomed");
        let a = project
            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(2.0))
            .expect("a");
        project.set_clip_layout(
            &a,
            Layout {
                posx: -320,
                posy: -180,
                scale: 2.5,
                alpha: 1.0,
                volume: 1.0,
            },
        );
        let read = project.clip_layout(&a).expect("a layout");
        assert!((read.scale - 2.5).abs() < 1e-3, "read back {}", read.scale);
        let doc = project.to_document();
        let mut reopened = Project::new(|_f| {}).expect("project");
        reopened.apply_document(&doc).expect("apply");
        let again = &reopened.to_document().clips[0];
        assert!(
            (again.layout.scale - 2.5).abs() < 1e-3,
            "after a load {}",
            again.layout.scale
        );
        assert_eq!((again.layout.posx, again.layout.posy), (-320, -180));
        let _ = std::fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 2: Run it**

Run: `cargo test -p kuvatin-video -- --test-threads=1 a_zoomed_clip_keeps_its_scale`
Expected: PASS on the first run, because it pins behaviour the engine already has. To see that it guards something, temporarily change `(width as f64 / fit_w).max(0.0)` in `clip_layout` to `(width as f64 / fit_w).clamp(0.0, 1.0)`, run it again and watch it fail with "read back 1", then undo that change.

- [ ] **Step 3: Correct the doc comment**

Replace the `Layout` doc comment (lines 649-651):

```rust
/// A clip's transform + audio level for the inspector. `scale` is relative to
/// the largest size that fits the canvas WITHOUT distorting the source, so a
/// non-16:9 clip keeps its aspect ratio: 1.0 fits, above 1.0 zooms past the
/// canvas edges, and nothing in the engine caps it.
```

- [ ] **Step 4: Gate it in CI**

In `.github/workflows/release.yml`, step `Test (video engine, self-contained — gates the release)`, the name list ends with the line `            removing_two_clips_back_to_back_while_playing_does_not_crash`. Append a space and `a_zoomed_clip_keeps_its_scale` to that line, so it reads:

```powershell
            removing_two_clips_back_to_back_while_playing_does_not_crash a_zoomed_clip_keeps_its_scale
```

Later tasks append to the same line.

Run: `python -c "import yaml,io; yaml.safe_load(io.open('.github/workflows/release.yml', encoding='utf-8')); print('yaml ok')"`
Expected: `yaml ok`.

- [ ] **Step 5: Commit**

Gates, plus `cargo test -p kuvatin-video -- --test-threads=1 a_zoomed_clip_keeps_its_scale`, then:

```bash
git add -A
git commit -m "A zoomed clip's scale is pinned by a test at last" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: The engine splits a clip

**Files:**
- Modify: `crates/kuvatin-video/src/project.rs` (`split_fits` after `trim_right_math`, which ends line 285; `Project::split_clip` after `set_clip_duration`, which ends line 1131; tests)
- Modify: `.github/workflows/release.yml`

- [ ] **Step 1: Write the failing pure test**

Add to the `tests` module in `crates/kuvatin-video/src/project.rs`, after `trim_math_never_goes_negative`:

```rust
    #[test]
    fn split_needs_the_minimum_on_both_sides() {
        // A clip at [1 s, 3 s).
        assert!(!split_fits(S, 2 * S, S), "at its start");
        assert!(
            !split_fits(S, 2 * S, S + MIN_TRIM_NS - 1),
            "a nanosecond too close to the start"
        );
        assert!(split_fits(S, 2 * S, S + MIN_TRIM_NS), "the minimum from the start");
        assert!(split_fits(S, 2 * S, 2 * S), "the middle");
        assert!(split_fits(S, 2 * S, 3 * S - MIN_TRIM_NS), "the minimum from the end");
        assert!(
            !split_fits(S, 2 * S, 3 * S - MIN_TRIM_NS + 1),
            "a nanosecond too close to the end"
        );
        assert!(!split_fits(S, 2 * S, 5 * S), "outside it");
        // 0.3 s cannot leave 0.2 s on both sides.
        assert!(!split_fits(0, 300_000_000, 150_000_000));
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p kuvatin-video -- --test-threads=1 split_needs_the_minimum`
Expected: FAIL to compile, "cannot find function `split_fits`".

- [ ] **Step 3: Implement the rule**

Add after `trim_right_math` (ends line 285):

```rust
/// Whether a clip at `start` lasting `dur` can be cut at `at`, all in
/// nanoseconds: only where both halves keep at least [`MIN_TRIM_NS`].
fn split_fits(start: i128, dur: i128, at: i128) -> bool {
    at >= start + MIN_TRIM_NS && at <= start + dur - MIN_TRIM_NS
}
```

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -p kuvatin-video -- --test-threads=1 split_needs_the_minimum`
Expected: 1 passed.

- [ ] **Step 5: Write the failing engine tests**

Add to the `tests` module:

```rust
    #[test]
    fn split_cuts_a_clip_in_two_where_asked() {
        let (dir, png, mut project) = undo_fixture("split-middle");
        let a = project
            .add_clip(&png, 0, secs(1.0), Duration::ZERO, secs(4.0))
            .expect("a");
        project.set_clip_layout(
            &a,
            Layout {
                posx: 12,
                posy: 34,
                scale: 0.6,
                alpha: 0.5,
                volume: 1.0,
            },
        );
        let before = record_of(&project, &a);
        let (b, left, right) = project.split_clip(&a, secs(2.5)).expect("split");
        assert_ne!(b, a, "the right half is a new clip");
        assert_eq!((left.start, left.duration), (secs(1.0), secs(1.5)));
        assert_eq!((right.start, right.duration), (secs(2.5), secs(2.5)));
        assert_eq!(left.start + left.duration, right.start, "they touch exactly");
        assert_eq!(
            right.inpoint,
            left.inpoint + left.duration,
            "the right half carries on where the left stops"
        );
        let (ra, rb) = (record_of(&project, &a), record_of(&project, &b));
        assert_eq!(ra.layout, before.layout, "the left keeps its transform");
        assert_eq!(rb.layout, before.layout, "and the right half has it too");
        assert_eq!((ra.track, rb.track), (0, 0));
        assert_eq!(project.clip_records().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn split_refuses_within_the_minimum_of_either_edge() {
        let (dir, png, mut project) = undo_fixture("split-edges");
        // A clip at [1 s, 3 s).
        let a = project
            .add_clip(&png, 0, secs(1.0), Duration::ZERO, secs(2.0))
            .expect("a");
        let before = record_of(&project, &a);
        for at in [0.5, 1.0, 1.1, 2.9, 3.0, 4.0] {
            let err = project.split_clip(&a, secs(at)).expect_err("refused");
            assert!(format!("{err:#}").contains("0.2 s"), "at {at}: {err:#}");
        }
        assert_eq!(project.clip_records().len(), 1, "nothing was cut");
        assert_same_record(&record_of(&project, &a), &before);
        // Exactly the minimum is allowed.
        let (_, left, right) = project
            .split_clip(&a, Duration::from_millis(1200))
            .expect("at 1.2 s");
        assert_eq!(
            (left.duration, right.duration),
            (Duration::from_millis(200), Duration::from_millis(1800))
        );
        // The playhead now sits on the cut, where cutting again is refused.
        assert!(project.split_clip(&a, Duration::from_millis(1200)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undo writes the left half's old record and removes the right half;
    /// redo writes the left half again and restores the right half under the
    /// ID the step holds. No split is replayed: these are the operations
    /// `undo::plan` already produces.
    #[test]
    fn undo_puts_a_split_clip_back_as_one_and_redo_cuts_it_again() {
        let (dir, png, mut project) = undo_fixture("undo-split");
        let a = project
            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(4.0))
            .expect("a");
        let whole = record_of(&project, &a);
        let (b, _, _) = project.split_clip(&a, secs(1.5)).expect("split");
        let (left, right) = (record_of(&project, &a), record_of(&project, &b));
        assert!(project.remove_clip(&b));
        assert!(write_back(&mut project, &[(&a, &whole)]));
        assert_eq!(project.clip_records().len(), 1, "one clip again");
        assert_same_record(&record_of(&project, &a), &whole);
        assert!(write_back(&mut project, &[(&a, &left)]));
        assert_eq!(project.restore_clip(&b, &right).expect("restore"), b);
        assert_same_record(&record_of(&project, &a), &left);
        assert_same_record(&record_of(&project, &b), &right);
        let _ = std::fs::remove_dir_all(&dir);
    }
```

The values are computed, not guessed: a still has no source length, and GES gives the right half the in-point `left.inpoint + left.duration` (M6); `secs(2.5)` and `secs(1.5)` are exact in `f64`; the boundary uses `from_millis` so no float rounding can move it.

- [ ] **Step 6: Run them and watch them fail**

Run: `cargo test -p kuvatin-video -- --test-threads=1 split_`
Expected: FAIL to compile, "no method named `split_clip`".

- [ ] **Step 7: Implement `split_clip`**

Add to `impl Project`, after `set_clip_duration` (ends line 1131):

```rust
    /// Cut the clip in two at timeline position `at`. The clip keeps its ID,
    /// start and in-point and ends at `at`; the returned clip begins there,
    /// with the in-point that follows (GES translates it through any speed
    /// change). Refused unless both halves keep at least 0.2 s. Returns the
    /// new clip's ID and both halves' geometry, left first.
    pub fn split_clip(
        &mut self,
        id: &ClipId,
        at: Duration,
    ) -> Result<(ClipId, ClipGeom, ClipGeom)> {
        if self.rendering.get() {
            anyhow::bail!("a render is in progress");
        }
        let clip = self
            .clips
            .get(&id.0)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("clip {} is not on the timeline", id.0))?;
        let start = clip.start().nseconds() as i128;
        let dur = clip.duration().nseconds() as i128;
        let at_ns = at.as_nanos() as i128;
        if !split_fits(start, dur, at_ns) {
            anyhow::bail!(
                "a split leaves at least {:.1} s of the clip on each side of the playhead",
                MIN_TRIM_NS as f64 / 1e9
            );
        }
        // GES copies the transform to the new half (measured, M6), but this
        // does not lean on it: the right half gets it written explicitly.
        // Only once the clip has one: a clip never laid out still has GES's
        // stretch-to-fill, and writing back the read-back of that would turn
        // it into a fitted frame in the corner.
        let laid_out = clip
            .child_property("width")
            .and_then(|v| v.get::<i32>().ok())
            .unwrap_or(0)
            > 0;
        let layout = if laid_out { self.clip_layout(id) } else { None };
        let right = clip
            .split_full(at_ns as u64)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .ok_or_else(|| anyhow::anyhow!("GES did not split the clip"))?;
        self.commit();
        // Named the way `add_clip_uri` names a clip: a nameless one would
        // collide on the "" key and orphan whatever was there.
        let name = right
            .name()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("GES returned an unnamed clip"))?;
        self.clips.insert(name.clone(), right.clone());
        let right_id = ClipId(name);
        if let Some(l) = layout {
            self.set_clip_layout(&right_id, l);
        }
        self.touched();
        Ok((right_id, clip_geom(&clip), clip_geom(&right)))
    }
```

- [ ] **Step 8: Run them and watch them pass**

Run: `cargo test -p kuvatin-video -- --test-threads=1 split_`
Expected: 4 passed (`split_needs_the_minimum_on_both_sides`, `split_cuts_a_clip_in_two_where_asked`, `split_refuses_within_the_minimum_of_either_edge`, and `undo_puts_a_split_clip_back_as_one_and_redo_cuts_it_again`, whose name contains `split_`).

- [ ] **Step 9: Gate them in CI**

Append ` split_` to the last line of the self-contained list in `.github/workflows/release.yml` (the line Task 2 extended). The `undo_` filter already in the list covers the undo test as well.

Run: `python -c "import yaml,io; yaml.safe_load(io.open('.github/workflows/release.yml', encoding='utf-8')); print('yaml ok')"`
Expected: `yaml ok`.

- [ ] **Step 10: Commit**

Gates, plus `cargo test -p kuvatin-video -- --test-threads=1 split_ undo_`, then:

```bash
git add -A
git commit -m "The engine cuts a clip in two where the playhead stands" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Split in the undo history and in the interface

**Files:**
- Modify: `crates/kuvatin/src/gui/video/undo.rs` (`StepKind` at 24-33, `describe` at 158-170, tests)
- Modify: `crates/kuvatin/ui/app.slint` (a callback after line 234; a chip after the Redo chip, which ends line 1737; `video-keys` at 430-465)
- Modify: `crates/kuvatin/src/gui/video/timeline.rs` (import at line 6; a handler after the trim handler, which ends line 272)

- [ ] **Step 1: Write the failing undo tests**

In the `tests` module of `crates/kuvatin/src/gui/video/undo.rs`:

In `gestures_on_the_same_clip_merge_and_nothing_else_does`, add `StepKind::Split,` to the second list, after `StepKind::AddTrack,`.

In `each_kind_describes_itself`, add after the `AddTrack` line:

```rust
        assert_eq!(d(StepKind::Split), "splitting intro.mp4");
```

Add a new test:

```rust
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
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p kuvatin -- undo::tests`
Expected: FAIL to compile, "no variant named `Split`".

- [ ] **Step 3: Add the kind**

In `enum StepKind`, after `AddTrack,`:

```rust
    Split,
```

In `describe`, after the `AddTrack` arm:

```rust
            StepKind::Split => format!("splitting {name}"),
```

`merges()` is unchanged: a split never merges.

- [ ] **Step 4: Run them and watch them pass**

Run: `cargo test -p kuvatin -- undo::tests`
Expected: every undo test passes, including `a_split_undoes_to_one_clip_and_redoes_to_two`.

- [ ] **Step 5: Add the callback, the chip and the key**

In `crates/kuvatin/ui/app.slint`, after `callback timeline-clip-removed(int);             // delete the clip at this index` (line 234):

```slint
    callback timeline-split();                       // cut the selected clip in two at the playhead
```

After the Redo chip's closing brace (line 1737), before the `// Zoom:` comment:

```slint
                                split-chip := TimelineChip {
                                    label: "Split";
                                    wide: true;
                                    hint: "Split the selected clip at the playhead (S)";
                                    enabled: root.timeline-selected >= 0 && !root.modal-open() && !root.video-engine-down;
                                    focus-on-click: false;
                                    clicked => { root.timeline-split(); if (!split-chip.has-focus) { kbd.focus(); } }
                                }
```

In `video-keys`, after the Delete line (line 463) and before `return root.clip-keys(event);`:

```slint
        // Plain S splits the selected clip at the playhead; Ctrl+S, above, saves.
        if ((event.text == "s" || event.text == "S") && !event.modifiers.control && !event.modifiers.alt && !event.modifiers.meta && root.timeline-selected >= 0) {
            root.timeline-split();
            return EventResult.accept;
        }
```

- [ ] **Step 6: Add the handler**

In `crates/kuvatin/src/gui/video/timeline.rs`, change line 6 to:

```rust
use crate::gui::{show_error, AppWindow, ClipKind, TimelineClip};
```

After the trim handler's block (ends line 272), before `// Inspector Duration field (stills)`:

```rust
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
                    show_error(&ui, &format!("Could not split {}", left.name), format!("{e:#}"));
                }
            }
        });
    }
```

The selection stays on the left half: its row index does not move, and the right half's row goes on the end.

- [ ] **Step 7: Build and run the suite**

Run: `cargo build -p kuvatin` and `cargo test -p kuvatin`
Expected: both clean. A Slint error names its line; fix it before moving on.

- [ ] **Step 8: Look at it**

Run: `cargo run -p kuvatin`. Open a video, select its clip, click the lane to put the playhead inside it, press **S**: two clips, touching, the left one selected. The Undo chip's hint says "Undo splitting …". Press Ctrl+Z: one clip. Ctrl+Y: two again. With the playhead on the cut, **S** shows "Could not split …" naming 0.2 s. With nothing selected the Split chip is grey. Close the window.

- [ ] **Step 9: Commit**

Gates, then:

```bash
git add -A
git commit -m "Split is a chip and a key, and undo puts the clip back as one" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: The engine knows how long a frame is

The preview pins no frame rate (`project.rs:806-815`), so the engine measures one from the frames it shows. Buffers of 1 ns appear around a flushing seek (M10); they are not frames.

**Files:**
- Modify: `crates/kuvatin-video/src/project.rs` (`emit_sample` at 164-171; `Project` fields after line 772; `Project::new` at 827-864; `frame_secs` after `seek_accurate`, which ends line 1724; tests)
- Modify: `.github/workflows/release.yml`

- [ ] **Step 1: Write the failing pure test and the fixture helper**

Add to the `tests` module:

```rust
    /// A generated image sequence: real source time, unlike a still, and no
    /// sound. `frames` PNGs at `fps`, so it lasts `frames / fps` seconds.
    fn sequence_fixture(tag: &str, frames: u32, fps: u32) -> (std::path::PathBuf, String) {
        let dir = scratch(tag);
        for i in 1..=frames {
            image::RgbaImage::from_pixel(64, 36, image::Rgba([(i * 7 % 255) as u8, 90, 200, 255]))
                .save(dir.join(format!("frame_{i:04}.png")))
                .expect("write a frame");
        }
        let mut spec =
            crate::sequence::detect_sequence(&dir.join("frame_0001.png")).expect("detect");
        spec.fps = fps;
        let uri = spec.uri().expect("uri");
        (dir, uri)
    }

    #[test]
    fn frame_length_ignores_buffers_too_short_to_be_frames() {
        assert_eq!(next_frame_ns(40_000_000, Some(33_333_333)), 33_333_333);
        assert_eq!(next_frame_ns(33_333_333, None), 33_333_333, "no duration keeps the last");
        assert_eq!(
            next_frame_ns(33_333_333, Some(1)),
            33_333_333,
            "the 1 ns buffers a flushing seek leaves"
        );
        assert_eq!(next_frame_ns(33_333_333, Some(100_000_000)), 100_000_000);
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p kuvatin-video -- --test-threads=1 frame_length`
Expected: FAIL to compile, "cannot find function `next_frame_ns`".

- [ ] **Step 3: Measure frames as they arrive**

Replace `emit_sample` (lines 164-171) with:

```rust
/// A frame's length until the preview has shown one: 1/25 s.
const DEFAULT_FRAME_NS: u64 = 40_000_000;

/// The shortest buffer taken as a frame. The composited preview stamps a few
/// buffers with a duration of 1 ns around a flushing seek (measured, M10); a
/// "frame" that short would make a frame step invisible.
const MIN_FRAME_NS: u64 = 1_000_000;

/// The frame length to keep once a buffer lasting `buffer_ns` has arrived: a
/// buffer with no duration, or one too short to be a frame, leaves the last
/// good length in place.
fn next_frame_ns(last_ns: u64, buffer_ns: Option<u64>) -> u64 {
    match buffer_ns {
        Some(ns) if ns >= MIN_FRAME_NS => ns,
        _ => last_ns,
    }
}

/// Push one RGBA video sample to the frame callback, noting how long it lasts.
fn emit_sample(
    sample: &gst::Sample,
    cb: &(dyn Fn(FrameView<'_>) + Send + Sync),
    frame_ns: &AtomicU64,
) -> std::result::Result<gst::FlowSuccess, gst::FlowError> {
    let lasts = sample.buffer().and_then(|b| b.duration()).map(|d| d.nseconds());
    frame_ns.store(
        next_frame_ns(frame_ns.load(Ordering::Relaxed), lasts),
        Ordering::Relaxed,
    );
    with_frame_view(sample, cb).ok_or(gst::FlowError::Error)?;
    Ok(gst::FlowSuccess::Ok)
}
```

In `struct Project`, after `track_commits: Vec<Arc<AtomicU64>>,` (line 772):

```rust
    /// How long the last composited preview frame lasted, in nanoseconds
    /// (see [`Project::frame_secs`]). Written on the appsink's streaming
    /// thread, read on the interface's.
    frame_ns: Arc<AtomicU64>,
```

In `Project::new`, replace the block from `let cb: Arc<dyn Fn(FrameView<'_>) + Send + Sync> = Arc::new(on_frame);` to the end of `appsink.set_callbacks(…);` (lines 827-845) with:

```rust
        let frame_ns = Arc::new(AtomicU64::new(DEFAULT_FRAME_NS));
        let cb: Arc<dyn Fn(FrameView<'_>) + Send + Sync> = Arc::new(on_frame);
        let cb_sample = cb.clone();
        let cb_preroll = cb;
        let ns_sample = frame_ns.clone();
        let ns_preroll = frame_ns.clone();
        appsink.set_callbacks(
            AppSinkCallbacks::builder()
                // Playing: frames arrive as samples.
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    emit_sample(&sample, &*cb_sample, &ns_sample)
                })
                // Paused / after a seek: the current frame arrives as a preroll
                // buffer, so deliver it too — otherwise edits don't repaint while
                // the timeline is paused.
                .new_preroll(move |sink| {
                    let sample = sink.pull_preroll().map_err(|_| gst::FlowError::Eos)?;
                    emit_sample(&sample, &*cb_preroll, &ns_preroll)
                })
                .build(),
        );
```

In the `Ok(Self { … })` at the end of `new`, after `track_commits,`:

```rust
            frame_ns,
```

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -p kuvatin-video -- --test-threads=1 frame_length_ignores`
Expected: 1 passed.

- [ ] **Step 5: Write the failing engine test**

```rust
    /// The composited preview runs at the source's rate (measured, M10), so a
    /// 10 fps sequence shows frames a tenth of a second long.
    #[test]
    fn frame_length_follows_the_preview() {
        let (dir, uri) = sequence_fixture("frame-length", 30, 10);
        let mut project = Project::new(|_f| {}).expect("project");
        assert_eq!(project.frame_secs(), 0.04, "1/25 s until a frame has arrived");
        project.append_clip_uri(&uri, 0, None).expect("append");
        project.play().expect("play");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while project.frame_secs() == 0.04 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(project.frame_secs(), 0.1);
        let _ = project.pause();
        let _ = std::fs::remove_dir_all(&dir);
    }
```

`40_000_000 as f64 / 1e9` and `100_000_000 as f64 / 1e9` are the doubles nearest 0.04 and 0.1, which is what the literals are, so exact comparison is right.

- [ ] **Step 6: Run it and watch it fail**

Run: `cargo test -p kuvatin-video -- --test-threads=1 frame_length_follows`
Expected: FAIL to compile, "no method named `frame_secs`".

- [ ] **Step 7: Add the accessor**

In `impl Project`, after `seek_accurate` (ends line 1724):

```rust
    /// How long one composited preview frame lasts, in seconds, from the last
    /// frame that arrived; 1/25 s until one has. A measurement of what the
    /// preview produces, which is what a frame step walks past.
    pub fn frame_secs(&self) -> f64 {
        self.frame_ns.load(Ordering::Relaxed) as f64 / 1e9
    }
```

- [ ] **Step 8: Run both and watch them pass**

Run: `cargo test -p kuvatin-video -- --test-threads=1 frame_length`
Expected: 2 passed.

- [ ] **Step 9: Gate them in CI**

Append ` frame_length` to the last line of the self-contained list in `.github/workflows/release.yml`, then run the YAML check from Task 2 Step 4.

- [ ] **Step 10: Commit**

Gates, plus `cargo test -p kuvatin-video -- --test-threads=1 frame_length`, then:

```bash
git add -A
git commit -m "The engine measures how long a preview frame lasts" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: The engine plays faster when asked

**Files:**
- Modify: `crates/kuvatin-video/src/project.rs` (a `rate` field; `seek`, `seek_accurate`, `refresh_preview`, `begin_restore`; `set_rate` and `rate` after `frame_secs`; a test)
- Modify: `.github/workflows/release.yml`

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn shuttle_plays_faster_and_refuses_reverse() {
        let (dir, png, mut project) = undo_fixture("shuttle");
        project
            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(20.0))
            .expect("a");
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(project.set_rate(bad).is_err(), "{bad} must be refused");
        }
        assert_eq!(project.rate(), 1.0, "a refused rate changes nothing");
        project.play().expect("play");
        let end = std::time::Instant::now() + Duration::from_secs(10);
        while settled(&project.pipeline) == Step::Pending && std::time::Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        project.set_rate(4.0).expect("4x");
        assert_eq!(project.rate(), 4.0);
        let _ = project.pipeline.state(gst::ClockTime::from_seconds(3));
        let from = project.position().expect("a position");
        std::thread::sleep(Duration::from_millis(1000));
        let to = project.position().expect("a position");
        // Measured at 3.96 s of timeline per second on a still (M9). Anything
        // clearly faster than normal proves the rate reached the pipeline.
        assert!(to > from + Duration::from_millis(2000), "{from:?} -> {to:?}");
        project.seek(secs(1.0)).expect("seek");
        assert_eq!(project.rate(), 1.0, "an ordinary seek plays at normal speed again");
        let _ = project.pause();
        let _ = std::fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p kuvatin-video -- --test-threads=1 shuttle_`
Expected: FAIL to compile, "no method named `set_rate`".

- [ ] **Step 3: Implement**

In `struct Project`, after the `frame_ns` field from Task 5:

```rust
    /// The rate the preview plays at: 1.0, except after [`Project::set_rate`]
    /// until the next ordinary seek, which plays at normal speed again.
    rate: std::cell::Cell<f64>,
```

In `Ok(Self { … })`, after `frame_ns,`:

```rust
            rate: std::cell::Cell::new(1.0),
```

In `seek` and in `seek_accurate`, after `)?;` that closes the `self.pipeline.seek_simple(` call and before `Ok(())`:

```rust
        // seek_simple always plays at 1.0.
        self.rate.set(1.0);
```

In `refresh_preview`, replace its final statement, `let _ = self.pipeline.seek_simple(…);`, with:

```rust
        if self
            .pipeline
            .seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                gst::ClockTime::from_nseconds(pos.as_nanos() as u64),
            )
            .is_ok()
        {
            self.rate.set(1.0);
        }
```

In `begin_restore`, before its `Ok(())`:

```rust
        // The pipeline went through NULL: it plays at 1.0 again.
        self.rate.set(1.0);
```

After `frame_secs`:

```rust
    /// Play forward at `rate` (1.0 = normal) from where the playhead is.
    /// Refuses a rate that is not finite and greater than zero: this engine
    /// has no reverse, and a negative rate would fail deep inside GES with
    /// nothing to show the user. Inert while rendering, like `play`.
    pub fn set_rate(&self, rate: f64) -> Result<()> {
        if !(rate.is_finite() && rate > 0.0) {
            anyhow::bail!("a playback rate must be above zero, not {rate}");
        }
        if self.rendering.get() {
            return Ok(());
        }
        let pos = self.position().unwrap_or(Duration::ZERO);
        // A full seek: seek_simple cannot carry a rate.
        self.pipeline.seek(
            rate,
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::SeekType::Set,
            gst::ClockTime::from_nseconds(pos.as_nanos() as u64),
            gst::SeekType::None,
            gst::ClockTime::NONE,
        )?;
        self.rate.set(rate);
        Ok(())
    }

    /// The rate the preview plays at: 1.0 unless [`Self::set_rate`] changed
    /// it since the last ordinary seek.
    pub fn rate(&self) -> f64 {
        self.rate.get()
    }
```

The spec's `SeekType::End` for the stop is replaced by `SeekType::None`: that is the call measured to work (M9).

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -p kuvatin-video -- --test-threads=1 shuttle_`
Expected: 1 passed.

- [ ] **Step 5: Gate it in CI**

Append ` shuttle_` to the last line of the self-contained list in `.github/workflows/release.yml`, then run the YAML check.

- [ ] **Step 6: Commit**

Gates, plus `cargo test -p kuvatin-video -- --test-threads=1 shuttle_ frame_length transport_and_edits_are_inert_while_rendering`, then:

```bash
git add -A
git commit -m "The engine plays forward faster on request, and never backwards" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Frame stepping and the shuttle, on the keyboard

**Files:**
- Create: `crates/kuvatin/src/gui/video/transport.rs`
- Modify: `crates/kuvatin/src/gui/video/mod.rs` (module list at lines 5-9; `wire` at 110; the volume handler at 216-228; the tick before line 367)
- Modify: `crates/kuvatin/ui/app.slint` (callbacks and a property after line 100; `transport-keys` after `clip-keys`, which ends line 429; `video-keys`; the readout after the play button, which ends line 1590)

- [ ] **Step 1: Write the failing tests**

Create `crates/kuvatin/src/gui/video/transport.rs` with the module doc and the tests only:

```rust
//! Frame stepping and the J / K / L shuttle. Neither is an edit: nothing is
//! recorded and nothing is marked unsaved. `J` is not reverse play, as it is
//! in other editors: the engine has no reverse (see the clip-edits design),
//! so `J` steps the speed down and then pauses.

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn shuttle_l_plays_then_climbs_to_eight_and_stops() {
        assert_eq!(shuttle(1, false, 1.0), Shuttle::Play(1.0), "paused: play");
        assert_eq!(shuttle(1, true, 1.0), Shuttle::Play(2.0));
        assert_eq!(shuttle(1, true, 2.0), Shuttle::Play(4.0));
        assert_eq!(shuttle(1, true, 4.0), Shuttle::Play(8.0));
        assert_eq!(shuttle(1, true, 8.0), Shuttle::Play(8.0), "8x is the top");
    }

    #[test]
    fn shuttle_j_climbs_down_and_pauses_at_normal_speed() {
        assert_eq!(shuttle(-1, true, 8.0), Shuttle::Play(4.0));
        assert_eq!(shuttle(-1, true, 4.0), Shuttle::Play(2.0));
        assert_eq!(shuttle(-1, true, 2.0), Shuttle::Play(1.0));
        assert_eq!(shuttle(-1, true, 1.0), Shuttle::Pause);
    }

    #[test]
    fn shuttle_j_while_paused_steps_back_a_frame() {
        assert_eq!(shuttle(-1, false, 1.0), Shuttle::Step(-1));
    }

    #[test]
    fn shuttle_k_pauses_from_any_speed_and_plays_when_paused() {
        for rate in [1.0, 2.0, 8.0] {
            assert_eq!(shuttle(0, true, rate), Shuttle::Pause, "{rate}");
        }
        assert_eq!(shuttle(0, false, 4.0), Shuttle::Play(1.0));
    }

    #[test]
    fn shuttle_from_a_rate_off_the_ladder_goes_to_the_next_rung() {
        assert_eq!(shuttle(1, true, 3.0), Shuttle::Play(4.0));
        assert_eq!(shuttle(-1, true, 3.0), Shuttle::Play(2.0));
    }

    #[test]
    fn a_frame_step_stays_inside_the_timeline() {
        assert!(near(step_target(1.0, 0.04, 1, 10.0), 1.04));
        assert!(near(step_target(1.0, 0.04, -1, 10.0), 0.96));
        assert_eq!(step_target(0.02, 0.04, -1, 10.0), 0.0, "not before the start");
        assert_eq!(step_target(9.99, 0.04, 1, 10.0), 10.0, "not past the end");
        assert_eq!(step_target(3.0, 0.04, 0, 10.0), 3.0, "no direction, no step");
    }
}
```

Add `mod transport;` to the module list in `crates/kuvatin/src/gui/video/mod.rs`, after `mod timeline;`.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p kuvatin -- transport::tests`
Expected: FAIL to compile, "cannot find function `shuttle`".

- [ ] **Step 3: Implement the rules**

Put above the tests in `crates/kuvatin/src/gui/video/transport.rs`:

```rust
use super::VideoState;
use crate::gui::AppWindow;
use slint::ComponentHandle;
use std::cell::Cell;
use std::time::Duration;

/// The forward speeds the shuttle climbs through.
const LADDER: [f64; 4] = [1.0, 2.0, 4.0, 8.0];

/// What a shuttle key asks the transport to do.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Shuttle {
    /// Play forward at this rate.
    Play(f64),
    /// Stop, at normal speed.
    Pause,
    /// Step one frame back (-1) or on (+1), paused.
    Step(i32),
}

/// `key` is -1 for J, 0 for K and +1 for L; `rate` is what the preview plays
/// at now. L plays, then climbs the ladder; J climbs down it and pauses at
/// normal speed, or steps a frame back when paused; K pauses, or plays.
fn shuttle(key: i32, playing: bool, rate: f64) -> Shuttle {
    match (key.signum(), playing) {
        (1, true) => Shuttle::Play(
            LADDER
                .iter()
                .copied()
                .find(|&r| r > rate)
                .unwrap_or(LADDER[LADDER.len() - 1]),
        ),
        (-1, true) => LADDER
            .iter()
            .copied()
            .rev()
            .find(|&r| r < rate)
            .map_or(Shuttle::Pause, Shuttle::Play),
        (0, true) => Shuttle::Pause,
        (-1, false) => Shuttle::Step(-1),
        // L or K while paused: play, at normal speed.
        _ => Shuttle::Play(1.0),
    }
}

/// Where a one-frame step from `at` lands: `frame` seconds back or on, never
/// before the start or past `duration`.
fn step_target(at: f64, frame: f64, dir: i32, duration: f64) -> f64 {
    (at + frame * f64::from(dir.signum())).clamp(0.0, duration.max(0.0))
}

/// Wire the frame-step and shuttle keys.
pub(super) fn wire(ui: &AppWindow, st: &VideoState) {
    {
        let ui_weak = ui.as_weak();
        let project_slot = st.project.clone();
        let pending_seek = st.pending_seek.clone();
        ui.on_video_step(move |dir| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let slot = project_slot.borrow();
            let Some(p) = slot.as_ref() else {
                return;
            };
            step_frame(&ui, p, &pending_seek, dir);
            sync_shuttle(&ui, p);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let project_slot = st.project.clone();
        let pending_seek = st.pending_seek.clone();
        ui.on_video_shuttle(move |key| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let slot = project_slot.borrow();
            let Some(p) = slot.as_ref() else {
                return;
            };
            match shuttle(key, ui.get_video_playing(), p.rate()) {
                Shuttle::Play(rate) => {
                    // A frame step still waiting for the tick lands first, so
                    // the shuttle starts from where the playhead shows.
                    if let Some((secs, _)) = pending_seek.take() {
                        let _ = p.seek_accurate(Duration::from_secs_f32(secs.max(0.0)));
                    }
                    // From the end of the timeline, start again, as Play does.
                    if let (Some(pos), Some(length)) = (p.position(), p.duration()) {
                        if pos + Duration::from_millis(120) >= length {
                            let _ = p.seek(Duration::ZERO);
                        }
                    }
                    if p.set_rate(rate).is_ok() && p.play().is_ok() {
                        ui.set_video_playing(true);
                    }
                }
                Shuttle::Pause => {
                    let _ = p.pause();
                    let _ = p.set_rate(1.0);
                    ui.set_video_playing(false);
                }
                Shuttle::Step(dir) => step_frame(&ui, p, &pending_seek, dir),
            }
            sync_shuttle(&ui, p);
        });
    }
}

/// Pause, and move the playhead one frame back (`dir` < 0) or on. The seek
/// is left for the preview tick, which lands it frame-accurately, as it does
/// the end of a scrub.
fn step_frame(
    ui: &AppWindow,
    p: &kuvatin_video::Project,
    pending_seek: &Cell<Option<(f32, bool)>>,
    dir: i32,
) {
    if ui.get_video_playing() {
        let _ = p.pause();
        ui.set_video_playing(false);
    }
    let length = p.duration().map(|d| d.as_secs_f64()).unwrap_or(0.0);
    let to = step_target(f64::from(ui.get_playhead()), p.frame_secs(), dir, length) as f32;
    pending_seek.set(Some((to, true)));
    ui.set_playhead(to);
    if length > 0.0 {
        ui.set_video_position((f64::from(to) / length).clamp(0.0, 1.0) as f32);
    }
}

/// Show the rate the preview plays at, and mute it while that is not 1×:
/// faster sound only chirps. The transport slider's value is left alone, so
/// the user's level comes back. Does nothing when the readout already
/// matches, so the preview tick calls it every time.
pub(super) fn sync_shuttle(ui: &AppWindow, project: &kuvatin_video::Project) {
    let rate = project.rate() as f32;
    if rate == ui.get_shuttle_rate() {
        return;
    }
    ui.set_shuttle_rate(rate);
    project.set_master_volume(if rate == 1.0 {
        f64::from(ui.get_video_volume())
    } else {
        0.0
    });
}
```

This follows the spec's key table. The spec's testing list also says L while paused steps a frame forward; that contradicts its table (L paused plays at 1×), and the table wins.

- [ ] **Step 4: Add the window's side**

In `crates/kuvatin/ui/app.slint`, after `callback video-volume-changed(float);  // 0..1` (line 100):

```slint
    // Frame stepping and the shuttle (gui/video/transport.rs).
    callback video-step(int);              // , and .: one frame back (-1) or on (+1)
    callback video-shuttle(int);           // J, K, L as -1, 0, +1
    // The rate the preview plays at, shown beside Play when it is not 1.
    in property <float> shuttle-rate: 1;
```

After `clip-keys` (ends line 429), before `function video-keys`:

```slint
    // , and . step one frame; J, K and L shuttle. J is not reverse play: the
    // engine has none, so it slows down and then pauses. Its own function for
    // the reason clip-keys is.
    function transport-keys(event: KeyEvent) -> EventResult {
        if (event.modifiers.control || event.modifiers.alt || event.modifiers.meta || root.exporting) { return EventResult.reject; }
        if (event.text == ",") { root.video-step(-1); return EventResult.accept; }
        if (event.text == ".") { root.video-step(1); return EventResult.accept; }
        if (event.text == "j" || event.text == "J") { root.video-shuttle(-1); return EventResult.accept; }
        if (event.text == "k" || event.text == "K") { root.video-shuttle(0); return EventResult.accept; }
        if (event.text == "l" || event.text == "L") { root.video-shuttle(1); return EventResult.accept; }
        return EventResult.reject;
    }
```

In `video-keys`, after the plain-S block from Task 4 and before `return root.clip-keys(event);`:

```slint
        if (root.transport-keys(event) == EventResult.accept) { return EventResult.accept; }
```

In the transport bar, after the play button's `Rectangle` (its `pp-ta := TouchArea` line is 1589 and it closes at 1590), before `// Repeat / loop toggle.`:

```slint
                            // The shuttle's speed (J / K / L), when it is not normal.
                            if root.shuttle-rate != 1 : Text {
                                text: root.shuttle-rate + "×";
                                color: Theme.accent; font-size: 11px; font-weight: 700;
                                vertical-alignment: center;
                            }
```

- [ ] **Step 5: Wire it in**

In `crates/kuvatin/src/gui/video/mod.rs`, in `wire`, after `timeline::wire(ui, st);`:

```rust
    transport::wire(ui, st);
```

Replace the volume handler's inner `if let` (lines 224-226):

```rust
            if let Some(p) = project_slot.borrow().as_ref() {
                // While the shuttle runs faster the sound stays muted; the new
                // level applies when it is back at normal speed.
                if p.rate() == 1.0 {
                    p.set_master_volume(v as f64);
                }
            }
```

In the preview tick, before `ui.set_playhead(pos.as_secs_f32());` (line 367):

```rust
                // Any ordinary seek plays at normal speed again (a scrub, a
                // frame step, the loop back to the start): keep the shuttle
                // readout, and the sound it mutes, in step with the engine.
                transport::sync_shuttle(&ui, project);
```

- [ ] **Step 6: Run the tests and build**

Run: `cargo test -p kuvatin -- transport::tests` and `cargo build -p kuvatin`
Expected: 6 passed; the build is clean.

- [ ] **Step 7: Look at it**

Run: `cargo run -p kuvatin`. Open a video. Press **L**: it plays. **L** again: "2×" beside Play, sound muted; again: 4×, 8×, and a fourth press stays at 8×. **J**: 4×, 2×, 1× (readout gone, sound back), then paused. Paused, **.** and **,** move the playhead one frame on and back; hold **.** through a cut. **K** plays; **K** again pauses. During a shuttle, drag the scrubber: the readout goes back to nothing and the sound returns. Close the window.

- [ ] **Step 8: Commit**

Gates, then:

```bash
git add -A
git commit -m "Comma and period step frames, and J, K and L shuttle" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: Phase 1 in the changelog and the README

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `README.md`

- [ ] **Step 1: Changelog**

Under `## [Unreleased]`, at the end of `### Added`:

```markdown
- **Split at the playhead.** The Split button in the timeline toolbar, or
  **S**, cuts the selected clip in two where the playhead stands; undo puts
  it back as one.
- **Frame stepping and shuttle keys.** **,** and **.** step one frame back
  and on. **L** plays and then speeds up to 2×, 4× and 8×, **J** slows back
  down and then pauses, and **K** pauses. Kuvatin has no reverse play, so
  **J** never plays backwards as it does in some editors. The sound is muted
  while the shuttle runs faster than normal.
- **Zoom a clip past the canvas.** Scale now reaches 400 %, from the
  inspector or by dragging the corner of the box in the preview.
```

- [ ] **Step 2: README**

Replace the paragraph that begins `In Videos mode: **Space** plays and pauses` (lines 57-64) with:

```markdown
In Videos mode: **Space** plays and pauses, **,** and **.** step one frame
back and on, and **J / K / L** shuttle: **L** plays and then speeds up to 2×,
4× and 8×, **J** slows back down and then pauses (Kuvatin does not play
backwards), and **K** pauses. **Left/Right** walk the clips on the timeline,
**Ctrl+Left/Right** slide the selected clip by a tenth of a second,
**Ctrl+Up/Down** move it to another track, **Shift+Left/Right** trim its right
edge, **S** splits it at the playhead, and **Delete** removes it. **Ctrl+S**
saves the project (to the file it came from, or asks the first time) and
**Ctrl+O** opens one. The scrubber and the inspector sliders take focus and
answer to the arrows too. The timeline zooms with the **- / Fit / +**
buttons, **Ctrl+plus / Ctrl+minus / Ctrl+0**, or **Ctrl+wheel** over the
lane; a plain wheel scrolls it.
```

Replace the whole bullet that begins `- **Layered timeline editor**` (three lines) with:

```markdown
- **Layered timeline editor** — drag files straight onto the timeline; slide,
  edge-trim, split at the playhead, and move clips across tracks with magnetic
  snapping; reorder tracks; drop below the last track to create a new one
```

and in the bullet that begins `- **Overlays & transforms**`, change `position, scale,` to `position, scale (up to 400 %),`.

- [ ] **Step 3: Run the whole Phase 1 gate**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p kuvatin -p kuvatin-core
cargo test -p kuvatin-video -- --test-threads=1 sequence:: previews_and_renders_an_image_sequence transport_and_edits_are_inert_while_rendering export_settings_normalize_size_and_fps trim_math_never_goes_negative stale_end_of_stream set_clip_duration_sets_and_clamps_a_still sets_canvas_size slid neighbour confined_to_the_gap already_overlapping discovery_gives_up without_blocking_the_caller document:: survives_being_saved missing_source encoder undo_ removing_a_clip_straight_after_adding_it_does_not_crash removing_a_clip_added_while_playing_does_not_crash removing_two_clips_back_to_back_while_playing_does_not_crash a_zoomed_clip_keeps_its_scale split_ frame_length shuttle_
```

Expected: all clean; `kuvatin` 270 passed, 2 ignored (262 before this plan); the video list 71 passed. This is the release pipeline's self-contained list as it now stands.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "The changelog and README describe split, stepping, the shuttle and zoom" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

**Phase 1 ends here. The branch is shippable.**

---

# Phase 2: clip speed

Speed is a pair of time effects on the clip, `videorate` for the picture and `pitch` for the sound, and one optional `rate` field in `ClipRecord`. The first task measures what GES does with them on your machine. Every later task says which of its steps rests on which measurement, and what to do if yours came out differently.

### Task 9: Measure what this GStreamer does with a speed change

**Files:**
- Modify: `crates/kuvatin-video/src/project.rs` (an ignored test)
- Modify: `docs/superpowers/plans/2026-09-23-editor-clip-edits.md` (this file: the Amendments section)

- [ ] **Step 1: Write the measurement**

Add to the `tests` module in `crates/kuvatin-video/src/project.rs`:

```rust
    /// Not a gate: a measurement of what this GStreamer does with a speed
    /// change, which the speed work was built on (see the Amendments of
    /// `docs/superpowers/plans/2026-09-23-editor-clip-edits.md`). Run it by
    /// hand after a GStreamer upgrade, with `GST_TEST_FILE` naming a video
    /// with sound:
    /// `cargo test -p kuvatin-video -- --ignored --nocapture --test-threads=1 speed_measure_the_runtime`
    #[test]
    #[ignore]
    fn speed_measure_the_runtime() {
        gst::init().expect("gst");
        ges::init().expect("ges");
        println!("M0 {}", gst::version_string());

        for desc in ["videorate", "pitch", "videorate rate=2", "pitch rate=2", "pitch tempo=2"] {
            let e = ges::Effect::new(desc).expect("effect");
            println!("M1 {desc:?}: is_time_effect = {}", e.is_time_effect());
        }
        for (desc, name) in [
            ("videorate", "rate"),
            ("videorate", "GstVideoRate::rate"),
            ("pitch", "rate"),
            ("pitch", "GstPitch::rate"),
            ("pitch", "tempo"),
        ] {
            let e = ges::Effect::new(desc).expect("effect");
            let took = e.register_time_property(name);
            println!(
                "M2 {desc} register_time_property({name:?}) = {took}, is_time_effect = {}",
                e.is_time_effect()
            );
        }

        let (dir, seq_uri) = sequence_fixture("speed-measure", 20, 10);
        let png = dir.join("still.png");
        image::RgbaImage::from_pixel(64, 36, image::Rgba([10, 120, 200, 255]))
            .save(&png)
            .expect("still");
        let mut project = Project::new(|_f| {}).expect("project");
        let seq = project.append_clip_uri(&seq_uri, 0, None).expect("sequence").id;
        let still = project
            .append_clip(&png, 1, Some(Duration::from_secs(4)))
            .expect("still")
            .id;
        let seq_clip = project.clips[&seq.0].clone();
        let still_clip = project.clips[&still.0].clone();
        let limit = |c: &ges::Clip| c.property::<Option<gst::ClockTime>>("duration-limit");

        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            still_clip.duration_limit()
        }));
        println!(
            "M7 still: duration_limit() panics = {}; the property reads {:?}",
            caught.is_err(),
            limit(&still_clip)
        );

        let pitch = ges::Effect::new("pitch rate=2").expect("pitch");
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            seq_clip.add_top_effect(&pitch, -1)
        }));
        let said = match &caught {
            Ok(Ok(())) => "Ok(())".to_string(),
            Ok(Err(e)) => format!("Err({e})"),
            Err(_) => "panicked: GES returned FALSE without a GError".to_string(),
        };
        println!("M5 pitch on a sequence, which has no sound: {said}");

        let video = ges::Effect::new("videorate rate=2").expect("videorate");
        println!(
            "M8 videorate on a still: {:?}",
            still_clip.add_top_effect(&video, -1)
        );

        let Some(path) = std::env::var_os("GST_TEST_FILE") else {
            println!("M3, M4, M6 and M11 need GST_TEST_FILE: a video with sound");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        let a = project
            .append_clip(Path::new(&path), 2, None)
            .expect("media")
            .id;
        let clip = project.clips[&a.0].clone();
        println!("M4 full length {} limit {:?}", clip.duration(), limit(&clip));
        let v = ges::Effect::new("videorate rate=2").expect("videorate");
        let added = clip.add_top_effect(&v, -1);
        println!(
            "M4 videorate=2 on the full-length clip: {added:?}; duration now {} limit {:?}",
            clip.duration(),
            limit(&clip)
        );
        let grew = clip.set_duration(clip.duration() + gst::ClockTime::from_seconds(1));
        println!("M4 growing past the limit: set_duration = {grew}");
        let p = ges::Effect::new("pitch rate=2").expect("pitch");
        println!("M4 pitch=2: {:?}", clip.add_top_effect(&p, -1));
        for e in clip.top_effects() {
            println!(
                "M3 {:?} {:?}: rate = {:?}, tempo = {:?}",
                e.track_type(),
                TimelineElementExt::name(&e),
                TimelineElementExt::child_property(&e, "rate"),
                TimelineElementExt::child_property(&e, "tempo")
            );
        }

        project.set_clip_layout(
            &a,
            Layout {
                posx: 40,
                posy: 0,
                scale: 0.5,
                alpha: 0.5,
                volume: 0.25,
            },
        );
        let right = clip
            .split_full(clip.start().nseconds() + 1_000_000_000)
            .expect("split")
            .expect("a new clip");
        println!(
            "M6 right half: start {} inpoint {} duration {} effects {} posx {:?} width {:?} alpha {:?} volume {:?}",
            right.start(),
            right.inpoint(),
            right.duration(),
            right.top_effects().len(),
            right.child_property("posx"),
            right.child_property("width"),
            right.child_property("alpha"),
            right.child_property("volume")
        );
        println!(
            "M6 split at the clip's own start: {:?}",
            clip.split_full(clip.start().nseconds()).map(|c| c.is_some())
        );

        let b = project
            .append_clip(Path::new(&path), 3, None)
            .expect("media")
            .id;
        let bc = project.clips[&b.0].clone();
        bc.set_duration(gst::ClockTime::from_nseconds(bc.duration().nseconds() / 2));
        let t = ges::Effect::new("pitch tempo=2").expect("tempo");
        println!(
            "M11 pitch tempo=2 on a half-length clip: {:?}, limit {:?}",
            bc.add_top_effect(&t, -1),
            limit(&bc)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
```

It uses `sequence_fixture` from Task 5. It prints and asserts nothing: the gates that hold these facts are the tests of Tasks 10 to 14.

- [ ] **Step 2: Run it**

Build the fixture (Before you start), then run:
`cargo test -p kuvatin-video -- --ignored --nocapture --test-threads=1 speed_measure_the_runtime`
Expected: `1 passed`, and one line per fact. On GStreamer 1.26.11 it printed the facts M1 to M8 and M11 in the table near the top of this plan: every `is_time_effect` true, every `register_time_property` false, `M7 … panics = true`, `M5 … panicked`, `M8 … Ok(())`, `M4 … Ok(()); duration now 0:00:03.482993197`, `set_duration = false`, `M3` rates of `(gdouble) 2` and `(gfloat) 2`, and `M6` with an in-point of 2 s, two effects and the transform copied.

- [ ] **Step 3: Record it**

Paste the printed lines under **Amendments → Task 9 measurement** at the bottom of this plan, with the date and the `M0` version line. For every line that differs from the table, write which contingency below you are taking, then take it in the task named.

| If this differs | Do this |
| --- | --- |
| M1 false for `videorate` or `pitch` | Task 10 Step 5: in `apply_rate`, right after `ges::Effect::new(&description)?`, call `effect.register_time_property(<name>)` with the name M2 reported `true` for, before the `is_time_effect()` check. The check stays. |
| M3: only a qualified name answers | Task 10 Step 5: use that name (`"GstVideoRate::rate"`, `"GstPitch::rate"`) in `clip_rate_of`. The effect descriptions do not change. |
| M3: `pitch` holds a double | Nothing: `clip_rate_of` reads either type, and `snap_rate` rounding is harmless. |
| M4: GES refuses instead of shortening | Nothing: `set_clip_rate` shrinks first either way; its refusal path (`None`, clip left as it was) then gets exercised more. |
| M5: an `Err` instead of a panic | Nothing: the `AudioSource` guard in `apply_rate` stays, since adding `pitch` to a silent clip is wrong either way. |
| M6: effects not copied | Nothing: Task 13 Step 7's repair in `split_clip` covers it; its test proves it. |
| M6: transform not copied | Nothing: Task 3 writes it explicitly. |
| M7: no panic | Nothing: the property read is used anyway. |
| M11: `tempo` not a time effect | Nothing: `PITCH_PROPERTY` stays `"rate"`. |

- [ ] **Step 4: Commit**

Gates, then:

```bash
git add -A
git commit -m "What this GStreamer does with a speed change is measured and written down" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 10: The engine changes a clip's speed

**Depends on Task 9:** Step 5's `apply_rate` rests on M1 (no registration), M3 (the name `"rate"`, and `f64` or `f32`) and M5 (the `AudioSource` guard); `set_clip_rate`'s ordering rests on M4. Step 5 says what changes if yours differed.

**Files:**
- Modify: `crates/kuvatin-video/src/project.rs` (constants and pure helpers after `split_fits`; time-effect helpers after `clip_placed_as`, which ends line 342; `clip_rate` and `set_clip_rate` after `split_clip`; tests)
- Modify: `.github/workflows/release.yml`

- [ ] **Step 1: Write the failing pure tests**

Add to the `tests` module:

```rust
    #[test]
    fn speed_rates_snap_to_what_the_engine_can_hold() {
        for r in [0.25, 0.5, 1.0, 1.5, 2.0, 4.0] {
            assert_eq!(snap_rate(r), r, "every rate the inspector offers is kept exactly");
        }
        assert_eq!(snap_rate(0.1), RATE_MIN);
        assert_eq!(snap_rate(9.0), RATE_MAX);
        assert_eq!(snap_rate(1.1), f64::from(1.1f32), "rounded to what pitch can store");
    }

    #[test]
    fn speed_change_keeps_the_span_of_source() {
        // Four seconds at 1x are two at 2x, and back.
        assert_eq!(rate_change_math(0, 4 * S, 1.0, 2.0, Some(10 * S), None), 2 * S);
        assert_eq!(rate_change_math(0, 2 * S, 2.0, 1.0, Some(10 * S), None), 4 * S);
        // A source with no length has nothing to run out of.
        assert_eq!(rate_change_math(0, 4 * S, 1.0, 0.5, None, None), 8 * S);
        // Slowing down stops at the next clip.
        assert_eq!(
            rate_change_math(0, 2 * S, 1.0, 0.5, Some(10 * S), Some(3 * S)),
            3 * S
        );
        // The minimum holds a very fast clip up...
        assert_eq!(
            rate_change_math(0, 400_000_000, 1.0, 4.0, Some(10 * S), None),
            MIN_TRIM_NS
        );
        // ...but the source wins where they conflict: 0.5 s of source left
        // is 0.125 s at 4x.
        assert_eq!(
            rate_change_math(9_500_000_000, 400_000_000, 1.0, 4.0, Some(10 * S), None),
            125_000_000
        );
    }

    #[test]
    fn speed_room_is_the_gap_before_the_next_clip() {
        // A clip at [2 s, 3 s).
        assert_eq!(room_after(2 * S, S, &[]), None, "nothing after it");
        assert_eq!(room_after(2 * S, S, &[(5 * S, 8 * S)]), Some(3 * S));
        assert_eq!(
            room_after(2 * S, S, &[(0, S), (9 * S, 10 * S), (5 * S, 8 * S)]),
            Some(3 * S),
            "the nearest, in any order; a clip before it does not count"
        );
        assert_eq!(
            room_after(2 * S, S, &[(3 * S, 4 * S)]),
            Some(S),
            "a clip butting up against it leaves no room to grow"
        );
    }
```

The expected values are worked out in the comments: 4 s × 1.0 / 2.0 = 2 s; 0.4 s × 1.0 / 4.0 = 0.1 s, raised to the 0.2 s minimum, under a source cap of 10 s / 4 = 2.5 s; with an in-point of 9.5 s the cap is (10 − 9.5) / 4 = 0.125 s.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p kuvatin-video -- --test-threads=1 speed_`
Expected: FAIL to compile, "cannot find function `snap_rate`".

- [ ] **Step 3: Implement the pure rules**

Add after `split_fits`:

```rust
/// The slowest and the fastest a clip plays (see [`Project::set_clip_rate`]).
pub const RATE_MIN: f64 = 0.25;
pub const RATE_MAX: f64 = 4.0;

/// Which of `pitch`'s properties carries a clip's speed. `rate` changes speed
/// and pitch together, as `videorate` does the picture; `tempo` would keep
/// the pitch. GES knows both as time properties (M1, M11), so choosing the
/// other is this one line.
const PITCH_PROPERTY: &str = "rate";

/// A rate as the engine keeps it: clamped to [`RATE_MIN`, `RATE_MAX`] and
/// rounded to the nearest `f32`. `pitch` stores its rate as a float (M3), and
/// undo compares records exactly, so a rate that did not survive that trip
/// would never read back as the record it came from.
fn snap_rate(rate: f64) -> f64 {
    f64::from(rate.clamp(RATE_MIN, RATE_MAX) as f32)
}

/// How long a clip lasts at `new_rate`, from `dur` at `old_rate`: the same
/// span of source, played at the new rate. Then, as a right-edge trim would
/// be: at least the trim minimum, never more than the source can still supply
/// from `inpoint` at the new rate (which wins over the minimum where they
/// conflict), and never more than `room`, the space before the next clip.
fn rate_change_math(
    inpoint: i128,
    dur: i128,
    old_rate: f64,
    new_rate: f64,
    max_ns: Option<i128>,
    room: Option<i128>,
) -> i128 {
    let mut nd = ((dur as f64 * old_rate / new_rate).round() as i128).max(MIN_TRIM_NS);
    if let Some(m) = max_ns {
        nd = nd.min(((m - inpoint) as f64 / new_rate) as i128);
    }
    if let Some(r) = room {
        nd = nd.min(r);
    }
    nd.max(0)
}

/// The room a clip at `start` lasting `dur` has to grow into: from its start
/// to the nearest neighbour that begins at or after its end. `neighbours` are
/// the other clips on its layer as `(start, end)`. None when nothing follows.
fn room_after(start: i128, dur: i128, neighbours: &[(i128, i128)]) -> Option<i128> {
    let end = start + dur;
    neighbours
        .iter()
        .filter(|&&(n_start, _)| n_start >= end)
        .map(|&(n_start, _)| n_start - start)
        .min()
}
```

- [ ] **Step 4: Write the failing engine tests**

These run on a generated image sequence: it has source time, unlike a still, and needs no media, so they gate in the self-contained step. A sequence has no sound, so it carries one effect; Task 14 covers the pair on real media.

```rust
    #[test]
    fn speed_halves_a_sequence_and_brings_it_back() {
        // Forty frames at 10 fps: four seconds of source.
        let (dir, uri) = sequence_fixture("speed-seq", 40, 10);
        let mut project = Project::new(|_f| {}).expect("project");
        let id = project.append_clip_uri(&uri, 0, None).expect("append").id;
        assert_eq!(project.clip_rate(&id), 1.0);
        let geom = project.set_clip_rate(&id, 2.0).expect("2x");
        assert_eq!(geom.duration, secs(2.0), "the same four seconds, twice as fast");
        assert_eq!(project.clip_rate(&id), 2.0);
        let clip = project.clips[&id.0].clone();
        let effects = time_effects(&clip);
        assert_eq!(effects.len(), 1, "the picture only: a sequence has no sound");
        assert!(effects.iter().all(|e| e.is_time_effect()));
        let back = project.set_clip_rate(&id, 1.0).expect("1x");
        assert_eq!(back.duration, secs(4.0));
        assert!(
            clip.top_effects().is_empty(),
            "normal speed carries no effects at all"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The spec puts this test on a clip sped up; speeding up shortens a
    /// clip, so it is slowing down that runs into a neighbour.
    #[test]
    fn speed_stops_at_the_next_clip() {
        // Twenty frames at 10 fps: two seconds, with a neighbour 1 s after it.
        let (dir, uri) = sequence_fixture("speed-gap", 20, 10);
        let mut project = Project::new(|_f| {}).expect("project");
        let a = project
            .add_clip_uri(&uri, 0, secs(0.0), Duration::ZERO, secs(2.0))
            .expect("a");
        project
            .add_clip_uri(&uri, 0, secs(3.0), Duration::ZERO, secs(2.0))
            .expect("b");
        let geom = project.set_clip_rate(&a, 0.5).expect("0.5x");
        assert_eq!(geom.duration, secs(3.0), "four seconds wanted, three of room");
        assert_eq!(project.clip_rate(&a), 0.5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn speed_is_refused_for_a_still() {
        let (dir, png, mut project) = undo_fixture("speed-still");
        let a = project
            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(2.0))
            .expect("a");
        assert!(project.set_clip_rate(&a, 2.0).is_none());
        assert_eq!(project.clip_rate(&a), 1.0);
        assert!(project.clips[&a.0].top_effects().is_empty());
        assert!(project.set_clip_rate(&a, f64::NAN).is_none());
        assert!(project.set_clip_rate(&ClipId("nope".into()), 2.0).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 5: Implement the engine**

Add after `clip_placed_as` (ends line 342):

```rust
/// A clip's time effects: its speed change, when it has one.
fn time_effects(clip: &ges::Clip) -> Vec<ges::BaseEffect> {
    clip.top_effects()
        .into_iter()
        .filter_map(|e| e.downcast::<ges::BaseEffect>().ok())
        .filter(|e| e.is_time_effect())
        .collect()
}

/// A clip's playback rate: what its time effects play at, or 1.0 when it has
/// none. `videorate` keeps its rate as a double and `pitch` as a float (M3),
/// so either is read.
fn clip_rate_of(clip: &ges::Clip) -> f64 {
    time_effects(clip)
        .iter()
        .find_map(|e| {
            let name = if e.track_type() == ges::TrackType::AUDIO {
                PITCH_PROPERTY
            } else {
                "rate"
            };
            let value = TimelineElementExt::child_property(e, name)?;
            value
                .get::<f64>()
                .ok()
                .or_else(|| value.get::<f32>().ok().map(f64::from))
        })
        .unwrap_or(1.0)
}

/// Replace a clip's time effects with ones playing at `rate`, or with none at
/// 1.0, so a clip at normal speed carries no effects at all. The picture and
/// the sound get one each, but only a stream the clip has: an audio effect on
/// a clip with no sound fails inside GES without an error, which the bindings
/// turn into a panic in a debug build (M5). Refused for a still, which has no
/// source time to stretch.
fn apply_rate(clip: &ges::Clip, rate: f64) -> Result<()> {
    if rate != 1.0
        && clip
            .property::<Option<gst::ClockTime>>("max-duration")
            .is_none()
    {
        anyhow::bail!("a still has no source time to play faster or slower");
    }
    for effect in time_effects(clip) {
        clip.remove_top_effect(&effect)?;
    }
    if rate == 1.0 {
        return Ok(());
    }
    let has = |kind: gst::glib::Type| {
        clip.find_track_element(None::<&ges::Track>, kind)
            .is_some()
    };
    let mut wanted = Vec::new();
    if has(ges::VideoSource::static_type()) {
        wanted.push(format!("videorate rate={rate}"));
    }
    if has(ges::AudioSource::static_type()) {
        wanted.push(format!("pitch {PITCH_PROPERTY}={rate}"));
    }
    for description in wanted {
        let effect = ges::Effect::new(&description)?;
        // A runtime that does not re-time the clip for this effect would give
        // a clip whose length no longer matches its sound: stop here instead.
        if !effect.is_time_effect() {
            anyhow::bail!("this GStreamer does not treat {description:?} as a speed change");
        }
        clip.add_top_effect(&effect, -1)?;
    }
    Ok(())
}
```

If Task 9 differed on M1, add the registration its contingency names right after `ges::Effect::new(&description)?`. If it differed on M3's names, use the qualified names it reported in `clip_rate_of`.

In `impl Project`, after `split_clip`:

```rust
    /// The clip's playback rate: what its time effects play at, or 1.0 when
    /// it has none or is not on the timeline.
    pub fn clip_rate(&self, id: &ClipId) -> f64 {
        self.clips.get(&id.0).map(clip_rate_of).unwrap_or(1.0)
    }

    /// Set the clip's playback rate, clamped to [`RATE_MIN`, `RATE_MAX`]. The
    /// clip keeps its start and in-point; its duration becomes the same span
    /// of source at the new rate, clamped to the trim minimum, to what the
    /// source can still supply, and to the gap before the next clip on its
    /// track (see [`rate_change_math`]). Returns the resulting geometry;
    /// None for an unknown clip, a still, a rate that is not a number, or a
    /// change GES refused, which leaves the clip as it was.
    pub fn set_clip_rate(&mut self, id: &ClipId, rate: f64) -> Option<ClipGeom> {
        if self.rendering.get() || !rate.is_finite() {
            return None;
        }
        let clip = self.clips.get(&id.0)?.clone();
        // A still has no source length and no source time to stretch.
        let max_ns = clip
            .property::<Option<gst::ClockTime>>("max-duration")?
            .nseconds() as i128;
        let rate = snap_rate(rate);
        let old_rate = clip_rate_of(&clip);
        if rate == old_rate {
            return Some(clip_geom(&clip));
        }
        let start = clip.start().nseconds() as i128;
        let dur = clip.duration().nseconds() as i128;
        let room = room_after(start, dur, &self.layer_neighbours(id));
        let nd = rate_change_math(
            clip.inpoint().nseconds() as i128,
            dur,
            old_rate,
            rate,
            Some(max_ns),
            room,
        );
        let new_dur = gst::ClockTime::from_nseconds(nd as u64);
        let old_dur = clip.duration();
        let set_len = |d: gst::ClockTime| clip.duration() == d || clip.set_duration(d);
        // Whichever change lowers the clip's duration-limit goes first, the
        // way `trim_clip` orders in-point and duration. Faster: the
        // shorter duration, then the effects. Slower: the effects, then the
        // longer duration, capped at the limit GES now reports, so a
        // nanosecond of rounding cannot get it refused.
        let applied = if rate > old_rate {
            set_len(new_dur) && apply_rate(&clip, rate).is_ok()
        } else {
            apply_rate(&clip, rate).is_ok() && {
                let limit = clip.property::<Option<gst::ClockTime>>("duration-limit");
                set_len(limit.map_or(new_dur, |l| new_dur.min(l)))
            }
        };
        if !applied || clip_rate_of(&clip) != rate {
            // Back as it was: the old effects, then the old length.
            let _ = apply_rate(&clip, old_rate);
            set_len(old_dur);
            self.commit();
            return None;
        }
        self.commit();
        self.touched();
        Some(clip_geom(&clip))
    }
```

- [ ] **Step 6: Run them and watch them pass**

Run: `cargo test -p kuvatin-video -- --test-threads=1 speed_`
Expected: 6 passed, 1 ignored (`speed_measure_the_runtime`).

- [ ] **Step 7: Gate them in CI**

Append ` speed_` to the last line of the self-contained list in `.github/workflows/release.yml`, then run the YAML check. The ignored measurement matches the filter too and stays ignored.

- [ ] **Step 8: Commit**

Gates, plus `cargo test -p kuvatin-video -- --test-threads=1 speed_ split_ undo_`, then:

```bash
git add -A
git commit -m "The engine plays a clip faster or slower with GES time effects" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 11: Trims follow the speed

With a time effect, one second of timeline is `rate` seconds of source; both trim helpers assumed 1:1.

**Depends on Task 9:** the engine test's cross-check asks GES for its duration-limit, which M4 and M7 describe. If M7 showed no panic, nothing changes: the property read is used either way.

**Files:**
- Modify: `crates/kuvatin-video/src/project.rs` (`trim_left_math` at 263-274, `trim_right_math` at 276-285, `trim_clip` at 1078-1119, `trim_math_never_goes_negative` at 2049-2077, a new test)
- Modify: `.github/workflows/release.yml`

- [ ] **Step 1: Write the failing tests**

In `trim_math_never_goes_negative`, add `, 1.0` as the last argument of each of its five calls (three `trim_left_math`, two `trim_right_math`), since nothing about them changes at normal speed.

Add:

```rust
    #[test]
    fn trim_math_follows_the_rate() {
        // Right edge at 2x: a 10 s source from its head fills 5 s of timeline.
        assert_eq!(trim_right_math(0, 2 * S, 20 * S, Some(10 * S), 2.0), 5 * S);
        // At 0.5x the same source fills 20 s.
        assert_eq!(trim_right_math(0, 2 * S, 30 * S, Some(10 * S), 0.5), 20 * S);
        // The source still wins over the minimum: 0.1 s of source left is
        // 0.05 s at 2x.
        assert_eq!(
            trim_right_math(9_900_000_000, 50_000_000, -S, Some(10 * S), 2.0),
            50_000_000
        );
        // Left edge at 2x: the in-point moves twice as far as the start.
        assert_eq!(trim_left_math(2 * S, S, 5 * S, S, 2.0), (3 * S, 3 * S, 4 * S));
        // Out past the source origin: it stops where the in-point reaches
        // zero, half a second of timeline for one second of source.
        assert_eq!(
            trim_left_math(3 * S, S, 5 * S, -2 * S, 2.0),
            (3 * S - S / 2, 0, 5 * S + S / 2)
        );
        // Never negative at any rate.
        let (ns, ni, nd) = trim_left_math(0, 0, 100_000_000, 50_000_000, 2.0);
        assert!(ns >= 0 && ni >= 0 && nd >= 0, "wrapped: {ns} {ni} {nd}");
        // At 1x nothing changed.
        assert_eq!(trim_left_math(2 * S, S, 5 * S, S, 1.0), (3 * S, 2 * S, 4 * S));
    }
```

Worked: `trim_left_math(3 s, 1 s, 5 s, −2 s, 2.0)`: the delta is held at −(1 s / 2) = −0.5 s, so the start is 2.5 s, the in-point 1 s − 0.5 s × 2 = 0, the duration 5.5 s.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p kuvatin-video -- --test-threads=1 trim_math`
Expected: FAIL to compile, "this function takes 4 arguments but 5 arguments were supplied".

- [ ] **Step 3: Give both helpers the rate**

Replace `trim_left_math` and `trim_right_math` with their doc comments (lines 263-285):

```rust
/// Left-edge trim math: the clip's END stays fixed; start moves by the
/// (clamped) delta, the in-point by `rate` times as much (one second of
/// timeline is `rate` seconds of source), and the duration shrinks or grows
/// to match. All results are guaranteed non-negative — the previous version
/// applied the min-duration override AFTER the >=0 clamps, so a clip already
/// shorter than the minimum could push inpoint/start negative and wrap
/// through the u64 cast into a ~10^5-hour ClockTime. Precedence here:
/// never-negative beats min-duration. The in-point is rounded down, so the
/// source position of the fixed end never moves past where it was.
fn trim_left_math(
    start: i128,
    inpoint: i128,
    dur: i128,
    delta: i128,
    rate: f64,
) -> (i128, i128, i128) {
    let mut d = delta;
    d = d.min(dur - MIN_TRIM_NS); // keep at least the minimum (may go negative)
    // Never before the source origin (in timeline time), nor the timeline's.
    d = d.max(-((inpoint as f64 / rate) as i128)).max(-start);
    let di = (d as f64 * rate).floor() as i128;
    ((start + d).max(0), (inpoint + di).max(0), (dur - d).max(0))
}

/// Right-edge trim math: only the duration changes. Clamped to the minimum,
/// then capped by what the source can still supply from `inpoint` at `rate`
/// (which wins over the minimum when the two conflict, e.g. `inpoint` near
/// the end of the media), floored at 0.
fn trim_right_math(
    inpoint: i128,
    dur: i128,
    delta: i128,
    max_ns: Option<i128>,
    rate: f64,
) -> i128 {
    let mut nd = (dur + delta).max(MIN_TRIM_NS);
    if let Some(m) = max_ns {
        nd = nd.min(((m - inpoint) as f64 / rate) as i128);
    }
    nd.max(0)
}
```

The spec truncates the in-point move (`as i128`). For a leftward extension that rounds toward zero, which puts the in-point a fraction of a nanosecond late and the fixed end past what the source has; GES would then refuse the duration. `floor` never does.

In `trim_clip`, after the `let delta = (delta_secs * 1e9) as i128;` line:

```rust
        let rate = clip_rate_of(&clip);
```

and add `, rate` to its two calls, so they read `trim_left_math(start, inpoint, dur, delta, rate)` and `trim_right_math(inpoint, dur, delta, max_ns, rate)`.

- [ ] **Step 4: Run them and watch them pass**

Run: `cargo test -p kuvatin-video -- --test-threads=1 trim_math`
Expected: 2 passed.

- [ ] **Step 5: Write the engine test**

```rust
    #[test]
    fn speed_trims_a_slowed_clip_from_both_edges() {
        // Four seconds of source at 0.5x: eight on the timeline, from 2 s.
        let (dir, uri) = sequence_fixture("speed-trim", 40, 10);
        let mut project = Project::new(|_f| {}).expect("project");
        let a = project
            .add_clip_uri(&uri, 0, secs(2.0), Duration::ZERO, secs(4.0))
            .expect("a");
        project.set_clip_rate(&a, 0.5).expect("0.5x");
        // In by a second of timeline: half a second of source.
        let g = project.trim_clip(&a, -1, 1.0).expect("left in");
        assert_eq!((g.start, g.inpoint, g.duration), (secs(3.0), secs(0.5), secs(7.0)));
        // Out as far as it goes: 3.5 s of source left is 7 s at 0.5x.
        let g = project.trim_clip(&a, 1, 30.0).expect("right out");
        assert_eq!(g.duration, secs(7.0), "the source is used up exactly");
        // Back out past the start of the source: it stops there.
        let g = project.trim_clip(&a, -1, -5.0).expect("left out");
        assert_eq!((g.start, g.inpoint, g.duration), (secs(2.0), Duration::ZERO, secs(8.0)));
        // The pure maths and GES agree on how long the clip may be.
        let clip = project.clips[&a.0].clone();
        let limit = clip
            .property::<Option<gst::ClockTime>>("duration-limit")
            .expect("a sequence has a length");
        let max = clip
            .property::<Option<gst::ClockTime>>("max-duration")
            .expect("a sequence has a length");
        let ours = trim_right_math(
            clip.inpoint().nseconds() as i128,
            clip.duration().nseconds() as i128,
            60 * S,
            Some(max.nseconds() as i128),
            0.5,
        );
        assert_eq!(ours, limit.nseconds() as i128);
        let _ = std::fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 6: Run it**

Run: `cargo test -p kuvatin-video -- --test-threads=1 speed_trims`
Expected: 1 passed. It passes as soon as Step 3 is in, because `trim_clip` is the code under test; if you wrote it before Step 3 it would fail on the first assertion with an in-point of 1 s.

- [ ] **Step 7: Gate it in CI**

Append ` trim_math_follows_the_rate` to the last line of the self-contained list (the engine test is covered by `speed_`), then run the YAML check.

- [ ] **Step 8: Commit**

Gates, plus `cargo test -p kuvatin-video -- --test-threads=1 trim_math speed_ undo_ set_clip_duration_sets_and_clamps_a_still`, then:

```bash
git add -A
git commit -m "Trimming a clip follows its speed" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 12: The saved clip carries its speed

**Files:**
- Modify: `crates/kuvatin-video/src/document.rs` (`ClipRecord` at 58-77; tests)
- Modify: `crates/kuvatin-video/src/project.rs` (`clip_records` at 1457-1481)
- Modify: `crates/kuvatin/src/gui/video/undo.rs` (the test helper `rec` at 678-695)

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module of `crates/kuvatin-video/src/document.rs`:

```rust
    /// A project that never changes a speed is written exactly as before.
    #[test]
    fn a_clip_at_normal_speed_writes_no_rate_and_reads_back_at_one() {
        let text = toml::to_string_pretty(&sample()).unwrap();
        assert!(
            !text.lines().any(|l| l.trim_start().starts_with("rate =")),
            "{text}"
        );
        let back: ProjectFile = toml::from_str(&text).unwrap();
        assert!(back.clips.iter().all(|c| c.rate == 1.0));
    }

    #[test]
    fn a_clip_at_another_speed_keeps_it_and_the_format_stays_one() {
        let mut doc = sample();
        doc.clips[0].rate = 2.0;
        let text = toml::to_string_pretty(&doc).unwrap();
        assert!(text.lines().any(|l| l.trim() == "rate = 2.0"), "{text}");
        assert!(text.contains("version = 1"), "{text}");
        let back: ProjectFile = toml::from_str(&text).unwrap();
        assert_eq!(back, doc);
    }

    /// What 2.12 wrote: no rate anywhere.
    #[test]
    fn a_file_written_before_speed_existed_still_opens() {
        let text = r#"version = 1
canvas_w = 1280
canvas_h = 720

[[clips]]
uri = "file:///C:/shots/take1.mp4"
name = "take1.mp4"
track = 0
start = 0.0
inpoint = 1.5
duration = 4.25

[clips.layout]
posx = 12
posy = -4
scale = 0.75
alpha = 0.5
volume = 1.0
"#;
        let doc: ProjectFile = toml::from_str(text).unwrap();
        assert_eq!(doc.version, 1);
        assert_eq!(doc.clips[0].rate, 1.0);
        assert_eq!(doc.clips[0].duration, 4.25);
    }
```

The first test looks for a line starting `rate =`, because the sample's sequence URI contains `framerate=24/1`.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p kuvatin-video -- --test-threads=1 document::`
Expected: FAIL to compile, "no field `rate` on type `ClipRecord`".

- [ ] **Step 3: Add the field**

In `ClipRecord`, between `pub duration: f64,` and `pub layout: LayoutRecord,` (it goes before the tables, `layout` and `sequence`, so the TOML writer never has to put a value after a table):

```rust
    /// Playback rate: 1.0 is normal, 2.0 twice as fast. Optional, and left
    /// out of the file at 1.0, so a project that never changes a clip's speed
    /// is written exactly as before and every file from 2.12 and earlier
    /// still loads. `#[serde(default)]` alone would read a missing one as 0.
    #[serde(default = "unit_rate", skip_serializing_if = "is_unit_rate")]
    pub rate: f64,
```

After the struct:

```rust
fn unit_rate() -> f64 {
    1.0
}

fn is_unit_rate(rate: &f64) -> bool {
    *rate == 1.0
}
```

In the test helper `sample()`, add `rate: 1.0,` after `duration: 4.25,` and after `duration: 2.0,`.

- [ ] **Step 4: Fill it everywhere a record is made**

In `crates/kuvatin-video/src/project.rs`, in `clip_records`, after `duration: secs(clip.duration()),`:

```rust
                        rate: clip_rate_of(clip),
```

In `crates/kuvatin/src/gui/video/undo.rs`, in the test helper `rec`, after `duration,`:

```rust
            rate: 1.0,
```

- [ ] **Step 5: Run them and watch them pass**

Run: `cargo test -p kuvatin-video -- --test-threads=1 document::` and `cargo test -p kuvatin`
Expected: 8 document tests pass; the kuvatin suite is clean.

- [ ] **Step 6: Commit**

Gates, plus `cargo test -p kuvatin-video -- --test-threads=1 document:: survives_being_saved missing_source undo_`, then:

```bash
git add -A
git commit -m "A saved clip records its speed, and only when it is not normal" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 13: Undo, restore, load and split keep the speed

**Depends on Task 9:** the restore and load order rests on M4 (a duration past the limit is refused, so a slow clip must go on short and grow); Step 7's repair rests on M6 and is there in case a runtime does not copy the effects.

**Files:**
- Modify: `crates/kuvatin-video/src/project.rs` (`clip_placed_as` at 335-342; `set_clip_times` at 344-361, replaced; `set_clip_records` at 1133-1239; `restore_clip` at 1241-1269; `apply_document` at 1499-1543; `split_clip`; a new `put_rate_back`; tests)

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn undo_writes_a_speed_change_back_both_ways() {
        let (dir, uri) = sequence_fixture("undo-speed", 40, 10);
        let mut project = Project::new(|_f| {}).expect("project");
        let a = project.append_clip_uri(&uri, 0, None).expect("append").id;
        let normal = record_of(&project, &a);
        project.set_clip_rate(&a, 2.0).expect("2x");
        let fast = record_of(&project, &a);
        assert_eq!((fast.rate, fast.duration), (2.0, 2.0));
        assert!(write_back(&mut project, &[(&a, &normal)]), "undo: slower, longer");
        assert_same_record(&record_of(&project, &a), &normal);
        assert!(write_back(&mut project, &[(&a, &fast)]), "redo: faster, shorter");
        assert_same_record(&record_of(&project, &a), &fast);
        // A slow clip is longer than its source: the other order entirely.
        project.set_clip_rate(&a, 0.5).expect("0.5x");
        let slow = record_of(&project, &a);
        assert_eq!((slow.rate, slow.duration), (0.5, 8.0));
        assert!(write_back(&mut project, &[(&a, &fast)]));
        assert_same_record(&record_of(&project, &a), &fast);
        assert!(write_back(&mut project, &[(&a, &slow)]));
        assert_same_record(&record_of(&project, &a), &slow);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_restores_a_slowed_clip_longer_than_its_source() {
        // Two seconds of source at 0.5x: four on the timeline.
        let (dir, uri) = sequence_fixture("undo-restore-speed", 20, 10);
        let mut project = Project::new(|_f| {}).expect("project");
        let a = project.append_clip_uri(&uri, 0, None).expect("append").id;
        project.set_clip_rate(&a, 0.5).expect("0.5x");
        let before = record_of(&project, &a);
        assert_eq!(before.duration, 4.0, "twice as long as the source");
        assert!(project.remove_clip(&a));
        project.restore_clip(&a, &before).expect("restore");
        assert_same_record(&record_of(&project, &a), &before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn speed_survives_a_save_and_a_load() {
        let (dir, uri) = sequence_fixture("speed-save", 20, 10);
        let file = dir.join("cut.kuvatin");
        let mut project = Project::new(|_f| {}).expect("project");
        let slow = project.append_clip_uri(&uri, 0, None).expect("slow").id;
        project.set_clip_rate(&slow, 0.5).expect("0.5x");
        let fast = project
            .add_clip_uri(&uri, 1, secs(0.0), Duration::ZERO, secs(2.0))
            .expect("fast");
        project.set_clip_rate(&fast, 4.0).expect("4x");
        project.to_document().save(&file).expect("save");
        let mut reopened = Project::new(|_f| {}).expect("project");
        let doc = crate::document::ProjectFile::load(&file).expect("load");
        reopened.apply_document(&doc).expect("apply");
        let got: Vec<(f64, f64)> = reopened
            .to_document()
            .clips
            .iter()
            .map(|c| (c.rate, c.duration))
            .collect();
        assert_eq!(got, vec![(0.5, 4.0), (4.0, 0.5)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn split_keeps_the_speed_on_both_halves() {
        // Four seconds of source at 2x: two on the timeline.
        let (dir, uri) = sequence_fixture("split-speed", 40, 10);
        let mut project = Project::new(|_f| {}).expect("project");
        let a = project.append_clip_uri(&uri, 0, None).expect("append").id;
        project.set_clip_rate(&a, 2.0).expect("2x");
        let (b, left, right) = project.split_clip(&a, secs(0.5)).expect("split");
        assert_eq!((project.clip_rate(&a), project.clip_rate(&b)), (2.0, 2.0));
        assert_eq!(right.inpoint, secs(1.0), "half a second at 2x is a second of source");
        assert_eq!(left.duration + right.duration, secs(2.0));
        assert_eq!(
            time_effects(&project.clips[&b.0]).len(),
            1,
            "one effect on the new half, not two"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
```

Worked: forty frames at 10 fps is 4 s of source; at 2× that is 2 s and at 0.5× 8 s. The 4× clip of 2 s of source is 0.5 s, over the 0.2 s minimum. `to_document` orders by track, so the slow clip on track 0 comes first.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p kuvatin-video -- --test-threads=1 undo_writes_a_speed undo_restores_a_slowed speed_survives split_keeps`
Expected: `undo_writes_a_speed_change_back_both_ways` fails at "undo: slower, longer" (the write ignores the rate, so the clip keeps its effect and the longer duration is refused), `undo_restores_a_slowed_clip_longer_than_its_source` and `speed_survives_a_save_and_a_load` fail on the rate or the duration. `split_keeps_the_speed_on_both_halves` already passes on a runtime that copies effects (M6); it stays as the pin.

- [ ] **Step 3: Compare the speed when reading a clip back**

Replace `clip_placed_as` (lines 335-342):

```rust
/// Whether a clip already sits where `record` puts it: its times to the
/// nanosecond, its track, and its speed.
fn clip_placed_as(clip: &ges::Clip, record: &crate::document::ClipRecord) -> bool {
    clip.start() == clock_time(record.start)
        && clip.inpoint() == clock_time(record.inpoint)
        && clip.duration() == clock_time(record.duration)
        && clip.layer().map(|l| l.priority() as usize) == Some(record.track)
        && clip_rate_of(clip) == record.rate
}
```

- [ ] **Step 4: Write times and speed in a safe order**

Replace `set_clip_times` and its doc comment (lines 344-361):

```rust
/// Set a clip's times and speed so that, at every step on the way, the source
/// is never asked for more than it has, which GES refuses: the duration goes
/// down first if it is going down at all; then the speed and the in-point,
/// whichever lowers the source used first; then the final duration; then the
/// start. With the speed unchanged this is the order `trim_clip` uses.
fn set_clip_placement(
    clip: &ges::Clip,
    start: gst::ClockTime,
    inpoint: gst::ClockTime,
    duration: gst::ClockTime,
    rate: f64,
) {
    let current = clip_rate_of(clip);
    clip.set_duration(duration.min(clip.duration()));
    if rate < current {
        let _ = apply_rate(clip, rate);
        clip.set_inpoint(inpoint);
    } else {
        clip.set_inpoint(inpoint);
        if rate != current {
            let _ = apply_rate(clip, rate);
        }
    }
    clip.set_duration(duration);
    clip.set_start(start);
}
```

A refusal shows in the read-back, as it always has.

In `set_clip_records`, replace the `origins` binding:

```rust
        let origins: Vec<_> = moving
            .iter()
            .map(|(_, _, clip)| {
                (
                    clip.layer(),
                    clip.start(),
                    clip.inpoint(),
                    clip.duration(),
                    clip_rate_of(clip),
                )
            })
            .collect();
```

replace the `set_clip_times(…)` call in the loop over `&moving`:

```rust
            set_clip_placement(
                clip,
                clock_time(record.start),
                clock_time(record.inpoint),
                clock_time(record.duration),
                record.rate,
            );
```

and replace the rollback loop's header and its `set_clip_times` call:

```rust
        for (k, ((_, record, clip), (layer, start, inpoint, duration, rate))) in
            moving.iter().zip(origins).enumerate()
        {
            if clip_placed_as(clip, record) {
                continue;
            }
            let _ = clip.move_to_layer(&self.layers[parking + k]);
            set_clip_placement(clip, start, inpoint, duration, rate);
```

In the doc comment of `set_clip_records`, change "start, in-point, duration, track and transform" to "start, in-point, duration, speed, track and transform".

- [ ] **Step 5: Put the speed back when a clip is restored or loaded**

Add to `impl Project`, after `restore_clip`:

```rust
    /// The last step of restoring or loading a clip whose speed was changed:
    /// its time effects go on, then it takes its length, which at a slow
    /// speed only fits once they are on (M4). A refusal shows in the
    /// read-back, as any other write's does.
    fn put_rate_back(&mut self, id: &ClipId, rate: f64, duration: f64) {
        let Some(clip) = self.clips.get(&id.0).cloned() else {
            return;
        };
        let length = clock_time(duration);
        if apply_rate(&clip, rate).is_ok() && clip.duration() != length {
            clip.set_duration(length);
        }
        self.commit();
    }
```

In `restore_clip`, replace from `clip.set_duration(clock_time(record.duration));` to `self.set_clip_layout(id, record.layout.into());` with:

```rust
        // Slower than normal, a clip can be longer than its source lasts at
        // 1×, and it goes onto the layer at 1×: at the length the source
        // allows, until its speed is back on.
        let mut length = clock_time(record.duration);
        if record.rate < 1.0 {
            if let Some(max) = clip.max_duration() {
                length = length.min(max.saturating_sub(clock_time(record.inpoint)));
            }
        }
        clip.set_duration(length);
        self.layer(record.track).add_clip(&clip)?;
        self.commit();
        self.clips.insert(id.0.clone(), clip.upcast());
        self.set_clip_layout(id, record.layout.into());
        if record.rate != 1.0 {
            self.put_rate_back(id, record.rate, record.duration);
        }
```

In `apply_document`, replace the part of the loop from `if ges::UriClipAsset::request_sync(&rec.uri).is_err() {` to the end of the `match placed { … }`:

```rust
            let asset = match ges::UriClipAsset::request_sync(&rec.uri) {
                Ok(asset) => asset,
                Err(_) => {
                    missing.push(name());
                    continue;
                }
            };
            // Slower than normal, a clip can be longer than its source lasts
            // at 1×: it goes on at the length the source allows, and takes
            // the rest once its speed is back on.
            let mut length = Duration::from_secs_f64(rec.duration.max(0.0));
            if rec.rate < 1.0 {
                if let Some(source) = asset.duration() {
                    let fits = source.saturating_sub(clock_time(rec.inpoint)).nseconds();
                    length = length.min(Duration::from_nanos(fits));
                }
            }
            let placed = self.add_clip_uri(
                &rec.uri,
                rec.track,
                Duration::from_secs_f64(rec.start.max(0.0)),
                Duration::from_secs_f64(rec.inpoint.max(0.0)),
                length,
            );
            match placed {
                Ok(id) => {
                    self.set_clip_layout(&id, rec.layout.into());
                    if rec.rate != 1.0 {
                        self.put_rate_back(&id, rec.rate, rec.duration);
                    }
                }
                Err(_) => missing.push(name()),
            }
```

- [ ] **Step 6: Run them**

Run: `cargo test -p kuvatin-video -- --test-threads=1 undo_writes_a_speed undo_restores_a_slowed speed_survives split_keeps`
Expected: 4 passed.

- [ ] **Step 7: Make split independent of M6**

In `split_clip`, before `self.touched();`:

```rust
        // GES copies the time effects to the new half and translates its
        // in-point through them (M6). Were a runtime not to, the right half
        // would play at normal speed and run past its source.
        let rate = clip_rate_of(&clip);
        if clip_rate_of(&right) != rate {
            let _ = apply_rate(&right, rate);
        }
```

- [ ] **Step 8: Run every undo, speed and split test**

Run: `cargo test -p kuvatin-video -- --test-threads=1 undo_ speed_ split_ document:: survives_being_saved missing_source`
Expected: all pass, 1 ignored. No gate-list change: `undo_`, `speed_` and `split_` cover the new tests.

- [ ] **Step 9: Commit**

Gates, then:

```bash
git add -A
git commit -m "Undo, restore, load and split all keep a clip's speed" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 14: Real media gets picture and sound effects

**Depends on Task 9:** M1 and M5. If your M5 came out as an `Err`, the test is unchanged.

**Files:**
- Modify: `crates/kuvatin-video/src/project.rs` (a test)
- Modify: `.github/workflows/release.yml` (the live-media step)

- [ ] **Step 1: Write the test**

```rust
    /// Real media has picture and sound, so a speed change is two time
    /// effects, and both must be ones GES re-times the clip for. Self-skips
    /// without `GST_TEST_FILE`.
    #[test]
    fn a_sped_up_video_changes_picture_and_sound() {
        let Some(path) = std::env::var_os("GST_TEST_FILE") else {
            eprintln!("skipping a_sped_up_video_changes_picture_and_sound: set GST_TEST_FILE");
            return;
        };
        let mut project = Project::new(|_f| {}).expect("project");
        let a = project
            .append_clip(Path::new(&path), 0, None)
            .expect("clip")
            .id;
        let full = record_of(&project, &a);
        let geom = project.set_clip_rate(&a, 2.0).expect("2x");
        let effects = time_effects(&project.clips[&a.0]);
        let kinds: Vec<ges::TrackType> = effects.iter().map(|e| e.track_type()).collect();
        assert_eq!(effects.len(), 2, "{kinds:?}");
        assert!(
            kinds.contains(&ges::TrackType::VIDEO) && kinds.contains(&ges::TrackType::AUDIO),
            "{kinds:?}"
        );
        assert!((geom.duration.as_secs_f64() - full.duration / 2.0).abs() < 1e-6);
        // Pulled right as far as it goes: the source runs out twice as fast.
        let long = project.trim_clip(&a, 1, 60.0).expect("trim");
        assert!((long.duration.as_secs_f64() - full.duration / 2.0).abs() < 1e-6);
        assert!(write_back(&mut project, &[(&a, &full)]), "undo the speed");
        assert_same_record(&record_of(&project, &a), &full);
        assert!(project.clips[&a.0].top_effects().is_empty());
    }
```

On the fixture, `full.duration` is 6.965986394 s and half of it, 3.482993197 s, is exactly the limit GES reported (M4).

- [ ] **Step 2: Run it**

With `GST_TEST_FILE` set (Before you start):
Run: `cargo test -p kuvatin-video -- --test-threads=1 --exact project::tests::a_sped_up_video_changes_picture_and_sound`
Expected: 1 passed. Without the variable it passes by skipping, and says so on stderr: run it with the variable.

- [ ] **Step 3: Gate it in CI**

In `.github/workflows/release.yml`, step `Test (live-media regressions — gates the release)`, replace the `$names` array and the `cargo test` line with its list (lines 294-301):

```powershell
          $names = @("renders_after_preview_eos", "renders_gapped_overlay_timeline",
            "undo_writes_a_left_trim_of_real_media_back", "undo_restores_a_trimmed_clip_of_real_media",
            "a_sped_up_video_changes_picture_and_sound")
          foreach ($attempt in 1..2) {
            cargo test -p kuvatin-video --release -- --test-threads=1 --exact `
              project::tests::renders_after_preview_eos `
              project::tests::renders_gapped_overlay_timeline `
              project::tests::undo_writes_a_left_trim_of_real_media_back `
              project::tests::undo_restores_a_trimmed_clip_of_real_media `
              project::tests::a_sped_up_video_changes_picture_and_sound
```

In the comment above the step, after the sentence ending "a trimmed clip restored with its in-point.", add: "One more proves a speed change on real media is two time effects, picture and sound."

Run the YAML check.

- [ ] **Step 4: Commit**

Gates, then:

```bash
git add -A
git commit -m "A sped-up video changes its picture and its sound, proven on real media" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 15: The timeline row carries the speed

The row needs the rate so the clip block can say so, and Phase 3's waveform window depends on it. `place` must write it from the record, or a sped-up clip's row goes stale after every undo.

**Files:**
- Modify: `crates/kuvatin/ui/app.slint` (`TimelineClip` at 25-35; the clip name at 1942; the clip's accessible label at 1886-1888)
- Modify: `crates/kuvatin/src/gui/video/undo.rs` (`place` at 291-297; the test helper `row` at 707-719; a test)
- Modify: `crates/kuvatin/src/gui/video/mod.rs` (the rows in `add_to_timeline` at 511-525 and `add_sequence_to_timeline` at 575-585)
- Modify: `crates/kuvatin/src/gui/video/project_file.rs` (the rows in `restore_models` at 214-224)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module of `crates/kuvatin/src/gui/video/undo.rs`:

```rust
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
            plan(&step(StepKind::Move, "a", &c0, &c1), Direction::Undo),
            Plan {
                removes: vec![],
                writes: vec![("a".into(), normal)],
                restores: vec![],
            }
        );
    }
```

(`StepKind::Move` stands in until Task 16 adds `StepKind::Speed`; `plan` does not look at the kind.)

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p kuvatin -- undo::tests`
Expected: FAIL to compile, "no field `rate` on type `TimelineClip`".

- [ ] **Step 3: Add the field**

In `crates/kuvatin/ui/app.slint`, in `struct TimelineClip`, after `thumb: image,`:

```slint
    rate: float,       // playback rate: 1 unless the clip's speed was changed
```

In `place` in `crates/kuvatin/src/gui/video/undo.rs`, after `row.inpoint = record.inpoint as f32;`:

```rust
    row.rate = record.rate as f32;
```

In the test helper `row`, after `thumb: Image::default(),`:

```rust
            rate: r.rate as f32,
```

In `crates/kuvatin/src/gui/video/mod.rs`, in both `TimelineClip { … }` literals (in `add_to_timeline` and `add_sequence_to_timeline`), after `thumb,`:

```rust
                rate: 1.0,
```

In `crates/kuvatin/src/gui/video/project_file.rs`, in the `TimelineClip { … }` literal in `restore_models`, after `thumb: Image::default(),`:

```rust
            rate: rec.rate as f32,
```

- [ ] **Step 4: Show it on the clip**

In `crates/kuvatin/ui/app.slint`, replace the clip-name `Text` inside `vis` (line 1942):

```slint
                                            Text { text: clip.rate > 0 && clip.rate != 1 ? clip.name + "  " + clip.rate + "×" : clip.name; color: white; font-size: 9px; font-weight: 700; x: 9px; y: (parent.height - self.height) / 2; }
```

In the clip's `accessible-label` (lines 1886-1888), replace its last line, `+ Math.round(clip.duration * 10) / 10 + " s long";`, with:

```slint
                                            + Math.round(clip.duration * 10) / 10 + " s long"
                                            + (clip.rate > 0 && clip.rate != 1 ? ", plays at " + clip.rate + "×" : "");
```

Edit these with the Edit tool: the first line of that label holds a `\u{2014}` escape.

- [ ] **Step 5: Run the suite and build**

Run: `cargo test -p kuvatin` and `cargo build -p kuvatin`
Expected: clean; the two new tests pass.

- [ ] **Step 6: Commit**

Gates, then:

```bash
git add -A
git commit -m "A timeline row carries its clip's speed, and undo keeps it right" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 16: Speed in the inspector

**Depends on Task 9:** the refusal message covers M1 or M5 going wrong on a user's runtime (a speed GES will not take): the clip is left as it was and the list shows what it still plays at.

**Files:**
- Modify: `crates/kuvatin/src/gui/video/undo.rs` (`StepKind::Speed`; tests)
- Modify: `crates/kuvatin/src/gui/video/mod.rs` (`SPEEDS`; the labels in `VideoState::new`)
- Modify: `crates/kuvatin/src/gui/video/timeline.rs` (import; `timeline_select`; a Speed handler; `speed_index`; tests)
- Modify: `crates/kuvatin/ui/app.slint` (properties after line 217; the Speed row after the Volume slider, line 1680)

- [ ] **Step 1: Write the failing tests**

In `crates/kuvatin/src/gui/video/undo.rs` tests: add `StepKind::Speed,` to the never-merge list in `gestures_on_the_same_clip_merge_and_nothing_else_does`, and to `each_kind_describes_itself`:

```rust
        assert_eq!(d(StepKind::Speed), "changing the speed of intro.mp4");
```

In `a_speed_change_is_one_changed_clip` (Task 15), change `StepKind::Move` to `StepKind::Speed`.

In `crates/kuvatin/src/gui/video/timeline.rs` tests:

```rust
    // ---- the Speed list -----------------------------------------------------

    #[test]
    fn each_speed_finds_its_own_entry_and_others_the_nearest() {
        for (i, &r) in SPEEDS.iter().enumerate() {
            assert_eq!(speed_index(r), i as i32, "{r}");
        }
        assert_eq!(speed_index(3.0), 4, "between 2x and 4x: the first of the two");
        assert_eq!(speed_index(0.1), 0);
        assert_eq!(speed_index(9.0), 5);
    }

    #[test]
    fn every_speed_offered_is_one_the_engine_keeps_exactly() {
        use kuvatin_video::project::{RATE_MAX, RATE_MIN};
        for r in SPEEDS {
            assert!((RATE_MIN..=RATE_MAX).contains(&r), "{r}");
            assert_eq!(f64::from(r as f32), r, "{r} survives the engine's f32 rounding");
        }
    }
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p kuvatin -- undo::tests timeline::tests`
Expected: FAIL to compile, "no variant named `Speed`" and "cannot find value `SPEEDS`".

- [ ] **Step 3: Add the kind and the list**

In `enum StepKind`, after `Split,`:

```rust
    Speed,
```

In `describe`, after the `Split` arm:

```rust
            StepKind::Speed => format!("changing the speed of {name}"),
```

In `crates/kuvatin/src/gui/video/mod.rs`, after `MAX_SCALE_PCT`:

```rust
/// The speeds the inspector offers, slowest first. A fixed list, not a
/// slider: every change re-times the clip and is one undo step. The labels
/// the window shows are built from it, so the list lives only here.
pub(super) const SPEEDS: [f64; 6] = [0.25, 0.5, 1.0, 1.5, 2.0, 4.0];
```

In `VideoState::new`, after the two scale setters:

```rust
        let labels: Vec<SharedString> = SPEEDS.iter().map(|r| format!("{r}×").into()).collect();
        ui.set_insp_speed_labels(ModelRc::from(Rc::new(VecModel::from(labels))));
```

In `crates/kuvatin/src/gui/video/timeline.rs`, change the `super` import to:

```rust
use super::{VideoState, MAX_SCALE_PCT, MIN_SCALE_PCT, SPEEDS};
```

Add above `#[cfg(test)]`:

```rust
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
```

`min_by` returns the first of equal minima, which is the slower entry.

- [ ] **Step 4: Run them and watch them pass**

Run: `cargo test -p kuvatin -- undo::tests timeline::tests`
Expected: all pass. (The build warns nothing: `SPEEDS` is used by `VideoState::new`, and `speed_index` by the handler in Step 6; if you run clippy before Step 6 it reports `speed_index` unused.)

- [ ] **Step 5: Add the window's side**

In `crates/kuvatin/ui/app.slint`, after `in property <bool> insp-is-still: false;   // selected clip is a still image (duration is free)` (line 217):

```slint
    // Speed, for videos and image sequences. The labels come from SPEEDS in
    // gui/video/mod.rs; the index is the entry showing.
    in property <bool> insp-has-rate: false;
    in property <[string]> insp-speed-labels;
    in-out property <int> insp-rate-index: 2;
    callback inspector-speed-changed(int);     // Speed list: the index picked
```

After the Volume slider (line 1680), before the comment `// A still has no source length`:

```slint
                                // Speed, for clips with source time to stretch: videos and
                                // image sequences, not stills.
                                if root.insp-has-rate : VerticalLayout {
                                    spacing: 4px;
                                    Text { text: "Speed"; color: Theme.muted2; font-size: 9.5px; font-weight: 600; }
                                    ComboBox {
                                        model: root.insp-speed-labels;
                                        current-index <=> root.insp-rate-index;
                                        accessible-label: "Speed";
                                        selected(t) => { root.inspector-speed-changed(self.current-index); }
                                    }
                                }
```

- [ ] **Step 6: Wire it**

In `crates/kuvatin/src/gui/video/timeline.rs`, in the `on_timeline_select` handler, after `ui.set_insp_duration_s(sel_dur.round().max(1.0) as i32);`:

```rust
            // Speed is for clips with source time to stretch.
            ui.set_insp_has_rate(matches!(sel_kind, ClipKind::Video | ClipKind::Sequence));
```

and inside its `if let Some(p) = slot.as_mut() {` block, after the `if let Some(l) = p.clip_layout(&cid) { … }` block:

```rust
                    ui.set_insp_rate_index(speed_index(p.clip_rate(&cid)));
```

After the `on_inspector_duration_changed` block:

```rust
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
                        format!("Could not change the speed of {}. It plays as it did.", row.name),
                    );
                }
            }
        });
    }
```

- [ ] **Step 7: Build and run the suite**

Run: `cargo build -p kuvatin` and `cargo test -p kuvatin`
Expected: clean.

- [ ] **Step 8: Look at it**

Run: `cargo run -p kuvatin`. Open a video with sound. Select its clip: a Speed list under Volume shows 1×. Pick 2×: the clip halves in length, its name reads "… 2×", and the sound plays chipmunk-fast. Ctrl+Z: back to full length at 1×, and the list says 1×. Pick 0.5× on a clip with another clip close after it: it stops at that clip. Trim the 0.5× clip from both edges. Select a still: no Speed row. Save, reopen: the speeds are back. Close the window.

- [ ] **Step 9: Commit**

Gates, then:

```bash
git add -A
git commit -m "The inspector sets a clip's speed, and undo takes it back" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 17: Phase 2 in the changelog and the README

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `README.md`

- [ ] **Step 1: Changelog**

Under `## [Unreleased]`, at the end of `### Added`:

```markdown
- **Clip speed.** The inspector's Speed list plays a video or an image
  sequence at 0.25× to 4×; the clip's length follows, and stops at the next
  clip. Saved with the project and undoable like any other edit. A project
  that uses it opens in 2.12 and earlier with the clip at its saved place and
  length, playing at normal speed.
```

- [ ] **Step 2: README**

In "Video features", after the bullet that begins `- **Overlays & transforms**`:

```markdown
- **Clip speed** — play a video or an image sequence at 0.25× to 4× from the
  inspector; trims follow the speed, and it is saved with the project
```

- [ ] **Step 3: Run the whole Phase 2 gate**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p kuvatin -p kuvatin-core
cargo test -p kuvatin-video -- --test-threads=1 sequence:: previews_and_renders_an_image_sequence transport_and_edits_are_inert_while_rendering export_settings_normalize_size_and_fps trim_math_never_goes_negative stale_end_of_stream set_clip_duration_sets_and_clamps_a_still sets_canvas_size slid neighbour confined_to_the_gap already_overlapping discovery_gives_up without_blocking_the_caller document:: survives_being_saved missing_source encoder undo_ removing_a_clip_straight_after_adding_it_does_not_crash removing_a_clip_added_while_playing_does_not_crash removing_two_clips_back_to_back_while_playing_does_not_crash a_zoomed_clip_keeps_its_scale split_ frame_length shuttle_ speed_ trim_math_follows_the_rate
cargo test -p kuvatin-video -- --test-threads=1 --exact project::tests::renders_after_preview_eos project::tests::renders_gapped_overlay_timeline project::tests::undo_writes_a_left_trim_of_real_media_back project::tests::undo_restores_a_trimmed_clip_of_real_media project::tests::a_sped_up_video_changes_picture_and_sound
```

The last line needs `GST_TEST_FILE` (and `GST_TEST_IMAGE` for the gapped-overlay test, which self-skips without it). Expected: all clean; `kuvatin` 274 passed, 2 ignored; the self-contained list 86 passed, 1 ignored (the measurement); the live-media list 5 passed.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "The changelog and README describe clip speed" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

**Phase 2 ends here. The branch is shippable.**

---

# Phase 3: the audio waveform

A view, not an edit: no undo step, nothing saved, no engine edit path. It follows the shape of `spawn_thumbnails` (`project_file.rs:287-332`): a worker decodes, `slint::invoke_from_event_loop` drops the result into the rows.

### Task 18: The engine draws a source's sound

**Files:**
- Modify: `crates/kuvatin-video/src/project.rs` (`waveform_uri` and `draw_waveform` after `thumbnail_uri`, which ends line 258; tests)
- Modify: `crates/kuvatin-video/src/lib.rs` (the export)
- Modify: `.github/workflows/release.yml` (both steps)

- [ ] **Step 1: Write the failing pure tests**

```rust
    #[test]
    fn waveform_draws_one_bar_per_column() {
        // Four blocks, one of them loud; four columns, four pixels high.
        let frame = draw_waveform(&[0, 0, 32767, 0], 4, 4);
        assert_eq!((frame.width, frame.height, frame.rgba.len()), (4, 4, 64));
        let inked = |x: usize| (0..4).filter(|&y| frame.rgba[(y * 4 + x) * 4 + 3] > 0).count();
        assert_eq!([inked(0), inked(1), inked(2), inked(3)], [0, 0, 4, 0]);
    }

    #[test]
    fn waveform_stretches_a_short_source_across_the_width() {
        // One block at half scale, three columns: a bar in every column,
        // half the height, in the middle.
        let frame = draw_waveform(&[16384], 3, 4);
        let alpha = |x: usize, y: usize| frame.rgba[(y * 3 + x) * 4 + 3];
        for x in 0..3 {
            assert_eq!(
                [alpha(x, 0), alpha(x, 1), alpha(x, 2), alpha(x, 3)],
                [0, 210, 210, 0],
                "column {x}"
            );
        }
    }
```

Worked: a full-scale peak (32767) in a 4-pixel column is `ceil(32767 / 32768 × 2)` = 2 pixels either side of the middle, so the whole column; 16384 is `ceil(0.5 × 2)` = 1 either side, rows 1 and 2. 210 is the ink's alpha.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p kuvatin-video -- --test-threads=1 waveform_`
Expected: FAIL to compile, "cannot find function `draw_waveform`".

- [ ] **Step 3: Implement the drawing**

Add after `thumbnail_uri`:

```rust
/// Samples per second the waveform decodes at: far more than a 22 px clip
/// block can show, and cheap on a long source.
const WAVE_RATE: i32 = 8000;
/// Samples per peak kept while decoding (10 ms). The picture is drawn from
/// these once the whole length is known.
const WAVE_BLOCK: usize = 80;
/// The waveform's ink: a light blue-white, not quite opaque, over a clear
/// background so the clip's own colour shows through.
const WAVE_INK: [u8; 4] = [230, 244, 255, 210];

/// Rasterise block peaks into a `width` × `height` RGBA picture: one column
/// per pixel, each the loudest block it covers, drawn as a bar symmetric
/// about the middle. A source shorter than the width is stretched across it.
fn draw_waveform(peaks: &[u16], width: u32, height: u32) -> Frame {
    let (w, h) = (width as usize, height as usize);
    let mut rgba = vec![0u8; w * h * 4];
    let n = peaks.len();
    if n > 0 {
        for x in 0..w {
            let from = (x * n / w).min(n - 1);
            let to = ((x + 1) * n / w).clamp(from + 1, n);
            let peak = peaks[from..to].iter().copied().max().unwrap_or(0);
            let half = ((f64::from(peak) / 32768.0) * (h as f64 / 2.0)).ceil() as usize;
            let top = (h / 2).saturating_sub(half);
            let bottom = (h / 2 + half).min(h);
            for y in top..bottom {
                let i = (y * w + x) * 4;
                rgba[i..i + 4].copy_from_slice(&WAVE_INK);
            }
        }
    }
    Frame {
        width,
        height,
        rgba,
    }
}
```

- [ ] **Step 4: Run them and watch them pass**

Run: `cargo test -p kuvatin-video -- --test-threads=1 waveform_`
Expected: 2 passed.

- [ ] **Step 5: Write the failing engine tests**

```rust
    /// A WAV written by hand, 16-bit mono PCM.
    fn write_wav(path: &Path, samples: &[i16], rate: u32) {
        let data_len = (samples.len() * 2) as u32;
        let mut b: Vec<u8> = Vec::with_capacity(44 + data_len as usize);
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes()); // PCM
        b.extend_from_slice(&1u16.to_le_bytes()); // mono
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 2).to_le_bytes()); // bytes per second
        b.extend_from_slice(&2u16.to_le_bytes()); // bytes per frame
        b.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        for s in samples {
            b.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(path, b).expect("write the wav");
    }

    /// A second of silence, then a second of a full-scale square wave, at the
    /// rate the waveform decodes at, so nothing is resampled and the length
    /// is exact. No media needed.
    #[test]
    fn waveform_of_a_wav_file_shows_where_the_sound_is() {
        let dir = scratch("waveform-wav");
        let wav = dir.join("half.wav");
        let mut samples = vec![0i16; 8000];
        samples.extend((0..8000).map(|i| if (i / 4) % 2 == 0 { i16::MAX } else { -i16::MAX }));
        write_wav(&wav, &samples, 8000);
        let uri = gst::glib::filename_to_uri(&wav, None).expect("uri");
        let (frame, secs) = waveform_uri(&uri, 100, 10).expect("a waveform");
        assert_eq!(secs, 2.0, "the whole source: 16000 samples at 8 kHz");
        assert_eq!((frame.width, frame.height), (100, 10));
        let inked = |x: usize| (0..10).filter(|&y| frame.rgba[(y * 100 + x) * 4 + 3] > 0).count();
        assert_eq!((inked(0), inked(49)), (0, 0), "the silent first second");
        assert_eq!((inked(50), inked(99)), (10, 10), "the loud one, full height");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn waveform_of_a_still_is_none() {
        let dir = scratch("waveform-still");
        let png = dir.join("still.png");
        image::RgbaImage::from_pixel(64, 36, image::Rgba([10, 120, 200, 255]))
            .save(&png)
            .expect("write still");
        let uri = gst::glib::filename_to_uri(&png, None).expect("uri");
        let started = std::time::Instant::now();
        assert!(waveform_uri(&uri, 64, 8).is_none());
        // It knows once every stream is out, not after a timeout (58 ms, M12).
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Self-skips without `GST_TEST_FILE`.
    #[test]
    fn the_sound_of_a_real_source_is_drawn() {
        let Some(path) = std::env::var_os("GST_TEST_FILE") else {
            eprintln!("skipping the_sound_of_a_real_source_is_drawn: set GST_TEST_FILE");
            return;
        };
        let uri = gst::glib::filename_to_uri(Path::new(&path), None).expect("uri");
        let worker_uri = uri.to_string();
        // Off the interface thread, as the waveform worker calls it.
        let (frame, secs) = std::thread::spawn(move || waveform_uri(&worker_uri, 512, 24))
            .join()
            .expect("the worker")
            .expect("a waveform");
        assert_eq!((frame.width, frame.height), (512, 24));
        assert!(
            frame.rgba.chunks_exact(4).any(|p| p[3] > 0),
            "the fixture's tone is drawn"
        );
        gst::init().expect("gst");
        ges::init().expect("ges");
        let length = ges::UriClipAsset::request_sync(&uri)
            .expect("asset")
            .duration()
            .expect("a length");
        // Measured: 6.982 s drawn for a 6.966 s source, the resampler's tail.
        assert!(
            (secs - length.nseconds() as f64 / 1e9).abs() < 0.05,
            "{secs} vs {length}"
        );
    }
```

Worked for the WAV: 16000 samples in blocks of 80 are 200 peaks; column x covers blocks 2x and 2x + 1, so columns 0 to 49 cover the silent 100 blocks and 50 to 99 the loud ones. The loud peak is 32767, a full column (Step 1's arithmetic). Measured on the probe: `secs = 2`, columns 0 and 49 empty, 50 and 99 full.

- [ ] **Step 6: Run them and watch them fail**

Run: `cargo test -p kuvatin-video -- --test-threads=1 waveform_`
Expected: FAIL to compile, "cannot find function `waveform_uri`".

- [ ] **Step 7: Implement the decoder**

Add after `draw_waveform`:

```rust
/// Peak-per-column audio waveform for a source, rasterised RGBA (see
/// [`draw_waveform`]), plus the seconds of source it spans: the whole source,
/// of which a clip shows a window. None when the source has no sound or
/// cannot be decoded. Its own throwaway pipeline, safe off the UI thread,
/// like [`thumbnail_uri`].
pub fn waveform_uri(uri: &str, width: u32, height: u32) -> Option<(Frame, f64)> {
    if width == 0 || height == 0 {
        return None;
    }
    gst::init().ok()?;
    let pipeline = gst::Pipeline::new();
    let src = gst::ElementFactory::make("uridecodebin")
        .property("uri", uri)
        .build()
        .ok()?;
    let convert = gst::ElementFactory::make("audioconvert").build().ok()?;
    let resample = gst::ElementFactory::make("audioresample").build().ok()?;
    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "S16LE")
        .field("layout", "interleaved")
        .field("channels", 1i32)
        .field("rate", WAVE_RATE)
        .build();
    let sink = AppSink::builder().caps(&caps).sync(false).build();
    pipeline
        .add_many([&src, &convert, &resample, sink.upcast_ref::<gst::Element>()])
        .ok()?;
    gst::Element::link_many([&convert, &resample, sink.upcast_ref::<gst::Element>()]).ok()?;
    // Pictures are not decoded at all: a picture decoder is not plugged and
    // its stream comes out still encoded, to be thrown away. Decoding a long
    // video's frames to draw its sound took twice as long on a trivial source
    // and far more on real footage (M12).
    src.connect("autoplug-select", false, |values| {
        let factory = values.get(3)?.get::<gst::ElementFactory>().ok()?;
        let klass = factory.klass();
        let picture =
            klass.contains("Decoder") && (klass.contains("Video") || klass.contains("Image"));
        let result = gst::glib::EnumClass::with_type(gst::glib::Type::from_name(
            "GstAutoplugSelectResult",
        )?)?;
        // 0 is GST_AUTOPLUG_SELECT_TRY, 1 is GST_AUTOPLUG_SELECT_EXPOSE.
        result.to_value(if picture { 1 } else { 0 })
    });
    let convert_weak = convert.downgrade();
    let pipeline_weak = pipeline.downgrade();
    src.connect_pad_added(move |_, pad| {
        let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
        let is_sound = caps
            .structure(0)
            .is_some_and(|s| s.name().starts_with("audio/x-raw"));
        if is_sound {
            if let Some(sinkpad) = convert_weak.upgrade().and_then(|c| c.static_pad("sink")) {
                if !sinkpad.is_linked() && pad.link(&sinkpad).is_ok() {
                    return;
                }
            }
        }
        // Everything else drains into a fakesink, so a stream nobody reads
        // can never hold the sound up.
        let Some(pipeline) = pipeline_weak.upgrade() else {
            return;
        };
        let Ok(drain) = gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .build()
        else {
            return;
        };
        if pipeline.add(&drain).is_ok() && drain.sync_state_with_parent().is_ok() {
            if let Some(sinkpad) = drain.static_pad("sink") {
                let _ = pad.link(&sinkpad);
            }
        }
    });
    let no_more_pads = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let flag = no_more_pads.clone();
        src.connect_no_more_pads(move |_| flag.store(true, Ordering::SeqCst));
    }
    let stop = |p: &gst::Pipeline| {
        let _ = p.set_state(gst::State::Null);
    };
    if pipeline.set_state(gst::State::Paused).is_err() {
        stop(&pipeline);
        return None;
    }
    let has_sound = || convert.static_pad("sink").is_some_and(|p| p.is_linked());
    // Settle, but stop waiting the moment every stream is out and none of
    // them is sound: a still would otherwise sit out the whole timeout.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let settled = loop {
        match pipeline.state(gst::ClockTime::from_mseconds(50)).0 {
            Ok(gst::StateChangeSuccess::Success) | Ok(gst::StateChangeSuccess::NoPreroll) => {
                break true
            }
            Err(_) => break false,
            _ => {}
        }
        if no_more_pads.load(Ordering::SeqCst) && !has_sound() {
            break false;
        }
        if std::time::Instant::now() >= deadline {
            break false;
        }
    };
    if !settled || !has_sound() {
        stop(&pipeline);
        return None;
    }
    if pipeline.set_state(gst::State::Playing).is_err() {
        stop(&pipeline);
        return None;
    }
    let mut peaks: Vec<u16> = Vec::new();
    let mut block_peak: u16 = 0;
    let mut in_block = 0usize;
    let mut samples: u64 = 0;
    while let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_seconds(5)) {
        let Some(buffer) = sample.buffer() else {
            continue;
        };
        let Ok(map) = buffer.map_readable() else {
            continue;
        };
        for pair in map.as_slice().chunks_exact(2) {
            let v = i16::from_le_bytes([pair[0], pair[1]]).unsigned_abs();
            block_peak = block_peak.max(v);
            in_block += 1;
            samples += 1;
            if in_block == WAVE_BLOCK {
                peaks.push(block_peak);
                block_peak = 0;
                in_block = 0;
            }
        }
    }
    let finished = sink.is_eos();
    stop(&pipeline);
    if in_block > 0 {
        peaks.push(block_peak);
    }
    if !finished || peaks.is_empty() {
        return None;
    }
    let secs = samples as f64 / f64::from(WAVE_RATE);
    Some((draw_waveform(&peaks, width, height), secs))
}
```

In `crates/kuvatin-video/src/lib.rs`, add `waveform_uri` to the `pub use project::{…}` list (`cargo fmt --all` puts it after `warm_asset_uri`).

- [ ] **Step 8: Run them and watch them pass**

Run: `cargo test -p kuvatin-video -- --test-threads=1 waveform_`
Expected: 4 passed. Then with `GST_TEST_FILE` set:
`cargo test -p kuvatin-video -- --test-threads=1 --exact project::tests::the_sound_of_a_real_source_is_drawn`
Expected: 1 passed.

- [ ] **Step 9: Gate them in CI**

Append ` waveform_` to the last line of the self-contained list. In the live-media step, add `"the_sound_of_a_real_source_is_drawn"` to `$names` (after `"a_sped_up_video_changes_picture_and_sound"`, with a comma) and a line `              project::tests::the_sound_of_a_real_source_is_drawn` at the end of the `--exact` list, with a backtick continuation on the line before it. Run the YAML check.

- [ ] **Step 10: Commit**

Gates, plus `cargo test -p kuvatin-video -- --test-threads=1 waveform_`, then:

```bash
git add -A
git commit -m "The engine draws a source's sound without decoding its picture" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 19: The clip block shows the waveform

**Files:**
- Modify: `crates/kuvatin/ui/app.slint` (`TimelineClip`; inside `vis`, between the thumbnail at 1939 and the scrim comment at 1940)
- Modify: `crates/kuvatin/src/gui/video/undo.rs` (the test helper `row`; a test)
- Modify: `crates/kuvatin/src/gui/video/mod.rs` (the two row literals)
- Modify: `crates/kuvatin/src/gui/video/project_file.rs` (the row literal in `restore_models`)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module of `crates/kuvatin/src/gui/video/undo.rs`:

```rust
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
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p kuvatin -- undo::tests::a_row_keeps_its_waveform`
Expected: FAIL to compile, "no field `wave` on type `TimelineClip`".

- [ ] **Step 3: Add the fields**

In `crates/kuvatin/ui/app.slint`, in `struct TimelineClip`, after the `rate` line from Task 15:

```slint
    wave: image,       // the whole source's waveform, or empty
    wave-secs: float,  // how many seconds of source that image spans
```

`place` in `undo.rs` does not change: it must not write these, because they belong to the source and the row already carries them.

In the test helper `row` in `undo.rs`, after `rate: r.rate as f32,`:

```rust
            wave: Image::default(),
            wave_secs: 0.0,
```

In both row literals in `crates/kuvatin/src/gui/video/mod.rs`, after `rate: 1.0,`, and in the one in `crates/kuvatin/src/gui/video/project_file.rs`, after `rate: rec.rate as f32,`:

```rust
                wave: Image::default(),
                wave_secs: 0.0,
```

- [ ] **Step 4: Draw it**

In `crates/kuvatin/ui/app.slint`, inside `vis`, after the thumbnail line `Image { source: clip.thumb; image-fit: cover; width: 100%; height: 100%; }` (line 1939) and before `// left scrim so the name stays legible over the thumb`:

```slint
                                            // The sound: the whole source's waveform, windowed to what
                                            // this clip plays of it, from its in-point for its duration
                                            // times its speed. Trims and speed move the window with no
                                            // work in Rust and no second decode.
                                            if clip.wave.width > 0 && clip.wave-secs > 0 : Image {
                                                property <float> speed: clip.rate > 0 ? clip.rate : 1;
                                                source: clip.wave;
                                                image-fit: fill;
                                                x: 0; y: parent.height * 0.6;
                                                width: parent.width; height: parent.height * 0.4;
                                                source-clip-x: Math.min(clip.wave.width - 1, Math.max(0, Math.round(clip.inpoint / clip.wave-secs * clip.wave.width)));
                                                source-clip-width: Math.max(1, Math.min(clip.wave.width - self.source-clip-x, Math.round(clip.duration * self.speed / clip.wave-secs * clip.wave.width)));
                                            }
```

`source-clip-x` and `source-clip-width` are `int` properties of Slint's `Image` (`i-slint-compiler-1.16.1/builtins.slint:59-63`); the clamps keep the window inside the picture when the source's last few milliseconds round differently from the clip's end.

- [ ] **Step 5: Run the suite and build**

Run: `cargo test -p kuvatin` and `cargo build -p kuvatin`
Expected: clean; `a_row_keeps_its_waveform_through_an_undo` passes.

- [ ] **Step 6: Commit**

Gates, then:

```bash
git add -A
git commit -m "A clip block can draw its sound, windowed by trim and speed" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 20: Decode waveforms on a worker, once per source

The spec's `Rc<RefCell<HashMap<String, (slint::Image, f32)>>>` cannot be filled from a worker: `Rc` and `slint::Image` are not `Send`. The cache holds the pixels (`SharedPixelBuffer`, which is) behind an `Arc<Mutex<…>>`, and each row gets a `slint::Image` made on the interface thread.

The spec says to start it from the places `spawn_thumbnails` is started from, naming opening a project, `add_to_timeline` and `add_sequence_to_timeline`. `spawn_thumbnails` is actually called from `restore_models` (opening) and from undo's `apply_step` (a restored clip), and not from either add function. This task starts waveforms from `restore_models`, `add_to_timeline` and `apply_step`. `add_sequence_to_timeline` is left out: a sequence has no sound. A split needs nothing: the right half's row is a copy of the left's, waveform included (Task 4).

**Files:**
- Create: `crates/kuvatin/src/gui/video/waves.rs`
- Modify: `crates/kuvatin-video/src/project.rs` (`clip_uri` after `clip_track`, which ends line 1339; a test)
- Modify: `crates/kuvatin/src/gui/video/mod.rs` (`mod waves;`; a `VideoState` field; `add_to_timeline`)
- Modify: `crates/kuvatin/src/gui/video/import.rs` (two `add_to_timeline` calls, lines 415 and 537)
- Modify: `crates/kuvatin/src/gui/video/project_file.rs` (`VideoHandles`, `clone_handles`, `restore_models`)
- Modify: `crates/kuvatin/src/gui/video/undo.rs` (`wire`, `apply_step`)

- [ ] **Step 1: Write the failing engine test**

```rust
    #[test]
    fn waveform_source_of_a_clip_is_its_uri() {
        let (dir, png, mut project) = undo_fixture("waveform-uri");
        let a = project
            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(1.0))
            .expect("a");
        let uri = gst::glib::filename_to_uri(&png, None).expect("uri");
        assert_eq!(project.clip_uri(&a).as_deref(), Some(uri.as_str()));
        assert_eq!(project.clip_uri(&ClipId("nope".into())), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
```

Run: `cargo test -p kuvatin-video -- --test-threads=1 waveform_source`
Expected: FAIL to compile, "no method named `clip_uri`".

- [ ] **Step 2: Add the accessor**

In `impl Project`, after `clip_track`:

```rust
    /// The source a clip plays, as its record would name it.
    pub fn clip_uri(&self, id: &ClipId) -> Option<String> {
        Some(
            self.clips
                .get(&id.0)?
                .downcast_ref::<ges::UriClip>()?
                .uri()
                .to_string(),
        )
    }
```

Run: `cargo test -p kuvatin-video -- --test-threads=1 waveform_`
Expected: 5 passed. (`waveform_` already gates it.)

- [ ] **Step 3: Write the failing cache tests**

Create `crates/kuvatin/src/gui/video/waves.rs` with the module doc and the tests only:

```rust
//! The sound drawn in each clip block. A view, not an edit: decoded once per
//! source on a worker, kept by URI for the session, never saved, never
//! recorded, and always safe to throw away and decode again. Only video
//! files are listened to: a still or an image sequence has no sound.

#[cfg(test)]
mod tests {
    use super::*;

    const CLIP: &str = "file:///C:/media/intro.mp4";

    fn wave() -> Wave {
        (SharedPixelBuffer::new(4, 1), 2.0)
    }

    fn ids(v: &[SharedString]) -> Vec<&str> {
        v.iter().map(|s| s.as_str()).collect()
    }

    #[test]
    fn a_source_is_decoded_once_however_many_clips_wait_for_it() {
        let mut inner = Inner::default();
        let (ready, decode) =
            inner.request(vec![("a".into(), CLIP.into()), ("b".into(), CLIP.into())]);
        assert!(ready.is_empty());
        assert_eq!(decode, vec![CLIP.to_string()]);
        let (_, again) = inner.request(vec![("c".into(), CLIP.into())]);
        assert!(again.is_empty(), "already being decoded");
        assert_eq!(ids(&inner.finish(CLIP, Some(wave()))), vec!["a", "b", "c"]);
        let (ready, decode) = inner.request(vec![("d".into(), CLIP.into())]);
        let got: Vec<&str> = ready.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(got, vec!["d"], "from the cache");
        assert!(decode.is_empty());
    }

    #[test]
    fn a_source_with_no_sound_is_not_decoded_again() {
        let mut inner = Inner::default();
        let _ = inner.request(vec![("a".into(), CLIP.into())]);
        inner.finish(CLIP, None);
        let (ready, decode) = inner.request(vec![("b".into(), CLIP.into())]);
        assert!(ready.is_empty() && decode.is_empty());
    }

    #[test]
    fn stills_and_sequences_are_not_listened_to() {
        let mut inner = Inner::default();
        let (ready, decode) = inner.request(vec![
            ("a".into(), "file:///C:/media/logo.png".into()),
            (
                "b".into(),
                "imagesequence://C:/r/f_%04d.png?start-index=1&framerate=24/1".into(),
            ),
        ]);
        assert!(ready.is_empty() && decode.is_empty());
    }
}
```

Add `mod waves;` to the module list in `crates/kuvatin/src/gui/video/mod.rs`, after `mod undo;`.

Run: `cargo test -p kuvatin -- waves::tests`
Expected: FAIL to compile, "cannot find type `Inner`".

- [ ] **Step 4: Implement the cache**

Put above the tests in `crates/kuvatin/src/gui/video/waves.rs`:

```rust
use super::project_file::kind_of;
use crate::gui::{AppWindow, ClipKind};
use slint::{Image, Model, Rgba8Pixel, SharedPixelBuffer, SharedString};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The waveform picture's size. It spans the whole source and each clip shows
/// a window of it, so it is wide: a few minutes of source still give a column
/// every few pixels of a zoomed-in clip.
const WAVE_W: u32 = 4096;
const WAVE_H: u32 = 32;

/// One source's waveform and the seconds of source it spans. The pixels, not
/// a `slint::Image`, so the worker can hand it over.
type Wave = (SharedPixelBuffer<Rgba8Pixel>, f32);

/// Clips whose waveform is known, each with it.
type Ready = Vec<(SharedString, Wave)>;

#[derive(Default)]
struct Inner {
    /// Decoded sources by URI; `None` for a source with no sound, so it is
    /// not decoded again.
    done: HashMap<String, Option<Wave>>,
    /// Sources being decoded, each with the clips waiting for it.
    waiting: HashMap<String, Vec<SharedString>>,
}

impl Inner {
    /// Sort `clips` (clip id, source URI) into those whose waveform is known
    /// and the sources to decode, each once however many clips wait on it. A
    /// clip that is not a video, or whose source has no sound, gets nothing.
    fn request(&mut self, clips: Vec<(SharedString, String)>) -> (Ready, Vec<String>) {
        let mut ready = Vec::new();
        let mut decode = Vec::new();
        for (id, uri) in clips {
            if kind_of(&uri) != ClipKind::Video {
                continue;
            }
            match self.done.get(&uri) {
                Some(Some(wave)) => ready.push((id, wave.clone())),
                Some(None) => {}
                None => {
                    let waiting = self.waiting.entry(uri.clone()).or_default();
                    if waiting.is_empty() {
                        decode.push(uri);
                    }
                    waiting.push(id);
                }
            }
        }
        (ready, decode)
    }

    /// A source finished decoding: keep it, and hand back the clips that
    /// were waiting for it.
    fn finish(&mut self, uri: &str, wave: Option<Wave>) -> Vec<SharedString> {
        self.done.insert(uri.to_string(), wave);
        self.waiting.remove(uri).unwrap_or_default()
    }
}

/// The waveform cache, shared with the worker that fills it.
#[derive(Clone, Default)]
pub(super) struct Waves(Arc<Mutex<Inner>>);

impl Waves {
    /// Give each clip its source's waveform: at once from the cache, else once
    /// a worker has decoded it. `clips` are (clip id, source URI). A failure
    /// to decode is silent: a missing picture of the sound is better than a
    /// dialog about one.
    pub(super) fn fill(&self, ui_weak: slint::Weak<AppWindow>, clips: Vec<(SharedString, String)>) {
        let Ok((ready, decode)) = self.0.lock().map(|mut inner| inner.request(clips)) else {
            return;
        };
        if let Some(ui) = ui_weak.upgrade() {
            for (id, wave) in &ready {
                set_wave(&ui, id, wave);
            }
        }
        if decode.is_empty() {
            return;
        }
        let inner = self.0.clone();
        let _ = std::thread::Builder::new()
            .name("kuvatin-waveforms".into())
            .spawn(move || {
                for uri in decode {
                    let wave = kuvatin_video::waveform_uri(&uri, WAVE_W, WAVE_H).map(|(f, secs)| {
                        (
                            SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                                &f.rgba, f.width, f.height,
                            ),
                            secs as f32,
                        )
                    });
                    let Ok(ids) = inner.lock().map(|mut i| i.finish(&uri, wave.clone())) else {
                        continue;
                    };
                    let Some(wave) = wave else {
                        continue;
                    };
                    let ui_weak = ui_weak.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_weak.upgrade() {
                            for id in &ids {
                                set_wave(&ui, id, &wave);
                            }
                        }
                    });
                }
            });
    }
}

/// Put a waveform into the timeline row of clip `id`, if it is still there.
fn set_wave(ui: &AppWindow, id: &SharedString, wave: &Wave) {
    let clips = ui.get_timeline_clips();
    for i in 0..clips.row_count() {
        if let Some(mut row) = clips.row_data(i) {
            if row.id == *id {
                row.wave = Image::from_rgba8(wave.0.clone());
                row.wave_secs = wave.1;
                clips.set_row_data(i, row);
            }
        }
    }
}
```

Run: `cargo test -p kuvatin -- waves::tests`
Expected: 3 passed. (Clippy reports `Waves` unused until Step 5.)

- [ ] **Step 5: Start it where clips appear**

In `crates/kuvatin/src/gui/video/mod.rs`, in `struct VideoState`, after `history: undo::TimelineHistory,` (private, like `history`, so its type need not leave the Videos modules):

```rust
    /// The waveform per source, decoded once and shared by every clip of it.
    /// A view, never saved; see `waves`.
    waves: waves::Waves,
```

In `VideoState::new`'s `Self { … }`, after `history: …,`:

```rust
            waves: waves::Waves::default(),
```

Give `add_to_timeline` a last parameter, `waves: &waves::Waves,`, and after its `rec.record(…)` call:

```rust
            // Only a video can have sound.
            if !is_img {
                if let Some(uri) = project.clip_uri(&info.id) {
                    waves.fill(ui_weak.clone(), vec![(info.id.0.as_str().into(), uri)]);
                }
            }
```

In `crates/kuvatin/src/gui/video/import.rs`, in the import-timer block, after `let rec = rec.clone();` (line 366), and in the media-bin block, after `let rec = rec.clone();` (line 513):

```rust
        let waves = st.waves.clone();
```

and change both calls (lines 415 and 537) to end `…, thumb, &rec, &waves);`.

In `crates/kuvatin/src/gui/video/project_file.rs`, add to `VideoHandles`, after `history`:

```rust
    pub(super) waves: super::waves::Waves,
```

to `clone_handles`, after `history: self.history.clone(),`:

```rust
            waves: self.waves.clone(),
```

and in `restore_models`, replace its last line, `spawn_thumbnails(ui.as_weak(), records);`, with:

```rust
    let sources: Vec<(SharedString, String)> = records
        .iter()
        .map(|(id, rec)| (id.0.as_str().into(), rec.uri.clone()))
        .collect();
    st.waves.fill(ui.as_weak(), sources);
    spawn_thumbnails(ui.as_weak(), records);
```

In `crates/kuvatin/src/gui/video/undo.rs`, in `wire`, after `let rec = st.recorder(ui);`:

```rust
        let waves = st.waves.clone();
```

change the call to `apply_step(&ui, &project, &rec, &sel_idx, dir, &waves);`, and give `apply_step` a last parameter, `waves: &super::waves::Waves,`. In `apply_step`, after the `without_thumb` binding:

```rust
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
```

and after `super::project_file::spawn_thumbnails(ui.as_weak(), without_thumb);`:

```rust
    waves.fill(ui.as_weak(), without_wave);
```

- [ ] **Step 6: Build and run the suite**

Run: `cargo build -p kuvatin` and `cargo test -p kuvatin`
Expected: clean.

- [ ] **Step 7: Look at it**

Run: `cargo run -p kuvatin`. Open a video with sound: the bottom of its clip block fills with the waveform a moment later while the timeline stays responsive. Trim the left edge: the waveform window slides with it. Set it to 2×: the block shows twice the sound in the same space. Split it: both halves show their own part at once. Delete a half and undo: its waveform comes back without a wait. Add a still: no waveform. Open a saved project: waveforms arrive after the thumbnails. Close the window.

- [ ] **Step 8: Commit**

Gates, plus `cargo test -p kuvatin-video -- --test-threads=1 waveform_`, then:

```bash
git add -A
git commit -m "Each video clip shows its waveform, decoded once per source" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 21: Phase 3 in the changelog and the README, and the last gate

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `README.md`

- [ ] **Step 1: Changelog**

Under `## [Unreleased]`, at the end of `### Added`:

```markdown
- **The sound is drawn on the clip.** A video's waveform runs along the
  bottom of its block on the timeline and follows trims and speed changes, so
  a cut can be placed by looking at the sound.
```

- [ ] **Step 2: README**

In "Video features", in the bullet that begins `- **Layered timeline editor**`, add before its last line: `waveforms on video clips;`, so the bullet reads:

```markdown
- **Layered timeline editor** — drag files straight onto the timeline; slide,
  edge-trim, split at the playhead, and move clips across tracks with magnetic
  snapping; waveforms on video clips; reorder tracks; drop below the last
  track to create a new one
```

- [ ] **Step 3: Run everything**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p kuvatin -p kuvatin-core
cargo test -p kuvatin-video -- --test-threads=1 sequence:: previews_and_renders_an_image_sequence transport_and_edits_are_inert_while_rendering export_settings_normalize_size_and_fps trim_math_never_goes_negative stale_end_of_stream set_clip_duration_sets_and_clamps_a_still sets_canvas_size slid neighbour confined_to_the_gap already_overlapping discovery_gives_up without_blocking_the_caller document:: survives_being_saved missing_source encoder undo_ removing_a_clip_straight_after_adding_it_does_not_crash removing_a_clip_added_while_playing_does_not_crash removing_two_clips_back_to_back_while_playing_does_not_crash a_zoomed_clip_keeps_its_scale split_ frame_length shuttle_ speed_ trim_math_follows_the_rate waveform_
cargo test -p kuvatin-video -- --test-threads=1 --exact project::tests::renders_after_preview_eos project::tests::renders_gapped_overlay_timeline project::tests::undo_writes_a_left_trim_of_real_media_back project::tests::undo_restores_a_trimmed_clip_of_real_media project::tests::a_sped_up_video_changes_picture_and_sound project::tests::the_sound_of_a_real_source_is_drawn
python -c "import yaml,io; yaml.safe_load(io.open('.github/workflows/release.yml', encoding='utf-8')); print('yaml ok')"
```

The fourth line must match the self-contained list in `release.yml` name for name; compare them. Expected: all clean; `kuvatin` 278 passed, 2 ignored; the self-contained list 91 passed, 1 ignored; the live-media list 6 passed (with `GST_TEST_FILE` and `GST_TEST_IMAGE` set); `yaml ok`.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "The changelog and README describe the waveform" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

**Phase 3 ends here.**

---

## Where this plan departs from the spec

Each of these was found by reading the current code or by running GES, and each task that carries one says so where it happens.

1. **A time effect is not refused for being too long** (M4): GES shortens the clip instead. The shrink-first order stays, because it is still what lets a slowed clip grow; the refusal path is rarer than the spec assumed.
2. **No `register_time_property` call** (M1, M2): GES already knows both elements, and the call returns `false`.
3. **`pitch` is only added to a clip that has sound** (M5): otherwise GES fails without an error and the binding panics in a debug build.
4. **Rates are rounded to `f32`** (`snap_rate`, M3): `pitch` stores a float and undo compares exactly.
5. **`duration_limit()` is never called** (M7): it panics on a still; the property is read instead.
6. **Frame lengths under 1 ms are ignored** (M10): the preview emits 1 ns buffers around a flushing seek.
7. **The engine speed tests run on a generated image sequence**, not a generated still: a still has no speed and no sound. The two-effect test runs on the live-media fixture (Task 14).
8. **The neighbour test slows the clip down.** The spec's "speeding a clip up against a neighbour" cannot reach the neighbour, since speeding up shortens a clip.
9. **The spec's testing list swaps split's undo and redo.** Undo is a write and a removal, redo a write and a restore, as its own architecture section says (Task 4's test).
10. **L while paused plays at 1×**, per the spec's key table; its testing list says it steps a frame, which contradicts the table.
11. **The in-point of a left trim is rounded down**, not truncated (Task 11), so a leftward extension never asks for more source than exists.
12. **A slowed clip is restored and loaded short, then grown** (Task 13): at 1× it may be longer than its source, and GES refuses a duration past the limit.
13. **Split writes the transform only onto a clip that has one** (Task 3): writing the read-back of an untouched clip would turn stretch-to-fill into a corner-aligned fit.
14. **`inspector-speed-changed` passes an index**, not a float, so the list of speeds lives once, in Rust (`SPEEDS`).
15. **`rate` on the row arrives with speed (Phase 2)**, not with the waveform, so Phase 3 can be dropped without taking it along.
16. **The waveform cache is `Arc<Mutex<…>>` of pixel buffers**, not `Rc<RefCell<…>>` of images, which cannot cross to the worker; and it is started from `restore_models`, `add_to_timeline` and undo's `apply_step`, which is where `spawn_thumbnails` really is called from, not from `add_sequence_to_timeline`.
17. **The waveform decoder does not decode pictures** (M12) and returns `None` for a silent source as soon as every stream is out.
18. **`set_rate` stops with `SeekType::None`**, the call measured to work (M9), not `SeekType::End`.

## Self-review

- **Spec coverage.** Split: engine (Task 3), undo kind and interface (Task 4), speed kept across a split (Task 13). Frame stepping and the shuttle: frame length (Task 5), rate (Task 6), keys, readout, mute (Task 7). Scale: the three interface places and the constant (Task 1), the engine pin and the doc comment (Task 2). Speed: the measurement (Task 9), the engine (Task 10), trims (Task 11), the record (Task 12), undo, restore, load and split (Task 13), real media (Task 14), the row (Task 15), the inspector and `StepKind::Speed` (Task 16). Waveform: the decoder (Task 18), the row and the drawing (Task 19), the cache, the worker and where it starts (Task 20). Every failure-handling row of the spec: split with nothing selected (the chip's `enabled`, the S key's condition), split refused (Task 3's message, Task 4's dialog), shuttle with no engine (the handlers return), a refused rate (Task 16's message, the clip left as it was), a speed change into a neighbour (Task 10), a waveform that cannot be decoded (Task 20's silent `fill`). The spec's testing list: every item has a test above, with the corrections listed under "Where this plan departs".
- **Placeholder scan.** No step says "similar to", "TBD" or "add error handling"; every code step shows its code; every expected value in a test is worked in the task or measured (M4, M10, M12).
- **Names used consistently.** Engine: `split_fits`, `split_clip`, `next_frame_ns`, `DEFAULT_FRAME_NS`, `MIN_FRAME_NS`, `frame_secs`, `set_rate`, `rate`, `RATE_MIN`, `RATE_MAX`, `PITCH_PROPERTY`, `snap_rate`, `rate_change_math`, `room_after`, `time_effects`, `clip_rate_of`, `apply_rate`, `clip_rate`, `set_clip_rate`, `set_clip_placement`, `put_rate_back`, `clip_uri`, `WAVE_RATE`, `WAVE_BLOCK`, `WAVE_INK`, `draw_waveform`, `waveform_uri`; test helpers `sequence_fixture`, `write_wav`. Interface: `MIN_SCALE_PCT`, `MAX_SCALE_PCT`, `SPEEDS`, `scale_percent`, `speed_index`, `StepKind::Split`, `StepKind::Speed`, `LADDER`, `Shuttle`, `shuttle`, `step_target`, `step_frame`, `sync_shuttle`, `Waves`, `Inner::request`, `Inner::finish`, `set_wave`. Slint: `insp-scale-min`, `insp-scale-max`, `timeline-split`, `video-step`, `video-shuttle`, `shuttle-rate`, `transport-keys`, `insp-has-rate`, `insp-speed-labels`, `insp-rate-index`, `inspector-speed-changed`, and the row fields `rate`, `wave`, `wave-secs`.
- **Gate lists.** Self-contained additions: `a_zoomed_clip_keeps_its_scale` (Task 2), `split_` (3), `frame_length` (5), `shuttle_` (6), `speed_` (10), `trim_math_follows_the_rate` (11), `waveform_` (18). Live media: `a_sped_up_video_changes_picture_and_sound` (14), `the_sound_of_a_real_source_is_drawn` (18). Undo tests are covered by the existing `undo_`, document tests by `document::`.
- **Highest risk.** Task 13 (the order of duration, rate and in-point writes in undo and restore: a wrong order is refused silently and only the read-back says so) and Task 10 (the engine's first contact with time effects on a live pipeline). Task 7 and Task 16 touch the most interface code without a test that drives the window; their "Look at it" steps are the check.

---

## Amendments

### Task 9 measurement

Run 2026-09-23 on the development machine (Windows 11, GStreamer 1.26.11 MSVC x86_64, `gstreamer-editing-services` 0.23.5, debug build), against the fixture from "Before you start" (`gst-discoverer-1.0` gives it 0:00:06.965986394, VP8 and Vorbis):
`cargo test -p kuvatin-video -- --ignored --nocapture --test-threads=1 speed_measure_the_runtime` gave `1 passed` and printed:

```text
M0 GStreamer 1.26.11
M1 "videorate": is_time_effect = true
M1 "pitch": is_time_effect = true
M1 "videorate rate=2": is_time_effect = true
M1 "pitch rate=2": is_time_effect = true
M1 "pitch tempo=2": is_time_effect = true
M2 videorate register_time_property("rate") = false, is_time_effect = true
M2 videorate register_time_property("GstVideoRate::rate") = false, is_time_effect = true
M2 pitch register_time_property("rate") = false, is_time_effect = true
M2 pitch register_time_property("GstPitch::rate") = false, is_time_effect = true
M2 pitch register_time_property("tempo") = false, is_time_effect = true
thread '…' panicked at …\gstreamer-editing-services-0.23.5\src\auto\clip.rs:137:14:
mandatory glib value is None: GlibNoneError
M7 still: duration_limit() panics = true; the property reads None
thread '…' panicked at …\gstreamer-editing-services-0.23.5\src\auto\clip.rs:86:13:
assertion `left == right` failed
  left: true
 right: false
M5 pitch on a sequence, which has no sound: panicked: GES returned FALSE without a GError
M8 videorate on a still: Ok(())
M4 full length 0:00:06.965986394 limit Some(0:00:06.965986394)
M4 videorate=2 on the full-length clip: Ok(()); duration now 0:00:03.482993197 limit Some(0:00:03.482993197)
M4 growing past the limit: set_duration = false
M4 pitch=2: Ok(())
M3 TrackType(VIDEO) Some("effect12"): rate = Some((gdouble) 2.000000), tempo = None
M3 TrackType(AUDIO) Some("effect13"): rate = Some((gfloat) 2.000000), tempo = Some((gfloat) 1.000000)
M6 right half: start 0:00:01.000000000 inpoint 0:00:02.000000000 duration 0:00:02.482993197 effects 2 posx Some((gint) 40) width Some((gint) 640) alpha Some((gdouble) 0.500000) volume Some((gdouble) 0.250000)
M6 split at the clip's own start: Ok(false)
M11 pitch tempo=2 on a half-length clip: Ok(()), limit Some(0:00:03.482993197)
```

The two `panicked at` blocks are the panic hook's output for the panics the test catches on purpose: the binding's `expect` in `duration_limit()` (M7) and its `debug_assert` on a FALSE return with no GError (M5).

**Against the table, fact by fact:**

| # | Measured here | Same as the table? |
| --- | --- | --- |
| M1 | All five descriptions are time effects as created | Yes |
| M2 | All five `register_time_property` calls return `false` | Yes |
| M3 | Unqualified `"rate"` answers on both; `videorate` a `gdouble`, `pitch` a `gfloat` | Yes |
| M4 | Accepted, and the clip shortened from 6.965986394 s to 3.482993197 s; growing past the limit refused (`false`) | Yes. The sub-fact "raising the rate on a live effect does the same" is not in the measurement; Task 10 removes and re-adds effects and never raises a live one |
| M5 | Panics in this debug build: FALSE without a GError | Yes |
| M6 | In-point 2 s after 1 s of timeline at 2×, both effects and the transform copied; `Ok(None)` at the clip's own start | Yes. "Outside the clip" is not in the measurement |
| M7 | `duration_limit()` panics on a still; the property reads `None` | Yes |
| M8 | `videorate` on a still: `Ok(())` | Yes |
| M9 | Not in this measurement. Held on this machine by Phase 1's gate `shuttle_plays_faster_and_refuses_reverse` | Not re-measured |
| M10 | Not in this measurement. Held on this machine by Phase 1's gates `frame_length_follows_the_preview` and `frame_length_ignores_buffers_too_short_to_be_frames` | Not re-measured |
| M11 | `pitch tempo=2` accepted on a half-length clip, limit 3.482993197 s, the same as `rate=2` gives | Yes |
| M12 | Not in this measurement: the waveform decoder is Phase 3 (Task 18) | Not re-measured |

No measured fact differs, so no contingency is taken: Task 10 goes ahead as written, with no `register_time_property` call, the name `"rate"` in `clip_rate_of`, and the `AudioSource` guard in `apply_rate`.
