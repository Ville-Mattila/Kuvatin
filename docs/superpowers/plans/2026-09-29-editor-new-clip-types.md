# Editor New Clip Types Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Clips on one track may overlap, and GES cross-dissolves wherever they do; a Dissolve chip makes or removes a one-second dissolve. A text overlay is a new kind of clip: added with a Text chip, typed and styled in the inspector, moved, scaled, faded, trimmed, undone, saved and reopened like any other clip.

**Architecture:** `ClipRecord` gains one optional `body` (the spec's guiding decision 1); `None` is a media clip, `Some(ClipBody::Title(..))` a GES `TitleClip`. `clip_records`, `apply_document`, `restore_clip` and `set_clip_records` learn to build and read a title, so files, undo and the diff all see it. A file is stamped with the oldest format that can read it: 1 without titles, 2 with. Overlap stops being forbidden and becomes bounded by what GES refuses (no clip wholly over another, no three at one instant), for slides and outward trims alike. GES's own auto-transitions draw the dissolve, so a dissolve is never a record, a step or a line in a file. The interface draws a band over each overlap and keeps the Dissolve chip's state from the 100 ms UI tick.

**Tech Stack:** Rust, Slint 1.16, GStreamer 1.26 with GES through `gstreamer-editing-services` 0.23, TOML project files.

**Spec:** `docs/superpowers/specs/2026-09-16-editor-new-clip-types-design.md`. Read it before Task 1. It was written before the clip edits and the track controls merged, so its line numbers are out of date; this plan cites only current code, and "Where this plan departs from the spec" below says what changed and why.

**Verified before review:** every diff below was applied, in task order, to an export of `master` at `f0d821d`, and run on GStreamer 1.26.11. After each task `cargo fmt --all --check` was clean, `cargo clippy --workspace --all-targets -D warnings` passed, `cargo test -p kuvatin` passed, and so did the video tests the task names; the counts in each task are the ones that run produced. The diffs were then applied again with `git apply` to a fresh worktree of `master`, with the same gates after every task, and the result was identical, file for file, to the verified export. The whole workspace suite passed at the end (315 app tests, 151 engine tests, with the live-media fixture). The build was also run by hand (Task 9 says what was and was not checked), which is how the two faults folded into Tasks 2 and 8 were found.

---

## The undo design is amended

`docs/superpowers/specs/2026-09-13-undo-design.md` asks, in its "Contract for later edits", that a new step kind be written into it before it is built. The amendment landed in the same commit as this plan: **Text** is a step kind, it merges like Rename track, it describes itself as "editing the text of …", the Text chip records an Add and the title timer a Text step, the Dissolve chip is a Move through the drop callback, and undo never checks a title's source. Nothing in this plan edits that design again.

## Where this plan departs from the spec

1. **A title's background is its `foreground-color`, and it had to be cleared.** The spec planned to set a `background` child property with alpha 0. There is no such property. GES draws a title over a `videotestsrc` filled with `foreground-color`, opaque white by default, which hid everything beneath the title. `write_title` sets it to 0 (Task 2). The spec's proposed measurement, a white still under the title, cannot see this, and the first measurement for this plan was fooled by it; the running build showed it. The test puts a **blue** still under the title and reads the corner.
2. **Colours are ARGB, alignments are written by nick.** `color` takes alpha in the top byte (`0xffff0000` is red), so `parse_color` maps `#rrggbbaa` to `aarrggbb`. `halignment` and `valignment` are `textoverlay`'s own enums, not GES's `TextHAlign`, so they are set and read through their nicks (`set_enum_child`, `enum_child_nick`).
3. **Test names carry `title_` or `dissolve_`.** The release gate matches tests by substring, so every new engine test is named to fall under the two substrings Task 9 adds: `clip_records_does_not_drop_a_title` is `title_is_not_dropped_by_clip_records`, `a_title_is_never_a_missing_source` is `title_is_never_a_missing_source`, `an_empty_layer_holding_a_transition_is_still_pruned` is `dissolve_leaves_no_dead_track_when_its_clips_go`, and so on. The rewritten slide tests keep `slid`, `neighbour`, `confined_to_the_gap` and `already_overlapping`, and the trim-bound tests use `neighbour`.
4. **The dissolve bands and the chip's state come from the UI tick.** The spec recomputed the overlaps beside each of the eleven `set_timeline_duration` calls, several of which run before the rows they would read are updated, and missed the track reorder. `show_dissolves` runs on the existing 100 ms tick instead, compares with what is shown, and only touches the window when something changed (Task 8). No edit, undo or reopen can leave a stale band.
5. **A clip with no legal place stays where it was.** The spec's clamp returns the floor when `ceil < floor`, which can itself be illegal. That only happens for a clip already in a tangle, and there the slide now leaves it at its start (Task 3).
6. **Only outward trims are bounded.** Trimming in is never refused, so a clip in an old tangle can always be shrunk out of it (Task 3).
7. **A speed change still stops at the next clip.** `set_clip_rate` keeps `room_after`: a slower clip grows up to its neighbour and does not make a dissolve by itself. The spec predates clip speed and says nothing about it.
8. **A title's row follows its text.** A title's record is named from its text, so `place` in `undo.rs` updates a title row's name (a media clip's row keeps its own), and the title timer renames the row and the inspector heading (Tasks 5 and 6).
9. **The font and alignment helpers live in a new `titles.rs`**, with the Text chip and the title controls' wiring (Tasks 6 and 7), rather than in the already long `timeline.rs`.
10. **Wide timeline chips grow with their label.** "Remove dissolve" overflowed the fixed 34 px chip and covered Split in the running build; a wide chip is now at least 34 px and otherwise as wide as its label (Task 8).
11. **Small things the spec named or implied:** the `Project` doc comment now says layer 0 is on top (Task 3); `record_of` reports a clip kind it cannot describe once per process instead of dropping it silently (Task 2); the Text chip is disabled, with a hint saying why, while the top track is locked, and the handler refuses too (Task 7); thumbnails and waveforms skip titles by their empty URI (Task 5); the README's "Deferred" line no longer lists undo, which shipped in 2.12 (Task 9).

## Before you start

- **Work in a worktree of its own on branch `new-clip-types`** (superpowers:using-git-worktrees). Do not commit to `master`, and do not touch the main checkout at `C:\Työt\Koodaus\Kuvatin`. Run every command from the worktree root.
- **GStreamer must be on PATH for every cargo command** (Git Bash):
  `export PATH="/c/Program Files/gstreamer/1.0/msvc_x86_64/bin:$PATH"`.
- **The video tests run one at a time.** Every command that runs `kuvatin-video` tests passes `-- --test-threads=1`. Concurrent GES pipelines deadlock, and the failures that produces are not real.
- **Applying a task.** Each task's change is one diff, verified in order. Save it to a file outside the repository and apply it with `git apply --whitespace=nowarn <file>` from the worktree root, or make the same edits by hand with the Edit tool. Do not paste a diff through a heredoc: the Bash tool loses one level of backslash there, and the diffs hold `\n`, `\\` and `\u{…}` escapes. If `git apply` refuses a hunk, an earlier task was not applied exactly; find out why rather than forcing it. Each diff adds its tests and the code they test together; to see a test fail first, apply the diff, then revert the non-test hunks by hand, which is how the dissolve tests were checked (four of six fail with auto-transitions off).
- **`kuvatin` is a binary crate.** A `pub(super)` item with no caller fails `clippy -D warnings`. Every helper here lands in the task that first calls it, so no task needs `#[allow(dead_code)]`.
- **Every new engine test is in the release gate** by the end of Task 9, through the `title_` and `dissolve_` substrings added to `Test (video engine, self-contained — gates the release)` in `.github/workflows/release.yml`. None needs real media.
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
  plus, for a task that names video tests, those tests with `-- --test-threads=1`.
- **The live-media fixture** is only needed for the full suite at the end (Task 9), the same one the track controls used:
  ```bash
  F="$(cygpath -m "$LOCALAPPDATA")/Temp/kuvatin-fixtures"
  mkdir -p "$F"
  gst-launch-1.0 -e videotestsrc num-buffers=150 ! "video/x-raw,width=320,height=180" ! vp8enc ! webmmux name=m ! filesink location="$F/fixture_av.webm" audiotestsrc num-buffers=300 ! audioconvert ! vorbisenc ! m.
  gst-launch-1.0 videotestsrc num-buffers=1 ! "video/x-raw,width=320,height=180" ! pngenc ! filesink location="$F/fixture.png"
  export GST_TEST_FILE="$F/fixture_av.webm" GST_TEST_IMAGE="$F/fixture.png"
  ```

## Files

| File | What changes |
| --- | --- |
| `crates/kuvatin-video/src/document.rs` | `ClipBody`, `TitleRecord`, `TitleHAlign`, `TitleVAlign`, `parse_color`, `format_color`, `required_version`; `ClipRecord::body`; `FORMAT_VERSION` 2; `ProjectFile::new` stamps the required version |
| `crates/kuvatin-video/src/lib.rs` | re-exports the title types and the colour helpers |
| `crates/kuvatin-video/src/project.rs` | title clips (`add_title_clip`, `append_title_clip`, `set_title`, `title_of`, `record_of`, `write_title`); `layer_is_empty`; the overlap rule (`slide_within_layer`, `trim_bounds`); auto-transitions; engine tests |
| `crates/kuvatin/src/gui/video/titles.rs` (new) | font and alignment helpers, the row rename, the Text chip and the title controls' wiring |
| `crates/kuvatin/src/gui/video/undo.rs` | `StepKind::Text`; a title's kind and name in restored rows; `missing_sources` skips titles |
| `crates/kuvatin/src/gui/video/project_file.rs` | `kind_of` takes a record; `kind_of_uri`; thumbnails skip titles |
| `crates/kuvatin/src/gui/video/waves.rs` | titles get no waveform |
| `crates/kuvatin/src/gui/video/mod.rs` | `pending_title`, applied and recorded on the tick; `show_dissolves` on the tick |
| `crates/kuvatin/src/gui/video/timeline.rs` | the inspector knows a title; `overlaps`, `dissolve_slide`, `show_dissolves`, the Dissolve chip; the snap comment |
| `crates/kuvatin/ui/app.slint` | `ClipKind.title` in amber; the title controls; `insp-free-duration`; the Text and Dissolve chips; `TimelineOverlap` bands |
| `crates/kuvatin/ui/widgets.slint` | a wide `TimelineChip` grows with its label |
| `.github/workflows/release.yml`, `CHANGELOG.md`, `README.md` | the gate, and the words |

---

### Task 1: The project file can hold a title

A record says what kind of clip it is with an optional `body`; a title's text, font, colour and alignment are its own table. A file of media clips alone stays format 1; one title makes it format 2, which 2.13 and earlier refuse by name.

**Files:**
- Modify: `crates/kuvatin-video/src/document.rs` (types, helpers, tests)
- Modify: `crates/kuvatin-video/src/project.rs:2175`, `crates/kuvatin/src/gui/video/undo.rs:803` (`body: None` in the two existing `ClipRecord` literals)

- [ ] **Step 1: Apply the diff.**

````diff
diff --git a/crates/kuvatin-video/src/document.rs b/crates/kuvatin-video/src/document.rs
index 28e12a2..6716cd3 100644
--- a/crates/kuvatin-video/src/document.rs
+++ b/crates/kuvatin-video/src/document.rs
@@ -14,10 +14,14 @@ use gstreamer as gst;
 use serde::{Deserialize, Serialize};
 use std::path::Path;
 
-/// What this version of Kuvatin writes. A file from a LATER version is
-/// refused rather than half-understood: a project silently missing the clips
-/// it could not parse is worse than a project that will not open.
-pub const FORMAT_VERSION: u32 = 1;
+/// The newest format this version of Kuvatin reads. A file from a LATER
+/// version is refused rather than half-understood: a project silently missing
+/// the clips it could not parse is worse than a project that will not open.
+///
+/// A file is stamped with the OLDEST format that can read it
+/// ([`required_version`]), not with this: a project of media clips alone is
+/// still format 1, and opens in every build ever shipped.
+pub const FORMAT_VERSION: u32 = 2;
 
 /// A clip's transform, as stored. Mirrors [`crate::Layout`], which is not
 /// serialisable on purpose — the engine type can change shape without changing
@@ -80,6 +84,139 @@ pub struct ClipRecord {
     /// it afterwards, which the URI alone cannot describe.
     #[serde(default, skip_serializing_if = "Option::is_none")]
     pub sequence: Option<crate::sequence::SequenceSpec>,
+    /// What kind of clip this record describes, when it is not a clip on a
+    /// media source. Absent, which is every record written before this
+    /// existed, means a URI clip, and `uri` is the whole of its identity.
+    #[serde(default, skip_serializing_if = "Option::is_none")]
+    pub body: Option<ClipBody>,
+}
+
+/// What a record describes, when it is not a clip on a media source. One
+/// variant for now: the tag is what costs something to add later, so
+/// `[clips.body.title]` today makes another kind of clip a variant tomorrow,
+/// with no change to how a file is read.
+#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
+#[serde(rename_all = "snake_case")]
+pub enum ClipBody {
+    /// A text overlay, built as a GES `TitleClip`.
+    Title(TitleRecord),
+}
+
+/// A text overlay's own state: what a title needs beyond the place, times
+/// and transform every clip has.
+#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
+pub struct TitleRecord {
+    /// What it says. May be empty and may hold newlines: an empty title is
+    /// still a clip you can see and select, which a half-typed one has to be.
+    pub text: String,
+    /// A Pango font description, "Sans Bold 48". Written whole, so a later
+    /// version can offer more than the interface does now without changing
+    /// the file.
+    #[serde(default = "default_font")]
+    pub font: String,
+    /// The text colour, `#rrggbb` or `#rrggbbaa`: a string, because a project
+    /// file is a document someone may read, the reason times are seconds.
+    #[serde(default = "default_text_color")]
+    pub color: String,
+    #[serde(default)]
+    pub halign: TitleHAlign,
+    #[serde(default)]
+    pub valign: TitleVAlign,
+}
+
+/// Where a title's lines sit across the frame.
+#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
+#[serde(rename_all = "snake_case")]
+pub enum TitleHAlign {
+    Left,
+    #[default]
+    Center,
+    Right,
+}
+
+/// Where a title sits down the frame.
+#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
+#[serde(rename_all = "snake_case")]
+pub enum TitleVAlign {
+    Top,
+    #[default]
+    Center,
+    Bottom,
+}
+
+fn default_font() -> String {
+    "Sans Bold 48".into()
+}
+
+fn default_text_color() -> String {
+    "#ffffff".into()
+}
+
+impl Default for TitleRecord {
+    /// What "Add text" puts on the timeline.
+    fn default() -> Self {
+        TitleRecord {
+            text: "Text".into(),
+            font: default_font(),
+            color: default_text_color(),
+            halign: TitleHAlign::Center,
+            valign: TitleVAlign::Center,
+        }
+    }
+}
+
+impl TitleRecord {
+    /// The name a title shows on the timeline: its first line, cut to 24
+    /// characters, or "Text" when it has none.
+    pub fn name(&self) -> String {
+        let first = self.text.lines().next().unwrap_or("").trim();
+        if first.is_empty() {
+            return "Text".into();
+        }
+        let mut name: String = first.chars().take(24).collect();
+        if first.chars().count() > 24 {
+            name.push('…');
+        }
+        name
+    }
+}
+
+/// `#rrggbb` or `#rrggbbaa` as the value GES takes for a title's colour:
+/// ARGB, alpha in the top byte (measured: `0xffff0000` draws red). None for
+/// anything else.
+pub fn parse_color(s: &str) -> Option<u32> {
+    let hex = s.strip_prefix('#')?;
+    if !(hex.len() == 6 || hex.len() == 8) || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
+        return None;
+    }
+    let v = u32::from_str_radix(hex, 16).ok()?;
+    Some(if hex.len() == 6 {
+        0xff00_0000 | v
+    } else {
+        // #rrggbbaa → aarrggbb
+        (v >> 8) | ((v & 0xff) << 24)
+    })
+}
+
+/// The inverse of [`parse_color`]: `#rrggbb`, or `#rrggbbaa` when the colour
+/// is not fully opaque.
+pub fn format_color(v: u32) -> String {
+    let (a, rgb) = (v >> 24, v & 0x00ff_ffff);
+    if a == 0xff {
+        format!("#{rgb:06x}")
+    } else {
+        format!("#{rgb:06x}{a:02x}")
+    }
+}
+
+/// The format a document needs: 2 once any clip is something a format-1
+/// reader has no shape for (a title), 1 otherwise.
+pub fn required_version(clips: &[ClipRecord]) -> u32 {
+    if clips.iter().any(|c| c.body.is_some()) {
+        2
+    } else {
+        1
+    }
 }
 
 fn unit_rate() -> f64 {
@@ -147,7 +284,7 @@ pub struct ProjectFile {
 impl ProjectFile {
     pub fn new(canvas_w: i32, canvas_h: i32, clips: Vec<ClipRecord>) -> Self {
         ProjectFile {
-            version: FORMAT_VERSION,
+            version: required_version(&clips),
             canvas_w,
             canvas_h,
             clips,
@@ -219,6 +356,7 @@ mod tests {
                     rate: 1.0,
                     layout: layout(),
                     sequence: None,
+                    body: None,
                 },
                 ClipRecord {
                     uri: "imagesequence://C:/render/frame_%04d.png?framerate=24/1".into(),
@@ -238,6 +376,7 @@ mod tests {
                         count: 48,
                         fps: 24,
                     }),
+                    body: None,
                 },
             ],
         )
@@ -411,4 +550,150 @@ volume = 1.0
         );
         assert!(text.contains("version = 1"), "{text}");
     }
+
+    fn title(text: &str) -> ClipRecord {
+        ClipRecord {
+            uri: String::new(),
+            name: String::new(),
+            track: 0,
+            start: 1.0,
+            inpoint: 0.0,
+            duration: 5.0,
+            rate: 1.0,
+            layout: layout(),
+            sequence: None,
+            body: Some(ClipBody::Title(TitleRecord {
+                text: text.into(),
+                font: "Serif 72".into(),
+                color: "#ffcc00".into(),
+                halign: TitleHAlign::Left,
+                valign: TitleVAlign::Bottom,
+            })),
+        }
+    }
+
+    /// Opaque colours read as ARGB with the alpha byte full; `#rrggbbaa`
+    /// moves its alpha to the top, where GES looks for it.
+    #[test]
+    fn title_colours_parse_to_argb() {
+        assert_eq!(parse_color("#ff0000"), Some(0xffff_0000));
+        assert_eq!(parse_color("#FFCC00"), Some(0xffff_cc00));
+        assert_eq!(parse_color("#11223380"), Some(0x8011_2233));
+        for bad in ["", "ff0000", "#ff00", "#ff00001", "#gg0000", "#ff0000ff00"] {
+            assert_eq!(parse_color(bad), None, "{bad}");
+        }
+    }
+
+    #[test]
+    fn title_colours_format_back_to_what_they_parsed_from() {
+        for s in ["#ffffff", "#000000", "#ffcc00", "#11223380", "#00000000"] {
+            assert_eq!(format_color(parse_color(s).unwrap()), s);
+        }
+    }
+
+    #[test]
+    fn title_names_are_their_first_line_cut_short() {
+        let named = |text: &str| {
+            TitleRecord {
+                text: text.into(),
+                ..TitleRecord::default()
+            }
+            .name()
+        };
+        assert_eq!(
+            named(
+                "Opening
+second line"
+            ),
+            "Opening"
+        );
+        assert_eq!(named("   "), "Text");
+        assert_eq!(named(""), "Text");
+        assert_eq!(
+            named("A title far longer than twenty-four characters"),
+            "A title far longer than …"
+        );
+    }
+
+    /// A file of media clips alone stays format 1, so every build ever
+    /// shipped still opens it; one title makes it format 2, so a build that
+    /// cannot draw titles refuses it instead of dropping them.
+    #[test]
+    fn title_clips_raise_the_format_and_media_clips_do_not() {
+        assert_eq!(sample().version, 1);
+        let mut clips = sample().clips;
+        clips.push(title("Hello"));
+        assert_eq!(required_version(&clips), 2);
+        assert_eq!(ProjectFile::new(1920, 1080, clips).version, 2);
+    }
+
+    #[test]
+    fn title_clips_survive_the_round_trip_with_their_newlines() {
+        let dir = tempfile::tempdir().unwrap();
+        let path = dir.path().join("titled.kuvatin");
+        let mut clips = sample().clips;
+        clips.push(title(
+            "First line
+\"quoted\" second",
+        ));
+        let doc = ProjectFile::new(1920, 1080, clips);
+        doc.save(&path).unwrap();
+        assert_eq!(ProjectFile::load(&path).unwrap(), doc);
+        let text = std::fs::read_to_string(&path).unwrap();
+        assert!(text.contains("[clips.body.title]"), "{text}");
+        assert!(text.contains("version = 2"), "{text}");
+        assert_eq!(
+            text.matches("[clips.body").count(),
+            1,
+            "a media clip writes no body: {text}"
+        );
+    }
+
+    /// A title written with only its text gets the defaults for the rest.
+    #[test]
+    fn title_fields_left_out_take_their_defaults() {
+        let text = r#"version = 2
+canvas_w = 1280
+canvas_h = 720
+
+[[clips]]
+uri = ""
+track = 0
+start = 0.0
+inpoint = 0.0
+duration = 5.0
+
+[clips.layout]
+posx = 0
+posy = 0
+scale = 1.0
+alpha = 1.0
+volume = 1.0
+
+[clips.body.title]
+text = "Hi"
+"#;
+        let doc: ProjectFile = toml::from_str(text).unwrap();
+        let Some(ClipBody::Title(t)) = &doc.clips[0].body else {
+            panic!("a title: {:?}", doc.clips[0].body);
+        };
+        assert_eq!(
+            t,
+            &TitleRecord {
+                text: "Hi".into(),
+                ..TitleRecord::default()
+            }
+        );
+    }
+
+    #[test]
+    fn title_era_builds_refuse_a_format_three_file() {
+        let dir = tempfile::tempdir().unwrap();
+        let path = dir.path().join("three.kuvatin");
+        let mut doc = sample();
+        doc.version = 3;
+        doc.save(&path).unwrap();
+        let err = ProjectFile::load(&path).unwrap_err().to_string();
+        assert!(err.contains("format 3, this build reads 2"), "{err}");
+    }
 }
diff --git a/crates/kuvatin-video/src/project.rs b/crates/kuvatin-video/src/project.rs
index bf29990..239127e 100644
--- a/crates/kuvatin-video/src/project.rs
+++ b/crates/kuvatin-video/src/project.rs
@@ -2173,6 +2173,7 @@ impl Project {
                         // Filled in by the caller, which is the only side that
                         // knows how a sequence clip was described when it arrived.
                         sequence: None,
+                        body: None,
                     },
                 ))
             })
diff --git a/crates/kuvatin/src/gui/video/undo.rs b/crates/kuvatin/src/gui/video/undo.rs
index 2bfe5d6..411d072 100644
--- a/crates/kuvatin/src/gui/video/undo.rs
+++ b/crates/kuvatin/src/gui/video/undo.rs
@@ -801,6 +801,7 @@ mod tests {
                 volume: 1.0,
             },
             sequence: None,
+            body: None,
         }
     }
 
````

- [ ] **Step 2: Run the gates.** Expected: `cargo test -p kuvatin` 303 passed; `cargo test -p kuvatin-video --lib -- --test-threads=1 document::` 18 passed, among them the seven `title_` tests.

- [ ] **Step 3: Commit.** `A project file can hold a title, and says which format it needs`

### Task 2: The engine builds, reads back and restores title clips

`clip_records` stops dropping every clip that is not a `UriClip`: the body moves to `record_of`, which describes a title through `title_of`. `add_title_clip` and `append_title_clip` build one; `set_title` rewrites it; `apply_document` and `restore_clip` build a title from its record and never look for a source; `set_clip_records` writes a title's text for every clip that landed, without parking it for that. Three places that decided a layer was empty by asking GES now ask `layer_is_empty`, which only counts our clips, because Task 4 puts GES transitions on layers.

`write_title` clears `foreground-color` first. Without it the title is drawn on opaque white (departure 1).

**Files:**
- Modify: `crates/kuvatin-video/src/project.rs` (helpers after `clip_placed_as`; `record_of`, `title_of`, `add_title_clip`, `append_title_clip`, `set_title`, `layer_is_empty` before `apply_document`; `apply_document`; `restore_clip`; `set_clip_records`; `prune_tracks`; `remove_clip`; tests at the end of the module)

- [ ] **Step 1: Apply the diff.**

````diff
diff --git a/crates/kuvatin-video/src/project.rs b/crates/kuvatin-video/src/project.rs
index 239127e..f44554e 100644
--- a/crates/kuvatin-video/src/project.rs
+++ b/crates/kuvatin-video/src/project.rs
@@ -637,6 +637,63 @@ fn clip_placed_as(clip: &ges::Clip, record: &crate::document::ClipRecord) -> boo
         && clip_rate_of(clip) == record.rate
 }
 
+/// The `textoverlay` nick for a title's horizontal alignment. The child
+/// property is textoverlay's own enum, not GES's `TextHAlign`, so it is
+/// written and read by nick.
+fn halign_nick(a: crate::document::TitleHAlign) -> &'static str {
+    use crate::document::TitleHAlign::*;
+    match a {
+        Left => "left",
+        Center => "center",
+        Right => "right",
+    }
+}
+
+fn valign_nick(a: crate::document::TitleVAlign) -> &'static str {
+    use crate::document::TitleVAlign::*;
+    match a {
+        Top => "top",
+        Center => "center",
+        Bottom => "bottom",
+    }
+}
+
+/// Set an enum child property by its nick. Nothing happens for a property
+/// the clip does not have or a nick its enum does not know.
+fn set_enum_child(clip: &ges::Clip, name: &str, nick: &str) {
+    let Some((_, pspec)) = clip.lookup_child(name) else {
+        return;
+    };
+    let value = gst::glib::EnumClass::with_type(pspec.value_type())
+        .and_then(|class| class.to_value_by_nick(nick));
+    if let Some(value) = value {
+        let _ = clip.set_child_property(name, &value);
+    }
+}
+
+/// The nick of an enum child property's current value.
+fn enum_child_nick(clip: &ges::Clip, name: &str) -> Option<String> {
+    let value = clip.child_property(name)?;
+    let (_, v) = gst::glib::EnumValue::from_value(&value)?;
+    Some(v.nick().to_string())
+}
+
+/// Write a title's text and styling onto a `TitleClip`'s text overlay, and
+/// make the frame behind the text transparent. GES draws a title over a
+/// `videotestsrc` filled with its `foreground-color`, opaque white unless
+/// told otherwise, which hid everything beneath the title (measured: a blue
+/// still read back white). There is no `background` child property.
+fn write_title(clip: &ges::Clip, title: &crate::document::TitleRecord) {
+    let _ = clip.set_child_property("foreground-color", &0u32.to_value());
+    let _ = clip.set_child_property("text", &title.text.to_value());
+    let _ = clip.set_child_property("font-desc", &title.font.to_value());
+    if let Some(color) = crate::document::parse_color(&title.color) {
+        let _ = clip.set_child_property("color", &color.to_value());
+    }
+    set_enum_child(clip, "halignment", halign_nick(title.halign));
+    set_enum_child(clip, "valignment", valign_nick(title.valign));
+}
+
 /// A clip's time effects: its speed change, when it has one.
 fn time_effects(clip: &ges::Clip) -> Vec<ges::BaseEffect> {
     clip.top_effects()
@@ -1794,16 +1851,21 @@ impl Project {
         }
         // Empty again, unless a clip could not go back to its place.
         while self.layers.len() > parking {
-            if self.layers.last().is_some_and(|l| !l.clips().is_empty()) {
+            if self.layers.last().is_some_and(|l| !self.layer_is_empty(l)) {
                 break;
             }
             if let Some(last) = self.layers.pop() {
                 let _ = self.timeline.remove_layer(&last);
             }
         }
-        // A refused clip is back as it was, transform included.
-        for (id, record, _) in found.iter().filter(|(id, _, _)| !failed.contains(*id)) {
+        // A refused clip is back as it was, transform and text included.
+        // Text is written whether or not the clip moved: parking decides on
+        // geometry alone, and undoing a typed word moves nothing.
+        for (id, record, clip) in found.iter().filter(|(id, _, _)| !failed.contains(*id)) {
             self.set_clip_layout(id, record.layout.into());
+            if let Some(crate::document::ClipBody::Title(title)) = &record.body {
+                write_title(clip, title);
+            }
         }
         if !found.is_empty() {
             self.commit();
@@ -1833,7 +1895,15 @@ impl Project {
         if self.clips.contains_key(&id.0) {
             anyhow::bail!("clip {} is already on the timeline", id.0);
         }
-        let clip = ges::UriClip::new(&record.uri)?;
+        let title = record.body.as_ref().map(|body| match body {
+            crate::document::ClipBody::Title(title) => title,
+        });
+        let clip: ges::Clip = match title {
+            Some(_) => ges::TitleClip::new()
+                .ok_or_else(|| anyhow::anyhow!("GES could not make a title clip"))?
+                .upcast(),
+            None => ges::UriClip::new(&record.uri)?.upcast(),
+        };
         clip.set_start(clock_time(record.start));
         clip.set_inpoint(clock_time(record.inpoint));
         // Slower than normal, a clip can be longer than its source lasts at
@@ -1847,8 +1917,11 @@ impl Project {
         }
         clip.set_duration(length);
         self.layer(record.track).add_clip(&clip)?;
+        if let Some(title) = title {
+            write_title(&clip, title);
+        }
         self.commit();
-        self.clips.insert(id.0.clone(), clip.upcast());
+        self.clips.insert(id.0.clone(), clip);
         self.set_clip_layout(id, record.layout.into());
         if record.rate != 1.0 {
             let was = self.before_time_effects();
@@ -1903,7 +1976,7 @@ impl Project {
         }
         let mut changed = false;
         while self.layers.len() > keep.max(1) {
-            if !self.layers[self.layers.len() - 1].clips().is_empty() {
+            if !self.layer_is_empty(&self.layers[self.layers.len() - 1]) {
                 break;
             }
             if let Some(last) = self.layers.pop() {
@@ -2058,7 +2131,7 @@ impl Project {
         // don't accumulate dead rows forever.
         while self.layers.len() > 1 {
             let last = self.layers.last().unwrap();
-            if !last.clips().is_empty() {
+            if !self.layer_is_empty(last) {
                 break;
             }
             let last = self.layers.pop().unwrap();
@@ -2144,38 +2217,9 @@ impl Project {
             .clips
             .iter()
             .filter_map(|(name, clip)| {
-                let uri = clip.downcast_ref::<ges::UriClip>()?.uri().to_string();
-                let secs = |t: gst::ClockTime| t.nseconds() as f64 / 1e9;
-                Some((
-                    ClipId(name.clone()),
-                    crate::document::ClipRecord {
-                        name: uri
-                            .rsplit(['/', '\\'])
-                            .next()
-                            .map(|s| s.split('?').next().unwrap_or(s).to_string())
-                            .unwrap_or_default(),
-                        uri,
-                        track: clip.layer().map(|l| l.priority() as usize).unwrap_or(0),
-                        start: secs(clip.start()),
-                        inpoint: secs(clip.inpoint()),
-                        duration: secs(clip.duration()),
-                        rate: clip_rate_of(clip),
-                        layout: self
-                            .clip_layout(&ClipId(name.clone()))
-                            .map(Into::into)
-                            .unwrap_or(crate::document::LayoutRecord {
-                                posx: 0,
-                                posy: 0,
-                                scale: 1.0,
-                                alpha: 1.0,
-                                volume: 1.0,
-                            }),
-                        // Filled in by the caller, which is the only side that
-                        // knows how a sequence clip was described when it arrived.
-                        sequence: None,
-                        body: None,
-                    },
-                ))
+                let id = ClipId(name.clone());
+                let record = self.record_of(&id, clip)?;
+                Some((id, record))
             })
             .collect();
         clips.sort_by(|(_, a), (_, b)| {
@@ -2187,6 +2231,175 @@ impl Project {
         clips
     }
 
+    /// One clip as a record, or None for a clip of a kind this engine never
+    /// puts on the timeline itself. Only the engine's own builders write
+    /// `self.clips`, and a transition GES inserts over an overlap is never
+    /// among them, so nothing reaches the last arm today: it says so once
+    /// rather than dropping a clip in silence, so a kind added later that
+    /// forgets this method is noisy instead of lossy.
+    fn record_of(&self, id: &ClipId, clip: &ges::Clip) -> Option<crate::document::ClipRecord> {
+        use crate::document::ClipBody;
+        let (uri, name, body) = if let Some(uri_clip) = clip.downcast_ref::<ges::UriClip>() {
+            let uri = uri_clip.uri().to_string();
+            let name = uri
+                .rsplit(['/', '\\'])
+                .next()
+                .map(|s| s.split('?').next().unwrap_or(s).to_string())
+                .unwrap_or_default();
+            (uri, name, None)
+        } else if clip.is::<ges::TitleClip>() {
+            let title = self.title_of(id)?;
+            (String::new(), title.name(), Some(ClipBody::Title(title)))
+        } else {
+            static UNKNOWN: std::sync::Once = std::sync::Once::new();
+            UNKNOWN.call_once(|| {
+                eprintln!(
+                    "kuvatin-video: a {} on the timeline has no record and is not saved",
+                    clip.type_().name()
+                )
+            });
+            return None;
+        };
+        let secs = |t: gst::ClockTime| t.nseconds() as f64 / 1e9;
+        Some(crate::document::ClipRecord {
+            uri,
+            name,
+            track: clip.layer().map(|l| l.priority() as usize).unwrap_or(0),
+            start: secs(clip.start()),
+            inpoint: secs(clip.inpoint()),
+            duration: secs(clip.duration()),
+            rate: clip_rate_of(clip),
+            layout: self
+                .clip_layout(id)
+                .map(Into::into)
+                .unwrap_or(crate::document::LayoutRecord {
+                    posx: 0,
+                    posy: 0,
+                    scale: 1.0,
+                    alpha: 1.0,
+                    volume: 1.0,
+                }),
+            // Filled in by the caller, which is the only side that knows how a
+            // sequence clip was described when it arrived.
+            sequence: None,
+            body,
+        })
+    }
+
+    /// A title clip's text and styling, read back from its text overlay; None
+    /// for any other clip. Child properties rather than `TitleSource`'s own
+    /// getters, which GES deprecated.
+    pub fn title_of(&self, id: &ClipId) -> Option<crate::document::TitleRecord> {
+        use crate::document::{TitleHAlign, TitleRecord, TitleVAlign};
+        let clip = self.clips.get(&id.0)?;
+        if !clip.is::<ges::TitleClip>() {
+            return None;
+        }
+        let string = |n: &str| {
+            clip.child_property(n)
+                .and_then(|v| v.get::<Option<String>>().ok().flatten())
+                .unwrap_or_default()
+        };
+        let color = clip
+            .child_property("color")
+            .and_then(|v| v.get::<u32>().ok())
+            .map(crate::document::format_color)
+            .unwrap_or_else(|| "#ffffff".into());
+        Some(TitleRecord {
+            text: string("text"),
+            font: string("font-desc"),
+            color,
+            halign: match enum_child_nick(clip, "halignment").as_deref() {
+                Some("left") => TitleHAlign::Left,
+                Some("right") => TitleHAlign::Right,
+                _ => TitleHAlign::Center,
+            },
+            valign: match enum_child_nick(clip, "valignment").as_deref() {
+                Some("top") => TitleVAlign::Top,
+                Some("bottom") => TitleVAlign::Bottom,
+                _ => TitleVAlign::Center,
+            },
+        })
+    }
+
+    /// Add a text overlay: a GES `TitleClip` on `track` at `start`, lasting
+    /// `duration`. It has no source, so nothing is discovered and nothing can
+    /// be missing.
+    pub fn add_title_clip(
+        &mut self,
+        title: &crate::document::TitleRecord,
+        track: usize,
+        start: Duration,
+        duration: Duration,
+    ) -> Result<ClipId> {
+        if self.rendering.get() {
+            anyhow::bail!("a render is in progress");
+        }
+        let clip = ges::TitleClip::new()
+            .ok_or_else(|| anyhow::anyhow!("GES could not make a title clip"))?;
+        clip.set_start(gst::ClockTime::from_nseconds(start.as_nanos() as u64));
+        clip.set_inpoint(gst::ClockTime::ZERO);
+        clip.set_duration(gst::ClockTime::from_nseconds(duration.as_nanos() as u64));
+        self.layer(track).add_clip(&clip)?;
+        // The text overlay belongs to the clip's track element, which only
+        // exists once the clip is on a layer.
+        write_title(clip.upcast_ref(), title);
+        // Async, as in add_clip_uri.
+        self.commit();
+        let name = clip
+            .name()
+            .map(|s| s.to_string())
+            .filter(|s| !s.is_empty())
+            .ok_or_else(|| anyhow::anyhow!("GES returned an unnamed clip"))?;
+        self.clips.insert(name.clone(), clip.upcast());
+        self.touched();
+        Ok(ClipId(name))
+    }
+
+    /// [`Self::add_title_clip`] at the end of `track`, for "Add text".
+    pub fn append_title_clip(
+        &mut self,
+        title: &crate::document::TitleRecord,
+        track: usize,
+        duration: Duration,
+    ) -> Result<ClipInfo> {
+        let start = Duration::from_nanos(self.track_end(track).nseconds());
+        let id = self.add_title_clip(title, track, start, duration)?;
+        Ok(ClipInfo {
+            id,
+            track,
+            start,
+            duration,
+        })
+    }
+
+    /// Rewrite a title clip's text, font, colour and alignment. Inert for a
+    /// clip that is not a title, and while rendering.
+    pub fn set_title(&mut self, id: &ClipId, title: &crate::document::TitleRecord) {
+        if self.rendering.get() {
+            return;
+        }
+        let Some(clip) = self.clips.get(&id.0) else {
+            return;
+        };
+        if !clip.is::<ges::TitleClip>() {
+            return;
+        }
+        write_title(clip, title);
+        self.commit();
+        self.touched();
+    }
+
+    /// Whether no clip of ours is on `layer`. Not `layer.clips()`: over an
+    /// overlap GES puts a transition of its own on the layer, and a layer
+    /// holding only a stranded one is still empty.
+    fn layer_is_empty(&self, layer: &ges::Layer) -> bool {
+        !self
+            .clips
+            .values()
+            .any(|c| c.layer().is_some_and(|l| &l == layer))
+    }
+
     /// Replace the timeline with what `doc` describes.
     ///
     /// Returns the sources it could not open, by name, rather than failing the
@@ -2213,6 +2426,20 @@ impl Project {
             // file name in it. Discovery is the honest check — and it warms the
             // asset the clip is about to use. (It is bounded: see
             // `ensure_discovery_timeout`.)
+            if let Some(crate::document::ClipBody::Title(title)) = &rec.body {
+                // A title has no source: it can never be missing, and must
+                // never be named as such.
+                let placed = self.add_title_clip(
+                    title,
+                    rec.track,
+                    Duration::from_secs_f64(rec.start.max(0.0)),
+                    Duration::from_secs_f64(rec.duration.max(0.0)),
+                );
+                if let Ok(id) = placed {
+                    self.set_clip_layout(&id, rec.layout.into());
+                }
+                continue;
+            }
             let name = || {
                 if rec.name.is_empty() {
                     rec.uri.clone()
@@ -5984,4 +6211,232 @@ mod tests {
             "{secs} vs {length}"
         );
     }
+
+    fn title_record(text: &str) -> crate::document::TitleRecord {
+        crate::document::TitleRecord {
+            text: text.into(),
+            font: "Serif Italic 40".into(),
+            color: "#ffcc0080".into(),
+            halign: crate::document::TitleHAlign::Right,
+            valign: crate::document::TitleVAlign::Bottom,
+        }
+    }
+
+    /// The regression the record change exists for: a clip that is not a URI
+    /// clip used to be dropped from every record, and so from every file and
+    /// every undo step.
+    #[test]
+    fn title_is_not_dropped_by_clip_records() {
+        use crate::document::ClipBody;
+        let (dir, png, mut project) = undo_fixture("title-records");
+        let still = project
+            .add_clip(&png, 1, secs(0.0), Duration::ZERO, secs(2.0))
+            .expect("still");
+        let title = project
+            .add_title_clip(&title_record("Hello"), 0, secs(0.5), secs(3.0))
+            .expect("title");
+        let records = project.clip_records();
+        assert_eq!(records.len(), 2, "{records:?}");
+        assert_eq!((&records[0].0, &records[1].0), (&title, &still));
+        let r = &records[0].1;
+        assert_eq!((r.track, r.start, r.duration), (0, 0.5, 3.0));
+        assert_eq!((r.uri.as_str(), r.name.as_str()), ("", "Hello"));
+        assert_eq!(r.body, Some(ClipBody::Title(title_record("Hello"))));
+        assert!(records[1].1.body.is_none());
+        // A title's setter is inert on anything else.
+        project.set_title(&still, &title_record("Not a title"));
+        assert!(record_of(&project, &still).body.is_none());
+        assert_eq!(project.title_of(&still), None);
+        let _ = std::fs::remove_dir_all(&dir);
+    }
+
+    #[test]
+    fn title_appends_after_the_last_clip_on_its_track() {
+        let (dir, png, mut project) = undo_fixture("title-append");
+        project
+            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(2.0))
+            .expect("still");
+        let info = project
+            .append_title_clip(&crate::document::TitleRecord::default(), 0, secs(5.0))
+            .expect("title");
+        assert_eq!(
+            (info.track, info.start, info.duration),
+            (0, secs(2.0), secs(5.0))
+        );
+        let r = record_of(&project, &info.id);
+        assert_eq!((r.start, r.duration, r.name.as_str()), (2.0, 5.0, "Text"));
+        let _ = std::fs::remove_dir_all(&dir);
+    }
+
+    /// Text over two lines, every style field and a transform, through a
+    /// file and back.
+    #[test]
+    fn title_round_trips_through_a_document() {
+        let (dir, png, mut project) = undo_fixture("title-document");
+        project
+            .add_clip(&png, 1, secs(0.0), Duration::ZERO, secs(4.0))
+            .expect("still");
+        let title = project
+            .add_title_clip(&title_record("First line\nsecond"), 0, secs(1.0), secs(2.5))
+            .expect("title");
+        project.set_clip_layout(
+            &title,
+            Layout {
+                posx: 40,
+                posy: -20,
+                scale: 0.5,
+                alpha: 0.75,
+                volume: 1.0,
+            },
+        );
+        let doc = project.to_document();
+        assert_eq!(doc.version, 2);
+        let path = dir.join("titled.kuvatin");
+        doc.save(&path).expect("save");
+        let loaded = crate::document::ProjectFile::load(&path).expect("load");
+        let mut reopened = Project::new(|_f| {}).expect("project");
+        let missing = reopened.apply_document(&loaded).expect("apply");
+        assert!(missing.is_empty(), "{missing:?}");
+        assert_eq!(reopened.to_document().clips, doc.clips);
+        let _ = std::fs::remove_dir_all(&dir);
+    }
+
+    /// A title has no source, so opening one can never report it missing.
+    #[test]
+    fn title_is_never_a_missing_source() {
+        let (dir, _png, mut project) = undo_fixture("title-missing");
+        project
+            .add_title_clip(&title_record("Only"), 0, secs(0.0), secs(2.0))
+            .expect("title");
+        let doc = project.to_document();
+        let mut reopened = Project::new(|_f| {}).expect("project");
+        let missing = reopened.apply_document(&doc).expect("apply");
+        assert!(missing.is_empty(), "{missing:?}");
+        assert_eq!(reopened.clip_records().len(), 1);
+        let _ = std::fs::remove_dir_all(&dir);
+    }
+
+    #[test]
+    fn title_comes_back_after_being_removed() {
+        let (dir, _png, mut project) = undo_fixture("title-restore");
+        let id = project
+            .add_title_clip(&title_record("Back again"), 0, secs(1.0), secs(2.0))
+            .expect("title");
+        let before = record_of(&project, &id);
+        assert!(project.remove_clip(&id));
+        assert!(project.clip_records().is_empty());
+        project.restore_clip(&id, &before).expect("restore");
+        assert_eq!(project.clip_records().len(), 1);
+        assert_same_record(&record_of(&project, &id), &before);
+        let _ = std::fs::remove_dir_all(&dir);
+    }
+
+    /// Undoing a typed word writes the old text back, with or without a move,
+    /// and a clip whose place did not change is never parked to do it.
+    #[test]
+    fn title_text_is_written_back_by_set_clip_records() {
+        let (dir, _png, mut project) = undo_fixture("title-write-back");
+        let id = project
+            .add_title_clip(&title_record("Before"), 0, secs(1.0), secs(2.0))
+            .expect("title");
+        let before = record_of(&project, &id);
+        let layer = project.clips[&id.0].layer().expect("on a layer");
+        project.set_title(&id, &title_record("After"));
+        assert_eq!(record_of(&project, &id).name, "After");
+        assert!(write_back(&mut project, &[(&id, &before)]));
+        assert_same_record(&record_of(&project, &id), &before);
+        assert_eq!(project.clips[&id.0].layer(), Some(layer), "never parked");
+        // Text and place together.
+        let mut moved = before.clone();
+        moved.start = 3.0;
+        moved.name = "Moved".into();
+        moved.body = Some(crate::document::ClipBody::Title(title_record("Moved")));
+        assert!(write_back(&mut project, &[(&id, &moved)]));
+        assert_same_record(&record_of(&project, &id), &moved);
+        let _ = std::fs::remove_dir_all(&dir);
+    }
+
+    /// A title is a source, so it has a frame positioner like any other clip:
+    /// position, scale and opacity all work on it.
+    #[test]
+    fn title_scales_and_fades() {
+        let (dir, _png, mut project) = undo_fixture("title-layout");
+        let id = project
+            .add_title_clip(&title_record("Layout"), 0, secs(0.0), secs(2.0))
+            .expect("title");
+        let want = Layout {
+            posx: -60,
+            posy: 30,
+            scale: 0.4,
+            alpha: 0.5,
+            volume: 1.0,
+        };
+        project.set_clip_layout(&id, want);
+        let got = project.clip_layout(&id).expect("layout");
+        assert_eq!((got.posx, got.posy), (want.posx, want.posy));
+        assert!((got.scale - want.scale).abs() < 1e-3, "{}", got.scale);
+        assert!((got.alpha - want.alpha).abs() < 1e-9, "{}", got.alpha);
+        let _ = std::fs::remove_dir_all(&dir);
+    }
+
+    /// A title draws over a transparent frame: away from its text, the blue
+    /// still beneath it shows. (Over a white still an opaque white frame
+    /// would pass, so the still is blue.) The text is the colour asked for,
+    /// which pins the byte order `parse_color` gives GES.
+    #[test]
+    fn title_composites_over_a_clip() {
+        type Shot = Arc<std::sync::Mutex<Option<(u32, u32, Vec<u8>)>>>;
+        let shot: Shot = Arc::new(std::sync::Mutex::new(None));
+        let sink = shot.clone();
+        let dir = scratch("title-composite");
+        let png = dir.join("blue.png");
+        image::RgbaImage::from_pixel(320, 180, image::Rgba([0, 0, 255, 255]))
+            .save(&png)
+            .expect("still");
+        let mut project = Project::new(move |f| {
+            let mut buf = vec![0u8; (f.width * f.height * 4) as usize];
+            f.copy_packed_into(&mut buf);
+            *sink.lock().unwrap() = Some((f.width, f.height, buf));
+        })
+        .expect("project");
+        project
+            .add_clip(&png, 1, Duration::ZERO, Duration::ZERO, secs(3.0))
+            .expect("still");
+        let title = crate::document::TitleRecord {
+            text: "MMMM".into(),
+            font: "Sans Bold 60".into(),
+            color: "#ff0000".into(),
+            halign: crate::document::TitleHAlign::Left,
+            valign: crate::document::TitleVAlign::Top,
+        };
+        project
+            .add_title_clip(&title, 0, Duration::ZERO, secs(3.0))
+            .expect("title");
+        project.pause().expect("pause");
+        wait_settled(&project);
+        *shot.lock().unwrap() = None;
+        project.seek_accurate(secs(1.0)).expect("seek");
+        wait_settled(&project);
+        std::thread::sleep(Duration::from_millis(500));
+        let (w, h, px) = shot.lock().unwrap().clone().expect("a frame");
+        let at = |x: u32, y: u32| {
+            let i = ((y * w + x) * 4) as usize;
+            (px[i], px[i + 1], px[i + 2])
+        };
+        let (r, g, b) = at(w - 5, h - 5);
+        assert!(
+            r < 15 && g < 15 && b > 240,
+            "the blue still shows: {:?}",
+            (r, g, b)
+        );
+        let red = (0..h / 4)
+            .flat_map(|y| (0..w / 3).map(move |x| (x, y)))
+            .filter(|&(x, y)| {
+                let (r, g, b) = at(x, y);
+                r > 200 && g < 60 && b < 60
+            })
+            .count();
+        assert!(red > 100, "red text at the top left: {red} pixels");
+        let _ = std::fs::remove_dir_all(&dir);
+    }
 }
````

- [ ] **Step 2: Run the gates.** Expected: `cargo test -p kuvatin` 303 passed; `cargo test -p kuvatin-video --lib -- --test-threads=1 title_ undo_` 36 passed. `title_composites_over_a_clip` is the one that proves the background: with the `foreground-color` line removed it reads the corner as white, `(253, 253, 253)`, and fails.

- [ ] **Step 3: Commit.** `The engine builds, reads back and restores title clips`

### Task 3: Clips may overlap on a track, bounded by what GES refuses

`slide_within_gap` becomes `slide_within_layer`: a slid clip may overlap the clip before or after it, but the neighbour keeps 0.2 s of its head (or tail), this clip keeps 0.2 s clear of the neighbour's end (or start), and it never reaches the clip beyond either one. `around` picks those four neighbours. `trim_bounds` gives an edge the same limits, and `trim_clip` applies them to outward trims only. The six slide tests are rewritten under names that keep the CI substrings, and `undo_writes_a_clamped_slide_back_exactly` now expects the clip to stop overlapping its neighbour rather than touching it. The `Project` doc comment is corrected, and `snap_slide`'s comment says what happens past the magnet.

**Files:**
- Modify: `crates/kuvatin-video/src/project.rs` (`around`, `slide_within_layer`, `trim_bounds` replace `slide_within_gap`; `slide_clip`; `trim_clip`; the `Project` comment; tests)
- Modify: `crates/kuvatin/src/gui/video/timeline.rs` (`snap_slide`'s doc comment)

- [ ] **Step 1: Apply the diff.**

````diff
diff --git a/crates/kuvatin-video/src/project.rs b/crates/kuvatin-video/src/project.rs
index f44554e..c52f5a3 100644
--- a/crates/kuvatin-video/src/project.rs
+++ b/crates/kuvatin-video/src/project.rs
@@ -579,40 +579,102 @@ fn room_after(start: i128, dur: i128, neighbours: &[(i128, i128)]) -> Option<i12
         .min()
 }
 
-/// Read a GES clip's current timeline geometry.
+/// The clips around one at `start` on its layer, from `neighbours` (every
+/// OTHER clip there as `(start, end)`, any order): the one before it and the
+/// one before that, the one after it and the one after that. A neighbour
+/// starting where the clip does counts as after it.
+type Around = (
+    Option<(i128, i128)>,
+    Option<(i128, i128)>,
+    Option<(i128, i128)>,
+    Option<(i128, i128)>,
+);
+
+fn around(start: i128, neighbours: &[(i128, i128)]) -> Around {
+    let mut sorted = neighbours.to_vec();
+    sorted.sort_unstable();
+    let split = sorted.partition_point(|&(n_start, _)| n_start < start);
+    let (before, after) = sorted.split_at(split);
+    let back = |k: usize| before.len().checked_sub(k).map(|i| before[i]);
+    (
+        back(1),
+        back(2),
+        after.first().copied(),
+        after.get(1).copied(),
+    )
+}
+
 /// Where a slid clip may actually land on its layer.
 ///
-/// GES stacks whatever it is told to stack: drop one clip onto another on the
-/// same layer and the later one simply hides the earlier, with nothing on
-/// screen to say so. A clip therefore stays inside the gap it already occupies
-/// — it can butt up against a neighbour on either side, and no further.
+/// Clips on one layer may overlap, and GES draws a cross-dissolve where they
+/// do. What GES refuses is one clip wholly on top of another and three clips
+/// at one instant, so a slide stops short of those, the same rule
+/// [`trim_bounds`] gives a trim:
+///
+/// - the clip before (P) keeps at least [`MIN_TRIM_NS`] of its head, so the
+///   order on the track cannot change, and this clip keeps at least as much
+///   past P's end, so it is never swallowed;
+/// - the same two, mirrored, against the clip after (N);
+/// - it never reaches the clip beyond either of those.
 ///
 /// `neighbours` is every OTHER clip on the layer as `(start, end)` in
-/// nanoseconds, in any order. A clip that is already overlapping something
-/// (a project made before this rule) is not frozen in place: it just gets the
-/// old clamp at zero, so it can be dragged out of the mess.
-fn slide_within_gap(start: i128, dur: i128, delta: i128, neighbours: &[(i128, i128)]) -> i128 {
-    let desired = (start + delta).max(0);
-    let end = start + dur;
-    // The gap around the clip's current position: the nearest neighbour edge
-    // on each side. Anything already overlapping it is not a boundary — it is
-    // the mess the user is trying to drag out of.
+/// nanoseconds, in any order. The bounds come from where the neighbours are,
+/// not from where the clip is, so a clip already in a tangle (a project from
+/// before any rule) is moved out of it rather than frozen in it. A gap too
+/// small to hold it leaves it where it was.
+fn slide_within_layer(start: i128, dur: i128, delta: i128, neighbours: &[(i128, i128)]) -> i128 {
+    let desired = start + delta;
+    let (p, pp, n, nn) = around(start, neighbours);
     let mut floor = 0i128;
+    if let Some((p_start, p_end)) = p {
+        floor = floor
+            .max(p_start + MIN_TRIM_NS)
+            .max(p_end + MIN_TRIM_NS - dur);
+    }
+    if let Some((_, pp_end)) = pp {
+        floor = floor.max(pp_end);
+    }
     let mut ceil = i128::MAX;
-    for &(n_start, n_end) in neighbours {
-        if n_end <= start {
-            floor = floor.max(n_end);
-        } else if n_start >= end {
-            ceil = ceil.min(n_start);
-        }
+    if let Some((n_start, n_end)) = n {
+        ceil = ceil
+            .min(n_start - MIN_TRIM_NS)
+            .min(n_end - MIN_TRIM_NS - dur);
+    }
+    if let Some((nn_start, _)) = nn {
+        ceil = ceil.min(nn_start - dur);
     }
-    if ceil == i128::MAX {
-        return desired.max(floor);
+    if ceil < floor {
+        return start;
     }
-    // A gap too small to hold the clip leaves it exactly where it was.
-    desired.clamp(floor, (ceil - dur).max(floor))
+    desired.clamp(floor, ceil)
+}
+
+/// How far a clip at `start` may be trimmed out on its layer, as (earliest
+/// start, latest end) in nanoseconds; `i128::MAX` when nothing bounds the
+/// end. The slide rule's bounds for an edge: a left edge keeps the clip before
+/// at least [`MIN_TRIM_NS`] of its head and never reaches the clip before
+/// that; a right edge leaves the clip after at least as much of its tail and
+/// never reaches the clip after that.
+fn trim_bounds(start: i128, neighbours: &[(i128, i128)]) -> (i128, i128) {
+    let (p, pp, n, nn) = around(start, neighbours);
+    let mut earliest = 0i128;
+    if let Some((p_start, _)) = p {
+        earliest = earliest.max(p_start + MIN_TRIM_NS);
+    }
+    if let Some((_, pp_end)) = pp {
+        earliest = earliest.max(pp_end);
+    }
+    let mut latest = i128::MAX;
+    if let Some((_, n_end)) = n {
+        latest = latest.min(n_end - MIN_TRIM_NS);
+    }
+    if let Some((nn_start, _)) = nn {
+        latest = latest.min(nn_start);
+    }
+    (earliest, latest)
 }
 
+/// Read a GES clip's current timeline geometry.
 fn clip_geom(clip: &ges::Clip) -> ClipGeom {
     ClipGeom {
         start: Duration::from_nanos(clip.start().nseconds()),
@@ -1161,7 +1223,7 @@ fn set_clip_frame(clip: &ges::Clip, posx: i32, posy: i32, width: i32, height: i3
 }
 
 /// A GES-backed editing project: one timeline, one preview pipeline. Layers are
-/// visual tracks, index 0 = bottom (top layers composite over lower ones).
+/// visual tracks, index 0 = top (it composites over the layers below it).
 pub struct Project {
     timeline: ges::Timeline,
     layers: Vec<ges::Layer>,
@@ -1502,9 +1564,10 @@ impl Project {
     }
 
     /// Slide a clip along its track by `delta_secs` (may be negative); start is
-    /// clamped to >= 0 and to the gap the clip occupies on its layer, so a drag
-    /// cannot bury one clip under another (see [`slide_within_gap`]). Returns
-    /// the resulting geometry, or None for an unknown id.
+    /// clamped to >= 0 and to what its neighbours on the layer allow: it may
+    /// overlap one, for a cross-dissolve, but never bury one or be buried
+    /// (see [`slide_within_layer`]). Returns the resulting geometry, or None
+    /// for an unknown id.
     pub fn slide_clip(&mut self, id: &ClipId, delta_secs: f64) -> Option<ClipGeom> {
         if self.rendering.get() {
             return None;
@@ -1513,7 +1576,7 @@ impl Project {
         let start = clip.start().nseconds() as i128;
         let dur = clip.duration().nseconds() as i128;
         let delta = (delta_secs * 1e9) as i128;
-        let new_start = slide_within_gap(start, dur, delta, &self.layer_neighbours(id)) as u64;
+        let new_start = slide_within_layer(start, dur, delta, &self.layer_neighbours(id)) as u64;
         clip.set_start(gst::ClockTime::from_nseconds(new_start));
         self.commit();
         self.touched();
@@ -1541,8 +1604,10 @@ impl Project {
 
     /// Trim a clip by dragging an edge. `edge < 0` = left edge (keeps the right
     /// end fixed by moving start+inpoint and shrinking duration); `edge > 0` =
-    /// right edge (adjusts duration only). Clamped to the source bounds and a
-    /// 0.2 s minimum. Returns the resulting geometry.
+    /// right edge (adjusts duration only). Clamped to the source bounds, a
+    /// 0.2 s minimum and, outward, to what the neighbours on its layer allow
+    /// (see [`trim_bounds`]); trimming in is never refused. Returns the
+    /// resulting geometry.
     pub fn trim_clip(&mut self, id: &ClipId, edge: i32, delta_secs: f64) -> Option<ClipGeom> {
         if self.rendering.get() {
             return None;
@@ -1556,8 +1621,14 @@ impl Project {
         let max_ns = clip
             .property::<Option<gst::ClockTime>>("max-duration")
             .map(|m| m.nseconds() as i128);
-        let delta = (delta_secs * 1e9) as i128;
+        let mut delta = (delta_secs * 1e9) as i128;
         let rate = clip_rate_of(&clip);
+        let (earliest, latest) = trim_bounds(start, &self.layer_neighbours(id));
+        if edge < 0 && delta < 0 {
+            delta = delta.max((earliest - start).min(0));
+        } else if edge > 0 && delta > 0 && latest != i128::MAX {
+            delta = delta.min((latest - (start + dur)).max(0));
+        }
 
         if edge < 0 {
             let (ns, ni, nd) = trim_left_math(start, inpoint, dur, delta, rate);
@@ -3075,45 +3146,120 @@ mod tests {
 
     #[test]
     fn a_clip_on_an_empty_layer_slides_freely() {
-        assert_eq!(slide_within_gap(2 * S, S, 3 * S, &[]), 5 * S);
-        assert_eq!(slide_within_gap(2 * S, S, -S, &[]), S);
+        assert_eq!(slide_within_layer(2 * S, S, 3 * S, &[]), 5 * S);
+        assert_eq!(slide_within_layer(2 * S, S, -S, &[]), S);
     }
 
     #[test]
     fn sliding_past_the_start_of_the_timeline_stops_at_zero() {
-        assert_eq!(slide_within_gap(2 * S, S, -5 * S, &[]), 0);
+        assert_eq!(slide_within_layer(2 * S, S, -5 * S, &[]), 0);
     }
 
-    /// GES stacks whatever it is told to stack: the later clip simply hides the
-    /// earlier one, with nothing on screen to say so.
+    /// Butting up is exact, and a slide further on overlaps the neighbour
+    /// for a dissolve, until this clip would be swallowed by it: it keeps
+    /// the trim minimum of its head clear of the neighbour's start.
     #[test]
-    fn a_clip_stops_against_the_neighbour_on_its_right() {
+    fn a_clip_overlaps_the_neighbour_on_its_right_but_is_never_swallowed() {
         // [2,3) sliding right into a neighbour at [5,8).
         let neighbours = [(5 * S, 8 * S)];
-        assert_eq!(slide_within_gap(2 * S, S, 10 * S, &neighbours), 4 * S);
+        assert_eq!(slide_within_layer(2 * S, S, 2 * S, &neighbours), 4 * S);
+        assert_eq!(
+            slide_within_layer(2 * S, S, 5 * S / 2, &neighbours),
+            9 * S / 2
+        );
+        assert_eq!(
+            slide_within_layer(2 * S, S, 10 * S, &neighbours),
+            5 * S - MIN_TRIM_NS
+        );
+        // A long clip stops where the neighbour keeps 0.2 s of its tail.
+        assert_eq!(
+            slide_within_layer(0, 5 * S, 10 * S, &neighbours),
+            8 * S - MIN_TRIM_NS - 5 * S
+        );
     }
 
     #[test]
-    fn a_clip_stops_against_the_neighbour_on_its_left() {
+    fn a_clip_overlaps_the_neighbour_on_its_left_but_never_swallows_it() {
         // [6,7) sliding left into a neighbour at [1,4).
         let neighbours = [(S, 4 * S)];
-        assert_eq!(slide_within_gap(6 * S, S, -10 * S, &neighbours), 4 * S);
+        assert_eq!(slide_within_layer(6 * S, S, -2 * S, &neighbours), 4 * S);
+        // Stops with 0.2 s of this clip past the neighbour's end.
+        assert_eq!(
+            slide_within_layer(6 * S, S, -10 * S, &neighbours),
+            4 * S + MIN_TRIM_NS - S
+        );
+        // A long clip stops where the neighbour keeps 0.2 s of its head.
+        assert_eq!(
+            slide_within_layer(6 * S, 5 * S, -10 * S, &neighbours),
+            S + MIN_TRIM_NS
+        );
     }
 
+    /// Order on the track never changes, and a clip never reaches the clip
+    /// beyond a neighbour, so three never overlap at one instant.
     #[test]
-    fn a_clip_is_confined_to_the_gap_it_is_already_in() {
-        // [4,6) between [0,4) and [6,9): it cannot move at all.
+    fn a_clip_is_confined_to_the_gap_between_the_neighbours_neighbours() {
+        // [4,6) between [0,4) and [6,9).
         let neighbours = [(0, 4 * S), (6 * S, 9 * S)];
-        assert_eq!(slide_within_gap(4 * S, 2 * S, 3 * S, &neighbours), 4 * S);
-        assert_eq!(slide_within_gap(4 * S, 2 * S, -3 * S, &neighbours), 4 * S);
+        assert_eq!(
+            slide_within_layer(4 * S, 2 * S, 3 * S, &neighbours),
+            6 * S - MIN_TRIM_NS
+        );
+        assert_eq!(
+            slide_within_layer(4 * S, 2 * S, -9 * S, &neighbours),
+            4 * S + MIN_TRIM_NS - 2 * S
+        );
+        // [0,5) before [6,9) and [7,12), which already dissolve into each
+        // other: it may overlap the first, and stops where the second starts.
+        let after = [(6 * S, 9 * S), (7 * S, 12 * S)];
+        assert_eq!(slide_within_layer(0, 5 * S, 10 * S, &after), 2 * S);
+        // [10,15) after [0,6) and [4,8), mirrored.
+        let before = [(0, 6 * S), (4 * S, 8 * S)];
+        assert_eq!(slide_within_layer(10 * S, 5 * S, -10 * S, &before), 6 * S);
     }
 
-    /// Clips that already overlap (a project from before this rule) must not be
-    /// frozen in place: the move is clamped to zero and nothing else.
+    /// A clip in a tangle with no legal place anywhere is left where it was.
+    #[test]
+    fn a_slid_clip_with_no_legal_place_stays_put() {
+        // [2,3) inside [0,4), with [3,4.1) after it.
+        let neighbours = [(0, 4 * S), (3 * S, 41 * S / 10)];
+        assert_eq!(slide_within_layer(2 * S, S, S, &neighbours), 2 * S);
+    }
+
+    /// Clips already in a tangle (a project from before any rule) are not
+    /// frozen in place: the bounds come from the neighbours, so the clip is
+    /// moved to where it is legal.
     #[test]
     fn an_already_overlapping_clip_can_still_be_moved() {
+        // [2,3) wholly inside [0,10): it comes out 0.2 s past the end.
         let neighbours = [(0, 10 * S)];
-        assert_eq!(slide_within_gap(2 * S, S, 3 * S, &neighbours), 5 * S);
+        assert_eq!(
+            slide_within_layer(2 * S, S, 3 * S, &neighbours),
+            10 * S + MIN_TRIM_NS - S
+        );
+    }
+
+    #[test]
+    fn a_trim_with_no_neighbour_is_bounded_only_by_zero() {
+        assert_eq!(trim_bounds(2 * S, &[]), (0, i128::MAX));
+    }
+
+    /// A right trim may cover the next clip for a dissolve, but leaves it
+    /// 0.2 s of its tail and never reaches the clip after it.
+    #[test]
+    fn a_right_trim_stops_short_of_covering_the_next_neighbour() {
+        let (_, latest) = trim_bounds(0, &[(3 * S, 5 * S)]);
+        assert_eq!(latest, 5 * S - MIN_TRIM_NS);
+        let (_, latest) = trim_bounds(0, &[(3 * S, 8 * S), (6 * S, 9 * S)]);
+        assert_eq!(latest, 6 * S);
+    }
+
+    #[test]
+    fn a_left_trim_stops_short_of_covering_the_previous_neighbour() {
+        let (earliest, _) = trim_bounds(6 * S, &[(S, 4 * S)]);
+        assert_eq!(earliest, S + MIN_TRIM_NS);
+        let (earliest, _) = trim_bounds(6 * S, &[(0, 2 * S), (S, 7 * S)]);
+        assert_eq!(earliest, 2 * S);
     }
 
     use super::*;
@@ -4489,8 +4635,9 @@ mod tests {
         project.set_clip_records(&owned).is_empty()
     }
 
-    /// The slide was clamped against a neighbour; reversing the amount would
-    /// not put the clip back, writing the old record does.
+    /// The slide was clamped against a neighbour, overlapping it as far as
+    /// the rule allows; reversing the amount would not put the clip back,
+    /// writing the old record does.
     #[test]
     fn undo_writes_a_clamped_slide_back_exactly() {
         let (dir, png, mut project) = undo_fixture("undo-slide");
@@ -4502,7 +4649,7 @@ mod tests {
             .expect("b");
         let before = record_of(&project, &b);
         let geom = project.slide_clip(&b, -10.0).expect("slide");
-        assert_eq!(geom.start, secs(2.0), "stopped against a");
+        assert_eq!(geom.start, secs(0.2), "a keeps 0.2 s of its head");
         assert!(write_back(&mut project, &[(&b, &before)]));
         assert_same_record(&record_of(&project, &b), &before);
         let _ = std::fs::remove_dir_all(&dir);
diff --git a/crates/kuvatin/src/gui/video/timeline.rs b/crates/kuvatin/src/gui/video/timeline.rs
index 8eddb7f..8601305 100644
--- a/crates/kuvatin/src/gui/video/timeline.rs
+++ b/crates/kuvatin/src/gui/video/timeline.rs
@@ -592,6 +592,10 @@ fn remove_timeline_clip(
 /// smallest nudge; on a tie the earlier target wins, the origin first. The
 /// start never goes before zero. `pps` is the zoom in pixels per second, and
 /// with none yet the drag comes back untouched.
+///
+/// Snapping to a neighbour's edge is the "butt up, no dissolve" place. A drag
+/// further than the magnet overlaps the neighbour, for a cross-dissolve, as far
+/// as the engine's slide rule allows.
 fn snap_slide(start: f32, duration: f32, others: &[(f32, f32)], dx_s: f32, pps: f32) -> f32 {
     if pps <= 0.0 {
         return dx_s;
````

- [ ] **Step 2: Run the gates.** Expected: `cargo test -p kuvatin` 303 passed; `cargo test -p kuvatin-video --lib -- --test-threads=1 slid neighbour confined_to_the_gap already_overlapping trim undo_` 34 passed.

- [ ] **Step 3: Commit.** `Clips may overlap on a track, bounded by what GES refuses`

### Task 4: GES cross-dissolves wherever two clips on a track overlap

`set_auto_transition(true)` on the timeline and on every layer `layer()` makes. A transition GES inserts is never in `self.clips`, so it reaches no record, no file and no step. A `#[cfg(test)]` `layer_clip_count` lets the tests see it.

**Files:**
- Modify: `crates/kuvatin-video/src/project.rs` (`Project::new`, `layer`, `layer_clip_count`, tests)

- [ ] **Step 1: Apply the diff.**

````diff
diff --git a/crates/kuvatin-video/src/project.rs b/crates/kuvatin-video/src/project.rs
index c52f5a3..cc2b871 100644
--- a/crates/kuvatin-video/src/project.rs
+++ b/crates/kuvatin-video/src/project.rs
@@ -1306,7 +1306,12 @@ impl Project {
                 done
             })
             .collect();
+        // Where two clips on a layer overlap, GES puts a cross-dissolve of
+        // its own between them, resizes it as they move and takes it away
+        // when they part. It is never one of our clips (see `record_of`).
+        timeline.set_auto_transition(true);
         let layer = timeline.append_layer();
+        layer.set_auto_transition(true);
         let pipeline = ges::Pipeline::new();
         pipeline.set_timeline(&timeline)?;
 
@@ -1438,11 +1443,13 @@ impl Project {
     }
 
     /// Ensure at least `index + 1` layers exist; return the layer at `index`.
+    /// Every layer draws a cross-dissolve where its clips overlap.
     /// A layer made for a muted position arrives silent: without that, a clip
     /// dropped onto a muted track that had no layer yet would be heard.
     fn layer(&mut self, index: usize) -> ges::Layer {
         while self.layers.len() <= index {
             let layer = self.timeline.append_layer();
+            layer.set_auto_transition(true);
             if self.mutes.get(self.layers.len()).copied().unwrap_or(false) {
                 self.apply_mute(&layer, true);
             }
@@ -2471,6 +2478,12 @@ impl Project {
             .any(|c| c.layer().is_some_and(|l| &l == layer))
     }
 
+    /// Everything GES has on a track's layer, its own transitions included.
+    #[cfg(test)]
+    fn layer_clip_count(&self, track: usize) -> usize {
+        self.layers.get(track).map_or(0, |l| l.clips().len())
+    }
+
     /// Replace the timeline with what `doc` describes.
     ///
     /// Returns the sources it could not open, by name, rather than failing the
@@ -6586,4 +6599,131 @@ mod tests {
         assert!(red > 100, "red text at the top left: {red} pixels");
         let _ = std::fs::remove_dir_all(&dir);
     }
+
+    /// Two stills on one track, the second starting 1 s before the first
+    /// ends: a 1 s dissolve.
+    fn overlapping_pair(project: &mut Project, png: &Path, track: usize) -> (ClipId, ClipId) {
+        let a = project
+            .add_clip(png, track, secs(0.0), Duration::ZERO, secs(3.0))
+            .expect("a");
+        let b = project
+            .add_clip(png, track, secs(2.0), Duration::ZERO, secs(3.0))
+            .expect("b");
+        (a, b)
+    }
+
+    /// The transition GES puts over an overlap is never one of our clips: it
+    /// reaches no record, no file and no undo step.
+    #[test]
+    fn dissolve_overlapping_clips_are_still_two_records() {
+        let (dir, png, mut project) = undo_fixture("dissolve-records");
+        overlapping_pair(&mut project, &png, 0);
+        assert_eq!(project.clip_records().len(), 2);
+        assert_eq!(project.to_document().clips.len(), 2);
+        let _ = std::fs::remove_dir_all(&dir);
+    }
+
+    #[test]
+    fn dissolve_appears_on_the_layer_and_goes_when_the_clips_part() {
+        let (dir, png, mut project) = undo_fixture("dissolve-layer");
+        let (_, b) = overlapping_pair(&mut project, &png, 0);
+        assert_eq!(project.layer_clip_count(0), 3, "two clips and a dissolve");
+        project.slide_clip(&b, 1.0).expect("slide");
+        assert_eq!(project.layer_clip_count(0), 2, "butted up: no dissolve");
+        let _ = std::fs::remove_dir_all(&dir);
+    }
+
+    #[test]
+    fn dissolve_survives_a_reopen() {
+        let (dir, png, mut project) = undo_fixture("dissolve-reopen");
+        overlapping_pair(&mut project, &png, 0);
+        let path = dir.join("dissolve.kuvatin");
+        project.to_document().save(&path).expect("save");
+        let text = std::fs::read_to_string(&path).expect("read");
+        assert_eq!(text.matches("[[clips]]").count(), 2, "{text}");
+        assert!(text.contains("version = 1"), "{text}");
+        let doc = crate::document::ProjectFile::load(&path).expect("load");
+        let mut reopened = Project::new(|_f| {}).expect("project");
+        reopened.apply_document(&doc).expect("apply");
+        assert_eq!(reopened.layer_clip_count(0), 3);
+        let _ = std::fs::remove_dir_all(&dir);
+    }
+
+    /// A layer is empty when none of OUR clips is on it, whatever GES has
+    /// left there: removing both clips of a dissolve takes the track.
+    #[test]
+    fn dissolve_leaves_no_dead_track_when_its_clips_go() {
+        let (dir, png, mut project) = undo_fixture("dissolve-prune");
+        project
+            .add_clip(&png, 0, secs(0.0), Duration::ZERO, secs(2.0))
+            .expect("top");
+        let (a, b) = overlapping_pair(&mut project, &png, 1);
+        assert_eq!(project.track_count(), 2);
+        assert!(project.remove_clip(&b));
+        assert!(project.remove_clip(&a));
+        assert_eq!(project.track_count(), 1);
+        let _ = std::fs::remove_dir_all(&dir);
+    }
+
+    /// Undo writes an overlap back through the parking dance, and the
+    /// dissolve comes back with it.
+    #[test]
+    fn dissolve_comes_back_when_undo_writes_the_overlap_back() {
+        let (dir, png, mut project) = undo_fixture("dissolve-undo");
+        let (a, b) = overlapping_pair(&mut project, &png, 0);
+        let (ra, rb) = (record_of(&project, &a), record_of(&project, &b));
+        project.slide_clip(&b, 2.0).expect("part");
+        assert_eq!(project.layer_clip_count(0), 2);
+        assert!(write_back(&mut project, &[(&a, &ra), (&b, &rb)]));
+        assert_same_record(&record_of(&project, &b), &rb);
+        assert_eq!(project.layer_clip_count(0), 3);
+        assert_eq!(project.track_count(), 1, "no parking layer left behind");
+        let _ = std::fs::remove_dir_all(&dir);
+    }
+
+    /// Halfway through a dissolve from white to black, the picture is grey.
+    #[test]
+    fn dissolve_blends_the_two_clips_in_the_preview() {
+        type Shot = Arc<std::sync::Mutex<Option<(u32, u32, Vec<u8>)>>>;
+        let shot: Shot = Arc::new(std::sync::Mutex::new(None));
+        let sink = shot.clone();
+        let dir = scratch("dissolve-blend");
+        let white = dir.join("white.png");
+        let black = dir.join("black.png");
+        image::RgbaImage::from_pixel(320, 180, image::Rgba([255, 255, 255, 255]))
+            .save(&white)
+            .expect("white");
+        image::RgbaImage::from_pixel(320, 180, image::Rgba([0, 0, 0, 255]))
+            .save(&black)
+            .expect("black");
+        let mut project = Project::new(move |f| {
+            let mut buf = vec![0u8; (f.width * f.height * 4) as usize];
+            f.copy_packed_into(&mut buf);
+            *sink.lock().unwrap() = Some((f.width, f.height, buf));
+        })
+        .expect("project");
+        project
+            .add_clip(&white, 0, secs(0.0), Duration::ZERO, secs(3.0))
+            .expect("white");
+        project
+            .add_clip(&black, 0, secs(2.0), Duration::ZERO, secs(3.0))
+            .expect("black");
+        project.pause().expect("pause");
+        wait_settled(&project);
+        *shot.lock().unwrap() = None;
+        project.seek_accurate(secs(2.5)).expect("seek");
+        wait_settled(&project);
+        std::thread::sleep(Duration::from_millis(500));
+        let (w, h, px) = shot.lock().unwrap().clone().expect("a frame");
+        let i = (((h / 2) * w + w / 2) * 4) as usize;
+        let (r, g, b) = (px[i], px[i + 1], px[i + 2]);
+        for v in [r, g, b] {
+            assert!(
+                (60..=200).contains(&v),
+                "grey mid-dissolve: {:?}",
+                (r, g, b)
+            );
+        }
+        let _ = std::fs::remove_dir_all(&dir);
+    }
 }
````

- [ ] **Step 2: Run the gates.** Expected: `cargo test -p kuvatin` 303 passed; `cargo test -p kuvatin-video --lib -- --test-threads=1 dissolve_ undo_` 27 passed. With both `set_auto_transition` calls set to `false`, four of the six `dissolve_` tests fail; the other two guard the records and the pruning.

- [ ] **Step 3: Commit.** `GES cross-dissolves wherever two clips on a track overlap`

### Task 5: The timeline knows a title clip by its body

`ClipKind` gains `title`, drawn amber. `kind_of` takes a record and answers `Title` for a title body before it looks at the URI (`kind_of_uri` keeps the old question for the waveform cache). Thumbnails and waveforms skip a title, whose URI is empty. Undo's restore check moves into `missing_sources`, which never asks about a title's source, and `place` gives a title row the name its record now has.

**Files:**
- Modify: `crates/kuvatin-video/src/lib.rs` (re-exports)
- Modify: `crates/kuvatin/ui/app.slint` (`ClipKind`, the block's gradient)
- Modify: `crates/kuvatin/src/gui/video/project_file.rs` (`kind_of`, `kind_of_uri`, `spawn_thumbnails`, test)
- Modify: `crates/kuvatin/src/gui/video/waves.rs` (skip empty URIs, test)
- Modify: `crates/kuvatin/src/gui/video/undo.rs` (`rows_after`, `place`, `missing_sources`, tests)

- [ ] **Step 1: Apply the diff.**

````diff
diff --git a/crates/kuvatin-video/src/lib.rs b/crates/kuvatin-video/src/lib.rs
index d525fc3..962fd4e 100644
--- a/crates/kuvatin-video/src/lib.rs
+++ b/crates/kuvatin-video/src/lib.rs
@@ -12,7 +12,10 @@ pub struct Frame {
 pub mod document;
 pub mod project;
 pub mod sequence;
-pub use document::{path_from_uri, ClipRecord, LayoutRecord, ProjectFile, TrackRecord};
+pub use document::{
+    format_color, parse_color, path_from_uri, ClipBody, ClipRecord, LayoutRecord, ProjectFile,
+    TitleHAlign, TitleRecord, TitleVAlign, TrackRecord,
+};
 pub use project::{
     hardware_encoding_available, is_hardware_encoder, normalize_render_size, thumbnail,
     thumbnail_uri, warm_asset, warm_asset_uri, waveform_uri, ClipGeom, ClipId, ClipInfo, Encoder,
diff --git a/crates/kuvatin/src/gui/video/project_file.rs b/crates/kuvatin/src/gui/video/project_file.rs
index 3a59aa9..b6c82c7 100644
--- a/crates/kuvatin/src/gui/video/project_file.rs
+++ b/crates/kuvatin/src/gui/video/project_file.rs
@@ -222,7 +222,7 @@ fn restore_models(
             duration: rec.duration as f32,
             inpoint: rec.inpoint as f32,
             name: rec.name.clone().into(),
-            kind: kind_of(&rec.uri),
+            kind: kind_of(rec),
             selected: false,
             thumb: Image::default(),
             rate: rec.rate as f32,
@@ -315,7 +315,8 @@ pub(super) fn spawn_thumbnails(
     let _ = std::thread::Builder::new()
         .name("kuvatin-project-thumbs".into())
         .spawn(move || {
-            for (id, rec) in records {
+            // A title has no source to picture: its name on the block is all.
+            for (id, rec) in records.into_iter().filter(|(_, r)| !r.uri.is_empty()) {
                 let Some(frame) = kuvatin_video::thumbnail_uri(&rec.uri, 160) else {
                     continue;
                 };
@@ -350,8 +351,17 @@ pub(super) fn spawn_thumbnails(
         });
 }
 
-/// What a saved URI is, for the clip's colour on the timeline.
-pub(super) fn kind_of(uri: &str) -> ClipKind {
+/// What a clip is, for its colour on the timeline: a title by its body,
+/// anything else by its URI.
+pub(super) fn kind_of(record: &kuvatin_video::ClipRecord) -> ClipKind {
+    match record.body {
+        Some(kuvatin_video::ClipBody::Title(_)) => ClipKind::Title,
+        None => kind_of_uri(&record.uri),
+    }
+}
+
+/// What a saved URI is.
+pub(super) fn kind_of_uri(uri: &str) -> ClipKind {
     if uri.starts_with("imagesequence://") {
         return ClipKind::Sequence;
     }
@@ -402,14 +412,45 @@ mod tests {
     #[test]
     fn a_uri_says_what_kind_of_clip_it_is() {
         assert_eq!(
-            kind_of("imagesequence://C:/r/f_%04d.png?start-index=1&framerate=24/1"),
+            kind_of_uri("imagesequence://C:/r/f_%04d.png?start-index=1&framerate=24/1"),
             ClipKind::Sequence
         );
-        assert_eq!(kind_of("file:///C:/shots/take1.mp4"), ClipKind::Video);
-        assert_eq!(kind_of("file:///C:/shots/logo.png"), ClipKind::Image);
+        assert_eq!(kind_of_uri("file:///C:/shots/take1.mp4"), ClipKind::Video);
+        assert_eq!(kind_of_uri("file:///C:/shots/logo.png"), ClipKind::Image);
         // A query on a plain file URI must not be read as part of the
         // extension: `.png?x=1` is still a PNG.
-        assert_eq!(kind_of("file:///C:/shots/logo.png?x=1"), ClipKind::Image);
+        assert_eq!(
+            kind_of_uri("file:///C:/shots/logo.png?x=1"),
+            ClipKind::Image
+        );
+    }
+
+    /// A title is known by its body, whatever its URI says.
+    #[test]
+    fn a_title_record_is_a_title_clip() {
+        let record = |body| kuvatin_video::ClipRecord {
+            uri: String::new(),
+            name: "Hello".into(),
+            track: 0,
+            start: 0.0,
+            inpoint: 0.0,
+            duration: 5.0,
+            rate: 1.0,
+            layout: kuvatin_video::LayoutRecord {
+                posx: 0,
+                posy: 0,
+                scale: 1.0,
+                alpha: 1.0,
+                volume: 1.0,
+            },
+            sequence: None,
+            body,
+        };
+        let title = kuvatin_video::ClipBody::Title(kuvatin_video::TitleRecord::default());
+        assert_eq!(kind_of(&record(Some(title))), ClipKind::Title);
+        let mut still = record(None);
+        still.uri = "file:///C:/shots/logo.png".into();
+        assert_eq!(kind_of(&still), ClipKind::Image);
     }
 
     fn named(name: &str) -> kuvatin_video::TrackRecord {
diff --git a/crates/kuvatin/src/gui/video/undo.rs b/crates/kuvatin/src/gui/video/undo.rs
index 411d072..a63d11a 100644
--- a/crates/kuvatin/src/gui/video/undo.rs
+++ b/crates/kuvatin/src/gui/video/undo.rs
@@ -328,7 +328,7 @@ pub(super) fn rows_after(
             (Some(record), None) => {
                 let mut row = kept.get(&a.id).cloned().unwrap_or_else(|| TimelineClip {
                     name: record.name.as_str().into(),
-                    kind: kind_of(&record.uri),
+                    kind: kind_of(record),
                     ..Default::default()
                 });
                 row.selected = false;
@@ -340,9 +340,32 @@ pub(super) fn rows_after(
     out
 }
 
+/// The names of the clips among `restores` whose source is not there to
+/// bring back, as `available` answers for a URI. A title has no source, so it
+/// is never asked about: `available("")` would look for a file with no name.
+fn missing_sources(
+    restores: &[(String, ClipRecord)],
+    kept: &HashMap<String, TimelineClip>,
+    available: impl Fn(&str) -> bool,
+) -> Vec<String> {
+    restores
+        .iter()
+        .filter(|(_, record)| record.body.is_none() && !available(&record.uri))
+        .map(|(id, record)| match kept.get(id) {
+            Some(row) => row.name.to_string(),
+            None => record.name.clone(),
+        })
+        .collect()
+}
+
 /// Put a row where `record` places its clip, under the ID the clip has now.
+/// A title's name is its text, so it follows the record; any other clip keeps
+/// the name its row has.
 fn place(row: &mut TimelineClip, id: &str, record: &ClipRecord) {
     row.id = id.into();
+    if record.body.is_some() {
+        row.name = record.name.as_str().into();
+    }
     row.track = record.track as i32;
     row.start = record.start as f32;
     row.duration = record.duration as f32;
@@ -575,15 +598,7 @@ fn apply_step(
     };
 
     // A deleted clip whose file has gone since: say which, and change nothing.
-    let missing: Vec<String> = ops
-        .restores
-        .iter()
-        .filter(|(_, record)| !p.source_available(&record.uri))
-        .map(|(id, record)| match kept.get(id) {
-            Some(row) => row.name.to_string(),
-            None => record.name.clone(),
-        })
-        .collect();
+    let missing = missing_sources(&ops.restores, &kept, |uri| p.source_available(uri));
     if !missing.is_empty() {
         drop(slot);
         show_error(
@@ -1558,4 +1573,76 @@ mod tests {
             }
         );
     }
+
+    fn title(text: &str, start: f64) -> ClipRecord {
+        let body = kuvatin_video::TitleRecord {
+            text: text.into(),
+            ..Default::default()
+        };
+        ClipRecord {
+            uri: String::new(),
+            name: body.name(),
+            body: Some(kuvatin_video::ClipBody::Title(body)),
+            ..rec(0, start, 5.0)
+        }
+    }
+
+    /// A title has no source: undo never asks whether one is there, and
+    /// never names it as gone.
+    #[test]
+    fn title_restores_never_ask_for_a_source() {
+        let restores = vec![
+            ("t".to_string(), title("Hello", 0.0)),
+            ("v".to_string(), rec(1, 0.0, 2.0)),
+        ];
+        let asked = RefCell::new(Vec::new());
+        let missing = missing_sources(&restores, &HashMap::new(), |uri| {
+            asked.borrow_mut().push(uri.to_string());
+            false
+        });
+        assert_eq!(missing, vec!["intro.mp4".to_string()]);
+        assert_eq!(
+            *asked.borrow(),
+            vec!["file:///C:/media/intro.mp4".to_string()]
+        );
+    }
+
+    /// A title coming back is amber and named by its text; a title whose
+    /// text changes takes the new name, which a media clip never does.
+    #[test]
+    fn title_rows_take_their_kind_and_name_from_the_record() {
+        let rows = vec![row("v", &rec(1, 0.0, 2.0)), {
+            let mut r = row("t", &title("Before", 0.0));
+            r.kind = ClipKind::Title;
+            r
+        }];
+        let mut renamed = rec(1, 0.0, 2.0);
+        renamed.name = "other.mp4".into();
+        let applied = vec![
+            Applied {
+                id: "t".into(),
+                now_id: "t".into(),
+                record: Some(title("After", 0.0)),
+            },
+            Applied {
+                id: "v".into(),
+                now_id: "v".into(),
+                record: Some(renamed),
+            },
+            Applied {
+                id: "back".into(),
+                now_id: "back".into(),
+                record: Some(title("Returned", 6.0)),
+            },
+        ];
+        let out = rows_after(&rows, &applied, &HashMap::new());
+        assert_eq!(
+            out[0].name.as_str(),
+            "intro.mp4",
+            "a media clip keeps its name"
+        );
+        assert_eq!(out[1].name.as_str(), "After");
+        assert_eq!(out[2].name.as_str(), "Returned");
+        assert_eq!(out[2].kind, ClipKind::Title);
+    }
 }
diff --git a/crates/kuvatin/src/gui/video/waves.rs b/crates/kuvatin/src/gui/video/waves.rs
index 6222088..66f3f18 100644
--- a/crates/kuvatin/src/gui/video/waves.rs
+++ b/crates/kuvatin/src/gui/video/waves.rs
@@ -3,7 +3,7 @@
 //! recorded, and always safe to throw away and decode again. Only video
 //! files are listened to: a still or an image sequence has no sound.
 
-use super::project_file::kind_of;
+use super::project_file::kind_of_uri;
 use crate::gui::{AppWindow, ClipKind};
 use slint::{Image, Model, Rgba8Pixel, SharedPixelBuffer, SharedString};
 use std::collections::HashMap;
@@ -34,12 +34,13 @@ struct Inner {
 impl Inner {
     /// Sort `clips` (clip id, source URI) into those whose waveform is known
     /// and the sources to decode, each once however many clips wait on it. A
-    /// clip that is not a video, or whose source has no sound, gets nothing.
+    /// clip that is not a video, or whose source has no sound, gets nothing;
+    /// nor does a title, which has no source at all (its URI is empty).
     fn request(&mut self, clips: Vec<(SharedString, String)>) -> (Ready, Vec<String>) {
         let mut ready = Vec::new();
         let mut decode = Vec::new();
         for (id, uri) in clips {
-            if kind_of(&uri) != ClipKind::Video {
+            if uri.is_empty() || kind_of_uri(&uri) != ClipKind::Video {
                 continue;
             }
             match self.done.get(&uri) {
@@ -201,6 +202,7 @@ mod tests {
                 "b".into(),
                 "imagesequence://C:/r/f_%04d.png?start-index=1&framerate=24/1".into(),
             ),
+            ("c".into(), String::new()),
         ]);
         assert!(ready.is_empty() && decode.is_empty());
     }
diff --git a/crates/kuvatin/ui/app.slint b/crates/kuvatin/ui/app.slint
index 949b6c7..2f3c1d7 100644
--- a/crates/kuvatin/ui/app.slint
+++ b/crates/kuvatin/ui/app.slint
@@ -18,7 +18,7 @@ export struct FileRow {
 }
 
 // What a timeline clip is (drives its colour and whether it has audio).
-export enum ClipKind { video, image, sequence }
+export enum ClipKind { video, image, sequence, title }
 
 // A clip placed on the video timeline. Positions are in seconds; the UI maps
 // them to pixels via the pixels-per-second zoom.
@@ -2139,7 +2139,9 @@ export component AppWindow inherits Window {
                                                 ? @linear-gradient(135deg, #3f7df5 0%, #1f4fc0 100%)
                                                 : clip.kind == ClipKind.sequence
                                                     ? @linear-gradient(135deg, #8b5cf6 0%, #5b34c9 100%)
-                                                    : @linear-gradient(135deg, #1fd3b6 0%, #0f9d84 100%);
+                                                    : clip.kind == ClipKind.title
+                                                        ? @linear-gradient(135deg, #f5a742 0%, #c07a1f 100%)
+                                                        : @linear-gradient(135deg, #1fd3b6 0%, #0f9d84 100%);
                                             border-width: clip.selected ? 2px : 1px;
                                             border-color: clip.selected ? Theme.accent : #00000055;
                                             animate border-color { duration: 130ms; }
````

- [ ] **Step 2: Run the gates.** Expected: `cargo test -p kuvatin` 306 passed.

- [ ] **Step 3: Commit.** `The timeline knows a title clip by its body`

### Task 6: Typing into a title is one undo step

`StepKind::Text` merges and describes itself as "editing the text of …". Title edits go into `pending_title` and the 100 ms tick writes them, as `pending_xform` does for the sliders: one write and one step per tick, merged within a second. After recording, the tick renames the row (`titles::rename_row`), so the step is named by the title's old text. Undo and redo drop a pending title edit, as they drop a pending transform.

**Files:**
- Create: `crates/kuvatin/src/gui/video/titles.rs` (`rename_row` only; Task 7 adds the rest)
- Modify: `crates/kuvatin/src/gui/video/mod.rs` (`mod titles`, `pending_title`, the tick)
- Modify: `crates/kuvatin/src/gui/video/undo.rs` (`StepKind::Text`, the undo run, tests)

- [ ] **Step 1: Apply the diff.**

````diff
diff --git a/crates/kuvatin/src/gui/video/mod.rs b/crates/kuvatin/src/gui/video/mod.rs
index 62841dd..91b4bdf 100644
--- a/crates/kuvatin/src/gui/video/mod.rs
+++ b/crates/kuvatin/src/gui/video/mod.rs
@@ -6,6 +6,7 @@ pub(super) mod export;
 pub(super) mod import;
 mod project_file;
 mod timeline;
+mod titles;
 mod tracks;
 mod transport;
 mod undo;
@@ -68,6 +69,9 @@ pub(super) struct VideoState {
     /// Latest inspector transform awaiting a coalesced apply on the UI timer.
     /// Rapid slider drags only stash a value here; no GES work per event.
     pub(super) pending_xform: Rc<RefCell<Option<(String, kuvatin_video::Layout)>>>,
+    /// Latest title edit awaiting the same apply: typing stashes the whole
+    /// title here per keystroke, and the tick writes it once.
+    pub(super) pending_title: Rc<RefCell<Option<(String, kuvatin_video::TitleRecord)>>>,
     /// Latest scrub target (seconds, frame-accurate?) awaiting the UI tick:
     /// one seek per tick instead of one per pointer event, and an ACCURATE
     /// landing on release so the picture matches the playhead.
@@ -101,6 +105,7 @@ impl VideoState {
             tracks,
             sel_idx: Rc::new(Cell::new(-1)),
             pending_xform: Rc::new(RefCell::new(None)),
+            pending_title: Rc::new(RefCell::new(None)),
             pending_seek: Rc::new(Cell::new(None)),
             history: Rc::new(RefCell::new(crate::gui::history::History::new())),
             waves: waves::Waves::default(),
@@ -327,6 +332,7 @@ pub(super) fn wire(
         let ui_weak = ui_weak.clone();
         let project_slot = project_slot.clone();
         let pending_xform = pending_xform.clone();
+        let pending_title = st.pending_title.clone();
         let pending_seek = pending_seek.clone();
         let export_active = export_active.clone();
         let export_pending = export_pending.clone();
@@ -357,13 +363,16 @@ pub(super) fn wire(
                 // a locked track keeps its transform and the value is
                 // dropped: its sliders and preview box stand down, so only a
                 // value stashed just before the lock went on gets here.
-                if let Some((id, l)) = pending_xform.borrow_mut().take() {
+                let unlocked = |id: &str| {
                     let track = rec
                         .tl_clips
                         .iter()
                         .find(|r| r.id.as_str() == id)
                         .map_or(-1, |r| r.track);
-                    if !tracks::locked(&tracks::rows_of(&rec.tracks), track) {
+                    !tracks::locked(&tracks::rows_of(&rec.tracks), track)
+                };
+                if let Some((id, l)) = pending_xform.borrow_mut().take() {
+                    if unlocked(&id) {
                         let before = rec.before(Some(&*project));
                         project.set_clip_layout(&kuvatin_video::ClipId(id.clone()), l);
                         rec.record(
@@ -374,6 +383,23 @@ pub(super) fn wire(
                         );
                     }
                 }
+                // The same for a title's text: one write and one step per
+                // tick, merged with the last if it is the same title. The
+                // row takes the name the text gives it, after the step has
+                // named itself by the old one.
+                if let Some((id, title)) = pending_title.borrow_mut().take() {
+                    if unlocked(&id) {
+                        let before = rec.before(Some(&*project));
+                        project.set_title(&kuvatin_video::ClipId(id.clone()), &title);
+                        rec.record(
+                            Some(&*project),
+                            undo::StepKind::Text,
+                            Some(undo::Subject::Clip(id.clone())),
+                            before,
+                        );
+                        titles::rename_row(&ui, &rec.tl_clips, &id, &title.name());
+                    }
+                }
                 // Scrub target: one (keyframe) seek per tick during a drag,
                 // a frame-accurate one on release.
                 if let Some((secs, accurate)) = pending_seek.take() {
diff --git a/crates/kuvatin/src/gui/video/titles.rs b/crates/kuvatin/src/gui/video/titles.rs
new file mode 100644
index 0000000..eca8796
--- /dev/null
+++ b/crates/kuvatin/src/gui/video/titles.rs
@@ -0,0 +1,22 @@
+//! Text overlays as the interface shows them.
+
+use crate::gui::{AppWindow, TimelineClip};
+use slint::{Model, VecModel};
+
+/// Give a title's row the name its text now gives it, and the inspector's
+/// heading too when the title is the selected clip.
+pub(super) fn rename_row(ui: &AppWindow, rows: &VecModel<TimelineClip>, id: &str, name: &str) {
+    for i in 0..rows.row_count() {
+        let Some(mut row) = rows.row_data(i) else {
+            continue;
+        };
+        if row.id.as_str() != id || row.name.as_str() == name {
+            continue;
+        }
+        row.name = name.into();
+        rows.set_row_data(i, row);
+        if ui.get_timeline_selected() == i as i32 {
+            ui.set_inspector_name(name.into());
+        }
+    }
+}
diff --git a/crates/kuvatin/src/gui/video/undo.rs b/crates/kuvatin/src/gui/video/undo.rs
index a63d11a..4391304 100644
--- a/crates/kuvatin/src/gui/video/undo.rs
+++ b/crates/kuvatin/src/gui/video/undo.rs
@@ -36,11 +36,14 @@ pub(super) enum StepKind {
     MuteTrack,
     LockTrack,
     RenameTrack,
+    /// A title's text, font, colour or alignment. Its own kind so the hint
+    /// does not call typing a transform.
+    Text,
 }
 
 impl StepKind {
-    /// The kinds a continuous gesture produces, and a rename, whose
-    /// keystrokes are one renaming. Only these merge. A mute or a lock is one
+    /// The kinds a continuous gesture produces, and a rename or a title's
+    /// text, whose keystrokes are one edit. Only these merge. A mute or a lock is one
     /// click: muting and unmuting inside a second is two steps, not nothing.
     fn merges(self) -> bool {
         matches!(
@@ -50,6 +53,7 @@ impl StepKind {
                 | StepKind::Transform
                 | StepKind::Duration
                 | StepKind::RenameTrack
+                | StepKind::Text
         )
     }
 }
@@ -219,6 +223,7 @@ impl Step for TimelineStep {
             StepKind::LockTrack if self.turned_on(|t| t.locked) => format!("locking {name}"),
             StepKind::LockTrack => format!("unlocking {name}"),
             StepKind::RenameTrack => format!("renaming {name}"),
+            StepKind::Text => format!("editing the text of {name}"),
         }
     }
 
@@ -520,6 +525,7 @@ pub(super) fn wire(ui: &AppWindow, st: &super::VideoState, ex: &super::export::E
         let project = st.project.clone();
         let sel_idx = st.sel_idx.clone();
         let pending_xform = st.pending_xform.clone();
+        let pending_title = st.pending_title.clone();
         let active = ex.active.clone();
         let pending = ex.pending.clone();
         let rec = st.recorder(ui);
@@ -533,9 +539,10 @@ pub(super) fn wire(ui: &AppWindow, st: &super::VideoState, ex: &super::export::E
             if active.get() || pending.get() || ui.get_video_engine_down() {
                 return;
             }
-            // A transform still waiting for the preview tick would be applied,
-            // and recorded, after the undo.
+            // A transform or a title still waiting for the preview tick would
+            // be applied, and recorded, after the undo.
             pending_xform.borrow_mut().take();
+            pending_title.borrow_mut().take();
             apply_step(&ui, &project, &rec, &sel_idx, dir, &waves);
         };
         match dir {
@@ -960,6 +967,7 @@ mod tests {
             StepKind::Trim,
             StepKind::Transform,
             StepKind::Duration,
+            StepKind::Text,
         ] {
             let first = step(kind, "a", &c0, &c1);
             assert!(first.merges_with(&step(kind, "a", &c1, &c0)), "{kind:?}");
@@ -1212,6 +1220,7 @@ mod tests {
         assert_eq!(d(StepKind::AddTrack), "adding a track");
         assert_eq!(d(StepKind::Split), "splitting intro.mp4");
         assert_eq!(d(StepKind::Speed), "changing the speed of intro.mp4");
+        assert_eq!(d(StepKind::Text), "editing the text of intro.mp4");
     }
 
     /// A step about track `t`, going from one table to another, named as the
````

- [ ] **Step 2: Run the gates.** Expected: `cargo test -p kuvatin` 306 passed (`gestures_on_the_same_clip_merge_and_nothing_else_does` and `each_kind_describes_itself` now cover Text).

- [ ] **Step 3: Commit.** `Typing into a title is one undo step`

### Task 7: The Text chip adds a title, and the inspector edits it

The Text chip appends a five-second title to the top track, records an Add and selects it; it is disabled while the top track is locked, and refuses in Rust too. The inspector shows, above the sliders, a three-line `TextEdit`, a Size dropdown (8 to 400) and a Bold box that build the Pango description, six colour swatches, and two alignment toggles; any of them fires `title-changed`, which stashes the whole title in `pending_title`. Selecting a title fills the controls from the engine (`titles::show`). `insp-is-still` becomes `insp-free-duration`, set for a still or a title.

**Files:**
- Modify: `crates/kuvatin/src/gui/video/titles.rs` (font and alignment helpers, `show`, `from_inspector`, `wire`, tests)
- Modify: `crates/kuvatin/src/gui/video/mod.rs` (`titles::wire`)
- Modify: `crates/kuvatin/src/gui/video/timeline.rs` (the inspector on select)
- Modify: `crates/kuvatin/ui/app.slint` (properties, the title controls, the Text chip, `TextEdit` import)

- [ ] **Step 1: Apply the diff.**

````diff
diff --git a/crates/kuvatin/src/gui/video/mod.rs b/crates/kuvatin/src/gui/video/mod.rs
index 91b4bdf..33498e3 100644
--- a/crates/kuvatin/src/gui/video/mod.rs
+++ b/crates/kuvatin/src/gui/video/mod.rs
@@ -139,6 +139,7 @@ pub(super) fn wire(
 ) {
     import::wire(ui, st, im, timers);
     timeline::wire(ui, st);
+    titles::wire(ui, st);
     tracks::wire(ui, st);
     transport::wire(ui, st);
     export::wire(ui, st, ex, timers);
diff --git a/crates/kuvatin/src/gui/video/timeline.rs b/crates/kuvatin/src/gui/video/timeline.rs
index 8601305..9e5045c 100644
--- a/crates/kuvatin/src/gui/video/timeline.rs
+++ b/crates/kuvatin/src/gui/video/timeline.rs
@@ -51,8 +51,10 @@ pub(super) fn wire(ui: &AppWindow, st: &VideoState) {
             ui.set_inspector_name(name);
             // Only real videos carry audio — stills and image sequences don't.
             ui.set_insp_has_audio(sel_kind == ClipKind::Video);
-            // Stills get a free Duration field; real media is trimmed instead.
-            ui.set_insp_is_still(sel_kind == ClipKind::Image);
+            // Stills and titles get a free Duration field; real media is
+            // trimmed instead.
+            ui.set_insp_free_duration(matches!(sel_kind, ClipKind::Image | ClipKind::Title));
+            ui.set_insp_is_title(sel_kind == ClipKind::Title);
             ui.set_insp_duration_s(sel_dur.round().max(1.0) as i32);
             // Speed is for clips with source time to stretch.
             ui.set_insp_has_rate(matches!(sel_kind, ClipKind::Video | ClipKind::Sequence));
@@ -71,6 +73,9 @@ pub(super) fn wire(ui: &AppWindow, st: &VideoState) {
                         ui.set_insp_volume((l.volume as f32 * 100.0).clamp(0.0, 100.0));
                     }
                     ui.set_insp_rate_index(speed_index(p.clip_rate(&cid)));
+                    if let Some(title) = p.title_of(&cid) {
+                        super::titles::show(&ui, &title);
+                    }
                     // Fit size drives the preview bounding box dimensions.
                     let (fw, fh) = p.clip_fit_size(&cid).unwrap_or((
                         kuvatin_video::CANVAS_W as u32,
diff --git a/crates/kuvatin/src/gui/video/titles.rs b/crates/kuvatin/src/gui/video/titles.rs
index eca8796..c5bc9c3 100644
--- a/crates/kuvatin/src/gui/video/titles.rs
+++ b/crates/kuvatin/src/gui/video/titles.rs
@@ -1,7 +1,105 @@
-//! Text overlays as the interface shows them.
+//! Text overlays as the inspector shows them: a size and a bold switch
+//! instead of a Pango font description, and alignment as an index. Also the
+//! Text chip, which puts one on the timeline.
 
-use crate::gui::{AppWindow, TimelineClip};
-use slint::{Model, VecModel};
+use super::{tracks, undo, ClipKind};
+use crate::gui::{show_error, AppWindow, TimelineClip};
+use kuvatin_video::{TitleHAlign, TitleRecord, TitleVAlign};
+use slint::{ComponentHandle, Image, Model, VecModel};
+
+/// How long a new title lasts.
+const NEW_TITLE: std::time::Duration = std::time::Duration::from_secs(5);
+
+/// The one family a title is set in. A fixed family cannot fail to resolve on
+/// another machine; the record keeps the whole description, so a later
+/// version can offer more without changing the file.
+const FAMILY: &str = "Sans";
+
+/// The sizes the inspector offers, in points.
+const MIN_SIZE: i32 = 8;
+const MAX_SIZE: i32 = 400;
+
+/// The size a description without one is drawn at.
+const DEFAULT_SIZE: i32 = 48;
+
+/// The Pango description for a size and weight: "Sans Bold 48", "Sans 48".
+pub(super) fn font_desc(size: i32, bold: bool) -> String {
+    let size = size.clamp(MIN_SIZE, MAX_SIZE);
+    if bold {
+        format!("{FAMILY} Bold {size}")
+    } else {
+        format!("{FAMILY} {size}")
+    }
+}
+
+/// The size and weight in a Pango description, as the inspector shows them:
+/// the trailing number (48 if there is none) and whether any word is "Bold".
+pub(super) fn font_parts(desc: &str) -> (i32, bool) {
+    let words: Vec<&str> = desc.split_whitespace().collect();
+    let size = words
+        .last()
+        .and_then(|w| w.parse::<f64>().ok())
+        .map_or(DEFAULT_SIZE, |s| s.round() as i32)
+        .clamp(MIN_SIZE, MAX_SIZE);
+    let bold = words.iter().any(|w| w.eq_ignore_ascii_case("bold"));
+    (size, bold)
+}
+
+/// Horizontal alignment as the inspector's toggle index: left, centre, right.
+pub(super) fn halign_index(a: TitleHAlign) -> i32 {
+    match a {
+        TitleHAlign::Left => 0,
+        TitleHAlign::Center => 1,
+        TitleHAlign::Right => 2,
+    }
+}
+
+pub(super) fn halign_at(i: i32) -> TitleHAlign {
+    match i {
+        0 => TitleHAlign::Left,
+        2 => TitleHAlign::Right,
+        _ => TitleHAlign::Center,
+    }
+}
+
+/// Vertical alignment as the inspector's toggle index: top, middle, bottom.
+pub(super) fn valign_index(a: TitleVAlign) -> i32 {
+    match a {
+        TitleVAlign::Top => 0,
+        TitleVAlign::Center => 1,
+        TitleVAlign::Bottom => 2,
+    }
+}
+
+pub(super) fn valign_at(i: i32) -> TitleVAlign {
+    match i {
+        0 => TitleVAlign::Top,
+        2 => TitleVAlign::Bottom,
+        _ => TitleVAlign::Center,
+    }
+}
+
+/// Show a title's text and style in the inspector.
+pub(super) fn show(ui: &AppWindow, title: &TitleRecord) {
+    let (size, bold) = font_parts(&title.font);
+    ui.set_insp_text(title.text.as_str().into());
+    ui.set_insp_font_size(size);
+    ui.set_insp_font_bold(bold);
+    ui.set_insp_text_color(title.color.as_str().into());
+    ui.set_insp_halign(halign_index(title.halign));
+    ui.set_insp_valign(valign_index(title.valign));
+}
+
+/// The title the inspector's controls describe.
+fn from_inspector(ui: &AppWindow) -> TitleRecord {
+    TitleRecord {
+        text: ui.get_insp_text().into(),
+        font: font_desc(ui.get_insp_font_size(), ui.get_insp_font_bold()),
+        color: ui.get_insp_text_color().into(),
+        halign: halign_at(ui.get_insp_halign()),
+        valign: valign_at(ui.get_insp_valign()),
+    }
+}
 
 /// Give a title's row the name its text now gives it, and the inspector's
 /// heading too when the title is the selected clip.
@@ -20,3 +118,127 @@ pub(super) fn rename_row(ui: &AppWindow, rows: &VecModel<TimelineClip>, id: &str
         }
     }
 }
+
+/// Wire the Text chip and the inspector's title controls.
+pub(super) fn wire(ui: &AppWindow, st: &super::VideoState) {
+    // Text chip: a five-second title at the end of the top track, which
+    // composites over everything, selected so it can be typed into at once.
+    {
+        let ui_weak = ui.as_weak();
+        let project_slot = st.project.clone();
+        let tl_clips = st.tl_clips.clone();
+        let rec = st.recorder(ui);
+        ui.on_timeline_add_text(move || {
+            let Some(ui) = ui_weak.upgrade() else {
+                return;
+            };
+            if tracks::refuse_locked(&ui, &tracks::rows_of(&rec.tracks), &[0]) {
+                return;
+            }
+            if project_slot.borrow().is_none() {
+                *project_slot.borrow_mut() = super::make_project(&ui_weak);
+            }
+            let mut slot = project_slot.borrow_mut();
+            let Some(project) = slot.as_mut() else {
+                return;
+            };
+            tracks::push_mutes(project, &rec.tracks);
+            let before = rec.before(Some(&*project));
+            let title = TitleRecord::default();
+            let info = match project.append_title_clip(&title, 0, NEW_TITLE) {
+                Ok(info) => info,
+                Err(e) => {
+                    drop(slot);
+                    show_error(&ui, "Could not add text", format!("{e:#}"));
+                    return;
+                }
+            };
+            tl_clips.push(TimelineClip {
+                id: info.id.0.as_str().into(),
+                track: info.track as i32,
+                start: info.start.as_secs_f32(),
+                duration: info.duration.as_secs_f32(),
+                inpoint: 0.0,
+                name: title.name().into(),
+                kind: ClipKind::Title,
+                selected: false,
+                thumb: Image::default(),
+                rate: 1.0,
+                wave: Image::default(),
+                wave_secs: 0.0,
+            });
+            rec.record(
+                Some(&*project),
+                undo::StepKind::Add,
+                Some(undo::Subject::Clip(info.id.0.clone())),
+                before,
+            );
+            if let Some(d) = project.duration() {
+                ui.set_timeline_duration(d.as_secs_f32());
+            }
+            project.refresh_preview();
+            drop(slot);
+            ui.invoke_timeline_select(tl_clips.row_count() as i32 - 1);
+        });
+    }
+
+    // Any title control edited: stash the whole title; the UI tick writes it
+    // and records one Text step (see `pending_title`).
+    {
+        let ui_weak = ui.as_weak();
+        let tl_clips = st.tl_clips.clone();
+        let sel_idx = st.sel_idx.clone();
+        let pending_title = st.pending_title.clone();
+        ui.on_title_changed(move || {
+            let Some(ui) = ui_weak.upgrade() else {
+                return;
+            };
+            let Some(row) = usize::try_from(sel_idx.get())
+                .ok()
+                .and_then(|i| tl_clips.row_data(i))
+            else {
+                return;
+            };
+            if row.kind != ClipKind::Title {
+                return;
+            }
+            *pending_title.borrow_mut() = Some((row.id.to_string(), from_inspector(&ui)));
+        });
+    }
+}
+
+#[cfg(test)]
+mod tests {
+    use super::*;
+
+    #[test]
+    fn title_fonts_are_sans_at_a_size_bold_or_not() {
+        assert_eq!(font_desc(48, true), "Sans Bold 48");
+        assert_eq!(font_desc(12, false), "Sans 12");
+        assert_eq!(font_desc(2, false), "Sans 8", "clamped to the smallest");
+        assert_eq!(font_desc(900, true), "Sans Bold 400");
+    }
+
+    /// What Pango hands back, and a description a later version may write,
+    /// both read as a size and a weight.
+    #[test]
+    fn title_fonts_read_back_as_a_size_and_a_weight() {
+        assert_eq!(font_parts("Sans Bold 48"), (48, true));
+        assert_eq!(font_parts("Sans 12"), (12, false));
+        assert_eq!(font_parts("Serif Italic Bold 30.5"), (31, true));
+        assert_eq!(font_parts("Sans"), (48, false), "no size: the default");
+        assert_eq!(font_parts(""), (48, false));
+        assert_eq!(font_parts("Sans 1000"), (400, false));
+    }
+
+    #[test]
+    fn title_alignments_round_trip_through_their_indices() {
+        for a in [TitleHAlign::Left, TitleHAlign::Center, TitleHAlign::Right] {
+            assert_eq!(halign_at(halign_index(a)), a);
+        }
+        for a in [TitleVAlign::Top, TitleVAlign::Center, TitleVAlign::Bottom] {
+            assert_eq!(valign_at(valign_index(a)), a);
+        }
+        assert_eq!(halign_at(7), TitleHAlign::Center, "out of range: centre");
+    }
+}
diff --git a/crates/kuvatin/ui/app.slint b/crates/kuvatin/ui/app.slint
index 2f3c1d7..457fe35 100644
--- a/crates/kuvatin/ui/app.slint
+++ b/crates/kuvatin/ui/app.slint
@@ -1,4 +1,4 @@
-import { ComboBox, ListView, LineEdit, SpinBox, CheckBox, ScrollView } from "std-widgets.slint";
+import { ComboBox, ListView, LineEdit, SpinBox, CheckBox, ScrollView, TextEdit } from "std-widgets.slint";
 import { Theme } from "theme.slint";
 import { Modal } from "modal.slint";
 import {
@@ -246,7 +246,20 @@ export component AppWindow inherits Window {
     callback inspector-changed();              // any inspector slider moved
     callback inspector-duration-changed(int);  // Duration field (stills), whole seconds
     in-out property <int> insp-duration-s: 5;  // selected clip's length, whole seconds
-    in property <bool> insp-is-still: false;   // selected clip is a still image (duration is free)
+    // The selected clip's length is a free choice: a still or a title, which
+    // have no source length. A real clip's is set by trimming.
+    in property <bool> insp-free-duration: false;
+    // A title (text overlay) is selected: its text and style, edited here.
+    // gui/video/titles.rs turns them into a TitleRecord and back.
+    in property <bool> insp-is-title: false;
+    in-out property <string> insp-text: "";
+    in-out property <int> insp-font-size: 48;
+    in-out property <bool> insp-font-bold: true;
+    in-out property <string> insp-text-color: "#ffffff";
+    in-out property <int> insp-halign: 1;      // 0 left, 1 centre, 2 right
+    in-out property <int> insp-valign: 1;      // 0 top, 1 middle, 2 bottom
+    callback title-changed();                  // any of the above edited
+    callback timeline-add-text();              // the Text chip: a title at the end of the top track
     // Speed, for videos and image sequences. The labels come from SPEEDS in
     // gui/video/mod.rs; the index is the entry showing.
     in property <bool> insp-has-rate: false;
@@ -1746,6 +1759,79 @@ export component AppWindow inherits Window {
                                 text: "On a locked track. Unlock it to change this clip.";
                                 color: Theme.hint; font-size: 10px; wrap: word-wrap;
                             }
+                            // A title's own controls, above the sliders every clip has.
+                            if root.inspector-name != "" && root.insp-is-title : VerticalLayout {
+                                vertical-stretch: 0;
+                                spacing: 6px;
+                                Text { text: "Text"; color: Theme.muted2; font-size: 9.5px; font-weight: 600; }
+                                TextEdit {
+                                    enabled: !root.insp-locked;
+                                    height: 58px;
+                                    font-size: 11px;
+                                    wrap: word-wrap;
+                                    text <=> root.insp-text;
+                                    accessible-label: "Title text";
+                                    edited(t) => { root.title-changed(); }
+                                }
+                                HorizontalLayout {
+                                    spacing: 8px;
+                                    NumberDropdown {
+                                        label: "Size"; value: root.insp-font-size; minimum: 8; maximum: 400;
+                                        edited(v) => {
+                                            if (!root.insp-locked) { root.insp-font-size = v; root.title-changed(); }
+                                        }
+                                    }
+                                    CheckBox {
+                                        text: "Bold";
+                                        enabled: !root.insp-locked;
+                                        checked <=> root.insp-font-bold;
+                                        toggled => { root.title-changed(); }
+                                    }
+                                    Rectangle { horizontal-stretch: 1; }
+                                }
+                                HorizontalLayout {
+                                    spacing: 6px;
+                                    for sw in [
+                                        { c: #ffffff, s: "#ffffff", n: "White" },
+                                        { c: #000000, s: "#000000", n: "Black" },
+                                        { c: #ffd34d, s: "#ffd34d", n: "Yellow" },
+                                        { c: #ff5f5f, s: "#ff5f5f", n: "Red" },
+                                        { c: #4dd2ff, s: "#4dd2ff", n: "Blue" },
+                                        { c: Theme.accent, s: "#2dd4bf", n: "Teal" },
+                                    ] : Rectangle {
+                                        width: 18px; height: 18px; border-radius: 9px;
+                                        background: sw.c;
+                                        border-width: root.insp-text-color == sw.s ? 2px : 1px;
+                                        border-color: root.insp-text-color == sw.s ? Theme.ink : Theme.line2;
+                                        accessible-role: button;
+                                        accessible-label: sw.n + " text";
+                                        accessible-checkable: true;
+                                        accessible-checked: root.insp-text-color == sw.s;
+                                        TouchArea {
+                                            enabled: !root.insp-locked;
+                                            mouse-cursor: pointer;
+                                            clicked => { root.insp-text-color = sw.s; root.title-changed(); }
+                                        }
+                                    }
+                                    Rectangle { horizontal-stretch: 1; }
+                                }
+                                SegToggle {
+                                    height: 24px;
+                                    labels: ["Left", "Centre", "Right"];
+                                    selected: root.insp-halign;
+                                    select(i) => {
+                                        if (!root.insp-locked) { root.insp-halign = i; root.title-changed(); }
+                                    }
+                                }
+                                SegToggle {
+                                    height: 24px;
+                                    labels: ["Top", "Middle", "Bottom"];
+                                    selected: root.insp-valign;
+                                    select(i) => {
+                                        if (!root.insp-locked) { root.insp-valign = i; root.title-changed(); }
+                                    }
+                                }
+                            }
                             if root.inspector-name != "" : VerticalLayout {
                                 // The spare height below the controls belongs to the filler
                                 // after this layout. Without this the layout took a share of
@@ -1775,9 +1861,9 @@ export component AppWindow inherits Window {
                                         selected(t) => { root.inspector-speed-changed(self.current-index); }
                                     }
                                 }
-                                // A still has no source length, so its duration is a free
-                                // choice; a real clip's is set by trimming on the timeline.
-                                if root.insp-is-still : VerticalLayout {
+                                // A still or a title has no source length, so its duration
+                                // is a free choice; a real clip's is set by trimming.
+                                if root.insp-free-duration : VerticalLayout {
                                     spacing: 4px;
                                     HorizontalLayout {
                                         Text { text: "Duration"; color: Theme.muted2; font-size: 9.5px; font-weight: 600; vertical-alignment: center; }
@@ -1841,6 +1927,16 @@ export component AppWindow inherits Window {
                                     focus-on-click: false;
                                     clicked => { root.timeline-split(); if (!split-chip.has-focus) { kbd.focus(); } }
                                 }
+                                text-chip := TimelineChip {
+                                    label: "Text";
+                                    wide: true;
+                                    hint: root.timeline-tracks[0].locked
+                                        ? "The top track is locked"
+                                        : "Add a text overlay to the timeline";
+                                    enabled: !root.timeline-tracks[0].locked && !root.modal-open() && !root.video-engine-down;
+                                    focus-on-click: false;
+                                    clicked => { root.timeline-add-text(); if (!text-chip.has-focus) { kbd.focus(); } }
+                                }
                                 // Zoom: discoverable buttons for the wheel gesture, and the
                                 // only way to reach it without a wheel.
                                 TimelineChip {
````

- [ ] **Step 2: Run the gates.** Expected: `cargo test -p kuvatin` 309 passed.

- [ ] **Step 3: Commit.** `The Text chip adds a title, and the inspector edits it`

### Task 8: Dissolves are drawn on the timeline, and a chip makes or removes one

`overlaps` finds where neighbours on a track overlap; the lane draws a band over each, after the clip blocks and with no `TouchArea`, so drags and trims still reach the clips. `dissolve_slide` gives the slide that makes a `want`-second dissolve with the clip before the selected one, or takes it away. The Dissolve chip reads "Dissolve" or "Remove dissolve" and slides through `timeline-clip-dropped`, so the engine's rule bounds it and it is a Move. `show_dissolves` runs on the tick (departure 4). A wide `TimelineChip` grows with its label (departure 10).

**Files:**
- Modify: `crates/kuvatin/src/gui/video/timeline.rs` (`overlaps`, `previous_on_track`, `dissolve_slide`, `dissolve_of`, `show_dissolves`, the chip's handler, tests)
- Modify: `crates/kuvatin/src/gui/video/mod.rs` (`show_dissolves` on the tick)
- Modify: `crates/kuvatin/ui/app.slint` (`TimelineOverlap`, the properties, the chip, the bands)
- Modify: `crates/kuvatin/ui/widgets.slint` (`TimelineChip` width)

- [ ] **Step 1: Apply the diff.**

````diff
diff --git a/crates/kuvatin/src/gui/video/mod.rs b/crates/kuvatin/src/gui/video/mod.rs
index 33498e3..af10802 100644
--- a/crates/kuvatin/src/gui/video/mod.rs
+++ b/crates/kuvatin/src/gui/video/mod.rs
@@ -355,6 +355,7 @@ pub(super) fn wire(
                 if ui.get_app_mode() != 1 {
                     return;
                 }
+                timeline::show_dissolves(&ui, &rec.tl_clips);
                 let mut slot = project_slot.borrow_mut();
                 let Some(project) = slot.as_mut() else {
                     return;
diff --git a/crates/kuvatin/src/gui/video/timeline.rs b/crates/kuvatin/src/gui/video/timeline.rs
index 9e5045c..b6113cd 100644
--- a/crates/kuvatin/src/gui/video/timeline.rs
+++ b/crates/kuvatin/src/gui/video/timeline.rs
@@ -4,8 +4,8 @@
 use super::tracks;
 use super::undo::{Recorder, StepKind, Subject};
 use super::{VideoState, MAX_SCALE_PCT, MIN_SCALE_PCT, SPEEDS};
-use crate::gui::{show_error, AppWindow, ClipKind, TimelineClip, TimelineTrack};
-use slint::{ComponentHandle, Model, SharedString, VecModel};
+use crate::gui::{show_error, AppWindow, ClipKind, TimelineClip, TimelineOverlap, TimelineTrack};
+use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
 use std::cell::RefCell;
 use std::rc::Rc;
 
@@ -186,6 +186,31 @@ pub(super) fn wire(ui: &AppWindow, st: &VideoState) {
         });
     }
 
+    // Dissolve chip: slide the selected clip so it overlaps the clip before
+    // it by the default length, or so the two only meet. Through the drop,
+    // so the engine's rule bounds it and it is a Move like any drag.
+    {
+        let ui_weak = ui_weak.clone();
+        let tl_clips = tl_clips.clone();
+        ui.on_timeline_dissolve(move || {
+            let Some(ui) = ui_weak.upgrade() else {
+                return;
+            };
+            let Ok(sel) = usize::try_from(ui.get_timeline_selected()) else {
+                return;
+            };
+            let rows: Vec<TimelineClip> = tl_clips.iter().collect();
+            let want = match dissolve_of(&rows, sel) {
+                Some(d) if d > 0.0 => 0.0,
+                Some(_) => DISSOLVE_SECS,
+                None => return,
+            };
+            if let Some((_, delta)) = dissolve_slide(&rows, sel, want) {
+                ui.invoke_timeline_clip_dropped(sel as i32, delta, 0);
+            }
+        });
+    }
+
     // Magnetic snap: given the dragged clip's proposed slide (seconds), nudge
     // its nearest edge onto a neighbouring clip edge or the timeline start when
     // within ~8px. Pure/read-only — it drives the live drag binding AND the drop
@@ -644,6 +669,90 @@ fn drop_target_track(
     (current + delta_rows).clamp(0, count)
 }
 
+/// How long a dissolve the Dissolve chip makes, in seconds.
+const DISSOLVE_SECS: f32 = 1.0;
+
+/// Shorter than this, two clips are taken to meet rather than overlap:
+/// positions come through f32 seconds.
+const OVERLAP_EPS: f32 = 1e-3;
+
+/// Where clips on the same track overlap, in track and start order. Only
+/// neighbours on a track can: the engine never lets a clip reach past the
+/// one beside it.
+pub(super) fn overlaps(rows: &[TimelineClip]) -> Vec<TimelineOverlap> {
+    let mut sorted: Vec<&TimelineClip> = rows.iter().collect();
+    sorted.sort_by(|a, b| a.track.cmp(&b.track).then(a.start.total_cmp(&b.start)));
+    sorted
+        .windows(2)
+        .filter(|w| w[0].track == w[1].track)
+        .filter_map(|w| {
+            let end = (w[0].start + w[0].duration).min(w[1].start + w[1].duration);
+            (end - w[1].start > OVERLAP_EPS).then(|| TimelineOverlap {
+                track: w[1].track,
+                start: w[1].start,
+                duration: end - w[1].start,
+            })
+        })
+        .collect()
+}
+
+/// The row before row `sel` on its own track: the last one starting earlier.
+fn previous_on_track(rows: &[TimelineClip], sel: usize) -> Option<usize> {
+    let s = rows.get(sel)?;
+    rows.iter()
+        .enumerate()
+        .filter(|(i, r)| *i != sel && r.track == s.track && r.start < s.start)
+        .max_by(|(_, a), (_, b)| a.start.total_cmp(&b.start))
+        .map(|(i, _)| i)
+}
+
+/// The clip before `sel` on its own track and the slide that would give them
+/// a `want` second dissolve: negative to make one, positive to take one away.
+/// None when nothing precedes it on the track, or when the clip before it is
+/// too short to give up `want` and still keep 0.2 s of its own.
+fn dissolve_slide(rows: &[TimelineClip], sel: usize, want: f32) -> Option<(usize, f32)> {
+    let p = previous_on_track(rows, sel)?;
+    let (prev, s) = (&rows[p], &rows[sel]);
+    if prev.duration < want + 0.2 {
+        return None;
+    }
+    Some((p, prev.start + prev.duration - want - s.start))
+}
+
+/// How far the selected clip dissolves from the one before it, when there
+/// is one before it: 0 when they only meet or a gap parts them.
+fn dissolve_of(rows: &[TimelineClip], sel: usize) -> Option<f32> {
+    let p = previous_on_track(rows, sel)?;
+    let overlap = rows[p].start + rows[p].duration - rows[sel].start;
+    Some(if overlap > OVERLAP_EPS { overlap } else { 0.0 })
+}
+
+/// Show the cross-dissolves on the timeline and what the Dissolve chip would
+/// do for the selected clip. The UI tick calls this, so no edit, undo or
+/// reopen can leave a stale wedge; it only touches the window when
+/// something changed.
+pub(super) fn show_dissolves(ui: &AppWindow, rows: &VecModel<TimelineClip>) {
+    let rows: Vec<TimelineClip> = rows.iter().collect();
+    let now = overlaps(&rows);
+    let shown = ui.get_timeline_overlaps();
+    if shown.row_count() != now.len() || shown.iter().zip(&now).any(|(a, b)| a != *b) {
+        ui.set_timeline_overlaps(ModelRc::new(VecModel::from(now)));
+    }
+    let dissolve = usize::try_from(ui.get_timeline_selected())
+        .ok()
+        .and_then(|sel| dissolve_of(&rows, sel));
+    let hint = match dissolve {
+        None => "Select a clip with another before it on its track".to_string(),
+        Some(d) if d > 0.0 => format!("Remove the {d:.1} s dissolve with the clip before it"),
+        Some(_) => format!("Cross-dissolve with the clip before it ({DISSOLVE_SECS:.1} s)"),
+    };
+    ui.set_dissolve_possible(dissolve.is_some());
+    ui.set_dissolve_present(dissolve.is_some_and(|d| d > 0.0));
+    if ui.get_dissolve_hint() != hint.as_str() {
+        ui.set_dissolve_hint(hint.into());
+    }
+}
+
 /// Which row is selected after row `removed` is deleted: none if it was the
 /// selected one, one fewer if the selection sat after it (those rows move up),
 /// otherwise the same.
@@ -845,4 +954,83 @@ mod tests {
             );
         }
     }
+
+    // ---- cross-dissolves ----------------------------------------------------
+
+    fn clip_at(track: i32, start: f32, duration: f32) -> TimelineClip {
+        TimelineClip {
+            track,
+            start,
+            duration,
+            ..Default::default()
+        }
+    }
+
+    #[test]
+    fn dissolve_wedges_sit_where_clips_on_a_track_overlap() {
+        let rows = [clip_at(0, 3.0, 4.0), clip_at(0, 0.0, 4.0)];
+        let got = overlaps(&rows);
+        assert_eq!(got.len(), 1);
+        assert_eq!((got[0].track, got[0].start), (0, 3.0));
+        assert!(near(got[0].duration, 1.0));
+    }
+
+    #[test]
+    fn dissolve_wedges_need_the_same_track_and_a_real_overlap() {
+        assert!(overlaps(&[clip_at(0, 0.0, 4.0), clip_at(1, 3.0, 4.0)]).is_empty());
+        assert!(
+            overlaps(&[clip_at(0, 0.0, 4.0), clip_at(0, 4.0, 2.0)]).is_empty(),
+            "butted up: no dissolve"
+        );
+    }
+
+    #[test]
+    fn dissolve_wedges_come_in_track_and_start_order() {
+        let rows = [
+            clip_at(1, 0.0, 3.0),
+            clip_at(0, 5.0, 3.0),
+            clip_at(0, 0.0, 3.0),
+            clip_at(0, 2.5, 3.0),
+            clip_at(1, 2.0, 3.0),
+        ];
+        let got: Vec<(i32, f32)> = overlaps(&rows).iter().map(|o| (o.track, o.start)).collect();
+        assert_eq!(got, vec![(0, 2.5), (0, 5.0), (1, 2.0)]);
+    }
+
+    #[test]
+    fn dissolve_slide_makes_one_of_the_length_asked() {
+        // [0,4) then [5,8): slide 2 s left to overlap by 1 s.
+        let rows = [clip_at(0, 0.0, 4.0), clip_at(0, 5.0, 3.0)];
+        let (p, delta) = dissolve_slide(&rows, 1, 1.0).expect("a clip before it");
+        assert_eq!(p, 0);
+        assert!(near(delta, -2.0), "{delta}");
+    }
+
+    #[test]
+    fn dissolve_slide_takes_one_away() {
+        let rows = [clip_at(0, 0.0, 4.0), clip_at(0, 2.8, 3.0)];
+        let (_, delta) = dissolve_slide(&rows, 1, 0.0).expect("a clip before it");
+        assert!(near(delta, 1.2), "{delta}");
+        assert_eq!(
+            dissolve_of(&rows, 1).map(|d| (d * 10.0).round()),
+            Some(12.0)
+        );
+    }
+
+    #[test]
+    fn dissolve_slide_needs_a_long_enough_clip_before_it() {
+        let alone = [clip_at(0, 5.0, 3.0), clip_at(1, 0.0, 4.0)];
+        assert_eq!(
+            dissolve_slide(&alone, 0, 1.0),
+            None,
+            "nothing before it on its track"
+        );
+        let short = [clip_at(0, 0.0, 1.1), clip_at(0, 2.0, 3.0)];
+        assert_eq!(
+            dissolve_slide(&short, 1, 1.0),
+            None,
+            "1.1 s cannot give up 1.0 s"
+        );
+        assert!(dissolve_slide(&short, 1, 0.5).is_some());
+    }
 }
diff --git a/crates/kuvatin/ui/app.slint b/crates/kuvatin/ui/app.slint
index 457fe35..0844a30 100644
--- a/crates/kuvatin/ui/app.slint
+++ b/crates/kuvatin/ui/app.slint
@@ -20,6 +20,14 @@ export struct FileRow {
 // What a timeline clip is (drives its colour and whether it has audio).
 export enum ClipKind { video, image, sequence, title }
 
+// Where two clips on one track overlap: GES cross-dissolves between them.
+// Drawn over both blocks; it changes only by moving or trimming them.
+export struct TimelineOverlap {
+    track: int,
+    start: float,      // seconds
+    duration: float,   // seconds
+}
+
 // A clip placed on the video timeline. Positions are in seconds; the UI maps
 // them to pixels via the pixels-per-second zoom.
 export struct TimelineClip {
@@ -260,6 +268,13 @@ export component AppWindow inherits Window {
     in-out property <int> insp-valign: 1;      // 0 top, 1 middle, 2 bottom
     callback title-changed();                  // any of the above edited
     callback timeline-add-text();              // the Text chip: a title at the end of the top track
+    // The cross-dissolves on the timeline, and the Dissolve chip's state for
+    // the selected clip (gui/video/timeline.rs, `show_dissolves`).
+    in property <[TimelineOverlap]> timeline-overlaps;
+    in property <bool> dissolve-possible: false;   // the selected clip has one before it on its track
+    in property <bool> dissolve-present: false;    // and already dissolves from it
+    in property <string> dissolve-hint: "";
+    callback timeline-dissolve();                  // make the dissolve, or take it away
     // Speed, for videos and image sequences. The labels come from SPEEDS in
     // gui/video/mod.rs; the index is the entry showing.
     in property <bool> insp-has-rate: false;
@@ -1927,6 +1942,14 @@ export component AppWindow inherits Window {
                                     focus-on-click: false;
                                     clicked => { root.timeline-split(); if (!split-chip.has-focus) { kbd.focus(); } }
                                 }
+                                dissolve-chip := TimelineChip {
+                                    label: root.dissolve-present ? "Remove dissolve" : "Dissolve";
+                                    wide: true;
+                                    hint: root.dissolve-hint;
+                                    enabled: root.dissolve-possible && !root.insp-locked && !root.modal-open() && !root.video-engine-down;
+                                    focus-on-click: false;
+                                    clicked => { root.timeline-dissolve(); if (!dissolve-chip.has-focus) { kbd.focus(); } }
+                                }
                                 text-chip := TimelineChip {
                                     label: "Text";
                                     wide: true;
@@ -2358,6 +2381,21 @@ export component AppWindow inherits Window {
                                         }
                                     }
 
+                                    // Cross-dissolves, over the two blocks they join. No
+                                    // TouchArea: a drag or a trim on either clip goes to the
+                                    // clip beneath, as if this were not here.
+                                    for ov in root.timeline-overlaps : Rectangle {
+                                        x: (ov.start * root.timeline-pps) * 1px - lane.scroll;
+                                        y: ov.track * root.track-h + 4px;
+                                        width: (ov.duration * root.timeline-pps) * 1px;
+                                        height: 22px;
+                                        background: @linear-gradient(90deg, #ffffff00 0%, #ffffff38 50%, #ffffff00 100%);
+                                        accessible-role: text;
+                                        accessible-label: "Cross-dissolve, " + Math.round(ov.duration * 10) / 10 + " s, track " + (ov.track + 1);
+                                        Rectangle { y: 0; height: 1px; background: #ffffff66; }
+                                        Rectangle { y: parent.height - 1px; height: 1px; background: #ffffff66; }
+                                    }
+
                                     // playhead — the 90ms linear animate interpolates the
                                     // ~100ms position poll into a continuous glide.
                                     Rectangle {
diff --git a/crates/kuvatin/ui/widgets.slint b/crates/kuvatin/ui/widgets.slint
index fa15824..0c756eb 100644
--- a/crates/kuvatin/ui/widgets.slint
+++ b/crates/kuvatin/ui/widgets.slint
@@ -663,7 +663,8 @@ export component TimelineChip inherits Rectangle {
     in property <bool> focus-on-click: true;
     out property <bool> has-focus: fs.has-focus;
     callback clicked();
-    width: root.wide ? 34px : 22px;
+    // A wide chip grows with its label ("Remove dissolve"), never below 34px.
+    width: root.wide ? max(34px, chip-label.preferred-width + 12px) : 22px;
     height: 22px;
     border-radius: 5px;
     background: root.enabled && ta.has-hover ? Theme.hover : #1b1f27;
@@ -683,7 +684,7 @@ export component TimelineChip inherits Rectangle {
             Tooltip.text = root.hint;
         }
     }
-    Text {
+    chip-label := Text {
         text: root.label;
         color: root.enabled ? Theme.ink2 : Theme.disabled-ink;
         font-size: 10px;
````

- [ ] **Step 2: Run the gates.** Expected: `cargo test -p kuvatin` 315 passed.

- [ ] **Step 3: Commit.** `Dissolves are drawn on the timeline, and a chip makes or removes one`

### Task 9: The gate, the words, the hand checks, and the full suite

**Files:**
- Modify: `.github/workflows/release.yml` (`title_ dissolve_` join the self-contained engine filter)
- Modify: `CHANGELOG.md` (Unreleased: cross-dissolves, text; Changed: old overlapping projects now dissolve)
- Modify: `README.md` (the keys paragraph, the feature list, the Deferred line)

- [ ] **Step 1: Apply the diff.**

````diff
diff --git a/.github/workflows/release.yml b/.github/workflows/release.yml
index e1e1b62..8b33d33 100644
--- a/.github/workflows/release.yml
+++ b/.github/workflows/release.yml
@@ -253,7 +253,7 @@ jobs:
             slid neighbour confined_to_the_gap already_overlapping discovery_gives_up `
             without_blocking_the_caller document:: survives_being_saved missing_source encoder undo_ `
             removing_a_clip_straight_after_adding_it_does_not_crash removing_a_clip_added_while_playing_does_not_crash `
-            removing_two_clips_back_to_back_while_playing_does_not_crash a_zoomed_clip_keeps_its_scale split_ frame_length shuttle_ speed_ trim_math_follows_the_rate unsaved_work waveform_ track_mute_
+            removing_two_clips_back_to_back_while_playing_does_not_crash a_zoomed_clip_keeps_its_scale split_ frame_length shuttle_ speed_ trim_math_follows_the_rate unsaved_work waveform_ track_mute_ title_ dissolve_
 
       # Fixtures for everything that needs real media. They live under the
       # workspace (a clean long path — the runner's %TEMP% is an 8.3 short path
diff --git a/CHANGELOG.md b/CHANGELOG.md
index 04bc1de..76f23db 100644
--- a/CHANGELOG.md
+++ b/CHANGELOG.md
@@ -26,6 +26,23 @@ something is fixed.
   solo is neither. A project with named, muted or locked tracks opens in 2.13
   and earlier with its clips as they were and every track unnamed, audible and
   unlocked.
+- **Cross-dissolves.** Drag a clip onto the one before it on its track and the
+  two dissolve into each other for as long as they overlap; a band over the
+  overlap shows it. The **Dissolve** chip makes a one-second dissolve with the
+  clip before the selected one, or takes it away again. A clip still cannot
+  cover another completely, pass the clip beside it, or reach the one beyond,
+  and trimming an edge now follows the same rule.
+- **Text.** The **Text** chip puts a five-second title on the top track. Type
+  it in the inspector, over as many lines as you like, and pick its size,
+  weight, colour and alignment; move, scale and fade it like any other clip.
+  Typing a sentence is one undo step. A project with text in it needs this
+  version to open: 2.13 and earlier refuse it by name rather than open it
+  without the text. A project without text opens in them as before.
+
+### Changed
+- A project from before this version that has clips overlapping on a track
+  now plays a cross-dissolve where they overlap, instead of one clip hiding
+  the other.
 
 ## [2.13.0] - 2026-09-23
 The video editor learns to cut. The S key splits a clip at the playhead,
diff --git a/README.md b/README.md
index 7c27e59..782abd2 100644
--- a/README.md
+++ b/README.md
@@ -62,7 +62,9 @@ goes no faster than normal: GStreamer cannot play faster through a speed
 change. **Left/Right** walk the clips on the timeline,
 **Ctrl+Left/Right** slide the selected clip by a tenth of a second,
 **Ctrl+Up/Down** move it to another track, **Shift+Left/Right** trim its right
-edge, **S** splits it at the playhead, and **Delete** removes it.
+edge, **S** splits it at the playhead, and **Delete** removes it. The
+**Dissolve** chip cross-dissolves the selected clip with the one before it,
+and **Text** adds a title.
 Double-click a track's name to rename it; the M, S and L buttons beside it
 mute, solo and lock the track. **Ctrl+S**
 saves the project (to the file it came from, or asks the first time) and
@@ -87,6 +89,11 @@ lane; a plain wheel scrolls it.
   edge-trim, split at the playhead, and move clips across tracks with magnetic
   snapping; waveforms on video clips; reorder tracks; name, mute, solo and
   lock them; drop below the last track to create a new one
+- **Cross-dissolves** — overlap two clips on a track, or press **Dissolve**,
+  and they dissolve into each other for as long as they overlap
+- **Text** — titles over the picture, typed in the inspector, in any size,
+  bold or not, in six colours, aligned to any side; moved, scaled and faded
+  like any clip
 - **Overlays & transforms** — stack videos and still images; position, scale (up to 400 %),
   opacity and per-clip volume via the inspector or by dragging/resizing the clip
   right in the preview
@@ -329,7 +336,7 @@ are registered and then removed again. A tag publishes the result with a
 SHA-256 file and a CycloneDX SBOM that lists the Rust crates and every file of
 the bundled GStreamer runtime, with its hash.
 
-Deferred to later: audio-only tracks and transitions in the video editor, undo,
-and macOS/Linux packaging. The
+Deferred to later: audio-only tracks and transitions other than a
+cross-dissolve in the video editor, and macOS/Linux packaging. The
 top-level Windows 11 menu and signing of the menu package shipped in 2.9.1;
 the installer itself is still unsigned, so SmartScreen still prompts once.
````

- [ ] **Step 2: Run the gates.** Expected: `cargo test -p kuvatin` 315 passed; `cargo test -p kuvatin-video --lib -- --test-threads=1 title_ dissolve_` 21 passed.

- [ ] **Step 3: Run the whole suite with the fixture exported.** `cargo test --workspace -- --test-threads=1`. Expected: every result line `0 failed`; the app 315 passed, the engine 151 passed and 1 ignored. `a_speed_change_during_playback_leaves_the_preview_playing` has failed once in a full run before and passed 20 of 20 alone and on rerun; if it fails, rerun it alone before looking further.

- [ ] **Step 4: Check the build by hand.** Build with `cargo build -p kuvatin` and run `target/debug/kuvatin.exe` with GStreamer on PATH. Make a project of two stills on one track, the second starting a second before the first ends. These were checked in the verified build:
  - a band sits over the overlap; the preview is a blend halfway through it; the Dissolve chip on the second clip reads "Remove dissolve", takes the overlap away, then reads "Dissolve (1.0 s)" in its hint; Undo puts the overlap and the band back;
  - the Text chip adds an amber "Text" block on the top track, selected, with the title controls in the inspector; the preview shows white "Text" over the clip beneath, which stays visible;
  - Left, Top, the yellow swatch and Bold each reach the engine as a title write (checked with temporary log lines, since removed);
  - the Opacity slider fades the title.

  Not checked by hand, for you: that those four show in the preview and undo one at a time; typing a sentence into the title and undoing it as one step (computer use could not type into the field); saving and reopening a project with a title (the engine tests cover it); exporting a dissolve; opening a format-2 project in 2.13.0 and reading its refusal. When driving the app with computer use, move the pointer to a control and wait a second before clicking it: a click sent straight after the pointer jumps can land where the pointer last was.

- [ ] **Step 5: Commit.** `CI gates on the title and dissolve tests; the changelog and README describe them`

## Risks

- **Old projects gain dissolves.** A project from before any overlap rule can hold overlapping clips; it now plays a crossfade there. The changelog says so under Changed.
- **Transitions during the parking dance.** `set_clip_records` moves clips through scratch layers, and GES builds and tears down transitions as it goes. `dissolve_comes_back_when_undo_writes_the_overlap_back` and the `undo_` tests passed with auto-transitions on; a refusal would show as a refused undo, not a corrupt timeline.
- **Fonts resolve on the machine that renders.** The family is fixed at Sans, which always resolves; widening it is a later spec's decision.
- **Snapping still magnets across tracks** (`on_timeline_snap_dx` builds its targets from every row). Pre-existing, out of scope, and more visible now that a same-track edge means "no dissolve".
