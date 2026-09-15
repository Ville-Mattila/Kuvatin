//! Carry out one profile's [`FilePlan`] as SYSTEM, without ever deleting
//! through a reparse point.
//!
//! **What is trusted.** `profiles::vet_dir` has established that the profile
//! directory itself is a real, unredirected directory. Nothing below it is
//! vouched for by anybody. Every folder inside a profile belongs to that
//! account, a directory junction needs no privilege at all, and the account can
//! be signed in and working while the uninstall runs — so its owner can aim
//! `AppData`, `Local`, `Temp`, `Kuvatin`, `Packages`, or a
//! `VilleMattila.Kuvatin_*` entry of their own making, at anywhere on the
//! machine, and can do it between any two of our statements. A SYSTEM delete
//! that followed one of those is an arbitrary-delete primitive against the
//! whole machine. `super::paths` mints names and vets none of them; this is
//! where the names become deletions, so this is where the vetting is.
//!
//! # A directory can change into a junction without moving
//!
//! The obvious defence — hold every directory open without `FILE_SHARE_DELETE`,
//! so nothing can be renamed or deleted behind us — is not enough, and the
//! first version of this module was broken because of it. `FSCTL_SET_REPARSE_POINT`
//! converts a directory into a junction **in place**: same object, same handle,
//! no rename and no delete. All it needs is that the directory be empty
//! (`ERROR_DIR_NOT_EMPTY` otherwise) and a handle with write access — and
//! `FILE_WRITE_ATTRIBUTES` counts, which takes no part in Windows' sharing
//! check at all, so no share mode we could ask for can refuse it. All three
//! facts are measured, not assumed:
//! `a_directory_we_hold_can_still_be_turned_into_a_junction` pins the first two
//! and `a_parent_whose_child_we_hold_cannot_be_converted` the third.
//!
//! So a name checked and then used is worthless here, and that is what the
//! walk is built around.
//!
//! # How the path is frozen
//!
//! [`reach`] walks from the profile root one component at a time — the
//! components of `files` as much as those of `trees` — opening each with
//! `FILE_FLAG_OPEN_REPARSE_POINT`, so the handle is the entry itself and never
//! what it points at, and judging it by that handle's own attributes
//! (`File::metadata` on Windows is `GetFileInformationByHandle`). Then:
//!
//! 1. **The leaf is opened and held too**, with `DELETE` access and without
//!    `FILE_SHARE_DELETE`. A held object cannot be opened for `DELETE` by
//!    anyone else, so its name cannot be removed from its parent — not even by
//!    a POSIX-semantics delete.
//! 2. **Every ancestor is then re-read from its handle** and refused if it has
//!    become a reparse point since the walk passed it. This is the one window
//!    that exists: until we held the leaf, the leaf's parent could be emptied
//!    and converted, and the leaf open itself resolves the whole path. If that
//!    happened, the handle we now hold is on the wrong object — so we look at
//!    the ancestors again and delete nothing.
//! 3. **After that the path is frozen.** The leaf's name is pinned by our
//!    handle, so its parent is permanently non-empty and therefore cannot be
//!    converted; the same argument holds for that parent's parent, since it
//!    contains a directory whose name we hold, and so on up to the profile
//!    root. Every component is now an object we vetted and none of them can
//!    change identity while we work.
//!
//! # What the deletes then do
//!
//! A file, a prune, and a junction standing where one of our folders should be
//! are all deleted **through the handle** — `SetFileInformationByHandle` with
//! `FILE_DISPOSITION_INFO` — so no path is resolved a second time and the
//! object removed is exactly the object vetted. The junction case never
//! descends: the entry is what we delete, and whatever it points at is not ours
//! to look at.
//!
//! A tree is handed to `std::fs::remove_dir_all`, which is safe to point at a
//! folder inside a hostile profile *because the path is frozen by step 3*: it
//! opens its root by path (that path is now ours), and everything below the
//! root it does by handle, never by name — the evidence is quoted at the call.
//! It cannot perform the last step, deleting the root itself, because we are
//! holding the root without `FILE_SHARE_DELETE`; so it empties the tree and
//! stops, and the root goes through our own handle like everything else. That
//! is by design rather than a workaround: granting `FILE_SHARE_DELETE` so that
//! std could finish would let the owner POSIX-delete the root, empty the
//! parent and convert it between our check and std's open, which is the very
//! hole this module exists to close.
//!
//! # What this does not cover
//!
//! A reparse point *above* the profile (a junction at `C:\Users`) needs
//! administrator rights to create, and an attacker who has those has no need of
//! this. A **hard link** at one of the `files` carries no reparse point, so it
//! passes every check here; the disposition delete then unlinks that name and
//! leaves the file it shared, which is the harmless half of the same opening
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
use std::os::windows::io::AsRawHandle;
use std::path::{Component, Path, PathBuf};
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_DIR_NOT_EMPTY, ERROR_SHARING_VIOLATION, HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO,
};

use super::paths::{self, FilePlan};
use super::profiles::FILE_ATTRIBUTE_REPARSE_POINT;

/// The `CreateFileW` bits below are spelled out because the `windows` crate
/// exports them from a namespace this module reaches only for
/// `SetFileInformationByHandle` — and `std::fs::OpenOptions` passes them
/// straight through to the same `CreateFileW` call, with a `File` that closes
/// itself, so writing the open by hand would buy nothing but an `unsafe` block
/// and a handle to remember.
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
/// What the walk asks for, and it has to be this much.
///
/// `FILE_READ_ATTRIBUTES` alone is all `GetFileInformationByHandle` needs, and
/// it was the first thing tried — but an attribute-only open takes no part in
/// Windows' sharing check, so a handle held that way pins nothing at all.
/// Measured, not assumed: `a_held_directory_cannot_be_renamed_out_from_under_us`
/// fails on `FILE_READ_ATTRIBUTES` by itself. `FILE_LIST_DIRECTORY` —
/// `FILE_READ_DATA` by another name, and what std's own `remove_dir_all` asks
/// for — does count, and is the least that does.
const FILE_LIST_DIRECTORY: u32 = 0x0000_0001;
const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
/// `DELETE`, which the leaf handle needs so the disposition below can be set
/// on it, and whose presence is also what makes a held leaf unremovable by
/// anyone else.
const DELETE: u32 = 0x0001_0000;
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
#[derive(Debug, Default)]
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

/// Delete one named file, through its own handle.
fn remove_file_at(profile: &Path, path: &Path, sweep: &mut FileSweep) {
    if let Some(why) = kept_by_the_uninstall(path) {
        sweep.trouble.push(why);
        return;
    }
    match reach(profile, path) {
        Reached::Absent => sweep.files_absent += 1,
        Reached::Refused(why) => sweep.trouble.push(why),
        Reached::Leaf(held) => {
            if held.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                sweep.trouble.push(format!(
                    "{}: it is a reparse point, not a file this uninstall wrote; leaving it alone",
                    path.display()
                ));
                return;
            }
            if held.attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
                sweep.trouble.push(format!(
                    "{}: it is a folder where a file of ours would be, so it is not ours to delete",
                    path.display()
                ));
                return;
            }
            match dispose(&held.leaf) {
                Ok(()) => sweep.files_removed += 1,
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
        Reached::Leaf(held) => {
            if held.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                // A junction where our folder should be — a
                // `Packages\VilleMattila.Kuvatin_…` entry the account planted
                // and aimed elsewhere. We delete the entry through its own
                // handle and never descend: what it points at is not ours to
                // look at, and a disposition on a reparse point removes the
                // reparse point.
                match dispose(&held.leaf) {
                    Ok(()) => sweep.trees_removed += 1,
                    Err(e) => {
                        sweep
                            .trouble
                            .push(format!("{}: {}", path.display(), explain("unlink", &e)))
                    }
                }
                return;
            }
            if held.attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
                sweep.trouble.push(format!(
                    "{}: it is a file where a folder of ours would be, so it is not ours to delete",
                    path.display()
                ));
                return;
            }
            // Safe to point at a folder inside a profile we do not trust — but
            // only because `reach` has frozen the path it is about to resolve;
            // see this module's documentation. What std then does below that
            // root is the other half, and this is the evidence, read out of the
            // std source for the toolchain this builds with (rustc 1.96.0):
            //
            //  * `library/std/src/sys/fs/windows.rs`, `remove_dir_all` opens
            //    the root itself without following a link —
            //    "`FILE_FLAG_OPEN_REPARSE_POINT` opens a link instead of its
            //    target", `opts.custom_flags(c::FILE_FLAG_BACKUP_SEMANTICS |
            //    c::FILE_FLAG_OPEN_REPARSE_POINT)`.
            //  * `library/std/src/sys/fs/windows/remove_dir_all.rs` descends by
            //    handle and not by name: `open_dir` goes through
            //    `open_link_no_reparse`, which calls `NtOpenFile` with the
            //    parent's handle as `RootDirectory` and with `OBJ_DONT_REPARSE`
            //    — "ensures that we haven't been tricked into following a
            //    symlink" — plus `FILE_OPEN_REPARSE_POINT`. Its module
            //    documentation names the reason: "It must not be possible to
            //    trick this into deleting files outside of the parent directory
            //    (see CVE-2022-21658)."
            //
            // One caveat that comes with quoting it: `OBJ_DONT_REPARSE` is
            // applied best-effort. `remove_dir_all.rs:90` holds it in a static
            // that `103-112` clears for the rest of the process the first time
            // `NtOpenFile` answers `INVALID_PARAMETER`, "Retry without
            // OBJ_DONT_REPARSE if it's not supported" — on a Windows too old
            // for it, which is well before any build this ships to.
            // `FILE_OPEN_REPARSE_POINT` is passed unconditionally either way,
            // so the open still takes the link rather than its target; what
            // would be lost is only the belt to that braces.
            //
            // So a junction *inside* the tree is opened as the link it is and
            // unlinked, never descended into.
            // `a_junction_nested_inside_a_tree_is_unlinked_with_the_tree` is
            // the measurement of that, kept so that a toolchain which ever
            // changed it would fail a test here rather than delete somebody's
            // files.
            let emptied = std::fs::remove_dir_all(path);
            // And the root itself through our own handle, because std cannot:
            // we are holding it without `FILE_SHARE_DELETE`, which is what
            // pinned this path in the first place. Its answer is the one that
            // decides, so a sharing violation from `remove_dir_all` — which is
            // us — needs no special case here.
            match dispose(&held.leaf) {
                Ok(()) => sweep.trees_removed += 1,
                Err(e) => {
                    let why = match &emptied {
                        // The tree did not empty, which is why the root will
                        // not go; that reason is the useful one.
                        Err(first) if e.raw_os_error() == Some(ERROR_DIR_NOT_EMPTY.0 as i32) => {
                            format!("{}; {}", explain("empty", first), explain("remove", &e))
                        }
                        _ => explain("remove", &e),
                    };
                    sweep.trouble.push(format!("{}: {why}", path.display()));
                }
            }
        }
    }
}

/// Remove one folder, and only if it is empty.
///
/// [`paths::is_protected`] is deliberately *not* consulted here, and the reason
/// is the whole point of both: `AppData\Local\Kuvatin` is the folder it exists
/// to keep out of a recursive delete, and this is not a recursive delete. A
/// disposition on a directory that still has anything in it is refused by
/// Windows with `ERROR_DIR_NOT_EMPTY`, and that refusal is what keeps the
/// user's signing keys — there is no second check to make.
fn prune_at(profile: &Path, path: &Path, sweep: &mut FileSweep) {
    match reach(profile, path) {
        // Nothing to prune, and nothing to say about it.
        Reached::Absent => {}
        Reached::Refused(why) => sweep.trouble.push(why),
        Reached::Leaf(held) => {
            if held.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                sweep.trouble.push(format!(
                    "{}: it is a reparse point, not a folder of ours to prune",
                    path.display()
                ));
                return;
            }
            if held.attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
                sweep.trouble.push(format!(
                    "{}: it is a file where a folder of ours would be, so it is not ours to prune",
                    path.display()
                ));
                return;
            }
            match dispose(&held.leaf) {
                Ok(()) => sweep.pruned += 1,
                // The user still keeps something of their own in it. That is
                // the ordinary case and the whole reason this is a prune.
                Err(e) if e.raw_os_error() == Some(ERROR_DIR_NOT_EMPTY.0 as i32) => {}
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

/// A leaf whose whole path has been walked, vetted and frozen.
///
/// The `ancestors` pin every component above the leaf and must outlive the
/// delete; `leaf` is the handle the delete goes through, and holding it is what
/// stops the leaf's parent being emptied and converted underneath us.
struct Held {
    ancestors: Vec<(PathBuf, File)>,
    leaf: File,
    attributes: u32,
}

/// What the walk down to one target found.
enum Reached {
    /// Every component was vetted and the whole path is now held.
    Leaf(Held),
    /// The target, or a directory on the way to it, is not there.
    Absent,
    /// Why nothing will be deleted, naming the component that stopped us.
    Refused(String),
}

/// Walk from `profile` down to `target`, holding every component, and hand back
/// a path nothing can change underneath the caller.
///
/// The leaf's attributes come back unjudged, because what they mean depends on
/// what was asked for: a reparse point where a *tree* should be is a planted
/// entry to unlink, and a reparse point where a *file* should be is something
/// to leave alone and report. Every component above it is judged here.
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
    // The profile directory itself. Deleting it would take the account's whole
    // profile, and there would be no ancestor left to pin anything against, so
    // this is refused rather than walked.
    let Some((leaf_name, ancestor_names)) = steps.split_last() else {
        return Reached::Refused(format!(
            "{}: it is the profile directory itself, which this uninstall never removes",
            target.display()
        ));
    };

    let mut ancestors: Vec<(PathBuf, File)> = Vec::new();
    let mut here = profile.to_path_buf();
    for name in std::iter::once(None).chain(ancestor_names.iter().map(Some)) {
        if let Some(name) = name {
            here.push(name);
        }
        let opened = match open_pinned(&here) {
            Ok(opened) => opened,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Reached::Absent,
            Err(e) => {
                return Reached::Refused(format!("{}: {}", here.display(), explain("open", &e)))
            }
        };
        let attributes = match opened.metadata() {
            Ok(meta) => meta.file_attributes(),
            Err(e) => {
                return Reached::Refused(format!("{}: {}", here.display(), explain("read", &e)))
            }
        };
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Reached::Refused(reparse_on_the_way(&here, target));
        }
        if attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Reached::Refused(format!(
                "{}: it is not a directory, so {} is not there to delete",
                here.display(),
                target.display()
            ));
        }
        ancestors.push((here.clone(), opened));
    }

    here.push(leaf_name);
    // The one window there is, and the one place a test can stand in it.
    meddle_between_walk_and_leaf(&here);
    // The leaf, with `DELETE` and no `FILE_SHARE_DELETE`: from here its name
    // cannot be removed from its parent, so the parent cannot be emptied, so
    // the parent cannot be turned into a junction. That is what freezes the
    // path — and it is only true from this line onwards, which is why the
    // ancestors are looked at again below.
    let leaf = match open_leaf(&here) {
        Ok(leaf) => Some(leaf),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Reached::Refused(format!("{}: {}", here.display(), explain("open", &e))),
    };

    // Until the line above, the leaf's parent was a directory the owner could
    // empty and convert in place, and the open that just happened resolved the
    // whole path by name. So look at every ancestor once more, through the
    // handle we have held all along, and refuse if any of them has become a
    // reparse point since we passed it. Done on the absent path too: a parent
    // that has turned into a junction aimed somewhere with no such file would
    // otherwise be reported as an ordinary "nothing there".
    for (path, handle) in &ancestors {
        match handle.metadata() {
            Ok(meta) if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 => {
                return Reached::Refused(format!(
                    "{}: it became a reparse point while we were walking down to {} — nothing \
                     was deleted",
                    path.display(),
                    target.display()
                ))
            }
            Ok(_) => {}
            Err(e) => {
                return Reached::Refused(format!("{}: {}", path.display(), explain("re-read", &e)))
            }
        }
    }

    let Some(leaf) = leaf else {
        return Reached::Absent;
    };
    let attributes = match leaf.metadata() {
        Ok(meta) => meta.file_attributes(),
        Err(e) => return Reached::Refused(format!("{}: {}", here.display(), explain("read", &e))),
    };
    Reached::Leaf(Held {
        ancestors,
        leaf,
        attributes,
    })
}

// The seam the regression test stands in, and nothing else uses. It fires at
// the only moment the attack works: every directory above the leaf has been
// vetted and pinned, and the leaf itself has not been opened yet, so its parent
// is still empty-able and so still convertible. What must catch the meddling is
// the re-read of the ancestors afterwards — which is the property under test,
// so the hook goes here and nowhere later.
#[cfg(test)]
type Meddling = std::cell::RefCell<Option<Box<dyn FnMut(&Path)>>>;

#[cfg(test)]
thread_local! {
    static MEDDLE: Meddling = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn meddle_between_walk_and_leaf(leaf: &Path) {
    MEDDLE.with(|slot| {
        if let Some(meddle) = slot.borrow_mut().as_mut() {
            meddle(leaf);
        }
    });
}

#[cfg(not(test))]
#[inline]
fn meddle_between_walk_and_leaf(_leaf: &Path) {}

/// Why a component we met on the way down stops the whole path.
fn reparse_on_the_way(here: &Path, target: &Path) -> String {
    format!(
        "{}: it is a reparse point, so {} is not being deleted through it — a junction there \
         needs no privilege to make and this runs as SYSTEM",
        here.display(),
        target.display()
    )
}

/// Open one directory on the way down and hold it.
///
/// `std::fs::OpenOptions` passes all of this straight to `CreateFileW`, and
/// `File::metadata` on the result is `GetFileInformationByHandle`, so writing
/// either by hand would gain nothing. The `File` closes itself, which is what
/// makes "hold every ancestor" a `Vec` that simply stays in scope.
fn open_pinned(path: &Path) -> std::io::Result<File> {
    File::options()
        .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES)
        // Not `FILE_SHARE_DELETE`: that omission is what stops a held directory
        // being renamed away or deleted. It does *not* stop the directory being
        // converted into a junction in place — nothing can — which is why
        // `reach` looks at these handles a second time rather than trusting the
        // first look.
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// Open the leaf: the same no-follow open, plus the `DELETE` right that both
/// lets us delete it through this handle and stops anyone else removing its
/// name while we hold it.
fn open_leaf(path: &Path) -> std::io::Result<File> {
    File::options()
        .access_mode(DELETE | FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// Delete the object a handle names, with no path for anything to redirect.
///
/// The name goes as soon as the last handle to it closes. On a directory that
/// still holds anything this is refused with `ERROR_DIR_NOT_EMPTY`, which is
/// the property [`prune_at`] leans on; on a reparse point it removes the
/// reparse point and not what it points at.
fn dispose(handle: &File) -> std::io::Result<()> {
    let info = FILE_DISPOSITION_INFO {
        DeleteFile: true.into(),
    };
    unsafe {
        SetFileInformationByHandle(
            HANDLE(handle.as_raw_handle()),
            FileDispositionInfo,
            std::ptr::addr_of!(info).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    }
    // Taken straight after the failed call, so it is that call's error and
    // carries the raw code `explain` wants.
    .map_err(|_| std::io::Error::last_os_error())
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
        Some(code) if code == ERROR_DIR_NOT_EMPTY.0 => {
            format!("it still has something in it, so it would not {doing} (error {code})")
        }
        Some(code) => format!("it would not {doing} (error {code})"),
        None => format!("it would not {doing} ({e})"),
    }
}
#[cfg(test)]
mod tests {
    use super::super::test_support::{skip_even_on_ci, skip_or_fail_on_ci};
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
            // One rule for every self-skipping test in the crate, and it lives
            // in `test_support`.
            skip_or_fail_on_ci(&format!(
                "could not create a junction at {} ({made:?})",
                link.display()
            ));
            None
        }
    }

    impl Drop for Junction {
        fn drop(&mut self) {
            // `symlink_metadata`, not `exists`: `exists` follows the junction
            // and answers `false` for one whose target has gone, which is
            // exactly the dangling link that most needs removing. Gone
            // altogether is the ordinary case here — half of these tests assert
            // that the code under test unlinked it.
            if std::fs::symlink_metadata(&self.link).is_err() {
                return;
            }
            // `remove_dir` on a junction removes the link, never its target.
            if let Err(e) = std::fs::remove_dir(&self.link) {
                eprintln!("could not remove the junction {:?}: {e}", self.link);
            }
        }
    }

    /// `FSCTL_SET_REPARSE_POINT`, which turns a directory into a junction
    /// **in place** — no rename, no delete, the same object throughout. This is
    /// the move that breaks the obvious defence, so the tests build it for real
    /// rather than describe it.
    const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
    const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
    /// Takes no part in Windows' sharing check, so no share mode we could ask
    /// for can keep the attacker from getting a handle good enough for the
    /// FSCTL above. Measured in
    /// `a_directory_we_hold_can_still_be_turned_into_a_junction`.
    const FILE_WRITE_ATTRIBUTES: u32 = 0x0000_0100;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;

    // Declared here rather than taken from a `windows` crate feature: this is
    // the one call only the tests make, and it needs no feature the shipped
    // code would then carry.
    extern "system" {
        fn DeviceIoControl(
            device: *mut core::ffi::c_void,
            code: u32,
            in_buf: *const u8,
            in_len: u32,
            out_buf: *mut u8,
            out_len: u32,
            returned: *mut u32,
            overlapped: *mut core::ffi::c_void,
        ) -> i32;
    }

    /// A mount-point `REPARSE_DATA_BUFFER` aimed at `target`: the eight-byte
    /// header, the four `USHORT`s of the mount-point buffer, then the
    /// NT-style substitute name and an empty print name, each NUL-terminated.
    fn mount_point_buffer(target: &Path) -> Vec<u8> {
        let substitute: Vec<u16> = format!(r"\??\{}", target.display())
            .encode_utf16()
            .collect();
        let sub_bytes = (substitute.len() * 2) as u16;
        let data_len = 8 + sub_bytes + 2 + 2;

        let mut buf = Vec::new();
        buf.extend_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
        buf.extend_from_slice(&data_len.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // Reserved
        buf.extend_from_slice(&0u16.to_le_bytes()); // SubstituteNameOffset
        buf.extend_from_slice(&sub_bytes.to_le_bytes()); // SubstituteNameLength
        buf.extend_from_slice(&(sub_bytes + 2).to_le_bytes()); // PrintNameOffset
        buf.extend_from_slice(&0u16.to_le_bytes()); // PrintNameLength
        for unit in &substitute {
            buf.extend_from_slice(&unit.to_le_bytes());
        }
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf
    }

    /// Turn the empty directory `dir` into a junction pointing at `target`,
    /// the way the account's owner could at any moment. `Err` with a reason fit
    /// for a skip line.
    fn convert_to_junction(dir: &Path, target: &Path) -> Result<(), String> {
        let handle = File::options()
            // Attribute-only, deliberately: this is what makes the attack work
            // whatever share mode the walk holds the directory with.
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(dir)
            .map_err(|e| format!("could not open {} for the FSCTL ({e})", dir.display()))?;
        let buf = mount_point_buffer(target);
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                handle.as_raw_handle().cast(),
                FSCTL_SET_REPARSE_POINT,
                buf.as_ptr(),
                buf.len() as u32,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(format!(
                "FSCTL_SET_REPARSE_POINT on {} ({})",
                dir.display(),
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
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

    /// Half of what holding a directory buys: while it is held, nobody can
    /// rename it out of the way or delete it, so its name keeps naming it.
    ///
    /// This is also the test that settled the access right. On
    /// `FILE_READ_ATTRIBUTES` alone — which is all the metadata read needs —
    /// the rename below **succeeds**, because an attribute-only open takes no
    /// part in Windows' sharing check at all.
    #[test]
    fn a_held_directory_cannot_be_renamed_out_from_under_us() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let held_path = dir.path().join("AppData");
        std::fs::create_dir(&held_path).expect("a directory to hold");

        let held = super::open_pinned(&held_path).expect("open the directory");
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

    /// The other half, and the one that broke the first version of this module:
    /// holding a directory does **not** stop it becoming a junction. The
    /// conversion happens in place — no rename, no delete, the same object our
    /// handle refers to — and the handle then reports the reparse attribute,
    /// which is the only reason the walk can catch it at all.
    ///
    /// Also pins the two facts the defence is built on: the attacker's handle
    /// needs nothing but `FILE_WRITE_ATTRIBUTES`, which no share mode can
    /// refuse, and the directory has to be empty.
    #[test]
    fn a_directory_we_hold_can_still_be_turned_into_a_junction() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let victim = dir.path().join("victim");
        std::fs::create_dir(&victim).expect("the target");
        let held_path = dir.path().join("Temp");
        std::fs::create_dir(&held_path).expect("a directory to hold");

        let held = super::open_pinned(&held_path).expect("open the directory");
        assert_eq!(
            held.metadata().expect("attributes").file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT,
            0,
            "it starts out an ordinary directory"
        );

        if let Err(why) = convert_to_junction(&held_path, &victim) {
            skip_or_fail_on_ci(&why);
            return;
        }
        let _junction = Junction {
            link: held_path.clone(),
        };

        assert_ne!(
            held.metadata().expect("attributes").file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT,
            0,
            "the handle we have held all along must report what it has become — \
             without this there would be no way to catch the conversion"
        );

        // Before the guard above unlinks it: our own handle is the thing that
        // would stop it, since a held directory cannot be removed.
        drop(held);
    }

    /// And the fact that makes the defence work: a directory with anything in
    /// it cannot be converted, so holding the leaf is what freezes its parent.
    #[test]
    fn a_parent_whose_child_we_hold_cannot_be_converted() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let victim = dir.path().join("victim");
        std::fs::create_dir(&victim).expect("the target");
        let parent = dir.path().join("Temp");
        let child = parent.join("kuvatin");
        std::fs::create_dir_all(&child).expect("the tree");

        let held_parent = super::open_pinned(&parent).expect("hold the parent");
        let held_child = super::open_leaf(&child).expect("hold the child");

        let why = convert_to_junction(&parent, &victim)
            .expect_err("a non-empty directory must not convert");
        assert!(
            why.contains("(The directory is not empty"),
            "the refusal should be ERROR_DIR_NOT_EMPTY, got: {why}"
        );

        // …and it is our handle on the child that keeps it non-empty: nobody
        // can take the child's name away while we hold it.
        drop(held_parent);
        assert!(
            std::fs::remove_dir(&child).is_err(),
            "a leaf held without FILE_SHARE_DELETE cannot be removed, so its \
             parent cannot be emptied"
        );
        drop(held_child);
    }

    /// Installs the seam in [`super::MEDDLE`] for as long as it is alive, so a
    /// failed assertion cannot leave the hook armed for the next test on this
    /// thread.
    struct Meddler;

    impl Meddler {
        fn install(meddle: impl FnMut(&Path) + 'static) -> Self {
            super::MEDDLE.with(|slot| *slot.borrow_mut() = Some(Box::new(meddle)));
            Meddler
        }
    }

    impl Drop for Meddler {
        fn drop(&mut self) {
            super::MEDDLE.with(|slot| *slot.borrow_mut() = None);
        }
    }

    /// The whole module in one test: the owner converts the leaf's parent into
    /// a junction *after* the walk has vetted it, and the uninstall must not
    /// delete anything through it.
    ///
    /// This is the bypass that broke the first version. Holding every ancestor
    /// without `FILE_SHARE_DELETE` stops a rename and a delete, and stops
    /// neither an in-place conversion nor the emptying that enables it: the
    /// owner deletes the leaf — which the walk deliberately did not hold —
    /// leaving the parent empty, turns the parent into a junction aimed at
    /// somewhere else entirely, and the delete that follows resolves through
    /// it as SYSTEM. Against `c1062d7`'s logic this deletes `victim\kuvatin`.
    ///
    /// What has to stop it is the second look at the ancestors, through the
    /// handles held since the walk passed them.
    #[test]
    fn a_parent_converted_after_the_walk_deletes_nothing() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let profile = dir.path().join("profile");
        let local = local_in(&profile);
        let temp = local.join("Temp");
        let tree = temp.join("kuvatin");
        std::fs::create_dir_all(&tree).expect("the tree");
        std::fs::write(tree.join("ours.bin"), b"ours").expect("something of ours");

        // What the junction will point at, with the same leaf name inside it,
        // so the swapped-in path resolves and the delete would land.
        let victim = dir.path().join("victim");
        std::fs::create_dir_all(victim.join("kuvatin")).expect("the victim tree");
        let precious = victim.join("kuvatin").join("precious.txt");
        std::fs::write(&precious, b"precious").expect("bait");

        let converted = std::rc::Rc::new(std::cell::Cell::new(false));
        let armed = converted.clone();
        let temp_for_hook = temp.clone();
        let victim_for_hook = victim.clone();
        let _meddler = Meddler::install(move |leaf| {
            // Once: `reach` runs for every path in the plan.
            if armed.get() {
                return;
            }
            // Exactly what the account's owner can do, with no privilege:
            // take the leaf away, which empties the parent…
            if std::fs::remove_dir_all(leaf).is_err() {
                return;
            }
            // …then turn the parent into a junction, in place.
            if convert_to_junction(&temp_for_hook, &victim_for_hook).is_ok() {
                armed.set(true);
            }
        });

        let sweep = remove_plan(
            &profile,
            &FilePlan {
                trees: vec![tree.clone()],
                ..empty_plan()
            },
        );
        drop(_meddler);

        if !converted.get() {
            skip_or_fail_on_ci("could not convert the parent into a junction mid-walk");
            return;
        }
        let _junction = Junction { link: temp.clone() };

        // The one thing that must be true.
        assert_eq!(
            std::fs::read(&precious).expect("the victim's file survives"),
            b"precious",
            "SYSTEM deleted through a junction planted after the walk vetted it"
        );
        assert!(
            victim.join("kuvatin").is_dir(),
            "and the victim's folder itself must still be there"
        );

        assert_eq!(sweep.trees_removed, 0, "nothing was ours to remove");
        assert_eq!(sweep.trouble.len(), 1, "{:?}", sweep.trouble);
        let why = &sweep.trouble[0];
        assert!(
            why.contains("reparse point"),
            "the reason should say what was found: {why}"
        );
        assert!(
            why.contains("Temp"),
            "and name the component it was found on: {why}"
        );
    }

    /// A Deny ACE on a FILE, put on with `icacls` and taken off again — the
    /// same recipe `hive.rs` uses, because the `windows` crate features that
    /// could do it directly are ones this build does not otherwise want.
    struct DeniedFile {
        path: PathBuf,
        who: String,
    }

    impl DeniedFile {
        /// `None` when the ACE could not be set — no `icacls`, or no name to
        /// deny — which is a shortcoming of the machine and so a failure on CI.
        ///
        /// Setting it is not the same as its biting, and this walk has the
        /// second case the hive walk has: [`super::open_leaf`] opens with
        /// `FILE_FLAG_BACKUP_SEMANTICS`, which `SeBackupPrivilege` answers by
        /// granting a backup-intent handle over the top of any DACL. The
        /// caller has to measure that for itself; see
        /// `a_file_we_may_not_delete_says_so_by_name`.
        fn new(path: &Path) -> Option<Self> {
            let who = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
                (Ok(domain), Ok(user)) => format!(r"{domain}\{user}"),
                (_, Ok(user)) => user,
                _ => {
                    skip_or_fail_on_ci("no USERNAME to deny");
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
                    skip_or_fail_on_ci(&format!("could not deny access to {path:?}: {other:?}"));
                    None
                }
            }
        }
    }

    impl Drop for DeniedFile {
        fn drop(&mut self) {
            // `/remove:d` takes off *every* deny entry this principal has on
            // the file, not only the one we added — right for a file we made in
            // a temp directory moments ago, wrong for anything we did not.
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
        // Setting the ACE is not the same as its biting. `open_leaf` is the
        // call that refuses here — it is what `reach` reaches the file with,
        // and the delete happens through the handle it returns — so probe with
        // exactly that and with nothing else: `File::open` would omit
        // `FILE_FLAG_BACKUP_SEMANTICS` and report a deny as biting when it does
        // not. `SeBackupPrivilege` grants a backup-intent open over any DACL,
        // and this process may hold it: the offline hive test enables it for
        // the whole process, and thread order decides whether that has happened
        // before we get here. An unelevated token cannot hold the privilege at
        // all, so an unelevated success is a broken ACE and stays a failure.
        if super::open_leaf(&log).is_ok() && super::super::hive::is_elevated() {
            skip_even_on_ci("SeBackupPrivilege overrides the deny in this process");
            return;
        }
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

    /// The profile directory itself has no ancestor to pin it against, and
    /// removing it would take the whole account. Refused by name.
    #[test]
    fn the_profile_directory_itself_is_never_the_target() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let profile = dir.path().join("profile");
        std::fs::create_dir(&profile).expect("the profile");
        std::fs::write(profile.join("keep.txt"), b"precious").expect("bait");

        let sweep = remove_plan(
            &profile,
            &FilePlan {
                trees: vec![profile.clone()],
                ..empty_plan()
            },
        );

        assert!(profile.join("keep.txt").exists(), "the profile survives");
        assert_eq!(sweep.trees_removed, 0);
        assert_eq!(sweep.trouble.len(), 1, "{:?}", sweep.trouble);
        assert!(
            sweep.trouble[0].contains("the profile directory itself"),
            "{:?}",
            sweep.trouble
        );
    }

    /// A junction where the folder to prune should be is refused, not removed
    /// and certainly not followed. `Local\Kuvatin` is the one path the
    /// `is_protected` guard deliberately does not cover, because pruning it is
    /// the point — so the reparse check is all there is here.
    #[test]
    fn a_junction_where_the_prune_target_should_be_is_refused() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let profile = dir.path().join("profile");
        let local = local_in(&profile);

        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).expect("the junction's target");
        let bait = elsewhere.join("precious.txt");
        std::fs::write(&bait, b"precious").expect("bait");

        let link = local.join("Kuvatin");
        let Some(_junction) = Junction::new(&link, &elsewhere) else {
            return;
        };

        let sweep = remove_plan(
            &profile,
            &FilePlan {
                prune_if_empty: vec![link.clone()],
                ..empty_plan()
            },
        );

        assert_eq!(sweep.pruned, 0);
        assert_eq!(sweep.trouble.len(), 1, "{:?}", sweep.trouble);
        assert!(
            sweep.trouble[0].contains("reparse point"),
            "{:?}",
            sweep.trouble
        );
        assert_eq!(
            std::fs::read(&bait).expect("the bait survives"),
            b"precious",
            "the junction's target must survive"
        );
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "and the planted entry is left alone rather than quietly unlinked"
        );
    }

    /// A folder where one of the named files should be is reported for what it
    /// is, not as "we are not allowed to delete it" — which is what the raw
    /// error would have said, because `DeleteFileW` answers a directory with
    /// error 5. The mirror of `remove_tree_at`'s "a file where a folder of ours
    /// would be".
    #[test]
    fn a_folder_where_a_file_should_be_says_what_it_is() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let profile = dir.path();
        let local = local_in(profile);
        let kuvatin = local.join("Kuvatin");
        let log = kuvatin.join("kuvatin.log");
        std::fs::create_dir_all(&log).expect("a folder where the log would be");

        let sweep = remove_plan(
            profile,
            &FilePlan {
                files: vec![log.clone()],
                ..empty_plan()
            },
        );

        assert!(log.is_dir(), "it is left alone");
        assert_eq!(sweep.files_removed, 0);
        assert_eq!(sweep.trouble.len(), 1, "{:?}", sweep.trouble);
        let why = &sweep.trouble[0];
        assert!(
            why.contains("folder where a file of ours would be"),
            "the reason should say what it found, not just quote an error: {why}"
        );
        assert!(!why.contains("error 5"), "{why}");
    }

    /// And the same the other way for a prune target.
    #[test]
    fn a_file_where_the_prune_target_should_be_says_what_it_is() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let profile = dir.path();
        let local = local_in(profile);
        let kuvatin = local.join("Kuvatin");
        std::fs::write(&kuvatin, b"not a folder").expect("a file in its place");

        let sweep = remove_plan(
            profile,
            &FilePlan {
                prune_if_empty: vec![kuvatin.clone()],
                ..empty_plan()
            },
        );

        assert!(kuvatin.is_file(), "it is left alone");
        assert_eq!(sweep.pruned, 0);
        assert_eq!(sweep.trouble.len(), 1, "{:?}", sweep.trouble);
        assert!(
            sweep.trouble[0].contains("file where a folder of ours would be"),
            "{:?}",
            sweep.trouble
        );
    }

    /// A directory on the way down that is not a directory names both itself
    /// and what it was blocking, the way the reparse refusal does — one line
    /// that says the whole story rather than half of it.
    #[test]
    fn a_blocked_path_names_the_target_it_was_blocking() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let profile = dir.path();
        let local = local_in(profile);
        std::fs::write(local.join("Temp"), b"not a folder").expect("a file in its place");

        let sweep = remove_plan(
            profile,
            &FilePlan {
                trees: vec![local.join("Temp").join("kuvatin")],
                ..empty_plan()
            },
        );

        assert_eq!(sweep.trouble.len(), 1, "{:?}", sweep.trouble);
        let why = &sweep.trouble[0];
        assert!(why.contains("not a directory"), "{why}");
        assert!(
            why.contains("kuvatin"),
            "the path that will not be deleted should be named too: {why}"
        );
    }
}
