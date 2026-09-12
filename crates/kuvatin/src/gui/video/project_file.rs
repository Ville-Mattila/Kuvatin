//! Saving and reopening a video project.
//!
//! The engine owns the format (`kuvatin_video::ProjectFile`); this is the part
//! that picks a file, rebuilds the interface's models from what came back, and
//! says what could not be found. Opening replaces the timeline, so a timeline
//! with anything on it asks first.

use super::{ClipKind, TimelineClip, VideoState};
use crate::gui::{name_list, show_error, show_info, AppWindow, VideoAsset};
use slint::{ComponentHandle, Image, Model, SharedString, VecModel};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The extension a project is saved as.
const EXT: &str = "kuvatin";

pub(super) fn wire(ui: &AppWindow, st: &VideoState, im: &super::import::ImportState) {
    let ui_weak = ui.as_weak();
    // The file this project was last saved to or opened from; Ctrl+S writes
    // there instead of asking again.
    let current: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));

    // ---- save ----------------------------------------------------------
    {
        let ui_weak = ui_weak.clone();
        let project_slot = st.project.clone();
        let seq_by_path = im.seq_by_path.clone();
        let current = current.clone();
        ui.on_video_save_project(move |ask_where| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let doc = {
                let slot = project_slot.borrow();
                let Some(p) = slot.as_ref() else {
                    show_error(&ui, "Nothing to save", "Add a clip to the timeline first.");
                    return;
                };
                let mut doc = p.to_document();
                // A sequence clip's URI puts it back on the timeline, but only
                // the spec can put it back in the media bin, and only this side
                // still has it.
                let specs = seq_by_path.borrow();
                for rec in doc.clips.iter_mut() {
                    if !rec.uri.starts_with("imagesequence://") {
                        continue;
                    }
                    rec.sequence = specs
                        .values()
                        .find(|s| s.uri().map(|u| u == rec.uri).unwrap_or(false))
                        .cloned();
                }
                doc
            };
            let existing = current.borrow().clone();
            let path = match existing {
                Some(p) if !ask_where => p,
                _ => {
                    let mut dlg = rfd::FileDialog::new()
                        .add_filter("Kuvatin project", &[EXT])
                        .set_file_name(format!("project.{EXT}"));
                    if let Some(dir) = current.borrow().as_ref().and_then(|p| p.parent()) {
                        dlg = dlg.set_directory(dir);
                    }
                    match dlg.save_file() {
                        Some(p) => p,
                        None => return,
                    }
                }
            };
            match doc.save(&path) {
                Ok(()) => {
                    *current.borrow_mut() = Some(path.clone());
                    ui.set_project_name(file_label(&path));
                    show_info(
                        &ui,
                        "Project saved",
                        format!(
                            "{}\n\n{} clip{} on the timeline.",
                            path.display(),
                            doc.clips.len(),
                            if doc.clips.len() == 1 { "" } else { "s" }
                        ),
                    );
                }
                Err(e) => show_error(&ui, "Could not save the project", format!("{e:#}")),
            }
        });
    }

    // ---- open ----------------------------------------------------------
    {
        let ui_weak = ui_weak.clone();
        let tl_clips = st.tl_clips.clone();
        ui.on_video_open_project(move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            // Opening replaces what is on the timeline, and nothing here tracks
            // unsaved changes — so ask before throwing an arrangement away.
            if tl_clips.row_count() > 0 {
                ui.set_confirm_open_project(true);
            } else {
                ui.invoke_video_open_project_confirmed();
            }
        });
    }

    {
        let ui_weak = ui_weak.clone();
        let st = st.clone_handles();
        let seq_by_path = im.seq_by_path.clone();
        let import_q = im.q.clone();
        let current = current.clone();
        ui.on_video_open_project_confirmed(move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let mut dlg = rfd::FileDialog::new().add_filter("Kuvatin project", &[EXT]);
            if let Some(dir) = current.borrow().as_ref().and_then(|p| p.parent()) {
                dlg = dlg.set_directory(dir);
            }
            let Some(path) = dlg.pick_file() else {
                return;
            };
            let doc = match kuvatin_video::ProjectFile::load(&path) {
                Ok(d) => d,
                Err(e) => {
                    show_error(&ui, "Could not open the project", format!("{e:#}"));
                    return;
                }
            };
            if st.project.borrow().is_none() {
                *st.project.borrow_mut() = super::make_project(&ui.as_weak());
            }
            let missing = {
                let mut slot = st.project.borrow_mut();
                let Some(p) = slot.as_mut() else {
                    show_error(
                        &ui,
                        "Video engine unavailable",
                        "The project could not be opened because the engine did not start.",
                    );
                    return;
                };
                match p.apply_document(&doc) {
                    Ok(missing) => missing,
                    Err(e) => {
                        show_error(&ui, "Could not open the project", format!("{e:#}"));
                        return;
                    }
                }
            };
            restore_models(&ui, &st, &seq_by_path, &doc);
            import_q.reseed(&st.bin_paths.borrow());
            *current.borrow_mut() = Some(path.clone());
            ui.set_project_name(file_label(&path));
            if !missing.is_empty() {
                show_error(
                    &ui,
                    "Some media could not be found",
                    format!(
                        "{} clip{} left out because {} source{} missing:\n{}",
                        missing.len(),
                        if missing.len() == 1 { "" } else { "s" },
                        if missing.len() == 1 { "its" } else { "their" },
                        if missing.len() == 1 { " is" } else { "s are" },
                        name_list(&missing)
                    ),
                );
            }
        });
    }
}

/// "cut.kuvatin", for the label above the media bin.
fn file_label(path: &Path) -> SharedString {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .into()
}

/// Rebuild the timeline rows, the track list and the media bin from a project
/// the engine has just applied. Thumbnails arrive afterwards, off the
/// interface thread — a ten-clip project would otherwise freeze the window
/// while every source was decoded.
fn restore_models(
    ui: &AppWindow,
    st: &VideoHandles,
    seq_by_path: &Rc<RefCell<std::collections::HashMap<PathBuf, kuvatin_video::SequenceSpec>>>,
    doc: &kuvatin_video::ProjectFile,
) {
    let records = {
        let slot = st.project.borrow();
        match slot.as_ref() {
            Some(p) => p.clip_records(),
            None => Vec::new(),
        }
    };

    // Timeline rows, in the engine's order.
    let rows: Vec<TimelineClip> = records
        .iter()
        .map(|(id, rec)| TimelineClip {
            id: id.0.clone().into(),
            track: rec.track as i32,
            start: rec.start as f32,
            duration: rec.duration as f32,
            inpoint: rec.inpoint as f32,
            name: rec.name.clone().into(),
            kind: kind_of(&rec.uri),
            selected: false,
            thumb: Image::default(),
        })
        .collect();
    st.tl_clips.set_vec(rows);

    // Tracks: as many as the deepest clip uses, and never fewer than the two
    // an empty project starts with.
    let needed = records
        .iter()
        .map(|(_, r)| r.track + 1)
        .max()
        .unwrap_or(0)
        .max(2);
    st.tracks.set_vec(
        (0..needed)
            .map(|i| SharedString::from(format!("Track {}", i + 1)))
            .collect::<Vec<_>>(),
    );

    // Media bin: one row per distinct source, and the sequence specs come back
    // with it so a bin click re-adds the sequence rather than a single still.
    let mut bin: Vec<PathBuf> = Vec::new();
    let mut assets: Vec<VideoAsset> = Vec::new();
    seq_by_path.borrow_mut().clear();
    for rec in &doc.clips {
        let path = match (&rec.sequence, kuvatin_video::path_from_uri(&rec.uri)) {
            (Some(spec), _) => {
                let first = spec.dir.join(spec.frame_file_name(spec.start));
                seq_by_path.borrow_mut().insert(first.clone(), spec.clone());
                first
            }
            (None, Some(p)) => p,
            // A source that is neither a file nor a sequence cannot be re-added
            // from the bin; it is still on the timeline.
            (None, None) => continue,
        };
        if bin.contains(&path) {
            continue;
        }
        bin.push(path);
        assets.push(VideoAsset {
            name: rec.name.clone().into(),
            thumb: Image::default(),
        });
    }
    *st.bin_paths.borrow_mut() = bin;
    st.assets.set_vec(assets);

    st.sel_idx.set(-1);
    ui.set_timeline_selected(-1);
    ui.set_inspector_name("".into());
    ui.set_canvas_w(doc.canvas_w);
    ui.set_canvas_h(doc.canvas_h);
    if let Some(p) = st.project.borrow().as_ref() {
        ui.set_timeline_duration(p.duration().map(|d| d.as_secs_f32()).unwrap_or(0.0));
        let _ = p.seek(std::time::Duration::ZERO);
        p.refresh_preview();
    }
    ui.set_video_playing(false);
    ui.set_playhead(0.0);

    spawn_thumbnails(ui.as_weak(), records);
}

/// Decode one thumbnail per clip on a worker and drop each into its row (and
/// the matching media-bin row) as it arrives.
fn spawn_thumbnails(
    ui_weak: slint::Weak<AppWindow>,
    records: Vec<(kuvatin_video::ClipId, kuvatin_video::ClipRecord)>,
) {
    if records.is_empty() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("kuvatin-project-thumbs".into())
        .spawn(move || {
            for (id, rec) in records {
                let Some(frame) = kuvatin_video::thumbnail_uri(&rec.uri, 160) else {
                    continue;
                };
                let ui_weak = ui_weak.clone();
                let clip_id: SharedString = id.0.clone().into();
                let name: SharedString = rec.name.clone().into();
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak.upgrade() else {
                        return;
                    };
                    let thumb = super::frame_to_image(Some(frame));
                    let clips = ui.get_timeline_clips();
                    for i in 0..clips.row_count() {
                        if let Some(mut row) = clips.row_data(i) {
                            if row.id == clip_id {
                                row.thumb = thumb.clone();
                                clips.set_row_data(i, row);
                            }
                        }
                    }
                    let bin = ui.get_video_clips();
                    for i in 0..bin.row_count() {
                        if let Some(mut row) = bin.row_data(i) {
                            if row.name == name {
                                row.thumb = thumb.clone();
                                bin.set_row_data(i, row);
                            }
                        }
                    }
                });
            }
        });
}

/// What a saved URI is, for the clip's colour on the timeline.
fn kind_of(uri: &str) -> ClipKind {
    if uri.starts_with("imagesequence://") {
        return ClipKind::Sequence;
    }
    let ext = uri
        .rsplit('.')
        .next()
        .map(|e| e.split('?').next().unwrap_or(e).to_lowercase())
        .unwrap_or_default();
    if kuvatin_core::format::is_input_extension(&ext) {
        ClipKind::Image
    } else {
        ClipKind::Video
    }
}

/// The handles `restore_models` needs, cloned out of [`VideoState`] so the
/// callback can own them.
pub(super) struct VideoHandles {
    pub(super) project: Rc<RefCell<Option<kuvatin_video::Project>>>,
    pub(super) assets: Rc<VecModel<VideoAsset>>,
    pub(super) bin_paths: Rc<RefCell<Vec<PathBuf>>>,
    pub(super) tl_clips: Rc<VecModel<TimelineClip>>,
    pub(super) tracks: Rc<VecModel<SharedString>>,
    pub(super) sel_idx: Rc<std::cell::Cell<i32>>,
}

impl VideoState {
    fn clone_handles(&self) -> VideoHandles {
        VideoHandles {
            project: self.project.clone(),
            assets: self.assets.clone(),
            bin_paths: self.bin_paths.clone(),
            tl_clips: self.tl_clips.clone(),
            tracks: self.tracks.clone(),
            sel_idx: self.sel_idx.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_uri_says_what_kind_of_clip_it_is() {
        assert_eq!(
            kind_of("imagesequence://C:/r/f_%04d.png?start-index=1&framerate=24/1"),
            ClipKind::Sequence
        );
        assert_eq!(kind_of("file:///C:/shots/take1.mp4"), ClipKind::Video);
        assert_eq!(kind_of("file:///C:/shots/logo.png"), ClipKind::Image);
        // A query on a plain file URI must not be read as part of the
        // extension: `.png?x=1` is still a PNG.
        assert_eq!(kind_of("file:///C:/shots/logo.png?x=1"), ClipKind::Image);
    }

    #[test]
    fn the_label_is_the_file_name() {
        assert_eq!(
            file_label(Path::new(r"C:\work\edit\cut.kuvatin")).as_str(),
            "cut.kuvatin"
        );
    }
}
