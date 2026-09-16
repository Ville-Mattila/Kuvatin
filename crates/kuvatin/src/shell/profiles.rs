//! Enumerate the real user profiles from
//! `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList`, so the
//! all-users uninstall can visit each one's classes hive and files.
//!
//! `all()` is called by `super::allusers`, the `--unregister-all-users` entry
//! point, which is the one thing that has a reason to walk every account.

use std::os::windows::fs::MetadataExt;
use std::path::PathBuf;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS,
};
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY_LOCAL_MACHINE, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
};

use super::regutil::{enum_subkeys, wide};

const PROFILE_LIST: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList";

/// A `ProfileImagePath` longer than this is not a path, so stop growing the
/// buffer for it.
const MAX_VALUE_CHARS: usize = 64 * 1024;

/// `FILE_ATTRIBUTE_REPARSE_POINT`, spelled out because the `windows` crate
/// exports it from a file-system namespace this crate does not otherwise need.
pub(super) const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Profile {
    pub sid: String,
    pub dir: PathBuf,
}

/// A real, cleanable end-user account SID: a local/domain account
/// (`S-1-5-21-...`) or an Entra ID account (`S-1-12-1-...`). Service SIDs
/// (`S-1-5-18/19/20`), the `.bak` temp-profile markers and anything else are
/// rejected.
pub(super) fn is_cleanup_sid(sid: &str) -> bool {
    if sid.ends_with(".bak") {
        return false;
    }
    let known_prefix = sid.starts_with("S-1-5-21-") || sid.starts_with("S-1-12-1-");
    known_prefix
        && sid
            .chars()
            .all(|c| c.is_ascii_digit() || c == '-' || c == 'S')
}

/// Read the `ProfileImagePath` of a ProfileList entry unexpanded, then expand
/// the environment strings ourselves.
///
/// `Err` says which way it failed, in words fit to print and naming no SID (the
/// caller puts that in front). Absent and refused are kept apart for the same
/// reason `enum_subkeys` keeps them apart: a `ProfileList` entry with no
/// `ProfileImagePath` is a stub that never was a profile, while one we are not
/// allowed to read is a real account whose files this uninstall will not find,
/// and whoever reads the log can act on the second.
fn profile_dir(sid: &str) -> Result<PathBuf, String> {
    let subkey = wide(&format!(r"{PROFILE_LIST}\{sid}"));
    let name = wide("ProfileImagePath");
    // Roomy enough for any real profile path; grown and asked again if a hive
    // ever says otherwise.
    let mut buf = vec![0u16; 512];
    loop {
        let mut cb = (buf.len() * 2) as u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                PCWSTR(subkey.as_ptr()),
                PCWSTR(name.as_ptr()),
                // ProfileImagePath is REG_EXPAND_SZ; accept both types and do
                // not auto-expand (we expand with the machine environment
                // ourselves).
                RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut cb),
            )
        };
        if status == ERROR_MORE_DATA {
            // `cb` is the size the value needs, in bytes.
            let needed = (cb as usize).div_ceil(2);
            if needed > MAX_VALUE_CHARS {
                return Err(format!(
                    "its ProfileImagePath does not fit in {MAX_VALUE_CHARS} characters"
                ));
            }
            // Asked for more room and then named no more than we had already
            // offered: growing the buffer would change nothing, so say what
            // happened rather than go round again for ever.
            if needed <= buf.len() {
                return Err(format!(
                    "reading its ProfileImagePath wanted more than {} characters and then asked for no more",
                    buf.len()
                ));
            }
            buf = vec![0u16; needed];
            continue;
        }
        if status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND {
            return Err("it has no ProfileImagePath value".to_string());
        }
        if status == ERROR_ACCESS_DENIED {
            return Err(format!(
                "we are not allowed to read its ProfileImagePath (error {})",
                status.0
            ));
        }
        if status != ERROR_SUCCESS {
            return Err(format!(
                "its ProfileImagePath would not read (error {})",
                status.0
            ));
        }
        let units = (cb as usize / 2).min(buf.len());
        // The value carries its own terminating NUL, and one written with a
        // second one would otherwise leave a NUL inside the path — which no
        // file API would match, and which would read as a truncated path in
        // any message about it.
        let value = &buf[..units];
        let value = &value[..value.iter().position(|&u| u == 0).unwrap_or(value.len())];
        let raw = String::from_utf16_lossy(value);
        return Ok(PathBuf::from(expand_env(&raw)));
    }
}

/// The only two tokens Windows writes into a `ProfileImagePath`.
const EXPANDABLE: &[&str] = &["SystemDrive", "SystemRoot"];

/// Expand the tokens a `ProfileImagePath` is written with, from the process
/// environment — which for the SYSTEM installer holds the machine variables
/// those paths are written against.
///
/// Only [`EXPANDABLE`] is expanded, and anything else is left exactly as it
/// stands. This runs as SYSTEM over paths from a machine-wide key and hands
/// back a directory that will then be deleted from, so expanding whatever name
/// a value happens to contain — out of *our* environment, which is not the
/// environment it was written against — is a way to be pointed somewhere else
/// entirely. A token left standing yields a path that simply does not exist,
/// which `vet_dir` reports and skips.
///
/// Matched without regard to case, as the registry and the environment both
/// are: `%systemdrive%` appears in the wild as often as `%SystemDrive%`.
fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('%') else {
            out.push('%');
            return out + after;
        };
        let var = &after[..end];
        let value = EXPANDABLE
            .iter()
            .find(|known| known.eq_ignore_ascii_case(var))
            .and_then(|known| std::env::var(known).ok());
        match value {
            Some(v) => out.push_str(&v),
            None => {
                out.push('%');
                out.push_str(var);
                out.push('%');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Vet a directory before it is handed to anything that deletes inside it.
///
/// Split out from the read so it can be tested against a real junction —
/// `a_junction_is_not_a_profile_directory` does exactly that.
///
/// This vets one path: the profile directory itself, where a file walk would
/// start. The walk below it vets every path it takes for itself, so this is
/// the one step that has to happen before it begins.
///
/// Names no account, like `profile_dir`: the SID belongs to the caller that
/// knows it, and `cleanable_dir` puts it in front of either reason.
fn vet_dir(dir: PathBuf) -> Result<PathBuf, String> {
    let shown = dir.display();
    // `symlink_metadata`, so a reparse point is reported as itself rather than
    // as whatever it points at. Do not simplify this back to `Path::is_dir()`:
    // that follows a junction and answers `true`, so a junction would sail
    // through as an ordinary directory. Measured on a real one: `is_dir()`
    // true, `symlink_metadata().is_symlink()` true, attributes `0x410`.
    let meta = std::fs::symlink_metadata(&dir).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("{shown} does not exist")
        } else {
            format!("{shown} would not open ({e})")
        }
    })?;
    if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(format!(
            "{shown} is a reparse point, not a profile directory; deleting \
             through it would reach files outside the profile"
        ));
    }
    if !meta.is_dir() {
        return Err(format!("{shown} is not a directory"));
    }
    Ok(dir)
}

/// Where one profile's files are, or why we will not touch it.
///
/// Every `Err` here is something to say out loud rather than to pass over: a
/// cleanable account whose directory we cannot pin down is an account the
/// uninstall will not finish cleaning, and whoever ran it should be told which.
///
/// The one place the SID goes in front, so every reason about an account is
/// shaped the same way whichever step produced it.
fn cleanable_dir(sid: &str) -> Result<PathBuf, String> {
    profile_dir(sid)
        .and_then(vet_dir)
        .map_err(|why| format!("{sid}: {why}"))
}

/// Every cleanable profile whose directory we can stand behind, everything that
/// went wrong on the way, and which accounts that cost us.
///
/// Same shape as `verbs::subkeys_to_delete` and for the same reason: an
/// all-users uninstall that read a partial `ProfileList` and quietly treated
/// that as "no profiles" would leave every real account's classes hive
/// untouched while calling the machine clean. So the caller gets everything
/// this could read, plus the reasons when it is not everything, rather than a
/// short list passed off as a complete one.
///
/// The trouble gathers three kinds of line: an enumeration that would not read,
/// a list with nothing in it at all, and each individual account this had to
/// pass over — because an account skipped in silence is precisely the one that
/// keeps its menu. Accounts that are *meant* to be passed over, the service SIDs
/// and the `.bak` markers, say nothing: they are not trouble, and a line each
/// would bury the ones that are.
///
/// One line per entry rather than one joined string, and the skipped SIDs as
/// SIDs. The caller prints these and counts them, and both go wrong on a joined
/// string: a reason may carry a `"; "` of its own (`enum_children`'s
/// `"…(error 5); read 6 before it"` does), so splitting it back apart cuts a
/// sentence in half, and counting accounts by looking for a SID at the start of
/// a line is guesswork about text this module already knows the answer to.
pub(super) fn all() -> (Vec<Profile>, Vec<String>, Vec<String>) {
    let (sids, mut trouble) = match enum_subkeys(HKEY_LOCAL_MACHINE, PROFILE_LIST) {
        Ok(sids) => (sids, Vec::new()),
        Err(why) => (Vec::new(), vec![why]),
    };
    // Windows keeps the three service profiles (S-1-5-18/19/20) in this key on
    // every machine there is, so an empty list is never the truth — it is a
    // read that went wrong without saying so.
    if sids.is_empty() && trouble.is_empty() {
        trouble.push(format!(
            "{PROFILE_LIST} listed no accounts at all, which no Windows machine does"
        ));
    }
    let mut profiles = Vec::new();
    let mut skipped = Vec::new();
    for sid in sids {
        if !is_cleanup_sid(&sid) {
            continue;
        }
        match cleanable_dir(&sid) {
            Ok(dir) => profiles.push(Profile { sid, dir }),
            Err(why) => {
                trouble.push(why);
                skipped.push(sid);
            }
        }
    }
    (profiles, trouble, skipped)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::skip_or_fail_on_ci;
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A scratch directory tree under the temp directory, unique to this run
    /// and removed when the test ends — pass, fail or panic.
    struct TempTree {
        root: PathBuf,
    }

    impl TempTree {
        fn new() -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "kuvatin-profiles-test-{}-{nanos}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            // `create_dir`, not `create_dir_all`: it fails if the name is
            // already taken, which is how this run knows the tree is its own
            // to delete on the way out.
            std::fs::create_dir(&root).expect("create the temp tree");
            TempTree { root }
        }

        fn at(&self, rest: &str) -> PathBuf {
            self.root.join(rest)
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            for entry in std::fs::read_dir(&self.root)
                .into_iter()
                .flatten()
                .flatten()
            {
                let path = entry.path();
                let meta = match std::fs::symlink_metadata(&path) {
                    Ok(meta) => meta,
                    Err(e) => {
                        eprintln!("could not stat {} ({e})", path.display());
                        continue;
                    }
                };
                let swept = if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    // `remove_dir` says exactly what is meant here: remove the
                    // link, never what it points at. (`remove_dir_all` was
                    // measured on rustc 1.96 to do the same — it takes the link
                    // and leaves the target — but nothing here should rest on
                    // that, and this way the code states the intent.)
                    std::fs::remove_dir(&path)
                } else if meta.is_dir() {
                    std::fs::remove_dir_all(&path)
                } else {
                    // Nothing here makes plain files today; if something ever
                    // does, it goes too rather than keeping the root alive.
                    std::fs::remove_file(&path)
                };
                if let Err(e) = swept {
                    eprintln!("could not remove {} ({e})", path.display());
                }
            }
            // Say so loudly rather than leaving a directory behind in silence:
            // the next run would not reuse this name, so nobody would notice.
            if let Err(e) = std::fs::remove_dir(&self.root) {
                eprintln!(
                    "temp tree {} survived cleanup ({e}); remove it by hand",
                    self.root.display()
                );
            }
        }
    }

    /// A junction is a directory to `Path::is_dir()` and a reparse point to
    /// `symlink_metadata`, which is the whole reason the check is written the
    /// way it is: deleting a profile's files through one would reach whatever
    /// it points at, anywhere on the machine.
    #[test]
    fn a_junction_is_not_a_profile_directory() {
        let tree = TempTree::new();
        let target = tree.at("target");
        let link = tree.at("link");
        std::fs::create_dir(&target).expect("create the target directory");

        // Junctions need no privilege, unlike symbolic links, so this should
        // work anywhere.
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output();
        if made.is_err() || !link.exists() {
            // One rule for every self-skipping test in the crate, and it lives
            // in `test_support`: locally a line, on CI a failure, because a
            // gate that ran none of these would be green and worth nothing.
            skip_or_fail_on_ci(&format!(
                "could not create a junction at {} ({made:?})",
                link.display()
            ));
            return;
        }

        // The measurement the comment in `vet_dir` records, asserted here so it
        // stays true: this is why `is_dir()` alone would not do.
        assert!(link.is_dir(), "a junction answers is_dir() with true");
        let meta = std::fs::symlink_metadata(&link).expect("stat the junction");
        assert_ne!(
            meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT,
            0,
            "a junction carries FILE_ATTRIBUTE_REPARSE_POINT"
        );

        let why = vet_dir(link.clone()).expect_err("a junction must be refused");
        assert!(why.contains("reparse point"), "vague reason: {why}");
        assert!(
            why.contains("outside the profile"),
            "a reason should say what it would cost: {why}"
        );
        assert!(
            why.contains("link"),
            "a log line needs the path, got: {why}"
        );

        // …while the plain directory it points at is exactly what we want.
        assert_eq!(vet_dir(target.clone()), Ok(target));
    }

    #[test]
    fn a_missing_profile_directory_says_it_is_missing() {
        let tree = TempTree::new();

        let why = vet_dir(tree.at("nobody-here")).expect_err("absent");
        assert!(why.contains("does not exist"), "{why}");
        assert!(
            !why.contains("os error"),
            "a missing directory is not an error code to decipher: {why}"
        );
    }

    #[test]
    fn only_real_end_user_sids_are_cleaned() {
        assert!(is_cleanup_sid(
            "S-1-5-21-1004336348-1177238915-682003330-1001"
        ));
        assert!(is_cleanup_sid(
            "S-1-12-1-111111111-2222222222-3333333333-4444444444"
        ));
        assert!(!is_cleanup_sid("S-1-5-18")); // Local System
        assert!(!is_cleanup_sid("S-1-5-19")); // Local Service
        assert!(!is_cleanup_sid("S-1-5-20")); // Network Service
        assert!(!is_cleanup_sid("S-1-5-21-1-2-3-1001.bak")); // temp-profile marker
        assert!(!is_cleanup_sid(".DEFAULT"));
        assert!(!is_cleanup_sid("S-1-5-80-anything")); // service account
    }

    #[test]
    fn expands_the_profile_path_tokens() {
        let drive = std::env::var("SystemDrive").expect("every Windows machine sets SystemDrive");
        assert_eq!(
            expand_env(r"%SystemDrive%\Users\alice"),
            format!(r"{drive}\Users\alice")
        );
        // ProfileList entries are not consistent about case, and neither the
        // registry nor the environment cares.
        assert_eq!(
            expand_env(r"%systemdrive%\Users\bob"),
            format!(r"{drive}\Users\bob")
        );
        assert_eq!(expand_env(r"C:\Users\carol"), r"C:\Users\carol");
        // Set in this process, and still left alone: only the two tokens
        // Windows writes are ours to expand.
        assert!(std::env::var("USERPROFILE").is_ok());
        assert_eq!(expand_env(r"%USERPROFILE%\x"), r"%USERPROFILE%\x");
        assert_eq!(expand_env("%NO_SUCH_VAR_HERE%\\x"), "%NO_SUCH_VAR_HERE%\\x");
        // An unclosed token is text, not a token.
        assert_eq!(expand_env("%unterminated\\x"), "%unterminated\\x");
    }

    /// Every profile this returns must be a cleanable account with a real
    /// directory — a leaked service SID would mean cleaning the wrong hive.
    /// A `trouble` reason is not itself a failure here (the real machine's
    /// `ProfileList` is out of this test's control) but is worth a line if it
    /// ever shows up, since it should not happen on a normal dev machine or
    /// CI runner.
    ///
    /// The `!profiles.is_empty()` assertion below reads the machine this runs
    /// on, deliberately: it is there to catch an enumeration that finds nobody,
    /// which is what a broken all-users uninstall looks like from the outside.
    /// It therefore assumes the machine has at least one real user account,
    /// which a dev box and a CI runner both do. Somewhere that genuinely has
    /// only service SIDs — a bare container image — would fail it, and the
    /// answer there is to give that environment an account, not to drop the
    /// assertion.
    #[test]
    fn enumeration_returns_only_real_user_profiles() {
        let (profiles, trouble, skipped) = all();
        for why in &trouble {
            println!("ProfileList did not read in full: {why}");
        }
        // An account that was passed over is named as a SID *and* explained in
        // a line of its own, so the caller can count one and print the other
        // without reading either out of the other's text.
        assert!(
            skipped.len() <= trouble.len(),
            "every skipped account owes a reason: {skipped:?} against {trouble:?}"
        );
        for sid in &skipped {
            assert!(
                is_cleanup_sid(sid),
                "only a cleanable account can be skipped"
            );
            assert!(
                trouble.iter().any(|why| why.starts_with(sid)),
                "{sid} was skipped without saying why: {trouble:?}"
            );
            assert!(
                !profiles.iter().any(|p| &p.sid == sid),
                "{sid} was both cleaned and skipped"
            );
        }
        // Whoever is running this test has a profile, so an empty list means
        // the enumeration found nothing rather than that there is nothing —
        // and an all-users uninstall that cleaned nobody would look just like
        // one that had nobody to clean.
        assert!(
            !profiles.is_empty(),
            "no cleanable profile on a machine that is running this test ({trouble:?})"
        );
        for p in profiles {
            assert!(is_cleanup_sid(&p.sid), "leaked non-user SID {}", p.sid);
            assert!(p.dir.is_dir(), "{} has no directory", p.sid);
        }
    }
}
