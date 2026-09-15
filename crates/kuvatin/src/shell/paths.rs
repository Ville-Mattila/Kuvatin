//! Which of a profile's Kuvatin files and folders the uninstall deletes, and
//! which it keeps. Presets and settings (`AppData\Roaming\Kuvatin`) stay; logs,
//! `%TEMP%\kuvatin` and the sparse package's data folder go. The whole
//! `AppData\Local\Kuvatin` folder is NEVER deleted outright — it can hold the
//! user's signing keys — only named files inside it, and then the folder itself
//! only when it ends up empty.
//!
//! Nothing outside `#[cfg(test)]` calls into this module yet: the caller is
//! the all-users file walk of the `--unregister-all-users` entry point, a
//! later task in that plan. Until then, allow the otherwise-unused items.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// The family-name prefix of the sparse package's per-user data folder under
/// `AppData\Local\Packages`. The suffix is a Publisher hash Windows computes,
/// so we match by prefix.
pub(super) const PACKAGE_DATA_PREFIX: &str = "VilleMattila.Kuvatin_";

/// What to remove in one profile.
#[derive(Debug, Default, PartialEq, Eq)]
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
/// (usually zero or one). Reads the disk; `[]` when the parent is absent.
pub(super) fn package_data_dirs(profile: &Path) -> Vec<PathBuf> {
    let packages = profile.join("AppData").join("Local").join("Packages");
    let Ok(rd) = std::fs::read_dir(&packages) else {
        return Vec::new();
    };
    rd.flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(PACKAGE_DATA_PREFIX)
        })
        .map(|e| e.path())
        .collect()
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
}
