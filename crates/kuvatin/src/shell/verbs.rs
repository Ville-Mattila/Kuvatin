//! The classic Explorer verb keys Kuvatin creates under a classes root
//! (`HKCU\Software\Classes` per user, or a mounted `HKEY_USERS\<SID>_Classes`).
//! This is the ONE list both the per-user `unregister()` and the all-users
//! uninstall delete, so they can never drift.
//!
//! Not one key is spelled out here. Registration writes absolute
//! `HKCU\Software\Classes\…` paths and this module names the same keys
//! relative to whichever hive it is handed, so both lists are derived from the
//! constants in `super::windows` by taking that prefix off — a verb moved or a
//! store renamed there travels straight through to the uninstall, and
//! `the_shared_list_is_the_per_user_key_set` fails the build if it ever stops
//! doing so.

use windows::Win32::System::Registry::HKEY;

/// The same key one hive down: `Software\Classes\Directory\shell\Kuvatin`
/// becomes `Directory\shell\Kuvatin`, which is how it is named inside a
/// mounted `HKEY_USERS\<SID>_Classes`.
///
/// A constant that does not live under the classes root is a mistake in
/// `super::windows` rather than anything a hive could cause, so it stops the
/// build's tests here instead of quietly dropping a key from the uninstall.
fn under_classes(absolute: &str) -> String {
    let root = super::windows::CLASSES_ROOT;
    absolute
        .strip_prefix(root)
        .and_then(|rest| rest.strip_prefix('\\'))
        .unwrap_or_else(|| panic!("{absolute} is not a key under {root}"))
        .to_string()
}

/// The verb subkeys, relative to a classes root, that today's build writes.
/// Every extension the menu attaches to and the folder and folder-background
/// verbs (all of `windows::classic_roots`), plus the pre-schema-4
/// perceived-type root (`image`) and the command stores.
pub(super) fn classes_subkeys() -> Vec<String> {
    let mut keys: Vec<String> = super::windows::classic_roots()
        .iter()
        .map(|root| under_classes(root))
        .collect();
    keys.push(under_classes(super::windows::LEGACY_ROOT));
    keys.extend(super::windows::STORES.iter().map(|s| s.to_string()));
    keys
}

/// The subkeys to delete under `classes_root`: the static [`classes_subkeys`]
/// set, PLUS any `SystemFileAssociations\<assoc>\shell\Kuvatin` found by
/// enumeration (so a verb from an older schema, or an extension later dropped
/// from the list, is still cleaned). Deterministic order, de-duplicated
/// case-insensitively, as registry names are.
///
/// The static list comes back whatever happens; the `Option` says why the
/// enumeration beside it could not be read in full. Dropping the list on such
/// a failure would be the worst of both worlds — a hive we could not finish
/// reading is exactly one we should still delete the known keys from — so a
/// caller deletes everything in the `Vec` and logs the reason, which
/// `regutil::enum_subkeys` has already put into words.
pub(super) fn subkeys_to_delete(classes_root: HKEY) -> (Vec<String>, Option<String>) {
    let mut keys = classes_subkeys();
    let (children, trouble) =
        match super::regutil::enum_subkeys(classes_root, "SystemFileAssociations") {
            Ok(children) => (children, None),
            Err(why) => (Vec::new(), Some(why)),
        };
    for child in children {
        let candidate = format!(r"SystemFileAssociations\{child}\shell\Kuvatin");
        if let Some(found) = super::regutil::open_subkey(classes_root, &candidate) {
            super::regutil::close(found);
            if !keys.iter().any(|k| k.eq_ignore_ascii_case(&candidate)) {
                keys.push(candidate);
            }
        }
    }
    (keys, trouble)
}

#[cfg(test)]
mod tests {
    use super::super::regutil::{
        close, delete_tree_under, open_path_no_links, wide, DeleteOutcome,
    };
    use super::super::windows::{
        extension_roots, BACKGROUND_ROOT, CLASSES_ROOT, FOLDER_ROOT, LEGACY_ROOT, STORE_BACKGROUND,
        STORE_FRAMES, STORE_ITEM,
    };
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCreateKeyExW, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE,
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

    /// A scratch key under HKCU standing in for a classes root, unique to this
    /// run and removed when the test ends — pass, fail or panic. Planting a
    /// stray verb here instead of under the real `Software\Classes` keeps a
    /// test run out of the developer's own Explorer menu.
    struct Scratch {
        path: String,
        root: HKEY,
    }

    impl Scratch {
        fn new() -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = format!(
                r"Software\Kuvatin-verbs-test-{}-{nanos}",
                std::process::id()
            );
            create(&path);
            let root = open_path_no_links(HKEY_CURRENT_USER, &path, KEY_READ)
                .unwrap_or_else(|why| panic!("open {path}: {why}"));
            Scratch { path, root }
        }

        /// `Software\Kuvatin-verbs-test-…\<rest>`, as `create` takes it.
        fn at(&self, rest: &str) -> String {
            format!(r"{}\{rest}", self.path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            close(self.root);
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

    /// The one list, against the key set spelled out the way the per-user
    /// `unregister()` used to spell it. Rename a store, move a verb root or
    /// drop one from [`classes_subkeys`] and this fails here rather than
    /// leaving that key behind on every machine that uninstalls.
    #[test]
    fn the_shared_list_is_the_per_user_key_set() {
        let mut absolute: Vec<String> = extension_roots().into_iter().map(|(r, _)| r).collect();
        for path in [LEGACY_ROOT, FOLDER_ROOT, BACKGROUND_ROOT] {
            absolute.push(path.to_string());
        }
        for store in [STORE_ITEM, STORE_BACKGROUND, STORE_FRAMES] {
            absolute.push(format!(r"{CLASSES_ROOT}\{store}"));
        }
        let prefix = format!(r"{CLASSES_ROOT}\");
        let mut expected: Vec<String> = absolute
            .iter()
            .map(|p| {
                p.strip_prefix(&prefix)
                    .unwrap_or_else(|| panic!("{p} is not under {prefix}"))
                    .to_string()
            })
            .collect();
        expected.sort();
        let mut got = classes_subkeys();
        got.sort();
        assert_eq!(got, expected);
    }

    #[test]
    fn a_stray_verb_key_is_found_and_listed_once() {
        let scratch = Scratch::new();
        let stray = r"SystemFileAssociations\.qoi\shell\Kuvatin";
        create(&scratch.at(stray));
        // Another app's verb on another extension is not ours to delete.
        create(&scratch.at(r"SystemFileAssociations\.tga\shell\OtherApp"));

        let (keys, trouble) = subkeys_to_delete(scratch.root);
        assert_eq!(trouble, None, "a readable hive has nothing to report");
        assert_eq!(
            keys.iter().filter(|k| k.as_str() == stray).count(),
            1,
            "the stray key, exactly once: {keys:?}"
        );
        assert!(
            !keys.iter().any(|k| k.contains(".tga")),
            "someone else's verb must be left alone: {keys:?}"
        );
        for key in classes_subkeys() {
            assert!(keys.contains(&key), "{key} went missing from the list");
        }
    }

    #[test]
    fn a_hive_with_no_file_associations_yields_the_static_list() {
        let scratch = Scratch::new();

        let (keys, trouble) = subkeys_to_delete(scratch.root);
        assert_eq!(trouble, None, "an absent key is not a read failure");
        assert_eq!(keys, classes_subkeys());
    }

    #[test]
    fn an_extension_spelled_in_another_case_is_not_listed_twice() {
        let scratch = Scratch::new();
        // Registry names are case-insensitive, so another app may perfectly
        // well have created `.PNG`; listing it beside our own `.png` would
        // have us delete the same key twice.
        create(&scratch.at(r"SystemFileAssociations\.PNG\shell\Kuvatin"));

        let (keys, trouble) = subkeys_to_delete(scratch.root);
        assert_eq!(trouble, None);
        let png = keys
            .iter()
            .filter(|k| k.eq_ignore_ascii_case(r"SystemFileAssociations\.png\shell\Kuvatin"))
            .count();
        assert_eq!(png, 1, "one .png entry, whatever its case: {keys:?}");
    }

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
