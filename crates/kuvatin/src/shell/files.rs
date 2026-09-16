//! Carry out one profile's [`FilePlan`] as SYSTEM, without ever deleting
//! through a reparse point.
//!
//! **What is trusted.** `profiles::vet_dir` has established that the profile
//! directory itself is a real, unredirected directory, and that one path is the
//! only thing here opened by name. Nothing below it is vouched for by anybody:
//! every folder inside a profile belongs to that account, a directory junction
//! needs no privilege at all, and the account can be signed in and working
//! while the uninstall runs — so its owner can aim `AppData`, `Local`, `Temp`,
//! `Kuvatin`, `Packages`, or a `VilleMattila.Kuvatin_*` entry of their own
//! making, at anywhere on the machine, and can do it between any two of our
//! statements. A SYSTEM delete that followed one of those is an
//! arbitrary-delete primitive against the whole machine. `super::paths` mints
//! names and vets none of them; this is where the names become deletions, so
//! this is where the vetting is.
//!
//! # Why checking a name is never enough
//!
//! Two earlier versions of this module were broken, and both by the same
//! mistake in different clothes: they resolved a path by name, and then
//! resolved it again.
//!
//! The first held every directory open without `FILE_SHARE_DELETE`, reasoning
//! that a name which cannot be renamed or deleted stays put.
//! `FSCTL_SET_REPARSE_POINT` converts a directory into a junction **in place** —
//! same object, same handle, no rename and no delete. It wants only an empty
//! directory and a handle with write access, and `FILE_WRITE_ATTRIBUTES`
//! counts, which takes no part in Windows' sharing check at all, so no share
//! mode can refuse it. (`a_directory_we_hold_can_still_be_turned_into_a_junction`.)
//!
//! The second added a second look: open the leaf, then re-read every ancestor
//! through the handle held since the walk passed it, and refuse any that had
//! become a reparse point. A point-in-time check is not a binding. The owner
//! converts the parent, our open resolves *by name* through the junction and
//! lands on the victim's file, and then the owner puts the parent back with
//! `FSCTL_DELETE_REPARSE_POINT` — they can see the exact instant to do it,
//! because our open denies `FILE_SHARE_DELETE` and theirs starts failing with
//! `ERROR_SHARING_VIOLATION`. The re-read then sees an ordinary directory and
//! waves it through, and the handle we are about to delete through is the
//! victim's. An oplock on the victim's file can hold our open open for as long
//! as the owner likes, so being quick is no defence either.
//! (`a_parent_converted_and_reverted_around_the_leaf_open_deletes_nothing`.)
//!
//! # What this does instead
//!
//! It never resolves a name below the profile root a second time. The profile
//! root is opened by path, once, because it is the one path that has been
//! vetted. Every component after it is opened **relative to its parent's own
//! handle** — [`open_relative`], which is `NtOpenFile` with the parent handle
//! as `RootDirectory`, a single-component `UNICODE_STRING` as `ObjectName`, and
//! `OBJ_DONT_REPARSE`. There is no path for anything to redirect, because there
//! is no path: the kernel looks the name up inside the object we are holding.
//!
//! That turns the whole class of attack from something to detect into something
//! that cannot resolve. If the owner converts a parent we hold, a relative open
//! inside it does not reach a victim — it fails, with
//! `STATUS_REPARSE_POINT_ENCOUNTERED` or with the name simply not being there.
//! Failure is closed.
//!
//! And it makes the induction true rather than hopeful. Each parent is held
//! while its child is opened, and a directory with anything in it cannot be
//! converted (`ERROR_DIR_NOT_EMPTY` —
//! `a_parent_whose_child_we_hold_cannot_be_converted`); a held child's name
//! cannot be taken away, because removing it needs `DELETE` and we hold it
//! without `FILE_SHARE_DELETE`, POSIX-semantics deletes included. So from the
//! leaf upwards every directory in the chain is permanently non-empty, and
//! every one of them is the real object inside its real parent.
//!
//! [`reach`] still re-reads the ancestors at the end. That is belt and braces
//! over the induction above, not the thing that makes this safe — the argument
//! does not lean on it, and the tests that matter would still pass without it.
//!
//! # What the deletes then do
//!
//! A file, a prune, and a junction standing where one of our folders should be
//! are all deleted **through the handle** — `SetFileInformationByHandle` with
//! `FILE_DISPOSITION_INFO` — so again no name is resolved and the object
//! removed is exactly the object vetted. The junction case never descends: the
//! entry is what we delete, and whatever it points at is not ours to look at.
//!
//! A tree is handed to `std::fs::remove_dir_all`, which does take a path, and
//! that is safe here for a reason worth spelling out. We are holding the real
//! tree root, reached relatively, without `FILE_SHARE_DELETE`: so its name
//! cannot be moved out of its parent, so that parent is permanently non-empty
//! and cannot be converted, and the same holds all the way up to the profile
//! root. Every component std walks is therefore an object that cannot change
//! identity while we hold the chain. Below the root std never uses names at all
//! — it descends by handle through `NtOpenFile`, which is the same defence by
//! the same means, and the evidence is quoted at the call. If a junction did
//! somehow appear at the root's own name, std opens it with
//! `FILE_FLAG_OPEN_REPARSE_POINT` and unlinks it rather than following it.
//!
//! std cannot take the last step, deleting the root itself, because we are
//! holding it without `FILE_SHARE_DELETE` — that is what pinned the parent. So
//! it empties the tree and stops with a sharing violation that is us, and the
//! root goes through our own handle like everything else.
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
//! A file or folder another process holds open in a way that refuses our open
//! is reported and skipped, which any account can arrange deliberately: it
//! costs that account its own leftovers, and the line naming the path is the
//! whole of the damage. It is a nuisance, not an escalation.
//!
//! The disposition delete is not POSIX: the name goes when the last handle to
//! it closes, so a file something else still has open counts in `files_removed`
//! and disappears later. A prune that follows in the same sweep can then find
//! the folder still not empty and leave it, silently and by design — a folder
//! we did not remove is never worse than one we did.
//!
//! This module **reports**; it never prints and never logs. It runs as SYSTEM,
//! where `crate::applog` would resolve `%LOCALAPPDATA%` to the system profile
//! and leave a brand-new file behind — exactly the sort of leftover the
//! all-users uninstall exists to remove. The orchestrator prints what comes
//! back here to stdout.
//!
//! `--unregister-all-users` calls everything here.

use std::ffi::OsStr;
use std::fs::File;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Component, Path, PathBuf};
use windows::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows::Wdk::Storage::FileSystem::{
    NtOpenFile, FILE_DIRECTORY_FILE, FILE_OPEN_FOR_BACKUP_INTENT, FILE_OPEN_REPARSE_POINT,
    FILE_SYNCHRONOUS_IO_NONALERT, NTCREATEFILE_CREATE_OPTIONS,
};
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_DIR_NOT_EMPTY, ERROR_SHARING_VIOLATION, HANDLE, NTSTATUS,
    STATUS_ACCESS_DENIED, STATUS_DELETE_PENDING, STATUS_INVALID_PARAMETER,
    STATUS_IO_REPARSE_TAG_NOT_HANDLED, STATUS_NOT_A_DIRECTORY, STATUS_OBJECT_NAME_NOT_FOUND,
    STATUS_OBJECT_PATH_NOT_FOUND, STATUS_REPARSE_POINT_ENCOUNTERED,
    STATUS_REPARSE_POINT_NOT_RESOLVED, STATUS_SHARING_VIOLATION, UNICODE_STRING,
};
use windows::Win32::Storage::FileSystem::{
    FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO,
};
use windows::Win32::System::Kernel::{OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE};
use windows::Win32::System::IO::IO_STATUS_BLOCK;

use super::paths::{self, FilePlan};
use super::profiles::FILE_ATTRIBUTE_REPARSE_POINT;

/// Spelled out because the `windows` crate exports these from namespaces this
/// module does not otherwise need, and because the `NtOpenFile` side wants the
/// same numbers the `CreateFileW` side does.
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
/// `FILE_READ_DATA` on a file, `FILE_LIST_DIRECTORY` on a directory.
///
/// `FILE_READ_ATTRIBUTES` alone is all the metadata read needs, and it was the
/// first thing tried — but an attribute-only open takes no part in Windows'
/// sharing check, so a handle held that way pins nothing at all. Measured, not
/// assumed: `a_held_directory_cannot_be_renamed_out_from_under_us` fails on
/// `FILE_READ_ATTRIBUTES` by itself.
const FILE_LIST_DIRECTORY: u32 = 0x0000_0001;
const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
/// Wanted by `FILE_SYNCHRONOUS_IO_NONALERT`, which is what makes the handle an
/// ordinary synchronous one of the sort `std::fs::File` expects.
const SYNCHRONIZE: u32 = 0x0010_0000;
/// The leaf needs this so the disposition below can be set on it, and holding
/// it is also what stops anyone else removing the leaf's name.
const DELETE: u32 = 0x0001_0000;
const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
/// Without this, `CreateFileW` will not open a directory at all.
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
/// The handle is the entry itself, never what it points at.
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// What every relative open asks for. Read the name, read the attributes, and
/// be a synchronous handle.
const WALK_ACCESS: u32 = FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
/// Read and write stay shared so that holding a signed-in account's folders
/// open does not disturb it. `FILE_SHARE_DELETE` is left out on purpose: it is
/// what stops a held name being taken out of its parent, which is what keeps
/// that parent non-empty and so unconvertible.
const WALK_SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;

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
            let shown = held.path.display();
            if held.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                sweep.trouble.push(format!(
                    "{shown}: it is a reparse point, not a file this uninstall wrote; leaving it \
                     alone"
                ));
                return;
            }
            if held.attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
                sweep.trouble.push(format!(
                    "{shown}: it is a folder where a file of ours would be, so it is not ours to \
                     delete"
                ));
                return;
            }
            match dispose(&held.leaf) {
                Ok(()) => sweep.files_removed += 1,
                Err(e) => sweep
                    .trouble
                    .push(format!("{shown}: {}", explain("delete", &e))),
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
            let shown = held.path.display().to_string();
            if held.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                // A junction where our folder should be — a
                // `Packages\VilleMattila.Kuvatin_…` entry the account planted
                // and aimed elsewhere. We delete the entry through its own
                // handle and never descend: what it points at is not ours to
                // look at, and a disposition on a reparse point removes the
                // reparse point.
                match dispose(&held.leaf) {
                    Ok(()) => sweep.trees_removed += 1,
                    Err(e) => sweep
                        .trouble
                        .push(format!("{shown}: {}", explain("unlink", &e))),
                }
                return;
            }
            if held.attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
                sweep.trouble.push(format!(
                    "{shown}: it is a file where a folder of ours would be, so it is not ours to \
                     delete"
                ));
                return;
            }
            // The path this walk built component by component, not the
            // spelling the plan happened to use — they name the same thing, and
            // this is the one we have actually opened every step of.
            //
            // Pointing a path-taking call at a folder inside a hostile profile
            // is safe here only because of what we are holding; the argument is
            // in this module's documentation and it is not a short one. What
            // std does below that root is the other half, and this is the
            // evidence, read out of the std source for the toolchain this
            // builds with (rustc 1.96.0):
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
            //    symlink" — plus `FILE_OPEN_REPARSE_POINT`. That is the same
            //    shape as [`open_relative`] here, for the same reason; its
            //    module documentation names it: "It must not be possible to
            //    trick this into deleting files outside of the parent directory
            //    (see CVE-2022-21658)."
            //
            // One caveat that comes with quoting it: std applies
            // `OBJ_DONT_REPARSE` best-effort. `remove_dir_all.rs:90` holds it
            // in a static that `103-112` clears for the rest of the process the
            // first time `NtOpenFile` answers `INVALID_PARAMETER`, "Retry
            // without OBJ_DONT_REPARSE if it's not supported" — on a Windows
            // too old for it, which is well before any build this ships to.
            // `FILE_OPEN_REPARSE_POINT` is passed unconditionally either way,
            // so the open still takes the link rather than its target.
            // [`open_relative`] makes the other choice and refuses, since we
            // require Windows 10 and have no older case to be kind to.
            //
            // So a junction *inside* the tree is opened as the link it is and
            // unlinked, never descended into.
            // `a_junction_nested_inside_a_tree_is_unlinked_with_the_tree` is
            // the measurement of that, kept so that a toolchain which ever
            // changed it would fail a test here rather than delete somebody's
            // files.
            let emptied = std::fs::remove_dir_all(&held.path);
            // And the root itself through our own handle, because std cannot:
            // we are holding it without `FILE_SHARE_DELETE`, which is what
            // pinned this path in the first place. Its answer is the one that
            // decides.
            match dispose(&held.leaf) {
                Ok(()) => sweep.trees_removed += 1,
                Err(e) => {
                    sweep
                        .trouble
                        .push(format!("{shown}: {}", why_the_tree_stayed(&emptied, &e)));
                }
            }
        }
    }
}

/// Why a tree would not go, given what `remove_dir_all` said and what the
/// disposition said.
///
/// The sharing violation `remove_dir_all` hands back on the ordinary path is
/// *us*, holding the root so that nothing could move it, and saying so would
/// be a confession of the design rather than a fault to report. Anything else
/// it says is a real reason the tree did not empty, and when the disposition
/// then refuses for want of an empty directory, that reason is the useful half.
fn why_the_tree_stayed(emptied: &std::io::Result<()>, disposed: &std::io::Error) -> String {
    let ours = matches!(
        emptied.as_ref().err().and_then(|e| e.raw_os_error()),
        Some(code) if code == ERROR_SHARING_VIOLATION.0 as i32
    );
    match emptied {
        Err(first) if !ours && disposed.raw_os_error() == Some(ERROR_DIR_NOT_EMPTY.0 as i32) => {
            format!(
                "{}; {}",
                explain("empty", first),
                explain("remove", disposed)
            )
        }
        _ => explain("remove", disposed),
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
            let shown = held.path.display();
            if held.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                sweep.trouble.push(format!(
                    "{shown}: it is a reparse point, not a folder of ours to prune"
                ));
                return;
            }
            if held.attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
                sweep.trouble.push(format!(
                    "{shown}: it is a file where a folder of ours would be, so it is not ours to \
                     prune"
                ));
                return;
            }
            match dispose(&held.leaf) {
                Ok(()) => sweep.pruned += 1,
                // The user still keeps something of their own in it. That is
                // the ordinary case and the whole reason this is a prune.
                Err(e) if e.raw_os_error() == Some(ERROR_DIR_NOT_EMPTY.0 as i32) => {}
                Err(e) => sweep
                    .trouble
                    .push(format!("{shown}: {}", explain("remove", &e))),
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

/// A leaf whose whole path has been walked relatively and is now held.
///
/// The `ancestors` pin every component above the leaf and must outlive the
/// delete; `leaf` is the handle the delete goes through, and holding it is what
/// keeps the leaf's parent non-empty and so unconvertible. `path` is the path
/// this walk built, which is the one to name in any message and the only one to
/// hand to a path-taking call.
struct Held {
    #[allow(dead_code)] // Never read: held open is all these handles are for.
    ancestors: Vec<(PathBuf, File)>,
    leaf: File,
    attributes: u32,
    path: PathBuf,
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

/// Walk from `profile` down to `target`, opening each component relative to the
/// last, and hand back a leaf nothing can have substituted.
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
        // them is what keeps that true if it ever changes. A relative open
        // takes one component, so this is also what guarantees it gets one.
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

    // The one open by name, and the one that is allowed to be: this is the path
    // `profiles::vet_dir` vetted. Everything below it is reached from a handle.
    let mut here = profile.to_path_buf();
    let root = match open_root(profile) {
        Ok(root) => root,
        // The profile directory has gone between `profiles::vet_dir` vouching
        // for it and this walk starting. That is not the same as a profile with
        // nothing of ours left in it: one is an account this uninstall did not
        // clean, the other an account that was already clean, and counting the
        // first as "already gone" is the one place in this walk where a
        // disappearance would read as an absence.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Reached::Refused(format!(
                "{}: the profile directory is no longer there, so nothing of this account's was \
                 removed",
                here.display()
            ))
        }
        Err(e) => return Reached::Refused(format!("{}: {}", here.display(), explain("open", &e))),
    };
    match root.metadata().map(|m| m.file_attributes()) {
        Ok(attributes) if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 => {
            return Reached::Refused(reparse_on_the_way(&here, target))
        }
        Ok(attributes) if attributes & FILE_ATTRIBUTE_DIRECTORY == 0 => {
            return Reached::Refused(format!(
                "{}: it is not a directory, so {} is not there to delete",
                here.display(),
                target.display()
            ))
        }
        Ok(_) => {}
        Err(e) => return Reached::Refused(format!("{}: {}", here.display(), explain("read", &e))),
    }

    let mut ancestors: Vec<(PathBuf, File)> = vec![(here.clone(), root)];
    for name in ancestor_names {
        let parent = &ancestors.last().expect("the root is always there").1;
        here.push(name);
        // A directory is required on the way down, so ask the kernel for one
        // and let it refuse anything else.
        let opened = match open_relative(parent, name, WALK_ACCESS, WALK_SHARE, walk_options(true))
        {
            Ok(opened) => opened,
            Err(status) if is_absent(status) => return Reached::Absent,
            // `FILE_DIRECTORY_FILE` is what refuses this, so say what it means
            // and name what it was blocking, the way the reparse refusal does.
            Err(status) if status == STATUS_NOT_A_DIRECTORY => {
                return Reached::Refused(format!(
                    "{}: it is not a directory, so {} is not there to delete",
                    here.display(),
                    target.display()
                ))
            }
            // The defence firing: the account has aimed a directory we are
            // inside somewhere else, and the open refused rather than followed.
            Err(status) if is_reparse_refusal(status) => {
                return Reached::Refused(reparse_on_the_way(&here, target))
            }
            Err(status) => {
                return Reached::Refused(format!(
                    "{}: {}",
                    here.display(),
                    explain_status("open", status)
                ))
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
        ancestors.push((here.clone(), opened));
    }

    here.push(leaf_name);
    // The seam the regression tests stand in. Nothing it does can matter any
    // more — that is the property under test.
    #[cfg(test)]
    meddle(Meddle::BeforeLeafOpen, &here);

    let parent = &ancestors.last().expect("the root is always there").1;
    // The leaf, relative to its parent's handle like everything else, with
    // `DELETE` and no `FILE_SHARE_DELETE`: from here its name cannot be taken
    // out of its parent, so the parent cannot be emptied, so the parent cannot
    // be turned into a junction.
    let leaf = match open_relative(
        parent,
        leaf_name,
        WALK_ACCESS | DELETE,
        WALK_SHARE,
        walk_options(false),
    ) {
        Ok(leaf) => leaf,
        Err(status) if is_absent(status) => return Reached::Absent,
        // The parent has been aimed elsewhere since we opened it. The open
        // refused rather than resolving into whatever it now points at, which
        // is the whole of the defence — so name the parent, not the leaf.
        Err(status) if is_reparse_refusal(status) => {
            let parent = here.parent().unwrap_or(&here);
            return Reached::Refused(reparse_on_the_way(parent, &here));
        }
        Err(status) => {
            return Reached::Refused(format!(
                "{}: {}",
                here.display(),
                explain_status("open", status)
            ))
        }
    };

    // The second half of the attack the old walk fell to: the owner puts the
    // parent back here, so that anything looking again sees a plain directory.
    // On this walk the open above has already refused, so nothing gets here
    // with a redirected handle to put right.
    #[cfg(test)]
    meddle(Meddle::AfterLeafOpen, &here);

    // Belt and braces, not the defence. Every open above went through a handle
    // rather than a name, so an ancestor that had been converted could not have
    // redirected us — the open inside it would have failed instead. This costs
    // one query per level and would have to be wrong for the argument in the
    // module documentation to be wrong, so it stays as a second opinion.
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

    let attributes = match leaf.metadata() {
        Ok(meta) => meta.file_attributes(),
        Err(e) => return Reached::Refused(format!("{}: {}", here.display(), explain("read", &e))),
    };
    // The last moment anything could be aimed elsewhere: `remove_tree_at` still
    // hands a path to `remove_dir_all` after this. What keeps that safe is the
    // held chain, not the checks above — see this module's documentation.
    #[cfg(test)]
    meddle(Meddle::BeforeDelete, &here);
    Reached::Leaf(Held {
        ancestors,
        leaf,
        attributes,
        path: here,
    })
}

/// Why a component we met on the way down stops the whole path.
fn reparse_on_the_way(here: &Path, target: &Path) -> String {
    format!(
        "{}: it is a reparse point, so {} is not being deleted through it — a junction there \
         needs no privilege to make and this runs as SYSTEM",
        here.display(),
        target.display()
    )
}

/// The create options every step of the walk uses. `directory_required` is for
/// the components above the leaf, which must be directories; the leaf may be
/// either and is judged afterwards.
fn walk_options(directory_required: bool) -> NTCREATEFILE_CREATE_OPTIONS {
    let base =
        FILE_OPEN_REPARSE_POINT.0 | FILE_OPEN_FOR_BACKUP_INTENT.0 | FILE_SYNCHRONOUS_IO_NONALERT.0;
    NTCREATEFILE_CREATE_OPTIONS(if directory_required {
        base | FILE_DIRECTORY_FILE.0
    } else {
        base
    })
}

/// Whether a status means "there is nothing by that name", which is an ordinary
/// second uninstall rather than anything to report.
fn is_absent(status: NTSTATUS) -> bool {
    status == STATUS_OBJECT_NAME_NOT_FOUND || status == STATUS_OBJECT_PATH_NOT_FOUND
}

/// Whether a status is `OBJ_DONT_REPARSE` refusing to resolve a name through a
/// reparse point — which is the whole defence firing, and the ordinary answer
/// when the account has aimed a directory we are inside somewhere else.
///
/// `STATUS_REPARSE_POINT_NOT_RESOLVED` is the one measured in practice; the
/// other two are the neighbouring ways the same refusal is spelled, kept so a
/// different Windows saying one of them is still read as a refusal and not as
/// some unexplained error.
fn is_reparse_refusal(status: NTSTATUS) -> bool {
    status == STATUS_REPARSE_POINT_NOT_RESOLVED
        || status == STATUS_REPARSE_POINT_ENCOUNTERED
        || status == STATUS_IO_REPARSE_TAG_NOT_HANDLED
}

/// Open the profile directory by path. The only name this module resolves.
fn open_root(profile: &Path) -> std::io::Result<File> {
    File::options()
        .access_mode(WALK_ACCESS)
        .share_mode(WALK_SHARE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(profile)
}

/// Open `name` inside the directory `parent` holds, without resolving a path.
///
/// This is `open_link_no_reparse` from
/// `library/std/src/sys/fs/windows/remove_dir_all.rs` in miniature, and for the
/// same reason: `NtOpenFile` is the only way to open a child relative to a
/// parent *handle*, so it is the only way to look a name up inside an object we
/// are already holding rather than by walking a path from the volume root. The
/// caller guarantees `name` is a single plain component (`reach` refuses
/// anything else), so nothing here can traverse.
///
/// `OBJ_DONT_REPARSE` refuses the open outright if resolving the name would go
/// through a reparse point. std retries without it on systems too old to know
/// it, answering `STATUS_INVALID_PARAMETER`; this does not, because Kuvatin
/// needs Windows 10 and a walk that silently dropped its guard would be worse
/// than one that stops. `FILE_OPEN_REPARSE_POINT` is separate and means the
/// *last* component, if it is itself a link, is opened as the link — which is
/// exactly what a junction standing where our folder should be needs.
fn open_relative(
    parent: &File,
    name: &OsStr,
    access: u32,
    share: u32,
    options: NTCREATEFILE_CREATE_OPTIONS,
) -> Result<File, NTSTATUS> {
    let wide: Vec<u16> = name.encode_wide().collect();
    // A `UNICODE_STRING` counts bytes in a `u16`. Nothing NTFS can name comes
    // close, but the cast has to be safe rather than probably safe.
    let Ok(bytes) = u16::try_from(wide.len() * 2) else {
        return Err(STATUS_OBJECT_NAME_NOT_FOUND);
    };
    let unicode = UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: windows::core::PWSTR(wide.as_ptr() as *mut u16),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: HANDLE(parent.as_raw_handle()),
        ObjectName: &unicode,
        Attributes: (OBJ_DONT_REPARSE | OBJ_CASE_INSENSITIVE) as u32,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let mut handle = HANDLE::default();
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: `attributes` and `unicode` outlive the call, and `wide` outlives
    // `unicode`, so `ObjectName` and its `Buffer` are valid for its duration.
    // `RootDirectory` borrows a live `File`. `handle` and `status_block` are
    // owned here and written only on success. On success the handle is ours
    // alone and is handed straight to `File`, which closes it.
    let status = unsafe {
        NtOpenFile(
            &mut handle,
            access,
            &attributes,
            &mut status_block,
            share,
            options.0,
        )
    };
    if status.is_ok() {
        // SAFETY: `NtOpenFile` succeeded, so `handle` is a fresh open handle
        // that nothing else owns.
        Ok(unsafe { File::from_raw_handle(handle.0) })
    } else {
        Err(status)
    }
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
    // SAFETY: `info` is a live `FILE_DISPOSITION_INFO` and the size passed is
    // its own; the handle belongs to the borrowed `File`.
    unsafe {
        SetFileInformationByHandle(
            HANDLE(handle.as_raw_handle()),
            FileDispositionInfo,
            std::ptr::addr_of!(info).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    }
    .map_err(from_windows)
}

/// A `windows` error carries the Win32 code inside an `HRESULT`; take it back
/// out, so `explain` sees the number the API actually returned rather than a
/// second-hand one.
fn from_windows(e: windows::core::Error) -> std::io::Error {
    let hr = e.code().0 as u32;
    if hr & 0xFFFF_0000 == 0x8007_0000 {
        std::io::Error::from_raw_os_error((hr & 0xFFFF) as i32)
    } else {
        std::io::Error::other(e)
    }
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

/// The same for the status a relative open came back with. `regutil` spells its
/// statuses out the same way, for the same reason: the number alone tells a log
/// reader nothing, and the words alone leave them nothing to search for.
fn explain_status(doing: &str, status: NTSTATUS) -> String {
    let code = format!("status {:#010x}", status.0 as u32);
    if status == STATUS_ACCESS_DENIED {
        format!("we are not allowed to {doing} it ({code})")
    } else if status == STATUS_SHARING_VIOLATION {
        format!("something else has it open, so it would not {doing} ({code})")
    } else if is_reparse_refusal(status) {
        format!(
            "it is a reparse point, and this walk never opens through one, so it would not \
             {doing} ({code})"
        )
    } else if status == STATUS_NOT_A_DIRECTORY {
        format!("it is not a directory, so it would not {doing} ({code})")
    } else if status == STATUS_DELETE_PENDING {
        format!("it is already on its way out, so it would not {doing} ({code})")
    } else if status == STATUS_INVALID_PARAMETER {
        format!(
            "it would not {doing} ({code}); on a Windows too old for OBJ_DONT_REPARSE this is \
             what that looks like, and this walk will not open without it"
        )
    } else {
        format!("it would not {doing} ({code})")
    }
}

// The seam the regression tests stand in, and nothing else uses.
//
// Two moments, because the attack it has to reproduce takes two: the owner
// converts the leaf's parent *before* the leaf is opened, and puts it back
// *after*, so that anything looking a second time sees an ordinary directory.
// A test that did both before the open would prove nothing, because the
// redirection would be gone by the time the open ran.
//
// On this walk neither moment can matter: the leaf is opened through its
// parent's handle rather than through a name, so a junction on the parent
// cannot send the open anywhere — it fails instead. Proving exactly that is
// what the seam is for.
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Meddle {
    /// The walk has the leaf's name and has not opened it yet.
    BeforeLeafOpen,
    /// The leaf handle is open and nothing has been deleted through it.
    AfterLeafOpen,
    /// The walk has finished and said yes, and the caller is about to delete.
    /// The tree branch still has a path to resolve after this, which is the
    /// last thing an attacker could aim somewhere else.
    BeforeDelete,
}

#[cfg(test)]
type Meddling = std::cell::RefCell<Option<Box<dyn FnMut(Meddle, &Path)>>>;

#[cfg(test)]
thread_local! {
    static MEDDLE: Meddling = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn meddle(when: Meddle, leaf: &Path) {
    MEDDLE.with(|slot| {
        if let Some(meddle) = slot.borrow_mut().as_mut() {
            meddle(when, leaf);
        }
    });
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
    /// And the undo, which is what makes a point-in-time check worthless: the
    /// owner can put the directory back before anyone looks again.
    const FSCTL_DELETE_REPARSE_POINT: u32 = 0x0009_00AC;
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

    /// Put the directory back: remove the reparse point and leave an ordinary
    /// empty directory where it stood, with nothing to show it was ever
    /// anything else.
    ///
    /// This is the half that defeats looking twice — and it is why the walk no
    /// longer looks twice for its safety.
    fn revert_junction(dir: &Path) -> Result<(), String> {
        let handle = File::options()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(dir)
            .map_err(|e| format!("could not open {} to revert ({e})", dir.display()))?;
        // Deleting one wants only the header and the tag: eight bytes, with the
        // data length zero.
        let mut buf = Vec::new();
        buf.extend_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                handle.as_raw_handle().cast(),
                FSCTL_DELETE_REPARSE_POINT,
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
                "FSCTL_DELETE_REPARSE_POINT on {} ({})",
                dir.display(),
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    /// Whether anybody holds `path` open: opens it for `DELETE` sharing
    /// nothing, which succeeds only when no other handle exists.
    ///
    /// The walk holds every leaf it reaches without `FILE_SHARE_DELETE`, so
    /// "nobody holds the victim's file" is the same statement as "the walk
    /// never opened the victim's file".
    fn nobody_holds(path: &Path) -> bool {
        File::options()
            .access_mode(super::DELETE)
            .share_mode(0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .is_ok()
    }

    /// Hold a directory the way the walk holds its ancestors. By path, which
    /// only the profile root is in earnest — here it is just a short way to get
    /// a handle with the walk's access and share mode.
    fn hold_dir(path: &Path) -> std::io::Result<File> {
        super::open_root(path)
    }

    /// Open a leaf exactly the way [`super::reach`] does: through its parent's
    /// handle, with the walk's own access, share mode and create options. A
    /// probe that used anything else would measure a different call than the
    /// one the sweep will make.
    fn hold_leaf(path: &Path) -> Result<File, String> {
        let parent = hold_dir(path.parent().expect("a leaf has a parent"))
            .map_err(|e| format!("could not hold the parent ({e})"))?;
        super::open_relative(
            &parent,
            path.file_name().expect("a leaf has a name"),
            super::WALK_ACCESS | super::DELETE,
            super::WALK_SHARE,
            super::walk_options(false),
        )
        .map_err(|status| format!("{status:?}"))
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

    /// A profile directory that went between the vetting and the walk is an
    /// account this uninstall did not clean, and reads differently from one
    /// that had nothing of ours left in it.
    #[test]
    fn a_profile_directory_that_has_gone_is_said_rather_than_counted_absent() {
        let dir = tempfile::tempdir().expect("a temp directory");
        // Named but never created — and the names below are joined by hand
        // rather than through `local_in`, which would make the very directory
        // this test is about.
        let profile = dir.path().join("went-away");
        let local = profile.join("AppData").join("Local");
        assert!(
            !profile.exists(),
            "the account's directory must not be there"
        );

        let plan = FilePlan {
            files: vec![local.join("Kuvatin").join("crash.log")],
            trees: vec![local.join("Temp").join("kuvatin")],
            prune_if_empty: Vec::new(),
        };
        let sweep = remove_plan(&profile, &plan);

        assert_eq!(sweep.files_absent, 0, "{sweep:?}");
        assert_eq!(sweep.trees_absent, 0, "{sweep:?}");
        assert_eq!(
            sweep.trouble.len(),
            2,
            "one for each planned path: {:?}",
            sweep.trouble
        );
        for why in &sweep.trouble {
            assert!(why.contains("no longer there"), "{why}");
            assert!(why.contains("went-away"), "the account is named: {why}");
        }
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

        let held = hold_dir(&held_path).expect("open the directory");
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

        let held = hold_dir(&held_path).expect("open the directory");
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

        let held_parent = hold_dir(&parent).expect("hold the parent");
        let held_child = hold_leaf(&child).expect("hold the child");

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
        fn install(meddle: impl FnMut(Meddle, &Path) + 'static) -> Self {
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
        let _meddler = Meddler::install(move |when, leaf| {
            // Once, and only at the moment before the open: `reach` runs for
            // every path in the plan.
            if when != Meddle::BeforeLeafOpen || armed.get() {
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

    /// The bypass that broke the *second* version of this module, and the
    /// reason the walk no longer resolves names below the profile root.
    ///
    /// Holding the ancestors and re-reading them afterwards catches a parent
    /// that is *still* a junction when we look. It catches nothing if the owner
    /// puts it back: convert the parent, let our open resolve by name into the
    /// victim's file, then `FSCTL_DELETE_REPARSE_POINT` and the second look
    /// sees an ordinary directory. The handle we would then delete through is
    /// the victim's. Nothing about that is a race we could win — the owner can
    /// see the exact instant our open lands, because ours denies
    /// `FILE_SHARE_DELETE` and theirs starts failing with a sharing violation,
    /// and an oplock can hold our open open for as long as they like.
    ///
    /// What makes it fail now is that there is no name to redirect: the leaf is
    /// opened through its parent's handle, so a junction planted on the parent
    /// cannot send it anywhere. The open fails instead.
    #[test]
    fn a_parent_converted_and_reverted_around_the_leaf_open_deletes_nothing() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let profile = dir.path().join("profile");
        let local = local_in(&profile);
        let kuvatin = local.join("Kuvatin");
        std::fs::create_dir(&kuvatin).expect("the Kuvatin folder");
        let log = kuvatin.join("kuvatin.log");
        std::fs::write(&log, b"ours").expect("our log");

        // The same leaf name inside the victim, so a redirected open resolves.
        let victim = dir.path().join("victim");
        std::fs::create_dir(&victim).expect("the victim folder");
        let precious = victim.join("kuvatin.log");
        std::fs::write(&precious, b"precious").expect("bait");

        let converted = std::rc::Rc::new(std::cell::Cell::new(false));
        let reverted = std::rc::Rc::new(std::cell::Cell::new(false));
        let did_convert = converted.clone();
        let did_revert = reverted.clone();
        let kuvatin_for_hook = kuvatin.clone();
        let victim_for_hook = victim.clone();
        let _meddler = Meddler::install(move |when, _leaf| match when {
            Meddle::BeforeLeafOpen => {
                if did_convert.get() {
                    return;
                }
                // Empty our own folder, which the owner is free to do…
                if std::fs::remove_file(kuvatin_for_hook.join("kuvatin.log")).is_err() {
                    return;
                }
                // …and convert it, so the name now points at the victim.
                if convert_to_junction(&kuvatin_for_hook, &victim_for_hook).is_ok() {
                    did_convert.set(true);
                }
            }
            // Put it back, so any later look sees a plain directory. On the old
            // logic the leaf open in between has already landed on
            // `victim\kuvatin.log`, and this is what stops the second look
            // noticing. On this one the open refused and we never get here.
            Meddle::AfterLeafOpen => {
                if did_convert.get() && revert_junction(&kuvatin_for_hook).is_ok() {
                    did_revert.set(true);
                }
            }
            // A file is deleted through its handle and resolves no further
            // name, so there is nothing left here to aim anywhere.
            Meddle::BeforeDelete => {}
        });

        let sweep = remove_plan(
            &profile,
            &FilePlan {
                files: vec![log.clone()],
                ..empty_plan()
            },
        );
        drop(_meddler);

        if !converted.get() {
            skip_or_fail_on_ci("could not convert the parent mid-walk");
            return;
        }
        // Whatever state the seams left it in, take the junction down before
        // the temp directory is swept: the walk may have refused before the
        // seam that would have done it.
        let _ = revert_junction(&kuvatin);

        assert_eq!(
            std::fs::read(&precious).expect("the victim's file survives"),
            b"precious",
            "SYSTEM deleted through a junction that was put back before the second look"
        );
        // And it was never even opened: the walk holds every leaf it reaches
        // without FILE_SHARE_DELETE, so a handle on the victim's file would
        // still be open here and this would fail.
        assert!(
            nobody_holds(&precious),
            "the walk resolved into the victim's file, even if it did not delete it"
        );
        // A refusal or an absence, never a removal. Which of the two depends on
        // whether the junction was still standing when the relative open ran —
        // here it is, because the walk refused before the seam that would have
        // taken it down, so this comes back as a refusal naming the parent.
        assert_eq!(
            sweep.files_removed, 0,
            "nothing of ours was there to remove"
        );
        assert_eq!(
            sweep.files_absent + sweep.trouble.len(),
            1,
            "exactly one outcome, and not a removal: {:?}",
            sweep.trouble
        );
    }

    /// The same for a tree, where the old logic gave away a whole directory:
    /// after the second look passed, the leaf handle was on `victim\kuvatin`,
    /// the real parent was empty and convertible again, and `remove_dir_all`
    /// emptied the victim's tree.
    #[test]
    fn a_parent_converted_and_reverted_around_a_tree_deletes_nothing() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let profile = dir.path().join("profile");
        let local = local_in(&profile);
        let temp = local.join("Temp");
        let tree = temp.join("kuvatin");
        std::fs::create_dir_all(&tree).expect("our tree");

        let victim = dir.path().join("victim");
        let victim_tree = victim.join("kuvatin");
        std::fs::create_dir_all(victim_tree.join("sub")).expect("the victim tree");
        let precious = victim_tree.join("precious.txt");
        std::fs::write(&precious, b"precious").expect("bait");
        let deep = victim_tree.join("sub").join("deep.txt");
        std::fs::write(&deep, b"deep").expect("deeper bait");

        let converted = std::rc::Rc::new(std::cell::Cell::new(false));
        let reverted = std::rc::Rc::new(std::cell::Cell::new(false));
        let did_convert = converted.clone();
        let did_revert = reverted.clone();
        let temp_for_hook = temp.clone();
        let victim_for_hook = victim.clone();
        let _meddler = Meddler::install(move |when, leaf| match when {
            Meddle::BeforeLeafOpen => {
                if did_convert.get() {
                    return;
                }
                if std::fs::remove_dir_all(leaf).is_err() {
                    return;
                }
                if convert_to_junction(&temp_for_hook, &victim_for_hook).is_ok() {
                    did_convert.set(true);
                }
            }
            // Put it back so the re-read sees an ordinary directory…
            Meddle::AfterLeafOpen => {
                if did_convert.get() && revert_junction(&temp_for_hook).is_ok() {
                    did_revert.set(true);
                }
            }
            // …and aim it at the victim again, because the tree branch has one
            // more name to resolve: `remove_dir_all` takes a path. On the old
            // logic this is what turned a redirected handle into a whole
            // directory tree of somebody else's being emptied.
            Meddle::BeforeDelete => {
                if did_revert.get() {
                    let _ = convert_to_junction(&temp_for_hook, &victim_for_hook);
                }
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
            skip_or_fail_on_ci("could not convert the parent mid-walk");
            return;
        }
        // Whatever state the seams left it in, take the junction down before
        // the temp directory is swept.
        let _ = revert_junction(&temp);

        assert_eq!(
            std::fs::read(&precious).expect("the victim's file survives"),
            b"precious"
        );
        assert_eq!(
            std::fs::read(&deep).expect("and everything under it"),
            b"deep"
        );
        assert!(victim_tree.is_dir(), "the victim's folder itself survives");
        assert!(
            nobody_holds(&victim_tree),
            "the walk resolved into the victim's folder"
        );
        assert_eq!(
            sweep.trees_removed, 0,
            "nothing of ours was there to remove"
        );
        assert_eq!(
            sweep.trees_absent + sweep.trouble.len(),
            1,
            "a refusal or an absence, never a removal: {:?}",
            sweep.trouble
        );
    }

    /// The `files` branch had no test for a reparse point standing where one of
    /// the named files should be. It is left alone and reported: a link the
    /// account planted there is not a log this uninstall wrote.
    #[test]
    fn a_reparse_point_where_a_file_should_be_is_left_alone() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let profile = dir.path().join("profile");
        let local = local_in(&profile);
        let kuvatin = local.join("Kuvatin");
        std::fs::create_dir(&kuvatin).expect("the Kuvatin folder");

        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).expect("the junction's target");
        let bait = elsewhere.join("precious.txt");
        std::fs::write(&bait, b"precious").expect("bait");

        // A junction, not a file symlink: making one of those needs a privilege
        // this test cannot count on, and either is a reparse point where a file
        // should be, which is the thing under test.
        let link = kuvatin.join("kuvatin.log");
        let Some(_junction) = Junction::new(&link, &elsewhere) else {
            return;
        };

        let sweep = remove_plan(
            &profile,
            &FilePlan {
                files: vec![link.clone()],
                ..empty_plan()
            },
        );

        assert_eq!(sweep.files_removed, 0);
        assert_eq!(sweep.files_absent, 0);
        assert_eq!(sweep.trouble.len(), 1, "{:?}", sweep.trouble);
        assert!(
            sweep.trouble[0].contains("reparse point"),
            "{:?}",
            sweep.trouble
        );
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "it is left exactly where it was"
        );
        assert_eq!(
            std::fs::read(&bait).expect("the target survives"),
            b"precious"
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
        /// second case the hive walk has: the relative open `reach` reaches the
        /// leaf with passes `FILE_OPEN_FOR_BACKUP_INTENT`, which
        /// `SeBackupPrivilege` answers by
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
        // Setting the ACE is not the same as its biting, so probe first — and
        // probe with the call that would actually refuse. That is the relative
        // open `reach` reaches the leaf with, which `hold_leaf` makes exactly:
        // same access, same share mode, same create options. `File::open` would
        // omit `FILE_OPEN_FOR_BACKUP_INTENT` and report a deny as biting when
        // it does not.
        //
        // `SeBackupPrivilege` grants a backup-intent open over any DACL, and
        // this process may hold it: the offline hive test enables it for the
        // whole process, and thread order decides whether that has already
        // happened. So on an elevated run this assertion holds only when this
        // test gets there first — CI covers it on that ordering and skips on
        // the other, which is the limitation, written down rather than papered
        // over. An unelevated token cannot hold the privilege at all, so an
        // unelevated success is a broken ACE and stays a failure.
        if hold_leaf(&log).is_ok() && super::super::hive::is_elevated() {
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
        // The refusal comes from the relative open now, so the number kept is
        // the NT status rather than the Win32 error: STATUS_ACCESS_DENIED.
        assert!(
            why.contains("status 0xc0000022"),
            "with the code kept: {why}"
        );
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
