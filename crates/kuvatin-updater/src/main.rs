//! The updater: it waits for Kuvatin to close, runs the installer Kuvatin
//! staged, and starts Kuvatin again. Kuvatin copies this program into the
//! staging folder and hands over to it there, because an installer cannot
//! replace a running executable.
//!
//! It is its own program rather than a copy of `kuvatin.exe` because the app
//! statically imports the GLib and GStreamer libraries the installer places
//! beside it. A lone copy in the staging folder has none of them, and Windows
//! refuses to start it; pointing it back at the install folder for them would
//! hold those libraries open from the very directory the installer is about to
//! replace, which is the problem this whole arrangement exists to avoid.
//!
//! So this program depends on nothing but Windows. No GStreamer, no argument
//! parser, no error library: every dependency is a file that would have to be
//! beside it, and there is nothing beside it.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Where a failed install sends the user. Duplicated from `kuvatin`'s update
/// module on purpose: one line of text is a better price than a dependency.
const RELEASES_URL: &str = "https://github.com/Ville-Mattila/Kuvatin/releases/latest";

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
             You can install by hand from {RELEASES_URL}"
        ),
    }
}

/// How long the updater waits for the app to go before installing anyway.
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

/// Wait, install, and start the app again. Returns the process exit code: 0
/// when the update installed, 1 when it did not.
pub fn run_helper(msi: &Path, after: u32, relaunch: Option<&Path>) -> i32 {
    log(&format!(
        "update: waiting for process {after}, then installing {}",
        msi.display()
    ));
    if !wait_for_exit(after, WAIT_FOR_APP) {
        log("update: gave up waiting; installing anyway");
    }

    let status = std::process::Command::new("msiexec")
        .args(msiexec_args(msi))
        .status();
    let outcome = match status {
        Ok(s) => install_outcome(s.code().unwrap_or(-1) as u32),
        Err(e) => {
            let said = format!("could not start the installer: {e}");
            log(&format!("update: {said}"));
            report_failure(&said);
            return 1;
        }
    };
    log(&format!("update: {}", describe(outcome)));

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
            log(&format!("update: {said}"));
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

/// Append one line to the file `kuvatin`'s `applog` writes,
/// `%LOCALAPPDATA%\Kuvatin\kuvatin.log`, in its shape
/// (`2026-09-16T13:04:05Z pid=1234 <line>`), so the whole trail of an update
/// stays in one place. Best-effort: a locked file must never stop an install.
/// `applog` resolves the folder through the shell's known-folder API and
/// rotates the file at a megabyte; neither is worth a dependency here, and the
/// app rotates the same file at its next start anyway.
fn log(line: &str) {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("Kuvatin");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("kuvatin.log"))
    {
        let _ = writeln!(f, "{} pid={} {line}", timestamp(), std::process::id());
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` from the system clock, without a date crate
/// (days-to-civil per Howard Hinnant). The same routine `applog` uses, so the
/// lines this program writes sort in with the app's.
fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// A command line this program can act on.
#[derive(Debug, PartialEq, Eq)]
struct Parsed {
    msi: PathBuf,
    after: u32,
    relaunch: Option<PathBuf>,
}

/// Three flags, parsed by hand: this program exists to start when nothing is
/// beside it, and a command line parser is a dependency it does not need. The
/// `Err` is the complaint to print before exiting 2.
fn parse(args: impl Iterator<Item = OsString>) -> Result<Parsed, String> {
    let mut msi: Option<PathBuf> = None;
    let mut after: Option<u32> = None;
    let mut relaunch: Option<PathBuf> = None;
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.to_string_lossy().as_ref() {
            "--apply-update" => msi = args.next().map(PathBuf::from),
            "--after" => after = args.next().and_then(|v| v.to_string_lossy().parse().ok()),
            "--relaunch" => relaunch = args.next().map(PathBuf::from),
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    match (msi, after) {
        (Some(msi), Some(after)) => Ok(Parsed {
            msi,
            after,
            relaunch,
        }),
        _ => Err("--apply-update <MSI> and --after <PID> are both required".to_string()),
    }
}

/// Usage: kuvatin-updater --apply-update <MSI> --after <PID> [--relaunch <EXE>]
///
/// Started by Kuvatin from the staging folder as it closes. A command line it
/// cannot act on is exit 2, so the mistake is visible rather than mistaken for
/// an install that failed.
fn main() {
    match parse(std::env::args_os().skip(1)) {
        Ok(asked) => std::process::exit(run_helper(
            &asked.msi,
            asked.after,
            asked.relaunch.as_deref(),
        )),
        Err(complaint) => {
            eprintln!("kuvatin-updater: {complaint}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Result<Parsed, String> {
        parse(args.iter().map(OsString::from))
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

    /// Same guard as the uninstall report, for the same reason: these lines
    /// reach kuvatin.log, a message box and a console through Windows tooling
    /// that does not always read UTF-8.
    #[test]
    fn every_word_the_helper_can_print_is_ascii() {
        for code in [0u32, 3010, 1602, 1223, 1603, 7] {
            let said = describe(install_outcome(code));
            assert!(said.is_ascii(), "{said:?}");
        }
        for complaint in [
            parsed(&["--wat"]).expect_err("a flag it does not know"),
            parsed(&[]).expect_err("nothing at all"),
        ] {
            assert!(complaint.is_ascii(), "{complaint:?}");
        }
    }

    #[test]
    fn waiting_on_a_process_that_has_already_gone_is_success_not_failure() {
        // A pid that cannot be opened has exited (or never existed), which is
        // exactly the state the updater is waiting for.
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

    #[test]
    fn reads_the_three_flags() {
        let p = parsed(&[
            "--apply-update",
            r"C:\t\k.msi",
            "--after",
            "42",
            "--relaunch",
            r"C:\p\k.exe",
        ])
        .expect("all three");
        assert_eq!(p.msi, PathBuf::from(r"C:\t\k.msi"));
        assert_eq!(p.after, 42);
        assert_eq!(p.relaunch, Some(PathBuf::from(r"C:\p\k.exe")));
    }

    #[test]
    fn relaunch_is_optional_because_the_pipeline_leaves_it_out() {
        let p = parsed(&["--apply-update", r"C:\t\k.msi", "--after", "42"]).expect("two");
        assert_eq!(p.relaunch, None);
    }

    #[test]
    fn refuses_a_command_line_it_cannot_act_on() {
        assert!(
            parsed(&["--apply-update", r"C:\t\k.msi"]).is_err(),
            "no --after"
        );
        assert!(parsed(&["--after", "42"]).is_err(), "no installer");
        assert!(parsed(&["--after", "not-a-pid", "--apply-update", "k.msi"]).is_err());
        assert!(parsed(&["--wat"]).is_err());
    }
}
