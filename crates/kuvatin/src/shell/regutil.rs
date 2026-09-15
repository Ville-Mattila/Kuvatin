//! Registry helpers that operate on an arbitrary open `HKEY` root, so the same
//! code can act on `HKEY_CURRENT_USER\Software\Classes` (the per-user unregister)
//! and on a mounted `HKEY_USERS\<SID>_Classes` hive (the all-users uninstall).
//!
//! The all-users path runs as SYSTEM against a hive its owner can write, so
//! every deletion here assumes the hive is hostile: a user may plant registry
//! symbolic links both along the path we walk and inside the subtree we
//! delete, and SYSTEM must not follow one out of the hive. The rule that
//! keeps that true is simple — resolve each name exactly once, then work
//! through the handle that resolution returned. `RegDeleteTreeW` and
//! `RegDeleteKeyExW` break it: both re-open keys by name and follow a link
//! they find there (two tests at the bottom of this file pin that down), so
//! neither appears outside those tests.
//!
//! The two ways in are `open_path_no_links` (link-safe, for anything that will
//! write or delete) and `open_subkey` / `enum_subkeys` (which *do* follow
//! links, and so are for reading only).
//!
//! Nothing outside `#[cfg(test)]` calls into this module yet: later tasks in
//! the all-users-uninstall plan wire `windows.rs` and the new all-users path
//! to it. Until then, allow the otherwise-unused helpers.
#![allow(dead_code)]

use windows::core::{PCWSTR, PWSTR};
use windows::Wdk::System::Registry::NtDeleteKey;
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS,
    ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, HANDLE, NTSTATUS, STATUS_ACCESS_DENIED,
    STATUS_CANNOT_DELETE, STATUS_SUCCESS,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, KEY_ENUMERATE_SUB_KEYS,
    KEY_QUERY_VALUE, KEY_READ, REG_LINK, REG_OPTION_OPEN_LINK, REG_SAM_FLAGS, REG_VALUE_TYPE,
};

/// Keys nested deeper than this inside a subtree we are deleting are treated
/// as a refusal rather than walked; the registry's own limit is well under it.
const MAX_DEPTH: u32 = 512;

/// How many times to empty a key and try again when something re-creates
/// subkeys under it while we work. Enough for a benign race — Explorer
/// touching the key mid-uninstall — without grinding on against an owner who
/// is re-creating keys on purpose.
const DELETE_ROUNDS: u32 = 3;

/// What removing a key actually needs: `DELETE` for `NtDeleteKey`, plus the
/// two read rights that let us list a key's children and see a link value on
/// it. `KEY_ALL_ACCESS` would also demand `WRITE_DAC` and `WRITE_OWNER`, which
/// a hive's owner can deny us purely to block the uninstall. The `DELETE` bit
/// is spelled out because the `windows` crate exports it only from the
/// file-system namespace, which this crate does not otherwise need.
const DELETE_ACCESS: REG_SAM_FLAGS =
    REG_SAM_FLAGS(0x0001_0000 | KEY_ENUMERATE_SUB_KEYS.0 | KEY_QUERY_VALUE.0);

/// A name buffer past this size means something other than a key name; the
/// registry caps names at 255 characters.
const MAX_NAME_CHARS: usize = 64 * 1024;

/// NUL-terminated UTF-16, for the `PCWSTR` registry APIs.
pub(super) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Give an open key handle back to the registry; a failed close leaves a
/// caller nothing to do about it.
pub(super) fn close(h: HKEY) {
    unsafe {
        let _ = RegCloseKey(h);
    }
}

/// An open key that closes itself, so a walk can give up at any point without
/// leaking the handles it opened on the way down.
struct OwnedKey(HKEY);

impl OwnedKey {
    fn get(&self) -> HKEY {
        self.0
    }

    /// Hand the handle to a caller who will close it.
    fn into_raw(self) -> HKEY {
        std::mem::ManuallyDrop::new(self).0
    }
}

impl Drop for OwnedKey {
    fn drop(&mut self) {
        close(self.0);
    }
}

/// Open `root\subpath` for read, **following any symbolic link on the way**.
/// `None` when the key does not exist or cannot be opened.
///
/// Reading is all this is for. Anything that will write or delete wants
/// `open_path_no_links`, which refuses to be redirected.
pub(super) fn open_subkey(root: HKEY, subpath: &str) -> Option<HKEY> {
    let w = wide(subpath);
    let mut h = HKEY::default();
    let status = unsafe { RegOpenKeyExW(root, PCWSTR(w.as_ptr()), 0, KEY_READ, &mut h) };
    (status == ERROR_SUCCESS).then_some(h)
}

/// The immediate subkey names of `root\subpath`, or `Err` with a reason when
/// the list could not be read in full — never a short list passed off as a
/// complete one, which would let an uninstall conclude a hive was already
/// clean. A key that is simply absent has no children, so that is `Ok(empty)`.
///
/// Like `open_subkey`, this follows a symbolic link at any segment, so it is
/// for reading only.
pub(super) fn enum_subkeys(root: HKEY, subpath: &str) -> Result<Vec<String>, String> {
    let Some(key) = open_subkey(root, subpath) else {
        return Ok(Vec::new());
    };
    let out = enum_children(key);
    close(key);
    out
}

/// The immediate subkey names of the key an open handle names.
fn enum_children(key: HKEY) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut index = 0u32;
    // Key names stop at 255 characters, but ask again at the same index with a
    // roomier heap buffer if a hive ever says otherwise.
    let mut buf = vec![0u16; 256];
    loop {
        let mut len = buf.len() as u32;
        let status = unsafe {
            RegEnumKeyExW(
                key,
                index,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
                None,
                PWSTR::null(),
                None,
                None,
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            return Ok(out);
        }
        if status == ERROR_MORE_DATA {
            if buf.len() >= MAX_NAME_CHARS {
                return Err(format!(
                    "subkey {index} claims a name longer than {MAX_NAME_CHARS} characters"
                ));
            }
            buf = vec![0u16; buf.len() * 2];
            continue;
        }
        if status != ERROR_SUCCESS {
            return Err(format!(
                "subkey {index} would not read ({}); read {} before it",
                explain_error(status.0),
                out.len()
            ));
        }
        out.push(String::from_utf16_lossy(&buf[..len as usize]));
        index += 1;
    }
}

/// Delete the key an open handle names — the key itself, with no second look
/// at its name, so nothing planted at that name can redirect the delete. The
/// key must already be empty of subkeys.
fn delete_this_key(h: HKEY) -> NTSTATUS {
    unsafe { NtDeleteKey(HANDLE(h.0)) }
}

/// True when the key an open handle names carries a `REG_LINK`
/// `SymbolicLinkValue`.
///
/// This is a heuristic, not proof: a plain key can be given the same value and
/// will read as a link here. It is therefore used to decide what *not* to walk
/// through, never to decide what to skip — see `delete_tree_under`.
fn is_link_handle(h: HKEY) -> bool {
    let name = wide("SymbolicLinkValue");
    let mut kind = REG_VALUE_TYPE::default();
    let status =
        unsafe { RegQueryValueExW(h, PCWSTR(name.as_ptr()), None, Some(&mut kind), None, None) };
    status == ERROR_SUCCESS && kind == REG_LINK
}

/// True when `root\subpath` carries a `REG_LINK` `SymbolicLinkValue`. Carries
/// the same caveat as `is_link_handle`: a plain key can be dressed up to look
/// like this, so treat a `true` as "do not walk through it", not as "this key
/// is not mine to delete".
pub(super) fn is_reg_link(root: HKEY, subpath: &str) -> bool {
    let w = wide(subpath);
    let mut h = HKEY::default();
    // REG_OPTION_OPEN_LINK opens the link itself rather than its target.
    let status = unsafe {
        RegOpenKeyExW(
            root,
            PCWSTR(w.as_ptr()),
            REG_OPTION_OPEN_LINK.0,
            KEY_READ,
            &mut h,
        )
    };
    if status != ERROR_SUCCESS {
        return false;
    }
    let link = is_link_handle(h);
    close(h);
    link
}

/// Say what a Win32 error means in words a log reader can act on, keeping the
/// raw code for whoever needs it.
fn explain_error(code: u32) -> String {
    if code == ERROR_ACCESS_DENIED.0 {
        format!("we are not allowed to open it (error {code})")
    } else {
        format!("would not open (error {code})")
    }
}

/// The same for the status `NtDeleteKey` came back with.
fn explain_status(status: NTSTATUS) -> String {
    let code = format!("status {:#010x}", status.0);
    if status == STATUS_CANNOT_DELETE {
        format!("still has subkeys (something re-created them while we worked) ({code})")
    } else if status == STATUS_ACCESS_DENIED {
        format!("we are not allowed to delete it ({code})")
    } else {
        format!("would not delete ({code})")
    }
}

/// What `delete_tree_under` did, ready for the uninstall log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DeleteOutcome {
    /// The key existed and it and its subtree are gone.
    Deleted { notes: Vec<String> },
    /// Nothing to do: the key, or a segment on the way to it, is not there.
    Absent,
    /// Some of it is still there. `why` names the key that would not go and
    /// is meant to be printed as-is.
    Refused { why: String, notes: Vec<String> },
}

impl DeleteOutcome {
    /// Anything worth logging that happened along the way, whatever the
    /// outcome — a symbolic link found planted inside the subtree, say.
    pub(super) fn notes(&self) -> &[String] {
        match self {
            DeleteOutcome::Deleted { notes } | DeleteOutcome::Refused { notes, .. } => notes,
            DeleteOutcome::Absent => &[],
        }
    }
}

/// Why one component would not open.
enum OpenFailure {
    /// It simply is not there.
    Absent,
    /// The `RegOpenKeyExW` error code.
    Failed(u32),
}

/// Why a whole path would not open.
enum PathFailure {
    Absent,
    Refused(String),
}

/// Open one path component from `parent` as itself, never following a link.
fn open_component(parent: HKEY, name: &str, access: REG_SAM_FLAGS) -> Result<HKEY, OpenFailure> {
    let w = wide(name);
    let mut h = HKEY::default();
    let status = unsafe {
        RegOpenKeyExW(
            parent,
            PCWSTR(w.as_ptr()),
            REG_OPTION_OPEN_LINK.0,
            access,
            &mut h,
        )
    };
    if status == ERROR_SUCCESS {
        Ok(h)
    } else if status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND {
        Err(OpenFailure::Absent)
    } else {
        Err(OpenFailure::Failed(status.0))
    }
}

/// The path as the log should print it: segments joined, empties dropped.
fn normalised(subpath: &str) -> String {
    subpath
        .split('\\')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\\")
}

/// Open one segment and refuse to go through it if it carries a link value.
fn open_step(
    parent: HKEY,
    name: &str,
    access: REG_SAM_FLAGS,
    trail: &str,
) -> Result<OwnedKey, PathFailure> {
    let key = match open_component(parent, name, access) {
        Ok(h) => OwnedKey(h),
        Err(OpenFailure::Absent) => return Err(PathFailure::Absent),
        Err(OpenFailure::Failed(e)) => {
            return Err(PathFailure::Refused(format!(
                "{trail} {}",
                explain_error(e)
            )))
        }
    };
    if is_link_handle(key.get()) {
        return Err(PathFailure::Refused(format!(
            "{trail} carries a REG_LINK SymbolicLinkValue; not walking through it"
        )));
    }
    Ok(key)
}

fn walk_no_links(
    root: HKEY,
    subpath: &str,
    access: REG_SAM_FLAGS,
) -> Result<OwnedKey, PathFailure> {
    let segments: Vec<&str> = subpath.split('\\').filter(|s| !s.is_empty()).collect();
    let Some((leaf_name, above)) = segments.split_last() else {
        // An empty path would name the root itself; never hand that out.
        return Err(PathFailure::Refused(
            "an empty path names no key".to_string(),
        ));
    };
    let mut trail = String::new();
    let mut held: Option<OwnedKey> = None;
    for seg in above {
        if !trail.is_empty() {
            trail.push('\\');
        }
        trail.push_str(seg);
        let parent = held.as_ref().map_or(root, OwnedKey::get);
        // Opened from the handle above before the old one is dropped.
        held = Some(open_step(parent, seg, KEY_READ, &trail)?);
    }
    if !trail.is_empty() {
        trail.push('\\');
    }
    trail.push_str(leaf_name);
    let parent = held.as_ref().map_or(root, OwnedKey::get);
    open_step(parent, leaf_name, access, &trail)
}

/// Open `root\subpath` without ever being redirected by a symbolic link, and
/// hand back the handle for the caller to close.
///
/// Each segment is opened from the handle above it with
/// `REG_OPTION_OPEN_LINK` — which protects only the *last* component of a
/// path, so one call per segment is the point — and the walk stops at the
/// first segment carrying a `REG_LINK` `SymbolicLinkValue`. That check fails
/// closed: a plain key wearing that value blocks the path too. On a path the
/// uninstall owns that is the safe way round, and the message says plainly
/// what was found rather than asserting the key is a link.
///
/// This is the only opener to use for a handle you will write or delete
/// through.
pub(super) fn open_path_no_links(
    root: HKEY,
    subpath: &str,
    access: REG_SAM_FLAGS,
) -> Result<HKEY, String> {
    match walk_no_links(root, subpath, access) {
        Ok(key) => Ok(key.into_raw()),
        Err(PathFailure::Absent) => Err(format!("{} is not there", normalised(subpath))),
        Err(PathFailure::Refused(why)) => Err(why),
    }
}

/// Delete every child of the key `key` names, each through a handle of its
/// own so that no planted link is ever followed.
///
/// Every child is recursed into, including one that carries a `REG_LINK`
/// `SymbolicLinkValue`. Skipping those would be worse than useless: the value
/// can be set on an ordinary key, so a hive owner could hang it on
/// `…\shell\Kuvatin` and keep its children — and with them the whole subtree —
/// out of our reach. Recursing costs nothing on a genuine link, which
/// enumerates no children of its own when opened with `REG_OPTION_OPEN_LINK`,
/// so the walk falls straight through to `NtDeleteKey` and takes the link
/// entry while leaving its target alone.
///
/// Every child is attempted even after one fails, so a single key the owner
/// has locked does not shelter its siblings; the first failure is reported
/// with a count of the rest.
fn clear_children(
    key: HKEY,
    trail: &str,
    depth: u32,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    if depth == 0 {
        return Err(format!("{trail} is nested deeper than we will walk"));
    }
    let names = enum_children(key).map_err(|why| format!("{trail}: {why}"))?;
    let mut first: Option<String> = None;
    let mut failed = 0usize;
    for name in names {
        let here = format!(r"{trail}\{name}");
        let child = match open_component(key, &name, DELETE_ACCESS) {
            Ok(h) => OwnedKey(h),
            Err(OpenFailure::Absent) => continue, // already gone
            Err(OpenFailure::Failed(e)) => {
                failed += 1;
                if first.is_none() {
                    first = Some(format!("{here} {}", explain_error(e)));
                }
                continue;
            }
        };
        if is_link_handle(child.get()) {
            notes.push(format!(
                "{here} carried a REG_LINK SymbolicLinkValue; removed that key itself, never what it named"
            ));
        }
        if let Err(why) = clear_and_delete(&child, &here, depth - 1, notes) {
            failed += 1;
            if first.is_none() {
                first = Some(why);
            }
        }
    }
    match first {
        None => Ok(()),
        Some(why) if failed == 1 => Err(why),
        Some(why) => Err(format!(
            "{why} — and {} more under {trail} would not go either",
            failed - 1
        )),
    }
}

/// Empty a key and delete it, through its own handle throughout. If something
/// re-creates subkeys under it while we work, empty it and try again a few
/// times before giving up, so a benign race does not read as a refusal.
fn clear_and_delete(
    key: &OwnedKey,
    trail: &str,
    depth: u32,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    let mut last = None;
    for _ in 0..DELETE_ROUNDS {
        clear_children(key.get(), trail, depth, notes)?;
        let status = delete_this_key(key.get());
        if status == STATUS_SUCCESS {
            return Ok(());
        }
        let why = format!("{trail} {}", explain_status(status));
        if status != STATUS_CANNOT_DELETE {
            return Err(why);
        }
        last = Some(why);
    }
    Err(last.unwrap_or_else(|| format!("{trail} would not delete")))
}

/// Delete the subtree `root\subpath` on a hive we do not trust, never
/// following a symbolic link — not at a segment of the path, and not at any
/// key inside the subtree.
///
/// The path is opened by the same segment-at-a-time walk as
/// `open_path_no_links`, which refuses to go through a segment carrying a
/// link value (it just keeps absence apart from refusal, which the outcome
/// needs); the subtree below it is then removed by
/// `clear_children` and `NtDeleteKey`, which go through a handle for every key
/// they touch. So every name is resolved exactly once and what that
/// resolution produced is what gets deleted — a planted link has no second
/// lookup to hijack. Naming the leaf again for `RegDeleteKeyExW(parent,
/// leaf_name)` would reopen it, and that call *follows* a link, deleting its
/// target and leaving the link standing (`reg_delete_key_ex_follows_a_link`
/// shows it).
///
/// The two halves treat a link value differently on purpose. On the *path* it
/// is a stop sign, because walking through a real link would take SYSTEM out
/// of the hive. *Inside* the subtree every key is deleted regardless, because
/// the value proves nothing — an ordinary key can carry it — and honouring it
/// there would let a hive owner keep any subtree simply by labelling it. A
/// link entry found inside is removed as the key it is, its target untouched,
/// and noted for the log.
///
/// What remains is not a way through but a way to be told no: the hive's owner
/// can lock a key against us, or keep re-creating keys faster than
/// `DELETE_ROUNDS` empties them, and either ends as `Refused` naming the key
/// that would not go.
pub(super) fn delete_tree_under(root: HKEY, subpath: &str) -> DeleteOutcome {
    let leaf = match walk_no_links(root, subpath, DELETE_ACCESS) {
        Ok(key) => key,
        Err(PathFailure::Absent) => return DeleteOutcome::Absent,
        Err(PathFailure::Refused(why)) => {
            return DeleteOutcome::Refused {
                why,
                notes: Vec::new(),
            }
        }
    };
    let trail = normalised(subpath);
    let mut notes = Vec::new();
    match clear_and_delete(&leaf, &trail, MAX_DEPTH, &mut notes) {
        Ok(()) => DeleteOutcome::Deleted { notes },
        Err(why) => DeleteOutcome::Refused { why, notes },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    use windows::Win32::System::Registry::{
        RegCreateKeyExW, RegDeleteKeyExW, RegDeleteTreeW, RegSetValueExW, HKEY_CURRENT_USER,
        HKEY_USERS, KEY_ALL_ACCESS, KEY_CREATE_LINK, KEY_SET_VALUE, KEY_WRITE,
        REG_OPTION_CREATE_LINK, REG_OPTION_NON_VOLATILE,
    };

    fn create(path: &str) {
        let w = wide(path);
        let mut h = HKEY::default();
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
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
        assert_eq!(status, ERROR_SUCCESS, "create {path}");
        close(h);
    }

    /// Open `parent\name` as itself, link or not.
    fn open_as_itself(parent: HKEY, name: &str) -> Option<HKEY> {
        let w = wide(name);
        let mut h = HKEY::default();
        let status = unsafe {
            RegOpenKeyExW(
                parent,
                PCWSTR(w.as_ptr()),
                REG_OPTION_OPEN_LINK.0,
                KEY_ALL_ACCESS,
                &mut h,
            )
        };
        (status == ERROR_SUCCESS).then_some(h)
    }

    /// Remove `parent\name` without ever following a link: every key, a link
    /// among them, is deleted through its own handle, children first.
    fn purge(parent: HKEY, name: &str) -> Result<(), String> {
        let Some(h) = open_as_itself(parent, name) else {
            return Ok(()); // absent, or a link whose target has gone
        };
        let kids = enum_children(h);
        let mut trouble = kids.as_ref().err().cloned();
        for kid in kids.unwrap_or_default() {
            if let Err(why) = purge(h, &kid) {
                trouble.get_or_insert(why);
            }
        }
        let status = delete_this_key(h);
        close(h);
        if status != STATUS_SUCCESS {
            trouble.get_or_insert(format!("{name} {}", explain_status(status)));
        }
        match trouble {
            Some(why) => Err(why),
            None => Ok(()),
        }
    }

    /// A scratch tree under HKCU, unique to this run and removed when the test
    /// ends — pass, fail or panic.
    struct Scratch {
        path: String,
    }

    impl Scratch {
        fn new() -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let me = Scratch {
                path: format!(
                    r"Software\Kuvatin-regutil-test-{}-{nanos}",
                    std::process::id()
                ),
            };
            create(&me.path);
            me
        }

        /// `Software\Kuvatin-regutil-test-…\<rest>`, as the helpers take it.
        fn at(&self, rest: &str) -> String {
            format!(r"{}\{rest}", self.path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let swept = purge(HKEY_CURRENT_USER, &self.path);
            // Say so loudly rather than leaving a key behind in silence: the
            // next run would not reuse this name, so nobody would notice.
            match open_path_no_links(HKEY_CURRENT_USER, &self.path, KEY_READ) {
                Ok(h) => {
                    close(h);
                    eprintln!(
                        "scratch key HKCU\\{} survived cleanup ({swept:?}); remove it by hand",
                        self.path
                    );
                }
                Err(_) => {
                    if let Err(why) = swept {
                        eprintln!("scratch key HKCU\\{} cleanup complained: {why}", self.path);
                    }
                }
            }
        }
    }

    /// The NT path of the hive HKCU maps to, found by looking for this run's
    /// own scratch key under each `HKEY_USERS` subkey — no SID lookup, and it
    /// proves the path really names the hive we are writing to.
    fn hive_nt_path(scratch: &Scratch) -> String {
        for sid in enum_subkeys(HKEY_USERS, "").expect("list HKEY_USERS") {
            if sid.ends_with("_Classes") {
                continue;
            }
            if let Some(h) = open_subkey(HKEY_USERS, &format!(r"{sid}\{}", scratch.path)) {
                close(h);
                return format!(r"\Registry\User\{sid}");
            }
        }
        panic!(
            "no HKEY_USERS subkey holds {}; cannot name the link target",
            scratch.path
        );
    }

    /// Write a `REG_LINK` `SymbolicLinkValue` naming `target_nt` on an already
    /// open key. The registry does not check that the key is a link.
    fn set_link_value(h: HKEY, target_nt: &str) -> u32 {
        // SymbolicLinkValue carries the target with no terminating NUL.
        let target: Vec<u16> = target_nt.encode_utf16().collect();
        let bytes = unsafe {
            std::slice::from_raw_parts(
                target.as_ptr().cast::<u8>(),
                std::mem::size_of_val(&target[..]),
            )
        };
        let name = wide("SymbolicLinkValue");
        unsafe { RegSetValueExW(h, PCWSTR(name.as_ptr()), 0, REG_LINK, Some(bytes)).0 }
    }

    /// Create `HKCU\link_path` as a real registry symbolic link pointing at
    /// `target_nt` (an absolute `\Registry\…` path).
    fn create_link(link_path: &str, target_nt: &str) {
        let w = wide(link_path);
        let mut h = HKEY::default();
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(w.as_ptr()),
                0,
                PCWSTR::null(),
                REG_OPTION_CREATE_LINK,
                KEY_CREATE_LINK | KEY_SET_VALUE,
                None,
                &mut h,
                None,
            )
        };
        assert_eq!(
            status, ERROR_SUCCESS,
            "RegCreateKeyExW(REG_OPTION_CREATE_LINK) for {link_path}"
        );
        let status = set_link_value(h, target_nt);
        close(h);
        assert_eq!(status, 0, "set SymbolicLinkValue on {link_path}");
    }

    /// Dress a plain, existing key up as a link without making it one.
    fn fake_link_value(path: &str, target_nt: &str) {
        let h = open_path_no_links(HKEY_CURRENT_USER, path, KEY_SET_VALUE)
            .unwrap_or_else(|why| panic!("open {path}: {why}"));
        let status = set_link_value(h, target_nt);
        close(h);
        assert_eq!(
            status, 0,
            "a plain key should accept a REG_LINK SymbolicLinkValue"
        );
    }

    fn deleted() -> DeleteOutcome {
        DeleteOutcome::Deleted { notes: Vec::new() }
    }

    fn kids(path: &str) -> Vec<String> {
        let mut out = enum_subkeys(HKEY_CURRENT_USER, path).expect("list subkeys");
        out.sort();
        out
    }

    fn refusal(outcome: DeleteOutcome) -> String {
        match outcome {
            DeleteOutcome::Refused { why, .. } => why,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn enumerates_and_deletes_subkeys() {
        let scratch = Scratch::new();
        create(&scratch.at("alpha"));
        create(&scratch.at("beta"));
        create(&scratch.at(r"gamma\deep\deeper"));

        assert_eq!(kids(&scratch.path), vec!["alpha", "beta", "gamma"]);

        assert_eq!(
            delete_tree_under(HKEY_CURRENT_USER, &scratch.at("alpha")),
            deleted()
        );
        assert_eq!(
            delete_tree_under(HKEY_CURRENT_USER, &scratch.at("alpha")),
            DeleteOutcome::Absent // gone now
        );
        // A whole tree goes, not just an empty leaf.
        assert_eq!(
            delete_tree_under(HKEY_CURRENT_USER, &scratch.at("gamma")),
            deleted()
        );
        // A missing segment above the leaf is absence, not a refusal.
        assert_eq!(
            delete_tree_under(HKEY_CURRENT_USER, &scratch.at(r"nowhere\deeper")),
            DeleteOutcome::Absent
        );
        // The root itself is never the target.
        assert!(refusal(delete_tree_under(HKEY_CURRENT_USER, "")).contains("empty path"));

        assert!(!is_reg_link(HKEY_CURRENT_USER, &scratch.at("beta")));
        assert_eq!(kids(&scratch.path), vec!["beta"]);
    }

    #[test]
    fn a_refusal_names_the_whole_path_not_a_bare_segment() {
        let scratch = Scratch::new();
        create(&scratch.at(r"Kuvatin\shell\command"));
        let target_nt = format!(r"{}\{}", hive_nt_path(&scratch), scratch.at("Kuvatin"));
        create_link(&scratch.at(r"Kuvatin\shell\link"), &target_nt);

        let why = refusal(delete_tree_under(
            HKEY_CURRENT_USER,
            &scratch.at(r"Kuvatin\shell\link\deeper"),
        ));
        assert!(
            why.contains(&scratch.at(r"Kuvatin\shell\link")),
            "a log line needs the whole path, got: {why}"
        );
        assert!(why.contains("SymbolicLinkValue"), "vague reason: {why}");
    }

    #[test]
    fn refuses_a_symbolic_link_at_any_segment() {
        let scratch = Scratch::new();
        // What a hostile hive would aim a link at: keys that must survive.
        create(&scratch.at("target"));
        create(&scratch.at(r"target\keep"));
        create(&scratch.at(r"target\shell\Kuvatin\sentinel"));

        let link = scratch.at("planted-link");
        let target_nt = format!(r"{}\{}", hive_nt_path(&scratch), scratch.at("target"));
        create_link(&link, &target_nt);

        // The link is real: it reads as a link, and it resolves to the target.
        assert!(is_reg_link(HKEY_CURRENT_USER, &link));
        assert!(!is_reg_link(HKEY_CURRENT_USER, &scratch.at("target")));
        assert_eq!(
            kids(&link),
            vec!["keep", "shell"],
            "the link should resolve"
        );

        // A link as the leaf.
        let why = refusal(delete_tree_under(HKEY_CURRENT_USER, &link));
        assert!(
            why.contains("planted-link") && why.contains("SymbolicLinkValue"),
            "unhelpful reason: {why}"
        );

        // A link as an *intermediate* segment — the case REG_OPTION_OPEN_LINK
        // on the full path does not cover.
        let why = refusal(delete_tree_under(
            HKEY_CURRENT_USER,
            &scratch.at(r"planted-link\shell\Kuvatin"),
        ));
        assert!(
            why.contains("planted-link") && why.contains("SymbolicLinkValue"),
            "unhelpful reason: {why}"
        );

        // Neither refusal touched the target.
        assert_eq!(kids(&scratch.at("target")), vec!["keep", "shell"]);
        assert_eq!(kids(&scratch.at(r"target\shell\Kuvatin")), vec!["sentinel"]);

        // Deleting a link through its own handle removes the link entry, not
        // what it points at. (Naming it for RegDeleteKeyExW would do the
        // opposite — see `reg_delete_key_ex_follows_a_link`.)
        let parent = open_path_no_links(HKEY_CURRENT_USER, &scratch.path, DELETE_ACCESS)
            .expect("open scratch");
        let itself = open_as_itself(parent, "planted-link").expect("open the link as itself");
        let removed = delete_this_key(itself);
        close(itself);
        close(parent);
        assert_eq!(removed, STATUS_SUCCESS, "delete the link entry");
        assert!(!is_reg_link(HKEY_CURRENT_USER, &link));
        assert!(open_subkey(HKEY_CURRENT_USER, &link).is_none());
        assert_eq!(
            kids(&scratch.at(r"target\shell\Kuvatin")),
            vec!["sentinel"],
            "the target must outlive its link"
        );

        // With no link in the way the same call deletes.
        assert_eq!(
            delete_tree_under(HKEY_CURRENT_USER, &scratch.at("target")),
            deleted()
        );
    }

    #[test]
    fn deletes_a_tree_holding_a_link_without_following_it() {
        let scratch = Scratch::new();
        // What the link points at, outside the tree we are told to delete.
        create(&scratch.at(r"victim\precious"));
        // The tree we are told to delete, with a link planted deep inside it.
        create(&scratch.at(r"Kuvatin\shell\command"));
        let nested = scratch.at(r"Kuvatin\shell\nested-link");
        let target_nt = format!(r"{}\{}", hive_nt_path(&scratch), scratch.at("victim"));
        create_link(&nested, &target_nt);
        assert!(is_reg_link(HKEY_CURRENT_USER, &nested));

        let outcome = delete_tree_under(HKEY_CURRENT_USER, &scratch.at("Kuvatin"));
        assert!(
            matches!(outcome, DeleteOutcome::Deleted { .. }),
            "{outcome:?}"
        );
        // The planted link is worth a line in the uninstall log.
        assert!(
            outcome.notes().iter().any(|n| n.contains("nested-link")),
            "a planted link should be noted, got {:?}",
            outcome.notes()
        );
        assert!(open_subkey(HKEY_CURRENT_USER, &scratch.at("Kuvatin")).is_none());
        // The link entry went with the tree; its target did not.
        assert_eq!(
            kids(&scratch.at("victim")),
            vec!["precious"],
            "a link inside the tree must not drag its target in"
        );
    }

    #[test]
    fn deletes_a_link_nested_three_deep() {
        let scratch = Scratch::new();
        create(&scratch.at(r"victim\precious"));
        create(&scratch.at(r"Kuvatin\a\b\c"));
        let target_nt = format!(r"{}\{}", hive_nt_path(&scratch), scratch.at("victim"));
        create_link(&scratch.at(r"Kuvatin\a\b\c\deep-link"), &target_nt);

        let outcome = delete_tree_under(HKEY_CURRENT_USER, &scratch.at("Kuvatin"));
        assert!(
            matches!(outcome, DeleteOutcome::Deleted { .. }),
            "{outcome:?}"
        );
        assert!(open_subkey(HKEY_CURRENT_USER, &scratch.at("Kuvatin")).is_none());
        assert_eq!(kids(&scratch.at("victim")), vec!["precious"]);
    }

    #[test]
    fn a_link_to_its_own_ancestor_does_not_hang() {
        let scratch = Scratch::new();
        create(&scratch.at(r"Kuvatin\shell\command"));
        // Pointing back up at the very tree being deleted: following it would
        // loop for ever, so the fact this returns at all is the assertion.
        let target_nt = format!(r"{}\{}", hive_nt_path(&scratch), scratch.at("Kuvatin"));
        create_link(&scratch.at(r"Kuvatin\shell\loop-link"), &target_nt);

        let outcome = delete_tree_under(HKEY_CURRENT_USER, &scratch.at("Kuvatin"));
        assert!(
            matches!(outcome, DeleteOutcome::Deleted { .. }),
            "{outcome:?}"
        );
        assert!(open_subkey(HKEY_CURRENT_USER, &scratch.at("Kuvatin")).is_none());
    }

    #[test]
    fn a_planted_link_value_does_not_save_a_plain_key() {
        let scratch = Scratch::new();
        create(&scratch.at(r"victim\precious"));
        create(&scratch.at(r"Kuvatin\shell\command"));
        create(&scratch.at(r"Kuvatin\shell\open"));
        // An ordinary key with children, wearing a link value as camouflage.
        let target_nt = format!(r"{}\{}", hive_nt_path(&scratch), scratch.at("victim"));
        fake_link_value(&scratch.at(r"Kuvatin\shell"), &target_nt);
        assert!(
            is_reg_link(HKEY_CURRENT_USER, &scratch.at(r"Kuvatin\shell")),
            "the value should make a plain key read as a link"
        );

        // Honouring that value would strand `command` and `open` and leave the
        // whole subtree behind.
        let outcome = delete_tree_under(HKEY_CURRENT_USER, &scratch.at("Kuvatin"));
        assert!(
            matches!(outcome, DeleteOutcome::Deleted { .. }),
            "{outcome:?}"
        );
        assert!(open_subkey(HKEY_CURRENT_USER, &scratch.at("Kuvatin")).is_none());
        // Nothing was followed on the way: the named target is untouched.
        assert_eq!(kids(&scratch.at("victim")), vec!["precious"]);
    }

    #[test]
    fn deletes_a_key_with_thousands_of_children() {
        let scratch = Scratch::new();
        create(&scratch.at("many"));
        for i in 0..3000 {
            create(&scratch.at(&format!("many\\child-{i:04}")));
        }
        assert_eq!(kids(&scratch.at("many")).len(), 3000);

        assert_eq!(
            delete_tree_under(HKEY_CURRENT_USER, &scratch.at("many")),
            deleted()
        );
        assert!(open_subkey(HKEY_CURRENT_USER, &scratch.at("many")).is_none());
    }

    #[test]
    fn open_path_no_links_says_which_way_it_failed() {
        let scratch = Scratch::new();
        create(&scratch.at(r"Kuvatin\shell"));
        let h = open_path_no_links(HKEY_CURRENT_USER, &scratch.at(r"Kuvatin\shell"), KEY_READ)
            .expect("open a plain path");
        close(h);

        let why = open_path_no_links(HKEY_CURRENT_USER, &scratch.at("nowhere"), KEY_READ)
            .expect_err("absent");
        assert!(why.contains("is not there"), "{why}");

        let target_nt = format!(r"{}\{}", hive_nt_path(&scratch), scratch.at("Kuvatin"));
        create_link(&scratch.at("a-link"), &target_nt);
        let why = open_path_no_links(HKEY_CURRENT_USER, &scratch.at(r"a-link\shell"), KEY_READ)
            .expect_err("through a link");
        assert!(why.contains("SymbolicLinkValue"), "{why}");
    }

    // The two tests below pin down Windows behaviour that `delete_tree_under`
    // is built around. They assert what the API does today, not what it ought
    // to do: if one ever fails, the registry has changed under us and the
    // handle-only deletion above can be revisited.

    #[test]
    fn reg_delete_key_ex_follows_a_link() {
        let scratch = Scratch::new();
        create(&scratch.at("empty-target"));
        let link = scratch.at("probe-link");
        let target_nt = format!(r"{}\{}", hive_nt_path(&scratch), scratch.at("empty-target"));
        create_link(&link, &target_nt);

        let parent = open_path_no_links(HKEY_CURRENT_USER, &scratch.path, DELETE_ACCESS)
            .expect("open scratch");
        let name = wide("probe-link");
        let removed = unsafe { RegDeleteKeyExW(parent, PCWSTR(name.as_ptr()), 0, 0) };
        close(parent);

        assert_eq!(removed, ERROR_SUCCESS);
        assert!(
            open_subkey(HKEY_CURRENT_USER, &scratch.at("empty-target")).is_none(),
            "RegDeleteKeyExW took the link's target"
        );
        assert!(
            is_reg_link(HKEY_CURRENT_USER, &link),
            "…and left the link itself standing"
        );
    }

    #[test]
    fn reg_delete_tree_follows_a_nested_link() {
        let scratch = Scratch::new();
        create(&scratch.at(r"victim\precious"));
        create(&scratch.at("doomed"));
        let nested = scratch.at(r"doomed\nested-link");
        let target_nt = format!(r"{}\{}", hive_nt_path(&scratch), scratch.at("victim"));
        create_link(&nested, &target_nt);

        let parent = open_path_no_links(HKEY_CURRENT_USER, &scratch.path, DELETE_ACCESS)
            .expect("open scratch");
        let leaf = open_as_itself(parent, "doomed").expect("open doomed");
        // Even handed a vetted handle and a NULL subkey, RegDeleteTreeW walks
        // its descendants by name — and follows the link it meets.
        unsafe {
            let _ = RegDeleteTreeW(leaf, PCWSTR::null());
        }
        close(leaf);
        close(parent);

        assert!(
            open_subkey(HKEY_CURRENT_USER, &scratch.at("victim")).is_none(),
            "RegDeleteTreeW reached out of the subtree and took the link's target"
        );
        assert!(
            is_reg_link(HKEY_CURRENT_USER, &nested),
            "…and left the link itself standing"
        );
    }
}
