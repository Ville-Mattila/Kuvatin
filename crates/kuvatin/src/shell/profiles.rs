//! Enumerate the real user profiles from
//! `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList`, so the
//! all-users uninstall can visit each one's classes hive and files.
//!
//! `Profile` is already in use by the offline-hive walk; `all()` is not called
//! outside `#[cfg(test)]` yet, because what calls it is the all-users entry
//! point — the `--unregister-all-users` mode, a later task in that plan, which
//! is the one thing that has a reason to walk every account. Until then, allow
//! the otherwise-unused helpers.
#![allow(dead_code)]

use std::os::windows::fs::MetadataExt;
use std::path::PathBuf;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS};
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
/// the environment strings ourselves. `None` when absent/unreadable.
fn profile_dir(sid: &str) -> Option<PathBuf> {
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
            // `cb` is the size the value needs, in bytes. Refuse to go round
            // again unless that is really more than we just offered, so a hive
            // cannot hold us here.
            let needed = (cb as usize).div_ceil(2);
            if needed <= buf.len() || needed > MAX_VALUE_CHARS {
                return None;
            }
            buf = vec![0u16; needed];
            continue;
        }
        if status != ERROR_SUCCESS {
            return None;
        }
        let units = (cb as usize / 2).min(buf.len());
        // The value carries its own terminating NUL, and one written with a
        // second one would otherwise leave a NUL inside the path — which no
        // file API would match, and which would read as a truncated path in
        // any message about it.
        let value = &buf[..units];
        let value = &value[..value.iter().position(|&u| u == 0).unwrap_or(value.len())];
        let raw = String::from_utf16_lossy(value);
        return Some(PathBuf::from(expand_env(&raw)));
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
/// which `cleanable_dir` reports and skips.
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

/// Where one profile's files are, or why we will not touch it.
///
/// Every `Err` here is something to say out loud rather than to pass over: a
/// cleanable account whose directory we cannot pin down is an account the
/// uninstall will not finish cleaning, and whoever ran it should be told which.
fn cleanable_dir(sid: &str) -> Result<PathBuf, String> {
    let dir =
        profile_dir(sid).ok_or_else(|| format!("{sid}: its ProfileImagePath would not read"))?;
    let shown = dir.display();
    // symlink_metadata, so a reparse point is reported as itself rather than as
    // whatever it points at.
    let meta = std::fs::symlink_metadata(&dir)
        .map_err(|e| format!("{sid}: {shown} would not open ({e})"))?;
    if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(format!(
            "{sid}: {shown} is a reparse point, not a profile directory; \
             deleting through it would reach files outside the profile"
        ));
    }
    if !meta.is_dir() {
        return Err(format!("{sid}: {shown} is not a directory"));
    }
    Ok(dir)
}

/// Every cleanable profile whose directory we can stand behind, plus what went
/// wrong on the way, when anything did.
///
/// Same shape as `verbs::subkeys_to_delete` and for the same reason: an
/// all-users uninstall that read a partial `ProfileList` and quietly treated
/// that as "no profiles" would leave every real account's classes hive
/// untouched while calling the machine clean. So the caller gets everything
/// this could read, plus the reason when it is not everything, rather than a
/// short list passed off as a complete one.
///
/// The reason gathers three kinds of trouble: an enumeration that would not
/// read, a list with nothing in it at all, and each individual account this
/// had to pass over — because an account skipped in silence is precisely the
/// one that keeps its menu. Accounts that are *meant* to be passed over, the
/// service SIDs and the `.bak` markers, say nothing: they are not trouble, and
/// a line each would bury the ones that are.
pub(super) fn all() -> (Vec<Profile>, Option<String>) {
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
    for sid in sids {
        if !is_cleanup_sid(&sid) {
            continue;
        }
        match cleanable_dir(&sid) {
            Ok(dir) => profiles.push(Profile { sid, dir }),
            Err(why) => trouble.push(why),
        }
    }
    (profiles, (!trouble.is_empty()).then(|| trouble.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    #[test]
    fn enumeration_returns_only_real_user_profiles() {
        let (profiles, trouble) = all();
        if let Some(why) = &trouble {
            eprintln!("ProfileList did not read in full: {why}");
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
