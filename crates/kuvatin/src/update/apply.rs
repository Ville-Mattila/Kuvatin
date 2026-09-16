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

/// What `msiexec` exiting with a given code means for us.
#[allow(dead_code)] // Only the tests call this so far; the helper (Task 8) is next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installed {
    Yes,
    /// The elevation prompt was declined. Nothing was changed.
    Declined,
    Failed(u32),
}

/// 3010 is "installed, reboot when you like", which is still installed. 1602
/// is "user cancelled" and 1223 is "the elevation prompt was refused".
#[allow(dead_code)] // Only the tests call this so far; the helper (Task 8) is next.
pub fn install_outcome(code: u32) -> Installed {
    match code {
        0 | 3010 => Installed::Yes,
        1602 | 1223 => Installed::Declined,
        other => Installed::Failed(other),
    }
}

/// One ASCII line about an outcome, for the log and the message box.
#[allow(dead_code)] // Only the tests call this so far; the helper (Task 8) is next.
pub fn describe(outcome: Installed) -> String {
    match outcome {
        Installed::Yes => "the update installed".to_string(),
        Installed::Declined => {
            "the update needs administrator rights, and the prompt was declined. \
             Kuvatin is unchanged."
                .to_string()
        }
        Installed::Failed(code) => format!(
            "the installer stopped with code {code}. Kuvatin is unchanged. \
             You can install by hand from {}",
            crate::update::RELEASES_URL
        ),
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

    #[test]
    fn reads_what_msiexec_said() {
        assert_eq!(install_outcome(0), Installed::Yes);
        // 3010: installed, wants a reboot at the user's convenience.
        assert_eq!(install_outcome(3010), Installed::Yes);
        // 1602 and 1223: the elevation prompt was declined.
        assert_eq!(install_outcome(1602), Installed::Declined);
        assert_eq!(install_outcome(1223), Installed::Declined);
        assert_eq!(install_outcome(1603), Installed::Failed(1603));
        assert_eq!(install_outcome(1), Installed::Failed(1));
    }

    #[test]
    fn every_word_the_helper_can_print_is_ascii() {
        // The helper's lines end up in kuvatin.log and in a message box, and
        // the uninstall report taught us what a non-ASCII character does on
        // the way through Windows tooling.
        for code in [0u32, 3010, 1602, 1223, 1603, 7] {
            let said = describe(install_outcome(code));
            assert!(said.is_ascii(), "{said:?}");
        }
    }
}
