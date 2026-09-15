//! Remove the classic Kuvatin verbs from one account's classes hive, whether
//! that account is signed in or signed out.
//!
//! Signed in, Windows already has the hive mounted at
//! `HKEY_USERS\<SID>_Classes` — a real key, not the symbolic link that
//! `HKCU\Software\Classes` is — and that mounted copy is the one Explorer
//! reads, so it is the one we edit. Signed out, the hive is only a file,
//! `<profile>\AppData\Local\Microsoft\Windows\UsrClass.dat`; we mount it under
//! a private name of our own, delete through that, and unmount again. Mounting
//! needs `SeBackupPrivilege` and `SeRestorePrivilege`, which the SYSTEM
//! installer's token holds but leaves disabled.
//!
//! **The file path is not trusted.** `ProfileImagePath` comes from an
//! admin-only key, but everything below the profile root belongs to the
//! account, and a directory junction needs no privilege at all — so its owner
//! can aim `AppData\Local\Microsoft\Windows` at another account's folder or at
//! a system one and wait for SYSTEM to come along. Worse, `RegLoadKeyW`
//! *creates* the hive file when it is missing, so a wrong path is not merely
//! read but written. `checked_usrclass_path` is what stands in the way: every
//! component from `AppData` down is refused if it is a reparse point, the file
//! is confirmed to resolve back inside the profile, and `clean_offline` looks
//! once more, as late as it can, that the file is still a file.
//!
//! That leaves a window neither can close: between the last check and
//! `RegLoadKeyW` the owner can still swap the path. It cannot be closed from
//! here — holding the file open ourselves is exactly what makes `RegLoadKeyW`
//! fail — so what is left is to make the window as small as possible and say
//! plainly that it exists. Anything outside the profile that a junction could
//! aim at is refused before we get there, so what remains is a race, measured
//! in the microseconds between two adjacent statements, against a machine that
//! is already running an uninstall as SYSTEM.
//!
//! Everything in the registry works *relative to an open root handle* and never
//! walks a path down to a key: `super::regutil` refuses to step through a
//! symbolic link, and the hive belongs to a user who may have planted one. The
//! root is held with `TRAVERSE_ACCESS` — `KEY_QUERY_VALUE` alone — because
//! `KEY_READ` would also ask for `READ_CONTROL` and `KEY_NOTIFY`, and those are
//! the hive owner's to deny.
//!
//! This module **reports**; it never prints and never logs. It runs as SYSTEM,
//! where `crate::applog` would resolve `%LOCALAPPDATA%` to the system profile
//! and leave a brand-new file behind — exactly the sort of leftover the
//! all-users uninstall exists to remove. The orchestrator prints what comes
//! back here to stdout.
//!
//! Nothing outside `#[cfg(test)]` calls this yet: the orchestrator that walks
//! every profile and prints these results is a later task in the
//! all-users-uninstall plan.
#![allow(dead_code)]

use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_NOT_ALL_ASSIGNED, ERROR_SHARING_VIOLATION, ERROR_SUCCESS,
    HANDLE, LUID,
};
use windows::Win32::Security::{
    AdjustTokenPrivileges, GetTokenInformation, LookupPrivilegeValueW, TokenElevation,
    LUID_AND_ATTRIBUTES, SE_BACKUP_NAME, SE_PRIVILEGE_ENABLED, SE_RESTORE_NAME, TOKEN_ACCESS_MASK,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_ELEVATION, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Registry::{RegLoadKeyW, RegUnLoadKeyW, HKEY_USERS};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::profiles::{is_cleanup_sid, Profile, FILE_ATTRIBUTE_REPARSE_POINT};
use super::regutil::{open_owned_no_links, wide, TRAVERSE_ACCESS};
use super::verbs::{remove_verbs_under, VerbSweep};

/// How many times to ask the registry to unmount a hive before giving up. A
/// key we have just closed can keep the hive busy for a moment, so the first
/// refusal means nothing; ten tries a tenth of a second apart is a second of
/// patience, which is plenty and is not a hang.
const UNMOUNT_ATTEMPTS: u32 = 10;

/// The steps from a profile's directory down to its classes hive file. Each
/// one is checked on the way; see `checked_usrclass_path`.
const USRCLASS_UNDER_PROFILE: [&str; 5] =
    ["AppData", "Local", "Microsoft", "Windows", "UsrClass.dat"];

/// Where a profile's classes hive file lives, named but not vouched for.
/// Anything that means to *load* it wants `checked_usrclass_path`.
pub(super) fn usrclass_path(profile_dir: &Path) -> PathBuf {
    let mut path = profile_dir.to_path_buf();
    for step in USRCLASS_UNDER_PROFILE {
        path.push(step);
    }
    path
}

/// The profile's classes hive file, but only when every step down to it is an
/// ordinary directory inside that profile.
///
/// Two checks, because either alone has a hole. Walking the components with
/// `symlink_metadata` names the offending directory, which is what an operator
/// needs, and catches a junction whose target we could never open. Resolving
/// the file catches everything the walk could have missed, including a junction
/// planted between two of its steps, because it asks Windows where the file
/// really is rather than where its name says it is.
pub(super) fn checked_usrclass_path(profile_dir: &Path) -> Result<PathBuf, String> {
    let mut here = profile_dir.to_path_buf();
    for step in USRCLASS_UNDER_PROFILE {
        here.push(step);
        // symlink_metadata, so a reparse point is reported as itself rather
        // than as whatever it points at.
        let meta = std::fs::symlink_metadata(&here)
            .map_err(|e| format!("{} would not open ({e})", here.display()))?;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(format!(
                "{} is a reparse point; refusing to mount a hive through it, \
                 because its owner can aim it anywhere and this runs as SYSTEM",
                here.display()
            ));
        }
    }
    if !here.is_file() {
        return Err(format!("{} is not a file", here.display()));
    }
    // `canonicalize` opens the file and asks Windows for its final path, so
    // this is the same answer `GetFinalPathNameByHandleW` gives — with no need
    // for a `windows` crate feature this build does not otherwise want.
    let resolved = std::fs::canonicalize(&here)
        .map_err(|e| format!("{} would not resolve ({e})", here.display()))?;
    let root = std::fs::canonicalize(profile_dir)
        .map_err(|e| format!("{} would not resolve ({e})", profile_dir.display()))?;
    if !under(&resolved, &root) {
        return Err(format!(
            "{} really is {}, which is outside {}; refusing to mount it",
            here.display(),
            resolved.display(),
            root.display()
        ));
    }
    Ok(here)
}

/// Whether `child` is `parent` or lies below it, compared component by
/// component and case-insensitively, the way Windows compares names. A string
/// prefix would not do: `C:\Users\al` is a prefix of `C:\Users\alice` and names
/// a different account.
fn under(child: &Path, parent: &Path) -> bool {
    let mut walk = child.components();
    parent.components().all(|want| {
        walk.next().is_some_and(|here| {
            here.as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&want.as_os_str().to_string_lossy())
        })
    })
}

/// How an account's classes hive was reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HiveAccess {
    /// Already mounted at `HKEY_USERS\<SID>_Classes`: the account is signed in,
    /// and that mounted copy is what Explorer reads.
    Loaded,
    /// We mounted the profile's `UsrClass.dat` ourselves and unmounted it after.
    Mounted,
    /// Never opened, so nothing was deleted. `HiveOutcome::trouble` says why.
    None,
}

impl HiveAccess {
    /// How the uninstall should say this, so whoever prints does not have to
    /// work it out again.
    pub(super) fn wording(&self) -> &'static str {
        match self {
            HiveAccess::Loaded => "signed in; its hive was already mounted",
            HiveAccess::Mounted => "signed out; its UsrClass.dat was mounted and unmounted",
            HiveAccess::None => "not reached; nothing was removed",
        }
    }
}

/// What visiting one account's classes hive came to, ready for the uninstall to
/// print. Nothing here has been printed or logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HiveOutcome {
    /// The account whose hive this is.
    pub sid: String,
    /// How its hive was reached.
    pub access: HiveAccess,
    /// What the verb sweep came to — all zeroes when the hive was never opened.
    pub sweep: VerbSweep,
    /// What went wrong around the sweep: a hive that would not open, a
    /// `UsrClass.dat` that is not there or not to be trusted, a hive we could
    /// not unmount again. Empty on the ordinary path.
    pub trouble: Vec<String>,
}

/// A sweep and whatever went wrong around it. The same shape as
/// `verbs::subkeys_to_delete` and for the same reason: what we managed comes
/// back whatever else happened, because a hive we could not finish with is
/// exactly one whose keys we should still have deleted.
type Cleaned = (VerbSweep, Vec<String>);

/// Why an offline clean did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OfflineFailure {
    /// The mount was refused the way a hive already in use is refused. This is
    /// wording, not a decision: `clean_profile` goes back to the loaded path
    /// after *any* mount failure, because MSDN documents no error code here and
    /// `ERROR_SHARING_VIOLATION` is only what we observe.
    InUse(String),
    /// Anything else, in words fit to print.
    Failed(String),
}

impl OfflineFailure {
    pub(super) fn why(&self) -> &str {
        match self {
            OfflineFailure::InUse(why) | OfflineFailure::Failed(why) => why,
        }
    }
}

/// Clean the classes hive of an account that is signed in, whose hive Windows
/// already has mounted at `HKEY_USERS\<SID>_Classes`.
///
/// `Err` when that hive is not there or will not open — which for a signed-out
/// account is simply the ordinary case, and the caller's cue to take the
/// offline path.
pub(super) fn clean_loaded(sid: &str) -> Result<Cleaned, String> {
    debug_assert!(
        is_cleanup_sid(sid),
        "{sid} is not a cleanable end-user SID; this must never open a service account's hive"
    );
    // A real key, unlike `HKCU\Software\Classes`, so this resolves nothing and
    // redirects nowhere; every delete then happens below the handle.
    let root = open_owned_no_links(HKEY_USERS, &format!("{sid}_Classes"), TRAVERSE_ACCESS)
        .map_err(|why| format!(r"HKEY_USERS\{why}"))?;
    Ok((remove_verbs_under(root.get()), Vec::new()))
}

/// Clean the classes hive of an account that is signed out, by mounting its
/// `UsrClass.dat` under `mount_name`, deleting through the mounted root, and
/// unmounting again.
///
/// A failed unmount comes back in the `Vec`, not as an `Err`: the deletions
/// stand, and what is left to say is that the hive is still under our name.
pub(super) fn clean_offline(usrclass: &Path, mount_name: &str) -> Result<Cleaned, OfflineFailure> {
    enable_backup_restore().map_err(OfflineFailure::Failed)?;
    // `RegLoadKeyW` CREATES the hive file when it is not there, so a path that
    // has gone missing is not merely unreadable — it is one SYSTEM would write
    // a fresh hive to. Look as late as we can and refuse rather than create.
    // The caller has looked too; this is the last word before the load, and the
    // module doc says plainly what the remaining window is.
    if !usrclass.is_file() {
        return Err(OfflineFailure::Failed(format!(
            "{} is not a file; refusing to have RegLoadKeyW create one there",
            usrclass.display()
        )));
    }
    let mounted = mount(usrclass, mount_name)?;
    let swept = match open_owned_no_links(HKEY_USERS, mount_name, TRAVERSE_ACCESS) {
        // The root closes at the end of this arm, before the unmount below.
        Ok(root) => Ok(remove_verbs_under(root.get())),
        Err(why) => Err(format!(r"HKEY_USERS\{why}")),
    };
    // Unmounted here, on every path out, so the only thing left for `Drop` is a
    // panic — and so a refusal has somewhere to be reported.
    let unmounted = mounted.release();
    match swept {
        Ok(sweep) => Ok((sweep, unmounted.err().into_iter().collect())),
        Err(why) => Err(OfflineFailure::Failed(match unmounted {
            Ok(()) => why,
            Err(also) => format!("{why}; {also}"),
        })),
    }
}

/// Clean one profile's classes hive by whichever way is open: the mounted hive
/// if the account is signed in, its `UsrClass.dat` if it is not.
///
/// The mounted hive is tried first and wins, because for a signed-in account it
/// is the copy Explorer reads — and its file cannot be mounted twice anyway.
/// If the mount is then refused for *any* reason the loaded path is tried once
/// more: an account can sign in between the two steps, and the error that
/// produces is undocumented, so the only safe reading of a refused mount is
/// "the hive may be mounted now". Every reason gathered on the way is carried
/// into `trouble`, because a `_Classes` key we were denied and an account that
/// was signing in look identical from the outside and must not read alike.
pub(super) fn clean_profile(profile: &Profile) -> HiveOutcome {
    let sid = profile.sid.clone();
    let loaded_why = match clean_loaded(&sid) {
        Ok((sweep, trouble)) => {
            return HiveOutcome {
                sid,
                access: HiveAccess::Loaded,
                sweep,
                trouble,
            }
        }
        // Not being mounted is just what signed out looks like, so this is only
        // worth printing if the offline path cannot be taken either.
        Err(why) => why,
    };
    let file = match checked_usrclass_path(&profile.dir) {
        Ok(file) => file,
        // Nothing mounted and nothing we are willing to mount: a container-style
        // profile that is not attached, a profile directory already cleared out,
        // or a path someone has aimed elsewhere. Say so rather than count the
        // account clean.
        Err(why) => return unreachable_hive(sid, vec![loaded_why, why]),
    };
    match clean_offline(&file, &mount_name_for(&sid)) {
        Ok((sweep, trouble)) => HiveOutcome {
            sid,
            access: HiveAccess::Mounted,
            sweep,
            trouble,
        },
        Err(failure) => {
            let so_far = vec![loaded_why, failure.why().to_string()];
            match clean_loaded(&sid) {
                Ok((sweep, trouble)) => HiveOutcome {
                    sid,
                    access: HiveAccess::Loaded,
                    sweep,
                    // Why we went the long way round is worth saying even
                    // though it ended well.
                    trouble: [so_far, trouble].concat(),
                },
                Err(second) => unreachable_hive(sid, [so_far, vec![second]].concat()),
            }
        }
    }
}

fn unreachable_hive(sid: String, trouble: Vec<String>) -> HiveOutcome {
    HiveOutcome {
        sid,
        access: HiveAccess::None,
        sweep: VerbSweep::default(),
        trouble,
    }
}

/// A mount name nothing else on the machine will be using: our own process, a
/// counter for the profiles after the first, and the account it belongs to, so
/// a name that does somehow survive says whose hive it holds.
fn mount_name_for(sid: &str) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "Kuvatin-Uninstall-{}-{}-{sid}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// A hive we mounted. Unmounting it is the caller's job, through `release`,
/// which can say what went wrong; the `Drop` below is a backstop for a panic
/// unwinding past that, where there is nobody left to tell.
struct Mounted {
    name: String,
    released: bool,
}

impl Mounted {
    /// Unmount, and hand back what happened.
    ///
    /// Marked released before the attempt, deliberately: whatever `unmount`
    /// comes back with, it has already tried `UNMOUNT_ATTEMPTS` times across a
    /// second, and one more try from `Drop` a microsecond later would not
    /// succeed where those failed — it would only hide the error this is about
    /// to hand the caller.
    fn release(mut self) -> Result<(), String> {
        self.released = true;
        unmount(&self.name)
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        // Only a panic unwinding past `release` gets here, and there is nobody
        // left to tell: this module must not print, and the caller is already
        // on its way out. A hive left mounted is worse than a silent retry.
        let _ = unmount(&self.name);
    }
}

/// Mount a hive file under `mount_name` in `HKEY_USERS`.
fn mount(usrclass: &Path, mount_name: &str) -> Result<Mounted, OfflineFailure> {
    let file = wide(&usrclass.to_string_lossy());
    let name = wide(mount_name);
    let status = unsafe { RegLoadKeyW(HKEY_USERS, PCWSTR(name.as_ptr()), PCWSTR(file.as_ptr())) };
    if status == ERROR_SUCCESS {
        return Ok(Mounted {
            name: mount_name.to_string(),
            released: false,
        });
    }
    let why = format!(
        "{} would not mount (error {})",
        usrclass.display(),
        status.0
    );
    if status == ERROR_SHARING_VIOLATION {
        Err(OfflineFailure::InUse(format!(
            "{why}: something else has the hive open, most likely the account signing in"
        )))
    } else {
        Err(OfflineFailure::Failed(why))
    }
}

/// Unmount a hive we mounted, retrying for a moment: the registry can take a
/// beat to let go of a key we have only just closed. A hive left mounted keeps
/// its file locked and its keys under `HKEY_USERS`, so a refusal is worth a
/// line rather than silence.
pub(super) fn unmount(mount_name: &str) -> Result<(), String> {
    let name = wide(mount_name);
    let mut last = ERROR_SUCCESS;
    for attempt in 0..UNMOUNT_ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let status = unsafe { RegUnLoadKeyW(HKEY_USERS, PCWSTR(name.as_ptr())) };
        if status == ERROR_SUCCESS {
            return Ok(());
        }
        last = status;
    }
    Err(format!(
        r"HKEY_USERS\{mount_name} would not unmount after {UNMOUNT_ATTEMPTS} tries (error {}); the hive is still mounted",
        last.0
    ))
}

/// Enable `SeBackupPrivilege` and `SeRestorePrivilege` on this process's token.
/// `RegLoadKeyW` and `RegUnLoadKeyW` need both, and the SYSTEM installer's
/// token holds them but leaves them disabled.
///
/// Done once and remembered: a run that visits twenty profiles should not say
/// the same failure twenty different ways.
///
/// They then stay enabled for the rest of the process's life, and that is
/// deliberate. Turning them off again would protect nothing — the token *holds*
/// both privileges, so enabling is flipping a bit any code in this process
/// could flip straight back, and the process this runs in is
/// `--unregister-all-users`, which does this and exits. What switching them off
/// per hive would cost is this memo, and with it the one clear failure message
/// a run gets instead of one per account.
pub(super) fn enable_backup_restore() -> Result<(), String> {
    static DONE: OnceLock<Result<(), String>> = OnceLock::new();
    DONE.get_or_init(adjust_backup_restore).clone()
}

fn adjust_backup_restore() -> Result<(), String> {
    let token = process_token(TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY)?;
    let mut outcome = Ok(());
    for (name, spelling) in [
        (SE_BACKUP_NAME, "SeBackupPrivilege"),
        (SE_RESTORE_NAME, "SeRestorePrivilege"),
    ] {
        if let Err(why) = enable_one(token, name, spelling) {
            outcome = Err(why);
            break;
        }
    }
    close_handle(token);
    outcome
}

fn enable_one(token: HANDLE, name: PCWSTR, spelling: &str) -> Result<(), String> {
    let mut luid = LUID::default();
    unsafe { LookupPrivilegeValueW(PCWSTR::null(), name, &mut luid) }
        .map_err(|e| format!("this machine does not know {spelling} ({e})"))?;
    let privileges = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    unsafe { AdjustTokenPrivileges(token, false, Some(&privileges), 0, None, None) }
        .map_err(|e| format!("{spelling} could not be enabled ({e})"))?;
    // AdjustTokenPrivileges reports success even when it assigned nothing; the
    // last error is the only place it admits that.
    if unsafe { GetLastError() } == ERROR_NOT_ALL_ASSIGNED {
        return Err(format!(
            "this process's token does not hold {spelling}, so a signed-out account's hive cannot be mounted"
        ));
    }
    Ok(())
}

fn process_token(access: TOKEN_ACCESS_MASK) -> Result<HANDLE, String> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut token) }
        .map_err(|e| format!("this process's token would not open ({e})"))?;
    Ok(token)
}

fn close_handle(token: HANDLE) {
    unsafe {
        let _ = CloseHandle(token);
    }
}

/// Whether this process runs with a full (elevated) token. Cleaning another
/// account's hive needs one; the orchestrator prints the answer, and the
/// offline test skips without it.
pub(super) fn is_elevated() -> bool {
    let Ok(token) = process_token(TOKEN_QUERY) else {
        return false;
    };
    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0u32;
    let got = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some(std::ptr::addr_of_mut!(elevation).cast()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    };
    close_handle(token);
    got.is_ok() && elevation.TokenIsElevated != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCreateKeyExW, RegOpenKeyExW, RegSaveKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_READ, KEY_SET_VALUE, KEY_WRITE, REG_LATEST_FORMAT, REG_LINK, REG_OPTION_NON_VOLATILE,
        REG_SZ,
    };

    use super::super::regutil::{delete_tree_under, open_owned, DeleteOutcome, OwnedKey};
    use super::super::verbs::{classes_subkeys, remove_verbs_under};
    use super::super::windows::menu_extensions;

    /// How many times to ask for the cleaned hive file back before calling it
    /// locked. Separate from `UNMOUNT_ATTEMPTS` because it measures something
    /// else — the file system letting go after a successful unmount, not the
    /// registry letting go of the hive — even though a second of patience
    /// happens to suit both.
    const HIVE_FILE_DELETE_ATTEMPTS: u32 = 10;

    /// A name no other run, and no other test in this run, will use.
    fn unique(what: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        // The clock ticks every 100 ns here, which two tests starting together
        // can share; the counter is what keeps them apart.
        static NEXT: AtomicU64 = AtomicU64::new(0);
        format!(
            "Kuvatin-hive-test-{what}-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn create_at(root: HKEY, sub: &str) {
        let w = wide(sub);
        let mut h = HKEY::default();
        let status = unsafe {
            RegCreateKeyExW(
                root,
                PCWSTR(w.as_ptr()),
                0,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut h,
                None,
            )
        };
        assert_eq!(status, ERROR_SUCCESS, "create {sub}");
        drop(OwnedKey::own(h));
    }

    /// Open `root\sub` for writing a value, as a handle that closes itself.
    fn open_to_set(root: HKEY, sub: &str) -> OwnedKey {
        let w = wide(sub);
        let mut h = HKEY::default();
        let status = unsafe { RegOpenKeyExW(root, PCWSTR(w.as_ptr()), 0, KEY_SET_VALUE, &mut h) };
        assert_eq!(status, ERROR_SUCCESS, "open {sub} for writing");
        OwnedKey::own(h)
    }

    /// Put a string value on an existing key — a verb key with nothing in it
    /// would prove less than the ones an install actually writes.
    fn set_string(root: HKEY, sub: &str, name: &str, data: &str) {
        let key = open_to_set(root, sub);
        let value = wide(data);
        let bytes = unsafe {
            std::slice::from_raw_parts(
                value.as_ptr().cast::<u8>(),
                std::mem::size_of_val(&value[..]),
            )
        };
        let n = wide(name);
        let status =
            unsafe { RegSetValueExW(key.get(), PCWSTR(n.as_ptr()), 0, REG_SZ, Some(bytes)) };
        assert_eq!(status, ERROR_SUCCESS, "set {name} on {sub}");
    }

    /// Dress a plain key up as a symbolic link without making it one. The walk
    /// in `regutil` fails closed on the value alone, which is the point: a hive
    /// owner cannot keep a key out of our reach by labelling it, and cannot
    /// redirect us by labelling one either. `regutil`'s own link tests use the
    /// same recipe on real links.
    fn set_link_value(root: HKEY, sub: &str, target_nt: &str) {
        let key = open_to_set(root, sub);
        // SymbolicLinkValue carries the target with no terminating NUL.
        let target: Vec<u16> = target_nt.encode_utf16().collect();
        let bytes = unsafe {
            std::slice::from_raw_parts(
                target.as_ptr().cast::<u8>(),
                std::mem::size_of_val(&target[..]),
            )
        };
        let name = wide("SymbolicLinkValue");
        let status =
            unsafe { RegSetValueExW(key.get(), PCWSTR(name.as_ptr()), 0, REG_LINK, Some(bytes)) };
        assert_eq!(status, ERROR_SUCCESS, "plant a link value on {sub}");
    }

    fn exists(root: HKEY, sub: &str) -> bool {
        open_owned(root, sub).is_some()
    }

    /// A scratch key under `HKCU\Software` standing in for a classes root, held
    /// exactly the way `clean_loaded` holds a real one — `KEY_QUERY_VALUE` and
    /// nothing more — and removed when the test ends, pass, fail or panic.
    /// Planting a verb here rather than under the real `Software\Classes` keeps
    /// a test run out of the developer's own Explorer menu.
    struct Scratch {
        path: String,
        root: OwnedKey,
    }

    impl Scratch {
        fn new() -> Self {
            let path = format!(r"Software\{}", unique("scratch"));
            create_at(HKEY_CURRENT_USER, &path);
            let root = open_owned_no_links(HKEY_CURRENT_USER, &path, TRAVERSE_ACCESS)
                .unwrap_or_else(|why| panic!("open {path}: {why}"));
            Scratch { path, root }
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            // The root handle is still open here, which is fine: a key deleted
            // through another handle goes from the namespace at once, and this
            // one closes with the struct a moment later.
            //
            // Say so loudly rather than leaving a key behind in silence: the
            // next run would not reuse this name, so nobody would notice.
            match delete_tree_under(HKEY_CURRENT_USER, &self.path) {
                DeleteOutcome::Deleted { .. } | DeleteOutcome::Absent => {}
                DeleteOutcome::Refused { why, .. } => eprintln!(
                    "scratch key HKCU\\{} survived cleanup ({why}); remove it by hand",
                    self.path
                ),
            }
        }
    }

    /// A hive file mounted under a private name in `HKEY_USERS`, unmounted when
    /// the test ends however it ends: a hive left mounted keeps its file locked
    /// and leaves a key under `HKU` for the next run to trip over.
    struct Mount {
        name: String,
    }

    impl Mount {
        fn new(file: &Path) -> Self {
            let name = unique("mount");
            let w = wide(&name);
            let f = wide(&file.to_string_lossy());
            let status = unsafe { RegLoadKeyW(HKEY_USERS, PCWSTR(w.as_ptr()), PCWSTR(f.as_ptr())) };
            assert_eq!(
                status,
                ERROR_SUCCESS,
                "RegLoadKeyW({name}, {})",
                file.display()
            );
            Mount { name }
        }

        /// The mounted hive's root, opened the way the production path opens it
        /// and closing itself — so an assertion that fails between here and the
        /// end of the test still leaves the hive free to unmount.
        fn root(&self) -> OwnedKey {
            open_owned_no_links(HKEY_USERS, &self.name, TRAVERSE_ACCESS)
                .unwrap_or_else(|why| panic!("open the mounted hive {}: {why}", self.name))
        }
    }

    impl Drop for Mount {
        fn drop(&mut self) {
            if let Err(why) = unmount(&self.name) {
                eprintln!("{why}; remove it by hand");
            }
        }
    }

    /// Write a key out as a hive file, the way a profile's `UsrClass.dat` is
    /// one. Needs `SeBackupPrivilege`, which the caller has already enabled, and
    /// its own `KEY_READ` handle — saving a hive is not one of the things the
    /// production access constants are cut down for.
    fn save_hive(path: &str, file: &Path) {
        let w = wide(path);
        let mut h = HKEY::default();
        let status =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(w.as_ptr()), 0, KEY_READ, &mut h) };
        assert_eq!(status, ERROR_SUCCESS, "open {path} for saving");
        let key = OwnedKey::own(h);
        let f = wide(&file.to_string_lossy());
        let status =
            unsafe { RegSaveKeyExW(key.get(), PCWSTR(f.as_ptr()), None, REG_LATEST_FORMAT) };
        assert_eq!(status, ERROR_SUCCESS, "RegSaveKeyExW to {}", file.display());
    }

    /// Seed one hive's worth of keys under an open root: a verb on an extension
    /// this build knows, a verb on one it does not (only enumeration can find
    /// that), a command store, and a bystander verb another app might have
    /// written, which must survive.
    fn seed_verbs(root: HKEY) {
        create_at(root, r"SystemFileAssociations\.png\shell\Kuvatin\command");
        set_string(
            root,
            r"SystemFileAssociations\.png\shell\Kuvatin",
            "MUIVerb",
            "Kuvatin",
        );
        create_at(root, r"SystemFileAssociations\.qoi\shell\Kuvatin\command");
        create_at(
            root,
            r"SystemFileAssociations\.png\shell\OpenWithOther\command",
        );
        create_at(root, r"Kuvatin.CommandStore\shell\item");
    }

    /// How many of `seed_verbs`'s key trees the uninstall must remove.
    const SEEDED_KUVATIN_KEYS: usize = 3;

    /// The signed-out path, end to end: build a hive FILE the way a profile's
    /// `UsrClass.dat` is one, run the production cleanup against that file, then
    /// re-mount and look. Needs SeBackup/SeRestore, so it runs only elevated —
    /// CI's runner is elevated, and a skip there would mean the gate is not
    /// running the thing it gates, so on CI a skip is a failure.
    #[test]
    fn offline_cleanup_removes_only_kuvatin_verbs() {
        if !is_elevated() {
            if std::env::var_os("CI").is_some() {
                panic!("offline hive test must run elevated on CI");
            }
            println!("skipping: not elevated");
            return;
        }
        enable_backup_restore().expect("enable SeBackup/SeRestore");

        let dir = tempfile::tempdir().expect("a temp directory");
        // RegSaveKeyExW refuses to overwrite, so this must not exist yet.
        let file = dir.path().join("UsrClass.dat");
        {
            let scratch = Scratch::new();
            seed_verbs(scratch.root.get());
            save_hive(&scratch.path, &file);
        }
        assert!(
            file.is_file(),
            "RegSaveKeyExW should have written {}",
            file.display()
        );

        let (sweep, trouble) =
            clean_offline(&file, &unique("offline")).expect("the offline cleanup");
        assert!(
            trouble.is_empty(),
            "nothing should have gone wrong: {trouble:?}"
        );
        assert!(
            sweep.troubles().is_empty(),
            "the hive reads in full: {:?}",
            sweep.lines
        );
        assert_eq!(
            sweep.removed, SEEDED_KUVATIN_KEYS,
            "three seeded key trees: {:?}",
            sweep.lines
        );
        assert_eq!(sweep.refused, 0, "{:?}", sweep.lines);
        assert_eq!(
            sweep.absent,
            classes_subkeys().len() + 1 - SEEDED_KUVATIN_KEYS,
            "every other key on the list was already gone"
        );

        {
            let check = Mount::new(&file);
            let root = check.root();
            assert!(
                !exists(root.get(), r"SystemFileAssociations\.png\shell\Kuvatin"),
                "known-extension verb left behind"
            );
            assert!(
                !exists(root.get(), r"SystemFileAssociations\.qoi\shell\Kuvatin"),
                "unknown-extension verb left behind (enumeration missed it)"
            );
            assert!(
                !exists(root.get(), "Kuvatin.CommandStore"),
                "store left behind"
            );
            assert!(
                exists(
                    root.get(),
                    r"SystemFileAssociations\.png\shell\OpenWithOther\command"
                ),
                "a bystander key was wrongly deleted"
            );
        }

        // Nothing holds the file open, so the hive really was unmounted. Given
        // a moment's patience of its own: the file system can take a beat to
        // let go after the registry has, and a test that read that as a hive
        // left mounted would be crying wolf.
        let mut locked = None;
        for attempt in 0..HIVE_FILE_DELETE_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            match std::fs::remove_file(&file) {
                Ok(()) => {
                    locked = None;
                    break;
                }
                Err(e) => locked = Some(e),
            }
        }
        assert!(
            locked.is_none(),
            "the cleaned hive should be unlocked: {locked:?}"
        );
    }

    /// The loaded-hive path's deletion loop, against a root held the way
    /// `clean_loaded` holds a signed-in account's: with `KEY_QUERY_VALUE` and
    /// nothing else. `KEY_READ` would also ask for `READ_CONTROL` and
    /// `KEY_NOTIFY`, and those are the hive owner's to deny — asking for them
    /// would hand them a way to stop the uninstall at the door.
    #[test]
    fn a_root_held_with_only_query_value_still_cleans() {
        let scratch = Scratch::new();
        seed_verbs(scratch.root.get());

        let sweep = remove_verbs_under(scratch.root.get());
        assert!(sweep.troubles().is_empty(), "{:?}", sweep.lines);
        assert_eq!(sweep.removed, SEEDED_KUVATIN_KEYS, "{:?}", sweep.lines);
        assert_eq!(sweep.refused, 0, "{:?}", sweep.lines);
        assert!(!exists(
            scratch.root.get(),
            r"SystemFileAssociations\.png\shell\Kuvatin"
        ));
        assert!(!exists(
            scratch.root.get(),
            r"SystemFileAssociations\.qoi\shell\Kuvatin"
        ));
        assert!(!exists(scratch.root.get(), "Kuvatin.CommandStore"));
        assert!(
            exists(
                scratch.root.get(),
                r"SystemFileAssociations\.png\shell\OpenWithOther\command"
            ),
            "a bystander key was wrongly deleted"
        );
    }

    /// One obstacle at `SystemFileAssociations` blocks every verb key under it
    /// — twelve of them on today's list — and the result says so once, loudly,
    /// instead of twelve times. Whoever reads the uninstall output needs the one
    /// key that has to be dealt with, not a wall of repeats.
    #[test]
    fn one_obstacle_at_system_file_associations_refuses_every_key_under_it() {
        let scratch = Scratch::new();
        create_at(scratch.root.get(), "SystemFileAssociations");
        set_link_value(
            scratch.root.get(),
            "SystemFileAssociations",
            r"\Registry\User\.DEFAULT\Software\Nowhere",
        );
        // A store outside the blocked subtree: one refusal must not shelter the
        // keys that could still have gone.
        create_at(scratch.root.get(), r"Kuvatin.CommandStore\shell\item");

        let sweep = remove_verbs_under(scratch.root.get());
        let under_assoc = menu_extensions().len() + 1; // every extension, plus `image`
        assert_eq!(sweep.refused, under_assoc, "{:?}", sweep.lines);
        assert_eq!(
            sweep.removed, 1,
            "the store should still go: {:?}",
            sweep.lines
        );
        let (why, count) = sweep
            .shared_obstacle()
            .expect("one obstacle behind many refusals");
        assert_eq!(count, under_assoc);
        assert!(
            why.starts_with("SystemFileAssociations") && why.contains("SymbolicLinkValue"),
            "the one key to deal with should be named: {why}"
        );
        assert!(
            sweep.other_refusals().is_empty(),
            "everything refused here is that one obstacle: {:?}",
            sweep.other_refusals()
        );
        assert!(!exists(scratch.root.get(), "Kuvatin.CommandStore"));
    }

    /// A hive that is not mounted is reported, not passed off as a clean sweep.
    /// The SID is one no machine issues, so this never touches a real account.
    #[test]
    fn a_hive_that_is_not_mounted_is_reported_not_swept() {
        let why = clean_loaded("S-1-5-21-0-0-0-4242").expect_err("no such account is signed in");
        assert!(why.contains("S-1-5-21-0-0-0-4242_Classes"), "{why}");
        assert!(why.contains("is not there"), "{why}");
    }

    /// An account whose profile container is not mounted — no hive under
    /// `HKEY_USERS` and no `UsrClass.dat` on disk — is reported and skipped.
    /// Counting it clean would be the quiet kind of wrong: the uninstall would
    /// say it visited every account and leave that one's menu in place.
    #[test]
    fn a_profile_with_no_hive_is_reported_and_skipped() {
        let profile = Profile {
            sid: "S-1-5-21-0-0-0-4243".to_string(),
            dir: PathBuf::from(r"C:\NoSuchKuvatinProfile"),
        };
        let outcome = clean_profile(&profile);
        assert_eq!(outcome.sid, profile.sid);
        assert_eq!(outcome.access, HiveAccess::None);
        assert_eq!(outcome.sweep, VerbSweep::default(), "nothing was deleted");
        assert!(
            outcome.trouble.iter().any(|t| t.contains("AppData")),
            "the path we could not reach should be named: {:?}",
            outcome.trouble
        );
        assert_eq!(outcome.access.wording(), "not reached; nothing was removed");
    }

    /// A junction anywhere between the profile root and `UsrClass.dat` is the
    /// attack this guard exists for: the directories below the profile belong
    /// to the account, `mklink /J` needs no privilege, and `RegLoadKeyW` would
    /// happily mount — or, on a missing file, CREATE — a hive wherever the
    /// junction points, as SYSTEM.
    #[test]
    fn a_junction_on_the_way_to_the_hive_is_refused() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let profile = dir.path().join("profile");
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(profile.join(r"AppData\Local\Microsoft")).expect("profile tree");
        std::fs::create_dir_all(&elsewhere).expect("the junction's target");
        // A file at the far end, so nothing but the junction check can be what
        // refuses this.
        std::fs::write(elsewhere.join("UsrClass.dat"), b"not really a hive").expect("bait");

        let link = profile.join(r"AppData\Local\Microsoft\Windows");
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&elsewhere)
            .output();
        match made {
            Ok(out) if link.exists() => out,
            other => {
                // Junctions need no privilege, but say so rather than pass in
                // silence if this environment will not make one.
                eprintln!("skipping: could not create a junction at {link:?}: {other:?}");
                return;
            }
        };

        let why = checked_usrclass_path(&profile).expect_err("a junction must be refused");
        assert!(
            why.contains("reparse point"),
            "the reason should name what was found: {why}"
        );
        assert!(
            why.contains("Windows"),
            "the reason should name the component: {why}"
        );

        // …and the whole path refuses it too, rather than only the helper.
        let outcome = clean_profile(&Profile {
            sid: "S-1-5-21-0-0-0-4244".to_string(),
            dir: profile.clone(),
        });
        assert_eq!(outcome.access, HiveAccess::None);
        assert_eq!(outcome.sweep, VerbSweep::default());
        assert!(
            outcome.trouble.iter().any(|t| t.contains("reparse point")),
            "{:?}",
            outcome.trouble
        );

        // The bait was never mounted, so it is still exactly what we wrote.
        assert_eq!(
            std::fs::read(elsewhere.join("UsrClass.dat")).expect("bait survives"),
            b"not really a hive",
        );
        // Junctions confuse recursive removal, so take this one out first and
        // leave the temp directory nothing to trip over.
        std::fs::remove_dir(&link).expect("remove the junction");
    }

    /// The guard is not so strict that it refuses an ordinary profile: a plain
    /// tree resolves to itself and comes back as the path to mount.
    #[test]
    fn an_ordinary_profile_tree_passes_the_guard() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let profile = dir.path().join("profile");
        let hive = usrclass_path(&profile);
        std::fs::create_dir_all(hive.parent().expect("a parent")).expect("profile tree");
        std::fs::write(&hive, b"not really a hive").expect("the hive file");

        assert_eq!(
            checked_usrclass_path(&profile).expect("a plain tree passes"),
            hive
        );

        // …but a directory where the file should be is not a hive file.
        let other = dir.path().join("other");
        let as_dir = usrclass_path(&other);
        std::fs::create_dir_all(&as_dir).expect("a directory in the file's place");
        let why = checked_usrclass_path(&other).expect_err("a directory is not a hive file");
        assert!(why.contains("is not a file"), "{why}");
    }

    #[test]
    fn under_compares_whole_components_not_string_prefixes() {
        assert!(under(
            Path::new(r"C:\Users\alice\AppData"),
            Path::new(r"C:\Users\alice")
        ));
        assert!(under(
            Path::new(r"C:\Users\ALICE\AppData"),
            Path::new(r"c:\users\alice")
        ));
        assert!(under(Path::new(r"C:\Users"), Path::new(r"C:\Users")));
        // The one a string prefix would get wrong.
        assert!(!under(
            Path::new(r"C:\Users\alice\AppData"),
            Path::new(r"C:\Users\al")
        ));
        assert!(!under(Path::new(r"C:\Users"), Path::new(r"C:\Users\alice")));
    }
}
