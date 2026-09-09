use kuvatin_core::format::is_input_extension;
use std::path::{Path, PathBuf};

fn ext_of(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

fn is_image(path: &Path) -> bool {
    ext_of(path)
        .map(|e| is_input_extension(&e))
        .unwrap_or(false)
}

/// A file the video editor can import directly: a video container or an
/// image input (stills become overlays).
fn is_media(path: &Path) -> bool {
    ext_of(path)
        .map(|e| kuvatin_video::VIDEO_EXTENSIONS.contains(&e.as_str()) || is_input_extension(&e))
        .unwrap_or(false)
}

/// A sequence-only frame format (`.exr`): not importable as a still, but the
/// user probably meant "Import sequence…".
fn is_frame_only(path: &Path) -> bool {
    ext_of(path)
        .map(|e| {
            kuvatin_video::FRAME_EXTENSIONS.contains(&e.as_str())
                && !is_input_extension(&e)
                && !kuvatin_video::VIDEO_EXTENSIONS.contains(&e.as_str())
        })
        .unwrap_or(false)
}

/// Hidden or system files (Explorer hides them; AppleDouble `._foo.png`
/// sidecars and thumbnail caches are the usual offenders) are skipped when a
/// FOLDER is expanded — never when the user selected the file explicitly.
#[cfg(windows)]
fn is_hidden(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    meta.file_attributes() & (HIDDEN | SYSTEM) != 0
}

#[cfg(not(windows))]
fn is_hidden(_meta: &std::fs::Metadata) -> bool {
    false
}

fn is_dotfile(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with('.'))
        .unwrap_or(false)
}

/// Expand a mix of files and folders into a flat, de-duplicated list of the
/// files `keep` accepts. Folders are scanned one level deep (non-recursive);
/// hidden, system and dot-files inside them are skipped.
fn collect(paths: &[PathBuf], keep: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for p in paths {
        if p.is_dir() {
            if let Ok(entries) = std::fs::read_dir(p) {
                for e in entries.flatten() {
                    let path = e.path();
                    let hidden = e.metadata().map(|m| is_hidden(&m)).unwrap_or(false);
                    if path.is_file() && keep(&path) && !hidden && !is_dotfile(&path) {
                        out.push(path);
                    }
                }
            }
        } else if p.is_file() && keep(p) {
            out.push(p.clone());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Image files for the Images mode (see [`collect`]).
pub fn collect_images(paths: &[PathBuf]) -> Vec<PathBuf> {
    collect(paths, is_image)
}

/// Media files for the Videos mode: `(importable, frame-only)`. The second
/// list holds explicitly selected `.exr` frames, so the caller can point the
/// user at "Import sequence…" instead of letting GStreamer fail on them; the
/// rest (`.txt`, project files) is dropped silently, like the image mode does.
pub fn collect_media(paths: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let media = collect(paths, is_media);
    let frames: Vec<PathBuf> = paths
        .iter()
        .filter(|p| p.is_file() && is_frame_only(p))
        .cloned()
        .collect();
    (media, frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_files_and_folder_contents() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("a.png");
        std::fs::write(&img, b"x").unwrap();
        let txt = dir.path().join("note.txt");
        std::fs::write(&txt, b"x").unwrap();

        let got = collect_images(&[dir.path().to_path_buf()]);
        assert_eq!(got, vec![img]);
    }

    #[test]
    fn dedups_and_filters_non_images() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("a.JPG");
        std::fs::write(&img, b"x").unwrap();
        let got = collect_images(&[img.clone(), img.clone()]);
        assert_eq!(got, vec![img]);
    }

    /// The canonical list applies: `.tif` and `.jfif` are inputs now.
    #[test]
    fn accepts_every_input_extension() {
        let dir = tempfile::tempdir().unwrap();
        let tif = dir.path().join("scan.tif");
        let jfif = dir.path().join("photo.jfif");
        std::fs::write(&tif, b"x").unwrap();
        std::fs::write(&jfif, b"x").unwrap();
        let got = collect_images(&[dir.path().to_path_buf()]);
        assert_eq!(got, vec![jfif, tif]);
    }

    /// Dot-files and (on Windows) hidden files inside a folder are skipped;
    /// an explicitly selected hidden file is still honoured.
    #[test]
    fn folder_scan_skips_hidden_and_dotfiles() {
        let dir = tempfile::tempdir().unwrap();
        let shown = dir.path().join("a.png");
        let dotted = dir.path().join("._a.png");
        std::fs::write(&shown, b"x").unwrap();
        std::fs::write(&dotted, b"x").unwrap();
        #[cfg(windows)]
        let hidden = {
            let h = dir.path().join("thumb.png");
            std::fs::write(&h, b"x").unwrap();
            let status = std::process::Command::new("attrib")
                .arg("+h")
                .arg(&h)
                .status()
                .unwrap();
            assert!(status.success(), "attrib +h");
            h
        };
        let got = collect_images(&[dir.path().to_path_buf()]);
        assert_eq!(got, vec![shown.clone()], "only the visible file: {got:?}");
        #[cfg(windows)]
        assert_eq!(
            collect_images(std::slice::from_ref(&hidden)),
            vec![hidden],
            "explicit selection wins"
        );
    }

    /// A Videos-mode drop expands folders, keeps videos + stills, drops junk
    /// silently and reports EXR frames separately (for the sequence hint).
    #[test]
    fn media_collection_expands_filters_and_flags_exr() {
        let dir = tempfile::tempdir().unwrap();
        let mp4 = dir.path().join("clip.MP4");
        let png = dir.path().join("logo.png");
        let exr = dir.path().join("frame_0001.exr");
        for p in [&mp4, &png, &exr, &dir.path().join("notes.txt")] {
            std::fs::write(p, b"x").unwrap();
        }
        let (media, frames) = collect_media(&[dir.path().to_path_buf(), exr.clone()]);
        assert_eq!(media, vec![mp4, png]);
        assert_eq!(frames, vec![exr], "an explicitly dropped EXR is flagged");
    }
}
