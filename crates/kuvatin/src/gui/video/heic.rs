//! HEIC stills in the video editor. GStreamer can't read HEIF, so an iPhone
//! photo is decoded once (through Windows' codec, `kuvatin_core::heif`) into
//! an sRGB PNG, and that PNG is what the timeline, the media bin and a saved
//! project point at.
//!
//! The PNGs live in `%LOCALAPPDATA%\Kuvatin\heic\`, not in the temp folder
//! the EXR sequence cache uses: a saved project refers to them, and a temp
//! clean-up would leave it with missing stills. They are named after the
//! photo (`IMG_1234.png`, in a folder keyed by the photo's path, size and
//! date), so re-importing the same photo reuses its PNG and a changed photo
//! gets a new one.

use kuvatin_core::heif;
use std::path::{Path, PathBuf};

/// Where the converted stills live.
fn cache_root() -> PathBuf {
    crate::applog::dir().join("heic")
}

/// The file the editor should open for `path`: `path` itself for anything
/// GStreamer reads, or the PNG made from it for a HEIC (converting it first
/// if this is the first time). The error is one line, for the import dialog.
pub(crate) fn editor_source(path: &Path) -> Result<PathBuf, String> {
    if !heif::is_heif_path(path) {
        return Ok(path.to_path_buf());
    }
    editor_source_in(&cache_root(), path)
}

fn editor_source_in(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let out = root.join(cache_key(path)).join(png_name(path));
    if out.is_file() {
        return Ok(out);
    }
    heif::convert_to_png(path, &out).map_err(|e| {
        // Lead with what to install: the dialog shows one line.
        match heif::missing_codec_hint(heif::support()) {
            Some(hint) => hint.to_string(),
            None => e.to_string(),
        }
    })?;
    Ok(out)
}

/// `IMG_1234.HEIC` → `IMG_1234.png`.
fn png_name(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| "photo".into());
    let mut name = PathBuf::from(stem);
    name.set_extension("png");
    name
}

/// The photo's identity: its full path (case-folded, as Windows compares
/// them), size and modification time. FNV-1a rather than the standard
/// hasher, whose output may change between Rust releases — the key names a
/// folder that outlives this build.
fn cache_key(path: &Path) -> String {
    let full = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let meta = std::fs::metadata(path).ok();
    let len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let mtime = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    eat(full.to_string_lossy().to_lowercase().as_bytes());
    eat(&len.to_le_bytes());
    eat(&mtime.to_le_bytes());
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_files_pass_through_untouched() {
        let p = Path::new(r"C:\clips\IMG_0001.MOV");
        assert_eq!(editor_source(p).unwrap(), p);
        let p = Path::new(r"C:\stills\logo.png");
        assert_eq!(editor_source(p).unwrap(), p);
    }

    #[test]
    fn the_png_is_named_after_the_photo() {
        assert_eq!(
            png_name(Path::new(r"C:\a\IMG_1234.HEIC")),
            PathBuf::from("IMG_1234.png")
        );
    }

    #[test]
    fn the_key_follows_the_photo_not_the_spelling_of_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("IMG_1.HEIC");
        std::fs::write(&p, b"one").unwrap();
        let upper = PathBuf::from(p.to_string_lossy().to_uppercase());
        assert_eq!(
            cache_key(&p),
            cache_key(&upper),
            "Windows paths ignore case"
        );
        let before = cache_key(&p);
        std::fs::write(&p, b"changed, longer").unwrap();
        assert_ne!(before, cache_key(&p), "an edited photo gets a new PNG");
    }

    /// A HEIC that can't be decoded (here: not a HEIC at all) fails with
    /// one line and leaves nothing behind in the cache.
    #[test]
    fn a_broken_heic_fails_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("broken.heic");
        std::fs::write(&p, b"not a photo").unwrap();
        let root = dir.path().join("cache");
        let err = editor_source_in(&root, &p).unwrap_err();
        assert!(!err.contains('\n'), "{err}");
        let left: Vec<_> = walk(&root);
        assert!(
            left.iter().all(|f| f.extension() != Some("png".as_ref())),
            "{left:?}"
        );
    }

    fn walk(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(root) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    out.extend(walk(&p));
                } else {
                    out.push(p);
                }
            }
        }
        out
    }

    /// The editor's whole path for a HEIC: converted once, reused after, and
    /// GStreamer takes the result as a still. Skipped where Windows can't
    /// decode HEIC.
    #[test]
    fn a_heic_becomes_a_still_gstreamer_reads() {
        if heif::support() != heif::Support::Ready {
            eprintln!(
                "skipped: Windows can't decode HEIC here ({:?})",
                heif::support()
            );
            return;
        }
        let probe = include_bytes!("../../../../kuvatin-core/src/heif_probe.heic");
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("IMG_0042.HEIC");
        std::fs::write(&photo, probe).unwrap();
        let root = dir.path().join("cache");
        let png = editor_source_in(&root, &photo).unwrap();
        assert_eq!(png.file_name().unwrap(), "IMG_0042.png");
        let made = std::fs::metadata(&png).unwrap().modified().unwrap();
        assert_eq!(editor_source_in(&root, &photo).unwrap(), png, "reused");
        assert_eq!(std::fs::metadata(&png).unwrap().modified().unwrap(), made);
        kuvatin_video::warm_asset(&png).expect("GStreamer reads the PNG");
        let thumb = kuvatin_video::thumbnail(&png, 16).expect("a thumbnail");
        assert_eq!((thumb.width, thumb.height), (16, 16));
    }
}
