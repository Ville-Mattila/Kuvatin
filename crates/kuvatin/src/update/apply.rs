//! Staging an installer, handing off to a copy of this executable, and that
//! copy's own run. See the design doc for why the copy exists.

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

/// Where a download waits to be installed. Under `%TEMP%\kuvatin`, which the
/// every-account uninstall deletes, so a machine that never runs Kuvatin
/// again is still left clean.
#[allow(dead_code)] // Only the tests call this so far; staging a download (Task 7) is next.
pub fn stage_dir() -> Result<PathBuf> {
    let temp = std::env::temp_dir();
    if temp.as_os_str().is_empty() {
        bail!("there is no temporary folder to stage the download in");
    }
    Ok(temp.join("kuvatin").join("update"))
}

/// Delete a staging folder, saying nothing if it is not there. The helper
/// cannot delete the copy it is running from, so this runs at the next start.
#[allow(dead_code)] // Only the tests call this so far; staging a download (Task 7) is next.
pub fn sweep(dir: &Path) {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => crate::applog::log(&format!("update: cleared {}", dir.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => crate::applog::log(&format!("update: could not clear {}: {e}", dir.display())),
    }
}

/// Clear the staging folder left by a previous update. Called once at start.
#[allow(dead_code)] // Only the tests call this so far; wiring the dialog (Task 12) is next.
pub fn sweep_stage() {
    if let Ok(dir) = stage_dir() {
        sweep(&dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_sits_inside_the_tree_the_uninstall_sweeps() {
        let dir = stage_dir().expect("a staging path");
        let tail: Vec<String> = dir
            .components()
            .rev()
            .take(3)
            .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
            .collect();
        // %LOCALAPPDATA%\Temp\kuvatin\update — the uninstaller deletes the
        // whole Temp\kuvatin tree, so anything left here goes with it.
        assert_eq!(tail, vec!["update", "kuvatin", "temp"], "{dir:?}");
    }

    #[test]
    fn sweeping_removes_what_was_staged_and_shrugs_at_nothing() {
        let root = std::env::temp_dir().join(format!("kuvatin-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("deep")).expect("make the folder");
        std::fs::write(root.join("deep").join("kuvatin.exe"), b"not really").expect("write");

        sweep(&root);
        assert!(!root.exists(), "the staging folder should be gone");

        // Called again on the next start, with nothing there: still quiet.
        sweep(&root);
    }
}
