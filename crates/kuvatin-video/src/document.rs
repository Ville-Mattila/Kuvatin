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

/// What this version of Kuvatin writes. A file from a LATER version is
/// refused rather than half-understood: a project silently missing the clips
/// it could not parse is worse than a project that will not open.
pub const FORMAT_VERSION: u32 = 1;

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
    pub layout: LayoutRecord,
    /// Set when the clip is an image sequence. The URI is enough to put the
    /// sequence back on the timeline; this is what lets the media bin re-add
    /// it afterwards, which the URI alone cannot describe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<crate::sequence::SequenceSpec>,
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
}

impl ProjectFile {
    pub fn new(canvas_w: i32, canvas_h: i32, clips: Vec<ClipRecord>) -> Self {
        ProjectFile {
            version: FORMAT_VERSION,
            canvas_w,
            canvas_h,
            clips,
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
                    layout: layout(),
                    sequence: None,
                },
                ClipRecord {
                    uri: "imagesequence://C:/render/frame_%04d.png?framerate=24/1".into(),
                    name: "frame_####.png (48)".into(),
                    track: 1,
                    start: 2.0,
                    inpoint: 0.0,
                    duration: 2.0,
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
}
