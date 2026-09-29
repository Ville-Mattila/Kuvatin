//! Saving and reopening a video project.
//!
//! The engine owns the format (`kuvatin_video::ProjectFile`); this is the part
//! that picks a file, rebuilds the interface's models from what came back, and
//! says what could not be found. Opening replaces the timeline, so a timeline
//! with anything on it asks first.

use super::{ClipKind, TimelineClip, TimelineTrack, VideoState};
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
        let track_rows = st.tracks.clone();
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
                // Names and locks live only here, and the engine silences
                // solo too, so the table comes from the rows: solo left out.
                doc.tracks = super::tracks::records(&super::tracks::rows_of(&track_rows));
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
                    // The document reached the file: the timeline and the disk
                    // agree again, so closing now would lose nothing.
                    if let Some(p) = project_slot.borrow().as_ref() {
                        p.mark_saved();
                    }
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
            // The history described the timeline this project replaced.
            st.history.borrow_mut().clear();
            super::undo::refresh(&ui, &st.history.borrow());
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
            kind: kind_of(rec),
            selected: false,
            thumb: Image::default(),
            rate: rec.rate as f32,
            wave: Image::default(),
            wave_secs: 0.0,
        })
        .collect();
    st.tl_clips.set_vec(rows);

    // Tracks: the saved table, reaching the deepest clip, and then the engine
    // hears the saved mutes.
    let deepest = records.iter().map(|(_, r)| r.track + 1).max().unwrap_or(0);
    st.tracks.set_vec(track_rows_on_open(&doc.tracks, deepest));
    if let Some(p) = st.project.borrow_mut().as_mut() {
        super::tracks::push_mutes(p, &st.tracks);
    }

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

    let sources: Vec<(SharedString, String)> = records
        .iter()
        .map(|(id, rec)| (id.0.as_str().into(), rec.uri.clone()))
        .collect();
    st.waves.fill(ui.as_weak(), sources);
    spawn_thumbnails(ui.as_weak(), records);
}

/// The track rows a project opens with: its saved table, padded with unnamed
/// rows to reach the deepest clip (`deepest` is that clip's track plus one)
/// and to the two an empty project starts with. The largest of the three
/// wins: a table shorter than the deepest clip would otherwise lose a track,
/// and a file from 2.13 or earlier has no table at all. Solo is never saved,
/// so every row opens unsoloed.
fn track_rows_on_open(table: &[kuvatin_video::TrackRecord], deepest: usize) -> Vec<TimelineTrack> {
    let needed = table.len().max(deepest).max(2);
    (0..needed)
        .map(|i| super::tracks::row(&table.get(i).cloned().unwrap_or_default(), false))
        .collect()
}

/// Decode one thumbnail per clip on a worker and drop each into its row (and
/// the matching media-bin row) as it arrives.
pub(super) fn spawn_thumbnails(
    ui_weak: slint::Weak<AppWindow>,
    records: Vec<(kuvatin_video::ClipId, kuvatin_video::ClipRecord)>,
) {
    if records.is_empty() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("kuvatin-project-thumbs".into())
        .spawn(move || {
            // A title has no source to picture: its name on the block is all.
            for (id, rec) in records.into_iter().filter(|(_, r)| !r.uri.is_empty()) {
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

/// What a clip is, for its colour on the timeline: a title by its body,
/// anything else by its URI.
pub(super) fn kind_of(record: &kuvatin_video::ClipRecord) -> ClipKind {
    match record.body {
        Some(kuvatin_video::ClipBody::Title(_)) => ClipKind::Title,
        None => kind_of_uri(&record.uri),
    }
}

/// What a saved URI is.
pub(super) fn kind_of_uri(uri: &str) -> ClipKind {
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
    pub(super) tracks: Rc<VecModel<TimelineTrack>>,
    pub(super) sel_idx: Rc<std::cell::Cell<i32>>,
    pub(super) history: super::undo::TimelineHistory,
    pub(super) waves: super::waves::Waves,
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
            history: self.history.clone(),
            waves: self.waves.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_uri_says_what_kind_of_clip_it_is() {
        assert_eq!(
            kind_of_uri("imagesequence://C:/r/f_%04d.png?start-index=1&framerate=24/1"),
            ClipKind::Sequence
        );
        assert_eq!(kind_of_uri("file:///C:/shots/take1.mp4"), ClipKind::Video);
        assert_eq!(kind_of_uri("file:///C:/shots/logo.png"), ClipKind::Image);
        // A query on a plain file URI must not be read as part of the
        // extension: `.png?x=1` is still a PNG.
        assert_eq!(
            kind_of_uri("file:///C:/shots/logo.png?x=1"),
            ClipKind::Image
        );
    }

    /// A title is known by its body, whatever its URI says.
    #[test]
    fn a_title_record_is_a_title_clip() {
        let record = |body| kuvatin_video::ClipRecord {
            uri: String::new(),
            name: "Hello".into(),
            track: 0,
            start: 0.0,
            inpoint: 0.0,
            duration: 5.0,
            rate: 1.0,
            layout: kuvatin_video::LayoutRecord {
                posx: 0,
                posy: 0,
                scale: 1.0,
                alpha: 1.0,
                volume: 1.0,
            },
            sequence: None,
            body,
        };
        let title = kuvatin_video::ClipBody::Title(kuvatin_video::TitleRecord::default());
        assert_eq!(kind_of(&record(Some(title))), ClipKind::Title);
        let mut still = record(None);
        still.uri = "file:///C:/shots/logo.png".into();
        assert_eq!(kind_of(&still), ClipKind::Image);
    }

    fn named(name: &str) -> kuvatin_video::TrackRecord {
        kuvatin_video::TrackRecord {
            name: name.into(),
            ..Default::default()
        }
    }

    /// The saved table decides, when it is the longest of the three.
    #[test]
    fn a_project_opens_with_its_saved_tracks() {
        let rows = track_rows_on_open(&[named("A"), named("B"), named("C")], 1);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["A", "B", "C"]);
    }

    /// A table shorter than the deepest clip would lose a track: the clip
    /// wins, and the rows past the table are unnamed.
    #[test]
    fn the_deepest_clip_outranks_a_shorter_table() {
        let rows = track_rows_on_open(&[named("A")], 4);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].name.as_str(), "A");
        assert_eq!(rows[3], TimelineTrack::default());
    }

    /// A file with no table and a clip on the top track only still opens
    /// with the two tracks every project starts with: 2.13 and earlier.
    #[test]
    fn a_project_never_opens_with_fewer_than_two_tracks() {
        assert_eq!(track_rows_on_open(&[], 1).len(), 2);
        assert_eq!(track_rows_on_open(&[], 0).len(), 2);
    }

    /// Solo is not saved, so no key a file could carry brings it back.
    #[test]
    fn no_track_opens_soloed() {
        let text = "version = 1\ncanvas_w = 1280\ncanvas_h = 720\n\n[[tracks]]\nname = \"A\"\nsoloed = true\n";
        let doc: kuvatin_video::ProjectFile = toml::from_str(text).expect("a project");
        let rows = track_rows_on_open(&doc.tracks, 0);
        assert!(rows.iter().all(|r| !r.soloed));
        assert_eq!(rows[0].name.as_str(), "A");
    }

    #[test]
    fn the_label_is_the_file_name() {
        assert_eq!(
            file_label(Path::new(r"C:\work\edit\cut.kuvatin")).as_str(),
            "cut.kuvatin"
        );
    }
}
