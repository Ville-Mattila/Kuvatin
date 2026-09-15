//! Carry out one profile's [`FilePlan`] as SYSTEM, without ever deleting
//! through a reparse point.
//!
//! **What is trusted.** `profiles::vet_dir` has established that the profile
//! directory itself is a real, unredirected directory. Nothing below it is
//! vouched for by anybody. Every folder inside a profile belongs to that
//! account, a directory junction needs no privilege at all (`mklink /J`), and
//! the account can be signed in and working while the uninstall runs — so its
//! owner can aim `AppData`, `Local`, `Temp`, `Kuvatin`, `Packages`, or a
//! `VilleMattila.Kuvatin_*` entry of their own making, at anywhere on the
//! machine, and can do it between any two of our statements. A SYSTEM delete
//! that followed one of those is an arbitrary-delete primitive against the
//! whole machine. `super::paths` mints names and vets none of them; this is
//! where the names become deletions, so this is where the vetting is.
//!
//! **The walk.** Every path in the plan is walked down from the profile root
//! one component at a time — the components of `files` as much as those of
//! `trees`. Each component is opened with `FILE_FLAG_OPEN_REPARSE_POINT`, so
//! the handle is the entry itself and not whatever it points at, and the
//! handle's own attributes are read rather than the path's: `File::metadata`
//! on Windows is `GetFileInformationByHandle`, which cannot be aimed elsewhere
//! once the handle is open. A `FILE_ATTRIBUTE_REPARSE_POINT` on any component
//! above the leaf refuses the whole path, by name, before anything is deleted.
//!
//! **Why every ancestor stays open.** Looking at a name and then deleting
//! through it is a race the owner wins whenever they care to. So each directory
//! from the profile root down to the target's parent is *held open* until the
//! delete has finished, with a share mode of `FILE_SHARE_READ |
//! FILE_SHARE_WRITE` and deliberately not `FILE_SHARE_DELETE`. A directory with
//! such a handle on it cannot be renamed or deleted, so the name we vetted still
//! names the object we vetted when the delete re-resolves the path.
//! `a_held_directory_cannot_be_renamed_out_from_under_us` is the measurement.
//!
//! **The leaf.** That leaves the last name, which the owner can still swap
//! between our look and our delete — and which is why all three deletes used
//! here are ones that refuse to follow a reparse point found there.
//! `remove_file` is `DeleteFileW`, which deletes a symbolic link rather than
//! its target and simply fails on a directory. `remove_dir` is
//! `RemoveDirectoryW`, which removes a junction's entry and leaves its target
//! alone. `remove_dir_all` opens its root without following a link; the
//! evidence for that is quoted where it is called. So the worst a leaf swap can
//! do is unlink something the account planted, in the account's own folder.
//!
//! Two things this does not cover, said plainly rather than left to be assumed:
//! a reparse point *above* the profile (a junction at `C:\Users`) needs
//! administrator rights to create, and an attacker who has those has no need of
//! this; and a **hard link** at one of the `files` carries no reparse point, so
//! it passes every check here — `DeleteFileW` then unlinks that name and leaves
//! the file it shared, which is the harmless half of the same opening
//! `hive.rs` describes.
//!
//! This module **reports**; it never prints and never logs. It runs as SYSTEM,
//! where `crate::applog` would resolve `%LOCALAPPDATA%` to the system profile
//! and leave a brand-new file behind — exactly the sort of leftover the
//! all-users uninstall exists to remove. The orchestrator prints what comes
//! back here to stdout.
//!
//! Nothing outside `#[cfg(test)]` calls this yet: the caller is the
//! `--unregister-all-users` entry point, a later task in that plan.
#![allow(dead_code)]

use std::fs::File;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path};
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_DIR_NOT_EMPTY, ERROR_SHARING_VIOLATION,
};

use super::paths::{self, FilePlan};
use super::profiles::FILE_ATTRIBUTE_REPARSE_POINT;

/// The `CreateFileW` bits below are spelled out because the `windows` crate
/// exports them from `Win32_Storage_FileSystem`, a feature this build does not
/// otherwise need — and `std::fs::OpenOptions` reaches exactly the same
/// `CreateFileW` call without it (see [`open_no_follow`]), so the feature would
/// buy nothing but an `unsafe` block and a handle to close by hand.
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
/// What the walk asks for, and it has to be this much.
///
/// `FILE_READ_ATTRIBUTES` alone is all `GetFileInformationByHandle` needs, and
/// it was the first thing tried — but an attribute-only open takes no part in
/// Windows' sharing check, so a handle held that way pins nothing and the
/// directory under it can still be renamed away. Measured, not assumed:
/// `a_held_directory_cannot_be_renamed_out_from_under_us` fails on
/// `FILE_READ_ATTRIBUTES` by itself. `FILE_LIST_DIRECTORY` — `FILE_READ_DATA`
/// by another name, and what std's own `remove_dir_all` asks for — does count,
/// and is the least that does.
const FILE_LIST_DIRECTORY: u32 = 0x0000_0001;
const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
/// Without this, `CreateFileW` will not open a directory at all.
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
/// The handle is the entry itself, never what it points at.
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// What removing one profile's files came to, ready for the uninstall to print.
/// Nothing here has been printed or logged.
///
/// Removed and absent are counted apart because they mean different things:
/// files removed is the account's leftovers going, files already gone is an
/// ordinary second uninstall. A refusal is neither — it is in `trouble`, where
/// somebody can read it and act on it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct FileSweep {
    /// Named files that were there and are gone.
    pub files_removed: usize,
    /// Named files that were not there to begin with.
    pub files_absent: usize,
    /// Directory trees that were there and are gone. A planted junction that
    /// was unlinked rather than followed counts here too: the entry went.
    pub trees_removed: usize,
    /// Directory trees that were not there to begin with.
    pub trees_absent: usize,
    /// Folders that ended up empty and were removed. A folder with anything
    /// left in it is not counted and is not trouble — keeping it is the point.
    pub pruned: usize,
    /// Every refusal and every error, each `<path>: <what happened>` and each
    /// meant to be printed as it stands.
    pub trouble: Vec<String>,
}

/// Carry out `plan` inside `profile`, which must be the directory
/// `profiles::vet_dir` passed — every path in the plan is walked down from it.
///
/// Files, then trees, then the prunes, in that order: a folder is only ever
/// considered for pruning once everything of ours inside it has gone.
pub(super) fn remove_plan(profile: &Path, plan: &FilePlan) -> FileSweep {
    let mut sweep = FileSweep::default();
    for path in &plan.files {
        remove_file_at(profile, path, &mut sweep);
    }
    for path in &plan.trees {
        remove_tree_at(profile, path, &mut sweep);
    }
    for path in &plan.prune_if_empty {
        prune_at(profile, path, &mut sweep);
    }
    sweep
}

/// Delete one named file.
fn remove_file_at(profile: &Path, path: &Path, sweep: &mut FileSweep) {
    if let Some(why) = kept_by_the_uninstall(path) {
        sweep.trouble.push(why);
        return;
    }
    match reach(profile, path) {
        Reached::Absent => sweep.files_absent += 1,
        Reached::Refused(why) => sweep.trouble.push(why),
        Reached::Leaf { held, attributes } => {
            if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                sweep.trouble.push(format!(
                    "{}: it is a reparse point, not a file this uninstall wrote; leaving it alone",
                    path.display()
                ));
                return;
            }
            // `DeleteFileW`, which deletes a symbolic link rather than what it
            // points at and refuses a directory outright — so even a leaf
            // swapped in after the look above cannot take us anywhere.
            let done = std::fs::remove_file(path);
            // Not before here: every ancestor has to still be held while the
            // path above is resolved for the delete.
            drop(held);
            match done {
                Ok(()) => sweep.files_removed += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => sweep.files_absent += 1,
                Err(e) => {
                    sweep
                        .trouble
                        .push(format!("{}: {}", path.display(), explain("delete", &e)))
                }
            }
        }
    }
}

/// Delete one directory tree, or unlink one planted entry standing where a
/// directory tree of ours should be.
fn remove_tree_at(profile: &Path, path: &Path, sweep: &mut FileSweep) {
    if let Some(why) = kept_by_the_uninstall(path) {
        sweep.trouble.push(why);
        return;
    }
    match reach(profile, path) {
        Reached::Absent => sweep.trees_absent += 1,
        Reached::Refused(why) => sweep.trouble.push(why),
        Reached::Leaf { held, attributes } => {
            let done = if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                // A junction where our folder should be — a
                // `Packages\VilleMattila.Kuvatin_…` entry the account planted
                // and aimed elsewhere. `remove_dir` is `RemoveDirectoryW`,
                // which takes the link and leaves the target, and it never
                // descends. The entry is ours to remove; whatever it points at
                // is not ours to look at.
                std::fs::remove_dir(path)
            } else if attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
                sweep.trouble.push(format!(
                    "{}: it is a file where a folder of ours would be, so it is not ours to delete",
                    path.display()
                ));
                return;
            } else {
                // Safe to point at a folder inside a profile we do not trust,
                // and this is the evidence, read out of the std source for the
                // toolchain this builds with (rustc 1.96.0):
                //
                //  * `library/std/src/sys/fs/windows.rs`, `remove_dir_all`
                //    opens the root itself without following a link —
                //    "`FILE_FLAG_OPEN_REPARSE_POINT` opens a link instead of
                //    its target", `opts.custom_flags(c::FILE_FLAG_BACKUP_SEMANTICS
                //    | c::FILE_FLAG_OPEN_REPARSE_POINT)`.
                //  * `library/std/src/sys/fs/windows/remove_dir_all.rs`
                //    descends by handle and not by name: `open_dir` goes
                //    through `open_link_no_reparse`, which calls `NtOpenFile`
                //    with the parent's handle as `RootDirectory` and with
                //    `OBJ_DONT_REPARSE` — "ensures that we haven't been tricked
                //    into following a symlink" — plus `FILE_OPEN_REPARSE_POINT`.
                //    Its module documentation names the reason: "It must not be
                //    possible to trick this into deleting files outside of the
                //    parent directory (see CVE-2022-21658)."
                //
                // So a junction *inside* the tree is opened as the link it is
                // and unlinked, never descended into.
                // `a_junction_nested_inside_a_tree_is_unlinked_with_the_tree`
                // is the measurement of that, kept so that a toolchain which
                // ever changed it would fail a test here rather than delete
                // somebody's files.
                std::fs::remove_dir_all(path)
            };
            drop(held);
            match done {
                Ok(()) => sweep.trees_removed += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => sweep.trees_absent += 1,
                Err(e) => {
                    sweep
                        .trouble
                        .push(format!("{}: {}", path.display(), explain("delete", &e)))
                }
            }
        }
    }
}

/// Remove one folder, and only if it is empty.
///
/// [`paths::is_protected`] is deliberately *not* consulted here, and the reason
/// is the whole point of both: `AppData\Local\Kuvatin` is the folder it exists
/// to keep out of a recursive delete, and this is not a recursive delete.
/// `remove_dir` refusing a folder with anything left in it is what keeps the
/// user's signing keys — there is no second check to make.
fn prune_at(profile: &Path, path: &Path, sweep: &mut FileSweep) {
    match reach(profile, path) {
        // Nothing to prune, and nothing to say about it.
        Reached::Absent => {}
        Reached::Refused(why) => sweep.trouble.push(why),
        Reached::Leaf { held, attributes } => {
            if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                sweep.trouble.push(format!(
                    "{}: it is a reparse point, not a folder of ours to prune",
                    path.display()
                ));
                return;
            }
            let done = std::fs::remove_dir(path);
            drop(held);
            match done {
                Ok(()) => sweep.pruned += 1,
                // The user still keeps something of their own in it, or it has
                // gone already. Both are the ordinary case, not trouble.
                Err(e)
                    if e.raw_os_error() == Some(ERROR_DIR_NOT_EMPTY.0 as i32)
                        || e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    sweep
                        .trouble
                        .push(format!("{}: {}", path.display(), explain("remove", &e)))
                }
            }
        }
    }
}

/// Belt and braces over a path before a delete that would take everything under
/// it: `Roaming\Kuvatin` holds presets and settings, `Local\Kuvatin` can hold
/// the user's signing keys, and neither is ever ours to remove whole. `None`
/// when the path is fine.
fn kept_by_the_uninstall(path: &Path) -> Option<String> {
    paths::is_protected(path).then(|| {
        format!(
            "{}: it is one of the folders this uninstall keeps, whatever the plan asked for",
            path.display()
        )
    })
}

/// What the walk down to one target found.
enum Reached {
    /// Every component above the leaf is an ordinary, unredirected directory.
    /// `held` pins each of them, from the profile root down to the target's
    /// parent, and must outlive the delete. `attributes` are the leaf's own,
    /// read from its handle — the leaf is *not* held, because a handle on it
    /// would be the thing stopping us from deleting it.
    Leaf { held: Vec<File>, attributes: u32 },
    /// The target, or a directory on the way to it, is not there.
    Absent,
    /// Why nothing will be deleted, naming the component that stopped us.
    Refused(String),
}

/// Walk from `profile` down to `target`, holding every directory on the way.
///
/// The leaf's attributes come back unjudged, because what they mean depends on
/// what was asked for: a reparse point where a *tree* should be is a planted
/// entry to unlink, and a reparse point where a *file* should be is something
/// to leave alone and report. Every component above it is judged here, and a
/// reparse point on any of them refuses the path outright.
fn reach(profile: &Path, target: &Path) -> Reached {
    let Ok(rest) = target.strip_prefix(profile) else {
        return Reached::Refused(format!(
            "{}: it is not inside {}, so it is not this account's to delete",
            target.display(),
            profile.display()
        ));
    };
    let mut steps = Vec::new();
    for component in rest.components() {
        // Plain names only. `..` would climb back out of the profile, and a
        // root or prefix component would mean `strip_prefix` left an absolute
        // path behind. Neither can come out of `paths::plan` today; refusing
        // them is what keeps that true if it ever changes.
        match component {
            Component::Normal(name) => steps.push(name),
            other => {
                return Reached::Refused(format!(
                    "{}: {:?} is not a plain name, so the path is not one to walk",
                    target.display(),
                    other.as_os_str()
                ))
            }
        }
    }

    let mut held: Vec<File> = Vec::new();
    let mut here = profile.to_path_buf();
    let mut left = steps.into_iter();
    loop {
        let opened = match open_no_follow(&here) {
            Ok(opened) => opened,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Reached::Absent,
            Err(e) => {
                return Reached::Refused(format!("{}: {}", here.display(), explain("open", &e)))
            }
        };
        // The handle's attributes, not the path's: once the handle is open,
        // nothing anyone does to the name can change what this answers about.
        let attributes = match opened.metadata() {
            Ok(meta) => meta.file_attributes(),
            Err(e) => {
                return Reached::Refused(format!("{}: {}", here.display(), explain("read", &e)))
            }
        };
        let Some(name) = left.next() else {
            return Reached::Leaf { held, attributes };
        };
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Reached::Refused(format!(
                "{}: it is a reparse point, so {} is not being deleted through it — a junction \
                 there needs no privilege to make and this runs as SYSTEM",
                here.display(),
                target.display()
            ));
        }
        if attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Reached::Refused(format!(
                "{}: it is not a directory, so nothing under it is there to delete",
                here.display()
            ));
        }
        // Held from here until the caller is done: while this handle is open
        // with no `FILE_SHARE_DELETE`, this directory cannot be renamed away or
        // removed, so the next step down resolves through the object we have
        // just vetted rather than through one swapped in behind us.
        held.push(opened);
        here.push(name);
    }
}

/// Open one component the way the walk needs it: the entry itself rather than
/// whatever it points at, and held in a way that stops anything renaming or
/// deleting it while we work.
///
/// This is `CreateFileW(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
/// OPEN_EXISTING)` — `std::fs::OpenOptions` on Windows passes these straight
/// through to it, and `File::metadata` on the result is
/// `GetFileInformationByHandle`, so nothing is gained by calling either by hand.
/// The `File` closes itself, which is what makes "hold every ancestor" a
/// `Vec<File>` that simply stays in scope.
///
/// One cost of asking for a right that counts towards sharing: a *leaf* that
/// another process is holding without sharing reads is reported rather than
/// attempted, where a bare attribute open would have looked at it. That is a
/// narrow loss — such a file will not delete either unless its holder shared
/// `FILE_SHARE_DELETE` — and the same right is what pins every ancestor, so it
/// is not one worth splitting the open in two to recover.
fn open_no_follow(path: &Path) -> std::io::Result<File> {
    File::options()
        .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES)
        // Not `FILE_SHARE_DELETE`, and that omission is the whole point: it is
        // what makes a held directory unrenameable and unremovable. Read and
        // write stay shared so that holding a profile's folders open does not
        // disturb an account that is signed in and using them.
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// Say what a file-system error means in words a log reader can act on, keeping
/// the raw code for whoever needs it. The same shape as
/// `regutil::explain_error`, which reports on the registry half of this same
/// uninstall, so a line from either reads alike.
fn explain(doing: &str, e: &std::io::Error) -> String {
    match e.raw_os_error().map(|code| code as u32) {
        Some(code) if code == ERROR_ACCESS_DENIED.0 => {
            format!("we are not allowed to {doing} it (error {code})")
        }
        Some(code) if code == ERROR_SHARING_VIOLATION.0 => {
            format!("something else has it open, so it would not {doing} (error {code})")
        }
        Some(code) => format!("it would not {doing} (error {code})"),
        None => format!("it would not {doing} ({e})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A junction that removes itself, so a failed assertion cannot leave the
    /// temp directory with a link in it for `TempDir`'s recursive cleanup to
    /// walk into.
    struct Junction {
        link: PathBuf,
    }

    impl Junction {
        /// `None` when this environment will not make one, with a printed
        /// reason — a test that cannot build its attack proves nothing either
        /// way. On CI a skip would mean the gate is not running the thing it
        /// gates, so there it is a failure instead.
        fn new(link: &Path, target: &Path) -> Option<Self> {
            let made = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output();
            if made.is_ok() && link.exists() {
                return Some(Junction {
                    link: link.to_path_buf(),
                });
            }
            if std::env::var_os("CI").is_some() {
                panic!(
                    "junction tests must run on CI, but no junction could be made at {} ({made:?})",
                    link.display()
                );
            }
            println!(
                "skipping: could not create a junction at {} ({made:?})",
                link.display()
            );
            None
        }
    }

    impl Drop for Junction {
        fn drop(&mut self) {
            // Gone already when the thing under test unlinked it, which is what
            // half of these tests assert; only a surviving link needs removing.
            if !self.link.exists() {
                return;
            }
            // `remove_dir` on a junction removes the link, never its target.
            if let Err(e) = std::fs::remove_dir(&self.link) {
                eprintln!("could not remove the junction {:?}: {e}", self.link);
            }
        }
    }

    /// An empty plan, to be filled in by whichever test wants it.
    fn empty_plan() -> FilePlan {
        FilePlan {
            files: Vec::new(),
            trees: Vec::new(),
            prune_if_empty: Vec::new(),
        }
    }

    /// `<profile>\AppData\Local`, created.
    fn local_in(profile: &Path) -> PathBuf {
        let local = profile.join("AppData").join("Local");
        std::fs::create_dir_all(&local).expect("the AppData\\Local tree");
        local
    }

    #[test]
    fn a_plain_tree_and_a_plain_file_go() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let profile = dir.path();
        let local = local_in(profile);

        let tree = local.join("Temp").join("kuvatin");
        std::fs::create_dir_all(tree.join("seq-cache").join("abc")).expect("the cache tree");
        std::fs::write(tree.join("seq-cache").join("abc").join(".complete"), b"x")
            .expect("a nested file");
        std::fs::write(tree.join("spool.paths"), b"x").expect("a file in the tree");

        let log = local.join("Kuvatin").join("kuvatin.log");
        std::fs::create_dir_all(log.parent().expect("a parent")).expect("the Kuvatin folder");
        std::fs::write(&log, b"log").expect("a log file");

        let plan = FilePlan {
            files: vec![log.clone()],
            trees: vec![tree.clone()],
            prune_if_empty: Vec::new(),
        };
        let sweep = remove_plan(profile, &plan);

        assert!(!tree.exists(), "the tree should be gone");
        assert!(!log.exists(), "the log should be gone");
        assert_eq!(sweep.files_removed, 1);
        assert_eq!(sweep.trees_removed, 1);
        assert_eq!(sweep.trouble, Vec::<String>::new());
    }

    #[test]
    fn what_was_never_there_is_counted_absent_and_is_not_trouble() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let profile = dir.path();
        let local = local_in(profile);

        let plan = FilePlan {
            files: vec![local.join("Kuvatin").join("crash.log")],
            trees: vec![local.join("Temp").join("kuvatin")],
            prune_if_empty: Vec::new(),
        };
        let sweep = remove_plan(profile, &plan);

        assert_eq!(sweep.files_absent, 1);
        assert_eq!(sweep.trees_absent, 1);
        assert_eq!(sweep.files_removed, 0);
        assert_eq!(sweep.trees_removed, 0);
        assert_eq!(
            sweep.trouble,
            Vec::<String>::new(),
            "a second uninstall finds nothing and that is not trouble"
        );
    }

    /// The whole point of `prune_if_empty`: `Local\Kuvatin` can hold the user's
    /// signing keys, so it goes only when our own files were all that was in
    /// it.
    #[test]
    fn prune_takes_an_empty_folder_and_leaves_one_with_anything_in_it() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let profile = dir.path();
        let local = local_in(profile);

        let emptied = local.join("Kuvatin");
        std::fs::create_dir(&emptied).expect("the Kuvatin folder");
        let sweep = remove_plan(
            profile,
            &FilePlan {
                prune_if_empty: vec![emptied.clone()],
                ..empty_plan()
            },
        );
        assert!(!emptied.exists(), "an empty folder should be pruned");
        assert_eq!(sweep.pruned, 1);
        assert_eq!(sweep.trouble, Vec::<String>::new());

        std::fs::create_dir(&emptied).expect("the Kuvatin folder again");
        std::fs::write(emptied.join("signing.pfx"), b"key").expect("something of the user's");
        let sweep = remove_plan(
            profile,
            &FilePlan {
                prune_if_empty: vec![emptied.clone()],
                ..empty_plan()
            },
        );
        assert!(emptied.exists(), "a folder with anything in it must stay");
        assert!(
            emptied.join("signing.pfx").exists(),
            "and so must what is in it"
        );
        assert_eq!(sweep.pruned, 0);
        assert_eq!(
            sweep.trouble,
            Vec::<String>::new(),
            "a folder the user still keeps things in is the ordinary case, not trouble"
        );
    }

    /// A junction part-way down the path is the attack this module exists for:
    /// `AppData\Local\Temp` belongs to the account, `mklink /J` needs no
    /// privilege, and a recursive delete that followed it would reach whatever
    /// it points at, as SYSTEM.
    #[test]
    fn a_junction_on_the_way_to_a_tree_is_refused_and_nothing_behind_it_is_touched() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let profile = dir.path().join("profile");
        let local = local_in(&profile);

        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(elsewhere.join("kuvatin")).expect("the junction's target");
        let bait = elsewhere.join("kuvatin").join("precious.txt");
        std::fs::write(&bait, b"precious").expect("bait");

        let link = local.join("Temp");
        let Some(_junction) = Junction::new(&link, &elsewhere) else {
            return;
        };

        let target = link.join("kuvatin");
        let sweep = remove_plan(
            &profile,
            &FilePlan {
                trees: vec![target],
                ..empty_plan()
            },
        );

        assert_eq!(sweep.trees_removed, 0);
        assert_eq!(sweep.trees_absent, 0);
        assert_eq!(sweep.trouble.len(), 1, "{:?}", sweep.trouble);
        let why = &sweep.trouble[0];
        assert!(why.contains("reparse point"), "vague reason: {why}");
        assert!(
            why.contains("Temp"),
            "the reason should name the component: {why}"
        );

        assert_eq!(
            std::fs::read(&bait).expect("the bait survives"),
            b"precious",
            "nothing behind the junction may be touched"
        );
        assert!(
            elsewhere.join("kuvatin").is_dir(),
            "and the target directory itself must still be there"
        );
    }

    /// A junction *as* the thing to delete — a `VilleMattila.Kuvatin_evil`
    /// entry the account planted in its own `Packages` folder. The entry is
    /// unlinked; what it points at is not descended into and not touched.
    #[test]
    fn a_junction_where_a_tree_should_be_is_unlinked_not_followed() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let profile = dir.path().join("profile");
        let local = local_in(&profile);
        let packages = local.join("Packages");
        std::fs::create_dir(&packages).expect("the Packages folder");

        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).expect("the junction's target");
        let bait = elsewhere.join("precious.txt");
        std::fs::write(&bait, b"precious").expect("bait");

        let link = packages.join("VilleMattila.Kuvatin_evil");
        let Some(_junction) = Junction::new(&link, &elsewhere) else {
            return;
        };

        let sweep = remove_plan(
            &profile,
            &FilePlan {
                trees: vec![link.clone()],
                ..empty_plan()
            },
        );

        assert!(!link.exists(), "the planted entry should be unlinked");
        assert_eq!(sweep.trees_removed, 1);
        assert_eq!(
            std::fs::read(&bait).expect("the bait survives"),
            b"precious",
            "the junction's target must survive"
        );
        assert!(
            sweep.trouble.is_empty(),
            "unlinking a planted entry is what should happen: {:?}",
            sweep.trouble
        );
    }

    /// A junction *inside* a tree we are entitled to delete. The tree goes, the
    /// link inside it goes with it, and the target it pointed at does not.
    #[test]
    fn a_junction_nested_inside_a_tree_is_unlinked_with_the_tree() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let profile = dir.path().join("profile");
        let local = local_in(&profile);

        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).expect("the junction's target");
        let bait = elsewhere.join("precious.txt");
        std::fs::write(&bait, b"precious").expect("bait");

        let tree = local.join("Temp").join("kuvatin");
        std::fs::create_dir_all(tree.join("seq-cache")).expect("the cache tree");
        std::fs::write(tree.join("seq-cache").join("real.bin"), b"ours").expect("a real file");

        let link = tree.join("seq-cache").join("somewhere-else");
        let Some(_junction) = Junction::new(&link, &elsewhere) else {
            return;
        };

        let sweep = remove_plan(
            &profile,
            &FilePlan {
                trees: vec![tree.clone()],
                ..empty_plan()
            },
        );

        assert!(
            !tree.exists(),
            "the tree should be gone: {:?}",
            sweep.trouble
        );
        assert_eq!(sweep.trees_removed, 1);
        assert_eq!(
            std::fs::read(&bait).expect("the bait survives"),
            b"precious",
            "the nested junction's target must survive"
        );
        assert!(elsewhere.is_dir(), "and so must the target directory");
    }

    /// The belt-and-braces check, proved from this side too: a path the plan
    /// should never have contained is refused rather than deleted.
    #[test]
    fn a_folder_the_uninstall_keeps_is_refused_even_if_it_is_asked_for() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let profile = dir.path();
        let local = local_in(profile);

        let kept = local.join("Kuvatin");
        std::fs::create_dir(&kept).expect("the Kuvatin folder");
        std::fs::write(kept.join("signing.pfx"), b"key").expect("the user's key");

        let roaming = profile.join("AppData").join("Roaming").join("Kuvatin");
        std::fs::create_dir_all(&roaming).expect("the Roaming folder");
        std::fs::write(roaming.join("presets.toml"), b"[]").expect("the user's presets");

        let sweep = remove_plan(
            profile,
            &FilePlan {
                trees: vec![kept.clone(), roaming.clone()],
                ..empty_plan()
            },
        );

        assert!(kept.join("signing.pfx").exists(), "the key must survive");
        assert!(
            roaming.join("presets.toml").exists(),
            "presets must survive"
        );
        assert_eq!(sweep.trees_removed, 0);
        assert_eq!(sweep.trouble.len(), 2, "{:?}", sweep.trouble);
        for why in &sweep.trouble {
            assert!(
                why.contains("Kuvatin"),
                "the reason should name the folder: {why}"
            );
        }
    }

    /// Holding every ancestor open is what keeps the path we vetted the path we
    /// delete through, so this pins the property the whole walk rests on: while
    /// a directory is held, nobody can rename it out of the way and put a
    /// junction in its place.
    #[test]
    fn a_held_directory_cannot_be_renamed_out_from_under_us() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let held_path = dir.path().join("AppData");
        std::fs::create_dir(&held_path).expect("a directory to hold");

        let held = super::open_no_follow(&held_path).expect("open the directory");
        let moved = std::fs::rename(&held_path, dir.path().join("moved-aside"));
        assert!(
            moved.is_err(),
            "a directory held without FILE_SHARE_DELETE must not be renameable"
        );
        assert!(
            std::fs::remove_dir(&held_path).is_err(),
            "nor removable while it is held"
        );

        drop(held);
        std::fs::rename(&held_path, dir.path().join("moved-aside"))
            .expect("and it moves freely once the handle is gone");
    }

    /// A Deny ACE on a FILE, put on with `icacls` and taken off again — the
    /// same recipe `hive.rs` uses, because the `windows` crate features that
    /// could do it directly are ones this build does not otherwise want.
    struct DeniedFile {
        path: PathBuf,
        who: String,
    }

    impl DeniedFile {
        fn new(path: &Path) -> Option<Self> {
            let who = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
                (Ok(domain), Ok(user)) => format!(r"{domain}\{user}"),
                (_, Ok(user)) => user,
                _ => {
                    println!("skipping: no USERNAME to deny");
                    return None;
                }
            };
            let out = std::process::Command::new("icacls")
                .arg(path)
                .arg("/deny")
                .arg(format!("{who}:(F)"))
                .output();
            match out {
                Ok(out) if out.status.success() => Some(DeniedFile {
                    path: path.to_path_buf(),
                    who,
                }),
                other => {
                    println!("skipping: could not deny access to {path:?}: {other:?}");
                    None
                }
            }
        }
    }

    impl Drop for DeniedFile {
        fn drop(&mut self) {
            let out = std::process::Command::new("icacls")
                .arg(&self.path)
                .arg("/remove:d")
                .arg(&self.who)
                .output();
            if !matches!(&out, Ok(o) if o.status.success()) {
                eprintln!("could not restore access to {:?}: {out:?}", self.path);
            }
        }
    }

    /// A file we are not allowed to touch is reported by name, in words that
    /// say what to do about it, rather than counted absent or passed over.
    #[test]
    fn a_file_we_may_not_delete_says_so_by_name() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let profile = dir.path();
        let local = local_in(profile);
        let kuvatin = local.join("Kuvatin");
        std::fs::create_dir(&kuvatin).expect("the Kuvatin folder");
        let log = kuvatin.join("kuvatin.log");
        std::fs::write(&log, b"log").expect("a log file");

        let Some(_denied) = DeniedFile::new(&log) else {
            return;
        };
        let sweep = remove_plan(
            profile,
            &FilePlan {
                files: vec![log.clone()],
                ..empty_plan()
            },
        );

        assert_eq!(sweep.files_removed, 0);
        assert_eq!(
            sweep.files_absent, 0,
            "a file we may not touch is not a file that is not there"
        );
        assert_eq!(sweep.trouble.len(), 1, "{:?}", sweep.trouble);
        let why = &sweep.trouble[0];
        assert!(
            why.contains("kuvatin.log"),
            "the file should be named: {why}"
        );
        assert!(
            why.contains("not allowed"),
            "and the reason should be readable: {why}"
        );
        assert!(why.contains("error 5"), "with the code kept: {why}");
    }

    /// A path that is not inside the profile at all is refused before anything
    /// is opened. The plan cannot produce one today; this is what happens if it
    /// ever does.
    #[test]
    fn a_path_outside_the_profile_is_refused() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let profile = dir.path().join("profile");
        std::fs::create_dir(&profile).expect("the profile");
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).expect("somewhere else");
        std::fs::write(outside.join("keep.txt"), b"precious").expect("bait");

        let sweep = remove_plan(
            &profile,
            &FilePlan {
                trees: vec![outside.clone()],
                ..empty_plan()
            },
        );

        assert!(outside.join("keep.txt").exists(), "the bait survives");
        assert_eq!(sweep.trees_removed, 0);
        assert_eq!(sweep.trouble.len(), 1, "{:?}", sweep.trouble);
        assert!(sweep.trouble[0].contains("outside"), "{:?}", sweep.trouble);
    }
}
