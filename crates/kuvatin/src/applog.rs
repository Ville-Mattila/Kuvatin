//! A small append-only log for the headless paths and a crash log for
//! panics. Explorer right-click runs have no console and usually no user
//! watching, so when one goes wrong on another machine this is the only
//! trace: `%LOCALAPPDATA%\Kuvatin\kuvatin.log` (rotated at 1 MB, one
//! generation kept) and `crash.log` next to it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const LOG_MAX_BYTES: u64 = 1_000_000;

/// The log directory (created on demand): the user's local app-data folder,
/// `%LOCALAPPDATA%\Kuvatin`. Resolved through the shell's known-folder API
/// (`directories`) rather than the environment variable, so a process
/// launched with a stripped or foreign environment block (a service, a
/// scheduler, an installer) still logs where the user looks; the variable
/// and then the temp dir are only fallbacks.
pub fn dir() -> PathBuf {
    let base = directories::BaseDirs::new()
        .map(|d| d.data_local_dir().to_path_buf())
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("Kuvatin");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn log_path() -> PathBuf {
    dir().join("kuvatin.log")
}

pub fn crash_path() -> PathBuf {
    dir().join("crash.log")
}

/// Append one line: `2026-09-09T13:04:05Z pid=1234 <line>`. Best-effort —
/// a full disk or a locked file must never take the run down.
pub fn log(line: &str) {
    append(&log_path(), line);
}

/// The launch context worth a log line when a headless run misbehaves on
/// another machine: which exe, and whose environment it inherited.
pub fn context() -> String {
    let var = |k: &str| std::env::var(k).unwrap_or_else(|_| "<unset>".into());
    format!(
        "exe={}, USERNAME={}, LOCALAPPDATA={}",
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "?".into()),
        var("USERNAME"),
        var("LOCALAPPDATA")
    )
}

fn append(path: &Path, line: &str) {
    rotate_if_large(path);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{} pid={} {}", timestamp(), std::process::id(), line);
    }
}

fn rotate_if_large(path: &Path) {
    if std::fs::metadata(path)
        .map(|m| m.len() > LOG_MAX_BYTES)
        .unwrap_or(false)
    {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
}

/// Route panics somewhere visible: the message, location and backtrace go to
/// `crash.log` (and a line to the run log); a panic on the main thread also
/// shows the error dialog, because the windowed build has no stderr and
/// would otherwise just vanish. Worker-thread panics only log — the batch
/// runner already isolates those into per-file errors.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".into());
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("?").to_string();
        let backtrace = std::backtrace::Backtrace::force_capture();
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(crash_path())
        {
            let _ = writeln!(
                f,
                "---- {} Kuvatin {} pid={} thread={}\n{msg}\n  at {location}\n{backtrace}\n",
                timestamp(),
                env!("CARGO_PKG_VERSION"),
                std::process::id(),
                thread_name
            );
        }
        log(&format!(
            "PANIC on thread {thread_name}: {msg} (at {location})"
        ));
        if thread_name == "main" {
            crate::shell::notify_error(
                "Kuvatin \u{2014} internal error",
                &format!(
                    "{msg}\n\nat {location}\n\nDetails were written to {}.",
                    crash_path().display()
                ),
            );
        }
        default(info);
    }));
}

/// `YYYY-MM-DDTHH:MM:SSZ` from the system clock, without a date crate
/// (days-to-civil per Howard Hinnant).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_iso_utc() {
        let t = timestamp();
        assert_eq!(t.len(), 20, "{t}");
        assert!(t.ends_with('Z') && t.as_bytes()[10] == b'T', "{t}");
        assert!(t.starts_with("20"), "{t}");
    }

    #[test]
    fn log_lines_append_and_rotate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kuvatin.log");
        append(&path, "first");
        append(&path, "second");
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(
            text.contains("pid=") && text.ends_with("second\n"),
            "{text}"
        );
        // Grow it past the cap: the next write rotates it away first.
        std::fs::write(&path, vec![b'x'; LOG_MAX_BYTES as usize + 1]).unwrap();
        append(&path, "after rotation");
        assert!(path.with_extension("log.1").exists());
        let fresh = std::fs::read_to_string(&path).unwrap();
        assert_eq!(fresh.lines().count(), 1);
    }
}
