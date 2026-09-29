//! The saved form of a video project.
//!
//! A GES timeline is a live object graph; this is the flat description of it
//! that goes in a file — what is on the timeline, where, and how it is
//! transformed. Nothing in here touches GStreamer, so it round-trips and is
//! tested without an engine.
//!
//! Clips are stored by URI rather than by path because an image sequence is not
//! a single file: it arrives as `imagesequence://…`, and the engine takes the
//! same URI back when the project is reopened.

use anyhow::{Context, Result};
use gstreamer as gst;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The newest format this version of Kuvatin reads. A file from a LATER
/// version is refused rather than half-understood: a project silently missing
/// the clips it could not parse is worse than a project that will not open.
///
/// A file is stamped with the OLDEST format that can read it
/// ([`required_version`]), not with this: a project of media clips alone is
/// still format 1, and opens in every build ever shipped.
pub const FORMAT_VERSION: u32 = 2;

/// A clip's transform, as stored. Mirrors [`crate::Layout`], which is not
/// serialisable on purpose — the engine type can change shape without changing
/// what a saved file means.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LayoutRecord {
    pub posx: i32,
    pub posy: i32,
    pub scale: f64,
    pub alpha: f64,
    pub volume: f64,
}

impl From<crate::Layout> for LayoutRecord {
    fn from(l: crate::Layout) -> Self {
        LayoutRecord {
            posx: l.posx,
            posy: l.posy,
            scale: l.scale,
            alpha: l.alpha,
            volume: l.volume,
        }
    }
}

impl From<LayoutRecord> for crate::Layout {
    fn from(l: LayoutRecord) -> Self {
        crate::Layout {
            posx: l.posx,
            posy: l.posy,
            scale: l.scale,
            alpha: l.alpha,
            volume: l.volume,
        }
    }
}

/// One clip on the timeline. Times are seconds: a saved project is a document
/// someone may read, and nanoseconds in a text file help nobody.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipRecord {
    /// `file:///…` for a single file, `imagesequence://…` for a frame run.
    pub uri: String,
    /// The name shown on the clip and in the media bin.
    #[serde(default)]
    pub name: String,
    pub track: usize,
    pub start: f64,
    pub inpoint: f64,
    pub duration: f64,
    /// Playback rate: 1.0 is normal, 2.0 twice as fast. Optional, and left
    /// out of the file at 1.0, so a project that never changes a clip's speed
    /// is written exactly as before and every file from 2.12 and earlier
    /// still loads. `#[serde(default)]` alone would read a missing one as 0.
    #[serde(default = "unit_rate", skip_serializing_if = "is_unit_rate")]
    pub rate: f64,
    pub layout: LayoutRecord,
    /// Set when the clip is an image sequence. The URI is enough to put the
    /// sequence back on the timeline; this is what lets the media bin re-add
    /// it afterwards, which the URI alone cannot describe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<crate::sequence::SequenceSpec>,
    /// What kind of clip this record describes, when it is not a clip on a
    /// media source. Absent, which is every record written before this
    /// existed, means a URI clip, and `uri` is the whole of its identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<ClipBody>,
}

/// What a record describes, when it is not a clip on a media source. One
/// variant for now: the tag is what costs something to add later, so
/// `[clips.body.title]` today makes another kind of clip a variant tomorrow,
/// with no change to how a file is read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipBody {
    /// A text overlay, built as a GES `TitleClip`.
    Title(TitleRecord),
}

/// A text overlay's own state: what a title needs beyond the place, times
/// and transform every clip has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TitleRecord {
    /// What it says. May be empty and may hold newlines: an empty title is
    /// still a clip you can see and select, which a half-typed one has to be.
    pub text: String,
    /// A Pango font description, "Sans Bold 48". Written whole, so a later
    /// version can offer more than the interface does now without changing
    /// the file.
    #[serde(default = "default_font")]
    pub font: String,
    /// The text colour, `#rrggbb` or `#rrggbbaa`: a string, because a project
    /// file is a document someone may read, the reason times are seconds.
    #[serde(default = "default_text_color")]
    pub color: String,
    #[serde(default)]
    pub halign: TitleHAlign,
    #[serde(default)]
    pub valign: TitleVAlign,
}

/// Where a title's lines sit across the frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TitleHAlign {
    Left,
    #[default]
    Center,
    Right,
}

/// Where a title sits down the frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TitleVAlign {
    Top,
    #[default]
    Center,
    Bottom,
}

fn default_font() -> String {
    "Sans Bold 48".into()
}

fn default_text_color() -> String {
    "#ffffff".into()
}

impl Default for TitleRecord {
    /// What "Add text" puts on the timeline.
    fn default() -> Self {
        TitleRecord {
            text: "Text".into(),
            font: default_font(),
            color: default_text_color(),
            halign: TitleHAlign::Center,
            valign: TitleVAlign::Center,
        }
    }
}

impl TitleRecord {
    /// The name a title shows on the timeline: its first line, cut to 24
    /// characters, or "Text" when it has none.
    pub fn name(&self) -> String {
        let first = self.text.lines().next().unwrap_or("").trim();
        if first.is_empty() {
            return "Text".into();
        }
        let mut name: String = first.chars().take(24).collect();
        if first.chars().count() > 24 {
            name.push('…');
        }
        name
    }
}

/// `#rrggbb` or `#rrggbbaa` as the value GES takes for a title's colour:
/// ARGB, alpha in the top byte (measured: `0xffff0000` draws red). None for
/// anything else.
pub fn parse_color(s: &str) -> Option<u32> {
    let hex = s.strip_prefix('#')?;
    if !(hex.len() == 6 || hex.len() == 8) || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some(if hex.len() == 6 {
        0xff00_0000 | v
    } else {
        // #rrggbbaa → aarrggbb
        (v >> 8) | ((v & 0xff) << 24)
    })
}

/// The inverse of [`parse_color`]: `#rrggbb`, or `#rrggbbaa` when the colour
/// is not fully opaque.
pub fn format_color(v: u32) -> String {
    let (a, rgb) = (v >> 24, v & 0x00ff_ffff);
    if a == 0xff {
        format!("#{rgb:06x}")
    } else {
        format!("#{rgb:06x}{a:02x}")
    }
}

/// The format a document needs: 2 once any clip is something a format-1
/// reader has no shape for (a title), 1 otherwise.
pub fn required_version(clips: &[ClipRecord]) -> u32 {
    if clips.iter().any(|c| c.body.is_some()) {
        2
    } else {
        1
    }
}

fn unit_rate() -> f64 {
    1.0
}

fn is_unit_rate(rate: &f64) -> bool {
    *rate == 1.0
}

/// One track, as stored. Its index in [`ProjectFile::tracks`] is the track:
/// 0 is the top one, the numbering [`ClipRecord::track`] uses. Each field is
/// left out of the file at its default, so a track nobody touched is a bare
/// `[[tracks]]` header.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackRecord {
    /// What the user called it. Empty means "call it Track N".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Silent: its layer plays no sound, in the preview or the export.
    #[serde(default, skip_serializing_if = "is_false")]
    pub muted: bool,
    /// Refuses every edit to the clips on it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub locked: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A track table that says nothing a default one would not. It is left out of
/// the file, and the track count comes from the clips, as it always has.
fn untouched(tracks: &[TrackRecord]) -> bool {
    tracks.iter().all(|t| *t == TrackRecord::default())
}

/// The file a `file://` URI names, or None for anything else (an
/// `imagesequence://` clip is a run of files, not one). The interface needs
/// this to put a reopened project's sources back in the media bin.
pub fn path_from_uri(uri: &str) -> Option<std::path::PathBuf> {
    gst::glib::filename_from_uri(uri).ok().map(|(p, _)| p)
}

/// A whole project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectFile {
    pub version: u32,
    pub canvas_w: i32,
    pub canvas_h: i32,
    /// In timeline order, top track first. Empty is a valid project.
    #[serde(default)]
    pub clips: Vec<ClipRecord>,
    /// One per track row, top first: names, mutes and locks. Left out when no
    /// track has any of them, so a project that never used them is written
    /// exactly as before. Empty on reading means "infer the tracks from the
    /// clips", which is what every file from 2.13 and earlier needs.
    ///
    /// Filled in by the interface, which owns names and locks; the engine
    /// writes it empty (see `Project::to_document`).
    #[serde(default, skip_serializing_if = "untouched")]
    pub tracks: Vec<TrackRecord>,
}

impl ProjectFile {
    pub fn new(canvas_w: i32, canvas_h: i32, clips: Vec<ClipRecord>) -> Self {
        ProjectFile {
            version: required_version(&clips),
            canvas_w,
            canvas_h,
            clips,
            tracks: Vec::new(),
        }
    }

    /// Write the project to `path` (TOML, as the presets are).
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).context("could not encode the project")?;
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("could not create {}", dir.display()))?;
            }
        }
        std::fs::write(path, text).with_context(|| format!("could not write {}", path.display()))
    }

    /// Read a project written by [`save`](Self::save).
    pub fn load(path: &Path) -> Result<Self> {
        let bytes =
            std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
        // A file picked by mistake is usually binary, and "stream did not
        // contain valid UTF-8" sends the user hunting for a permissions
        // problem they do not have. It is simply not a project.
        let text = String::from_utf8(bytes)
            .with_context(|| format!("{} is not a Kuvatin project", path.display()))?;
        let doc: ProjectFile = toml::from_str(&text)
            .with_context(|| format!("{} is not a Kuvatin project", path.display()))?;
        if doc.version > FORMAT_VERSION {
            anyhow::bail!(
                "{} was written by a newer version of Kuvatin (format {}, this build reads {})",
                path.display(),
                doc.version,
                FORMAT_VERSION
            );
        }
        Ok(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> LayoutRecord {
        LayoutRecord {
            posx: 12,
            posy: -4,
            scale: 0.75,
            alpha: 0.5,
            volume: 1.0,
        }
    }

    fn sample() -> ProjectFile {
        ProjectFile::new(
            1920,
            1080,
            vec![
                ClipRecord {
                    uri: "file:///C:/shots/take1.mp4".into(),
                    name: "take1.mp4".into(),
                    track: 0,
                    start: 0.0,
                    inpoint: 1.5,
                    duration: 4.25,
                    rate: 1.0,
                    layout: layout(),
                    sequence: None,
                    body: None,
                },
                ClipRecord {
                    uri: "imagesequence://C:/render/frame_%04d.png?framerate=24/1".into(),
                    name: "frame_####.png (48)".into(),
                    track: 1,
                    start: 2.0,
                    inpoint: 0.0,
                    duration: 2.0,
                    rate: 1.0,
                    layout: layout(),
                    sequence: Some(crate::sequence::SequenceSpec {
                        dir: std::path::PathBuf::from("C:/render"),
                        prefix: "frame_".into(),
                        suffix: ".png".into(),
                        pad: 4,
                        start: 1,
                        count: 48,
                        fps: 24,
                    }),
                    body: None,
                },
            ],
        )
    }

    #[test]
    fn a_project_survives_the_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cut.kuvatin");
        let doc = sample();
        doc.save(&path).unwrap();
        assert_eq!(ProjectFile::load(&path).unwrap(), doc);
    }

    /// The file is meant to be readable: seconds, names, and one table per clip.
    #[test]
    fn the_file_reads_as_a_document() {
        let text = toml::to_string_pretty(&sample()).unwrap();
        assert!(text.contains("version = 1"), "{text}");
        assert!(text.contains("canvas_w = 1920"), "{text}");
        assert!(text.contains("take1.mp4"), "{text}");
        assert!(text.contains("duration = 4.25"), "{text}");
    }

    #[test]
    fn an_empty_timeline_is_a_valid_project() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.kuvatin");
        let doc = ProjectFile::new(1280, 720, Vec::new());
        doc.save(&path).unwrap();
        let back = ProjectFile::load(&path).unwrap();
        assert!(back.clips.is_empty());
        assert_eq!((back.canvas_w, back.canvas_h), (1280, 720));
    }

    /// Half-opening a file from a later version would drop whatever it could
    /// not parse — and the user would find out when they exported.
    #[test]
    fn a_project_from_a_newer_version_is_refused_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.kuvatin");
        let mut doc = sample();
        doc.version = FORMAT_VERSION + 3;
        doc.save(&path).unwrap();
        let err = ProjectFile::load(&path).unwrap_err().to_string();
        assert!(err.contains("newer version"), "{err}");
    }

    #[test]
    fn something_that_is_not_a_project_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.png");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\n not toml at all").unwrap();
        let err = ProjectFile::load(&path).unwrap_err().to_string();
        assert!(err.contains("not a Kuvatin project"), "{err}");

        let missing = dir.path().join("gone.kuvatin");
        let err = ProjectFile::load(&missing).unwrap_err().to_string();
        assert!(err.contains("could not read"), "{err}");
    }

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
        assert!(
            doc.tracks.is_empty(),
            "no table: the tracks come from the clips"
        );
    }

    /// A top track at its defaults, a named and muted one, and a locked one.
    fn tracks() -> Vec<TrackRecord> {
        vec![
            TrackRecord::default(),
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
    }

    #[test]
    fn a_track_table_survives_the_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tracks.kuvatin");
        let mut doc = sample();
        doc.tracks = tracks();
        doc.save(&path).unwrap();
        assert_eq!(ProjectFile::load(&path).unwrap(), doc);
    }

    /// A project nobody renamed, muted or locked is written exactly as before.
    #[test]
    fn an_untouched_track_table_is_left_out_of_the_file() {
        let mut doc = sample();
        doc.tracks = vec![TrackRecord::default(); 3];
        let text = toml::to_string_pretty(&doc).unwrap();
        assert!(!text.contains("tracks"), "{text}");
        assert_eq!(text, toml::to_string_pretty(&sample()).unwrap());
    }

    /// Once one track has something to say, every row is written, so the
    /// count survives; a row at its defaults is a bare header. The format
    /// stays at version 1: the table is additive.
    #[test]
    fn a_touched_table_writes_every_row_and_only_what_is_set() {
        let mut doc = sample();
        doc.tracks = tracks();
        let text = toml::to_string_pretty(&doc).unwrap();
        assert_eq!(text.matches("[[tracks]]").count(), 3, "{text}");
        assert!(text.contains("name = \"Dialogue\""), "{text}");
        assert!(
            text.contains("muted = true") && text.contains("locked = true"),
            "{text}"
        );
        assert!(
            !text.contains("= false") && !text.contains("name = \"\""),
            "{text}"
        );
        assert!(text.contains("version = 1"), "{text}");
    }

    fn title(text: &str) -> ClipRecord {
        ClipRecord {
            uri: String::new(),
            name: String::new(),
            track: 0,
            start: 1.0,
            inpoint: 0.0,
            duration: 5.0,
            rate: 1.0,
            layout: layout(),
            sequence: None,
            body: Some(ClipBody::Title(TitleRecord {
                text: text.into(),
                font: "Serif 72".into(),
                color: "#ffcc00".into(),
                halign: TitleHAlign::Left,
                valign: TitleVAlign::Bottom,
            })),
        }
    }

    /// Opaque colours read as ARGB with the alpha byte full; `#rrggbbaa`
    /// moves its alpha to the top, where GES looks for it.
    #[test]
    fn title_colours_parse_to_argb() {
        assert_eq!(parse_color("#ff0000"), Some(0xffff_0000));
        assert_eq!(parse_color("#FFCC00"), Some(0xffff_cc00));
        assert_eq!(parse_color("#11223380"), Some(0x8011_2233));
        for bad in ["", "ff0000", "#ff00", "#ff00001", "#gg0000", "#ff0000ff00"] {
            assert_eq!(parse_color(bad), None, "{bad}");
        }
    }

    #[test]
    fn title_colours_format_back_to_what_they_parsed_from() {
        for s in ["#ffffff", "#000000", "#ffcc00", "#11223380", "#00000000"] {
            assert_eq!(format_color(parse_color(s).unwrap()), s);
        }
    }

    #[test]
    fn title_names_are_their_first_line_cut_short() {
        let named = |text: &str| {
            TitleRecord {
                text: text.into(),
                ..TitleRecord::default()
            }
            .name()
        };
        assert_eq!(
            named(
                "Opening
second line"
            ),
            "Opening"
        );
        assert_eq!(named("   "), "Text");
        assert_eq!(named(""), "Text");
        assert_eq!(
            named("A title far longer than twenty-four characters"),
            "A title far longer than …"
        );
    }

    /// A file of media clips alone stays format 1, so every build ever
    /// shipped still opens it; one title makes it format 2, so a build that
    /// cannot draw titles refuses it instead of dropping them.
    #[test]
    fn title_clips_raise_the_format_and_media_clips_do_not() {
        assert_eq!(sample().version, 1);
        let mut clips = sample().clips;
        clips.push(title("Hello"));
        assert_eq!(required_version(&clips), 2);
        assert_eq!(ProjectFile::new(1920, 1080, clips).version, 2);
    }

    #[test]
    fn title_clips_survive_the_round_trip_with_their_newlines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("titled.kuvatin");
        let mut clips = sample().clips;
        clips.push(title(
            "First line
\"quoted\" second",
        ));
        let doc = ProjectFile::new(1920, 1080, clips);
        doc.save(&path).unwrap();
        assert_eq!(ProjectFile::load(&path).unwrap(), doc);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[clips.body.title]"), "{text}");
        assert!(text.contains("version = 2"), "{text}");
        assert_eq!(
            text.matches("[clips.body").count(),
            1,
            "a media clip writes no body: {text}"
        );
    }

    /// A title written with only its text gets the defaults for the rest.
    #[test]
    fn title_fields_left_out_take_their_defaults() {
        let text = r#"version = 2
canvas_w = 1280
canvas_h = 720

[[clips]]
uri = ""
track = 0
start = 0.0
inpoint = 0.0
duration = 5.0

[clips.layout]
posx = 0
posy = 0
scale = 1.0
alpha = 1.0
volume = 1.0

[clips.body.title]
text = "Hi"
"#;
        let doc: ProjectFile = toml::from_str(text).unwrap();
        let Some(ClipBody::Title(t)) = &doc.clips[0].body else {
            panic!("a title: {:?}", doc.clips[0].body);
        };
        assert_eq!(
            t,
            &TitleRecord {
                text: "Hi".into(),
                ..TitleRecord::default()
            }
        );
    }

    #[test]
    fn title_era_builds_refuse_a_format_three_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("three.kuvatin");
        let mut doc = sample();
        doc.version = 3;
        doc.save(&path).unwrap();
        let err = ProjectFile::load(&path).unwrap_err().to_string();
        assert!(err.contains("format 3, this build reads 2"), "{err}");
    }
}
