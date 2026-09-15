//! The classic Explorer verb keys Kuvatin creates under a classes root
//! (`HKCU\Software\Classes` per user, or a mounted `HKEY_USERS\<SID>_Classes`).
//! This is the ONE list both the per-user `unregister()` and the all-users
//! uninstall delete, so they can never drift.
//!
//! Nothing outside `#[cfg(test)]` calls `subkeys_to_delete` yet: later tasks
//! in the all-users-uninstall plan wire it into `windows.rs` and the new
//! all-users path. Until then, allow the otherwise-unused helper.
#![allow(dead_code)]

use windows::Win32::System::Registry::HKEY;

/// The three command stores (`ExtendedSubCommandsKey` targets).
const STORES: &[&str] = &[
    "Kuvatin.CommandStore",
    "Kuvatin.CommandStore.Background",
    "Kuvatin.CommandStore.Frames",
];

/// The verb subkeys, relative to a classes root, that today's build writes.
/// Every extension the menu attaches to, plus the pre-schema-4 perceived-type
/// root (`image`), the folder and folder-background verbs, and the stores.
pub(super) fn classes_subkeys() -> Vec<String> {
    let mut keys: Vec<String> = super::windows::menu_extensions()
        .iter()
        .map(|e| format!(r"SystemFileAssociations\.{e}\shell\Kuvatin"))
        .collect();
    keys.push(r"SystemFileAssociations\image\shell\Kuvatin".to_string());
    keys.push(r"Directory\shell\Kuvatin".to_string());
    keys.push(r"Directory\Background\shell\Kuvatin".to_string());
    keys.extend(STORES.iter().map(|s| s.to_string()));
    keys
}

/// The subkeys to delete under `classes_root`: the static [`classes_subkeys`]
/// set, PLUS any `SystemFileAssociations\<assoc>\shell\Kuvatin` found by
/// enumeration (so a verb from an older schema, or an extension later dropped
/// from the list, is still cleaned). Deterministic order, de-duplicated.
///
/// `Err` when the enumeration this leans on could not be read in full —
/// `regutil::enum_subkeys` never hands back a short list silently, and
/// neither does this, so a caller logging the reason never mistakes a read
/// failure for "nothing more to delete".
pub(super) fn subkeys_to_delete(classes_root: HKEY) -> Result<Vec<String>, String> {
    let mut keys = classes_subkeys();
    for child in super::regutil::enum_subkeys(classes_root, "SystemFileAssociations")? {
        let candidate = format!(r"SystemFileAssociations\{child}\shell\Kuvatin");
        if super::regutil::open_subkey(classes_root, &candidate)
            .map(super::regutil::close)
            .is_some()
            && !keys.contains(&candidate)
        {
            keys.push(candidate);
        }
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_list_covers_every_piece() {
        let keys = classes_subkeys();
        assert!(keys.contains(&r"SystemFileAssociations\.png\shell\Kuvatin".to_string()));
        assert!(keys.contains(&r"SystemFileAssociations\.exr\shell\Kuvatin".to_string()));
        assert!(keys.contains(&r"SystemFileAssociations\image\shell\Kuvatin".to_string()));
        assert!(keys.contains(&r"Directory\shell\Kuvatin".to_string()));
        assert!(keys.contains(&r"Directory\Background\shell\Kuvatin".to_string()));
        assert!(keys.contains(&"Kuvatin.CommandStore".to_string()));
        assert!(keys.contains(&"Kuvatin.CommandStore.Background".to_string()));
        assert!(keys.contains(&"Kuvatin.CommandStore.Frames".to_string()));
        // One root per menu extension, plus image + 2 dirs + 3 stores.
        assert_eq!(
            keys.len(),
            super::super::windows::menu_extensions().len() + 6
        );
    }
}
