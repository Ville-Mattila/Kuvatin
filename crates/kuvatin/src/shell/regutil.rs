//! Registry helpers that operate on an arbitrary open `HKEY` root, so the same
//! code can act on `HKEY_CURRENT_USER\Software\Classes` (the per-user unregister)
//! and on a mounted `HKEY_USERS\<SID>_Classes` hive (the all-users uninstall).
//!
//! The all-users path runs as SYSTEM against a hive its owner can write, so
//! every deletion here assumes the hive is hostile: a user may plant registry
//! symbolic links both along the path we walk and inside the subtree we
//! delete, and SYSTEM must not follow one out of the hive. `delete_tree_under`
//! therefore resolves each name exactly once and works through the handle it
//! got back — refusing at the first link on the path, and deleting a link met
//! inside the subtree as the entry it is. `RegDeleteTreeW` and
//! `RegDeleteKeyExW` are no use here: both re-open keys by name and follow a
//! link they find (two tests at the bottom of this file pin that down).
//!
//! Nothing outside `#[cfg(test)]` calls into this module yet: later tasks in
//! the all-users-uninstall plan wire `windows.rs` and the new all-users path
//! to it. Until then, allow the otherwise-unused helpers.
#![allow(dead_code)]

use windows::core::{PCWSTR, PWSTR};
use windows::Wdk::System::Registry::NtDeleteKey;
use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, HANDLE, NTSTATUS,
    STATUS_SUCCESS,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, KEY_ALL_ACCESS, KEY_READ,
    REG_LINK, REG_OPTION_OPEN_LINK, REG_SAM_FLAGS, REG_VALUE_TYPE,
};

/// Keys nested deeper than this inside a subtree we are deleting are treated
/// as a refusal rather than walked; the registry's own limit is well under it.
const MAX_DEPTH: u32 = 512;

/// NUL-terminated UTF-16, for the `PCWSTR` registry APIs.
pub(super) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Open `root\subpath` for read, following any symbolic-link key. `None` when
/// the key does not exist or cannot be opened.
pub(super) fn open_subkey(root: HKEY, subpath: &str) -> Option<HKEY> {
    let w = wide(subpath);
    let mut h = HKEY::default();
    let status = unsafe { RegOpenKeyExW(root, PCWSTR(w.as_ptr()), 0, KEY_READ, &mut h) };
    (status == ERROR_SUCCESS).then_some(h)
}

/// Give an open key handle back to the registry; a failed close leaves a
/// caller nothing to do about it.
pub(super) fn close(h: HKEY) {
    unsafe {
        let _ = RegCloseKey(h);
    }
}

/// The immediate subkey names of `root\subpath` (empty when the key is absent).
pub(super) fn enum_subkeys(root: HKEY, subpath: &str) -> Vec<String> {
    let Some(key) = open_subkey(root, subpath) else {
        return Vec::new();
    };
    let out = enum_children(key);
    close(key);
    out
}

/// The immediate subkey names of the key an open handle names.
fn enum_children(key: HKEY) -> Vec<String> {
    let mut out = Vec::new();
    let mut index = 0u32;
    // Key names stop at 255 characters, but ask again with a bigger buffer if
    // a hive ever says otherwise, and skip an entry that will not read at all
    // rather than cutting the list short at it — a truncated list would leave
    // the uninstall thinking a hive is already clean.
    let mut buf = vec![0u16; 256];
    let mut failures = 0u32;
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
            break;
        }
        if status == ERROR_MORE_DATA && buf.len() < 64 * 1024 {
            buf = vec![0u16; buf.len() * 2];
            continue; // same index, roomier buffer
        }
        if status == ERROR_SUCCESS {
            out.push(String::from_utf16_lossy(&buf[..len as usize]));
            failures = 0;
        } else {
            // A dead handle fails every index; give up instead of counting to
            // u32::MAX.
            failures += 1;
            if failures > 16 {
                break;
            }
        }
        index += 1;
    }
    out
}

/// Delete the key an open handle names — the key itself, with no second look
/// at its name, so nothing planted at that name can redirect the delete. The
/// key must already be empty of subkeys.
fn delete_this_key(h: HKEY) -> NTSTATUS {
    unsafe { NtDeleteKey(HANDLE(h.0)) }
}

/// True when the key an open handle names carries a `REG_LINK`
/// `SymbolicLinkValue` — only meaningful for a handle opened with
/// `REG_OPTION_OPEN_LINK`, which names the link itself rather than its target.
fn is_link_handle(h: HKEY) -> bool {
    let name = wide("SymbolicLinkValue");
    let mut kind = REG_VALUE_TYPE::default();
    let status =
        unsafe { RegQueryValueExW(h, PCWSTR(name.as_ptr()), None, Some(&mut kind), None, None) };
    status == ERROR_SUCCESS && kind == REG_LINK
}

/// True when `root\subpath` is a registry symbolic link (a planted `REG_LINK`
/// inside a user's own hive). Such a key must NOT be deleted with
/// `RegDeleteTreeW`, which would follow it.
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

/// What `delete_tree_under` did, so the uninstall log can say why a key is
/// still there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DeleteOutcome {
    /// The key existed and its subtree is gone.
    Deleted,
    /// Nothing to do: the key, or a segment on the way to it, is not there.
    Absent,
    /// Left alone on purpose. The reason names the offending segment and is
    /// meant to be printed as-is.
    Refused(String),
}

/// Why one component would not open.
enum OpenFailure {
    /// It simply is not there.
    Absent,
    /// The `RegOpenKeyExW` error code.
    Failed(u32),
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
    } else if status == ERROR_FILE_NOT_FOUND {
        Err(OpenFailure::Absent)
    } else {
        Err(OpenFailure::Failed(status.0))
    }
}

/// Empty the key an open handle names, reaching every descendant through its
/// own handle so that no planted link is ever followed. A link met inside the
/// subtree has its *entry* deleted — it is a key of ours to remove — while
/// whatever it points at is left untouched.
///
/// `RegDeleteTreeW` cannot do this job. It re-opens each descendant by name
/// and follows a link it finds there, so against a hostile hive it deletes
/// keys outside the hive altogether: `reg_delete_tree_follows_a_nested_link`
/// watches it take a link's target away.
///
/// The child list is a snapshot, so a hive owner racing us can add a key after
/// we read it; then the parent will not delete and the caller hears why,
/// which is the right end to a fight nobody can win.
fn clear_children(key: HKEY, trail: &str, depth: u32) -> Result<(), String> {
    if depth == 0 {
        return Err(format!("{trail} is nested deeper than we will walk"));
    }
    for name in enum_children(key) {
        let here = format!(r"{trail}\{name}");
        let child = match open_component(key, &name, KEY_ALL_ACCESS) {
            Ok(h) => h,
            Err(OpenFailure::Absent) => continue, // already gone
            Err(OpenFailure::Failed(e)) => {
                return Err(format!("{here} would not open (error {e})"))
            }
        };
        if !is_link_handle(child) {
            if let Err(why) = clear_children(child, &here, depth - 1) {
                close(child);
                return Err(why);
            }
        }
        let removed = delete_this_key(child);
        close(child);
        if removed != STATUS_SUCCESS {
            return Err(format!(
                "{here} would not delete (status {:#010x})",
                removed.0
            ));
        }
    }
    Ok(())
}

/// Delete the subtree `root\subpath` on a hive we do not trust, never
/// following a symbolic link — not at a segment of the path, and not at any
/// key inside the subtree.
///
/// `REG_OPTION_OPEN_LINK` protects only the *last* component handed to
/// `RegOpenKeyExW`; every component before it is still resolved through links.
/// So the path is walked one segment at a time, each opened from the handle
/// above it with `REG_OPTION_OPEN_LINK`, and the walk refuses at the first
/// segment that turns out to be a link. The leaf is then opened the same way,
/// refused if it is a link, emptied by `clear_children` (which goes through a
/// handle for every descendant) and removed with `NtDeleteKey` on the very
/// handle we vetted.
///
/// Every name is therefore resolved exactly once, and what that resolution
/// produced is what gets deleted — a planted link has no second lookup to
/// hijack. Naming the leaf again for `RegDeleteKeyExW(parent, leaf_name)`
/// would reopen it, and that call *follows* a link, deleting its target and
/// leaving the link standing (`reg_delete_key_ex_follows_a_link` shows it), so
/// a hive owner who swapped the emptied leaf for a link in that instant would
/// have SYSTEM delete a key of their choosing.
///
/// What remains is not a way through but a way to be told no: the hive's owner
/// can keep re-creating keys under a subtree while we empty it, and each round
/// they win ends as `Refused` naming the key that would not go.
pub(super) fn delete_tree_under(root: HKEY, subpath: &str) -> DeleteOutcome {
    // `held` is the one handle the walk owns at a time — the current parent.
    let mut held: Option<HKEY> = None;
    let outcome = delete_walk(root, subpath, &mut held);
    if let Some(h) = held {
        close(h);
    }
    outcome
}

fn delete_walk(root: HKEY, subpath: &str, held: &mut Option<HKEY>) -> DeleteOutcome {
    let segments: Vec<&str> = subpath.split('\\').filter(|s| !s.is_empty()).collect();
    let Some((leaf_name, above)) = segments.split_last() else {
        // An empty path would name the root itself; never delete that.
        return DeleteOutcome::Refused("an empty path names no key".to_string());
    };

    let mut parent = root;
    for seg in above {
        let child = match open_component(parent, seg, KEY_READ) {
            Ok(h) => h,
            Err(OpenFailure::Absent) => return DeleteOutcome::Absent,
            Err(OpenFailure::Failed(e)) => {
                return DeleteOutcome::Refused(format!("{seg} would not open (error {e})"))
            }
        };
        if let Some(previous) = held.replace(child) {
            close(previous);
        }
        parent = child;
        if is_link_handle(parent) {
            return DeleteOutcome::Refused(format!("{seg} is a symbolic link"));
        }
    }

    let leaf = match open_component(parent, leaf_name, KEY_ALL_ACCESS) {
        Ok(h) => h,
        Err(OpenFailure::Absent) => return DeleteOutcome::Absent,
        Err(OpenFailure::Failed(e)) => {
            return DeleteOutcome::Refused(format!("{leaf_name} would not open (error {e})"))
        }
    };
    if is_link_handle(leaf) {
        close(leaf);
        return DeleteOutcome::Refused(format!("{leaf_name} is a symbolic link"));
    }
    if let Err(why) = clear_children(leaf, leaf_name, MAX_DEPTH) {
        close(leaf);
        return DeleteOutcome::Refused(why);
    }
    let removed = delete_this_key(leaf);
    close(leaf);
    if removed == STATUS_SUCCESS {
        DeleteOutcome::Deleted
    } else {
        DeleteOutcome::Refused(format!(
            "{leaf_name} would not delete (status {:#010x})",
            removed.0
        ))
    }
}

/// Open `root\subpath` with full access (for callers that then delete under it).
pub(super) fn open_subkey_rw(root: HKEY, subpath: &str) -> Option<HKEY> {
    let w = wide(subpath);
    let mut h = HKEY::default();
    let status = unsafe { RegOpenKeyExW(root, PCWSTR(w.as_ptr()), 0, KEY_ALL_ACCESS, &mut h) };
    (status == ERROR_SUCCESS).then_some(h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    use windows::Win32::System::Registry::{
        RegCreateKeyExW, RegDeleteKeyExW, RegDeleteTreeW, RegSetValueExW, HKEY_CURRENT_USER,
        HKEY_USERS, KEY_CREATE_LINK, KEY_SET_VALUE, KEY_WRITE, REG_OPTION_CREATE_LINK,
        REG_OPTION_NON_VOLATILE,
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
    fn purge(parent: HKEY, name: &str) {
        let Some(h) = open_as_itself(parent, name) else {
            return;
        };
        if !is_link_handle(h) {
            for kid in enum_subkeys(h, "") {
                purge(h, &kid);
            }
        }
        let _ = delete_this_key(h);
        close(h);
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
            purge(HKEY_CURRENT_USER, &self.path);
        }
    }

    /// The NT path of the hive HKCU maps to, found by looking for this run's
    /// own scratch key under each `HKEY_USERS` subkey — no SID lookup, and it
    /// proves the path really names the hive we are writing to.
    fn hive_nt_path(scratch: &Scratch) -> String {
        for sid in enum_subkeys(HKEY_USERS, "") {
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
        // SymbolicLinkValue carries the target with no terminating NUL.
        let target: Vec<u16> = target_nt.encode_utf16().collect();
        let bytes = unsafe {
            std::slice::from_raw_parts(
                target.as_ptr().cast::<u8>(),
                std::mem::size_of_val(&target[..]),
            )
        };
        let name = wide("SymbolicLinkValue");
        let status = unsafe { RegSetValueExW(h, PCWSTR(name.as_ptr()), 0, REG_LINK, Some(bytes)) };
        close(h);
        assert_eq!(
            status, ERROR_SUCCESS,
            "set SymbolicLinkValue on {link_path}"
        );
    }

    fn refusal(outcome: DeleteOutcome) -> String {
        match outcome {
            DeleteOutcome::Refused(why) => why,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn enumerates_and_deletes_subkeys() {
        let scratch = Scratch::new();
        create(&scratch.at("alpha"));
        create(&scratch.at("beta"));
        create(&scratch.at(r"gamma\deep\deeper"));

        let mut kids = enum_subkeys(HKEY_CURRENT_USER, &scratch.path);
        kids.sort();
        assert_eq!(kids, vec!["alpha", "beta", "gamma"]);

        assert_eq!(
            delete_tree_under(HKEY_CURRENT_USER, &scratch.at("alpha")),
            DeleteOutcome::Deleted
        );
        assert_eq!(
            delete_tree_under(HKEY_CURRENT_USER, &scratch.at("alpha")),
            DeleteOutcome::Absent // gone now
        );
        // A whole tree goes, not just an empty leaf.
        assert_eq!(
            delete_tree_under(HKEY_CURRENT_USER, &scratch.at("gamma")),
            DeleteOutcome::Deleted
        );
        // A missing segment above the leaf is absence, not a refusal.
        assert_eq!(
            delete_tree_under(HKEY_CURRENT_USER, &scratch.at(r"nowhere\deeper")),
            DeleteOutcome::Absent
        );
        // The root itself is never the target.
        assert!(refusal(delete_tree_under(HKEY_CURRENT_USER, "")).contains("empty path"));

        assert!(!is_reg_link(HKEY_CURRENT_USER, &scratch.at("beta")));
        assert_eq!(enum_subkeys(HKEY_CURRENT_USER, &scratch.path), vec!["beta"]);
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
        let mut through = enum_subkeys(HKEY_CURRENT_USER, &link);
        through.sort();
        assert_eq!(through, vec!["keep", "shell"], "the link should resolve");

        // A link as the leaf.
        let why = refusal(delete_tree_under(HKEY_CURRENT_USER, &link));
        assert!(
            why.contains("planted-link") && why.contains("symbolic link"),
            "unhelpful reason: {why}"
        );

        // A link as an *intermediate* segment — the case REG_OPTION_OPEN_LINK
        // on the full path does not cover.
        let why = refusal(delete_tree_under(
            HKEY_CURRENT_USER,
            &scratch.at(r"planted-link\shell\Kuvatin"),
        ));
        assert!(
            why.contains("planted-link") && why.contains("symbolic link"),
            "unhelpful reason: {why}"
        );

        // Neither refusal touched the target.
        let mut survivors = enum_subkeys(HKEY_CURRENT_USER, &scratch.at("target"));
        survivors.sort();
        assert_eq!(survivors, vec!["keep", "shell"]);
        assert_eq!(
            enum_subkeys(HKEY_CURRENT_USER, &scratch.at(r"target\shell\Kuvatin")),
            vec!["sentinel"]
        );

        // Deleting a link through its own handle removes the link entry, not
        // what it points at. (Naming it for RegDeleteKeyExW would do the
        // opposite — see `reg_delete_key_ex_follows_a_link`.)
        let parent = open_subkey_rw(HKEY_CURRENT_USER, &scratch.path).expect("open scratch");
        let itself = open_as_itself(parent, "planted-link").expect("open the link as itself");
        let removed = delete_this_key(itself);
        close(itself);
        close(parent);
        assert_eq!(removed, STATUS_SUCCESS, "delete the link entry");
        assert!(!is_reg_link(HKEY_CURRENT_USER, &link));
        assert!(open_subkey(HKEY_CURRENT_USER, &link).is_none());
        assert_eq!(
            enum_subkeys(HKEY_CURRENT_USER, &scratch.at(r"target\shell\Kuvatin")),
            vec!["sentinel"],
            "the target must outlive its link"
        );

        // With no link in the way the same call deletes.
        assert_eq!(
            delete_tree_under(HKEY_CURRENT_USER, &scratch.at("target")),
            DeleteOutcome::Deleted
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

        assert_eq!(
            delete_tree_under(HKEY_CURRENT_USER, &scratch.at("Kuvatin")),
            DeleteOutcome::Deleted
        );
        assert!(open_subkey(HKEY_CURRENT_USER, &scratch.at("Kuvatin")).is_none());
        // The link entry went with the tree; its target did not.
        assert_eq!(
            enum_subkeys(HKEY_CURRENT_USER, &scratch.at("victim")),
            vec!["precious"],
            "a link inside the tree must not drag its target in"
        );
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

        let parent = open_subkey_rw(HKEY_CURRENT_USER, &scratch.path).expect("open scratch");
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

        let parent = open_subkey_rw(HKEY_CURRENT_USER, &scratch.path).expect("open scratch");
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
