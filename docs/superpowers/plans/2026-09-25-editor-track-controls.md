# Editor Track Controls Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Each timeline track can be named, muted, soloed and locked. A name, mute or lock survives an undo, a reorder, and closing and reopening the project; a muted track is silent in the preview and in the export; a soloed track is the only one heard while listening; a locked track refuses every edit to what is on it, and says so.

**Architecture:** A track stays its position (the spec's guiding decision 1). The interface's track rows become `TimelineTrack` structs and are the truth; the engine learns only which positions to silence (`Project::set_track_mutes`, GES `set_active_for_tracks` on the audio track). The undo capture's track count becomes a table of `TrackRecord`s, so mute, lock and rename are ordinary recorded steps with no new step shape. The table is saved as an optional `[[tracks]]` array, so the file stays at format version 1. Solo is derived, never saved or undone, and switched off when an export starts. Lock is a guard on every path that edits a clip, and the interface takes away a locked clip's handles.

**Tech Stack:** Rust, Slint 1.16, GStreamer 1.26 with GES through `gstreamer-editing-services` 0.23, TOML project files.

**Spec:** `docs/superpowers/specs/2026-09-16-editor-track-controls-design.md`. Read it before Task 1. It was written before the clip edits and the in-app update merged, so its line numbers and its list of edit paths are out of date; this plan cites only current code, and "Where this plan departs from the spec" below says what changed and why.

**Verified before review:** every diff below was applied, in task order, to an export of `master` at `16a2e61`, and run on GStreamer 1.26.11. `cargo fmt --check` was clean after each task, `cargo clippy -D warnings` after every task that touches the app crate and over the whole workspace at the end, every test the plan adds passed (the live-media ones against the fixture below), and the expected counts in each task are the ones that run produced. The diffs were then applied again with `git apply` to a fresh worktree of `master`, and after `cargo fmt --all` the result was identical, file for file, to the verified export. The build was also run by hand (see Task 15 for what was and was not checked), which is how the two faults in Task 14 were found.

---

## The undo design is amended

The spec says it cannot be built until `docs/superpowers/specs/2026-09-13-undo-design.md` is amended, per clause 4 of that design's "Contract for later edits". The amendment landed in the same commit as this plan: the track table replaces the track-row count in a step, Mute track, Lock track and Rename track are step kinds (Split and Speed, which the clip edits added, are listed too), Rename track merges, the three new hints are listed, and the Recording table names the new callbacks. Nothing in this plan edits that design again.

## Where this plan departs from the spec

1. **More edit paths need the lock guard.** The clip edits added two after the spec was written: Split (the chip and **S**, `on_timeline_split`) and the inspector's Speed list (`on_inspector_speed_changed`). Both are guarded, in Task 10, beside the seven the spec lists. Keyboard nudges and trims go through the drop and trim callbacks and need nothing of their own.
2. **A mute is not unsaved work to the engine; the interface says when it is.** The in-app update added `Project::touched()`, which marks work unsaved. The spec asked `set_track_mutes` to call it, but solo drives the same engine call, and solo is not saved: every solo click would have made closing the window ask about unsaved changes, and opening a project would have marked it unsaved the moment its mutes were pushed. So `set_track_mutes` only repaints, and a new `Project::mark_unsaved()` is called by the interface for a mute, a lock, a rename and every undo or redo (Tasks 2, 5 and 6). A rename or a lock changes nothing in the engine, so without it closing the window would silently lose them.
3. **The engine's mute vector follows the rows, not the layers.** The spec had `prune_tracks` truncate it. But a row outlives its layer: deleting the last clip on a muted track prunes the layer while the row, still muted, stays, and a clip dropped there later must arrive silent. So nothing truncates the vector, `layer()` applies it to every layer it makes, and `move_track` permutes it (Task 2). Every change to the rows ends in `push_mutes`, which rewrites it whole.
4. **Reordering works on a row with no layer, and before any clip exists.** Rows now carry names, so dragging one matters even when the engine has fewer layers than rows, or no engine at all yet. `move_track` makes the layers it needs, and the interface moves the row itself, recording the step with or without an engine (Tasks 2 and 8).
5. **The track table is left out of an untouched file.** TOML writes a bare `[[tracks]]` header per row even when every field is skipped, so a project nobody renamed would gain noise. The whole table is skipped when every record is at its defaults (Task 1).
6. **Rename is a bare `TextInput`, not a `LineEdit`.** The fluent `LineEdit` is at least 32 px tall and 160 px wide; a header row is 30 px and the name has about 80 (Task 9).
7. **A locked clip stays selectable.** Its drag, trim grips and × stand down, and so do the inspector's controls, its box in the preview and the Split chip, all from one derived `insp-locked` property; the Rust guards are the safety net (Task 11). The inspector says why.
8. **The refusal names the track by its label.** "Dialogue is locked" rather than "Track N is locked" for a named track; an unnamed one still reads "Track 2 is locked".
9. **Solo's render path clears the rows rather than passing a flag.** `effective_mutes` has no `solo_allowed` parameter; `clear_solo` switches the rows off and pushes, which is what the spec's render path amounts to (Task 12).
10. **Two faults found by running the build**, both fixed in Task 14 with a regression test: a mute set before any clip existed never reached the engine created for the first clip, and a mute straight after a track reorder made GStreamer call the audio composition invalid in three runs of six.

## Before you start

- **Work in a worktree of its own on branch `track-controls`** (superpowers:using-git-worktrees). Do not commit to `master`, and do not touch the main checkout at `C:\Työt\Koodaus\Kuvatin`. Run every command from the worktree root.
- **GStreamer must be on PATH for every cargo command** (Git Bash):
  `export PATH="/c/Program Files/gstreamer/1.0/msvc_x86_64/bin:$PATH"`.
- **The video tests run one at a time.** Every command that runs `kuvatin-video` tests passes `-- --test-threads=1`. Concurrent GES pipelines deadlock, and the failures that produces are not real.
- **Applying a task.** Each task's change is one diff, verified in order. Save it to a file outside the repository and apply it with `git apply --whitespace=nowarn <file>` from the worktree root, or make the same edits by hand with the Edit tool. Do not paste a diff through a heredoc: the Bash tool loses one level of backslash there, and `app.slint` holds `\u{…}` escapes. If `git apply` refuses a hunk, an earlier task was not applied exactly; find out why rather than forcing it.
- **Run `cargo fmt --all` before every commit.** The Task 2 and Task 3 diffs are shown as written; rustfmt reshapes a few of their lines, which is expected.
- **`kuvatin` is a binary crate.** A `pub(super)` item with no caller fails `clippy -D warnings`. Every helper here lands in the task that first calls it, so no task needs `#[allow(dead_code)]`.
- **Every new video test that gates behaviour is in `.github/workflows/release.yml`** by the end of Task 14: the self-contained engine tests through the `track_mute_` filter in `Test (video engine, self-contained — gates the release)`, the three that need real media by name in `Test (live-media regressions — gates the release)`. A test left out of those lists never runs in CI.
- **The workflow file changes.** The `gh` token has no `workflow` scope, so the pull request cannot be merged with `gh pr merge`; merge it locally when asked.
- **Never run filesystem-wide searches** (`find /`). Crate sources live under `~/.cargo/registry/src/index.crates.io-*/`.
- **Check the disk before build-heavy runs** (`df -h /c`). `target/` has filled the disk before; prune `target/debug/incremental` if it is over a day old.
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
- **The live-media fixture** (Tasks 3 and 14) is built the way the pipeline builds it, in a folder with an ASCII path:
  ```bash
  F="$(cygpath -m "$LOCALAPPDATA")/Temp/kuvatin-fixtures"
  mkdir -p "$F"
  gst-launch-1.0 -e videotestsrc num-buffers=150 ! "video/x-raw,width=320,height=180" ! vp8enc ! webmmux name=m ! filesink location="$F/fixture_av.webm" audiotestsrc num-buffers=300 ! audioconvert ! vorbisenc ! m.
  export GST_TEST_FILE="$F/fixture_av.webm"
  ```
  It is 6.97 s long, with a 30 fps picture and a sine tone.

## Files

| File | What changes |
| --- | --- |
| `crates/kuvatin-video/src/document.rs` | `TrackRecord`, and `ProjectFile::tracks`, skipped when untouched |
| `crates/kuvatin-video/src/lib.rs` | re-exports `TrackRecord` |
| `crates/kuvatin-video/src/project.rs` | the `mutes` vector, `set_track_mutes`, `track_muted`, `mark_unsaved`; `layer()`, `move_track` and `apply_document` keep the vector right; engine and live-media tests |
| `crates/kuvatin/src/gui/video/tracks.rs` (new) | the pure track helpers (effective mutes, records, labels, locks, refusals, solo clearing) and the header callbacks |
| `crates/kuvatin/src/gui/video/undo.rs` | the capture's table, `Subject`, the three new step kinds, `set_track_rows` against a table |
| `crates/kuvatin/src/gui/video/mod.rs` | the track model's type, the transform timer's lock guard, the add paths' lock guard and mute push |
| `crates/kuvatin/src/gui/video/timeline.rs` | new rows are default rows, the reorder moves rows, the lock guards |
| `crates/kuvatin/src/gui/video/project_file.rs` | the table saved, and the rows built from it on open |
| `crates/kuvatin/src/gui/video/export.rs` | solo switched off before a render |
| `crates/kuvatin/ui/app.slint` | `TimelineTrack`, the 168 px header with M / S / L, rename in place, lane tints, a locked clip's affordances |
| `crates/kuvatin/ui/widgets.slint` | `TrackToggle`; `InspSlider` gains `enabled` |
| `.github/workflows/release.yml` | the new tests in the gating steps |
| `CHANGELOG.md`, `README.md` | the feature, for 2.14.0 |

---

### Task 1: The saved format holds a track table

A `TrackRecord` is what the file and the undo history keep of a track: its name, whether it is muted, whether it is locked. `ProjectFile` gains an optional `tracks` array, left out of the file when every track is at its defaults, so a project nobody renamed is written exactly as before and every older file still opens.

**Files:**
- Modify: `crates/kuvatin-video/src/document.rs` (after `is_unit_rate`, in `ProjectFile`, and the tests)
- Modify: `crates/kuvatin-video/src/lib.rs:15`

- [ ] **Step 1: Apply the diff**

```diff
diff --git a/crates/kuvatin-video/src/document.rs b/crates/kuvatin-video/src/document.rs
index 07cf15f..28e12a2 100644
--- a/crates/kuvatin-video/src/document.rs
+++ b/crates/kuvatin-video/src/document.rs
@@ -90,6 +90,33 @@ fn is_unit_rate(rate: &f64) -> bool {
     *rate == 1.0
 }
 
+/// One track, as stored. Its index in [`ProjectFile::tracks`] is the track:
+/// 0 is the top one, the numbering [`ClipRecord::track`] uses. Each field is
+/// left out of the file at its default, so a track nobody touched is a bare
+/// `[[tracks]]` header.
+#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
+pub struct TrackRecord {
+    /// What the user called it. Empty means "call it Track N".
+    #[serde(default, skip_serializing_if = "String::is_empty")]
+    pub name: String,
+    /// Silent: its layer plays no sound, in the preview or the export.
+    #[serde(default, skip_serializing_if = "is_false")]
+    pub muted: bool,
+    /// Refuses every edit to the clips on it.
+    #[serde(default, skip_serializing_if = "is_false")]
+    pub locked: bool,
+}
+
+fn is_false(b: &bool) -> bool {
+    !*b
+}
+
+/// A track table that says nothing a default one would not. It is left out of
+/// the file, and the track count comes from the clips, as it always has.
+fn untouched(tracks: &[TrackRecord]) -> bool {
+    tracks.iter().all(|t| *t == TrackRecord::default())
+}
+
 /// The file a `file://` URI names, or None for anything else (an
 /// `imagesequence://` clip is a run of files, not one). The interface needs
 /// this to put a reopened project's sources back in the media bin.
@@ -106,6 +133,15 @@ pub struct ProjectFile {
     /// In timeline order, top track first. Empty is a valid project.
     #[serde(default)]
     pub clips: Vec<ClipRecord>,
+    /// One per track row, top first: names, mutes and locks. Left out when no
+    /// track has any of them, so a project that never used them is written
+    /// exactly as before. Empty on reading means "infer the tracks from the
+    /// clips", which is what every file from 2.13 and earlier needs.
+    ///
+    /// Filled in by the interface, which owns names and locks; the engine
+    /// writes it empty (see `Project::to_document`).
+    #[serde(default, skip_serializing_if = "untouched")]
+    pub tracks: Vec<TrackRecord>,
 }
 
 impl ProjectFile {
@@ -115,6 +151,7 @@ impl ProjectFile {
             canvas_w,
             canvas_h,
             clips,
+            tracks: Vec::new(),
         }
     }
 
@@ -311,5 +348,67 @@ volume = 1.0
         assert_eq!(doc.version, 1);
         assert_eq!(doc.clips[0].rate, 1.0);
         assert_eq!(doc.clips[0].duration, 4.25);
+        assert!(
+            doc.tracks.is_empty(),
+            "no table: the tracks come from the clips"
+        );
+    }
+
+    /// A top track at its defaults, a named and muted one, and a locked one.
+    fn tracks() -> Vec<TrackRecord> {
+        vec![
+            TrackRecord::default(),
+            TrackRecord {
+                name: "Dialogue".into(),
+                muted: true,
+                locked: false,
+            },
+            TrackRecord {
+                name: String::new(),
+                muted: false,
+                locked: true,
+            },
+        ]
+    }
+
+    #[test]
+    fn a_track_table_survives_the_round_trip() {
+        let dir = tempfile::tempdir().unwrap();
+        let path = dir.path().join("tracks.kuvatin");
+        let mut doc = sample();
+        doc.tracks = tracks();
+        doc.save(&path).unwrap();
+        assert_eq!(ProjectFile::load(&path).unwrap(), doc);
+    }
+
+    /// A project nobody renamed, muted or locked is written exactly as before.
+    #[test]
+    fn an_untouched_track_table_is_left_out_of_the_file() {
+        let mut doc = sample();
+        doc.tracks = vec![TrackRecord::default(); 3];
+        let text = toml::to_string_pretty(&doc).unwrap();
+        assert!(!text.contains("tracks"), "{text}");
+        assert_eq!(text, toml::to_string_pretty(&sample()).unwrap());
+    }
+
+    /// Once one track has something to say, every row is written, so the
+    /// count survives; a row at its defaults is a bare header. The format
+    /// stays at version 1: the table is additive.
+    #[test]
+    fn a_touched_table_writes_every_row_and_only_what_is_set() {
+        let mut doc = sample();
+        doc.tracks = tracks();
+        let text = toml::to_string_pretty(&doc).unwrap();
+        assert_eq!(text.matches("[[tracks]]").count(), 3, "{text}");
+        assert!(text.contains("name = \"Dialogue\""), "{text}");
+        assert!(
+            text.contains("muted = true") && text.contains("locked = true"),
+            "{text}"
+        );
+        assert!(
+            !text.contains("= false") && !text.contains("name = \"\""),
+            "{text}"
+        );
+        assert!(text.contains("version = 1"), "{text}");
     }
 }
diff --git a/crates/kuvatin-video/src/lib.rs b/crates/kuvatin-video/src/lib.rs
index 362df18..d525fc3 100644
--- a/crates/kuvatin-video/src/lib.rs
+++ b/crates/kuvatin-video/src/lib.rs
@@ -12,7 +12,7 @@ pub struct Frame {
 pub mod document;
 pub mod project;
 pub mod sequence;
-pub use document::{path_from_uri, ClipRecord, LayoutRecord, ProjectFile};
+pub use document::{path_from_uri, ClipRecord, LayoutRecord, ProjectFile, TrackRecord};
 pub use project::{
     hardware_encoding_available, is_hardware_encoder, normalize_render_size, thumbnail,
     thumbnail_uri, warm_asset, warm_asset_uri, waveform_uri, ClipGeom, ClipId, ClipInfo, Encoder,
```

- [ ] **Step 2: Run the document tests**

Run: `cargo test -p kuvatin-video -- --test-threads=1 document::`
Expected: 11 passed. Before the change, the new tests do not compile (`cannot find type TrackRecord`); `a_file_written_before_speed_existed_still_opens` now also pins that a file with no table opens with an empty one.

- [ ] **Step 3: Gates and commit**

```bash
git add crates/kuvatin-video/src/document.rs crates/kuvatin-video/src/lib.rs
git commit -m "A project file can hold a track table, and leaves it out when no track has anything to say" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: The engine silences a track's sound

`Project` keeps a `mutes` vector, per track position, as the interface last said. `set_track_mutes` switches each layer's activity for the timeline's audio track only, so a muted track's pictures still show; `track_muted` reads it back from GES. `layer()` makes a layer for a muted position silent, `move_track` permutes the vector (and now makes any layers it needs, so a row with no layer can be dragged), and `apply_document` starts a loaded project from no mutes. `set_track_mutes` repaints but does not mark unsaved work (departure 2); `mark_unsaved` is for the interface.

**Files:**
- Modify: `crates/kuvatin-video/src/project.rs` (the `Project` struct and `new`, `mark_saved`, `layer`, `track_count`, `apply_document`, `move_track`, and the tests after `undo_pruning_leaves_one_track_on_an_empty_timeline` and before `saving_and_loading_both_clear_unsaved_work`)

- [ ] **Step 1: Apply the diff**

```diff
diff --git a/crates/kuvatin-video/src/project.rs b/crates/kuvatin-video/src/project.rs
index a7a6fa9..ea1346a 100644
--- a/crates/kuvatin-video/src/project.rs
+++ b/crates/kuvatin-video/src/project.rs
@@ -1108,6 +1108,12 @@ fn set_clip_frame(clip: &ges::Clip, posx: i32, posy: i32, width: i32, height: i3
 pub struct Project {
     timeline: ges::Timeline,
     layers: Vec<ges::Layer>,
+    /// Per track position, top first, whether its sound is silenced, as the
+    /// interface last said (see [`Project::set_track_mutes`]). A position past
+    /// the end is audible. It describes the interface's track rows, not the
+    /// layers: a row keeps its mute when its last clip leaves and its layer is
+    /// pruned, so the layer made for the next clip there arrives silent.
+    mutes: Vec<bool>,
     pipeline: ges::Pipeline,
     /// Clips by ID, so the GUI can edit them (slide/trim/transform). An ID is
     /// the GES name a clip was placed under, or, for a restored clip, the ID it
@@ -1234,6 +1240,7 @@ impl Project {
         Ok(Self {
             timeline,
             layers: vec![layer],
+            mutes: Vec::new(),
             pipeline,
             clips: HashMap::new(),
             dirty: std::cell::Cell::new(false),
@@ -1274,6 +1281,13 @@ impl Project {
         self.unsaved.set(false);
     }
 
+    /// The interface changed something saved with the project that the engine
+    /// does not hold, or cannot tell was saved (a track's name, lock or mute,
+    /// or an undo of one). Closing now would lose it.
+    pub fn mark_unsaved(&self) {
+        self.unsaved.set(true);
+    }
+
     /// Current composited canvas ("viewport") size in px.
     pub fn canvas_size(&self) -> (i32, i32) {
         (self.canvas_w, self.canvas_h)
@@ -1305,9 +1319,15 @@ impl Project {
     }
 
     /// Ensure at least `index + 1` layers exist; return the layer at `index`.
+    /// A layer made for a muted position arrives silent: without that, a clip
+    /// dropped onto a muted track that had no layer yet would be heard.
     fn layer(&mut self, index: usize) -> ges::Layer {
         while self.layers.len() <= index {
-            self.layers.push(self.timeline.append_layer());
+            let layer = self.timeline.append_layer();
+            if self.mutes.get(self.layers.len()).copied().unwrap_or(false) {
+                self.apply_mute(&layer, true);
+            }
+            self.layers.push(layer);
         }
         self.layers[index].clone()
     }
@@ -1902,6 +1922,63 @@ impl Project {
         self.layers.len()
     }
 
+    /// Silence the sound of the tracks whose flag is set, and remember the
+    /// whole vector. Positional, top track first; a track past the end is
+    /// audible. Only the timeline's audio track is switched off for a layer,
+    /// never the video one: a muted track's pictures still show. Inert while
+    /// rendering, like every other edit.
+    ///
+    /// Repaints, but does not count as unsaved work: the interface drives
+    /// this for solo too, which is not saved. It marks an explicit mute
+    /// itself ([`Self::mark_unsaved`]). A vector that changes nothing asks
+    /// GES for nothing.
+    pub fn set_track_mutes(&mut self, mutes: &[bool]) {
+        if self.rendering.get() {
+            return;
+        }
+        self.mutes = mutes.to_vec();
+        let mut changed = false;
+        for (i, layer) in self.layers.iter().enumerate() {
+            let muted = self.mutes.get(i).copied().unwrap_or(false);
+            changed |= self.apply_mute(layer, muted);
+        }
+        if changed {
+            self.commit();
+            self.dirty.set(true);
+        }
+    }
+
+    /// Whether the track at `track` is silent right now, read back from GES.
+    /// False for a track that has no layer yet.
+    pub fn track_muted(&self, track: usize) -> bool {
+        self.layers
+            .get(track)
+            .is_some_and(|layer| self.is_silent(layer))
+    }
+
+    /// The timeline's audio tracks: exactly one, from `new_audio_video`.
+    fn audio_tracks(&self) -> Vec<ges::Track> {
+        self.timeline
+            .tracks()
+            .into_iter()
+            .filter(|t| t.track_type() == ges::TrackType::AUDIO)
+            .collect()
+    }
+
+    fn is_silent(&self, layer: &ges::Layer) -> bool {
+        self.audio_tracks()
+            .iter()
+            .any(|t| !layer.is_active_for_track(t))
+    }
+
+    /// Make `layer`'s sound match `muted`. True if that changed it.
+    fn apply_mute(&self, layer: &ges::Layer, muted: bool) -> bool {
+        if self.is_silent(layer) == muted {
+            return false;
+        }
+        layer.set_active_for_tracks(!muted, &self.audio_tracks())
+    }
+
     /// Move a clip to `track`, creating the layer if `track` is one past the last
     /// (a new bottom track). Returns the resulting track index.
     pub fn move_clip_to_track(&mut self, id: &ClipId, track: usize) -> Option<usize> {
@@ -2104,6 +2181,9 @@ impl Project {
         for id in self.clips.keys().cloned().collect::<Vec<_>>() {
             self.remove_clip(&ClipId(id));
         }
+        // The old project's mutes are not this one's. The interface tells the
+        // engine the new ones once it has built the new track rows.
+        self.set_track_mutes(&[]);
         self.set_canvas_size(doc.canvas_w, doc.canvas_h);
         let mut missing = Vec::new();
         for rec in &doc.clips {
@@ -2170,18 +2250,25 @@ impl Project {
     }
 
     /// Reorder tracks: move the track at `from` to position `to` (0 = top).
+    /// Either may be a row the engine has no layer for yet (a track added
+    /// with "+ New track" gets one only when a clip lands on it): the layers
+    /// up to it are made first, so the interface's rows and the layers move
+    /// together. The track's mute moves with it.
     pub fn move_track(&mut self, from: usize, to: usize) {
-        if self.rendering.get()
-            || from >= self.layers.len()
-            || to >= self.layers.len()
-            || from == to
-        {
+        if self.rendering.get() || from == to {
             return;
         }
+        let last = from.max(to);
+        self.layer(last);
         let layer = self.layers[from].clone();
         let _ = self.timeline.move_layer(&layer, to as u32);
         // Resync our layer vec to the new priority order.
         self.layers = self.timeline.layers();
+        if self.mutes.len() <= last {
+            self.mutes.resize(last + 1, false);
+        }
+        let muted = self.mutes.remove(from);
+        self.mutes.insert(to, muted);
         self.commit();
         self.touched();
     }
@@ -2977,6 +3064,18 @@ mod tests {
         );
     }
 
+    /// A track's name or lock lives in the interface, which says when one
+    /// changed: work the engine could not have noticed.
+    #[test]
+    fn unsaved_work_can_be_marked_by_the_interface() {
+        let project = Project::new(|_f| {}).expect("project");
+        assert!(!project.has_unsaved_work());
+        project.mark_unsaved();
+        assert!(project.has_unsaved_work());
+        project.mark_saved();
+        assert!(!project.has_unsaved_work());
+    }
+
     #[test]
     fn saving_and_loading_both_clear_unsaved_work() {
         let mut project = Project::new(|_f| {}).expect("project");
@@ -4549,6 +4648,190 @@ mod tests {
         let _ = std::fs::remove_dir_all(&dir);
     }
 
+    /// A mute switches off a layer's sound and nothing else: the pictures on
+    /// a muted track still show.
+    #[test]
+    fn track_mute_silences_the_sound_and_keeps_the_picture() {
+        let (dir, png, mut project) = undo_fixture("mute-audio-only");
+        project
+            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(2.0))
+            .expect("a");
+        project.set_track_mutes(&[true]);
+        assert!(project.track_muted(0));
+        let video: Vec<ges::Track> = project
+            .timeline
+            .tracks()
+            .into_iter()
+            .filter(|t| t.track_type() == ges::TrackType::VIDEO)
+            .collect();
+        assert_eq!(video.len(), 1);
+        assert!(
+            project.layers[0].is_active_for_track(&video[0]),
+            "the picture stays"
+        );
+        project.set_track_mutes(&[false]);
+        assert!(!project.track_muted(0));
+        let _ = std::fs::remove_dir_all(&dir);
+    }
+
+    #[test]
+    fn track_mute_reads_false_for_a_track_with_no_layer() {
+        let (dir, _png, mut project) = undo_fixture("mute-no-layer");
+        project.set_track_mutes(&[false, false, false, true]);
+        assert_eq!(project.track_count(), 1);
+        assert!(!project.track_muted(3), "no layer, nothing to silence");
+        let _ = std::fs::remove_dir_all(&dir);
+    }
+
+    /// A drop onto a muted row that has no layer yet must not unmute it: the
+    /// layer made for the clip arrives silent, the ones above it as their
+    /// own flags say.
+    #[test]
+    fn track_mute_arrives_on_a_layer_made_later() {
+        let (dir, png, mut project) = undo_fixture("mute-later");
+        let a = project
+            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(2.0))
+            .expect("a");
+        project.set_track_mutes(&[false, false, true]);
+        assert_eq!(project.move_clip_to_track(&a, 2), Some(2));
+        assert!(project.track_muted(2));
+        assert!(!project.track_muted(0) && !project.track_muted(1));
+        let _ = std::fs::remove_dir_all(&dir);
+    }
+
+    /// The mute is the track's, not the position's: moving the track moves
+    /// it, in GES and in the vector the engine remembers. This also pins that
+    /// GES's `move_layer` permutes the layers as a removal followed by an
+    /// insertion, which `move_track` assumes of the vector.
+    #[test]
+    fn track_mute_moves_with_its_track() {
+        let (dir, png, mut project) = undo_fixture("mute-move");
+        let ids: Vec<ClipId> = (0..3)
+            .map(|t| {
+                project
+                    .add_clip(&png, t, secs(0.0), Duration::ZERO, secs(2.0))
+                    .expect("clip")
+            })
+            .collect();
+        project.set_track_mutes(&[true, false, false]);
+        project.move_track(0, 2);
+        assert_eq!(
+            ids.iter().map(|id| project.clip_track(id)).collect::<Vec<_>>(),
+            vec![Some(2), Some(0), Some(1)],
+            "the top track went to the bottom, the others moved up"
+        );
+        assert_eq!(
+            (0..3).map(|t| project.track_muted(t)).collect::<Vec<_>>(),
+            vec![false, false, true]
+        );
+        // GES moved the silent layer itself; the remembered vector must have
+        // moved too. Empty the bottom track so its layer goes, then bring a
+        // clip back to it: the layer made for it follows the vector.
+        assert!(project.remove_clip(&ids[0]));
+        assert_eq!(project.track_count(), 2);
+        assert_eq!(project.move_clip_to_track(&ids[1], 2), Some(2));
+        assert!(
+            project.track_muted(2),
+            "the vector moved with the track"
+        );
+        let _ = std::fs::remove_dir_all(&dir);
+    }
+
+    /// A row added with "+ New track" has no layer until a clip lands on it,
+    /// but it can still be dragged, and a track can be dragged onto it.
+    #[test]
+    fn track_mute_moves_to_a_row_with_no_layer_yet() {
+        let (dir, png, mut project) = undo_fixture("mute-move-lazy");
+        let a = project
+            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(2.0))
+            .expect("a");
+        project.set_track_mutes(&[true]);
+        project.move_track(0, 2);
+        assert_eq!(project.track_count(), 3);
+        assert_eq!(project.clip_track(&a), Some(2));
+        assert!(project.track_muted(2) && !project.track_muted(0));
+        let _ = std::fs::remove_dir_all(&dir);
+    }
+
+    /// A track row outlives its layer. Deleting the last clip on a muted
+    /// track prunes the layer; the next clip there must still be silent.
+    #[test]
+    fn track_mute_survives_the_last_clip_leaving() {
+        let (dir, png, mut project) = undo_fixture("mute-prune");
+        let a = project
+            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(2.0))
+            .expect("a");
+        let b = project
+            .add_clip(&png, 1, secs(0.0), Duration::ZERO, secs(2.0))
+            .expect("b");
+        project.set_track_mutes(&[false, true]);
+        assert!(project.remove_clip(&b));
+        assert_eq!(project.track_count(), 1, "the empty bottom layer went");
+        assert_eq!(project.move_clip_to_track(&a, 1), Some(1));
+        assert!(project.track_muted(1));
+        let _ = std::fs::remove_dir_all(&dir);
+    }
+
+    /// Opening a project starts from its own mutes, not the last one's: the
+    /// layer the engine keeps across the load must not stay silent.
+    #[test]
+    fn track_mute_does_not_leak_into_the_next_project() {
+        let (dir, png, mut project) = undo_fixture("mute-load");
+        project
+            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(2.0))
+            .expect("a");
+        project
+            .add_clip(&png, 1, secs(0.0), Duration::ZERO, secs(2.0))
+            .expect("b");
+        let doc = project.to_document();
+        project.set_track_mutes(&[true, true]);
+        project.apply_document(&doc).expect("apply");
+        assert!(!project.track_muted(0) && !project.track_muted(1));
+        let _ = std::fs::remove_dir_all(&dir);
+    }
+
+    /// While a render owns the pipeline a mute changes nothing, and one set
+    /// before it is still there when the preview comes back.
+    #[test]
+    fn track_mute_is_inert_while_rendering_and_outlives_it() {
+        let (dir, png, mut project) = undo_fixture("mute-render");
+        project
+            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(2.0))
+            .expect("a");
+        project.set_track_mutes(&[true]);
+        project.prepare_render().expect("prepare");
+        project.set_track_mutes(&[false]);
+        assert!(project.track_muted(0), "inert while rendering");
+        project.begin_restore().expect("restore");
+        let end = std::time::Instant::now() + Duration::from_secs(10);
+        while project.restore_ready() == Step::Pending && std::time::Instant::now() < end {
+            std::thread::sleep(Duration::from_millis(20));
+        }
+        assert!(!project.is_rendering(), "the preview came back");
+        assert!(project.track_muted(0), "the mute outlived the render");
+        let _ = std::fs::remove_dir_all(&dir);
+    }
+
+    /// Solo drives the same call as a mute, and solo is not saved: a mute
+    /// repaints but does not call the project unsaved. A vector that changes
+    /// nothing commits nothing.
+    #[test]
+    fn track_mute_repaints_without_counting_as_unsaved_work() {
+        let (dir, png, mut project) = undo_fixture("mute-unsaved");
+        project
+            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(2.0))
+            .expect("a");
+        project.mark_saved();
+        project.dirty.set(false);
+        project.set_track_mutes(&[true]);
+        assert!(!project.has_unsaved_work());
+        assert!(project.dirty.get(), "the preview repaints");
+        let commits = project.commits.get();
+        project.set_track_mutes(&[true, false]);
+        assert_eq!(project.commits.get(), commits, "nothing changed");
+        let _ = std::fs::remove_dir_all(&dir);
+    }
+
     /// The engine zooms a clip past the canvas and the read-back must say so:
     /// a 1.0 ceiling in `clip_layout` once snapped every zoomed clip back to
     /// fit whenever the inspector refreshed. Through a save and a load too.
```

- [ ] **Step 2: Run the engine tests**

Run: `cargo test -p kuvatin-video -- --test-threads=1 track_mute_ unsaved_work moves_clips_and_tracks undo_`
Expected: 34 passed (9 `track_mute_`, 3 `unsaved_work`, and the undo and track tests around them, the real-media ones skipping without `GST_TEST_FILE`).

`track_mute_moves_with_its_track` is the one that pins the vector's permutation: GES moves the silent layer object by itself, so the test also empties the bottom track and brings a clip back to it, which only a moved vector makes silent. Measured: with the two `mutes.remove(from)` / `mutes.insert(to, …)` lines taken out it fails with "the vector moved with the track".

- [ ] **Step 3: Gates and commit**

```bash
git add crates/kuvatin-video/src/project.rs
git commit -m "The engine can silence a track's sound and keeps the mute with the track" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: A mute is proven on real media

Two live-media tests. One renders the same clip three times and measures the export's sound with the engine's own waveform decoder: on an audible track, on a track muted before its layer existed, and on an existing muted track the clip moved onto. The other mutes, and mutes and unmutes, a track under a playing preview six times and checks the playhead keeps moving: a change of a layer's activity is the kind of change that froze the preview for a speed effect.

**Files:**
- Modify: `crates/kuvatin-video/src/project.rs` (after `a_speed_change_during_playback_leaves_the_preview_playing`)

- [ ] **Step 1: Apply the diff**

```diff
diff --git a/crates/kuvatin-video/src/project.rs b/crates/kuvatin-video/src/project.rs
index ea1346a..bbea034 100644
--- a/crates/kuvatin-video/src/project.rs
+++ b/crates/kuvatin-video/src/project.rs
@@ -5171,6 +5171,119 @@ mod tests {
         }
     }
 
+    /// Render the timeline to a small WebM and measure its sound: the share
+    /// of a 400 × 200 waveform picture the export's audio inks. The CI
+    /// fixture's sine tone fills most of it; silence fills none, and a lossy
+    /// codec's rounding a sliver at most.
+    fn rendered_ink(project: &Project, tag: &str) -> f64 {
+        let out = scratch(tag).join("out.webm");
+        let _ = std::fs::remove_file(&out);
+        project
+            .begin_render(
+                &out,
+                ExportSettings {
+                    codec: VideoCodec::Vp8,
+                    width: 320,
+                    height: 180,
+                    fps: 30,
+                    bitrate_kbps: 500,
+                    encoder: Encoder::Auto,
+                },
+            )
+            .expect("begin_render");
+        let end = std::time::Instant::now() + Duration::from_secs(60);
+        loop {
+            match project.render_status() {
+                RenderStatus::Done => break,
+                RenderStatus::Failed(e) => panic!("render failed: {e}"),
+                RenderStatus::Rendering(_) => {
+                    assert!(std::time::Instant::now() < end, "render never finished");
+                    std::thread::sleep(Duration::from_millis(50));
+                }
+            }
+        }
+        project.end_render().expect("end_render");
+        let uri = gst::glib::filename_to_uri(&out, None).expect("uri");
+        let (frame, _) = waveform_uri(&uri, 400, 200).expect("the export has a sound stream");
+        let ink = frame.rgba.chunks_exact(4).filter(|p| p[3] > 0).count();
+        ink as f64 / (400.0 * 200.0)
+    }
+
+    /// A muted track is silent in the export, not only in the preview. The
+    /// same clip renders with its sound on an audible track and without it on
+    /// a muted one: one muted before the clip's layer existed, and one that
+    /// existed and was muted before the clip moved onto it. Needs
+    /// `GST_TEST_FILE`, a video with sound.
+    #[test]
+    fn a_muted_track_is_silent_in_the_export() {
+        let Some(path) = std::env::var_os("GST_TEST_FILE") else {
+            eprintln!("skipping a_muted_track_is_silent_in_the_export: set GST_TEST_FILE");
+            return;
+        };
+        let path = std::path::PathBuf::from(path);
+
+        let mut audible = Project::new(|_f| {}).expect("project");
+        audible.append_clip(&path, 1, None).expect("clip");
+        let loud = rendered_ink(&audible, "mute-export-loud");
+        assert!(loud > 0.3, "the fixture's tone fills the waveform: {loud}");
+        drop(audible);
+
+        let mut first = Project::new(|_f| {}).expect("project");
+        first.set_track_mutes(&[false, true]);
+        first.append_clip(&path, 1, None).expect("clip");
+        let quiet = rendered_ink(&first, "mute-export-first");
+        assert!(
+            quiet < loud / 10.0,
+            "muted before its layer existed: {quiet} against {loud}"
+        );
+        drop(first);
+
+        let mut moved = Project::new(|_f| {}).expect("project");
+        let clip = moved.append_clip(&path, 0, None).expect("clip");
+        moved.layer(1);
+        moved.set_track_mutes(&[false, true]);
+        assert_eq!(moved.move_clip_to_track(&clip.id, 1), Some(1));
+        let quiet = rendered_ink(&moved, "mute-export-moved");
+        assert!(
+            quiet < loud / 10.0,
+            "moved onto a muted track: {quiet} against {loud}"
+        );
+    }
+
+    /// A mute changes a layer's activity under a running pipeline, the same
+    /// kind of change that froze the preview for a speed effect (see
+    /// `after_time_effects`). Playback must carry on through a mute, and
+    /// through a mute and an unmute in a row. Needs `GST_TEST_FILE`.
+    #[test]
+    fn muting_a_track_during_playback_leaves_the_preview_playing() {
+        let Some(path) = std::env::var_os("GST_TEST_FILE") else {
+            eprintln!("skipping muting_a_track_during_playback_...: set GST_TEST_FILE");
+            return;
+        };
+        for round in 0..6 {
+            let mut project = Project::new(|_f| {}).expect("project");
+            project
+                .append_clip(Path::new(&path), 1, None)
+                .expect("clip");
+            project.play().expect("play");
+            wait_settled(&project);
+            project.set_track_mutes(&[false, true]);
+            if round >= 3 {
+                wait_settled(&project);
+                project.set_track_mutes(&[false, false]);
+            }
+            wait_settled(&project);
+            let from = project.position().expect("a position");
+            std::thread::sleep(Duration::from_millis(800));
+            let to = project.position().expect("a position");
+            assert!(
+                to > from + Duration::from_millis(400),
+                "round {round}: frozen after a mute, {from:?} -> {to:?}"
+            );
+            let _ = project.pause();
+        }
+    }
+
     /// Not a gate: a measurement of what this GStreamer does with a speed
     /// change, which the speed work was built on (see the Amendments of
     /// `docs/superpowers/plans/2026-09-23-editor-clip-edits.md`). Run it by
```

- [ ] **Step 2: Run them against the fixture**

Run: `cargo test -p kuvatin-video -- --test-threads=1 a_muted_track_is_silent muting_a_track_during`
Expected: 2 passed. Measured twice: the audible export inks 0.812 of the 400 × 200 picture; both muted exports ink exactly 0. Without `GST_TEST_FILE` both skip and say so.

- [ ] **Step 3: Gates and commit**

```bash
git add crates/kuvatin-video/src/project.rs
git commit -m "A muted track is proven silent in the export, and playback carries on through a mute" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Track rows become TimelineTrack

The interface's track list was `[string]`, regenerated from the index after every undo and every open, so a name could not survive. It becomes `[TimelineTrack]`: name, muted, soloed, locked. Nothing new is drawn yet. The header shows the name or "Track N", every `.length` use is unchanged, and new rows (a drop onto a new bottom track, "+ New track") are default rows. `set_track_rows` still takes a count for now; Task 5 gives it a table.

**Files:**
- Modify: `crates/kuvatin/ui/app.slint` (the struct after `TimelineClip`, the `timeline-tracks` property, the band height, the header and stripe loops, the new-track row and the drop banner)
- Modify: `crates/kuvatin/src/gui/video/mod.rs`, `project_file.rs`, `timeline.rs`, `undo.rs`

- [ ] **Step 1: Apply the diff**

```diff
diff --git a/crates/kuvatin/src/gui/video/mod.rs b/crates/kuvatin/src/gui/video/mod.rs
index 20e290f..1a0ac5f 100644
--- a/crates/kuvatin/src/gui/video/mod.rs
+++ b/crates/kuvatin/src/gui/video/mod.rs
@@ -10,7 +10,7 @@ mod transport;
 mod undo;
 mod waves;
 
-use super::{show_error, AppWindow, ClipKind, TimelineClip, VideoAsset};
+use super::{show_error, AppWindow, ClipKind, TimelineClip, TimelineTrack, VideoAsset};
 use export::ExportState;
 use import::ImportState;
 use slint::{
@@ -56,9 +56,12 @@ pub(super) struct VideoState {
     pub(super) assets: Rc<VecModel<VideoAsset>>,
     pub(super) bin_paths: Rc<RefCell<Vec<PathBuf>>>,
     pub(super) tl_clips: Rc<VecModel<TimelineClip>>,
-    /// Timeline tracks (GES layers, top = index 0 = composited on top). Kept
-    /// mutable so dragging a clip onto a new track can grow the list.
-    pub(super) tracks: Rc<VecModel<SharedString>>,
+    /// Timeline track rows (GES layers, top = index 0 = composited on top):
+    /// the truth about how many tracks there are and what each is called, and
+    /// whether it is muted, soloed or locked. A row can exist before its
+    /// layer does (see `on_add_track`). Kept mutable so dragging a clip onto a
+    /// new track can grow the list.
+    pub(super) tracks: Rc<VecModel<TimelineTrack>>,
     /// Index of the selected timeline clip (for the inspector), or -1.
     pub(super) sel_idx: Rc<Cell<i32>>,
     /// Latest inspector transform awaiting a coalesced apply on the UI timer.
@@ -82,11 +85,9 @@ impl VideoState {
         ui.set_video_clips(ModelRc::from(assets.clone()));
         let tl_clips = Rc::new(VecModel::<TimelineClip>::from(Vec::<TimelineClip>::new()));
         ui.set_timeline_clips(ModelRc::from(tl_clips.clone()));
-        let tracks = Rc::new(VecModel::<SharedString>::from(vec![
-            SharedString::from("Track 1"),
-            SharedString::from("Track 2"),
-        ]));
-        ui.set_timeline_track_labels(ModelRc::from(tracks.clone()));
+        // Two unnamed, audible, unlocked tracks: what an empty project shows.
+        let tracks = Rc::new(VecModel::from(vec![TimelineTrack::default(); 2]));
+        ui.set_timeline_tracks(ModelRc::from(tracks.clone()));
         ui.set_insp_scale_min(MIN_SCALE_PCT);
         ui.set_insp_scale_max(MAX_SCALE_PCT);
         let labels: Vec<SharedString> = SPEEDS.iter().map(|r| format!("{r}×").into()).collect();
diff --git a/crates/kuvatin/src/gui/video/project_file.rs b/crates/kuvatin/src/gui/video/project_file.rs
index fe43e7f..f193446 100644
--- a/crates/kuvatin/src/gui/video/project_file.rs
+++ b/crates/kuvatin/src/gui/video/project_file.rs
@@ -5,7 +5,7 @@
 //! says what could not be found. Opening replaces the timeline, so a timeline
 //! with anything on it asks first.
 
-use super::{ClipKind, TimelineClip, VideoState};
+use super::{ClipKind, TimelineClip, TimelineTrack, VideoState};
 use crate::gui::{name_list, show_error, show_info, AppWindow, VideoAsset};
 use slint::{ComponentHandle, Image, Model, SharedString, VecModel};
 use std::cell::RefCell;
@@ -236,11 +236,7 @@ fn restore_models(
         .max()
         .unwrap_or(0)
         .max(2);
-    st.tracks.set_vec(
-        (0..needed)
-            .map(|i| SharedString::from(format!("Track {}", i + 1)))
-            .collect::<Vec<_>>(),
-    );
+    st.tracks.set_vec(vec![TimelineTrack::default(); needed]);
 
     // Media bin: one row per distinct source, and the sequence specs come back
     // with it so a bin click re-adds the sequence rather than a single still.
@@ -363,7 +359,7 @@ pub(super) struct VideoHandles {
     pub(super) assets: Rc<VecModel<VideoAsset>>,
     pub(super) bin_paths: Rc<RefCell<Vec<PathBuf>>>,
     pub(super) tl_clips: Rc<VecModel<TimelineClip>>,
-    pub(super) tracks: Rc<VecModel<SharedString>>,
+    pub(super) tracks: Rc<VecModel<TimelineTrack>>,
     pub(super) sel_idx: Rc<std::cell::Cell<i32>>,
     pub(super) history: super::undo::TimelineHistory,
     pub(super) waves: super::waves::Waves,
diff --git a/crates/kuvatin/src/gui/video/timeline.rs b/crates/kuvatin/src/gui/video/timeline.rs
index 611163b..37f0ae9 100644
--- a/crates/kuvatin/src/gui/video/timeline.rs
+++ b/crates/kuvatin/src/gui/video/timeline.rs
@@ -3,7 +3,7 @@
 
 use super::undo::{Recorder, StepKind};
 use super::{VideoState, MAX_SCALE_PCT, MIN_SCALE_PCT, SPEEDS};
-use crate::gui::{show_error, AppWindow, ClipKind, TimelineClip};
+use crate::gui::{show_error, AppWindow, ClipKind, TimelineClip, TimelineTrack};
 use slint::{ComponentHandle, Model, SharedString, VecModel};
 use std::cell::RefCell;
 use std::rc::Rc;
@@ -144,11 +144,11 @@ pub(super) fn wire(ui: &AppWindow, st: &VideoState) {
                         row.track = t as i32;
                     }
                 }
-                // Grow the gutter labels to match any newly created track.
+                // Grow the rows to match any newly created track. A new
+                // track starts unnamed, audible, unsoloed and unlocked.
                 let new_count = p.track_count();
                 while tracks.row_count() < new_count {
-                    let n = tracks.row_count() + 1;
-                    tracks.push(SharedString::from(format!("Track {n}")));
+                    tracks.push(TimelineTrack::default());
                 }
             }
             rec.record(Some(&*p), StepKind::Move, Some(row.id.as_str()), before);
@@ -199,8 +199,7 @@ pub(super) fn wire(ui: &AppWindow, st: &VideoState) {
             }
             // No clip changes, so no project is needed to record it.
             let before = rec.before(None);
-            let n = tracks.row_count() + 1;
-            tracks.push(SharedString::from(format!("Track {n}")));
+            tracks.push(TimelineTrack::default());
             rec.record(None, StepKind::AddTrack, None, before);
         });
     }
diff --git a/crates/kuvatin/src/gui/video/undo.rs b/crates/kuvatin/src/gui/video/undo.rs
index 871e7d9..988e240 100644
--- a/crates/kuvatin/src/gui/video/undo.rs
+++ b/crates/kuvatin/src/gui/video/undo.rs
@@ -7,7 +7,7 @@
 
 use super::project_file::kind_of;
 use crate::gui::history::{History, Step};
-use crate::gui::{name_list, show_error, AppWindow, TimelineClip};
+use crate::gui::{name_list, show_error, AppWindow, TimelineClip, TimelineTrack};
 use kuvatin_video::ClipRecord;
 use slint::{ComponentHandle, Model, SharedString, VecModel};
 use std::cell::{Cell, RefCell};
@@ -339,14 +339,14 @@ pub(super) fn applied_from_engine(
     gone.chain(present).collect()
 }
 
-/// Make the timeline show exactly `count` track rows, named in order.
-pub(super) fn set_track_rows(tracks: &VecModel<SharedString>, count: usize) {
+/// Make the timeline show exactly `count` track rows. Rows past the end go;
+/// new ones are unnamed.
+pub(super) fn set_track_rows(tracks: &VecModel<TimelineTrack>, count: usize) {
     while tracks.row_count() > count {
         tracks.remove(tracks.row_count() - 1);
     }
     while tracks.row_count() < count {
-        let n = tracks.row_count() + 1;
-        tracks.push(SharedString::from(format!("Track {n}")));
+        tracks.push(TimelineTrack::default());
     }
 }
 
@@ -357,7 +357,7 @@ pub(super) fn set_track_rows(tracks: &VecModel<SharedString>, count: usize) {
 pub(super) struct Recorder {
     pub(super) history: TimelineHistory,
     pub(super) tl_clips: Rc<VecModel<TimelineClip>>,
-    pub(super) tracks: Rc<VecModel<SharedString>>,
+    pub(super) tracks: Rc<VecModel<TimelineTrack>>,
     pub(super) ui: slint::Weak<AppWindow>,
 }
 
@@ -1085,11 +1085,7 @@ mod tests {
         Recorder {
             history: Rc::new(RefCell::new(History::new())),
             tl_clips: Rc::new(VecModel::from(rows)),
-            tracks: Rc::new(VecModel::from(
-                (0..tracks)
-                    .map(|i| SharedString::from(format!("Track {}", i + 1)))
-                    .collect::<Vec<_>>(),
-            )),
+            tracks: Rc::new(VecModel::from(vec![TimelineTrack::default(); tracks])),
             ui: slint::Weak::default(),
         }
     }
@@ -1114,7 +1110,7 @@ mod tests {
     fn a_new_track_is_recorded_without_a_project() {
         let r = recorder(Vec::new(), 2);
         let before = r.before(None);
-        r.tracks.push("Track 3".into());
+        r.tracks.push(TimelineTrack::default());
         r.record(None, StepKind::AddTrack, None, before);
         let history = r.history.borrow();
         let s = history.peek_undo().expect("a step");
@@ -1162,13 +1158,10 @@ mod tests {
 
     #[test]
     fn track_rows_grow_and_shrink_to_a_count() {
-        let tracks = VecModel::from(vec![
-            SharedString::from("Track 1"),
-            SharedString::from("Track 2"),
-        ]);
+        let tracks = VecModel::from(vec![TimelineTrack::default(); 2]);
         set_track_rows(&tracks, 4);
         assert_eq!(tracks.row_count(), 4);
-        assert_eq!(tracks.row_data(3).unwrap().as_str(), "Track 4");
+        assert_eq!(tracks.row_data(3), Some(TimelineTrack::default()));
         set_track_rows(&tracks, 1);
         assert_eq!(tracks.row_count(), 1);
     }
diff --git a/crates/kuvatin/ui/app.slint b/crates/kuvatin/ui/app.slint
index bdbe7e1..1e282ec 100644
--- a/crates/kuvatin/ui/app.slint
+++ b/crates/kuvatin/ui/app.slint
@@ -37,6 +37,16 @@ export struct TimelineClip {
     wave-secs: float,  // how many seconds of source that image spans
 }
 
+// One timeline track row: its header in the gutter and its stripe in the
+// lane. Its index in `timeline-tracks` is the track, 0 = top, the numbering
+// TimelineClip.track uses.
+export struct TimelineTrack {
+    name: string,      // "" → the header shows "Track {t+1}"
+    muted: bool,
+    soloed: bool,      // for listening only: never saved, never undone
+    locked: bool,
+}
+
 // A media-bin entry: file name + a generated thumbnail.
 export struct VideoAsset {
     name: string,
@@ -113,7 +123,7 @@ export component AppWindow inherits Window {
 
     // Timeline editor.
     in property <[TimelineClip]> timeline-clips;
-    in property <[string]> timeline-track-labels;  // one per track, top row first
+    in property <[TimelineTrack]> timeline-tracks;  // one per track, top row first
     // Height of a single timeline track row. One source of truth: the band
     // height, header rows, lane stripes, clip Y and every drag/reorder distance
     // are all derived from this (change it here and the whole timeline follows).
@@ -1771,7 +1781,7 @@ export component AppWindow inherits Window {
                 Rectangle {
                     // Sized to the tracks + one "new track" drop row with breathing
                     // room below it (so a clip dragged onto the row stays fully visible).
-                    height: Math.max(140px, root.timeline-track-labels.length * root.track-h + 82px);
+                    height: Math.max(140px, root.timeline-tracks.length * root.track-h + 82px);
                     background: #101319;
                     border-width: 1px; border-color: Theme.line;
                     VerticalLayout {
@@ -1863,7 +1873,7 @@ export component AppWindow inherits Window {
                             VerticalLayout {
                                 width: 94px;
                                 // Drag a header up/down to reorder tracks.
-                                for lbl[t] in root.timeline-track-labels : hdr := Rectangle {
+                                for trk[t] in root.timeline-tracks : hdr := Rectangle {
                                     height: root.track-h;
                                     background: hta.pressed ? Theme.card : #13161c;
                                     animate background { duration: 100ms; }
@@ -1872,7 +1882,7 @@ export component AppWindow inherits Window {
                                     HorizontalLayout {
                                         padding-left: 9px; spacing: 6px;
                                         Text { text: "≡"; color: hta.has-hover ? Theme.muted2 : #5e6675; font-size: 11px; vertical-alignment: center; animate color { duration: 120ms; } }
-                                        Text { text: lbl; color: Theme.muted2; font-size: 9px; vertical-alignment: center; overflow: elide; }
+                                        Text { text: trk.name != "" ? trk.name : "Track " + (t + 1); color: Theme.muted2; font-size: 9px; vertical-alignment: center; overflow: elide; }
                                     }
                                     hta := TouchArea {
                                         mouse-cursor: ns-resize;
@@ -1882,7 +1892,7 @@ export component AppWindow inherits Window {
                                             if (ev.kind == PointerEventKind.down) { kbd.focus(); }
                                             if (ev.kind == PointerEventKind.up) {
                                                 if (Math.abs(hdr.ddy / 1px) > 15) {
-                                                    root.track-reordered(t, Math.clamp(t + Math.round(hdr.ddy / root.track-h), 0, root.timeline-track-labels.length - 1));
+                                                    root.track-reordered(t, Math.clamp(t + Math.round(hdr.ddy / root.track-h), 0, root.timeline-tracks.length - 1));
                                                 }
                                                 hdr.ddy = 0px;
                                             }
@@ -1944,7 +1954,7 @@ export component AppWindow inherits Window {
 
                                     // row background stripes (fixed; span the visible lane)
                                     VerticalLayout {
-                                        for lbl[t] in root.timeline-track-labels : Rectangle { height: root.track-h; border-width: 1px; border-color: #181c23; }
+                                        for trk[t] in root.timeline-tracks : Rectangle { height: root.track-h; border-width: 1px; border-color: #181c23; }
                                     }
 
                                     // clip blocks. The wrapper `blk` keeps the clip's MODEL
@@ -2160,8 +2170,8 @@ export component AppWindow inherits Window {
                                     // through. A dashed-look border keeps it reading as a drop
                                     // zone. Sits above the scrollbar with room to spare.
                                     Rectangle {
-                                        property <bool> active: root.clip-drag-target-row >= root.timeline-track-labels.length;
-                                        y: root.timeline-track-labels.length * root.track-h + 4px;
+                                        property <bool> active: root.clip-drag-target-row >= root.timeline-tracks.length;
+                                        y: root.timeline-tracks.length * root.track-h + 4px;
                                         animate y { duration: 160ms; easing: ease-out; }
                                         x: 4px; width: parent.width - 8px; height: 26px;
                                         border-radius: 7px;
@@ -2203,8 +2213,8 @@ export component AppWindow inherits Window {
         // It's purely a visual affordance: the drop target is decided by drag distance,
         // so the banner doesn't need to sit directly under the cursor.
         if root.clip-dragging : Rectangle {
-            property <bool> active: root.clip-drag-target-row >= root.timeline-track-labels.length;
-            property <length> band-h: Math.max(140px, root.timeline-track-labels.length * root.track-h + 82px);
+            property <bool> active: root.clip-drag-target-row >= root.timeline-tracks.length;
+            property <length> band-h: Math.max(140px, root.timeline-tracks.length * root.track-h + 82px);
             // Entrance: fade + rise so the banner glides in instead of popping.
             property <length> rise: 10px;
             opacity: 0;
```

- [ ] **Step 2: Build and test**

Run: `cargo test -p kuvatin`
Expected: 278 passed, 2 ignored. The window looks as it did.

- [ ] **Step 3: Gates and commit**

```bash
git add crates/kuvatin/ui/app.slint crates/kuvatin/src/gui/video
git commit -m "A timeline track row is a struct, not a generated label" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Undo keeps the track table

The capture's `tracks: usize` becomes `Vec<TrackRecord>`, so a step that changes only a name, a mute or a lock is not empty any more (with two counts it compared equal and the history dropped it), and an undo puts every row back as it was instead of regenerating the names. `set_track_rows` makes the rows match a table exactly, carrying each row's solo across. After an undo the engine is told what to silence and the project is marked unsaved. The new module `tracks.rs` starts with the pure helpers this needs.

**Files:**
- Create: `crates/kuvatin/src/gui/video/tracks.rs`
- Modify: `crates/kuvatin/src/gui/video/undo.rs`, `mod.rs`

- [ ] **Step 1: Apply the diff**

```diff
diff --git a/crates/kuvatin/src/gui/video/mod.rs b/crates/kuvatin/src/gui/video/mod.rs
index 1a0ac5f..6c9663a 100644
--- a/crates/kuvatin/src/gui/video/mod.rs
+++ b/crates/kuvatin/src/gui/video/mod.rs
@@ -6,6 +6,7 @@ pub(super) mod export;
 pub(super) mod import;
 mod project_file;
 mod timeline;
+mod tracks;
 mod transport;
 mod undo;
 mod waves;
diff --git a/crates/kuvatin/src/gui/video/tracks.rs b/crates/kuvatin/src/gui/video/tracks.rs
new file mode 100644
index 0000000..21a8863
--- /dev/null
+++ b/crates/kuvatin/src/gui/video/tracks.rs
@@ -0,0 +1,137 @@
+//! Per-track state in the Videos timeline: names, mutes, solo and locks.
+//!
+//! A track is its position (guiding decision 1 of the track controls
+//! design). The interface's rows are the truth; the engine is told what to
+//! silence and nothing else, and the undo history keeps the rows as
+//! [`TrackRecord`]s. Solo is the one flag a record leaves out: it is a way of
+//! listening, not an edit, so it is neither saved nor undone.
+
+use crate::gui::TimelineTrack;
+use kuvatin_video::TrackRecord;
+use slint::{Model, VecModel};
+
+/// What the engine should silence, given the rows as they are. While any
+/// track is soloed every other one is silent; an explicit mute wins over
+/// solo, so a track both soloed and muted stays silent.
+pub(super) fn effective_mutes(rows: &[TimelineTrack]) -> Vec<bool> {
+    let any_solo = rows.iter().any(|r| r.soloed);
+    rows.iter()
+        .map(|r| r.muted || (any_solo && !r.soloed))
+        .collect()
+}
+
+/// The rows as they are stored and undone: name, mute and lock. Solo is left
+/// out.
+pub(super) fn records(rows: &[TimelineTrack]) -> Vec<TrackRecord> {
+    rows.iter()
+        .map(|r| TrackRecord {
+            name: r.name.to_string(),
+            muted: r.muted,
+            locked: r.locked,
+        })
+        .collect()
+}
+
+/// The row a record draws as, carrying the solo flag it is given.
+pub(super) fn row(record: &TrackRecord, soloed: bool) -> TimelineTrack {
+    TimelineTrack {
+        name: record.name.as_str().into(),
+        muted: record.muted,
+        soloed,
+        locked: record.locked,
+    }
+}
+
+/// A model's rows, as a vector.
+pub(super) fn rows_of(tracks: &VecModel<TimelineTrack>) -> Vec<TimelineTrack> {
+    tracks.iter().collect()
+}
+
+/// Tell the engine what to silence, from the rows as they are. Idempotent:
+/// call it after anything that changes a mute, a solo, the order of the
+/// tracks or how many there are. It rewrites the engine's whole vector, so a
+/// missed call leaves the engine behind but never half-applied, and the next
+/// call catches it up.
+pub(super) fn push_mutes(project: &mut kuvatin_video::Project, tracks: &VecModel<TimelineTrack>) {
+    project.set_track_mutes(&effective_mutes(&rows_of(tracks)));
+}
+
+#[cfg(test)]
+pub(super) mod tests {
+    use super::*;
+
+    /// A row, for tests here and in the other timeline modules.
+    pub(in crate::gui::video) fn trk(
+        name: &str,
+        muted: bool,
+        soloed: bool,
+        locked: bool,
+    ) -> TimelineTrack {
+        TimelineTrack {
+            name: name.into(),
+            muted,
+            soloed,
+            locked,
+        }
+    }
+
+    #[test]
+    fn with_no_solo_the_mutes_are_the_rows_own() {
+        let rows = [
+            trk("", false, false, false),
+            trk("", true, false, false),
+            trk("", false, false, true),
+        ];
+        assert_eq!(effective_mutes(&rows), vec![false, true, false]);
+    }
+
+    /// Solo silences every other track, the ones already muted included.
+    #[test]
+    fn one_solo_silences_every_other_track() {
+        let rows = [
+            trk("", false, false, false),
+            trk("", false, true, false),
+            trk("", true, false, false),
+        ];
+        assert_eq!(effective_mutes(&rows), vec![true, false, true]);
+    }
+
+    /// With every track soloed there is no other track to silence.
+    #[test]
+    fn every_track_soloed_is_the_same_as_none() {
+        let rows = [trk("", false, true, false), trk("", true, true, false)];
+        assert_eq!(effective_mutes(&rows), vec![false, true]);
+    }
+
+    #[test]
+    fn a_muted_track_stays_silent_when_it_is_soloed() {
+        let rows = [trk("", true, true, false), trk("", false, false, false)];
+        assert_eq!(effective_mutes(&rows), vec![true, true]);
+    }
+
+    #[test]
+    fn a_record_keeps_name_mute_and_lock_and_drops_solo() {
+        let rows = [
+            trk("Dialogue", true, true, false),
+            trk("", false, false, true),
+        ];
+        let got = records(&rows);
+        assert_eq!(
+            got,
+            vec![
+                TrackRecord {
+                    name: "Dialogue".into(),
+                    muted: true,
+                    locked: false,
+                },
+                TrackRecord {
+                    name: String::new(),
+                    muted: false,
+                    locked: true,
+                },
+            ]
+        );
+        assert_eq!(row(&got[0], true), rows[0], "and a row comes back from it");
+        assert!(!row(&got[0], false).soloed);
+    }
+}
diff --git a/crates/kuvatin/src/gui/video/undo.rs b/crates/kuvatin/src/gui/video/undo.rs
index 988e240..c200258 100644
--- a/crates/kuvatin/src/gui/video/undo.rs
+++ b/crates/kuvatin/src/gui/video/undo.rs
@@ -6,9 +6,10 @@
 //! comes back gets the row it had, name and thumbnail included.
 
 use super::project_file::kind_of;
+use super::tracks;
 use crate::gui::history::{History, Step};
 use crate::gui::{name_list, show_error, AppWindow, TimelineClip, TimelineTrack};
-use kuvatin_video::ClipRecord;
+use kuvatin_video::{ClipRecord, TrackRecord};
 use slint::{ComponentHandle, Model, SharedString, VecModel};
 use std::cell::{Cell, RefCell};
 use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
@@ -44,16 +45,18 @@ impl StepKind {
     }
 }
 
-/// Every clip's record, by ID, and the number of track rows, at one moment.
+/// Every clip's record, by ID, and the track table, at one moment.
 #[derive(Clone, Debug, Default, PartialEq)]
 pub(super) struct Capture {
     pub(super) records: BTreeMap<String, ClipRecord>,
-    pub(super) tracks: usize,
+    /// One record per track row, top first: how many there are, and each
+    /// one's name, mute and lock. Solo is not in it.
+    pub(super) tracks: Vec<TrackRecord>,
 }
 
 impl Capture {
     /// Read the timeline as it is now. No project yet means no clips.
-    pub(super) fn of(project: Option<&kuvatin_video::Project>, tracks: usize) -> Self {
+    pub(super) fn of(project: Option<&kuvatin_video::Project>, tracks: Vec<TrackRecord>) -> Self {
         let records = project
             .map(|p| {
                 p.clip_records()
@@ -103,8 +106,10 @@ pub(super) struct TimelineStep {
     /// clip that comes back gets its row as it was: its name (the engine's
     /// record spells one from the URI), kind and thumbnail, without decoding.
     pub(super) kept_rows: HashMap<String, TimelineClip>,
-    pub(super) tracks_before: usize,
-    pub(super) tracks_after: usize,
+    /// The track table on each side. A step that changes nothing but the
+    /// table (a rename, a mute, a lock) is still a step.
+    pub(super) tracks_before: Vec<TrackRecord>,
+    pub(super) tracks_after: Vec<TrackRecord>,
 }
 
 impl TimelineStep {
@@ -134,8 +139,8 @@ impl TimelineStep {
             name: name.to_string(),
             changes,
             kept_rows,
-            tracks_before: before.tracks,
-            tracks_after: after.tracks,
+            tracks_before: before.tracks.clone(),
+            tracks_after: after.tracks.clone(),
         }
     }
 
@@ -240,11 +245,11 @@ pub(super) fn plan(step: &TimelineStep, dir: Direction) -> Plan {
     out
 }
 
-/// How many track rows the timeline shows on the `dir` side of `step`.
-pub(super) fn target_tracks(step: &TimelineStep, dir: Direction) -> usize {
+/// The track table the timeline shows on the `dir` side of `step`.
+pub(super) fn target_tracks(step: &TimelineStep, dir: Direction) -> Vec<TrackRecord> {
     match dir {
-        Direction::Undo => step.tracks_before,
-        Direction::Redo => step.tracks_after,
+        Direction::Undo => step.tracks_before.clone(),
+        Direction::Redo => step.tracks_after.clone(),
     }
 }
 
@@ -339,14 +344,25 @@ pub(super) fn applied_from_engine(
     gone.chain(present).collect()
 }
 
-/// Make the timeline show exactly `count` track rows. Rows past the end go;
-/// new ones are unnamed.
-pub(super) fn set_track_rows(tracks: &VecModel<TimelineTrack>, count: usize) {
-    while tracks.row_count() > count {
-        tracks.remove(tracks.row_count() - 1);
+/// Make the timeline's track rows match `want` exactly: how many there are,
+/// and every row's name, mute and lock. Solo is the row's own and is carried
+/// across rather than overwritten: undo does not change what you are
+/// listening to. Rows that already match are left alone, so Slint does not
+/// repaint them.
+pub(super) fn set_track_rows(rows: &VecModel<TimelineTrack>, want: &[TrackRecord]) {
+    while rows.row_count() > want.len() {
+        rows.remove(rows.row_count() - 1);
     }
-    while tracks.row_count() < count {
-        tracks.push(TimelineTrack::default());
+    for (i, record) in want.iter().enumerate() {
+        match rows.row_data(i) {
+            Some(have) => {
+                let row = tracks::row(record, have.soloed);
+                if row != have {
+                    rows.set_row_data(i, row);
+                }
+            }
+            None => rows.push(tracks::row(record, false)),
+        }
     }
 }
 
@@ -364,7 +380,12 @@ pub(super) struct Recorder {
 impl Recorder {
     /// Read the timeline just before an edit.
     pub(super) fn before(&self, project: Option<&kuvatin_video::Project>) -> Capture {
-        Capture::of(project, self.tracks.row_count())
+        Capture::of(project, self.table())
+    }
+
+    /// The track rows as the history keeps them.
+    fn table(&self) -> Vec<TrackRecord> {
+        tracks::records(&tracks::rows_of(&self.tracks))
     }
 
     /// Record what an edit changed. Call it after the engine edit and after an
@@ -377,7 +398,7 @@ impl Recorder {
         subject: Option<&str>,
         before: Capture,
     ) {
-        let after = Capture::of(project, self.tracks.row_count());
+        let after = Capture::of(project, self.table());
         self.record_captures(kind, subject, before, after);
     }
 
@@ -462,7 +483,7 @@ fn apply_step(
     };
     // Read what to do, then let go of the history: applying reads the rows,
     // and recording must never see it borrowed.
-    let (ops, tracks, subject, kept) = {
+    let (ops, table, subject, kept) = {
         let history = rec.history.borrow();
         let step = match dir {
             Direction::Undo => history.peek_undo(),
@@ -479,11 +500,12 @@ fn apply_step(
         )
     };
 
-    // A step that only changed track rows (a track added before any clip was
-    // placed) needs no engine, which may not exist yet.
+    // A step that only changed track rows (a track added, renamed, muted or
+    // locked before any clip was placed) needs no engine, which may not exist
+    // yet.
     if project.borrow().is_none() {
         if ops == Plan::default() {
-            set_track_rows(&rec.tracks, tracks);
+            set_track_rows(&rec.tracks, &table);
             let mut history = rec.history.borrow_mut();
             match dir {
                 Direction::Undo => history.commit_undo(),
@@ -583,7 +605,7 @@ fn apply_step(
             }
         }
     }
-    p.prune_tracks(tracks);
+    p.prune_tracks(table.len());
 
     let rows: Vec<TimelineClip> = rec.tl_clips.iter().collect();
     let selected_id = usize::try_from(sel_idx.get())
@@ -611,11 +633,23 @@ fn apply_step(
         (applied, kept)
     };
     let new_rows = rows_after(&rows, &to_rows, &kept);
-    let track_rows = tracks.max(p.track_count());
+    // A prune the engine refused (a clip still on a track this side does not
+    // have) leaves more layers than the table has rows: they stay on screen,
+    // as rows of their own.
+    let mut table = table;
+    if table.len() < p.track_count() {
+        table.resize(p.track_count(), TrackRecord::default());
+    }
     let duration = p.duration();
     drop(slot);
 
-    set_track_rows(&rec.tracks, track_rows);
+    set_track_rows(&rec.tracks, &table);
+    if let Some(p) = project.borrow_mut().as_mut() {
+        tracks::push_mutes(p, &rec.tracks);
+        // Every undo changes the work, and one that changes only the table (a
+        // rename, a lock) touches nothing the engine would notice by itself.
+        p.mark_unsaved();
+    }
     ui.set_timeline_duration(duration.map(|d| d.as_secs_f32()).unwrap_or(0.0));
 
     let subject_now = subject.map(|s| {
@@ -714,13 +748,29 @@ mod tests {
         }
     }
 
+    /// Clips, on `tracks` unnamed, audible, unlocked tracks.
     fn cap(clips: &[(&str, ClipRecord)], tracks: usize) -> Capture {
         Capture {
             records: clips
                 .iter()
                 .map(|(id, r)| (id.to_string(), r.clone()))
                 .collect(),
-            tracks,
+            tracks: vec![TrackRecord::default(); tracks],
+        }
+    }
+
+    /// No clips, and this track table.
+    fn table_only(tracks: &[TrackRecord]) -> Capture {
+        Capture {
+            records: BTreeMap::new(),
+            tracks: tracks.to_vec(),
+        }
+    }
+
+    fn named(name: &str) -> TrackRecord {
+        TrackRecord {
+            name: name.into(),
+            ..TrackRecord::default()
         }
     }
 
@@ -877,7 +927,10 @@ mod tests {
             diff(&c0, &c2),
             "b, which only the newer step touched, is in too"
         );
-        assert_eq!((first.tracks_before, first.tracks_after), (2, 3));
+        assert_eq!(
+            (first.tracks_before.len(), first.tracks_after.len()),
+            (2, 3)
+        );
     }
 
     #[test]
@@ -902,7 +955,7 @@ mod tests {
                 restores: vec![("b".into(), rec(1, 0.0, 2.0))],
             }
         );
-        assert_eq!(target_tracks(&s, Direction::Undo), 2);
+        assert_eq!(target_tracks(&s, Direction::Undo).len(), 2);
     }
 
     #[test]
@@ -918,7 +971,7 @@ mod tests {
                 restores: vec![("c".into(), rec(1, 4.0, 1.0))],
             }
         );
-        assert_eq!(target_tracks(&s, Direction::Redo), 3);
+        assert_eq!(target_tracks(&s, Direction::Redo).len(), 3);
     }
 
     /// Undo removes the right half and writes the left one back whole; redo
@@ -1114,7 +1167,7 @@ mod tests {
         r.record(None, StepKind::AddTrack, None, before);
         let history = r.history.borrow();
         let s = history.peek_undo().expect("a step");
-        assert_eq!((s.tracks_before, s.tracks_after), (2, 3));
+        assert_eq!((s.tracks_before.len(), s.tracks_after.len()), (2, 3));
     }
 
     #[test]
@@ -1157,13 +1210,107 @@ mod tests {
     }
 
     #[test]
-    fn track_rows_grow_and_shrink_to_a_count() {
-        let tracks = VecModel::from(vec![TimelineTrack::default(); 2]);
-        set_track_rows(&tracks, 4);
-        assert_eq!(tracks.row_count(), 4);
-        assert_eq!(tracks.row_data(3), Some(TimelineTrack::default()));
-        set_track_rows(&tracks, 1);
-        assert_eq!(tracks.row_count(), 1);
+    fn track_rows_grow_shrink_and_change_in_place() {
+        let rows = VecModel::from(vec![TimelineTrack::default(); 2]);
+        let mut want = vec![TrackRecord::default(); 4];
+        want[1].muted = true;
+        set_track_rows(&rows, &want);
+        assert_eq!(tracks::records(&tracks::rows_of(&rows)), want);
+        want.truncate(1);
+        set_track_rows(&rows, &want);
+        assert_eq!(rows.row_count(), 1);
+        want[0].locked = true;
+        set_track_rows(&rows, &want);
+        assert!(rows.row_data(0).unwrap().locked, "changed in place");
+    }
+
+    /// The bug this table exists for: an undo regenerated every track's name
+    /// from its index, so a rename was silently lost to the next Ctrl+Z.
+    #[test]
+    fn track_rows_take_their_names_from_the_table_not_the_index() {
+        let rows = VecModel::from(vec![TimelineTrack::default(); 2]);
+        set_track_rows(&rows, &[named("Music"), named("Dialogue")]);
+        let names: Vec<String> = tracks::rows_of(&rows)
+            .iter()
+            .map(|r| r.name.to_string())
+            .collect();
+        assert_eq!(names, vec!["Music", "Dialogue"]);
+    }
+
+    /// Undo does not change what you are listening to.
+    #[test]
+    fn track_rows_keep_their_solo() {
+        let rows = VecModel::from(vec![TimelineTrack {
+            soloed: true,
+            ..TimelineTrack::default()
+        }]);
+        set_track_rows(&rows, &[named("Dialogue")]);
+        let row = rows.row_data(0).unwrap();
+        assert!(row.soloed);
+        assert_eq!(row.name.as_str(), "Dialogue");
+    }
+
+    /// A rename, a mute or a lock changes no clip, and is still a step. With
+    /// a bare count on each side it compared equal and was thrown away.
+    #[test]
+    fn a_step_that_changes_only_the_track_table_is_not_empty() {
+        let plain = [TrackRecord::default(), TrackRecord::default()];
+        let mut muted = plain.clone();
+        muted[1].muted = true;
+        let mut locked = plain.clone();
+        locked[0].locked = true;
+        let mut renamed = plain.clone();
+        renamed[1].name = "Dialogue".into();
+        for other in [muted, locked, renamed] {
+            let s = TimelineStep::new(
+                StepKind::AddTrack,
+                None,
+                "",
+                &table_only(&plain),
+                &table_only(&other),
+                HashMap::new(),
+            );
+            assert!(!s.is_empty(), "{other:?}");
+        }
+        let same = TimelineStep::new(
+            StepKind::AddTrack,
+            None,
+            "",
+            &table_only(&plain),
+            &table_only(&plain),
+            HashMap::new(),
+        );
+        assert!(same.is_empty(), "identical tables and no clips");
+    }
+
+    #[test]
+    fn undo_goes_to_the_before_table_and_redo_to_the_after() {
+        let s = TimelineStep::new(
+            StepKind::AddTrack,
+            None,
+            "",
+            &table_only(&[named("A")]),
+            &table_only(&[named("B")]),
+            HashMap::new(),
+        );
+        assert_eq!(target_tracks(&s, Direction::Undo), vec![named("A")]);
+        assert_eq!(target_tracks(&s, Direction::Redo), vec![named("B")]);
+    }
+
+    /// The recorder reads the table from the rows, so a change to a row
+    /// between the two reads is what the step holds.
+    #[test]
+    fn the_recorder_reads_the_track_table_from_the_rows() {
+        let r = recorder(Vec::new(), 2);
+        let before = r.before(None);
+        let mut row = r.tracks.row_data(1).unwrap();
+        row.name = "Dialogue".into();
+        r.tracks.set_row_data(1, row);
+        r.record(None, StepKind::AddTrack, None, before);
+        let history = r.history.borrow();
+        let s = history.peek_undo().expect("a step");
+        assert_eq!(s.tracks_before[1].name, "");
+        assert_eq!(s.tracks_after[1].name, "Dialogue");
     }
 
     #[test]
```

- [ ] **Step 2: Build and test**

Run: `cargo test -p kuvatin`
Expected: 288 passed, 2 ignored. The new ones: five in `tracks::tests` (effective mutes with and without solo, and records), and five more in `undo::tests` beside the count test it rewrites, among them `track_rows_take_their_names_from_the_table_not_the_index`, the regression this design exists for, and `a_step_that_changes_only_the_track_table_is_not_empty`.

- [ ] **Step 3: Gates and commit**

```bash
git add crates/kuvatin/src/gui/video
git commit -m "Undo keeps each track's name, mute and lock, not only how many there are" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Mute, lock and rename are steps

A step's subject becomes `Option<Subject>`, a clip or a track, and three kinds arrive: Mute track, Lock track and Rename track. A toggle reads which way it went off its own tables ("muting Dialogue" / "unmuting Dialogue"); only a rename merges, so its keystrokes are one step, and typing a name back to what it was leaves nothing. The four header callbacks are declared in `app.slint` and wired in `tracks.rs`: mute, lock and rename go through `edit_row`, which records the step, pushes the mutes and marks the project unsaved, and works before the engine exists; solo changes the row and the engine and nothing else. Nothing on screen calls them yet.

**Files:**
- Modify: `crates/kuvatin/src/gui/video/undo.rs`, `tracks.rs`, `timeline.rs`, `mod.rs`
- Modify: `crates/kuvatin/ui/app.slint` (the callbacks after `add-track()`)

- [ ] **Step 1: Apply the diff**

```diff
diff --git a/crates/kuvatin/src/gui/video/mod.rs b/crates/kuvatin/src/gui/video/mod.rs
index 6c9663a..d1775c8 100644
--- a/crates/kuvatin/src/gui/video/mod.rs
+++ b/crates/kuvatin/src/gui/video/mod.rs
@@ -134,6 +134,7 @@ pub(super) fn wire(
 ) {
     import::wire(ui, st, im, timers);
     timeline::wire(ui, st);
+    tracks::wire(ui, st);
     transport::wire(ui, st);
     export::wire(ui, st, ex, timers);
     project_file::wire(ui, st, im);
@@ -359,7 +360,7 @@ pub(super) fn wire(
                     rec.record(
                         Some(&*project),
                         undo::StepKind::Transform,
-                        Some(id.as_str()),
+                        Some(undo::Subject::Clip(id.clone())),
                         before,
                     );
                 }
@@ -565,7 +566,7 @@ fn add_to_timeline(
             rec.record(
                 Some(&*project),
                 undo::StepKind::Add,
-                Some(info.id.0.as_str()),
+                Some(undo::Subject::Clip(info.id.0.clone())),
                 before,
             );
             // Only a video can have sound.
@@ -634,7 +635,7 @@ fn add_sequence_to_timeline(
             rec.record(
                 Some(&*project),
                 undo::StepKind::Add,
-                Some(info.id.0.as_str()),
+                Some(undo::Subject::Clip(info.id.0.clone())),
                 before,
             );
             let _ = project.play();
diff --git a/crates/kuvatin/src/gui/video/timeline.rs b/crates/kuvatin/src/gui/video/timeline.rs
index 37f0ae9..15a4fa8 100644
--- a/crates/kuvatin/src/gui/video/timeline.rs
+++ b/crates/kuvatin/src/gui/video/timeline.rs
@@ -1,7 +1,7 @@
 //! Timeline editing: selection + inspector, slide / trim / move-to-track,
 //! magnetic snapping, track rows and clip removal.
 
-use super::undo::{Recorder, StepKind};
+use super::undo::{Recorder, StepKind, Subject};
 use super::{VideoState, MAX_SCALE_PCT, MIN_SCALE_PCT, SPEEDS};
 use crate::gui::{show_error, AppWindow, ClipKind, TimelineClip, TimelineTrack};
 use slint::{ComponentHandle, Model, SharedString, VecModel};
@@ -151,7 +151,12 @@ pub(super) fn wire(ui: &AppWindow, st: &VideoState) {
                     tracks.push(TimelineTrack::default());
                 }
             }
-            rec.record(Some(&*p), StepKind::Move, Some(row.id.as_str()), before);
+            rec.record(
+                Some(&*p),
+                StepKind::Move,
+                Some(Subject::Clip(row.id.to_string())),
+                before,
+            );
             let dur = p.duration();
             drop(slot);
             tl_clips.set_row_data(i as usize, row);
@@ -251,7 +256,12 @@ pub(super) fn wire(ui: &AppWindow, st: &VideoState) {
                     edge,
                     delta as f64,
                 );
-                rec.record(Some(&*p), StepKind::Trim, Some(row.id.as_str()), before);
+                rec.record(
+                    Some(&*p),
+                    StepKind::Trim,
+                    Some(Subject::Clip(row.id.to_string())),
+                    before,
+                );
                 geom
             });
             let Some(geom) = geom else {
@@ -312,7 +322,12 @@ pub(super) fn wire(ui: &AppWindow, st: &VideoState) {
                     tl_clips.set_row_data(i as usize, left.clone());
                     tl_clips.push(right);
                     // After the push: the step keeps the row of a clip it adds.
-                    rec.record(Some(&*p), StepKind::Split, Some(left.id.as_str()), before);
+                    rec.record(
+                        Some(&*p),
+                        StepKind::Split,
+                        Some(Subject::Clip(left.id.to_string())),
+                        before,
+                    );
                     let length = p.duration();
                     let right_uri = p.clip_uri(&right_id);
                     drop(slot);
@@ -357,7 +372,12 @@ pub(super) fn wire(ui: &AppWindow, st: &VideoState) {
                 let before = rec.before(Some(&*p));
                 let geom =
                     p.set_clip_duration(&kuvatin_video::ClipId(row.id.to_string()), secs as f64);
-                rec.record(Some(&*p), StepKind::Duration, Some(row.id.as_str()), before);
+                rec.record(
+                    Some(&*p),
+                    StepKind::Duration,
+                    Some(Subject::Clip(row.id.to_string())),
+                    before,
+                );
                 geom
             });
             let Some(geom) = geom else {
@@ -399,7 +419,12 @@ pub(super) fn wire(ui: &AppWindow, st: &VideoState) {
             let done = project_slot.borrow_mut().as_mut().and_then(|p| {
                 let before = rec.before(Some(&*p));
                 let geom = p.set_clip_rate(&cid, rate)?;
-                rec.record(Some(&*p), StepKind::Speed, Some(row.id.as_str()), before);
+                rec.record(
+                    Some(&*p),
+                    StepKind::Speed,
+                    Some(Subject::Clip(row.id.to_string())),
+                    before,
+                );
                 Some((geom, p.clip_rate(&cid), p.duration()))
             });
             match done {
@@ -478,7 +503,12 @@ fn remove_timeline_clip(
             let before = rec.before(Some(&*p));
             p.remove_clip(&kuvatin_video::ClipId(row.id.to_string()));
             // Recorded before the row goes, so the step keeps the row.
-            rec.record(Some(&*p), StepKind::Delete, Some(row.id.as_str()), before);
+            rec.record(
+                Some(&*p),
+                StepKind::Delete,
+                Some(Subject::Clip(row.id.to_string())),
+                before,
+            );
             duration = Some(p.duration());
         }
     }
diff --git a/crates/kuvatin/src/gui/video/tracks.rs b/crates/kuvatin/src/gui/video/tracks.rs
index 21a8863..6dcf0d4 100644
--- a/crates/kuvatin/src/gui/video/tracks.rs
+++ b/crates/kuvatin/src/gui/video/tracks.rs
@@ -6,10 +6,94 @@
 //! [`TrackRecord`]s. Solo is the one flag a record leaves out: it is a way of
 //! listening, not an edit, so it is neither saved nor undone.
 
-use crate::gui::TimelineTrack;
+use super::undo::{Recorder, StepKind, Subject};
+use crate::gui::{AppWindow, TimelineTrack};
 use kuvatin_video::TrackRecord;
 use slint::{Model, VecModel};
 
+/// Wire the track header's controls: mute, solo, lock and rename.
+pub(super) fn wire(ui: &AppWindow, st: &super::VideoState) {
+    let rec = st.recorder(ui);
+    {
+        let project = st.project.clone();
+        let rec = rec.clone();
+        ui.on_track_muted(move |t, on| {
+            edit_row(&project, &rec, t, StepKind::MuteTrack, |r| r.muted = on);
+        });
+    }
+    {
+        let project = st.project.clone();
+        let rec = rec.clone();
+        ui.on_track_locked(move |t, on| {
+            edit_row(&project, &rec, t, StepKind::LockTrack, |r| r.locked = on);
+        });
+    }
+    {
+        let project = st.project.clone();
+        let rec = rec.clone();
+        ui.on_track_renamed(move |t, name| {
+            // Blank or spaces only: back to "Track N".
+            let name = name.trim().to_string();
+            edit_row(&project, &rec, t, StepKind::RenameTrack, |r| {
+                r.name = name.as_str().into()
+            });
+        });
+    }
+    // Solo is for listening: it changes what the engine silences and nothing
+    // else. Not the history, not the file, not unsaved work.
+    {
+        let project = st.project.clone();
+        let rows = st.tracks.clone();
+        ui.on_track_soloed(move |t, on| {
+            let Some(i) = usize::try_from(t).ok() else {
+                return;
+            };
+            let Some(mut row) = rows.row_data(i) else {
+                return;
+            };
+            row.soloed = on;
+            rows.set_row_data(i, row);
+            if let Some(p) = project.borrow_mut().as_mut() {
+                push_mutes(p, &rows);
+            }
+        });
+    }
+}
+
+/// Change the track row at `t` as an edit: recorded as a step of `kind`,
+/// pushed to the engine, and counted as unsaved work. A change that leaves
+/// the row as it was does nothing at all. Works before the engine exists,
+/// as adding a track does: the step needs no clips.
+fn edit_row(
+    project: &super::ProjectSlot,
+    rec: &Recorder,
+    t: i32,
+    kind: StepKind,
+    change: impl FnOnce(&mut TimelineTrack),
+) {
+    let Some(i) = usize::try_from(t).ok() else {
+        return;
+    };
+    let Some(was) = rec.tracks.row_data(i) else {
+        return;
+    };
+    let mut row = was.clone();
+    change(&mut row);
+    if row == was {
+        return;
+    }
+    let mut slot = project.borrow_mut();
+    let before = rec.before(slot.as_ref());
+    rec.tracks.set_row_data(i, row);
+    if let Some(p) = slot.as_mut() {
+        push_mutes(p, &rec.tracks);
+        // The engine cannot tell a name or a lock changed; a mute it does
+        // not count, because solo drives the same call.
+        p.mark_unsaved();
+    }
+    rec.record(slot.as_ref(), kind, Some(Subject::Track(i)), before);
+}
+
 /// What the engine should silence, given the rows as they are. While any
 /// track is soloed every other one is silent; an explicit mute wins over
 /// solo, so a track both soloed and muted stays silent.
@@ -32,6 +116,14 @@ pub(super) fn records(rows: &[TimelineTrack]) -> Vec<TrackRecord> {
         .collect()
 }
 
+/// What to call the track at `i`: what it was named, or "Track {i+1}".
+pub(super) fn label(table: &[TrackRecord], i: usize) -> String {
+    match table.get(i) {
+        Some(t) if !t.name.is_empty() => t.name.clone(),
+        _ => format!("Track {}", i + 1),
+    }
+}
+
 /// The row a record draws as, carrying the solo flag it is given.
 pub(super) fn row(record: &TrackRecord, soloed: bool) -> TimelineTrack {
     TimelineTrack {
@@ -134,4 +226,48 @@ pub(super) mod tests {
         assert_eq!(row(&got[0], true), rows[0], "and a row comes back from it");
         assert!(!row(&got[0], false).soloed);
     }
+
+    #[test]
+    fn a_track_is_called_by_its_name_or_its_number() {
+        let table = records(&[
+            trk("Dialogue", false, false, false),
+            trk("", false, false, false),
+        ]);
+        assert_eq!(label(&table, 0), "Dialogue");
+        assert_eq!(label(&table, 1), "Track 2");
+        assert_eq!(label(&table, 5), "Track 6", "past the end");
+    }
+
+    /// Two unnamed tracks and no engine, as the window starts.
+    fn recorder() -> (super::super::ProjectSlot, Recorder) {
+        let rec = Recorder {
+            history: std::rc::Rc::new(std::cell::RefCell::new(crate::gui::history::History::new())),
+            tl_clips: std::rc::Rc::new(VecModel::from(Vec::new())),
+            tracks: std::rc::Rc::new(VecModel::from(vec![TimelineTrack::default(); 2])),
+            ui: slint::Weak::default(),
+        };
+        (std::rc::Rc::new(std::cell::RefCell::new(None)), rec)
+    }
+
+    /// A mute is an edit, recorded with the track's label, and needs no
+    /// engine: a track can be muted before any clip is on the timeline.
+    #[test]
+    fn muting_a_row_is_a_step_named_for_its_track() {
+        use crate::gui::history::Step;
+        let (project, rec) = recorder();
+        edit_row(&project, &rec, 1, StepKind::MuteTrack, |r| r.muted = true);
+        assert!(rec.tracks.row_data(1).unwrap().muted);
+        let history = rec.history.borrow();
+        let step = history.peek_undo().expect("a step");
+        assert_eq!(step.describe(), "muting Track 2");
+        assert_eq!(step.subject, Some(Subject::Track(1)));
+    }
+
+    #[test]
+    fn a_change_that_changes_nothing_records_nothing() {
+        let (project, rec) = recorder();
+        edit_row(&project, &rec, 0, StepKind::LockTrack, |r| r.locked = false);
+        edit_row(&project, &rec, 9, StepKind::LockTrack, |r| r.locked = true);
+        assert!(!rec.history.borrow().can_undo());
+    }
 }
diff --git a/crates/kuvatin/src/gui/video/undo.rs b/crates/kuvatin/src/gui/video/undo.rs
index c200258..2bfe5d6 100644
--- a/crates/kuvatin/src/gui/video/undo.rs
+++ b/crates/kuvatin/src/gui/video/undo.rs
@@ -33,18 +33,47 @@ pub(super) enum StepKind {
     AddTrack,
     Split,
     Speed,
+    MuteTrack,
+    LockTrack,
+    RenameTrack,
 }
 
 impl StepKind {
-    /// The kinds a continuous gesture produces. Only these merge.
+    /// The kinds a continuous gesture produces, and a rename, whose
+    /// keystrokes are one renaming. Only these merge. A mute or a lock is one
+    /// click: muting and unmuting inside a second is two steps, not nothing.
     fn merges(self) -> bool {
         matches!(
             self,
-            StepKind::Move | StepKind::Trim | StepKind::Transform | StepKind::Duration
+            StepKind::Move
+                | StepKind::Trim
+                | StepKind::Transform
+                | StepKind::Duration
+                | StepKind::RenameTrack
         )
     }
 }
 
+/// What a step is about, when it is about one thing.
+#[derive(Clone, Debug, PartialEq, Eq)]
+pub(super) enum Subject {
+    Clip(String),
+    /// A track, by its index. Safe as a merge key: the only track kind that
+    /// merges is RenameTrack, and every edit that could renumber a track is a
+    /// kind of its own, which `merges_with` already refuses to merge across.
+    Track(usize),
+}
+
+impl Subject {
+    /// The clip this is about, if it is about a clip.
+    pub(super) fn clip(&self) -> Option<&str> {
+        match self {
+            Subject::Clip(id) => Some(id),
+            Subject::Track(_) => None,
+        }
+    }
+}
+
 /// Every clip's record, by ID, and the track table, at one moment.
 #[derive(Clone, Debug, Default, PartialEq)]
 pub(super) struct Capture {
@@ -97,9 +126,10 @@ pub(super) fn diff(before: &Capture, after: &Capture) -> Vec<ClipChange> {
 /// What one timeline edit changed.
 pub(super) struct TimelineStep {
     pub(super) kind: StepKind,
-    /// The clip the step is about, when it is about one.
-    pub(super) subject: Option<String>,
-    /// That clip's display name, for the hint.
+    /// The clip or track the step is about, when it is about one.
+    pub(super) subject: Option<Subject>,
+    /// That clip's display name, or that track's label on the step's "before"
+    /// side, for the hint.
     pub(super) name: String,
     pub(super) changes: Vec<ClipChange>,
     /// The timeline rows of the clips that exist on only one side, by ID, so a
@@ -117,7 +147,7 @@ impl TimelineStep {
     /// those of clips that come or go are kept.
     pub(super) fn new(
         kind: StepKind,
-        subject: Option<&str>,
+        subject: Option<Subject>,
         name: &str,
         before: &Capture,
         after: &Capture,
@@ -135,7 +165,7 @@ impl TimelineStep {
             .collect();
         TimelineStep {
             kind,
-            subject: subject.map(str::to_string),
+            subject,
             name: name.to_string(),
             changes,
             kept_rows,
@@ -147,8 +177,8 @@ impl TimelineStep {
     /// Replace a clip's ID everywhere in the step: the engine restored it
     /// under a new one.
     pub(super) fn rename_clip(&mut self, old: &str, new: &str) {
-        if self.subject.as_deref() == Some(old) {
-            self.subject = Some(new.to_string());
+        if self.subject.as_ref().and_then(Subject::clip) == Some(old) {
+            self.subject = Some(Subject::Clip(new.to_string()));
         }
         for change in &mut self.changes {
             if change.id == old {
@@ -159,6 +189,15 @@ impl TimelineStep {
             self.kept_rows.insert(new.to_string(), row);
         }
     }
+
+    /// Whether the step's track has `flag` set on its "after" side: which
+    /// way a toggle went, read off the step's own tables.
+    fn turned_on(&self, flag: impl Fn(&TrackRecord) -> bool) -> bool {
+        match self.subject {
+            Some(Subject::Track(t)) => self.tracks_after.get(t).is_some_and(flag),
+            _ => false,
+        }
+    }
 }
 
 impl Step for TimelineStep {
@@ -175,6 +214,11 @@ impl Step for TimelineStep {
             StepKind::AddTrack => "adding a track".into(),
             StepKind::Split => format!("splitting {name}"),
             StepKind::Speed => format!("changing the speed of {name}"),
+            StepKind::MuteTrack if self.turned_on(|t| t.muted) => format!("muting {name}"),
+            StepKind::MuteTrack => format!("unmuting {name}"),
+            StepKind::LockTrack if self.turned_on(|t| t.locked) => format!("locking {name}"),
+            StepKind::LockTrack => format!("unlocking {name}"),
+            StepKind::RenameTrack => format!("renaming {name}"),
         }
     }
 
@@ -395,7 +439,7 @@ impl Recorder {
         &self,
         project: Option<&kuvatin_video::Project>,
         kind: StepKind,
-        subject: Option<&str>,
+        subject: Option<Subject>,
         before: Capture,
     ) {
         let after = Capture::of(project, self.table());
@@ -405,18 +449,28 @@ impl Recorder {
     pub(super) fn record_captures(
         &self,
         kind: StepKind,
-        subject: Option<&str>,
+        subject: Option<Subject>,
         before: Capture,
         after: Capture,
     ) {
         let rows: Vec<TimelineClip> = self.tl_clips.iter().collect();
-        let from_rows = subject
-            .and_then(|id| rows.iter().find(|r| r.id.as_str() == id))
-            .map(|r| r.name.to_string());
-        let from_records = subject
-            .and_then(|id| before.records.get(id).or_else(|| after.records.get(id)))
-            .map(|r| r.name.clone());
-        let name = from_rows.or(from_records).unwrap_or_default();
+        let name = match &subject {
+            Some(Subject::Clip(id)) => {
+                let from_rows = rows
+                    .iter()
+                    .find(|r| r.id.as_str() == id)
+                    .map(|r| r.name.to_string());
+                let from_records = before
+                    .records
+                    .get(id)
+                    .or_else(|| after.records.get(id))
+                    .map(|r| r.name.clone());
+                from_rows.or(from_records).unwrap_or_default()
+            }
+            // As it was called before the step: "renaming Track 2".
+            Some(Subject::Track(t)) => tracks::label(&before.tracks, *t),
+            None => String::new(),
+        };
         let kept = rows.into_iter().map(|r| (r.id.to_string(), r)).collect();
         let step = TimelineStep::new(kind, subject, &name, &before, &after, kept);
         let mut history = self.history.borrow_mut();
@@ -652,12 +706,14 @@ fn apply_step(
     }
     ui.set_timeline_duration(duration.map(|d| d.as_secs_f32()).unwrap_or(0.0));
 
-    let subject_now = subject.map(|s| {
+    // A track's step selects no clip; a clip's step selects its clip, under
+    // the ID it has now.
+    let subject_now = subject.as_ref().and_then(Subject::clip).map(|s| {
         renames
             .iter()
-            .find(|(old, _)| *old == s)
+            .find(|(old, _)| old == s)
             .map(|(_, new)| new.clone())
-            .unwrap_or(s)
+            .unwrap_or_else(|| s.to_string())
     });
     let next = selection_after(&new_rows, subject_now.as_deref(), selected_id.as_deref());
     // A clip deleted before its thumbnail arrived comes back without one:
@@ -798,7 +854,7 @@ mod tests {
     fn step(kind: StepKind, subject: &str, before: &Capture, after: &Capture) -> TimelineStep {
         TimelineStep::new(
             kind,
-            Some(subject),
+            Some(Subject::Clip(subject.into())),
             "intro.mp4",
             before,
             after,
@@ -860,7 +916,7 @@ mod tests {
             .collect();
         let s = TimelineStep::new(
             StepKind::Delete,
-            Some("b"),
+            Some(Subject::Clip("b".into())),
             "intro.mp4",
             &before,
             &after,
@@ -1013,14 +1069,14 @@ mod tests {
                 .collect();
         let mut s = TimelineStep::new(
             StepKind::Delete,
-            Some("old"),
+            Some(Subject::Clip("old".into())),
             "intro.mp4",
             &before,
             &after,
             rows,
         );
         s.rename_clip("old", "new");
-        assert_eq!(s.subject.as_deref(), Some("new"));
+        assert_eq!(s.subject, Some(Subject::Clip("new".into())));
         let ids: Vec<&str> = s.changes.iter().map(|c| c.id.as_str()).collect();
         assert_eq!(ids, vec!["new", "other"], "only the renamed clip's change");
         assert!(s.kept_rows.contains_key("new") && !s.kept_rows.contains_key("old"));
@@ -1120,7 +1176,15 @@ mod tests {
     fn each_kind_describes_itself() {
         let c = cap(&[], 2);
         let d = |kind| {
-            TimelineStep::new(kind, Some("a"), "intro.mp4", &c, &c, HashMap::new()).describe()
+            TimelineStep::new(
+                kind,
+                Some(Subject::Clip("a".into())),
+                "intro.mp4",
+                &c,
+                &c,
+                HashMap::new(),
+            )
+            .describe()
         };
         assert_eq!(d(StepKind::Move), "moving intro.mp4");
         assert_eq!(d(StepKind::Trim), "trimming intro.mp4");
@@ -1134,6 +1198,118 @@ mod tests {
         assert_eq!(d(StepKind::Speed), "changing the speed of intro.mp4");
     }
 
+    /// A step about track `t`, going from one table to another, named as the
+    /// recorder names it: by the track's label before the step.
+    fn track_step(
+        kind: StepKind,
+        t: usize,
+        before: &[TrackRecord],
+        after: &[TrackRecord],
+    ) -> TimelineStep {
+        TimelineStep::new(
+            kind,
+            Some(Subject::Track(t)),
+            &tracks::label(before, t),
+            &table_only(before),
+            &table_only(after),
+            HashMap::new(),
+        )
+    }
+
+    /// Which way a toggle went is read off the step's own tables, and the
+    /// hint names the track as it was called before the step.
+    #[test]
+    fn track_steps_describe_which_way_they_went() {
+        let plain = [TrackRecord::default(), named("Dialogue")];
+        let mut muted = plain.clone();
+        muted[1].muted = true;
+        let mut locked = plain.clone();
+        locked[1].locked = true;
+        let mut renamed = plain.clone();
+        renamed[0].name = "Music".into();
+        let d =
+            |kind, t, b: &[TrackRecord], a: &[TrackRecord]| track_step(kind, t, b, a).describe();
+        assert_eq!(d(StepKind::MuteTrack, 1, &plain, &muted), "muting Dialogue");
+        assert_eq!(
+            d(StepKind::MuteTrack, 1, &muted, &plain),
+            "unmuting Dialogue"
+        );
+        assert_eq!(
+            d(StepKind::LockTrack, 1, &plain, &locked),
+            "locking Dialogue"
+        );
+        assert_eq!(
+            d(StepKind::LockTrack, 1, &locked, &plain),
+            "unlocking Dialogue"
+        );
+        assert_eq!(
+            d(StepKind::RenameTrack, 0, &plain, &renamed),
+            "renaming Track 1"
+        );
+    }
+
+    /// A rename is typed: its keystrokes are one step. Renames of different
+    /// tracks are not, and neither is a mute or a lock, which is one click.
+    #[test]
+    fn renames_of_one_track_merge_and_nothing_else_about_tracks_does() {
+        let t0 = [named("A"), TrackRecord::default()];
+        let t1 = [named("Ab"), TrackRecord::default()];
+        let t2 = [named("Abc"), TrackRecord::default()];
+        let first = track_step(StepKind::RenameTrack, 0, &t0, &t1);
+        assert!(first.merges_with(&track_step(StepKind::RenameTrack, 0, &t1, &t2)));
+        assert!(!first.merges_with(&track_step(StepKind::RenameTrack, 1, &t1, &t2)));
+        for kind in [StepKind::MuteTrack, StepKind::LockTrack] {
+            assert!(
+                !track_step(kind, 0, &t0, &t1).merges_with(&track_step(kind, 0, &t1, &t0)),
+                "{kind:?} never merges"
+            );
+        }
+    }
+
+    /// Typing a name and then typing the old one back within a second is no
+    /// rename at all: the history drops the step, and the next change starts
+    /// one of its own rather than merging into whatever came before.
+    #[test]
+    fn a_rename_typed_back_to_the_old_name_leaves_nothing() {
+        let t0 = [named("A")];
+        let t1 = [named("B")];
+        let now = Instant::now();
+        let mut history = History::new();
+        history.record(track_step(StepKind::RenameTrack, 0, &t0, &t1), now);
+        history.record(
+            track_step(StepKind::RenameTrack, 0, &t1, &t0),
+            now + std::time::Duration::from_millis(300),
+        );
+        assert!(!history.can_undo(), "nothing changed after all");
+    }
+
+    /// Muting and unmuting straight away is two clicks and two steps.
+    #[test]
+    fn a_mute_and_an_unmute_are_two_steps() {
+        let plain = [named("Dialogue")];
+        let muted = [TrackRecord {
+            muted: true,
+            ..named("Dialogue")
+        }];
+        let now = Instant::now();
+        let mut history = History::new();
+        history.record(track_step(StepKind::MuteTrack, 0, &plain, &muted), now);
+        history.record(
+            track_step(StepKind::MuteTrack, 0, &muted, &plain),
+            now + std::time::Duration::from_millis(100),
+        );
+        assert_eq!(history.undo_hint(), "Undo unmuting Dialogue");
+        history.commit_undo();
+        assert_eq!(history.undo_hint(), "Undo muting Dialogue");
+    }
+
+    /// Undoing a track's step selects no clip: the one selected stays.
+    #[test]
+    fn a_track_step_is_not_a_clip_to_select() {
+        assert_eq!(Subject::Track(0).clip(), None);
+        assert_eq!(Subject::Clip("a".into()).clip(), Some("a"));
+    }
+
     fn recorder(rows: Vec<TimelineClip>, tracks: usize) -> Recorder {
         Recorder {
             history: Rc::new(RefCell::new(History::new())),
@@ -1152,7 +1328,12 @@ mod tests {
         let r = recorder(vec![shown], 2);
         let before = cap(&[("a", rec(0, 0.0, 2.0))], 2);
         let after = cap(&[], 2);
-        r.record_captures(StepKind::Delete, Some("a"), before, after);
+        r.record_captures(
+            StepKind::Delete,
+            Some(Subject::Clip("a".into())),
+            before,
+            after,
+        );
         let history = r.history.borrow();
         let s = history.peek_undo().expect("a step");
         assert_eq!(s.describe(), "deleting intro (bin name).mp4");
@@ -1174,7 +1355,12 @@ mod tests {
     fn an_edit_that_changed_nothing_records_nothing() {
         let r = recorder(vec![row("a", &rec(0, 0.0, 2.0))], 2);
         let same = cap(&[("a", rec(0, 0.0, 2.0))], 2);
-        r.record_captures(StepKind::Move, Some("a"), same.clone(), same);
+        r.record_captures(
+            StepKind::Move,
+            Some(Subject::Clip("a".into())),
+            same.clone(),
+            same,
+        );
         assert!(!r.history.borrow().can_undo());
     }
 
@@ -1186,7 +1372,12 @@ mod tests {
         let before = cap(&[], 2);
         r.tl_clips.push(row("a", &rec(0, 0.0, 2.0)));
         let after = cap(&[("a", rec(0, 0.0, 2.0))], 2);
-        r.record_captures(StepKind::Add, Some("a"), before, after);
+        r.record_captures(
+            StepKind::Add,
+            Some(Subject::Clip("a".into())),
+            before,
+            after,
+        );
         let history = r.history.borrow();
         let s = history.peek_undo().expect("a step");
         assert_eq!(s.describe(), "adding intro.mp4");
diff --git a/crates/kuvatin/ui/app.slint b/crates/kuvatin/ui/app.slint
index 1e282ec..2adf380 100644
--- a/crates/kuvatin/ui/app.slint
+++ b/crates/kuvatin/ui/app.slint
@@ -209,6 +209,12 @@ export component AppWindow inherits Window {
     callback export-cancel();                  // abort a running render
     callback import-cancel();                  // abort an in-flight media import
     callback add-track();                      // click "+ New track" → new empty row
+    // A track header's controls (gui/video/tracks.rs): the row, and its new
+    // state. Mute, lock and rename are undoable edits; solo is for listening.
+    callback track-muted(int, bool);
+    callback track-soloed(int, bool);
+    callback track-locked(int, bool);
+    callback track-renamed(int, string);
     // Undo and redo, one history per mode (gui/history.rs). The hints name the
     // step ("Undo trimming intro.mp4") and show on hover over the buttons.
     in property <bool> video-can-undo: false;
```

- [ ] **Step 2: Build and test**

Run: `cargo test -p kuvatin`
Expected: 296 passed, 2 ignored. Among the new ones: `track_steps_describe_which_way_they_went`, `renames_of_one_track_merge_and_nothing_else_about_tracks_does`, `a_rename_typed_back_to_the_old_name_leaves_nothing`, `a_mute_and_an_unmute_are_two_steps`, and `muting_a_row_is_a_step_named_for_its_track`, which records "muting Track 2" with no engine at all.

- [ ] **Step 3: Gates and commit**

```bash
git add crates/kuvatin/src/gui/video crates/kuvatin/ui/app.slint
git commit -m "Muting, locking and renaming a track are undoable steps" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: The table is saved and opened

Saving writes the rows into `doc.tracks` (solo left out), the way it already patches each sequence clip's spec. Opening builds the rows from the table, padded to reach the deepest clip and to the two an empty project starts with, the largest of the three winning, every row unsoloed; then the engine hears the saved mutes. Opening does not mark the project unsaved: `set_track_mutes` only repaints.

**Files:**
- Modify: `crates/kuvatin/src/gui/video/project_file.rs`

- [ ] **Step 1: Apply the diff**

```diff
diff --git a/crates/kuvatin/src/gui/video/project_file.rs b/crates/kuvatin/src/gui/video/project_file.rs
index f193446..3a59aa9 100644
--- a/crates/kuvatin/src/gui/video/project_file.rs
+++ b/crates/kuvatin/src/gui/video/project_file.rs
@@ -26,6 +26,7 @@ pub(super) fn wire(ui: &AppWindow, st: &VideoState, im: &super::import::ImportSt
         let ui_weak = ui_weak.clone();
         let project_slot = st.project.clone();
         let seq_by_path = im.seq_by_path.clone();
+        let track_rows = st.tracks.clone();
         let current = current.clone();
         ui.on_video_save_project(move |ask_where| {
             let Some(ui) = ui_weak.upgrade() else {
@@ -51,6 +52,9 @@ pub(super) fn wire(ui: &AppWindow, st: &VideoState, im: &super::import::ImportSt
                         .find(|s| s.uri().map(|u| u == rec.uri).unwrap_or(false))
                         .cloned();
                 }
+                // Names and locks live only here, and the engine silences
+                // solo too, so the table comes from the rows: solo left out.
+                doc.tracks = super::tracks::records(&super::tracks::rows_of(&track_rows));
                 doc
             };
             let existing = current.borrow().clone();
@@ -228,15 +232,13 @@ fn restore_models(
         .collect();
     st.tl_clips.set_vec(rows);
 
-    // Tracks: as many as the deepest clip uses, and never fewer than the two
-    // an empty project starts with.
-    let needed = records
-        .iter()
-        .map(|(_, r)| r.track + 1)
-        .max()
-        .unwrap_or(0)
-        .max(2);
-    st.tracks.set_vec(vec![TimelineTrack::default(); needed]);
+    // Tracks: the saved table, reaching the deepest clip, and then the engine
+    // hears the saved mutes.
+    let deepest = records.iter().map(|(_, r)| r.track + 1).max().unwrap_or(0);
+    st.tracks.set_vec(track_rows_on_open(&doc.tracks, deepest));
+    if let Some(p) = st.project.borrow_mut().as_mut() {
+        super::tracks::push_mutes(p, &st.tracks);
+    }
 
     // Media bin: one row per distinct source, and the sequence specs come back
     // with it so a bin click re-adds the sequence rather than a single still.
@@ -288,6 +290,19 @@ fn restore_models(
     spawn_thumbnails(ui.as_weak(), records);
 }
 
+/// The track rows a project opens with: its saved table, padded with unnamed
+/// rows to reach the deepest clip (`deepest` is that clip's track plus one)
+/// and to the two an empty project starts with. The largest of the three
+/// wins: a table shorter than the deepest clip would otherwise lose a track,
+/// and a file from 2.13 or earlier has no table at all. Solo is never saved,
+/// so every row opens unsoloed.
+fn track_rows_on_open(table: &[kuvatin_video::TrackRecord], deepest: usize) -> Vec<TimelineTrack> {
+    let needed = table.len().max(deepest).max(2);
+    (0..needed)
+        .map(|i| super::tracks::row(&table.get(i).cloned().unwrap_or_default(), false))
+        .collect()
+}
+
 /// Decode one thumbnail per clip on a worker and drop each into its row (and
 /// the matching media-bin row) as it arrives.
 pub(super) fn spawn_thumbnails(
@@ -397,6 +412,49 @@ mod tests {
         assert_eq!(kind_of("file:///C:/shots/logo.png?x=1"), ClipKind::Image);
     }
 
+    fn named(name: &str) -> kuvatin_video::TrackRecord {
+        kuvatin_video::TrackRecord {
+            name: name.into(),
+            ..Default::default()
+        }
+    }
+
+    /// The saved table decides, when it is the longest of the three.
+    #[test]
+    fn a_project_opens_with_its_saved_tracks() {
+        let rows = track_rows_on_open(&[named("A"), named("B"), named("C")], 1);
+        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
+        assert_eq!(names, vec!["A", "B", "C"]);
+    }
+
+    /// A table shorter than the deepest clip would lose a track: the clip
+    /// wins, and the rows past the table are unnamed.
+    #[test]
+    fn the_deepest_clip_outranks_a_shorter_table() {
+        let rows = track_rows_on_open(&[named("A")], 4);
+        assert_eq!(rows.len(), 4);
+        assert_eq!(rows[0].name.as_str(), "A");
+        assert_eq!(rows[3], TimelineTrack::default());
+    }
+
+    /// A file with no table and a clip on the top track only still opens
+    /// with the two tracks every project starts with: 2.13 and earlier.
+    #[test]
+    fn a_project_never_opens_with_fewer_than_two_tracks() {
+        assert_eq!(track_rows_on_open(&[], 1).len(), 2);
+        assert_eq!(track_rows_on_open(&[], 0).len(), 2);
+    }
+
+    /// Solo is not saved, so no key a file could carry brings it back.
+    #[test]
+    fn no_track_opens_soloed() {
+        let text = "version = 1\ncanvas_w = 1280\ncanvas_h = 720\n\n[[tracks]]\nname = \"A\"\nsoloed = true\n";
+        let doc: kuvatin_video::ProjectFile = toml::from_str(text).expect("a project");
+        let rows = track_rows_on_open(&doc.tracks, 0);
+        assert!(rows.iter().all(|r| !r.soloed));
+        assert_eq!(rows[0].name.as_str(), "A");
+    }
+
     #[test]
     fn the_label_is_the_file_name() {
         assert_eq!(
```

- [ ] **Step 2: Build and test**

Run: `cargo test -p kuvatin -- project_file::tests`
Expected: 6 passed, the four new ones one per rule (the table, the deepest clip, the floor of two, and no row opening soloed even if a file says so). `cargo test -p kuvatin`: 300 passed, 2 ignored.

- [ ] **Step 3: Gates and commit**

```bash
git add crates/kuvatin/src/gui/video/project_file.rs
git commit -m "A project saves its track names, mutes and locks, and opens with them" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: The header has M, S and L

The gutter widens from 94 px to 168 px; the row height stays `Theme.track-h`, which the whole timeline is built on. Each header is the grip and the name on the left, the part that reorders, and three 20 px `TrackToggle` buttons on the right, which never start a drag. Each button's hint is its tooltip and accessible label ("Mute Dialogue", "Stop soloing Dialogue", "Unlock Dialogue"); a click hands focus back to the window's key scope. A muted track's name dims, and the lane stripe goes darker for a muted track and flat grey for a locked one.

A reorder now moves the row, so its name, mute, solo and lock go with its layer; a locked track does not move (its header shows no drag cursor and the handler refuses it silently), though a neighbour's reorder can still shift it, since that changes nothing on it but its place. With no engine yet, the rows move alone and the step is still recorded.

**Files:**
- Modify: `crates/kuvatin/ui/widgets.slint` (`TrackToggle`, after `TimelineChip`)
- Modify: `crates/kuvatin/ui/app.slint` (the import, the header, the lane stripes)
- Modify: `crates/kuvatin/src/gui/video/timeline.rs` (`on_track_reordered`), `tracks.rs` (`locked`)

- [ ] **Step 1: Apply the diff**

```diff
diff --git a/crates/kuvatin/src/gui/video/timeline.rs b/crates/kuvatin/src/gui/video/timeline.rs
index 15a4fa8..d97f3fc 100644
--- a/crates/kuvatin/src/gui/video/timeline.rs
+++ b/crates/kuvatin/src/gui/video/timeline.rs
@@ -1,6 +1,7 @@
 //! Timeline editing: selection + inspector, slide / trim / move-to-track,
 //! magnetic snapping, track rows and clip removal.
 
+use super::tracks;
 use super::undo::{Recorder, StepKind, Subject};
 use super::{VideoState, MAX_SCALE_PCT, MIN_SCALE_PCT, SPEEDS};
 use crate::gui::{show_error, AppWindow, ClipKind, TimelineClip, TimelineTrack};
@@ -209,33 +210,45 @@ pub(super) fn wire(ui: &AppWindow, st: &VideoState) {
         });
     }
 
-    // Reorder tracks by dragging a header: move the GES layer, then resync
-    // every clip's track from GES (a reorder shifts several layers' indices).
+    // Reorder tracks by dragging a header: move the row, and the GES layer
+    // with it, then resync every clip's track from GES (a reorder shifts
+    // several layers' indices). The row carries the track's name, mute, solo
+    // and lock. Before any clip exists there is no engine, only rows to move.
     {
         let project_slot = project_slot.clone();
         let tl_clips = tl_clips.clone();
+        let track_rows = video_tracks.clone();
         let rec = rec.clone();
         ui.on_track_reordered(move |from, to| {
-            if from == to {
+            let rows = tracks::rows_of(&track_rows);
+            let (Ok(f), Ok(t)) = (usize::try_from(from), usize::try_from(to)) else {
+                return;
+            };
+            // A locked track does not move; one next to it still can, since
+            // that changes nothing on it but its place.
+            if f == t || f >= rows.len() || t >= rows.len() || tracks::locked(&rows, from) {
                 return;
             }
             let mut slot = project_slot.borrow_mut();
-            let Some(p) = slot.as_mut() else {
-                return;
-            };
-            let before = rec.before(Some(&*p));
-            p.move_track(from as usize, to as usize);
-            for idx in 0..tl_clips.row_count() {
-                if let Some(mut row) = tl_clips.row_data(idx) {
-                    if let Some(t) = p.clip_track(&kuvatin_video::ClipId(row.id.to_string())) {
-                        if row.track != t as i32 {
-                            row.track = t as i32;
-                            tl_clips.set_row_data(idx, row);
+            let before = rec.before(slot.as_ref());
+            let moved = rows[f].clone();
+            track_rows.remove(f);
+            track_rows.insert(t, moved);
+            if let Some(p) = slot.as_mut() {
+                p.move_track(f, t);
+                for idx in 0..tl_clips.row_count() {
+                    if let Some(mut row) = tl_clips.row_data(idx) {
+                        if let Some(t) = p.clip_track(&kuvatin_video::ClipId(row.id.to_string())) {
+                            if row.track != t as i32 {
+                                row.track = t as i32;
+                                tl_clips.set_row_data(idx, row);
+                            }
                         }
                     }
                 }
+                tracks::push_mutes(p, &track_rows);
             }
-            rec.record(Some(&*p), StepKind::ReorderTracks, None, before);
+            rec.record(slot.as_ref(), StepKind::ReorderTracks, None, before);
         });
     }
 
diff --git a/crates/kuvatin/src/gui/video/tracks.rs b/crates/kuvatin/src/gui/video/tracks.rs
index 6dcf0d4..eacbf8e 100644
--- a/crates/kuvatin/src/gui/video/tracks.rs
+++ b/crates/kuvatin/src/gui/video/tracks.rs
@@ -104,6 +104,15 @@ pub(super) fn effective_mutes(rows: &[TimelineTrack]) -> Vec<bool> {
         .collect()
 }
 
+/// Whether the track at `t` refuses edits. A track past the end, or no track
+/// at all (-1), is not locked: a new bottom track never is.
+pub(super) fn locked(rows: &[TimelineTrack], t: i32) -> bool {
+    usize::try_from(t)
+        .ok()
+        .and_then(|t| rows.get(t))
+        .is_some_and(|r| r.locked)
+}
+
 /// The rows as they are stored and undone: name, mute and lock. Solo is left
 /// out.
 pub(super) fn records(rows: &[TimelineTrack]) -> Vec<TrackRecord> {
@@ -227,6 +236,15 @@ pub(super) mod tests {
         assert!(!row(&got[0], false).soloed);
     }
 
+    #[test]
+    fn only_a_row_that_exists_can_be_locked() {
+        let rows = [trk("", false, false, true), trk("", false, false, false)];
+        assert!(locked(&rows, 0));
+        assert!(!locked(&rows, 1));
+        assert!(!locked(&rows, 2), "a new bottom track");
+        assert!(!locked(&rows, -1), "no track");
+    }
+
     #[test]
     fn a_track_is_called_by_its_name_or_its_number() {
         let table = records(&[
diff --git a/crates/kuvatin/ui/app.slint b/crates/kuvatin/ui/app.slint
index 2adf380..aa6cb8f 100644
--- a/crates/kuvatin/ui/app.slint
+++ b/crates/kuvatin/ui/app.slint
@@ -4,7 +4,7 @@ import { Modal } from "modal.slint";
 import {
     QualitySlider, Card, SecondaryButton, DialogButton, RemoveButton, FieldLabel, CropHandle,
     WinButton, SegToggle, Splitter, NumberDropdown, PillButton, Scrubber, InspSlider,
-    TimelineChip, TooltipLayer, Gesture
+    TimelineChip, TrackToggle, TooltipLayer, Gesture
 } from "widgets.slint";
 export { ProgressWindow } from "progress.slint";
 
@@ -1877,34 +1877,83 @@ export component AppWindow inherits Window {
                             vertical-stretch: 1;
 
                             VerticalLayout {
-                                width: 94px;
-                                // Drag a header up/down to reorder tracks.
+                                // Wide enough for a name and three buttons. The row
+                                // height stays Theme.track-h: the whole timeline is
+                                // built on it, and the lane beside this simply gets
+                                // narrower.
+                                width: 168px;
                                 for trk[t] in root.timeline-tracks : hdr := Rectangle {
+                                    // "Track 3" until it is named.
+                                    property <string> title: trk.name != "" ? trk.name : "Track " + (t + 1);
+                                    property <length> ddy: 0px;
+                                    // Three 20 px buttons, 2 px apart, 8 px in from the right.
+                                    property <length> buttons-w: 3 * 20px + 2 * 2px;
                                     height: root.track-h;
                                     background: hta.pressed ? Theme.card : #13161c;
                                     animate background { duration: 100ms; }
                                     border-width: 1px; border-color: #181c23;
-                                    property <length> ddy: 0px;
-                                    HorizontalLayout {
-                                        padding-left: 9px; spacing: 6px;
-                                        Text { text: "≡"; color: hta.has-hover ? Theme.muted2 : #5e6675; font-size: 11px; vertical-alignment: center; animate color { duration: 120ms; } }
-                                        Text { text: trk.name != "" ? trk.name : "Track " + (t + 1); color: Theme.muted2; font-size: 9px; vertical-alignment: center; overflow: elide; }
-                                    }
-                                    hta := TouchArea {
-                                        mouse-cursor: ns-resize;
-                                        moved => { hdr.ddy = self.mouse-y - self.pressed-y; }
-                                        pointer-event(ev) => {
-                                            Gesture.held = ev.kind == PointerEventKind.down || (Gesture.held && ev.kind != PointerEventKind.up && ev.kind != PointerEventKind.cancel);
-                                            if (ev.kind == PointerEventKind.down) { kbd.focus(); }
-                                            if (ev.kind == PointerEventKind.up) {
-                                                if (Math.abs(hdr.ddy / 1px) > 15) {
-                                                    root.track-reordered(t, Math.clamp(t + Math.round(hdr.ddy / root.track-h), 0, root.timeline-tracks.length - 1));
+                                    // The grip and the name: the part that reorders. It
+                                    // stops short of the buttons, so a click on a button
+                                    // never starts a drag. A locked track does not move.
+                                    grab := Rectangle {
+                                        x: 0;
+                                        width: parent.width - hdr.buttons-w - 8px - 6px;
+                                        height: parent.height;
+                                        HorizontalLayout {
+                                            padding-left: 9px; spacing: 6px;
+                                            Text { text: "≡"; color: hta.has-hover && !trk.locked ? Theme.muted2 : #5e6675; font-size: 11px; vertical-alignment: center; animate color { duration: 120ms; } }
+                                            Text {
+                                                text: hdr.title;
+                                                color: trk.muted ? Theme.hint : Theme.muted2;
+                                                font-size: 9px; vertical-alignment: center; overflow: elide;
+                                                horizontal-stretch: 1;
+                                            }
+                                        }
+                                        hta := TouchArea {
+                                            mouse-cursor: trk.locked ? MouseCursor.default : MouseCursor.ns-resize;
+                                            moved => {
+                                                if (!trk.locked) {
+                                                    hdr.ddy = self.mouse-y - self.pressed-y;
                                                 }
-                                                hdr.ddy = 0px;
                                             }
-                                            // Reset stale drag on cancel, else the next plain
-                                            // click could fire a spurious reorder.
-                                            if (ev.kind == PointerEventKind.cancel) { hdr.ddy = 0px; }
+                                            pointer-event(ev) => {
+                                                Gesture.held = ev.kind == PointerEventKind.down || (Gesture.held && ev.kind != PointerEventKind.up && ev.kind != PointerEventKind.cancel);
+                                                if (ev.kind == PointerEventKind.down) { kbd.focus(); }
+                                                if (ev.kind == PointerEventKind.up) {
+                                                    if (Math.abs(hdr.ddy / 1px) > 15) {
+                                                        root.track-reordered(t, Math.clamp(t + Math.round(hdr.ddy / root.track-h), 0, root.timeline-tracks.length - 1));
+                                                    }
+                                                    hdr.ddy = 0px;
+                                                }
+                                                // Reset stale drag on cancel, else the next plain
+                                                // click could fire a spurious reorder.
+                                                if (ev.kind == PointerEventKind.cancel) { hdr.ddy = 0px; }
+                                            }
+                                        }
+                                    }
+                                    HorizontalLayout {
+                                        x: parent.width - hdr.buttons-w - 8px;
+                                        y: (parent.height - 20px) / 2;
+                                        width: hdr.buttons-w;
+                                        height: 20px;
+                                        spacing: 2px;
+                                        TrackToggle {
+                                            label: "M";
+                                            on: trk.muted;
+                                            hint: (trk.muted ? "Unmute " : "Mute ") + hdr.title;
+                                            toggled => { root.track-muted(t, !trk.muted); kbd.focus(); }
+                                        }
+                                        TrackToggle {
+                                            label: "S";
+                                            on: trk.soloed;
+                                            hint: (trk.soloed ? "Stop soloing " : "Solo ") + hdr.title;
+                                            toggled => { root.track-soloed(t, !trk.soloed); kbd.focus(); }
+                                        }
+                                        TrackToggle {
+                                            label: "L";
+                                            on: trk.locked;
+                                            hint: (trk.locked ? "Unlock " : "Lock ") + hdr.title;
+                                            toggled => { root.track-locked(t, !trk.locked); kbd.focus(); }
                                         }
                                     }
                                 }
@@ -1960,7 +2009,12 @@ export component AppWindow inherits Window {
 
                                     // row background stripes (fixed; span the visible lane)
                                     VerticalLayout {
-                                        for trk[t] in root.timeline-tracks : Rectangle { height: root.track-h; border-width: 1px; border-color: #181c23; }
+                                        // A muted track reads darker and a locked one flat grey,
+                                        // so the lane says so without a look at the gutter.
+                                        for trk[t] in root.timeline-tracks : Rectangle {
+                                            height: root.track-h; border-width: 1px; border-color: #181c23;
+                                            background: trk.locked ? Theme.well : (trk.muted ? #090b0f : transparent);
+                                        }
                                     }
 
                                     // clip blocks. The wrapper `blk` keeps the clip's MODEL
diff --git a/crates/kuvatin/ui/widgets.slint b/crates/kuvatin/ui/widgets.slint
index 5de3cbd..b243ea5 100644
--- a/crates/kuvatin/ui/widgets.slint
+++ b/crates/kuvatin/ui/widgets.slint
@@ -724,6 +724,60 @@ export component TimelineChip inherits Rectangle {
     }
 }
 
+// One of a track header's M, S and L buttons: a 20 px square that lights up
+// while it is on. Its hint ("Mute Dialogue") shows on hover and is its
+// accessible label; to assistive technology it is a checkable button. It
+// does not take keyboard focus; the header hands focus back to the window's
+// key scope after a click, as the timeline chips do.
+export component TrackToggle inherits Rectangle {
+    in property <string> label;
+    in property <string> hint;
+    in property <bool> on;
+    callback toggled();
+    width: 20px;
+    height: 20px;
+    border-radius: 4px;
+    background: root.on ? Theme.accent2 : (ta.has-hover ? Theme.hover : transparent);
+    animate background { duration: 110ms; }
+    accessible-role: button;
+    accessible-label: root.hint;
+    accessible-checkable: true;
+    accessible-checked: root.on;
+    accessible-action-default => { root.toggled(); }
+    // The hint flips under the pointer after a click ("Unmute Dialogue").
+    changed hint => {
+        if (ta.has-hover) {
+            Tooltip.text = root.hint;
+        }
+    }
+    Text {
+        text: root.label;
+        color: root.on ? Theme.on-accent : (ta.has-hover ? Theme.ink2 : #5e6675);
+        font-size: 9.5px;
+        font-weight: 700;
+        horizontal-alignment: center;
+        vertical-alignment: center;
+    }
+    ta := TouchArea {
+        mouse-cursor: pointer;
+        // A click is a gesture too: undo waits for the button to come up.
+        pointer-event(ev) => {
+            Gesture.held = ev.kind == PointerEventKind.down || (Gesture.held && ev.kind != PointerEventKind.up && ev.kind != PointerEventKind.cancel);
+        }
+        clicked => { root.toggled(); }
+        changed has-hover => {
+            if (self.has-hover) {
+                Tooltip.text = root.hint;
+                Tooltip.left = root.absolute-position.x;
+                Tooltip.top = root.absolute-position.y + root.height + 4px;
+                Tooltip.shown = true;
+            } else if (Tooltip.text == root.hint) {
+                Tooltip.shown = false;
+            }
+        }
+    }
+}
+
 // The one tooltip in the window. Slint has no tooltip element: a control with
 // a hint writes its text and position here while it is hovered, and
 // `TooltipLayer`, the last thing the window draws, shows it above everything.
```

- [ ] **Step 2: Build and test**

Run: `cargo test -p kuvatin`
Expected: 301 passed, 2 ignored (`only_a_row_that_exists_can_be_locked` is new).

- [ ] **Step 3: Look at it**

Run: `cargo run -p kuvatin`. In Videos mode each track header reads "≡ Track 1" with M, S, L on the right. Click M on Track 2: it lights, the name dims, and the Undo chip's hint reads "Undo muting Track 2" (no clip needed). Hover each button: its hint names the track. Close the window.

- [ ] **Step 4: Gates and commit**

```bash
git add crates/kuvatin/ui crates/kuvatin/src/gui/video
git commit -m "Each track header has mute, solo and lock buttons, and a reorder carries them" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: A track is renamed in place

Double-click a track's name to edit it where it is: a bare `TextInput` over the name, seeded with the name (empty for "Track N"), everything selected. Return keeps what was typed, so does clicking away, and Esc puts the name back; an empty name means "Track N" again. The field is focused as it appears, so its keys are its own: Space, Delete and the transport letters type, and Ctrl+Z is the field's own text undo (`undo-keys` already refuses while a text input has focus).

**Files:**
- Modify: `crates/kuvatin/ui/app.slint` (`track-renaming` after the track callbacks; the header's `double-clicked` and the edit field)

- [ ] **Step 1: Apply the diff**

```diff
diff --git a/crates/kuvatin/ui/app.slint b/crates/kuvatin/ui/app.slint
index aa6cb8f..622863e 100644
--- a/crates/kuvatin/ui/app.slint
+++ b/crates/kuvatin/ui/app.slint
@@ -215,6 +215,9 @@ export component AppWindow inherits Window {
     callback track-soloed(int, bool);
     callback track-locked(int, bool);
     callback track-renamed(int, string);
+    // The track whose name is being edited in place (double-click a name), or
+    // -1 for none.
+    in-out property <int> track-renaming: -1;
     // Undo and redo, one history per mode (gui/history.rs). The hints name the
     // step ("Undo trimming intro.mp4") and show on hover over the buttons.
     in property <bool> video-can-undo: false;
@@ -1929,6 +1932,49 @@ export component AppWindow inherits Window {
                                                 // click could fire a spurious reorder.
                                                 if (ev.kind == PointerEventKind.cancel) { hdr.ddy = 0px; }
                                             }
+                                            double-clicked => { root.track-renaming = t; }
+                                        }
+                                    }
+                                    // Renaming: an edit field over the name, seeded with it.
+                                    // Return or a click elsewhere keeps what was typed, Esc
+                                    // puts the name back, and an empty name means "Track N"
+                                    // again. A bare TextInput: the standard LineEdit is at
+                                    // least 32 px tall and 160 px wide, more than a header has.
+                                    if root.track-renaming == t : Rectangle {
+                                        x: 22px; y: 4px;
+                                        width: grab.width - 22px - 2px; height: parent.height - 8px;
+                                        background: Theme.well;
+                                        border-radius: 3px; border-width: 1px; border-color: Theme.accent;
+                                        name-edit := TextInput {
+                                            x: 4px; width: parent.width - 8px; height: parent.height;
+                                            text: trk.name;
+                                            color: Theme.ink;
+                                            font-size: 9.5px;
+                                            vertical-alignment: center;
+                                            single-line: true;
+                                            accessible-label: "Name of " + hdr.title;
+                                            init => { self.focus(); self.select-all(); }
+                                            // Once, whichever way the edit ends; a destroyed
+                                            // field's lost focus must not commit it again.
+                                            function finish(keep: bool) {
+                                                if (root.track-renaming != t) { return; }
+                                                if (keep) { root.track-renamed(t, self.text); }
+                                                root.track-renaming = -1;
+                                            }
+                                            accepted => { self.finish(true); kbd.focus(); }
+                                            key-pressed(e) => {
+                                                if (e.text == Key.Escape) {
+                                                    self.finish(false);
+                                                    kbd.focus();
+                                                    return accept;
+                                                }
+                                                return reject;
+                                            }
+                                            // Clicking away keeps the name rather than losing it,
+                                            // and leaves focus where the click put it.
+                                            changed has-focus => {
+                                                if (!self.has-focus) { self.finish(true); }
+                                            }
                                         }
                                     }
                                     HorizontalLayout {
```

- [ ] **Step 2: Build**

Run: `cargo build -p kuvatin` and `cargo test -p kuvatin` (301 passed; nothing here is a pure function).

- [ ] **Step 3: Look at it**

Run: `cargo run -p kuvatin`. Double-click "Track 1": an edit field with an accent border appears in its place. Type "Dialogue", press Return: the header reads "Dialogue" and the Undo hint "Undo renaming Track 1". Ctrl+Z: "Track 1". Ctrl+Y: "Dialogue". Close the window.

- [ ] **Step 4: Gates and commit**

```bash
git add crates/kuvatin/ui/app.slint
git commit -m "A double-click on a track's name renames it in place" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 10: A locked track refuses edits

Every path that changes a clip checks the tracks it touches and, if one is locked, refuses with "Dialogue is locked" / "Unlock the track to change what is on it." and changes nothing: a drop (the clip's track and the one it would land on, worked out before anything moves, so slide and track change are refused together), a trim, Split, the Duration field, the Speed list (which then shows the speed the clip still has), the × and the Delete key (one guard in `remove_timeline_clip`), and adding a clip from the media bin (images land on track 0, videos and sequences on track 1). The transform timer drops a value for a clip on a locked track without a message: the inspector and the preview box stand down for such a clip (Task 11), so only a value stashed just before the lock went on gets there.

Undo and redo are not guarded: a lock guards against new mistakes, and a step already in the history must stay appliable.

**Files:**
- Modify: `crates/kuvatin/src/gui/video/tracks.rs` (`first_locked`, `refusal`, `refuse_locked`)
- Modify: `crates/kuvatin/src/gui/video/timeline.rs`, `mod.rs`

- [ ] **Step 1: Apply the diff**

```diff
diff --git a/crates/kuvatin/src/gui/video/mod.rs b/crates/kuvatin/src/gui/video/mod.rs
index d1775c8..9948d45 100644
--- a/crates/kuvatin/src/gui/video/mod.rs
+++ b/crates/kuvatin/src/gui/video/mod.rs
@@ -353,16 +353,26 @@ pub(super) fn wire(
                     return;
                 };
                 // Apply the latest inspector transform (if any) then repaint,
-                // both coalesced to one commit + one seek per tick.
+                // both coalesced to one commit + one seek per tick. A clip on
+                // a locked track keeps its transform and the value is
+                // dropped: its sliders and preview box stand down, so only a
+                // value stashed just before the lock went on gets here.
                 if let Some((id, l)) = pending_xform.borrow_mut().take() {
-                    let before = rec.before(Some(&*project));
-                    project.set_clip_layout(&kuvatin_video::ClipId(id.clone()), l);
-                    rec.record(
-                        Some(&*project),
-                        undo::StepKind::Transform,
-                        Some(undo::Subject::Clip(id.clone())),
-                        before,
-                    );
+                    let track = rec
+                        .tl_clips
+                        .iter()
+                        .find(|r| r.id.as_str() == id)
+                        .map_or(-1, |r| r.track);
+                    if !tracks::locked(&tracks::rows_of(&rec.tracks), track) {
+                        let before = rec.before(Some(&*project));
+                        project.set_clip_layout(&kuvatin_video::ClipId(id.clone()), l);
+                        rec.record(
+                            Some(&*project),
+                            undo::StepKind::Transform,
+                            Some(undo::Subject::Clip(id.clone())),
+                            before,
+                        );
+                    }
                 }
                 // Scrub target: one (keyframe) seek per tick during a drag,
                 // a frame-accurate one on release.
@@ -521,14 +531,6 @@ fn add_to_timeline(
     rec: &undo::Recorder,
     waves: &waves::Waves,
 ) {
-    if project_slot.borrow().is_none() {
-        *project_slot.borrow_mut() = make_project(ui_weak);
-    }
-    let mut slot = project_slot.borrow_mut();
-    let Some(project) = slot.as_mut() else {
-        return;
-    };
-    let before = rec.before(Some(&*project));
     let ext = path
         .extension()
         .map(|e| e.to_string_lossy().to_lowercase())
@@ -537,7 +539,20 @@ fn add_to_timeline(
     let img_dur = is_img.then(|| std::time::Duration::from_secs(5));
     // GES composites lower layer indices ON TOP, so images (overlays) go on
     // layer 0 and videos on layer 1 (the base, underneath).
-    let track = if is_img { 0 } else { 1 };
+    let track: usize = if is_img { 0 } else { 1 };
+    if let Some(ui) = ui_weak.upgrade() {
+        if tracks::refuse_locked(&ui, &tracks::rows_of(&rec.tracks), &[track as i32]) {
+            return;
+        }
+    }
+    if project_slot.borrow().is_none() {
+        *project_slot.borrow_mut() = make_project(ui_weak);
+    }
+    let mut slot = project_slot.borrow_mut();
+    let Some(project) = slot.as_mut() else {
+        return;
+    };
+    let before = rec.before(Some(&*project));
     match project.append_clip(path, track, img_dur) {
         Ok(info) => {
             let name: SharedString = path
@@ -603,6 +618,14 @@ fn add_sequence_to_timeline(
     thumb: Image,
     rec: &undo::Recorder,
 ) {
+    // Sequences are footage, not overlays: the base video track (GES
+    // composites lower layer indices on top, so videos live on 1).
+    let track: usize = 1;
+    if let Some(ui) = ui_weak.upgrade() {
+        if tracks::refuse_locked(&ui, &tracks::rows_of(&rec.tracks), &[track as i32]) {
+            return;
+        }
+    }
     if project_slot.borrow().is_none() {
         *project_slot.borrow_mut() = make_project(ui_weak);
     }
@@ -613,9 +636,7 @@ fn add_sequence_to_timeline(
     let before = rec.before(Some(&*project));
     let added = spec
         .uri()
-        // Sequences are footage, not overlays: the base video track (GES
-        // composites lower layer indices on top, so videos live on 1).
-        .and_then(|uri| project.append_clip_uri(&uri, 1, None));
+        .and_then(|uri| project.append_clip_uri(&uri, track, None));
     match added {
         Ok(info) => {
             tl_clips.push(TimelineClip {
diff --git a/crates/kuvatin/src/gui/video/timeline.rs b/crates/kuvatin/src/gui/video/timeline.rs
index d97f3fc..8eddb7f 100644
--- a/crates/kuvatin/src/gui/video/timeline.rs
+++ b/crates/kuvatin/src/gui/video/timeline.rs
@@ -118,7 +118,7 @@ pub(super) fn wire(ui: &AppWindow, st: &VideoState) {
         let ui_weak = ui_weak.clone();
         let project_slot = project_slot.clone();
         let tl_clips = tl_clips.clone();
-        let tracks = video_tracks.clone();
+        let track_rows = video_tracks.clone();
         let rec = rec.clone();
         ui.on_timeline_clip_dropped(move |i, delta_secs, delta_rows| {
             let Some(mut row) = tl_clips.row_data(i as usize) else {
@@ -129,6 +129,24 @@ pub(super) fn wire(ui: &AppWindow, st: &VideoState) {
             let Some(p) = slot.as_mut() else {
                 return;
             };
+            // Where it would land, worked out before anything moves: a drop
+            // that touches a locked track is refused whole, slide and all.
+            let target = if delta_rows != 0 {
+                drop_target_track(
+                    row.track,
+                    delta_rows,
+                    p.track_count(),
+                    track_rows.row_count(),
+                )
+            } else {
+                row.track
+            };
+            if let Some(ui) = ui_weak.upgrade() {
+                let rows = tracks::rows_of(&track_rows);
+                if tracks::refuse_locked(&ui, &rows, &[row.track, target]) {
+                    return;
+                }
+            }
             let before = rec.before(Some(&*p));
             // Horizontal: slide along the track.
             if let Some(geom) = p.slide_clip(&cid, delta_secs as f64) {
@@ -137,19 +155,15 @@ pub(super) fn wire(ui: &AppWindow, st: &VideoState) {
                 row.duration = geom.duration.as_secs_f32();
             }
             // Vertical: move to another track, or a new bottom track.
-            if delta_rows != 0 {
-                let target =
-                    drop_target_track(row.track, delta_rows, p.track_count(), tracks.row_count());
-                if target != row.track {
-                    if let Some(t) = p.move_clip_to_track(&cid, target as usize) {
-                        row.track = t as i32;
-                    }
+            if target != row.track {
+                if let Some(t) = p.move_clip_to_track(&cid, target as usize) {
+                    row.track = t as i32;
                 }
                 // Grow the rows to match any newly created track. A new
                 // track starts unnamed, audible, unsoloed and unlocked.
                 let new_count = p.track_count();
-                while tracks.row_count() < new_count {
-                    tracks.push(TimelineTrack::default());
+                while track_rows.row_count() < new_count {
+                    track_rows.push(TimelineTrack::default());
                 }
             }
             rec.record(
@@ -262,6 +276,11 @@ pub(super) fn wire(ui: &AppWindow, st: &VideoState) {
             let Some(mut row) = tl_clips.row_data(i as usize) else {
                 return;
             };
+            if let Some(ui) = ui_weak.upgrade() {
+                if tracks::refuse_locked(&ui, &tracks::rows_of(&rec.tracks), &[row.track]) {
+                    return;
+                }
+            }
             let geom = project_slot.borrow_mut().as_mut().and_then(|p| {
                 let before = rec.before(Some(&*p));
                 let geom = p.trim_clip(
@@ -314,6 +333,9 @@ pub(super) fn wire(ui: &AppWindow, st: &VideoState) {
             let Some(mut left) = tl_clips.row_data(i as usize) else {
                 return;
             };
+            if tracks::refuse_locked(&ui, &tracks::rows_of(&rec.tracks), &[left.track]) {
+                return;
+            }
             let at = std::time::Duration::from_secs_f64(f64::from(ui.get_playhead().max(0.0)));
             let mut slot = project_slot.borrow_mut();
             let Some(p) = slot.as_mut() else {
@@ -381,6 +403,13 @@ pub(super) fn wire(ui: &AppWindow, st: &VideoState) {
             let Some(mut row) = tl_clips.row_data(i as usize) else {
                 return;
             };
+            if let Some(ui) = ui_weak.upgrade() {
+                if tracks::refuse_locked(&ui, &tracks::rows_of(&rec.tracks), &[row.track]) {
+                    // The field shows what the clip still has.
+                    ui.set_insp_duration_s(row.duration.round().max(1.0) as i32);
+                    return;
+                }
+            }
             let geom = project_slot.borrow_mut().as_mut().and_then(|p| {
                 let before = rec.before(Some(&*p));
                 let geom =
@@ -428,6 +457,11 @@ pub(super) fn wire(ui: &AppWindow, st: &VideoState) {
             let Some(mut row) = tl_clips.row_data(i as usize) else {
                 return;
             };
+            if tracks::refuse_locked(&ui, &tracks::rows_of(&rec.tracks), &[row.track]) {
+                // The list shows the speed the clip still plays at.
+                ui.set_insp_rate_index(speed_index(f64::from(row.rate)));
+                return;
+            }
             let cid = kuvatin_video::ClipId(row.id.to_string());
             let done = project_slot.borrow_mut().as_mut().and_then(|p| {
                 let before = rec.before(Some(&*p));
@@ -512,6 +546,12 @@ fn remove_timeline_clip(
     }
     let mut duration = None;
     if let Some(row) = tl_clips.row_data(i as usize) {
+        // One guard for the × and the Delete key.
+        if let Some(ui) = ui_weak.upgrade() {
+            if tracks::refuse_locked(&ui, &tracks::rows_of(&rec.tracks), &[row.track]) {
+                return;
+            }
+        }
         if let Some(p) = project_slot.borrow_mut().as_mut() {
             let before = rec.before(Some(&*p));
             p.remove_clip(&kuvatin_video::ClipId(row.id.to_string()));
diff --git a/crates/kuvatin/src/gui/video/tracks.rs b/crates/kuvatin/src/gui/video/tracks.rs
index eacbf8e..50896d1 100644
--- a/crates/kuvatin/src/gui/video/tracks.rs
+++ b/crates/kuvatin/src/gui/video/tracks.rs
@@ -113,6 +113,38 @@ pub(super) fn locked(rows: &[TimelineTrack], t: i32) -> bool {
         .is_some_and(|r| r.locked)
 }
 
+/// The first of `touched` that is locked: the track an edit touching those
+/// tracks must be refused for. A drop touches the track the clip is on and
+/// the one it would land on.
+pub(super) fn first_locked(rows: &[TimelineTrack], touched: &[i32]) -> Option<usize> {
+    touched
+        .iter()
+        .copied()
+        .find(|&t| locked(rows, t))
+        .map(|t| t as usize)
+}
+
+/// What an edit refused on the locked track `t` says.
+pub(super) fn refusal(rows: &[TimelineTrack], t: usize) -> (String, String) {
+    (
+        format!("{} is locked", label(&records(rows), t)),
+        "Unlock the track to change what is on it.".into(),
+    )
+}
+
+/// Refuse an edit that touches a locked track, saying which track and how to
+/// get past it. True when the edit must not go ahead. A locked track's clips
+/// lose their handles on screen, so this is for the keyboard, and for a
+/// click that got there first.
+pub(super) fn refuse_locked(ui: &AppWindow, rows: &[TimelineTrack], touched: &[i32]) -> bool {
+    let Some(t) = first_locked(rows, touched) else {
+        return false;
+    };
+    let (title, detail) = refusal(rows, t);
+    crate::gui::show_error(ui, &title, detail);
+    true
+}
+
 /// The rows as they are stored and undone: name, mute and lock. Solo is left
 /// out.
 pub(super) fn records(rows: &[TimelineTrack]) -> Vec<TrackRecord> {
@@ -245,6 +277,30 @@ pub(super) mod tests {
         assert!(!locked(&rows, -1), "no track");
     }
 
+    /// A drop is refused if the clip's track or the one it lands on is
+    /// locked, and the message names the one it found first.
+    #[test]
+    fn an_edit_is_refused_for_the_first_locked_track_it_touches() {
+        let rows = [
+            trk("", false, false, false),
+            trk("Music", false, false, true),
+            trk("", false, false, true),
+        ];
+        assert_eq!(first_locked(&rows, &[0, 0]), None, "neither");
+        assert_eq!(first_locked(&rows, &[1, 0]), Some(1), "the source");
+        assert_eq!(first_locked(&rows, &[0, 2]), Some(2), "the target");
+        assert_eq!(first_locked(&rows, &[2, 1]), Some(2), "both: the source");
+        assert_eq!(first_locked(&rows, &[0, 3]), None, "a new bottom track");
+        assert_eq!(
+            refusal(&rows, 1),
+            (
+                "Music is locked".to_string(),
+                "Unlock the track to change what is on it.".to_string()
+            )
+        );
+        assert_eq!(refusal(&rows, 2).0, "Track 3 is locked");
+    }
+
     #[test]
     fn a_track_is_called_by_its_name_or_its_number() {
         let table = records(&[
```

- [ ] **Step 2: Build and test**

Run: `cargo test -p kuvatin`
Expected: 302 passed, 2 ignored (`an_edit_is_refused_for_the_first_locked_track_it_touches`).

- [ ] **Step 3: Gates and commit**

```bash
git add crates/kuvatin/src/gui/video
git commit -m "A locked track refuses every edit to what is on it, and says so" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 11: A locked clip loses its handles

The refusals are the keyboard's safety net; the mouse should rarely meet one. A clip on a locked track can still be selected, so the inspector shows it, but its body does not drag, its trim grips are off and dim, and its × is gone. A derived `insp-locked` stands down the inspector's sliders (`InspSlider` gains `enabled`), the Speed list, the Duration field, the preview box and the Split chip, and a line under the clip's name says why.

**Files:**
- Modify: `crates/kuvatin/ui/widgets.slint` (`InspSlider`)
- Modify: `crates/kuvatin/ui/app.slint` (`insp-locked`, the Split chip, the inspector, the preview box, the clip block)

- [ ] **Step 1: Apply the diff**

```diff
diff --git a/crates/kuvatin/ui/app.slint b/crates/kuvatin/ui/app.slint
index 622863e..949b6c7 100644
--- a/crates/kuvatin/ui/app.slint
+++ b/crates/kuvatin/ui/app.slint
@@ -266,6 +266,12 @@ export component AppWindow inherits Window {
     // Which clip is selected (-1 = none). The model carries `selected` per
     // clip for drawing; this is the index the keyboard verbs act on.
     in property <int> timeline-selected: -1;
+    // The selected clip sits on a locked track. Its inspector controls, its
+    // box in the preview and Split stand down; gui/video/tracks.rs refuses
+    // the edits too, for anything that gets past this.
+    property <bool> insp-locked: root.timeline-selected >= 0
+        && root.timeline-selected < root.timeline-clips.length
+        && root.timeline-tracks[root.timeline-clips[root.timeline-selected].track].locked;
     callback timeline-seek-time(float);       // seconds (from ruler/lane click)
     callback timeline-clip-dropped(int, float, int); // clip index, delta seconds (slide), delta rows (track change)
     callback timeline-clip-trimmed(int, int, float); // clip index, edge (-1 left / +1 right), delta seconds
@@ -1581,6 +1587,7 @@ export component AppWindow inherits Window {
                                 }
 
                                 eta := TouchArea {
+                                    enabled: !root.insp-locked;
                                     property <EditMode> mode: EditMode.none;
                                     property <float> cen-x: 0;   // canvas center (resize anchor)
                                     property <float> cen-y: 0;
@@ -1735,6 +1742,10 @@ export component AppWindow inherits Window {
                                 animate opacity { duration: 180ms; easing: ease-out; }
                                 init => { self.opacity = 1; }
                             }
+                            if root.inspector-name != "" && root.insp-locked : Text {
+                                text: "On a locked track. Unlock it to change this clip.";
+                                color: Theme.hint; font-size: 10px; wrap: word-wrap;
+                            }
                             if root.inspector-name != "" : VerticalLayout {
                                 // The spare height below the controls belongs to the filler
                                 // after this layout. Without this the layout took a share of
@@ -1746,17 +1757,18 @@ export component AppWindow inherits Window {
                                 animate opacity { duration: 180ms; easing: ease-out; }
                                 init => { self.opacity = 1; }
                                 spacing: 6px;
-                                InspSlider { label: "Position X"; min: -root.canvas-w; max: root.canvas-w; suffix: "px"; value <=> root.insp-posx; changed => { root.inspector-changed(); } }
-                                InspSlider { label: "Position Y"; min: -root.canvas-h; max: root.canvas-h; suffix: "px"; value <=> root.insp-posy; changed => { root.inspector-changed(); } }
-                                InspSlider { label: "Scale"; min: root.insp-scale-min; max: root.insp-scale-max; suffix: "%"; value <=> root.insp-scale; changed => { root.inspector-changed(); } }
-                                InspSlider { label: "Opacity"; min: 0; max: 100; suffix: "%"; value <=> root.insp-alpha; changed => { root.inspector-changed(); } }
-                                if root.insp-has-audio : InspSlider { label: "Volume"; min: 0; max: 100; suffix: "%"; value <=> root.insp-volume; changed => { root.inspector-changed(); } }
+                                InspSlider { enabled: !root.insp-locked; label: "Position X"; min: -root.canvas-w; max: root.canvas-w; suffix: "px"; value <=> root.insp-posx; changed => { root.inspector-changed(); } }
+                                InspSlider { enabled: !root.insp-locked; label: "Position Y"; min: -root.canvas-h; max: root.canvas-h; suffix: "px"; value <=> root.insp-posy; changed => { root.inspector-changed(); } }
+                                InspSlider { enabled: !root.insp-locked; label: "Scale"; min: root.insp-scale-min; max: root.insp-scale-max; suffix: "%"; value <=> root.insp-scale; changed => { root.inspector-changed(); } }
+                                InspSlider { enabled: !root.insp-locked; label: "Opacity"; min: 0; max: 100; suffix: "%"; value <=> root.insp-alpha; changed => { root.inspector-changed(); } }
+                                if root.insp-has-audio : InspSlider { enabled: !root.insp-locked; label: "Volume"; min: 0; max: 100; suffix: "%"; value <=> root.insp-volume; changed => { root.inspector-changed(); } }
                                 // Speed, for clips with source time to stretch: videos and
                                 // image sequences, not stills.
                                 if root.insp-has-rate : VerticalLayout {
                                     spacing: 4px;
                                     Text { text: "Speed"; color: Theme.muted2; font-size: 9.5px; font-weight: 600; }
                                     ComboBox {
+                                        enabled: !root.insp-locked;
                                         model: root.insp-speed-labels;
                                         current-index <=> root.insp-rate-index;
                                         accessible-label: "Speed";
@@ -1773,6 +1785,7 @@ export component AppWindow inherits Window {
                                         Text { text: root.insp-duration-s + " s"; color: #cfd6e2; font-size: 9.5px; font-weight: 600; vertical-alignment: center; }
                                     }
                                     SpinBox {
+                                        enabled: !root.insp-locked;
                                         minimum: 1;
                                         maximum: 3600;
                                         value <=> root.insp-duration-s;
@@ -1824,7 +1837,7 @@ export component AppWindow inherits Window {
                                     label: "Split";
                                     wide: true;
                                     hint: "Split the selected clip at the playhead (S)";
-                                    enabled: root.timeline-selected >= 0 && !root.modal-open() && !root.video-engine-down;
+                                    enabled: root.timeline-selected >= 0 && !root.insp-locked && !root.modal-open() && !root.video-engine-down;
                                     focus-on-click: false;
                                     clicked => { root.timeline-split(); if (!split-chip.has-focus) { kbd.focus(); } }
                                 }
@@ -2076,8 +2089,12 @@ export component AppWindow inherits Window {
                                         accessible-label: clip.name + " \u{2014} track " + (clip.track + 1)
                                             + ", starts " + Math.round(clip.start * 10) / 10 + " s, "
                                             + Math.round(clip.duration * 10) / 10 + " s long"
-                                            + (clip.rate > 0 && clip.rate != 1 ? ", plays at " + clip.rate + "×" : "");
+                                            + (clip.rate > 0 && clip.rate != 1 ? ", plays at " + clip.rate + "×" : "")
+                                            + (blk.locked ? ", locked" : "");
                                         accessible-item-selected: clip.selected;
+                                        // On a locked track: selectable, so the inspector can show
+                                        // it, but not moved, trimmed or removed from here.
+                                        property <bool> locked: root.timeline-tracks[clip.track].locked;
                                         x: (clip.start * root.timeline-pps) * 1px - lane.scroll;
                                         // track 0 = GES layer 0 = top (both on-screen and composited).
                                         y: clip.track * root.track-h + 4px;
@@ -2144,12 +2161,12 @@ export component AppWindow inherits Window {
                                             // left scrim so the name stays legible over the thumb
                                             Rectangle { x: 0; width: 62%; background: @linear-gradient(90deg, #000000bb 0%, #00000000 100%); }
                                             Text { text: clip.rate > 0 && clip.rate != 1 ? clip.name + "  " + clip.rate + "×" : clip.name; color: white; font-size: 9px; font-weight: 700; x: 9px; y: (parent.height - self.height) / 2; }
-                                            Rectangle { x: 0; width: 5px; height: parent.height; background: #ffffffcc; }
-                                            Rectangle { x: parent.width - 5px; width: 5px; height: parent.height; background: #ffffffcc; }
+                                            Rectangle { x: 0; width: 5px; height: parent.height; background: blk.locked ? #ffffff40 : #ffffffcc; }
+                                            Rectangle { x: parent.width - 5px; width: 5px; height: parent.height; background: blk.locked ? #ffffff40 : #ffffffcc; }
                                             // Delete affordance on the selected clip (also Delete key).
                                             // The visible square is 16 px; the hit area spans the
                                             // clip's full height and 24 px of width.
-                                            if clip.selected : Rectangle {
+                                            if clip.selected && !blk.locked : Rectangle {
                                                 x: parent.width - self.width - 3px; y: 3px;
                                                 width: 16px; height: 16px; border-radius: 5px;
                                                 background: del-ta.has-hover ? Theme.danger2 : #00000088;
@@ -2170,13 +2187,15 @@ export component AppWindow inherits Window {
                                         // body: slide (horizontal) + move to another track
                                         // (vertical) + select. Stable frame = blk.
                                         TouchArea {
-                                            mouse-cursor: move;
+                                            mouse-cursor: blk.locked ? MouseCursor.default : MouseCursor.move;
                                             moved => {
-                                                blk.dmode = DragMode.slide;
-                                                blk.ddx = self.mouse-x - self.pressed-x;
-                                                blk.ddy = self.mouse-y - self.pressed-y;
-                                                root.clip-dragging = true;
-                                                root.clip-drag-target-row = clip.track + Math.round(blk.ddy / root.track-h);
+                                                if (!blk.locked) {
+                                                    blk.dmode = DragMode.slide;
+                                                    blk.ddx = self.mouse-x - self.pressed-x;
+                                                    blk.ddy = self.mouse-y - self.pressed-y;
+                                                    root.clip-dragging = true;
+                                                    root.clip-drag-target-row = clip.track + Math.round(blk.ddy / root.track-h);
+                                                }
                                             }
                                             pointer-event(ev) => {
                                                 Gesture.held = ev.kind == PointerEventKind.down || (Gesture.held && ev.kind != PointerEventKind.up && ev.kind != PointerEventKind.cancel);
@@ -2203,6 +2222,7 @@ export component AppWindow inherits Window {
                                         Rectangle {
                                             x: 0; width: 12px; height: parent.height; background: transparent;
                                             TouchArea {
+                                                enabled: !blk.locked;
                                                 mouse-cursor: ew-resize;
                                                 moved => { blk.dmode = DragMode.trim-left; blk.ddx = self.mouse-x - self.pressed-x; }
                                                 pointer-event(ev) => {
@@ -2222,6 +2242,7 @@ export component AppWindow inherits Window {
                                         Rectangle {
                                             x: parent.width - 12px; width: 12px; height: parent.height; background: transparent;
                                             TouchArea {
+                                                enabled: !blk.locked;
                                                 mouse-cursor: ew-resize;
                                                 moved => { blk.dmode = DragMode.trim-right; blk.ddx = self.mouse-x - self.pressed-x; }
                                                 pointer-event(ev) => {
diff --git a/crates/kuvatin/ui/widgets.slint b/crates/kuvatin/ui/widgets.slint
index b243ea5..fa15824 100644
--- a/crates/kuvatin/ui/widgets.slint
+++ b/crates/kuvatin/ui/widgets.slint
@@ -570,20 +570,25 @@ export component InspSlider inherits Rectangle {
     in property <float> min: 0;
     in property <float> max: 100;
     in property <string> suffix: "";
+    // Off for a clip on a locked track: it shows its value and takes no input.
+    in property <bool> enabled: true;
     in-out property <float> value: 0;
     callback changed();
     height: 38px;
+    opacity: root.enabled ? 1 : 0.45;
     accessible-role: slider;
     accessible-label: root.label;
     accessible-value: Math.round(root.value) + root.suffix;
+    accessible-enabled: root.enabled;
     property <float> frac: (root.max - root.min) > 0
         ? Math.clamp((root.value - root.min) / (root.max - root.min), 0, 1) : 0;
     border-radius: 6px;
-    border-width: ifs.has-focus ? 1px : 0px;
+    border-width: ifs.has-focus && root.enabled ? 1px : 0px;
     border-color: Theme.accent;
     // Arrows move by a hundredth of the range, up/down by a tenth — the same
     // proportions whatever the transform's units are.
     ifs := FocusScope {
+        enabled: root.enabled;
         function step(frac-of-range: float) {
             root.value = Math.clamp(
                 root.value + (root.max - root.min) * frac-of-range, root.min, root.max);
@@ -631,6 +636,7 @@ export component InspSlider inherits Rectangle {
                 animate background { duration: 100ms; }
             }
             kta := TouchArea {
+                enabled: root.enabled;
                 function emit(mx: length) {
                     root.value = root.min + Math.clamp(mx / self.width, 0, 1) * (root.max - root.min);
                     root.changed();
```

- [ ] **Step 2: Build**

Run: `cargo build -p kuvatin` and `cargo test -p kuvatin` (302 passed).

- [ ] **Step 3: Look at it**

Run: `cargo run -p kuvatin`. Open a video, lock its track (L), click the clip: it selects, with no ×; the inspector reads "On a locked track. Unlock it to change this clip." with its controls greyed; Split is grey. Drag the clip: it does not move. Press Delete: "Track 2 is locked", and the clip stays. Close the window.

- [ ] **Step 4: Gates and commit**

```bash
git add crates/kuvatin/ui
git commit -m "A clip on a locked track can be selected but not dragged, trimmed or removed" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 12: Solo goes off when an export starts

A solo forgotten on would silence most of an export. When a render starts, after the file is chosen and before `prepare_render` takes the pipeline, every solo button goes off where the user can see it and the engine is told; explicit mutes stay, and are rendered.

**Files:**
- Modify: `crates/kuvatin/src/gui/video/tracks.rs` (`unsoloed`, `clear_solo`)
- Modify: `crates/kuvatin/src/gui/video/export.rs` (`on_video_export`)

- [ ] **Step 1: Apply the diff**

```diff
diff --git a/crates/kuvatin/src/gui/video/export.rs b/crates/kuvatin/src/gui/video/export.rs
index e5e4015..c584290 100644
--- a/crates/kuvatin/src/gui/video/export.rs
+++ b/crates/kuvatin/src/gui/video/export.rs
@@ -141,6 +141,7 @@ pub(super) fn wire(
         let export_path = export_path.clone();
         let export_starting = export_starting.clone();
         let export_fell_back = export_fell_back.clone();
+        let track_rows = st.tracks.clone();
         ui.on_video_export(move || {
             if export_active.get() || export_pending.get() {
                 return;
@@ -195,6 +196,11 @@ pub(super) fn wire(
             // for the teardown, show the modal, and let the progress timer
             // start the render when the pipeline has settled.
             *export_path.borrow_mut() = Some(path.clone());
+            // Solo is for listening. It goes off, visibly, while the engine
+            // still takes a mute: prepare_render stops it taking any.
+            if let Some(p) = project_slot.borrow_mut().as_mut() {
+                super::tracks::clear_solo(p, &track_rows);
+            }
             let prepared = project_slot.borrow().as_ref().map(|p| p.prepare_render());
             match prepared {
                 Some(Ok(())) => {
diff --git a/crates/kuvatin/src/gui/video/tracks.rs b/crates/kuvatin/src/gui/video/tracks.rs
index 50896d1..620eee5 100644
--- a/crates/kuvatin/src/gui/video/tracks.rs
+++ b/crates/kuvatin/src/gui/video/tracks.rs
@@ -189,6 +189,29 @@ pub(super) fn push_mutes(project: &mut kuvatin_video::Project, tracks: &VecModel
     project.set_track_mutes(&effective_mutes(&rows_of(tracks)));
 }
 
+/// The rows with every solo off, and nothing else changed.
+fn unsoloed(rows: &[TimelineTrack]) -> Vec<TimelineTrack> {
+    rows.iter()
+        .map(|r| TimelineTrack {
+            soloed: false,
+            ..r.clone()
+        })
+        .collect()
+}
+
+/// Switch every solo off and tell the engine, before a render: an export
+/// hears every track that is not muted, and a solo forgotten on would
+/// silence most of it. The buttons go off where the user can see them.
+/// Explicit mutes stay, and are rendered.
+pub(super) fn clear_solo(project: &mut kuvatin_video::Project, tracks: &VecModel<TimelineTrack>) {
+    for (i, row) in unsoloed(&rows_of(tracks)).into_iter().enumerate() {
+        if tracks.row_data(i).is_some_and(|was| was.soloed) {
+            tracks.set_row_data(i, row);
+        }
+    }
+    push_mutes(project, tracks);
+}
+
 #[cfg(test)]
 pub(super) mod tests {
     use super::*;
@@ -301,6 +324,26 @@ pub(super) mod tests {
         assert_eq!(refusal(&rows, 2).0, "Track 3 is locked");
     }
 
+    /// Before an export: solo off everywhere, and what is left silent is
+    /// exactly what was muted on purpose.
+    #[test]
+    fn clearing_solo_leaves_the_explicit_mutes() {
+        let rows = [
+            trk("A", false, true, false),
+            trk("B", true, false, true),
+            trk("C", false, false, false),
+        ];
+        let cleared = unsoloed(&rows);
+        assert!(cleared.iter().all(|r| !r.soloed));
+        assert_eq!(
+            records(&cleared),
+            records(&rows),
+            "names, mutes, locks kept"
+        );
+        assert_eq!(effective_mutes(&rows), vec![false, true, true]);
+        assert_eq!(effective_mutes(&cleared), vec![false, true, false]);
+    }
+
     #[test]
     fn a_track_is_called_by_its_name_or_its_number() {
         let table = records(&[
```

- [ ] **Step 2: Build and test**

Run: `cargo test -p kuvatin`
Expected: 303 passed, 2 ignored (`clearing_solo_leaves_the_explicit_mutes`).

- [ ] **Step 3: Gates and commit**

```bash
git add crates/kuvatin/src/gui/video
git commit -m "Solo switches off when an export starts, so every unmuted track is heard" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 13: CI gates on the new tests

The self-contained engine tests join through the `track_mute_` filter; the two live-media tests of Task 3 join the live-media gate by name, in both `$names` and the `--exact` list.

**Files:**
- Modify: `.github/workflows/release.yml`

- [ ] **Step 1: Apply the diff**

```diff
diff --git a/.github/workflows/release.yml b/.github/workflows/release.yml
index c2e58c1..146cc85 100644
--- a/.github/workflows/release.yml
+++ b/.github/workflows/release.yml
@@ -253,7 +253,7 @@ jobs:
             slid neighbour confined_to_the_gap already_overlapping discovery_gives_up `
             without_blocking_the_caller document:: survives_being_saved missing_source encoder undo_ `
             removing_a_clip_straight_after_adding_it_does_not_crash removing_a_clip_added_while_playing_does_not_crash `
-            removing_two_clips_back_to_back_while_playing_does_not_crash a_zoomed_clip_keeps_its_scale split_ frame_length shuttle_ speed_ trim_math_follows_the_rate unsaved_work waveform_
+            removing_two_clips_back_to_back_while_playing_does_not_crash a_zoomed_clip_keeps_its_scale split_ frame_length shuttle_ speed_ trim_math_follows_the_rate unsaved_work waveform_ track_mute_
 
       # Fixtures for everything that needs real media. They live under the
       # workspace (a clean long path — the runner's %TEMP% is an 8.3 short path
@@ -286,9 +286,11 @@ jobs:
       # which pins the order of in-point and duration writes, and a trimmed clip
       # restored with its in-point. One more proves a speed change on real media
       # is two time effects, picture and sound, and one that a real source's
-      # sound is drawn as a waveform. One retry before it counts,
-      # because asset-URI resolution on the hosted runner is occasionally flaky
-      # — a second failure in a row is the code, not the runner.
+      # sound is drawn as a waveform. Two guard track mutes: a muted track is
+      # silent in the export, and a mute leaves the preview playing. One retry
+      # before it counts, because asset-URI resolution on the hosted runner is
+      # occasionally flaky — a second failure in a row is the code, not the
+      # runner.
       - name: Test (live-media regressions — gates the release)
         shell: pwsh
         run: |
@@ -298,7 +300,9 @@ jobs:
             "a_sped_up_video_changes_picture_and_sound", "the_sound_of_a_real_source_is_drawn",
             "shuttle_speed_waits_for_every_clip_to_play_at_normal_speed",
             "a_clip_speed_set_during_a_shuttle_does_not_freeze_the_preview",
-            "a_speed_change_during_playback_leaves_the_preview_playing")
+            "a_speed_change_during_playback_leaves_the_preview_playing",
+            "a_muted_track_is_silent_in_the_export",
+            "muting_a_track_during_playback_leaves_the_preview_playing")
           foreach ($attempt in 1..2) {
             cargo test -p kuvatin-video --release -- --test-threads=1 --exact `
               project::tests::renders_after_preview_eos `
@@ -309,7 +313,9 @@ jobs:
               project::tests::the_sound_of_a_real_source_is_drawn `
               project::tests::shuttle_speed_waits_for_every_clip_to_play_at_normal_speed `
               project::tests::a_clip_speed_set_during_a_shuttle_does_not_freeze_the_preview `
-              project::tests::a_speed_change_during_playback_leaves_the_preview_playing
+              project::tests::a_speed_change_during_playback_leaves_the_preview_playing `
+              project::tests::a_muted_track_is_silent_in_the_export `
+              project::tests::muting_a_track_during_playback_leaves_the_preview_playing
             if ($LASTEXITCODE -eq 0) { break }
             if ($attempt -eq 2) { throw "the live-media regressions failed twice: $($names -join ', ')" }
             Write-Host "::warning::live-media regressions failed once; retrying"
```

- [ ] **Step 2: Check the filter matches**

Run: `cargo test -p kuvatin-video -- --test-threads=1 --list track_mute_`
Expected: the nine `track_mute_` tests of Task 2, and nothing else.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "CI gates on the track mute tests" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 14: Two faults the running build showed

Both were found by using the build, not by any test, and both are fixed here with a test.

- **A mute set before the first clip never reached the engine.** Muting a track before any clip exists works on the rows alone (Task 6: no engine yet). The engine created for the first clip started with no mutes, so that clip was heard under a lit M. Adding a clip, from the bin or as a sequence, now pushes the rows' mutes first; with an engine that already knows, the push changes nothing.
- **A mute straight after a reorder broke the audio composition.** GStreamer reported "The NleComposition structure is not valid [audio_nlecomposition…]" as a playback error. Measured on the fixture, six runs each: a mute alone 0, a reorder alone 0, a reorder then a mute 3, a reorder, then waiting for its commit, then the mute 0. A layer's sound switched while the timeline was still applying the reorder's commit is the fault, the same shape as the speed effect's freeze. `set_track_mutes` now waits, up to two seconds and only when it has something to change and the pipeline is at PAUSED or above, for every track to finish the commits asked so far. With the wait, 0 of 18. The new live-media test fails in its first round without the wait.

**Files:**
- Modify: `crates/kuvatin-video/src/project.rs` (`set_track_mutes`; the test after `muting_a_track_during_playback_leaves_the_preview_playing`)
- Modify: `crates/kuvatin/src/gui/video/mod.rs` (`add_to_timeline`, `add_sequence_to_timeline`)
- Modify: `.github/workflows/release.yml` (the new test in the live-media gate)

- [ ] **Step 1: Apply the diff**

```diff
diff --git a/.github/workflows/release.yml b/.github/workflows/release.yml
index 146cc85..e1e1b62 100644
--- a/.github/workflows/release.yml
+++ b/.github/workflows/release.yml
@@ -287,7 +287,8 @@ jobs:
       # restored with its in-point. One more proves a speed change on real media
       # is two time effects, picture and sound, and one that a real source's
       # sound is drawn as a waveform. Two guard track mutes: a muted track is
-      # silent in the export, and a mute leaves the preview playing. One retry
+      # silent in the export, and a mute leaves the preview playing, even right
+      # after a track reorder. One retry
       # before it counts, because asset-URI resolution on the hosted runner is
       # occasionally flaky — a second failure in a row is the code, not the
       # runner.
@@ -302,7 +303,8 @@ jobs:
             "a_clip_speed_set_during_a_shuttle_does_not_freeze_the_preview",
             "a_speed_change_during_playback_leaves_the_preview_playing",
             "a_muted_track_is_silent_in_the_export",
-            "muting_a_track_during_playback_leaves_the_preview_playing")
+            "muting_a_track_during_playback_leaves_the_preview_playing",
+            "muting_a_track_just_after_a_reorder_keeps_the_audio_valid")
           foreach ($attempt in 1..2) {
             cargo test -p kuvatin-video --release -- --test-threads=1 --exact `
               project::tests::renders_after_preview_eos `
@@ -315,7 +317,8 @@ jobs:
               project::tests::a_clip_speed_set_during_a_shuttle_does_not_freeze_the_preview `
               project::tests::a_speed_change_during_playback_leaves_the_preview_playing `
               project::tests::a_muted_track_is_silent_in_the_export `
-              project::tests::muting_a_track_during_playback_leaves_the_preview_playing
+              project::tests::muting_a_track_during_playback_leaves_the_preview_playing `
+              project::tests::muting_a_track_just_after_a_reorder_keeps_the_audio_valid
             if ($LASTEXITCODE -eq 0) { break }
             if ($attempt -eq 2) { throw "the live-media regressions failed twice: $($names -join ', ')" }
             Write-Host "::warning::live-media regressions failed once; retrying"
diff --git a/crates/kuvatin-video/src/project.rs b/crates/kuvatin-video/src/project.rs
index e12827c..bf29990 100644
--- a/crates/kuvatin-video/src/project.rs
+++ b/crates/kuvatin-video/src/project.rs
@@ -1937,10 +1937,30 @@ impl Project {
             return;
         }
         self.mutes = mutes.to_vec();
+        let wanted = |i: usize| self.mutes.get(i).copied().unwrap_or(false);
+        let needed = self
+            .layers
+            .iter()
+            .enumerate()
+            .any(|(i, layer)| self.is_silent(layer) != wanted(i));
+        if !needed {
+            return;
+        }
+        // A layer's sound switched while the timeline is still applying an
+        // earlier commit (a track reorder, most often) left the audio
+        // composition invalid in three runs of six: "The NleComposition
+        // structure is not valid". Waited for, none of six failed. Below
+        // PAUSED no commit runs, so there is nothing to wait for.
+        if self.pipeline.current_state() >= gst::State::Paused {
+            let asked = self.commits.get();
+            let end = std::time::Instant::now() + Duration::from_secs(2);
+            while self.commits_done() < asked && std::time::Instant::now() < end {
+                std::thread::sleep(Duration::from_millis(5));
+            }
+        }
         let mut changed = false;
         for (i, layer) in self.layers.iter().enumerate() {
-            let muted = self.mutes.get(i).copied().unwrap_or(false);
-            changed |= self.apply_mute(layer, muted);
+            changed |= self.apply_mute(layer, wanted(i));
         }
         if changed {
             self.commit();
@@ -5208,6 +5228,37 @@ mod tests {
         ink as f64 / (400.0 * 200.0)
     }
 
+    /// A mute straight after a track reorder switched a layer's sound while
+    /// the timeline was still applying the reorder, and GStreamer called the
+    /// audio composition invalid in three runs of six. The mute now waits
+    /// for the reorder's commit. Needs `GST_TEST_FILE`, a video with sound.
+    #[test]
+    fn muting_a_track_just_after_a_reorder_keeps_the_audio_valid() {
+        let Some(path) = std::env::var_os("GST_TEST_FILE") else {
+            eprintln!("skipping muting_a_track_just_after_a_reorder_...: set GST_TEST_FILE");
+            return;
+        };
+        for round in 0..6 {
+            let mut project = Project::new(|_f| {}).expect("project");
+            project
+                .append_clip(Path::new(&path), 1, None)
+                .expect("clip");
+            project.play().expect("play");
+            wait_settled(&project);
+            project.move_track(0, 1);
+            project.set_track_mutes(&[true, false]);
+            let end = std::time::Instant::now() + Duration::from_millis(1500);
+            while std::time::Instant::now() < end {
+                project.refresh_preview();
+                if let Some(e) = project.poll_preview_error() {
+                    panic!("round {round}: {e}");
+                }
+                std::thread::sleep(Duration::from_millis(50));
+            }
+            let _ = project.pause();
+        }
+    }
+
     /// A muted track is silent in the export, not only in the preview. The
     /// same clip renders with its sound on an audible track and without it on
     /// a muted one: one muted before the clip's layer existed, and one that
diff --git a/crates/kuvatin/src/gui/video/mod.rs b/crates/kuvatin/src/gui/video/mod.rs
index 9948d45..62841dd 100644
--- a/crates/kuvatin/src/gui/video/mod.rs
+++ b/crates/kuvatin/src/gui/video/mod.rs
@@ -552,6 +552,10 @@ fn add_to_timeline(
     let Some(project) = slot.as_mut() else {
         return;
     };
+    // A track muted before the first clip arrived was muted in the rows
+    // only: the engine did not exist yet. It hears about it now, before the
+    // clip is heard. Nothing to do when it already knows.
+    tracks::push_mutes(project, &rec.tracks);
     let before = rec.before(Some(&*project));
     match project.append_clip(path, track, img_dur) {
         Ok(info) => {
@@ -633,6 +637,10 @@ fn add_sequence_to_timeline(
     let Some(project) = slot.as_mut() else {
         return;
     };
+    // A track muted before the first clip arrived was muted in the rows
+    // only: the engine did not exist yet. It hears about it now, before the
+    // clip is heard. Nothing to do when it already knows.
+    tracks::push_mutes(project, &rec.tracks);
     let before = rec.before(Some(&*project));
     let added = spec
         .uri()
```

- [ ] **Step 2: Run the live-media tests and the app tests**

Run: `cargo test -p kuvatin-video -- --test-threads=1 muting_a_track a_muted_track_is_silent track_mute_`
Expected: 12 passed with `GST_TEST_FILE` set.
Run: `cargo test -p kuvatin`
Expected: 303 passed, 2 ignored.

- [ ] **Step 3: Gates and commit**

```bash
git add crates/kuvatin-video/src/project.rs crates/kuvatin/src/gui/video/mod.rs .github/workflows/release.yml
git commit -m "A mute waits for the commit before it, and reaches an engine made after it" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 15: Changelog, README, the hand checks, and the full gates

**Files:**
- Modify: `CHANGELOG.md` (under `## [Unreleased]`)
- Modify: `README.md` (the Videos-mode keys paragraph and the "Layered timeline editor" bullet)

- [ ] **Step 1: The changelog**

Under `## [Unreleased]`, add an `### Added` section (create it if the section is empty) with:

```markdown
- **Track controls.** Each track's header has M, S and L buttons. **Mute**
  silences the track's sound in the preview and in the export; its pictures
  still show. **Solo** plays only the soloed tracks while you listen, and
  switches itself off when an export starts, so a forgotten solo cannot
  silence the file. **Lock** stops any change to the clips on the track, by
  mouse or keyboard, until it is unlocked. Double-click a track's name to
  rename it. Names, mutes and locks are undoable and saved with the project;
  solo is neither. A project with named, muted or locked tracks opens in 2.13
  and earlier with its clips as they were and every track unnamed, audible and
  unlocked.
```

- [ ] **Step 2: The README**

In the Videos-mode keys paragraph, after "…and **Delete** removes it.", add: "Double-click a track's name to rename it; the M, S and L buttons beside it mute, solo and lock the track." In the "Layered timeline editor" bullet, after "reorder tracks;", add "name, mute, solo and lock them;".

- [ ] **Step 3: The full gates**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p kuvatin-core -p kuvatin
cargo test -p kuvatin-video -- --test-threads=1
```

Expected: fmt and clippy clean; `kuvatin-core` 122 passed, `kuvatin` 303 passed; with `GST_TEST_FILE` and `GST_TEST_IMAGE` set, `kuvatin-video` 126 passed, 1 ignored.

- [ ] **Step 4: The hand checks**

Run `cargo run -p kuvatin`, Videos mode. Checked on the verified build:

1. Mute a track with no clip on the timeline: M lights, the name dims, Undo reads "Undo muting Track 2".
2. Double-click a name, type, Return: renamed; Ctrl+Z and Ctrl+Y take it back and forth.
3. Add a video, lock its track: the clip selects without a ×, the inspector is greyed and says why, Split is grey, a drag does not move it, and Delete opens the refusal and deletes nothing. (The dialog's wording was hidden behind another window during the check; `an_edit_is_refused_for_the_first_locked_track_it_touches` pins it.)
4. Drag a locked track's header: nothing. Drag another header past it: the rows swap, the name going with its row, and Undo reads "Undo reordering tracks". Undoing that reorder is where the second fault of Task 14 showed; after the fix it is covered by `muting_a_track_just_after_a_reorder_keeps_the_audio_valid` and has not been checked by hand again.

For Ville, because they need ears, a key computer use cannot send, or a text field it cannot focus reliably:

5. Mute a track and hear the preview go quiet; export and hear the file go quiet.
6. Solo one track, start an export, and watch the S buttons go off before the render starts.
7. Esc in the rename field puts the old name back; Ctrl+Z inside it undoes the typing, not the timeline.
8. Rename a track, make an unrelated edit, Ctrl+Z: the name is still there. Rename, save, open: still there; open the same file in 2.13: it opens with the names gone and nothing else wrong.
9. With the Speed list or the Duration field of a clip on a locked track: greyed; with the keyboard (Ctrl+arrows, Shift+arrows, S) on a selected locked clip: refused with the message.

- [ ] **Step 5: Commit**

```bash
git add CHANGELOG.md README.md
git commit -m "The changelog and README describe the track controls" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Risks

- **Positional state is only as good as its permutations.** `move_track` permutes the engine's vector, `layer()` applies it, the reorder moves the row, and `set_track_rows` rewrites the rows from a table. A fifth place that reorders layers inherits the obligation. The engine tests pin the first two; `push_mutes` after every row change repairs the engine from the rows.
- **Older builds drop the table.** 2.13 opens a project saved by this change with its tracks unnamed, audible and unlocked; the one that stings is a track muted on purpose playing there. Accepted over format version 2, which would refuse the file outright.
- **Undo can change a locked track,** by design, and a locked track can be shifted by another track's reorder. Whether either surprises anyone is unknown until it is used.
- **The mute's commit wait blocks the interface thread** for as long as the commit before it takes, about a tenth of a second after a reorder, bounded at two seconds. Measured only on this machine.
- **20 px buttons in a 30 px row are a small target.** If they prove fiddly the next move is a taller row, which re-tunes the whole timeline band; do not reach for it without the complaint.
- **The locked clip's disabled drag, and the rename field's key routing, live only in `app.slint`** and are checked by hand.
