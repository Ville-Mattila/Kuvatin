//! Coalesce the processes Explorer launches for a multi-item selection into
//! ONE batch.
//!
//! A classic static context-menu verb (`shell\…\command` with `"%1"`) is
//! invoked once per selected item — only COM handlers (`IDropTarget`,
//! `IExecuteCommand`) receive the whole selection at once. Rather than ship a
//! COM server, every launched process spools its paths into a shared per-group
//! directory and races for a lock file. The winner (the *leader*) waits until
//! new arrivals go quiet, claims every spooled path and runs the batch; the
//! losers (*followers*) exit immediately. Explorer starts the N processes in a
//! quick burst, so a few hundred milliseconds of quiet closes the batch.
//!
//! All coordination is plain filesystem atomics (`create_new` for the lock,
//! `rename` for claiming), so it is process-agnostic and unit-testable with
//! threads standing in for processes.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// What this process should do after the rendezvous.
#[derive(Debug, PartialEq, Eq)]
pub enum Role {
    /// Run the batch over these paths — this process' own plus everything the
    /// followers spooled. Empty when another leader already claimed them all.
    Leader(Vec<PathBuf>),
    /// Another process is leading and has this one's paths — exit.
    Follower,
}

/// Arrivals must be quiet for this long before the leader closes the batch —
/// once a SECOND arrival has shown up. A lone arrival only waits
/// [`FIRST_ARRIVAL_GRACE`]: Explorer launches a burst within tens of
/// milliseconds, so a single-file right-click shouldn't pay the full window.
pub const QUIET: Duration = Duration::from_millis(600);
const FIRST_ARRIVAL_GRACE: Duration = Duration::from_millis(250);
/// A lock or spool entry older than this is debris from a crashed run.
const STALE: Duration = Duration::from_secs(30);
const LOCK: &str = "leader.lock";
const SPOOL_EXT: &str = "paths";

/// Distinguishes spool entries written by the same process (threads in tests).
static SEQ: AtomicU64 = AtomicU64::new(0);

/// Rendezvous with any sibling processes in `group` (e.g. one group per
/// preset, so two different presets never merge). Never fails: if the
/// filesystem won't cooperate this process simply runs its own paths alone.
pub fn gather(group: &str, mine: &[PathBuf], quiet: Duration) -> Role {
    let root = std::env::temp_dir().join("kuvatin").join("rendezvous");
    gather_in(&root, group, mine, quiet)
}

/// [`gather`] under an explicit root directory.
pub fn gather_in(root: &Path, group: &str, mine: &[PathBuf], quiet: Duration) -> Role {
    let dir = root.join(group_dir(group));
    if std::fs::create_dir_all(&dir).is_err() || spool(&dir, mine).is_err() {
        return Role::Leader(mine.to_vec());
    }
    if !acquire_lock(&dir) {
        return Role::Follower;
    }
    // Let the burst settle: the clock restarts whenever a new entry lands. A
    // lone arrival (still just our own entry after the grace period) goes
    // straight ahead instead of waiting out the whole quiet window.
    let started = Instant::now();
    let mut last_change = Instant::now();
    let mut seen = spool_count(&dir);
    loop {
        std::thread::sleep(Duration::from_millis(40));
        let n = spool_count(&dir);
        if n != seen {
            seen = n;
            last_change = Instant::now();
        }
        if seen <= 1 && started.elapsed() >= FIRST_ARRIVAL_GRACE.min(quiet) {
            break;
        }
        if last_change.elapsed() >= quiet {
            break;
        }
    }
    let claim_dir = dir.join(format!(
        "claim-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::create_dir_all(&claim_dir);
    let mut paths = Vec::new();
    claim(&dir, &claim_dir, &mut paths);
    // Release, then sweep once more: a process that spooled just before the
    // release saw the lock and left, so its entry is ours. Anything spooled
    // after the release belongs to the next leader — rename is atomic, so no
    // entry is ever claimed twice.
    let _ = std::fs::remove_file(dir.join(LOCK));
    claim(&dir, &claim_dir, &mut paths);
    let _ = std::fs::remove_dir_all(&claim_dir);
    paths.sort();
    paths.dedup();
    Role::Leader(paths)
}

/// Filesystem-safe directory name for a group (groups are free text).
fn group_dir(group: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    group.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// One spool line per path. Valid-Unicode paths go verbatim (Windows forbids
/// control characters in paths, so `\n` can't occur inside one; a leading `#`
/// is escaped as `#u:`). A path that is NOT valid Unicode — an unpaired
/// surrogate in an NTFS name — is written as `#w:` + hex UTF-16 units, so the
/// leader converts exactly the file the user clicked instead of a lossy
/// `U+FFFD` look-alike that doesn't exist.
fn encode_path(p: &Path) -> String {
    if let Some(s) = p.to_str() {
        return if s.starts_with('#') { format!("#u:{s}") } else { s.to_string() };
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let hex: String = p.as_os_str().encode_wide().map(|u| format!("{u:04x}")).collect();
        return format!("#w:{hex}");
    }
    #[allow(unreachable_code)]
    p.to_string_lossy().into_owned()
}

fn decode_path(line: &str) -> PathBuf {
    if let Some(s) = line.strip_prefix("#u:") {
        return PathBuf::from(s);
    }
    #[cfg(windows)]
    if let Some(hex) = line.strip_prefix("#w:") {
        use std::os::windows::ffi::OsStringExt;
        let units: Vec<u16> = (0..hex.len() / 4)
            .filter_map(|i| u16::from_str_radix(&hex[i * 4..i * 4 + 4], 16).ok())
            .collect();
        return PathBuf::from(std::ffi::OsString::from_wide(&units));
    }
    PathBuf::from(line)
}

/// Write this process' paths as one spool entry. Written under a temporary
/// name and renamed into place so a half-written entry is never claimed.
fn spool(dir: &Path, paths: &[PathBuf]) -> std::io::Result<()> {
    let stem = format!(
        "{}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let tmp = dir.join(format!("{stem}.tmp"));
    let body: String = paths.iter().map(|p| encode_path(p)).collect::<Vec<_>>().join("\n");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, dir.join(format!("{stem}.{SPOOL_EXT}")))
}

fn is_spool_entry(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()) == Some(SPOOL_EXT)
}

fn spool_count(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().filter(|e| is_spool_entry(&e.path())).count())
        .unwrap_or(0)
}

fn age_of(p: &Path) -> Option<Duration> {
    let modified = std::fs::metadata(p).and_then(|m| m.modified()).ok()?;
    SystemTime::now().duration_since(modified).ok()
}

/// Try to become the leader. A lock left by a crashed leader (older than
/// [`STALE`]) is retired and the race re-run once.
///
/// Retirement is an atomic RENAME to a unique name, not a delete: with a
/// delete, two arrivals that both judged the lock stale could each remove
/// and recreate it — the second `remove_file` deleted the first's FRESH lock
/// and produced two leaders. Only one process can win the rename; the other
/// finds a fresh lock on its retry and follows.
fn acquire_lock(dir: &Path) -> bool {
    let lock = dir.join(LOCK);
    for attempt in 0..2 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(_) => return true,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                let stale = age_of(&lock).map(|a| a > STALE).unwrap_or(false);
                if !stale || attempt == 1 {
                    return false;
                }
                let retired = dir.join(format!(
                    "stale-{}-{}",
                    std::process::id(),
                    SEQ.fetch_add(1, Ordering::Relaxed)
                ));
                if std::fs::rename(&lock, &retired).is_ok() {
                    let _ = std::fs::remove_file(&retired);
                }
                // Whether or not we won the rename, retry create_new once.
            }
            // Can't coordinate at all — better to run alone than to drop the action.
            Err(_) => return true,
        }
    }
    false
}

/// Move every spool entry into `claim_dir` (atomic rename — whoever renames
/// first owns it) and read its paths into `out`. Entries older than [`STALE`]
/// are debris from a crashed run and are deleted instead of batched.
fn claim(dir: &Path, claim_dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let src = entry.path();
        if !is_spool_entry(&src) {
            continue;
        }
        if age_of(&src).map(|a| a > STALE).unwrap_or(false) {
            let _ = std::fs::remove_file(&src);
            continue;
        }
        let Some(name) = src.file_name() else {
            continue;
        };
        let dst = claim_dir.join(name);
        if std::fs::rename(&src, &dst).is_err() {
            continue; // another leader got it first
        }
        if let Ok(body) = std::fs::read_to_string(&dst) {
            out.extend(body.lines().filter(|l| !l.is_empty()).map(decode_path));
        }
        let _ = std::fs::remove_file(&dst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn leader_paths(role: &Role) -> Option<&Vec<PathBuf>> {
        match role {
            Role::Leader(v) => Some(v),
            Role::Follower => None,
        }
    }

    /// Eight "processes" arrive in a staggered burst (like Explorer's
    /// CreateProcess loop): exactly one leads and it holds every path, no
    /// path is lost or duplicated, and nothing is left behind.
    #[test]
    fn a_burst_of_processes_yields_one_leader_holding_every_path() {
        let root = tempfile::tempdir().unwrap();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let root = root.path().to_path_buf();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(i as u64 * 15));
                    gather_in(
                        &root,
                        "preset:test",
                        &[p(&format!("C:/img/{i}.png"))],
                        Duration::from_millis(250),
                    )
                })
            })
            .collect();
        let roles: Vec<Role> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        let mut union: Vec<PathBuf> = roles.iter().filter_map(leader_paths).flatten().cloned().collect();
        union.sort();
        let mut expect: Vec<PathBuf> = (0..8).map(|i| p(&format!("C:/img/{i}.png"))).collect();
        expect.sort();
        assert_eq!(union, expect, "every path exactly once: {roles:?}");
        let busy_leaders = roles.iter().filter(|r| leader_paths(r).map_or(false, |v| !v.is_empty())).count();
        assert_eq!(busy_leaders, 1, "one batch, not several: {roles:?}");

        let dir = root.path().join(group_dir("preset:test"));
        assert_eq!(spool_count(&dir), 0, "spool drained");
        assert!(!dir.join(LOCK).exists(), "lock released");
    }

    /// A single right-click shouldn't wait out the whole quiet window: with
    /// no second arrival, the leader goes ahead after the grace period.
    #[test]
    fn a_lone_arrival_does_not_wait_the_full_quiet_window() {
        let root = tempfile::tempdir().unwrap();
        let t0 = Instant::now();
        let role = gather_in(root.path(), "g", &[p("only.png")], Duration::from_millis(1500));
        let took = t0.elapsed();
        assert_eq!(role, Role::Leader(vec![p("only.png")]));
        assert!(took < Duration::from_millis(900), "waited {took:?} for a lone arrival");
    }

    /// Two processes that both judge a lock stale must not both lead: only
    /// the rename winner retires it, the other follows.
    #[test]
    fn a_stale_lock_is_retired_by_exactly_one_process() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(group_dir("g"));
        std::fs::create_dir_all(&dir).unwrap();
        let lock = std::fs::File::create(dir.join(LOCK)).unwrap();
        lock.set_modified(SystemTime::now() - Duration::from_secs(120)).unwrap();
        drop(lock);
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let root = root.path().to_path_buf();
                std::thread::spawn(move || {
                    gather_in(&root, "g", &[p(&format!("{i}.png"))], Duration::from_millis(200))
                })
            })
            .collect();
        let roles: Vec<Role> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let leaders = roles
            .iter()
            .filter(|r| leader_paths(r).map_or(false, |v| !v.is_empty()))
            .count();
        assert_eq!(leaders, 1, "one leader despite the stale lock: {roles:?}");
    }

    /// Sequential runs are independent batches — nothing carries over.
    #[test]
    fn a_later_run_starts_its_own_batch() {
        let root = tempfile::tempdir().unwrap();
        let q = Duration::from_millis(50);
        assert_eq!(gather_in(root.path(), "g", &[p("a.png")], q), Role::Leader(vec![p("a.png")]));
        assert_eq!(gather_in(root.path(), "g", &[p("b.png")], q), Role::Leader(vec![p("b.png")]));
    }

    /// Different groups (presets) never merge, even when concurrent.
    #[test]
    fn groups_are_isolated() {
        let root = tempfile::tempdir().unwrap();
        let r1 = root.path().to_path_buf();
        let r2 = root.path().to_path_buf();
        let a = std::thread::spawn(move || gather_in(&r1, "preset:A", &[p("a.png")], Duration::from_millis(120)));
        let b = std::thread::spawn(move || gather_in(&r2, "preset:B", &[p("b.png")], Duration::from_millis(120)));
        assert_eq!(a.join().unwrap(), Role::Leader(vec![p("a.png")]));
        assert_eq!(b.join().unwrap(), Role::Leader(vec![p("b.png")]));
    }

    /// A lock left by a crashed leader is taken over; a live lock is respected.
    #[test]
    fn a_crashed_leaders_lock_is_taken_over_but_a_live_one_is_not() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(group_dir("g"));
        std::fs::create_dir_all(&dir).unwrap();
        let q = Duration::from_millis(50);

        let lock = std::fs::File::create(dir.join(LOCK)).unwrap();
        lock.set_modified(SystemTime::now() - Duration::from_secs(120)).unwrap();
        drop(lock);
        assert_eq!(gather_in(root.path(), "g", &[p("a.png")], q), Role::Leader(vec![p("a.png")]));

        std::fs::File::create(dir.join(LOCK)).unwrap();
        assert_eq!(gather_in(root.path(), "g", &[p("b.png")], q), Role::Follower);
    }

    /// Spool debris from a crashed run must not be silently converted later.
    #[test]
    fn stale_spool_debris_is_discarded_not_batched() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(group_dir("g"));
        std::fs::create_dir_all(&dir).unwrap();
        let debris = dir.join("999-0-0.paths");
        std::fs::write(&debris, "C:/old/forgotten.png").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&debris)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(120))
            .unwrap();

        let role = gather_in(root.path(), "g", &[p("new.png")], Duration::from_millis(50));
        assert_eq!(role, Role::Leader(vec![p("new.png")]));
        assert!(!debris.exists(), "debris deleted");
    }

    /// Non-ASCII paths (and several per process) survive the spool round trip,
    /// as does a name starting with the escape character.
    #[test]
    fn paths_round_trip_verbatim() {
        let root = tempfile::tempdir().unwrap();
        let mine = [p("C:/Työt/kuva ä.png"), p("D:/render/frame_0001.exr"), p("#w:not-hex.png")];
        let role = gather_in(root.path(), "g", &mine, Duration::from_millis(50));
        let mut expect = mine.to_vec();
        expect.sort();
        assert_eq!(role, Role::Leader(expect));
    }

    /// A path that isn't valid Unicode (an unpaired surrogate in an NTFS name)
    /// used to come back as a `U+FFFD` look-alike that doesn't exist.
    #[cfg(windows)]
    #[test]
    fn a_non_unicode_path_survives_the_spool() {
        use std::os::windows::ffi::OsStringExt;
        let units: Vec<u16> = "C:\\odd\\".encode_utf16().chain([0xD800, 'x' as u16]).collect();
        let odd = PathBuf::from(std::ffi::OsString::from_wide(&units));
        assert!(odd.to_str().is_none(), "fixture must be non-Unicode");
        let root = tempfile::tempdir().unwrap();
        let role = gather_in(root.path(), "g", &[odd.clone()], Duration::from_millis(50));
        assert_eq!(role, Role::Leader(vec![odd]));
    }
}
