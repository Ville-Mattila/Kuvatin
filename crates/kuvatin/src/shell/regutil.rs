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
//! The two ways in are `open_owned_no_links` (link-safe, for anything that
//! will write or delete) and `open_owned` / `open_owned_reporting` /
//! `enum_subkeys` (which *do* follow links, and so are for reading only).
//! Every one of them hands back an [`OwnedKey`] that closes itself, so no
//! caller outside this module holds a raw `HKEY` it has to remember to close.
//!
//! Every reason this module hands back is written to be printed as it stands,
//! and they are all punctuated the same way: `<key>: <what happened>`, the key
//! relative to whichever root was passed in. One rule, so a caller can put the
//! hive in front of any of them and get a line that reads as one thought, and
//! so a log of several reads as a list rather than as several styles. The
//! colon is what keeps `SystemFileAssociations: subkey 12 claims a name
//! longer than…` from reading as a key called "SystemFileAssociations subkey
//! 12".
//!
//! The per-user unregister in `windows.rs` deletes through this module, so
//! most of it is live. What is not called outside `#[cfg(test)]` yet is
//! `is_reg_link`, because the path that needs it is the offline one: a later
//! task in the all-users-uninstall plan mounts each profile's hive and has to
//! open keys for writing in a hive whose owner may have planted links. Until
//! then, allow it.
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
    KEY_QUERY_VALUE, REG_LINK, REG_OPTION_OPEN_LINK, REG_SAM_FLAGS, REG_VALUE_TYPE,
};

/// Keys nested deeper than this inside a subtree we are deleting are treated
/// as a refusal rather than walked. 512 is the registry's own documented
/// nesting limit, so nothing legitimate reaches it.
const MAX_DEPTH: u32 = 512;

/// How many times to empty one key and try again when something re-creates
/// subkeys under it while we work. Enough for a benign race — Explorer
/// touching the key mid-uninstall — without grinding on against an owner who
/// is re-creating keys on purpose.
const DELETE_ROUNDS: u32 = 3;

/// How many *re*-attempts one `delete_tree_under` call may spend in total.
/// `DELETE_ROUNDS` alone is per key and the rounds nest, so a deep tree could
/// otherwise cost `DELETE_ROUNDS^depth` attempts. Each key's first attempt is
/// free; only a retry draws on this budget, so an ordinary delete of any size
/// never touches it.
const RETRY_BUDGET: u32 = 32;

/// What removing a key actually needs: `DELETE` for `NtDeleteKey`, plus the
/// two read rights that let us list a key's children and see a link value on
/// it. `KEY_ALL_ACCESS` would also demand `WRITE_DAC` and `WRITE_OWNER`, which
/// a hive's owner can deny us purely to block the uninstall. The `DELETE` bit
/// is spelled out because the `windows` crate exports it only from the
/// file-system namespace, which this crate does not otherwise need.
const DELETE_ACCESS: REG_SAM_FLAGS =
    REG_SAM_FLAGS(0x0001_0000 | KEY_ENUMERATE_SUB_KEYS.0 | KEY_QUERY_VALUE.0);

/// What a segment we merely pass *through* needs: enough to read
/// `SymbolicLinkValue` on it, and not one right more. Opening a child needs no
/// particular right on the parent's handle, so `KEY_READ` — which also asks
/// for `READ_CONTROL`, `KEY_ENUMERATE_SUB_KEYS` and `KEY_NOTIFY` — would only
/// hand the hive's owner three more ACEs to deny. One of those on, say,
/// `Directory\shell` would have blocked every delete below it.
///
/// It is also what a *root* handle needs when all the caller does with it is
/// open children — which is every hive root the all-users uninstall holds (see
/// `super::hive`), and for exactly the same reason: `KEY_READ` on
/// `HKEY_USERS\<SID>_Classes` is one Deny ACE away from stopping the uninstall
/// at the door.
pub(super) const TRAVERSE_ACCESS: REG_SAM_FLAGS = REG_SAM_FLAGS(KEY_QUERY_VALUE.0);

/// What the read-only openers ask for: the right to list a key's children and
/// the right to read a value on it, which between them is everything any
/// caller here does with such a handle.
///
/// `KEY_READ` is what this used to be, and it was the same mistake
/// `TRAVERSE_ACCESS` exists to avoid: it bundles in `READ_CONTROL` and
/// `KEY_NOTIFY`, neither of which is ever used, and each of which is one more
/// ACE a hive's owner can deny to make a key we can read perfectly well come
/// back as one we may not open at all.
const READ_ACCESS: REG_SAM_FLAGS = REG_SAM_FLAGS(KEY_QUERY_VALUE.0 | KEY_ENUMERATE_SUB_KEYS.0);

/// A name buffer past this size means something other than a key name; the
/// registry caps names at 255 characters.
const MAX_NAME_CHARS: usize = 64 * 1024;

/// NUL-terminated UTF-16, for the `PCWSTR` registry APIs.
pub(super) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Give an open key handle back to the registry; a failed close leaves a
/// caller nothing to do about it.
///
/// Private on purpose: every opener in this module hands back an [`OwnedKey`],
/// so nobody outside it has a raw handle to close and nobody can close one an
/// `OwnedKey` still holds. A caller who makes a handle of its own — a test
/// with `RegCreateKeyExW` — hands it to [`OwnedKey::own`] instead.
fn close(h: HKEY) {
    unsafe {
        let _ = RegCloseKey(h);
    }
}

/// An open key that closes itself, so a walk can give up at any point without
/// leaking the handles it opened on the way down — and so can a caller holding
/// a root it works relative to (see `open_owned`).
///
/// The only kind of key handle this module hands out, and with [`OwnedKey::own`]
/// the only kind anyone else need hold either: closing is nobody's job to
/// remember, and there is no raw handle about to be closed twice.
#[derive(Debug)]
pub(super) struct OwnedKey(HKEY);

impl OwnedKey {
    pub(super) fn get(&self) -> HKEY {
        self.0
    }

    /// Take over a handle someone else opened, so it closes with everything
    /// this module hands out. For a caller that had to call `RegCreateKeyExW`
    /// or the like itself; never for a handle an `OwnedKey` already holds,
    /// which would close it twice.
    pub(super) fn own(h: HKEY) -> Self {
        OwnedKey(h)
    }
}

impl Drop for OwnedKey {
    fn drop(&mut self) {
        close(self.0);
    }
}

/// Open `root\subpath` for read, **following any symbolic link on the way**,
/// keeping apart the two ways that can fail: the key is not there, or it is
/// there and would not open.
///
/// Reading is all this is for. Anything that will write or delete wants
/// `open_owned_no_links`, which refuses to be redirected.
fn open_subkey_reporting(root: HKEY, subpath: &str) -> Result<HKEY, OpenFailure> {
    let w = wide(subpath);
    let mut h = HKEY::default();
    let status = unsafe { RegOpenKeyExW(root, PCWSTR(w.as_ptr()), 0, READ_ACCESS, &mut h) };
    if status == ERROR_SUCCESS {
        Ok(h)
    } else if status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND {
        Err(OpenFailure::Absent)
    } else {
        Err(OpenFailure::Failed(status.0))
    }
}

/// Open `root\subpath` for read, **following any symbolic link on the way**.
/// `None` when the key does not exist *or* cannot be opened — a caller that
/// must tell those apart wants [`open_owned_reporting`], because reading a
/// refusal as absence is how a key gets left behind in silence.
pub(super) fn open_owned(root: HKEY, subpath: &str) -> Option<OwnedKey> {
    open_subkey_reporting(root, subpath).ok().map(OwnedKey)
}

/// What a read-only open found. The third arm is the one worth having: a key
/// that is there and will not open is not the same as no key, and an uninstall
/// that treats it as no key moves on and leaves it.
pub(super) enum Found {
    Key(OwnedKey),
    Absent,
    /// Why it would not open, in words fit to print, naming the key.
    Refused(String),
}

/// Open `root\subpath` for read, **following any symbolic link on the way**,
/// keeping absence and refusal apart.
pub(super) fn open_owned_reporting(root: HKEY, subpath: &str) -> Found {
    match open_subkey_reporting(root, subpath) {
        Ok(h) => Found::Key(OwnedKey(h)),
        Err(OpenFailure::Absent) => Found::Absent,
        Err(OpenFailure::Failed(e)) => {
            Found::Refused(format!("{}: {}", normalised(subpath), explain_error(e)))
        }
    }
}

/// The immediate subkey names of `root\subpath`, or `Err` with a reason when
/// the list could not be read in full — never a short list passed off as a
/// complete one, which would let an uninstall conclude a hive was already
/// clean and move on leaving the keys behind.
///
/// Only a key that is genuinely *not there* has no children, and that is the
/// one `Ok(empty)`. A key that is there and refuses to open — the hive's owner
/// can deny us the read — is a failure and says so.
///
/// Either way the reason opens with the key it is about, punctuated as every
/// reason in this module is (see the module docs).
///
/// Like `open_owned`, this follows a symbolic link at any segment, so it is
/// for reading only.
pub(super) fn enum_subkeys(root: HKEY, subpath: &str) -> Result<Vec<String>, String> {
    let named = normalised(subpath);
    let key = match open_subkey_reporting(root, subpath) {
        Ok(h) => h,
        Err(OpenFailure::Absent) => return Ok(Vec::new()),
        Err(OpenFailure::Failed(e)) => return Err(format!("{named}: {}", explain_error(e))),
    };
    let out = enum_children(key).map_err(|why| format!("{named}: {why}"));
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
            let trouble = if status == ERROR_ACCESS_DENIED {
                "we are not allowed to read"
            } else {
                "could not read"
            };
            return Err(format!(
                "{trouble} subkey {index} (error {}); read {} before it",
                status.0,
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

/// Whether `root\subpath` carries a `REG_LINK` `SymbolicLinkValue`: `Ok(true)`
/// if it does, `Ok(false)` if it is a plain key or is not there at all, and
/// `Err` when it is there and would not open, so we could not tell.
///
/// That third answer is the reason this is not a `bool`. A key whose owner has
/// denied us the read is exactly the key most likely to be a link, and folding
/// it into `false` would have this fail *open*: the one call that exists to say
/// "do not walk through that" would say "go ahead" under the one condition that
/// should stop it. Callers must decide what to do about not knowing; none of
/// them may read it as "not a link".
///
/// It asks for [`TRAVERSE_ACCESS`] and no more — reading one value is all it
/// does — so there is only `KEY_QUERY_VALUE` for an owner to deny in the first
/// place.
///
/// Carries the same caveat as `is_link_handle`: a plain key can be dressed up
/// to look like this, so treat a `true` as "do not walk through it", not as
/// "this key is not mine to delete".
pub(super) fn is_reg_link(root: HKEY, subpath: &str) -> Result<bool, String> {
    // `open_component` passes REG_OPTION_OPEN_LINK, which opens the link itself
    // rather than its target — and, as ever, protects only the last segment,
    // which is the one being asked about.
    match open_component(root, subpath, TRAVERSE_ACCESS) {
        Ok(h) => {
            let link = is_link_handle(h);
            close(h);
            Ok(link)
        }
        Err(OpenFailure::Absent) => Ok(false),
        Err(OpenFailure::Failed(e)) => Err(format!(
            "{}: {}, so whether it is a symbolic link is not knowable",
            normalised(subpath),
            explain_error(e)
        )),
    }
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

/// Open one segment of a path as itself, ready to be passed *through*: a
/// segment carrying a link value stops the walk, because stepping through a
/// real link is what would take us out of the hive.
fn open_through(parent: HKEY, name: &str, trail: &str) -> Result<OwnedKey, PathFailure> {
    let key = match open_component(parent, name, TRAVERSE_ACCESS) {
        Ok(h) => OwnedKey(h),
        Err(OpenFailure::Absent) => return Err(PathFailure::Absent),
        Err(OpenFailure::Failed(e)) => {
            return Err(PathFailure::Refused(format!(
                "{trail}: {}",
                explain_error(e)
            )))
        }
    };
    if is_link_handle(key.get()) {
        return Err(PathFailure::Refused(format!(
            "{trail}: carries a REG_LINK SymbolicLinkValue; not walking through it"
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
        held = Some(open_through(parent, seg, &trail)?);
    }
    if !trail.is_empty() {
        trail.push('\\');
    }
    trail.push_str(leaf_name);
    let parent = held.as_ref().map_or(root, OwnedKey::get);
    // The leaf is a destination, not a step: we stop here, so a link value on
    // it redirects nothing. `REG_OPTION_OPEN_LINK` means the handle names the
    // leaf itself either way.
    match open_component(parent, leaf_name, access) {
        Ok(h) => Ok(OwnedKey(h)),
        Err(OpenFailure::Absent) => Err(PathFailure::Absent),
        Err(OpenFailure::Failed(e)) => Err(PathFailure::Refused(format!(
            "{trail}: {}",
            explain_error(e)
        ))),
    }
}

/// Open `root\subpath` without ever being redirected by a symbolic link, as a
/// handle that closes itself.
///
/// Each segment is opened from the handle above it with
/// `REG_OPTION_OPEN_LINK` — which protects only the *last* component of a
/// path, so one call per segment is the point — and the walk stops at the
/// first segment it would have to pass *through* that carries a `REG_LINK`
/// `SymbolicLinkValue`. That check fails closed: a plain key wearing the value
/// blocks the path too. On a path the uninstall owns that is the safe way
/// round, and the message says what was found rather than asserting the key is
/// a link.
///
/// The leaf is exempt, because the walk ends there rather than going through
/// it: if it is a link you get a handle to the link key itself, never its
/// target. A caller that means to write values should bear that in mind; one
/// that means to delete wants exactly this.
///
/// This is the only opener to use for a handle you will write or delete
/// through.
pub(super) fn open_owned_no_links(
    root: HKEY,
    subpath: &str,
    access: REG_SAM_FLAGS,
) -> Result<OwnedKey, String> {
    match walk_no_links(root, subpath, access) {
        Ok(key) => Ok(key),
        Err(PathFailure::Absent) => Err(format!("{}: is not there", normalised(subpath))),
        Err(PathFailure::Refused(why)) => Err(why),
    }
}

/// One `delete_tree_under` call: the notes it has gathered and what is left
/// of its shared retry budget.
struct Sweep {
    /// Paths already noted, so a retry round that meets the same key again
    /// does not say it twice.
    noted: std::collections::HashSet<String>,
    notes: Vec<String>,
    retries_left: u32,
}

impl Sweep {
    fn new() -> Self {
        Sweep {
            noted: std::collections::HashSet::new(),
            notes: Vec::new(),
            retries_left: RETRY_BUDGET,
        }
    }

    /// Record that a key wearing a link value has been removed. Call this only
    /// once the delete has succeeded — a note that says "removed" about a key
    /// still sitting there would be worse than no note at all.
    fn note_link_removed(&mut self, path: &str) {
        if self.noted.insert(path.to_string()) {
            self.notes.push(format!(
                "{path}: carried a REG_LINK SymbolicLinkValue; removed that key itself, never what it named"
            ));
        }
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
impl Sweep {
    fn clear_children(&mut self, key: HKEY, trail: &str, depth: u32) -> Result<(), String> {
        if depth == 0 {
            return Err(format!("{trail}: nested deeper than we will walk"));
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
                        first = Some(format!("{here}: {}", explain_error(e)));
                    }
                    continue;
                }
            };
            let was_link = is_link_handle(child.get());
            match self.clear_and_delete(&child, &here, depth - 1) {
                // Only now is the note true: the key is actually gone.
                Ok(()) => {
                    if was_link {
                        self.note_link_removed(&here);
                    }
                }
                Err(why) => {
                    failed += 1;
                    if first.is_none() {
                        first = Some(why);
                    }
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

    /// Empty a key and delete it, through its own handle throughout. If
    /// something re-creates subkeys under it while we work, empty it and try
    /// again a few times before giving up, so a benign race does not read as a
    /// refusal. Re-attempts draw on a budget shared by the whole operation, so
    /// nesting cannot multiply them out.
    fn clear_and_delete(&mut self, key: &OwnedKey, trail: &str, depth: u32) -> Result<(), String> {
        let mut last = None;
        for round in 0..DELETE_ROUNDS {
            if round > 0 {
                if self.retries_left == 0 {
                    return Err(format!(
                        "{trail}: kept changing while we worked; gave up after {RETRY_BUDGET} retries across the tree"
                    ));
                }
                self.retries_left -= 1;
            }
            self.clear_children(key.get(), trail, depth)?;
            let status = delete_this_key(key.get());
            if status == STATUS_SUCCESS {
                return Ok(());
            }
            let why = format!("{trail}: {}", explain_status(status));
            if status != STATUS_CANNOT_DELETE {
                return Err(why);
            }
            last = Some(why);
        }
        Err(last.unwrap_or_else(|| format!("{trail}: would not delete")))
    }
}

/// Delete the subtree `root\subpath` on a hive we do not trust, never
/// following a symbolic link — not at a segment of the path, and not at any
/// key inside the subtree.
///
/// The path is opened by the same segment-at-a-time walk as
/// `open_owned_no_links`, which refuses to go through a segment carrying a
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
/// A link value only ever stops the walk at a segment we would pass
/// *through*, because stepping through a real link is what would take SYSTEM
/// out of the hive. At the leaf and at every key inside the subtree it stops
/// nothing: we never step through those, the value proves nothing anyway — an
/// ordinary key can carry it — and honouring it would let a hive owner keep
/// any key, the menu key included, simply by labelling it. Such a key is
/// removed as the key it is, whatever it names left untouched, and noted for
/// the log.
///
/// What remains is not a way through but a way to be told no: the hive's owner
/// can lock a key against us, or keep re-creating keys faster than
/// `DELETE_ROUNDS` and `RETRY_BUDGET` allow, and either ends as `Refused`
/// naming the key that would not go.
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
    let was_link = is_link_handle(leaf.get());
    let mut sweep = Sweep::new();
    let outcome = sweep.clear_and_delete(&leaf, &trail, MAX_DEPTH);
    if outcome.is_ok() && was_link {
        sweep.note_link_removed(&trail);
    }
    let notes = sweep.notes;
    match outcome {
        Ok(()) => DeleteOutcome::Deleted { notes },
        Err(why) => DeleteOutcome::Refused { why, notes },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    use windows::Win32::System::Registry::{
        RegCreateKeyExW, RegDeleteKeyExW, RegDeleteTreeW, RegSetValueExW, HKEY_CURRENT_USER,
        HKEY_USERS, KEY_ALL_ACCESS, KEY_CREATE_LINK, KEY_NOTIFY, KEY_READ, KEY_SET_VALUE,
        KEY_WRITE, REG_OPTION_CREATE_LINK, REG_OPTION_NON_VOLATILE,
    };

    use super::super::test_support::Denied;

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
            trouble.get_or_insert(format!("{name}: {}", explain_status(status)));
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
            // The clock ticks every 100 ns here, which two tests starting
            // together can share; the counter is what actually keeps them apart.
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let me = Scratch {
                path: format!(
                    r"Software\Kuvatin-regutil-test-{}-{nanos}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
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
            match open_owned_no_links(HKEY_CURRENT_USER, &self.path, READ_ACCESS) {
                Ok(_) => {
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
            if open_owned(HKEY_USERS, &format!(r"{sid}\{}", scratch.path)).is_some() {
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
        let key = open_owned_no_links(HKEY_CURRENT_USER, path, KEY_SET_VALUE)
            .unwrap_or_else(|why| panic!("open {path}: {why}"));
        let status = set_link_value(key.get(), target_nt);
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

        assert_eq!(
            is_reg_link(HKEY_CURRENT_USER, &scratch.at("beta")),
            Ok(false)
        );
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
    fn refuses_a_link_it_would_walk_through_but_deletes_one_at_the_leaf() {
        let scratch = Scratch::new();
        // What a hostile hive would aim a link at: keys that must survive.
        create(&scratch.at("target"));
        create(&scratch.at(r"target\keep"));
        create(&scratch.at(r"target\shell\Kuvatin\sentinel"));

        let link = scratch.at("planted-link");
        let target_nt = format!(r"{}\{}", hive_nt_path(&scratch), scratch.at("target"));
        create_link(&link, &target_nt);

        // The link is real: it reads as a link, and it resolves to the target.
        assert_eq!(is_reg_link(HKEY_CURRENT_USER, &link), Ok(true));
        assert_eq!(
            is_reg_link(HKEY_CURRENT_USER, &scratch.at("target")),
            Ok(false)
        );
        assert_eq!(
            kids(&link),
            vec!["keep", "shell"],
            "the link should resolve"
        );

        // A link as an *intermediate* segment is refused: walking through it
        // is the one move that would take us out of the hive.
        let why = refusal(delete_tree_under(
            HKEY_CURRENT_USER,
            &scratch.at(r"planted-link\shell\Kuvatin"),
        ));
        assert!(
            why.contains("planted-link") && why.contains("SymbolicLinkValue"),
            "unhelpful reason: {why}"
        );
        assert_eq!(kids(&scratch.at("target")), vec!["keep", "shell"]);
        assert_eq!(kids(&scratch.at(r"target\shell\Kuvatin")), vec!["sentinel"]);

        // A link as the *leaf* is deleted, entry and all. We stop there rather
        // than step through it, so nothing is redirected — and refusing would
        // have let one planted value keep the menu key for ever.
        let outcome = delete_tree_under(HKEY_CURRENT_USER, &link);
        assert!(
            matches!(outcome, DeleteOutcome::Deleted { .. }),
            "a link at the leaf should go: {outcome:?}"
        );
        assert!(
            outcome.notes().iter().any(|n| n.contains("planted-link")),
            "removing a link should be noted, got {:?}",
            outcome.notes()
        );
        assert_eq!(
            is_reg_link(HKEY_CURRENT_USER, &link),
            Ok(false),
            "a key that has gone is not a link"
        );
        assert!(open_owned(HKEY_CURRENT_USER, &link).is_none());

        // …and what it pointed at is untouched.
        assert_eq!(
            kids(&scratch.at(r"target\shell\Kuvatin")),
            vec!["sentinel"],
            "the target must outlive its link"
        );

        // With no link in the way the same call deletes, and says nothing.
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
        assert_eq!(is_reg_link(HKEY_CURRENT_USER, &nested), Ok(true));

        let outcome = delete_tree_under(HKEY_CURRENT_USER, &scratch.at("Kuvatin"));
        assert!(
            matches!(outcome, DeleteOutcome::Deleted { .. }),
            "{outcome:?}"
        );
        // The planted link is worth a line in the uninstall log — one line,
        // however many rounds the sweep took.
        assert_eq!(
            outcome.notes().len(),
            1,
            "one link, one note: {:?}",
            outcome.notes()
        );
        assert!(
            outcome.notes()[0].contains("nested-link"),
            "a planted link should be named, got {:?}",
            outcome.notes()
        );
        assert!(open_owned(HKEY_CURRENT_USER, &scratch.at("Kuvatin")).is_none());
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
        assert!(open_owned(HKEY_CURRENT_USER, &scratch.at("Kuvatin")).is_none());
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
        assert!(open_owned(HKEY_CURRENT_USER, &scratch.at("Kuvatin")).is_none());
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
        assert_eq!(
            is_reg_link(HKEY_CURRENT_USER, &scratch.at(r"Kuvatin\shell")),
            Ok(true),
            "the value should make a plain key read as a link"
        );

        // Honouring that value would strand `command` and `open` and leave the
        // whole subtree behind.
        let outcome = delete_tree_under(HKEY_CURRENT_USER, &scratch.at("Kuvatin"));
        assert!(
            matches!(outcome, DeleteOutcome::Deleted { .. }),
            "{outcome:?}"
        );
        assert!(open_owned(HKEY_CURRENT_USER, &scratch.at("Kuvatin")).is_none());
        // Nothing was followed on the way: the named target is untouched.
        assert_eq!(kids(&scratch.at("victim")), vec!["precious"]);
    }

    #[test]
    fn a_denied_key_notify_on_an_intermediate_does_not_block_the_delete() {
        let scratch = Scratch::new();
        create(&scratch.at(r"Kuvatin\shell\command"));
        let gate = scratch.at("Kuvatin");

        // Setting a DACL from the test process may not be possible everywhere;
        // say so rather than quietly proving nothing.
        let denied = match Denied::on(&gate, KEY_NOTIFY) {
            Ok(guard) => guard,
            Err(why) => {
                eprintln!("skipping: could not set a Deny ACE on {gate}: {why}");
                return;
            }
        };

        // The ACE really bites: KEY_READ asks for KEY_NOTIFY and is refused.
        // That it is KEY_READ spelled out here and not one of this module's own
        // constants is the point — none of them asks for KEY_NOTIFY any more.
        let parent = open_owned_no_links(HKEY_CURRENT_USER, &scratch.path, TRAVERSE_ACCESS)
            .expect("scratch");
        let denied_read = open_component(parent.get(), "Kuvatin", KEY_READ);
        assert!(
            matches!(denied_read, Err(OpenFailure::Failed(e)) if e == ERROR_ACCESS_DENIED.0),
            "the Deny ACE should refuse KEY_READ, or this test proves nothing"
        );
        // …while the rights this module actually asks for still open.
        for (access, named) in [
            (TRAVERSE_ACCESS, "TRAVERSE_ACCESS"),
            (READ_ACCESS, "READ_ACCESS"),
            (DELETE_ACCESS, "DELETE_ACCESS"),
        ] {
            let opened = open_component(parent.get(), "Kuvatin", access);
            assert!(opened.is_ok(), "{named} should open under the same ACE");
            if let Ok(h) = opened {
                close(h);
            }
        }
        drop(parent);

        // So a delete below the gated key goes through.
        assert_eq!(
            delete_tree_under(HKEY_CURRENT_USER, &scratch.at(r"Kuvatin\shell")),
            deleted()
        );
        assert!(open_owned(HKEY_CURRENT_USER, &scratch.at(r"Kuvatin\shell")).is_none());
        drop(denied);
    }

    /// The one answer `is_reg_link` must never give: "not a link" about a key
    /// it could not read. A key whose owner has denied us the read is exactly
    /// the key most likely to be one, so folding that into `false` would have
    /// the call that exists to stop a walk wave it through.
    #[test]
    fn a_key_we_may_not_read_is_not_reported_as_not_a_link() {
        let scratch = Scratch::new();
        create(&scratch.at("quiet"));
        let gate = scratch.at("quiet");
        assert_eq!(
            is_reg_link(HKEY_CURRENT_USER, &gate),
            Ok(false),
            "a plain key we can read is plainly not a link"
        );

        // Denying the one right the check asks for. Setting a DACL from the
        // test process may not be possible everywhere; say so rather than
        // quietly proving nothing.
        let denied = match Denied::on(&gate, KEY_QUERY_VALUE) {
            Ok(guard) => guard,
            Err(why) => {
                eprintln!("skipping: could not set a Deny ACE on {gate}: {why}");
                return;
            }
        };

        let why = is_reg_link(HKEY_CURRENT_USER, &gate)
            .expect_err("a key we may not read is not a key we know about");
        assert!(why.contains("not allowed"), "vague reason: {why}");
        assert!(
            why.contains("quiet"),
            "a log line needs the key, got: {why}"
        );
        drop(denied);

        // A key that is not there at all is a different thing, and is `false`:
        // nothing to walk through means nothing to refuse.
        assert_eq!(
            is_reg_link(HKEY_CURRENT_USER, &scratch.at("nowhere")),
            Ok(false)
        );
    }

    #[test]
    fn a_key_we_may_not_open_is_not_reported_as_empty() {
        let scratch = Scratch::new();
        create(&scratch.at(r"assoc\.png\shell\Kuvatin"));
        let gate = scratch.at("assoc");

        // A key that is simply not there has no children, and says so.
        assert_eq!(
            enum_subkeys(HKEY_CURRENT_USER, &scratch.at("nowhere")),
            Ok(Vec::new()),
            "an absent key is not a read failure"
        );

        // Denying the right the read actually asks for. `KEY_NOTIFY` would not
        // do: nothing here asks for it any more, which is the whole point of
        // `READ_ACCESS`, and a test denying it would pass while proving nothing.
        // Setting a DACL from the test process may not be possible everywhere;
        // say so rather than quietly proving nothing.
        let denied = match Denied::on(&gate, KEY_ENUMERATE_SUB_KEYS) {
            Ok(guard) => guard,
            Err(why) => {
                eprintln!("skipping: could not set a Deny ACE on {gate}: {why}");
                return;
            }
        };

        // Reading this one back as "no children" would tell an uninstall the
        // hive was already clean, and it would move on and leave the keys.
        let why = enum_subkeys(HKEY_CURRENT_USER, &gate)
            .expect_err("a key we may not open is not an empty key");
        assert!(why.contains("not allowed"), "vague reason: {why}");
        assert!(
            why.contains("assoc"),
            "a log line needs the path, got: {why}"
        );
        drop(denied);

        // With the ACE gone the same call reads it.
        assert_eq!(
            enum_subkeys(HKEY_CURRENT_USER, &gate).expect("readable again"),
            vec![".png"]
        );
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
        assert!(open_owned(HKEY_CURRENT_USER, &scratch.at("many")).is_none());
    }

    #[test]
    fn open_owned_no_links_says_which_way_it_failed() {
        let scratch = Scratch::new();
        create(&scratch.at(r"Kuvatin\shell"));
        drop(
            open_owned_no_links(
                HKEY_CURRENT_USER,
                &scratch.at(r"Kuvatin\shell"),
                READ_ACCESS,
            )
            .expect("open a plain path"),
        );

        let why = open_owned_no_links(HKEY_CURRENT_USER, &scratch.at("nowhere"), READ_ACCESS)
            .expect_err("absent");
        assert!(why.contains("is not there"), "{why}");

        let target_nt = format!(r"{}\{}", hive_nt_path(&scratch), scratch.at("Kuvatin"));
        create_link(&scratch.at("a-link"), &target_nt);
        let why = open_owned_no_links(HKEY_CURRENT_USER, &scratch.at(r"a-link\shell"), READ_ACCESS)
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

        let parent = open_owned_no_links(HKEY_CURRENT_USER, &scratch.path, DELETE_ACCESS)
            .expect("open scratch");
        let name = wide("probe-link");
        let removed = unsafe { RegDeleteKeyExW(parent.get(), PCWSTR(name.as_ptr()), 0, 0) };
        drop(parent);

        assert_eq!(removed, ERROR_SUCCESS);
        assert!(
            open_owned(HKEY_CURRENT_USER, &scratch.at("empty-target")).is_none(),
            "RegDeleteKeyExW took the link's target"
        );
        assert_eq!(
            is_reg_link(HKEY_CURRENT_USER, &link),
            Ok(true),
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

        let parent = open_owned_no_links(HKEY_CURRENT_USER, &scratch.path, DELETE_ACCESS)
            .expect("open scratch");
        let leaf = open_as_itself(parent.get(), "doomed").expect("open doomed");
        // Even handed a vetted handle and a NULL subkey, RegDeleteTreeW walks
        // its descendants by name — and follows the link it meets.
        unsafe {
            let _ = RegDeleteTreeW(leaf, PCWSTR::null());
        }
        close(leaf);
        drop(parent);

        assert!(
            open_owned(HKEY_CURRENT_USER, &scratch.at("victim")).is_none(),
            "RegDeleteTreeW reached out of the subtree and took the link's target"
        );
        assert_eq!(
            is_reg_link(HKEY_CURRENT_USER, &nested),
            Ok(true),
            "…and left the link itself standing"
        );
    }
}
