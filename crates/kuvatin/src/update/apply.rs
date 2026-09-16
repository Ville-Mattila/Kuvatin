//! Staging an installer and handing off to `kuvatin-updater`, which installs
//! it once this process is gone. See the design doc for why the installing is
//! done by another program: an installer cannot replace a running executable,
//! and this one imports libraries from the folder being replaced.

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

/// Delete a staging folder, saying nothing if it is not there. The updater
/// cannot delete the copy it is running from, so this runs at the next start.
pub fn sweep(dir: &Path) {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => crate::applog::log(&format!("update: cleared {}", dir.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => crate::applog::log(&format!("update: could not clear {}: {e}", dir.display())),
    }
}

/// Clear the staging folder left by a previous update. Called once at start.
pub fn sweep_stage() {
    if let Ok(dir) = stage_dir() {
        sweep(&dir);
    }
}

/// What a finished download left behind: the installer, and the copy of
/// `kuvatin-updater.exe` that will run it.
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

/// Download `version`'s installer and its checksum, check it, and copy the
/// updater in beside it to do the installing. Anything it wrote is removed if
/// any step fails.
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

        // The updater, not a copy of this executable: this one imports its
        // libraries from the install folder, which is the folder the installer
        // is about to replace.
        let beside = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|d| d.join("kuvatin-updater.exe")))
            .context("could not find kuvatin-updater.exe beside this program")?;
        let helper = dir.join("kuvatin-updater.exe");
        std::fs::copy(&beside, &helper).with_context(|| {
            format!(
                "could not copy {} to {}",
                beside.display(),
                helper.display()
            )
        })?;
        Ok(Staged { msi, helper })
    })();

    if staged.is_err() {
        sweep(&dir);
    }
    staged
}

/// Start the staged updater, then the caller quits. `relaunch` is the
/// executable to start afterwards, normally this one's own path.
pub fn hand_off(staged: &Staged, relaunch: &Path) -> Result<()> {
    std::process::Command::new(&staged.helper)
        .arg("--apply-update")
        .arg(&staged.msi)
        .arg("--after")
        .arg(std::process::id().to_string())
        .arg("--relaunch")
        .arg(relaunch)
        .spawn()
        .with_context(|| format!("could not start {}", staged.helper.display()))?;
    Ok(())
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

    /// Same guard as the uninstall report, for the same reason: these lines
    /// reach a log and a message box through Windows tooling that does not
    /// always read UTF-8.
    ///
    /// This deliberately does not check `stage_dir()`'s path: it runs
    /// through `std::env::temp_dir()`, which echoes the OS-supplied user
    /// profile name, so it is data Windows hands us rather than a message
    /// this module composes. There is nothing for this guard to check there.
    #[test]
    fn every_message_this_module_can_produce_is_ascii() {
        let dir = std::env::temp_dir().join("kuvatin-ascii-check");
        let msi = dir.join("kuvatin-9.9.9-x86_64.msi");
        let said = accept(&msi, "", "kuvatin-9.9.9-x86_64.msi")
            .expect_err("no checksum")
            .to_string();
        assert!(said.is_ascii(), "{said:?}");
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
