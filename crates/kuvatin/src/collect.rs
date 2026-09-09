use kuvatin_core::format::is_input_extension;
use std::path::{Path, PathBuf};

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(is_input_extension)
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

/// Expand a mix of files and folders into a flat, de-duplicated list of image
/// files. Folders are scanned one level deep (non-recursive for v1); hidden,
/// system and dot-files inside them are skipped.
pub fn collect_images(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for p in paths {
        if p.is_dir() {
            if let Ok(entries) = std::fs::read_dir(p) {
                for e in entries.flatten() {
                    let path = e.path();
                    let hidden = e.metadata().map(|m| is_hidden(&m)).unwrap_or(false);
                    if path.is_file() && is_image(&path) && !hidden && !is_dotfile(&path) {
                        out.push(path);
                    }
                }
            }
        } else if p.is_file() && is_image(p) {
            out.push(p.clone());
        }
    }
    out.sort();
    out.dedup();
    out
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
            let status = std::process::Command::new("attrib").arg("+h").arg(&h).status().unwrap();
            assert!(status.success(), "attrib +h");
            h
        };
        let got = collect_images(&[dir.path().to_path_buf()]);
        assert_eq!(got, vec![shown.clone()], "only the visible file: {got:?}");
        #[cfg(windows)]
        assert_eq!(collect_images(&[hidden.clone()]), vec![hidden], "explicit selection wins");
    }
}
