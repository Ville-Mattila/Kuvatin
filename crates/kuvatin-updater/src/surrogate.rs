//! Letting go of the Windows 11 menu handler before the installer runs.
//!
//! The "Kuvatin" entry in Explorer's context menu is `kuvatin_shellext.dll`,
//! hosted in a COM Surrogate (`dllhost.exe`), never in Explorer itself. That
//! surrogate lives as long as Explorer holds the menu object, which it can do
//! long after the right-click. While it does, the DLL is open, and the
//! installer's Restart Manager check finds it: "COM Surrogate", an application
//! Restart Manager cannot close (measured: type `RmUnknownApp`, not
//! restartable). Under `/qb` that stops the update.
//!
//! So before the installer runs, this asks Restart Manager the same question
//! the installer will, and ends the surrogates holding the DLL. The upgrade
//! removes the menu's package and registers it again anyway, so the handler
//! is loaded afresh after it either way; ending the surrogate first only
//! moves that before the check that cannot cope with it. Only a
//! `dllhost.exe` still holding the DLL is ended, never anything else.

use std::path::Path;

/// A process Restart Manager says holds a file open. The tests' view of
/// what `release` acts on.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    /// What Restart Manager calls it ("COM Surrogate").
    pub name: String,
}

/// What releasing the menu handler did, for the log.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Released {
    /// Surrogates that held the DLL and were ended.
    pub ended: Vec<u32>,
    /// Holders left alone, and why (not a surrogate, gone, not ours to end).
    pub left: Vec<(u32, String)>,
}

/// Whether a process holding the handler is a COM Surrogate, and so ours to
/// end: its image is `dllhost.exe`. Nothing else is ever ended, whatever it
/// holds.
pub fn is_com_surrogate(image: &Path) -> bool {
    image
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("dllhost.exe"))
}

/// The processes Restart Manager says hold `file` open. Empty when none do,
/// or when Restart Manager cannot be asked.
#[cfg(all(test, windows))]
pub fn holders_of(file: &Path) -> Vec<Holder> {
    rm::list(file)
        .iter()
        .map(|info| Holder {
            pid: info.Process.dwProcessId,
            name: rm::text(&info.strAppName),
        })
        .collect()
}

/// End the COM Surrogates holding `dll`, and wait for each to go.
#[cfg(windows)]
pub fn release(dll: &Path) -> Released {
    let mut released = Released::default();
    for info in rm::list(dll) {
        let pid = info.Process.dwProcessId;
        match rm::end_if_surrogate(&info) {
            Ok(()) => released.ended.push(pid),
            Err(why) => released.left.push((pid, why)),
        }
    }
    released
}

/// Restart Manager and the process calls behind `holders_of` and `release`.
#[cfg(windows)]
mod rm {
    use super::is_com_surrogate;
    use std::path::{Path, PathBuf};
    use windows::core::{HSTRING, PCWSTR, PWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, BOOL, ERROR_MORE_DATA, ERROR_SUCCESS, FILETIME, HANDLE, WAIT_OBJECT_0,
    };
    use windows::Win32::System::RestartManager::{
        RmEndSession, RmGetList, RmRegisterResources, RmStartSession, CCH_RM_SESSION_KEY,
        RM_PROCESS_INFO,
    };
    use windows::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
        WaitForSingleObject, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    /// How long to wait for an ended surrogate to be gone.
    const WAIT_FOR_EXIT_MS: u32 = 5_000;

    /// A NUL-terminated UTF-16 buffer as text.
    #[cfg(test)]
    pub(super) fn text(wide: &[u16]) -> String {
        let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
        String::from_utf16_lossy(&wide[..end])
    }

    /// Restart Manager's list of the processes holding `file`: the question
    /// the installer asks before it replaces the file. Empty when nothing
    /// holds it, or when Restart Manager could not be asked.
    pub(super) fn list(file: &Path) -> Vec<RM_PROCESS_INFO> {
        let mut session = 0u32;
        let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
        unsafe {
            if RmStartSession(&mut session, 0, PWSTR(key.as_mut_ptr())) != ERROR_SUCCESS {
                return Vec::new();
            }
            let wide = HSTRING::from(file.as_os_str());
            let mut infos: Vec<RM_PROCESS_INFO> = Vec::new();
            let mut answered = false;
            if RmRegisterResources(session, Some(&[PCWSTR(wide.as_ptr())]), None, None)
                == ERROR_SUCCESS
            {
                // Ask for the count, make room, ask again. A holder can start
                // between the two, so a few rounds, each with room to spare.
                for _ in 0..4 {
                    let (mut needed, mut count, mut reasons) = (0u32, infos.len() as u32, 0u32);
                    let room = if infos.is_empty() {
                        None
                    } else {
                        Some(infos.as_mut_ptr())
                    };
                    let asked = RmGetList(session, &mut needed, &mut count, room, &mut reasons);
                    if asked == ERROR_SUCCESS {
                        infos.truncate(count as usize);
                        answered = true;
                        break;
                    }
                    if asked != ERROR_MORE_DATA {
                        break;
                    }
                    infos.resize(needed as usize + 2, RM_PROCESS_INFO::default());
                }
            }
            let _ = RmEndSession(session);
            if answered {
                infos
            } else {
                Vec::new()
            }
        }
    }

    /// End the process `info` names, if it is still that process and it is a
    /// COM Surrogate, and wait for it to go. `Err` says why it was left.
    pub(super) fn end_if_surrogate(info: &RM_PROCESS_INFO) -> Result<(), String> {
        let pid = info.Process.dwProcessId;
        unsafe {
            let process = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
                BOOL::from(false),
                pid,
            )
            .map_err(|e| format!("could not open it: {e}"))?;
            let outcome = end_opened(process, info);
            let _ = CloseHandle(process);
            outcome
        }
    }

    unsafe fn end_opened(process: HANDLE, info: &RM_PROCESS_INFO) -> Result<(), String> {
        // Process ids are reused: only the process Restart Manager saw, with
        // the start time it saw, is the one holding the file.
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user)
            .map_err(|e| format!("could not read its start time: {e}"))?;
        let seen = info.Process.ProcessStartTime;
        if (created.dwLowDateTime, created.dwHighDateTime)
            != (seen.dwLowDateTime, seen.dwHighDateTime)
        {
            return Err("a different process now has that id".to_string());
        }
        let image = image_of(process)?;
        if !is_com_surrogate(&image) {
            return Err(format!("not a COM Surrogate ({})", image.display()));
        }
        TerminateProcess(process, 1).map_err(|e| format!("could not end it: {e}"))?;
        if WaitForSingleObject(process, WAIT_FOR_EXIT_MS) != WAIT_OBJECT_0 {
            return Err("ended, but it had not gone after five seconds".to_string());
        }
        Ok(())
    }

    unsafe fn image_of(process: HANDLE) -> Result<PathBuf, String> {
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .map_err(|e| format!("could not read its image: {e}"))?;
        Ok(PathBuf::from(String::from_utf16_lossy(
            &buf[..len as usize],
        )))
    }
}

#[cfg(not(windows))]
pub fn release(_dll: &Path) -> Released {
    Released::default()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    #[test]
    fn only_the_com_surrogate_is_ever_ended() {
        assert!(is_com_surrogate(Path::new(
            r"C:\Windows\System32\dllhost.exe"
        )));
        assert!(is_com_surrogate(Path::new(
            r"C:\WINDOWS\system32\DllHost.EXE"
        )));
        assert!(!is_com_surrogate(Path::new(r"C:\Windows\explorer.exe")));
        assert!(!is_com_surrogate(Path::new(
            r"C:\Program Files\kuvatin\bin\kuvatin.exe"
        )));
        assert!(!is_com_surrogate(Path::new(r"C:\x\dllhost.exe.bak")));
        assert!(!is_com_surrogate(Path::new("")));
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kuvatin-surrogate-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join("held.bin")
    }

    /// The same question the installer asks, answered for a file this test
    /// holds open itself.
    #[test]
    fn restart_manager_names_the_process_holding_a_file() {
        let path = scratch("held");
        let held = std::fs::File::create(&path).expect("hold the file");
        let holders = holders_of(&path);
        assert!(
            holders.iter().any(|h| h.pid == std::process::id()),
            "{holders:?}"
        );
        drop(held);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_holder_that_is_not_a_surrogate_is_left_alone() {
        let path = scratch("alone");
        let held = std::fs::File::create(&path).expect("hold the file");
        let released = release(&path);
        assert!(released.ended.is_empty(), "{released:?}");
        assert!(
            released
                .left
                .iter()
                .any(|(pid, _)| *pid == std::process::id()),
            "{released:?}"
        );
        drop(held);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The real thing, on a machine with Kuvatin installed and its Windows 11
    /// menu registered: have COM start the menu handler's surrogate, the way
    /// Explorer does on a right-click, and hold it; then release it as the
    /// updater will, and nothing holds the DLL any more.
    #[test]
    #[ignore = "needs an installed Kuvatin with its Windows 11 menu registered"]
    fn the_surrogate_holding_the_installed_menu_handler_is_ended() {
        let dll = Path::new(r"C:\Program Files\kuvatin\bin\kuvatin_shellext.dll");
        let mut keeper = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "$o = [Activator]::CreateInstance([Type]::GetTypeFromCLSID(\
                 [Guid]'7a3e2b6c-9d14-4f58-8b2a-1c6e5d4f3a90')); Start-Sleep 120",
            ])
            .spawn()
            .expect("start a process that holds the menu object");
        let deadline = Instant::now() + Duration::from_secs(20);
        while holders_of(dll).is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
        }
        let before = holders_of(dll);
        assert!(
            before.iter().any(|h| h.name == "COM Surrogate"),
            "the handler should be held by its surrogate: {before:?}"
        );

        let released = release(dll);
        let _ = keeper.kill();
        let _ = keeper.wait();
        assert!(!released.ended.is_empty(), "{released:?}");
        assert!(holders_of(dll).is_empty(), "{:?}", holders_of(dll));
    }
}
