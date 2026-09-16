//! Staging an installer, handing off to a copy of this executable, and that
//! copy's own run. See the design doc for why the copy exists.

use super::{asset_name, asset_urls, fetch, verify};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

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
pub fn sweep_stage() {
    if let Ok(dir) = stage_dir() {
        sweep(&dir);
    }
}

/// What a finished download left behind.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installed {
    Yes,
    /// The elevation prompt was declined. Nothing was changed.
    Declined,
    Failed(u32),
}

/// 3010 is "installed, reboot when you like", which is still installed. 1602
/// is "user cancelled" and 1223 is "the elevation prompt was refused".
pub fn install_outcome(code: u32) -> Installed {
    match code {
        0 | 3010 => Installed::Yes,
        1602 | 1223 => Installed::Declined,
        other => Installed::Failed(other),
    }
}

/// One ASCII line about an outcome, for the log and the message box.
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

/// How long the helper waits for the app to go before installing anyway.
const WAIT_FOR_APP: Duration = Duration::from_secs(30);

/// Wait for a process to exit. `true` when it has gone (including when it was
/// already gone), `false` when the deadline passed first.
#[cfg(windows)]
pub fn wait_for_exit(pid: u32, limit: Duration) -> bool {
    use windows::Win32::Foundation::{CloseHandle, BOOL, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE, BOOL::from(false), pid) else {
            // Not openable: it has exited, or it never existed. Either way
            // there is nothing left holding the files we are replacing.
            return true;
        };
        let waited =
            WaitForSingleObject(handle, limit.as_millis().min(u128::from(u32::MAX)) as u32);
        let _ = CloseHandle(handle);
        waited == WAIT_OBJECT_0
    }
}

#[cfg(not(windows))]
pub fn wait_for_exit(_pid: u32, _limit: Duration) -> bool {
    true
}

/// `/qb` shows a small progress window, so an elevation prompt and a slow
/// install are not a silent freeze.
fn msiexec_args(msi: &Path) -> Vec<String> {
    vec![
        "/i".to_string(),
        msi.display().to_string(),
        "/qb".to_string(),
        "/norestart".to_string(),
    ]
}

/// Start the staged copy as the updater, then the caller quits. `relaunch` is
/// the executable to start afterwards, normally this one's own path.
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

/// The `--apply-update` mode. Returns the process exit code: 0 when the
/// update installed, 1 when it did not.
pub fn run_helper(msi: &Path, after: u32, relaunch: Option<&Path>) -> i32 {
    crate::applog::log(&format!(
        "update: waiting for process {after}, then installing {}",
        msi.display()
    ));
    if !wait_for_exit(after, WAIT_FOR_APP) {
        crate::applog::log("update: gave up waiting; installing anyway");
    }

    let status = std::process::Command::new("msiexec")
        .args(msiexec_args(msi))
        .status();
    let outcome = match status {
        Ok(s) => install_outcome(s.code().unwrap_or(-1) as u32),
        Err(e) => {
            let said = format!("could not start the installer: {e}");
            crate::applog::log(&format!("update: {said}"));
            report_failure(&said);
            return 1;
        }
    };
    crate::applog::log(&format!("update: {}", describe(outcome)));

    if outcome != Installed::Yes {
        report_failure(&describe(outcome));
        // The app is gone, so put it back the way it was.
        if let Some(exe) = relaunch {
            let _ = std::process::Command::new(exe).spawn();
        }
        return 1;
    }

    let _ = std::fs::remove_file(msi);
    if let Some(exe) = relaunch {
        if let Err(e) = std::process::Command::new(exe).spawn() {
            let said = format!(
                "the update installed, but Kuvatin did not start again: {e}. \
                 Start it from the Start menu."
            );
            crate::applog::log(&format!("update: {said}"));
            report_failure(&said);
            return 1;
        }
    }
    0
}

/// No window is left by this point, so failures go to a message box as well
/// as the log.
#[cfg(windows)]
fn report_failure(text: &str) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONWARNING, MB_OK};
    let body = HSTRING::from(text);
    let title = HSTRING::from("Kuvatin update");
    unsafe {
        MessageBoxW(None, &body, &title, MB_OK | MB_ICONWARNING);
    }
}

#[cfg(not(windows))]
fn report_failure(text: &str) {
    eprintln!("{text}");
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
        let mut said: Vec<String> = Vec::new();
        for code in [0u32, 3010, 1602, 1223, 1603] {
            said.push(describe(install_outcome(code)));
        }
        said.push(
            accept(&msi, "", "kuvatin-9.9.9-x86_64.msi")
                .expect_err("no checksum")
                .to_string(),
        );
        for line in &said {
            assert!(line.is_ascii(), "{line:?}");
        }
    }

    #[test]
    fn waiting_on_a_process_that_has_already_gone_is_success_not_failure() {
        // A pid that cannot be opened has exited (or never existed), which is
        // exactly the state the helper is waiting for.
        assert!(wait_for_exit(
            0xFFFF_FFF0,
            std::time::Duration::from_millis(50)
        ));
    }

    #[test]
    fn waiting_on_ourselves_gives_up_at_the_deadline() {
        let started = std::time::Instant::now();
        let waited = wait_for_exit(std::process::id(), std::time::Duration::from_millis(200));
        assert!(!waited, "we are still running, so the wait must time out");
        assert!(started.elapsed() >= std::time::Duration::from_millis(150));
    }

    #[test]
    fn the_command_line_it_runs_names_the_installer_and_nothing_else() {
        let msi = Path::new(r"C:\Users\x\AppData\Local\Temp\kuvatin\update\k.msi");
        let args = msiexec_args(msi);
        assert_eq!(args[0], "/i");
        assert_eq!(Path::new(&args[1]), msi);
        assert!(args.contains(&"/qb".to_string()), "{args:?}");
        assert!(args.contains(&"/norestart".to_string()), "{args:?}");
        // No REINSTALLMODE: the package's MajorUpgrade handles an upgrade, and
        // forcing a reinstall mode here would fight it.
        assert!(
            !args.iter().any(|a| a.starts_with("REINSTALLMODE")),
            "{args:?}"
        );
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
