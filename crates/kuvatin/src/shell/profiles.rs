//! Enumerate the real user profiles from
//! `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList`, so the
//! all-users uninstall can visit each one's classes hive and files.
//!
//! Nothing outside `#[cfg(test)]` calls into this module yet: a later task in
//! the all-users-uninstall plan wires the offline-hive walk to it. Until then,
//! allow the otherwise-unused helpers.
#![allow(dead_code)]

use std::path::PathBuf;
use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY_LOCAL_MACHINE, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
};

use super::regutil::{enum_subkeys, wide};

const PROFILE_LIST: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList";

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
    let mut buf = [0u16; 1024];
    let mut cb = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(name.as_ptr()),
            // ProfileImagePath is REG_EXPAND_SZ; accept both types and do not
            // auto-expand (we expand with the machine environment ourselves).
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut cb),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let units = (cb as usize / 2).saturating_sub(1).min(buf.len());
    let raw = String::from_utf16_lossy(&buf[..units]);
    Some(PathBuf::from(expand_env(&raw)))
}

/// Expand `%SystemDrive%`-style tokens from the process environment, which for
/// the SYSTEM installer holds the machine variables these paths are written
/// against. An unknown token is left as it stands.
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
        match std::env::var(var) {
            Ok(v) => out.push_str(&v),
            Err(_) => {
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

/// Every cleanable profile whose directory exists, plus why the
/// `ProfileList` enumeration could not be read in full, when it couldn't.
///
/// Same shape as `verbs::subkeys_to_delete` and for the same reason: an
/// all-users uninstall that read a partial `ProfileList` and quietly treated
/// that as "no profiles" would leave every real account's classes hive
/// untouched while calling the machine clean. So the caller gets everything
/// this could read, plus the reason when it is not everything, rather than a
/// short list passed off as a complete one.
pub(super) fn all() -> (Vec<Profile>, Option<String>) {
    let (sids, trouble) = match enum_subkeys(HKEY_LOCAL_MACHINE, PROFILE_LIST) {
        Ok(sids) => (sids, None),
        Err(why) => (Vec::new(), Some(why)),
    };
    let profiles = sids
        .into_iter()
        .filter(|sid| is_cleanup_sid(sid))
        .filter_map(|sid| {
            let dir = profile_dir(&sid)?;
            dir.is_dir().then_some(Profile { sid, dir })
        })
        .collect();
    (profiles, trouble)
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
        std::env::set_var("KUVATIN_TEST_DRIVE", "C:");
        assert_eq!(
            expand_env(r"%KUVATIN_TEST_DRIVE%\Users\alice"),
            r"C:\Users\alice"
        );
        assert_eq!(expand_env(r"C:\Users\bob"), r"C:\Users\bob");
        assert_eq!(expand_env("%NO_SUCH_VAR_HERE%\\x"), "%NO_SUCH_VAR_HERE%\\x");
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
        for p in profiles {
            assert!(is_cleanup_sid(&p.sid), "leaked non-user SID {}", p.sid);
            assert!(p.dir.is_dir(), "{} has no directory", p.sid);
        }
    }
}
