//! Staging an installer, handing off to a copy of this executable, and that
//! copy's own run. See the design doc for why the copy exists.

use super::{asset_name, asset_urls, fetch, verify};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// Where a download waits to be installed. Under `%TEMP%\kuvatin`, which the
/// every-account uninstall deletes, so a machine that never runs Kuvatin
/// again is still left clean.
pub fn stage_dir() -> Result<PathBuf> {
    let temp = std::env::temp_dir();
    if temp.as_os_str().is_empty() {
        bail!("there is no temporary folder to stage the download in");
    }
    Ok(temp.join("kuvatin").join("update"))
}

/// Delete a staging folder, saying nothing if it is not there. The helper
/// cannot delete the copy it is running from, so this runs at the next start.
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

/// What a finished download left behind.
#[allow(dead_code)] // Nothing reads these yet; wiring the dialog (Task 12) is next.
#[derive(Debug, Clone)]
pub struct Staged {
    pub msi: PathBuf,
    pub helper: PathBuf,
}

/// Does this file match what the release says it should be? A file that fails
/// is deleted, so a later run cannot pick it up.
fn accept(msi: &Path, checksum_file: &str, name: &str) -> Result<()> {
    let expected = match verify::expected_hash(checksum_file, name) {
        Some(h) => h,
        None => {
            let _ = std::fs::remove_file(msi);
            bail!("could not check the download: the checksum file does not name {name}");
        }
    };
    let actual = match verify::sha256_file(msi) {
        Ok(a) => a,
        Err(e) => {
            let _ = std::fs::remove_file(msi);
            return Err(e).context("could not check the download");
        }
    };
    if actual != expected {
        let _ = std::fs::remove_file(msi);
        bail!("the download did not arrive intact");
    }
    Ok(())
}

/// Download `version`'s installer and its checksum, check it, and copy this
/// executable in beside it to do the installing. Anything it wrote is removed
/// if any step fails.
#[allow(dead_code)] // Only the tests call this so far; wiring the dialog (Task 12) is next.
pub fn stage(
    version: &str,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(fetch::Progress),
) -> Result<Staged> {
    let dir = stage_dir()?;
    sweep(&dir);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("could not write to {}", dir.display()))?;

    let name = asset_name(version);
    let (msi_url, sha_url) = asset_urls(version);
    let msi = dir.join(&name);

    let staged = (|| -> Result<Staged> {
        fetch::get_to_file(&msi_url, &msi, cancel, on_progress)?;
        let checksum = fetch::get_to_string(&sha_url, 4096)?;
        accept(&msi, &checksum, &name)?;

        let running = std::env::current_exe().context("could not find this executable")?;
        let helper = dir.join("kuvatin-updater.exe");
        std::fs::copy(&running, &helper)
            .with_context(|| format!("could not copy this executable to {}", helper.display()))?;
        Ok(Staged { msi, helper })
    })();

    if staged.is_err() {
        sweep(&dir);
    }
    staged
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

    /// The rule that decides whether a downloaded file may be run, separated
    /// from the download so it can be tested without a network.
    #[test]
    fn a_file_is_only_accepted_when_it_matches_the_published_digest() {
        let dir = std::env::temp_dir().join(format!("kuvatin-accept-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let msi = dir.join("kuvatin-2.13.0-x86_64.msi");
        std::fs::write(&msi, b"abc").expect("write");
        let real = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

        let good = format!("{real}  kuvatin-2.13.0-x86_64.msi\n");
        accept(&msi, &good, "kuvatin-2.13.0-x86_64.msi").expect("the digest matches");

        let wrong = format!("{}  kuvatin-2.13.0-x86_64.msi\n", "0".repeat(64));
        let err = accept(&msi, &wrong, "kuvatin-2.13.0-x86_64.msi")
            .expect_err("a mismatch must not be accepted");
        assert!(
            format!("{err:#}").contains("did not arrive intact"),
            "{err:#}"
        );
        assert!(
            !msi.exists(),
            "a file that failed its check must be deleted"
        );

        // And a checksum file that never names our asset.
        std::fs::write(&msi, b"abc").expect("write again");
        let other = format!("{real}  something-else.msi\n");
        let err = accept(&msi, &other, "kuvatin-2.13.0-x86_64.msi").expect_err("wrong name");
        assert!(format!("{err:#}").contains("could not check"), "{err:#}");
        assert!(!msi.exists());

        // And the third way out: the hashing itself fails. A refusal too, and
        // nothing is left behind for a later run to pick up.
        let err = accept(&msi, &good, "kuvatin-2.13.0-x86_64.msi").expect_err("nothing to hash");
        assert!(format!("{err:#}").contains("could not check"), "{err:#}");
        assert!(!msi.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
