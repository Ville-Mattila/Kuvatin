//! Undo and redo for Images mode: adding files, removing one, clearing the
//! list and applying a crop. A step keeps exactly what its action threw away
//! or overwrote and applies to the file list, the crop map and the thumbnail
//! cache as plain data; the handler then brings the rows in line with
//! `sync_rows`, the way every list change already does.

use super::{CropMap, ThumbData};
use crate::gui::history::Step;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A crop in absolute pixels: x, y, width, height.
pub(super) type Crop = (u32, u32, u32, u32);

/// One undoable change to the Images mode list or crops.
#[derive(Clone)]
pub(super) enum ImageStep {
    /// The files an add actually put in the list (not the ones already there).
    AddFiles { paths: Vec<PathBuf> },
    /// A file removed with its row's ×, and the crop it had.
    RemoveFile { path: PathBuf, crop: Option<Crop> },
    /// Everything Clear threw away.
    ClearList {
        paths: Vec<PathBuf>,
        crops: Vec<(PathBuf, Crop)>,
        thumbs: Vec<(PathBuf, ThumbData)>,
    },
    /// A crop applied to a file, and the crop it replaced.
    ApplyCrop {
        path: PathBuf,
        before: Option<Crop>,
        after: Crop,
    },
}

/// Images mode's undoable state, borrowed for one undo or redo.
pub(super) struct Lists<'a> {
    pub(super) files: &'a mut Vec<PathBuf>,
    pub(super) crops: &'a mut CropMap,
    pub(super) thumbs: &'a mut HashMap<PathBuf, ThumbData>,
}

/// What an undo or redo leaves for the handler to do.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Outcome {
    /// Files that could not come back because they are no longer on disk.
    pub(super) skipped: usize,
    /// The file to select so the change is visible, if the step is about one.
    pub(super) select: Option<PathBuf>,
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// "1 file", "12 files".
pub(super) fn files_phrase(n: usize) -> String {
    if n == 1 {
        "1 file".into()
    } else {
        format!("{n} files")
    }
}

/// Put `path` into the sorted, de-duplicated list.
fn insert_sorted(files: &mut Vec<PathBuf>, path: &Path) {
    if let Err(at) = files.binary_search_by(|f| f.as_path().cmp(path)) {
        files.insert(at, path.to_path_buf());
    }
}

impl Step for ImageStep {
    fn describe(&self) -> String {
        match self {
            ImageStep::AddFiles { paths } => format!("adding {}", files_phrase(paths.len())),
            ImageStep::RemoveFile { path, .. } => format!("removing {}", file_name(path)),
            ImageStep::ClearList { paths, .. } => {
                format!("clearing the list ({})", files_phrase(paths.len()))
            }
            ImageStep::ApplyCrop { path, .. } => format!("cropping {}", file_name(path)),
        }
    }

    /// Every Images mode action is a deliberate click; none of them merge.
    fn merges_with(&self, _newer: &Self) -> bool {
        false
    }

    fn absorb(&mut self, _newer: Self) {}

    fn is_empty(&self) -> bool {
        match self {
            ImageStep::AddFiles { paths } | ImageStep::ClearList { paths, .. } => paths.is_empty(),
            ImageStep::RemoveFile { .. } => false,
            ImageStep::ApplyCrop { before, after, .. } => *before == Some(*after),
        }
    }
}

impl ImageStep {
    /// Take the list and crops back to before this step. `exists` says whether
    /// a file is still on disk; one that is not is skipped and counted.
    pub(super) fn undo(&self, lists: Lists<'_>, exists: &dyn Fn(&Path) -> bool) -> Outcome {
        let mut out = Outcome::default();
        match self {
            ImageStep::AddFiles { paths } => {
                let added: HashSet<&Path> = paths.iter().map(PathBuf::as_path).collect();
                lists.files.retain(|f| !added.contains(f.as_path()));
                for path in paths {
                    lists.crops.remove(path);
                }
            }
            ImageStep::RemoveFile { path, crop } => {
                if exists(path) {
                    insert_sorted(lists.files, path);
                    if let Some(c) = crop {
                        lists.crops.insert(path.clone(), *c);
                    }
                    out.select = Some(path.clone());
                } else {
                    out.skipped = 1;
                }
            }
            ImageStep::ClearList {
                paths,
                crops,
                thumbs,
            } => {
                for path in paths {
                    if exists(path) {
                        insert_sorted(lists.files, path);
                    } else {
                        out.skipped += 1;
                    }
                }
                for (path, c) in crops {
                    if lists.files.binary_search(path).is_ok() {
                        lists.crops.insert(path.clone(), *c);
                    }
                }
                for (path, t) in thumbs {
                    if lists.files.binary_search(path).is_ok() {
                        lists.thumbs.insert(path.clone(), t.clone());
                    }
                }
            }
            ImageStep::ApplyCrop { path, before, .. } => {
                // A crop belongs to a file in the list: one that did not come
                // back must not leave a crop behind for a later add to pick up.
                if lists.files.binary_search(path).is_ok() {
                    match before {
                        Some(c) => lists.crops.insert(path.clone(), *c),
                        None => lists.crops.remove(path),
                    };
                    out.select = Some(path.clone());
                }
            }
        }
        out
    }

    /// Apply this step again after an undo.
    pub(super) fn redo(&self, lists: Lists<'_>, exists: &dyn Fn(&Path) -> bool) -> Outcome {
        let mut out = Outcome::default();
        match self {
            ImageStep::AddFiles { paths } => {
                for path in paths {
                    if exists(path) {
                        insert_sorted(lists.files, path);
                    } else {
                        out.skipped += 1;
                    }
                }
            }
            ImageStep::RemoveFile { path, .. } => {
                lists.files.retain(|f| f != path);
                lists.crops.remove(path);
            }
            ImageStep::ClearList { .. } => {
                lists.files.clear();
                lists.crops.clear();
                lists.thumbs.clear();
            }
            ImageStep::ApplyCrop { path, after, .. } => {
                if lists.files.binary_search(path).is_ok() {
                    lists.crops.insert(path.clone(), *after);
                    out.select = Some(path.clone());
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str) -> PathBuf {
        PathBuf::from(format!("C:/photos/{name}"))
    }

    fn thumb() -> ThumbData {
        ThumbData {
            rgba: vec![0, 0, 0, 255],
            w: 1,
            h: 1,
            dims: "1×1".into(),
        }
    }

    struct State {
        files: Vec<PathBuf>,
        crops: CropMap,
        thumbs: HashMap<PathBuf, ThumbData>,
    }

    impl State {
        fn new(files: &[&str]) -> Self {
            State {
                files: files.iter().map(|f| p(f)).collect(),
                crops: CropMap::new(),
                thumbs: HashMap::new(),
            }
        }
        fn lists(&mut self) -> Lists<'_> {
            Lists {
                files: &mut self.files,
                crops: &mut self.crops,
                thumbs: &mut self.thumbs,
            }
        }
    }

    fn everything_exists(_: &Path) -> bool {
        true
    }

    #[test]
    fn steps_describe_themselves() {
        let add = ImageStep::AddFiles {
            paths: vec![p("a.jpg"), p("b.jpg")],
        };
        assert_eq!(add.describe(), "adding 2 files");
        let one = ImageStep::AddFiles {
            paths: vec![p("a.jpg")],
        };
        assert_eq!(one.describe(), "adding 1 file");
        let rm = ImageStep::RemoveFile {
            path: p("a.jpg"),
            crop: None,
        };
        assert_eq!(rm.describe(), "removing a.jpg");
        let clear = ImageStep::ClearList {
            paths: vec![p("a.jpg"); 40],
            crops: vec![],
            thumbs: vec![],
        };
        assert_eq!(clear.describe(), "clearing the list (40 files)");
        let crop = ImageStep::ApplyCrop {
            path: p("a.jpg"),
            before: None,
            after: (0, 0, 5, 5),
        };
        assert_eq!(crop.describe(), "cropping a.jpg");
    }

    #[test]
    fn nothing_merges_and_no_ops_are_empty() {
        let crop = ImageStep::ApplyCrop {
            path: p("a.jpg"),
            before: None,
            after: (0, 0, 5, 5),
        };
        assert!(!crop.merges_with(&crop));
        assert!(
            ImageStep::AddFiles { paths: vec![] }.is_empty(),
            "re-adding files already there"
        );
        let same = ImageStep::ApplyCrop {
            path: p("a.jpg"),
            before: Some((0, 0, 5, 5)),
            after: (0, 0, 5, 5),
        };
        assert!(same.is_empty(), "applying the crop that was already there");
    }

    #[test]
    fn undoing_an_add_removes_only_the_new_files() {
        let mut s = State::new(&["a.jpg", "b.jpg", "c.jpg"]);
        let step = ImageStep::AddFiles {
            paths: vec![p("a.jpg"), p("c.jpg")],
        };
        let out = step.undo(s.lists(), &everything_exists);
        assert_eq!(s.files, vec![p("b.jpg")]);
        assert_eq!(out, Outcome::default());
        step.redo(s.lists(), &everything_exists);
        assert_eq!(
            s.files,
            vec![p("a.jpg"), p("b.jpg"), p("c.jpg")],
            "sorted back in on both sides"
        );
    }

    #[test]
    fn undoing_a_remove_brings_back_the_file_and_its_crop_and_selects_it() {
        let mut s = State::new(&["a.jpg", "c.jpg"]);
        let step = ImageStep::RemoveFile {
            path: p("b.jpg"),
            crop: Some((1, 2, 3, 4)),
        };
        let out = step.undo(s.lists(), &everything_exists);
        assert_eq!(s.files, vec![p("a.jpg"), p("b.jpg"), p("c.jpg")]);
        assert_eq!(s.crops.get(&p("b.jpg")), Some(&(1, 2, 3, 4)));
        assert_eq!(out.select, Some(p("b.jpg")));
        step.redo(s.lists(), &everything_exists);
        assert_eq!(s.files, vec![p("a.jpg"), p("c.jpg")]);
        assert!(s.crops.is_empty());
    }

    #[test]
    fn undoing_a_clear_restores_paths_crops_and_thumbnails() {
        let mut s = State::new(&[]);
        let step = ImageStep::ClearList {
            paths: vec![p("a.jpg"), p("b.jpg")],
            crops: vec![(p("b.jpg"), (0, 0, 9, 9))],
            thumbs: vec![(p("a.jpg"), thumb())],
        };
        step.undo(s.lists(), &everything_exists);
        assert_eq!(s.files, vec![p("a.jpg"), p("b.jpg")]);
        assert_eq!(s.crops.get(&p("b.jpg")), Some(&(0, 0, 9, 9)));
        assert!(s.thumbs.contains_key(&p("a.jpg")), "no decode needed");
        step.redo(s.lists(), &everything_exists);
        assert!(s.files.is_empty() && s.crops.is_empty() && s.thumbs.is_empty());
    }

    #[test]
    fn undoing_a_crop_puts_back_the_previous_crop_or_none() {
        let mut s = State::new(&["a.jpg"]);
        s.crops.insert(p("a.jpg"), (5, 5, 5, 5));
        let first = ImageStep::ApplyCrop {
            path: p("a.jpg"),
            before: None,
            after: (1, 1, 1, 1),
        };
        first.undo(s.lists(), &everything_exists);
        assert!(s.crops.is_empty(), "there was no crop before");
        let second = ImageStep::ApplyCrop {
            path: p("a.jpg"),
            before: Some((1, 1, 1, 1)),
            after: (2, 2, 2, 2),
        };
        let out = second.undo(s.lists(), &everything_exists);
        assert_eq!(s.crops.get(&p("a.jpg")), Some(&(1, 1, 1, 1)));
        assert_eq!(out.select, Some(p("a.jpg")), "the crop outline follows");
        let out = second.redo(s.lists(), &everything_exists);
        assert_eq!(s.crops.get(&p("a.jpg")), Some(&(2, 2, 2, 2)));
        assert_eq!(
            out.select,
            Some(p("a.jpg")),
            "a redone crop selects its file too"
        );
    }

    #[test]
    fn a_file_gone_from_disk_is_skipped_and_counted() {
        let mut s = State::new(&[]);
        let step = ImageStep::ClearList {
            paths: vec![p("a.jpg"), p("gone.jpg")],
            crops: vec![(p("gone.jpg"), (0, 0, 1, 1))],
            thumbs: vec![],
        };
        let out = step.undo(s.lists(), &|path| !path.ends_with("gone.jpg"));
        assert_eq!(s.files, vec![p("a.jpg")]);
        assert!(
            s.crops.is_empty(),
            "no crop for a file that is not in the list"
        );
        assert_eq!(out.skipped, 1);
    }

    #[test]
    fn a_step_that_changes_something_is_not_empty() {
        assert!(!ImageStep::AddFiles {
            paths: vec![p("a.jpg")]
        }
        .is_empty());
        assert!(!ImageStep::RemoveFile {
            path: p("a.jpg"),
            crop: None
        }
        .is_empty());
        let clear = ImageStep::ClearList {
            paths: vec![p("a.jpg")],
            crops: vec![],
            thumbs: vec![],
        };
        assert!(!clear.is_empty());
        let nothing = ImageStep::ClearList {
            paths: vec![],
            crops: vec![],
            thumbs: vec![],
        };
        assert!(nothing.is_empty(), "clearing an empty list");
        let recrop = ImageStep::ApplyCrop {
            path: p("a.jpg"),
            before: Some((0, 0, 5, 5)),
            after: (1, 1, 5, 5),
        };
        assert!(!recrop.is_empty(), "a crop over a different crop");
    }

    #[test]
    fn adding_back_and_removing_back_skip_and_count_a_file_gone_from_disk() {
        let exists = |path: &Path| !path.ends_with("gone.jpg");
        let mut s = State::new(&["a.jpg"]);
        let add = ImageStep::AddFiles {
            paths: vec![p("b.jpg"), p("gone.jpg")],
        };
        let out = add.redo(s.lists(), &exists);
        assert_eq!(s.files, vec![p("a.jpg"), p("b.jpg")]);
        assert_eq!(out.skipped, 1);
        let mut s = State::new(&["a.jpg"]);
        let remove = ImageStep::RemoveFile {
            path: p("gone.jpg"),
            crop: Some((1, 1, 1, 1)),
        };
        let out = remove.undo(s.lists(), &exists);
        assert_eq!(s.files, vec![p("a.jpg")]);
        assert!(
            s.crops.is_empty(),
            "no crop for a file that did not come back"
        );
        assert_eq!(
            out,
            Outcome {
                skipped: 1,
                select: None
            }
        );
    }

    /// A crop belongs to a file in the list: undoing or redoing one for a file
    /// that is not there leaves no crop behind for a later add to pick up.
    #[test]
    fn a_crop_for_a_file_not_in_the_list_is_not_written() {
        let mut s = State::new(&[]);
        let crop = ImageStep::ApplyCrop {
            path: p("a.jpg"),
            before: Some((1, 1, 1, 1)),
            after: (2, 2, 2, 2),
        };
        let out = crop.undo(s.lists(), &everything_exists);
        assert!(s.crops.is_empty() && out.select.is_none(), "undo");
        let out = crop.redo(s.lists(), &everything_exists);
        assert!(s.crops.is_empty() && out.select.is_none(), "redo");
    }
}
