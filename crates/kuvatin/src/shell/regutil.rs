//! Registry helpers that operate on an arbitrary open `HKEY` root, so the same
//! code can act on `HKEY_CURRENT_USER\Software\Classes` (the per-user unregister)
//! and on a mounted `HKEY_USERS\<SID>_Classes` hive (the all-users uninstall).
//!
//! Nothing outside `#[cfg(test)]` calls into this module yet: later tasks in
//! the all-users-uninstall plan wire `windows.rs` and the new all-users path
//! to it. Until then, allow the otherwise-unused helpers.
#![allow(dead_code)]

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteTreeW, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY,
    KEY_ALL_ACCESS, KEY_READ, REG_LINK, REG_OPTION_OPEN_LINK, REG_VALUE_TYPE,
};

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
    let mut out = Vec::new();
    let mut index = 0u32;
    loop {
        let mut name = [0u16; 256];
        let mut len = name.len() as u32;
        let status = unsafe {
            RegEnumKeyExW(
                key,
                index,
                PWSTR(name.as_mut_ptr()),
                &mut len,
                None,
                PWSTR::null(),
                None,
                None,
            )
        };
        if status == ERROR_NO_MORE_ITEMS || status != ERROR_SUCCESS {
            break;
        }
        out.push(String::from_utf16_lossy(&name[..len as usize]));
        index += 1;
    }
    close(key);
    out
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
    let name = wide("SymbolicLinkValue");
    let mut kind = REG_VALUE_TYPE::default();
    let present =
        unsafe { RegQueryValueExW(h, PCWSTR(name.as_ptr()), None, Some(&mut kind), None, None) };
    close(h);
    present == ERROR_SUCCESS && kind == REG_LINK
}

/// Delete the subtree `root\subpath`. Returns `true` when the key existed and
/// was deleted, `false` when it was absent, a symbolic link (refused), or the
/// delete failed. Never follows a `REG_LINK`.
pub(super) fn delete_tree_under(root: HKEY, subpath: &str) -> bool {
    if is_reg_link(root, subpath) {
        return false;
    }
    if open_subkey(root, subpath).map(close).is_none() {
        return false; // absent
    }
    let w = wide(subpath);
    let status = unsafe { RegDeleteTreeW(root, PCWSTR(w.as_ptr())) };
    status == ERROR_SUCCESS
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
    use windows::Win32::System::Registry::{
        RegCreateKeyExW, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE,
    };

    /// A unique scratch key under HKCU that this test creates and removes.
    fn scratch() -> String {
        format!(r"Software\Kuvatin-regutil-test-{}", std::process::id())
    }

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

    #[test]
    fn enumerates_and_deletes_subkeys() {
        let base = scratch();
        create(&format!(r"{base}\alpha"));
        create(&format!(r"{base}\beta"));
        let mut kids = enum_subkeys(HKEY_CURRENT_USER, &base);
        kids.sort();
        assert_eq!(kids, vec!["alpha".to_string(), "beta".to_string()]);
        assert!(delete_tree_under(
            HKEY_CURRENT_USER,
            &format!(r"{base}\alpha")
        ));
        assert!(!delete_tree_under(
            HKEY_CURRENT_USER,
            &format!(r"{base}\alpha")
        )); // gone now
        assert!(!is_reg_link(HKEY_CURRENT_USER, &format!(r"{base}\beta")));
        // cleanup
        delete_tree_under(HKEY_CURRENT_USER, &base);
    }
}
