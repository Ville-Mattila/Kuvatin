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
/// mounted `HKEY_USERS\<SID>_Classes`. `None` for a path that is not under the
/// classes root at all.
///
/// That can only be a mistake in `super::windows`, and it is caught where
/// mistakes should be: a `debug_assert` for whoever made it, and the drift
/// test, which counts the list exactly and so fails on a key silently
/// dropped. What it must not do is panic in a release build — this runs from
/// `unregister()`, which the installer calls from a custom action marked
/// `Return='ignore'`, so a panic there would take the whole menu removal down
/// without a word to anyone.
fn under_classes(absolute: &str) -> Option<String> {
    let root = super::windows::CLASSES_ROOT;
    let relative = absolute
        .strip_prefix(root)
        .and_then(|rest| rest.strip_prefix('\\'));
    debug_assert!(
        relative.is_some(),
        "{absolute} is not a key under {root}; it will not be uninstalled"
    );
    relative.map(str::to_string)
}

/// The verb subkeys, relative to a classes root, that today's build writes.
/// Every extension the menu attaches to and the folder and folder-background
/// verbs (all of `windows::classic_roots`), plus the pre-schema-4
/// perceived-type root (`image`) and the command stores.
pub(super) fn classes_subkeys() -> Vec<String> {
    let mut keys: Vec<String> = super::windows::classic_roots()
        .iter()
        .filter_map(|root| under_classes(root))
        .collect();
    keys.extend(under_classes(super::windows::LEGACY_ROOT));
    keys.extend(super::windows::STORES.iter().map(|s| s.to_string()));
    keys
}

/// The subkeys to delete under `classes_root`: the static [`classes_subkeys`]
/// set, PLUS any `SystemFileAssociations\<assoc>\shell\Kuvatin` found by
/// enumeration (so a verb from an older schema, or an extension later dropped
/// from the list, is still cleaned). Deterministic order, de-duplicated
/// ASCII-case-insensitively — registry names are compared case-insensitively,
/// and every name we put in the list is ASCII, so a `.PNG` someone else
/// created is our own `.png` key and is deleted once.
///
/// The static list comes back whatever happens; the second `Vec` says, in
/// words, everything that could not be read in full — the enumeration itself,
/// and any one candidate that is there and would not open. Dropping the list on
/// such a failure would be the worst of both worlds — a hive we could not
/// finish reading is exactly one we should still delete the known keys from —
/// so a caller deletes everything in the first `Vec` and reports the second.
///
/// A candidate that refuses to open is *kept* on the list as well as reported.
/// Reading a refusal as absence is how a stray verb gets left behind in
/// silence, and the delete is worth attempting anyway: it asks for different
/// rights than the read did, so it may well go — and if it does not, it says so
/// as a refusal naming the key, which is more use than the bare read failure.
pub(super) fn subkeys_to_delete(classes_root: HKEY) -> (Vec<String>, Vec<String>) {
    let mut keys = classes_subkeys();
    let mut troubles = Vec::new();
    let children = match super::regutil::enum_subkeys(classes_root, "SystemFileAssociations") {
        Ok(children) => children,
        Err(why) => {
            troubles.push(why);
            Vec::new()
        }
    };
    for child in children {
        let candidate = format!(r"SystemFileAssociations\{child}\shell\Kuvatin");
        // Opened only to ask whether it is there at all; the handle closes
        // itself at the end of the match.
        let here = match super::regutil::open_owned_reporting(classes_root, &candidate) {
            super::regutil::Found::Key(_) => true,
            super::regutil::Found::Absent => false,
            super::regutil::Found::Refused(why) => {
                troubles.push(why);
                true
            }
        };
        if here && !keys.iter().any(|k| k.eq_ignore_ascii_case(&candidate)) {
            keys.push(candidate);
        }
    }
    (keys, troubles)
}

/// One line the sweep produced, kept apart by what it means rather than by how
/// it happens to read. Every kind names a key relative to the classes root, so
/// a caller that just logs them puts the same hive in front of all three; what
/// telling them apart is for is the all-users uninstall, which reports the
/// enumeration trouble and the refusals in quite different places.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SweepLine {
    /// The list of keys may be short: the `SystemFileAssociations` enumeration
    /// could not be read in full. Comes first when it comes at all.
    Trouble(String),
    /// Something the deletion met and dealt with — a symbolic link planted
    /// inside a subtree, say.
    Note(String),
    /// Why one key tree would not go. The key is still there.
    Refused(String),
}

/// What one pass of [`remove_verbs_under`] came to.
///
/// Removed, absent and refused are counted apart because they mean different
/// things: keys removed is the menu coming off, keys already gone is an
/// ordinary second uninstall, and keys refused is the menu still on the
/// machine. `lines` holds everything worth saying in the order it happened, so
/// one caller can log it live and another can hand it to whoever prints.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct VerbSweep {
    /// Key trees that were there and are gone.
    pub removed: usize,
    /// Key trees that were not there to begin with.
    pub absent: usize,
    /// Key trees still wholly or partly there.
    pub refused: usize,
    /// Trouble, notes and refusals, in the order they happened.
    pub lines: Vec<SweepLine>,
}

// The per-user unregister walks `lines` itself; these are for the all-users
// uninstall, whose orchestrator is a later task in that plan.
#[allow(dead_code)]
impl VerbSweep {
    /// Every reason the key list may be short. Empty on a hive that read in
    /// full, which is every hive nobody has locked anything in.
    pub(super) fn troubles(&self) -> Vec<&str> {
        self.pick(|line| matches!(line, SweepLine::Trouble(_)))
    }

    /// Anything the deletion met and dealt with along the way.
    pub(super) fn notes(&self) -> Vec<&str> {
        self.pick(|line| matches!(line, SweepLine::Note(_)))
    }

    /// Why each key tree that is still there would not go.
    pub(super) fn refusals(&self) -> Vec<&str> {
        self.pick(|line| matches!(line, SweepLine::Refused(_)))
    }

    fn pick(&self, want: impl Fn(&SweepLine) -> bool) -> Vec<&str> {
        self.lines
            .iter()
            .filter(|line| want(line))
            .map(|line| match line {
                SweepLine::Trouble(s) | SweepLine::Note(s) | SweepLine::Refused(s) => s.as_str(),
            })
            .collect()
    }

    /// The one obstacle behind several refusals, with how many it accounts
    /// for — `None` when every refusal is its own.
    ///
    /// A single planted link or locked key at `SystemFileAssociations` refuses
    /// all twelve verb keys below it, in the same words every time, because the
    /// walk stops at that segment before it ever reaches the leaf. Whoever
    /// reads the uninstall output needs the one key that has to be dealt with,
    /// said once and loudly, not twelve lines that repeat it.
    pub(super) fn shared_obstacle(&self) -> Option<(&str, usize)> {
        let mut counted: Vec<(&str, usize)> = Vec::new();
        for why in self.refusals() {
            match counted.iter_mut().find(|(seen, _)| *seen == why) {
                Some((_, n)) => *n += 1,
                None => counted.push((why, 1)),
            }
        }
        // First past the post on a tie, so the reason stays put between runs.
        counted.into_iter().filter(|(_, n)| *n > 1).fold(
            None,
            |best: Option<(&str, usize)>, here| match best {
                Some((_, n)) if n >= here.1 => best,
                _ => Some(here),
            },
        )
    }

    /// The refusals [`Self::shared_obstacle`] does not account for: what is
    /// still to be dealt with once that one key has been named. With no shared
    /// obstacle this is simply every refusal, so a caller can print the
    /// obstacle (when there is one) and then this, and have said each thing
    /// exactly once.
    pub(super) fn other_refusals(&self) -> Vec<&str> {
        match self.shared_obstacle() {
            Some((shared, _)) => self
                .refusals()
                .into_iter()
                .filter(|why| *why != shared)
                .collect(),
            None => self.refusals(),
        }
    }
}

/// Delete every Kuvatin verb key under an already-open classes root — the one
/// loop the per-user unregister and the all-users uninstall both run, so the
/// two can never drift in what they delete or in what they make of the answer.
///
/// Works relative to the handle it is given and never walks a path down to it:
/// `HKCU\Software\Classes` is a registry symbolic link and `regutil` refuses to
/// step through one, so a path-walking delete would refuse every verb key and
/// leave the whole menu in place. The handle needs no more than
/// `regutil::TRAVERSE_ACCESS`.
///
/// Reports rather than prints. `windows.rs` logs the lines as they were
/// gathered; the all-users path must not log at all — running as SYSTEM, a log
/// call would create a brand-new leftover under the system profile — so it
/// carries the result back to whoever prints.
pub(super) fn remove_verbs_under(classes_root: HKEY) -> VerbSweep {
    let mut sweep = VerbSweep::default();
    let (keys, troubles) = subkeys_to_delete(classes_root);
    for why in troubles {
        sweep.lines.push(SweepLine::Trouble(why));
    }
    for sub in keys {
        let outcome = super::regutil::delete_tree_under(classes_root, &sub);
        for note in outcome.notes() {
            sweep.lines.push(SweepLine::Note(note.clone()));
        }
        match outcome {
            super::regutil::DeleteOutcome::Deleted { .. } => sweep.removed += 1,
            super::regutil::DeleteOutcome::Absent => sweep.absent += 1,
            super::regutil::DeleteOutcome::Refused { why, .. } => {
                sweep.refused += 1;
                sweep.lines.push(SweepLine::Refused(why));
            }
        }
    }
    sweep
}

#[cfg(test)]
mod tests {
    use super::super::regutil::{
        delete_tree_under, open_owned, wide, DeleteOutcome, OwnedKey, DELETE_RIGHT,
    };
    use super::super::test_support::{skip_or_fail_on_ci, Denied};
    use super::super::windows::{
        extension_roots, BACKGROUND_ROOT, CLASSES_ROOT, FOLDER_ROOT, LEGACY_ROOT, STORE_BACKGROUND,
        STORE_FRAMES, STORE_ITEM,
    };
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCreateKeyExW, HKEY_CURRENT_USER, KEY_ENUMERATE_SUB_KEYS, KEY_QUERY_VALUE, KEY_WRITE,
        REG_OPTION_NON_VOLATILE, REG_SAM_FLAGS,
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
        drop(OwnedKey::own(h));
    }

    /// A scratch key under HKCU standing in for a classes root, unique to this
    /// run and removed when the test ends — pass, fail or panic. Planting a
    /// stray verb here instead of under the real `Software\Classes` keeps a
    /// test run out of the developer's own Explorer menu.
    struct Scratch {
        path: String,
        root: OwnedKey,
    }

    impl Scratch {
        fn new() -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            // The clock ticks every 100 ns here, which two tests starting
            // together can share; the counter is what keeps them apart.
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = format!(
                r"Software\Kuvatin-verbs-test-{}-{nanos}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            create(&path);
            // Opened the way `windows.rs` opens the real classes root.
            let root =
                open_owned(HKEY_CURRENT_USER, &path).unwrap_or_else(|| panic!("open {path}"));
            Scratch { path, root }
        }

        /// `Software\Kuvatin-verbs-test-…\<rest>`, as `create` takes it.
        fn at(&self, rest: &str) -> String {
            format!(r"{}\{rest}", self.path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
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

        let (keys, trouble) = subkeys_to_delete(scratch.root.get());
        assert!(trouble.is_empty(), "a readable hive has nothing to report");
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

        let (keys, trouble) = subkeys_to_delete(scratch.root.get());
        assert!(trouble.is_empty(), "an absent key is not a read failure");
        assert_eq!(keys, classes_subkeys());
    }

    /// The whole reason this returns a list *and* a reason rather than one or
    /// the other: a hive whose owner has denied us the read is exactly the one
    /// we must still delete the keys we already know about from. Giving up
    /// would leave the menu on that account for good.
    #[test]
    fn a_hive_we_cannot_read_still_lists_the_keys_we_know() {
        let scratch = Scratch::new();
        create(&scratch.at(r"SystemFileAssociations\.qoi\shell\Kuvatin"));
        let gate = scratch.at("SystemFileAssociations");

        // The right the enumeration actually asks for — `KEY_NOTIFY` is not one
        // of them any more, so denying that would prove nothing. Setting a DACL
        // from the test process may not be possible everywhere; say so rather
        // than quietly proving nothing either way.
        let denied = match Denied::on(&gate, KEY_ENUMERATE_SUB_KEYS) {
            Ok(guard) => guard,
            Err(why) => {
                skip_or_fail_on_ci(&format!("could not set a Deny ACE on {gate}: {why}"));
                return;
            }
        };

        let (keys, trouble) = subkeys_to_delete(scratch.root.get());
        assert_eq!(
            keys,
            classes_subkeys(),
            "the static list must survive a hive we cannot enumerate"
        );
        let why = trouble.first().expect("a hive we cannot read must say so");
        assert!(why.contains("not allowed"), "vague reason: {why}");
        assert!(
            why.contains("SystemFileAssociations"),
            "a log line needs the key, got: {why}"
        );
        // Before the scratch cleanup, so it can delete the key again.
        drop(denied);
    }

    /// A stray verb whose key we may enumerate but not open. Reading that
    /// refusal as "not there" would keep the key off the list in silence, which
    /// is the one outcome an uninstall must never produce: the verb stays in
    /// that account's menu and nothing anywhere says so. It is listed — the
    /// delete asks for different rights and may well succeed — and reported.
    #[test]
    fn a_candidate_we_may_not_open_is_listed_and_reported() {
        let scratch = Scratch::new();
        let stray = r"SystemFileAssociations\.qoi\shell\Kuvatin";
        create(&scratch.at(stray));
        let gate = scratch.at(stray);

        let denied = match Denied::on(&gate, KEY_QUERY_VALUE) {
            Ok(guard) => guard,
            Err(why) => {
                skip_or_fail_on_ci(&format!("could not set a Deny ACE on {gate}: {why}"));
                return;
            }
        };

        let (keys, trouble) = subkeys_to_delete(scratch.root.get());
        assert!(
            keys.iter().any(|k| k == stray),
            "a key we could not open is still ours to try: {keys:?}"
        );
        let why = trouble
            .first()
            .expect("a candidate we may not open must say so");
        assert!(why.contains("not allowed"), "vague reason: {why}");
        assert!(why.contains(".qoi"), "a log line needs the key, got: {why}");
        // Before the scratch cleanup, so it can delete the key again.
        drop(denied);
    }

    /// The same stray verb, all the way through the sweep, with the delete
    /// refused as well as the read. What it must not come out as is `absent`:
    /// that is the count an operator reads as "there was nothing there", and a
    /// key still sitting in someone's menu must be `refused` instead, named, so
    /// somebody goes and deals with it.
    #[test]
    fn a_candidate_refused_at_both_ends_is_counted_refused_not_absent() {
        let scratch = Scratch::new();
        let stray = r"SystemFileAssociations\.qoi\shell\Kuvatin";
        create(&scratch.at(stray));
        let gate = scratch.at(stray);

        // Both rights the two halves need: the read asks for
        // KEY_ENUMERATE_SUB_KEYS, the delete for that and DELETE. Denying both
        // keeps this honest if either access constant is ever narrowed.
        let denied = match Denied::on(
            &gate,
            REG_SAM_FLAGS(KEY_ENUMERATE_SUB_KEYS.0 | DELETE_RIGHT.0),
        ) {
            Ok(guard) => guard,
            Err(why) => {
                skip_or_fail_on_ci(&format!("could not set a Deny ACE on {gate}: {why}"));
                return;
            }
        };

        let sweep = remove_verbs_under(scratch.root.get());
        assert_eq!(
            sweep.refused, 1,
            "the one key still there should be counted as still there: {:?}",
            sweep.lines
        );
        assert_eq!(sweep.removed, 0, "{:?}", sweep.lines);
        assert_eq!(
            sweep.absent,
            classes_subkeys().len(),
            "only the keys that really are not there: {:?}",
            sweep.lines
        );
        let read = sweep
            .troubles()
            .first()
            .copied()
            .expect("the refused read should be reported");
        assert!(read.contains(".qoi"), "{read}");
        let refusal = sweep
            .refusals()
            .first()
            .copied()
            .expect("the refused delete should be reported");
        assert!(refusal.contains(".qoi"), "{refusal}");
        assert!(refusal.contains("not allowed"), "vague reason: {refusal}");

        // Before the scratch cleanup, so it can delete the key again.
        drop(denied);
    }

    #[test]
    fn an_extension_spelled_in_another_case_is_not_listed_twice() {
        let scratch = Scratch::new();
        // Registry names are case-insensitive, so another app may perfectly
        // well have created `.PNG`; listing it beside our own `.png` would
        // have us delete the same key twice.
        create(&scratch.at(r"SystemFileAssociations\.PNG\shell\Kuvatin"));

        let (keys, trouble) = subkeys_to_delete(scratch.root.get());
        assert!(trouble.is_empty());
        let png = keys
            .iter()
            .filter(|k| k.eq_ignore_ascii_case(r"SystemFileAssociations\.png\shell\Kuvatin"))
            .count();
        assert_eq!(png, 1, "one .png entry, whatever its case: {keys:?}");
    }

    /// The one deletion loop both callers run, counted. Keys removed and keys
    /// that were never there are told apart because they mean different things:
    /// the first is the menu coming off, the second an ordinary second
    /// uninstall.
    #[test]
    fn the_shared_loop_counts_what_it_did() {
        let scratch = Scratch::new();
        create(&scratch.at(r"SystemFileAssociations\.png\shell\Kuvatin\command"));
        create(&scratch.at("Kuvatin.CommandStore"));

        let sweep = remove_verbs_under(scratch.root.get());
        assert!(sweep.troubles().is_empty());
        assert_eq!(sweep.removed, 2, "{:?}", sweep.lines);
        assert_eq!(sweep.refused, 0, "{:?}", sweep.lines);
        assert_eq!(
            sweep.absent,
            classes_subkeys().len() - 2,
            "everything else on the list was already gone"
        );
        assert!(sweep.notes().is_empty(), "{:?}", sweep.lines);
        assert!(sweep.shared_obstacle().is_none(), "nothing was refused");

        // Run again and it is all absence — nothing is counted twice.
        let again = remove_verbs_under(scratch.root.get());
        assert_eq!(again.removed, 0);
        assert_eq!(again.absent, classes_subkeys().len());
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
