//! Which of a profile's Kuvatin files and folders the uninstall deletes, and
//! which it keeps. Presets and settings (`AppData\Roaming\Kuvatin`) stay; logs,
//! `%TEMP%\kuvatin` and the sparse package's data folder go. The whole
//! `AppData\Local\Kuvatin` folder is NEVER deleted outright — it can hold the
//! user's signing keys — only named files inside it, and then the folder itself
//! only when it ends up empty.
//!
//! **Everything here is a name, and this module vets none of them.** A path in
//! a [`FilePlan`] is the profile root with fixed names joined onto it; nothing
//! here opens one or asks what it really is. Below the profile root every
//! directory belongs to the account, and a directory junction needs no
//! privilege at all, so its owner can aim `AppData`, `Local`, `Temp`,
//! `Kuvatin`, `Packages` — or a `VilleMattila.Kuvatin_*` entry of their own
//! making — anywhere on the machine and wait for the SYSTEM uninstall to come
//! and delete through it, and can do it *while* the uninstall is running.
//! Turning these names into deletions is [`super::files`]'s job, and it is
//! harder than it looks: a directory can be turned into a junction in place,
//! without being renamed or deleted, so no share mode holds a name still and
//! nothing checked by name stays checked. What that module does instead is
//! walk down from the profile directory `profiles::vet_dir` already vetted,
//! refusing a reparse point at every component — the components of `files` as
//! much as those of `trees` — then hold the leaf itself so its parent can no
//! longer be emptied, then look at every ancestor *again* through the handles
//! it has held all along, and only then delete, through a handle rather than
//! through a path wherever it can. Nothing may take a path from here and hand
//! it straight to a delete.
//!
//! [`package_data_dirs`] is the one thing here that reads the disk, and it
//! reads it by name like everything else: a junction at `AppData` would have it
//! list somebody else's folder. That is why what it finds is a *candidate* and
//! not a target — the walk in `super::files` starts at the profile root and
//! stops at that junction, so the names it brought back are never reached.
//!
//! [`is_protected`] is belt and braces over paths *this* module minted, not a
//! sanitizer: it is what keeps `Local\Kuvatin` and `Roaming\Kuvatin` out of a
//! recursive delete if the plan above is ever edited carelessly. It compares
//! names, so it says nothing worth having about a path that came from anywhere
//! else.
//!
//! `%TEMP%` is taken to be `AppData\Local\Temp`, which is where Windows puts it
//! and where it stays unless somebody moves it. An account that has redirected
//! its TEMP somewhere else keeps that cache, by decision: finding it would mean
//! reading that account's own environment out of its hive, and a wrong answer
//! there is a directory this uninstall would then delete as SYSTEM.
//!
//! The caller is `super::allusers`, the `--unregister-all-users` entry point:
//! it plans each account's files here and hands the plan to [`super::files`].

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The family-name prefix of the sparse package's per-user data folder under
/// `AppData\Local\Packages`. The suffix is a Publisher hash Windows computes,
/// so we match by prefix.
///
/// Spelled out rather than built from [`super::package::PACKAGE_NAME`] because
/// a `const` cannot be `format!`ed; `the_prefix_is_the_package_name_and_its_separator`
/// ties the two together instead, so renaming the package fails a test rather
/// than quietly stopping the uninstall from finding the folder.
pub(super) const PACKAGE_DATA_PREFIX: &str = "VilleMattila.Kuvatin_";

/// What to remove in one profile.
#[derive(Debug)]
pub(super) struct FilePlan {
    /// Individual files to delete.
    pub files: Vec<PathBuf>,
    /// Directory trees to delete (junction-safe walk).
    pub trees: Vec<PathBuf>,
    /// Directories to remove ONLY if they are empty afterwards.
    pub prune_if_empty: Vec<PathBuf>,
}

/// Build the plan for a profile directory. Pure — it touches no disk — so the
/// caller passes in the package data folders it globbed.
pub(super) fn plan(profile: &Path, package_data_dirs: &[PathBuf]) -> FilePlan {
    let local = profile.join("AppData").join("Local");
    let kuvatin_local = local.join("Kuvatin");
    let temp_kuvatin = local.join("Temp").join("kuvatin");

    let mut plan = FilePlan {
        files: vec![
            kuvatin_local.join("kuvatin.log"),
            kuvatin_local.join("kuvatin.log.1"),
            kuvatin_local.join("crash.log"),
        ],
        // The whole %TEMP%\kuvatin tree: rendezvous and seq-cache both live there.
        trees: vec![temp_kuvatin],
        prune_if_empty: vec![kuvatin_local],
    };
    plan.trees.extend(package_data_dirs.iter().cloned());
    plan
}

/// The `AppData\Local\Packages\VilleMattila.Kuvatin_*` folders in one profile
/// (usually zero or one), plus whatever went wrong on the way.
///
/// Same shape as `verbs::subkeys_to_delete` and for the same reason: an
/// account whose `Packages` folder we could not read is an account whose
/// package data this uninstall will not find, and reporting that as "there is
/// none" would have the uninstall call it clean. Only a `Packages` folder that
/// is genuinely *not there* is an empty list with nothing to say — which is
/// every account that has never run a packaged app.
///
/// Directories only. `DirEntry::file_type` on Windows answers out of the
/// directory listing without opening anything, and it calls a junction a
/// symbolic link rather than a directory — so a `VilleMattila.Kuvatin_*`
/// junction planted here is not returned at all, and this hands out one fewer
/// name for the walk in `super::files` to refuse. That is a convenience, not
/// the defence: see this module's documentation for where the defence is.
pub(super) fn package_data_dirs(profile: &Path) -> (Vec<PathBuf>, Vec<String>) {
    let packages = profile.join("AppData").join("Local").join("Packages");
    let rd = match std::fs::read_dir(&packages) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Vec::new(), Vec::new()),
        Err(e) => {
            return (
                Vec::new(),
                vec![format!("{} would not be read ({e})", packages.display())],
            )
        }
    };
    let mut found = Vec::new();
    let mut trouble = Vec::new();
    for entry in rd {
        let entry = match entry {
            Ok(entry) => entry,
            // One unreadable entry does not say which name it was, so name the
            // folder it was in — it is still the difference between a complete
            // list and a short one passed off as complete.
            Err(e) => {
                trouble.push(format!("{} did not list in full ({e})", packages.display()));
                continue;
            }
        };
        if !is_package_data_name(&entry.file_name()) {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => found.push(entry.path()),
            // A name of ours that is not a folder is not package data, and
            // saying so is cheaper than the walk refusing it later.
            Ok(_) => {}
            Err(e) => trouble.push(format!(
                "{} would not say what it is ({e})",
                entry.path().display()
            )),
        }
    }
    (found, trouble)
}

/// Whether a `Packages` entry carries our package's family-name prefix,
/// compared without regard to case the way [`is_protected`] and the file system
/// itself compare names. Windows writes the folder in the manifest's spelling,
/// but nothing stops the account from making one of its own in another case,
/// and a case-sensitive test would walk past it and leave it behind.
fn is_package_data_name(name: &OsStr) -> bool {
    name.to_string_lossy()
        .get(..PACKAGE_DATA_PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(PACKAGE_DATA_PREFIX))
}

/// A guard used before any recursive delete: refuse a Roaming Kuvatin folder
/// (presets/settings) or the bare `AppData\Local\Kuvatin` folder.
pub(super) fn is_protected(path: &Path) -> bool {
    let ends_with = |p: &Path, parent: &str, leaf: &str| {
        let mut it = p.components().rev();
        it.next()
            .map(|c| c.as_os_str().eq_ignore_ascii_case(leaf))
            .unwrap_or(false)
            && it
                .next()
                .map(|c| c.as_os_str().eq_ignore_ascii_case(parent))
                .unwrap_or(false)
    };
    ends_with(path, "Roaming", "Kuvatin") || ends_with(path, "Local", "Kuvatin")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_deletes_logs_and_temp_keeps_presets() {
        let profile = PathBuf::from(r"C:\Users\alice");
        let pkg = vec![PathBuf::from(
            r"C:\Users\alice\AppData\Local\Packages\VilleMattila.Kuvatin_5jce0xfqz5w2a",
        )];
        let p = plan(&profile, &pkg);

        assert!(p.files.contains(&PathBuf::from(
            r"C:\Users\alice\AppData\Local\Kuvatin\kuvatin.log"
        )));
        assert!(p.files.contains(&PathBuf::from(
            r"C:\Users\alice\AppData\Local\Kuvatin\kuvatin.log.1"
        )));
        assert!(p.files.contains(&PathBuf::from(
            r"C:\Users\alice\AppData\Local\Kuvatin\crash.log"
        )));
        assert!(p
            .trees
            .contains(&PathBuf::from(r"C:\Users\alice\AppData\Local\Temp\kuvatin")));
        assert!(p.trees.contains(&pkg[0]));

        // Presets and settings are never touched.
        assert!(!p.files.contains(&PathBuf::from(
            r"C:\Users\alice\AppData\Roaming\Kuvatin\presets.toml"
        )));
        assert!(!p
            .trees
            .contains(&PathBuf::from(r"C:\Users\alice\AppData\Roaming\Kuvatin")));
    }

    #[test]
    fn never_deletes_the_whole_local_kuvatin_folder() {
        let profile = PathBuf::from(r"C:\Users\alice");
        let p = plan(&profile, &[]);
        let local_kuvatin = PathBuf::from(r"C:\Users\alice\AppData\Local\Kuvatin");
        assert!(
            !p.trees.contains(&local_kuvatin),
            "Local\\Kuvatin must never be tree-deleted (it can hold signing keys)"
        );
        assert!(p.prune_if_empty.contains(&local_kuvatin));
    }

    #[test]
    fn protected_guard_rejects_data_folders() {
        assert!(is_protected(Path::new(
            r"C:\Users\alice\AppData\Roaming\Kuvatin"
        )));
        assert!(is_protected(Path::new(
            r"C:\Users\alice\AppData\Local\Kuvatin"
        )));
        assert!(!is_protected(Path::new(
            r"C:\Users\alice\AppData\Local\Temp\kuvatin"
        )));
        assert!(!is_protected(Path::new(
            r"C:\Users\alice\AppData\Local\Packages\VilleMattila.Kuvatin_x"
        )));
    }

    #[test]
    fn package_folder_names_match_by_prefix() {
        assert!("VilleMattila.Kuvatin_5jce0xfqz5w2a".starts_with(PACKAGE_DATA_PREFIX));
        assert!(!"Microsoft.WindowsStore_8wekyb3d8bbwe".starts_with(PACKAGE_DATA_PREFIX));
    }

    /// The prefix is the package's own identity plus the separator Windows puts
    /// before the publisher hash — not a string that merely looks like it. If
    /// the package is ever renamed, this is what says so out loud instead of
    /// letting the uninstall quietly stop finding the data folder.
    #[test]
    fn the_prefix_is_the_package_name_and_its_separator() {
        assert_eq!(
            PACKAGE_DATA_PREFIX,
            format!("{}_", crate::shell::package::PACKAGE_NAME),
            "the data folder is named <package>_<publisher hash>"
        );
    }

    /// Build a `Packages` directory with the given entries and hand back the
    /// profile root and the `Packages` path.
    fn profile_with_packages(dir: &tempfile::TempDir, names: &[&str]) -> (PathBuf, PathBuf) {
        let profile = dir.path().to_path_buf();
        let packages = profile.join("AppData").join("Local").join("Packages");
        std::fs::create_dir_all(&packages).expect("the Packages directory");
        for name in names {
            std::fs::create_dir(packages.join(name)).expect("a package data folder");
        }
        (profile, packages)
    }

    /// Reads a real directory, because what this has to get right — which
    /// entries are ours — is a property of the names on disk, not of a string
    /// literal. The near-miss is the point: `VilleMattila.KuvatinPro` shares
    /// every character of the package name and is a different package, and the
    /// separator is the only thing that keeps its data folder out of a
    /// recursive delete run as SYSTEM.
    #[test]
    fn only_our_own_package_data_folder_is_ours_to_delete() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let (profile, packages) = profile_with_packages(
            &dir,
            &[
                "VilleMattila.Kuvatin_5jce0xfqz5w2a",
                "VilleMattila.KuvatinPro_5jce0xfqz5w2a",
                "Microsoft.WindowsStore_8wekyb3d8bbwe",
            ],
        );

        assert_eq!(
            package_data_dirs(&profile),
            (
                vec![packages.join("VilleMattila.Kuvatin_5jce0xfqz5w2a")],
                Vec::new()
            )
        );
    }

    /// An account that never ran a packaged app has no `Packages` directory at
    /// all, which is nothing to report and nothing to delete.
    #[test]
    fn a_profile_without_packages_yields_nothing() {
        let dir = tempfile::tempdir().expect("a temp directory");
        assert_eq!(
            package_data_dirs(dir.path()),
            (Vec::new(), Vec::new()),
            "no Packages folder at all is nothing to report"
        );
    }

    /// Matched without regard to case, like [`is_protected`] and like the file
    /// system itself. Windows writes the name in the manifest's spelling, but
    /// nothing stops the account from making a folder of its own in another
    /// case, and a case-sensitive check would walk past it and leave it behind.
    #[test]
    fn the_package_folder_matches_whatever_case_it_is_spelled_in() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let (profile, packages) =
            profile_with_packages(&dir, &["villemattila.kuvatin_5JCE0XFQZ5W2A"]);

        assert_eq!(
            package_data_dirs(&profile),
            (
                vec![packages.join("villemattila.kuvatin_5JCE0XFQZ5W2A")],
                Vec::new()
            )
        );
    }

    /// Only directories come back. A plain file with the right name is not a
    /// package data folder, and the tree walk it would be handed to deletes
    /// directories — so the entry is not ours and saying so here is cheaper
    /// than an error later.
    #[test]
    fn a_file_named_like_the_package_folder_is_not_one() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let (profile, packages) = profile_with_packages(&dir, &[]);
        std::fs::write(
            packages.join("VilleMattila.Kuvatin_5jce0xfqz5w2a"),
            b"not a folder",
        )
        .expect("a file where a folder would be");

        assert_eq!(package_data_dirs(&profile), (Vec::new(), Vec::new()));
    }

    /// A `Packages` folder that is there and will not read is reported, never
    /// handed back as "there is nothing of ours here" — that difference is the
    /// difference between an account this uninstall cleaned and one it only
    /// believed it had.
    ///
    /// Provoked with a file standing where the folder should be, which fails
    /// the read with `ERROR_DIRECTORY` and needs no privilege to arrange, so
    /// this test never skips. A Deny ACE on a real `Packages` folder — the case
    /// this is really about — comes back through the same arm.
    #[test]
    fn a_packages_folder_that_will_not_read_is_reported_not_swallowed() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let profile = dir.path().to_path_buf();
        let local = profile.join("AppData").join("Local");
        std::fs::create_dir_all(&local).expect("the AppData\\Local tree");
        std::fs::write(local.join("Packages"), b"not a folder").expect("a file in its place");

        let (found, trouble) = package_data_dirs(&profile);
        assert_eq!(found, Vec::<PathBuf>::new());
        assert_eq!(trouble.len(), 1, "{trouble:?}");
        assert!(
            trouble[0].contains("Packages"),
            "the folder we could not read should be named: {}",
            trouble[0]
        );
    }
}
