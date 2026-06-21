use std::path::{Path, PathBuf};

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp", "tiff", "tif", "gif"];

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Expand a mix of files and folders into a flat, de-duplicated list of image
/// files. Folders are scanned one level deep (non-recursive for v1).
pub fn collect_images(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for p in paths {
        if p.is_dir() {
            if let Ok(entries) = std::fs::read_dir(p) {
                for e in entries.flatten() {
                    let path = e.path();
                    if path.is_file() && is_image(&path) {
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
}
