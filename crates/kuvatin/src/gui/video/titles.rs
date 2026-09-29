//! Text overlays as the inspector shows them: a size and a bold switch
//! instead of a Pango font description, and alignment as an index. Also the
//! Text chip, which puts one on the timeline.

use super::{tracks, undo, ClipKind};
use crate::gui::{show_error, AppWindow, TimelineClip};
use kuvatin_video::{TitleHAlign, TitleRecord, TitleVAlign};
use slint::{ComponentHandle, Image, Model, VecModel};

/// How long a new title lasts.
const NEW_TITLE: std::time::Duration = std::time::Duration::from_secs(5);

/// The one family a title is set in. A fixed family cannot fail to resolve on
/// another machine; the record keeps the whole description, so a later
/// version can offer more without changing the file.
const FAMILY: &str = "Sans";

/// The sizes the inspector offers, in points.
const MIN_SIZE: i32 = 8;
const MAX_SIZE: i32 = 400;

/// The size a description without one is drawn at.
const DEFAULT_SIZE: i32 = 48;

/// The Pango description for a size and weight: "Sans Bold 48", "Sans 48".
pub(super) fn font_desc(size: i32, bold: bool) -> String {
    let size = size.clamp(MIN_SIZE, MAX_SIZE);
    if bold {
        format!("{FAMILY} Bold {size}")
    } else {
        format!("{FAMILY} {size}")
    }
}

/// The size and weight in a Pango description, as the inspector shows them:
/// the trailing number (48 if there is none) and whether any word is "Bold".
pub(super) fn font_parts(desc: &str) -> (i32, bool) {
    let words: Vec<&str> = desc.split_whitespace().collect();
    let size = words
        .last()
        .and_then(|w| w.parse::<f64>().ok())
        .map_or(DEFAULT_SIZE, |s| s.round() as i32)
        .clamp(MIN_SIZE, MAX_SIZE);
    let bold = words.iter().any(|w| w.eq_ignore_ascii_case("bold"));
    (size, bold)
}

/// Horizontal alignment as the inspector's toggle index: left, centre, right.
pub(super) fn halign_index(a: TitleHAlign) -> i32 {
    match a {
        TitleHAlign::Left => 0,
        TitleHAlign::Center => 1,
        TitleHAlign::Right => 2,
    }
}

pub(super) fn halign_at(i: i32) -> TitleHAlign {
    match i {
        0 => TitleHAlign::Left,
        2 => TitleHAlign::Right,
        _ => TitleHAlign::Center,
    }
}

/// Vertical alignment as the inspector's toggle index: top, middle, bottom.
pub(super) fn valign_index(a: TitleVAlign) -> i32 {
    match a {
        TitleVAlign::Top => 0,
        TitleVAlign::Center => 1,
        TitleVAlign::Bottom => 2,
    }
}

pub(super) fn valign_at(i: i32) -> TitleVAlign {
    match i {
        0 => TitleVAlign::Top,
        2 => TitleVAlign::Bottom,
        _ => TitleVAlign::Center,
    }
}

/// Show a title's text and style in the inspector.
pub(super) fn show(ui: &AppWindow, title: &TitleRecord) {
    let (size, bold) = font_parts(&title.font);
    ui.set_insp_text(title.text.as_str().into());
    ui.set_insp_font_size(size);
    ui.set_insp_font_bold(bold);
    ui.set_insp_text_color(title.color.as_str().into());
    ui.set_insp_halign(halign_index(title.halign));
    ui.set_insp_valign(valign_index(title.valign));
}

/// The title the inspector's controls describe.
fn from_inspector(ui: &AppWindow) -> TitleRecord {
    TitleRecord {
        text: ui.get_insp_text().into(),
        font: font_desc(ui.get_insp_font_size(), ui.get_insp_font_bold()),
        color: ui.get_insp_text_color().into(),
        halign: halign_at(ui.get_insp_halign()),
        valign: valign_at(ui.get_insp_valign()),
    }
}

/// Give a title's row the name its text now gives it, and the inspector's
/// heading too when the title is the selected clip.
pub(super) fn rename_row(ui: &AppWindow, rows: &VecModel<TimelineClip>, id: &str, name: &str) {
    for i in 0..rows.row_count() {
        let Some(mut row) = rows.row_data(i) else {
            continue;
        };
        if row.id.as_str() != id || row.name.as_str() == name {
            continue;
        }
        row.name = name.into();
        rows.set_row_data(i, row);
        if ui.get_timeline_selected() == i as i32 {
            ui.set_inspector_name(name.into());
        }
    }
}

/// Wire the Text chip and the inspector's title controls.
pub(super) fn wire(ui: &AppWindow, st: &super::VideoState) {
    // Text chip: a five-second title at the end of the top track, which
    // composites over everything, selected so it can be typed into at once.
    {
        let ui_weak = ui.as_weak();
        let project_slot = st.project.clone();
        let tl_clips = st.tl_clips.clone();
        let rec = st.recorder(ui);
        ui.on_timeline_add_text(move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            if tracks::refuse_locked(&ui, &tracks::rows_of(&rec.tracks), &[0]) {
                return;
            }
            if project_slot.borrow().is_none() {
                *project_slot.borrow_mut() = super::make_project(&ui_weak);
            }
            let mut slot = project_slot.borrow_mut();
            let Some(project) = slot.as_mut() else {
                return;
            };
            tracks::push_mutes(project, &rec.tracks);
            let before = rec.before(Some(&*project));
            let title = TitleRecord::default();
            let info = match project.append_title_clip(&title, 0, NEW_TITLE) {
                Ok(info) => info,
                Err(e) => {
                    drop(slot);
                    show_error(&ui, "Could not add text", format!("{e:#}"));
                    return;
                }
            };
            tl_clips.push(TimelineClip {
                id: info.id.0.as_str().into(),
                track: info.track as i32,
                start: info.start.as_secs_f32(),
                duration: info.duration.as_secs_f32(),
                inpoint: 0.0,
                name: title.name().into(),
                kind: ClipKind::Title,
                selected: false,
                thumb: Image::default(),
                rate: 1.0,
                wave: Image::default(),
                wave_secs: 0.0,
            });
            rec.record(
                Some(&*project),
                undo::StepKind::Add,
                Some(undo::Subject::Clip(info.id.0.clone())),
                before,
            );
            if let Some(d) = project.duration() {
                ui.set_timeline_duration(d.as_secs_f32());
            }
            project.refresh_preview();
            drop(slot);
            ui.invoke_timeline_select(tl_clips.row_count() as i32 - 1);
        });
    }

    // Any title control edited: stash the whole title; the UI tick writes it
    // and records one Text step (see `pending_title`).
    {
        let ui_weak = ui.as_weak();
        let tl_clips = st.tl_clips.clone();
        let sel_idx = st.sel_idx.clone();
        let pending_title = st.pending_title.clone();
        ui.on_title_changed(move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let Some(row) = usize::try_from(sel_idx.get())
                .ok()
                .and_then(|i| tl_clips.row_data(i))
            else {
                return;
            };
            if row.kind != ClipKind::Title {
                return;
            }
            *pending_title.borrow_mut() = Some((row.id.to_string(), from_inspector(&ui)));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_fonts_are_sans_at_a_size_bold_or_not() {
        assert_eq!(font_desc(48, true), "Sans Bold 48");
        assert_eq!(font_desc(12, false), "Sans 12");
        assert_eq!(font_desc(2, false), "Sans 8", "clamped to the smallest");
        assert_eq!(font_desc(900, true), "Sans Bold 400");
    }

    /// What Pango hands back, and a description a later version may write,
    /// both read as a size and a weight.
    #[test]
    fn title_fonts_read_back_as_a_size_and_a_weight() {
        assert_eq!(font_parts("Sans Bold 48"), (48, true));
        assert_eq!(font_parts("Sans 12"), (12, false));
        assert_eq!(font_parts("Serif Italic Bold 30.5"), (31, true));
        assert_eq!(font_parts("Sans"), (48, false), "no size: the default");
        assert_eq!(font_parts(""), (48, false));
        assert_eq!(font_parts("Sans 1000"), (400, false));
    }

    #[test]
    fn title_alignments_round_trip_through_their_indices() {
        for a in [TitleHAlign::Left, TitleHAlign::Center, TitleHAlign::Right] {
            assert_eq!(halign_at(halign_index(a)), a);
        }
        for a in [TitleVAlign::Top, TitleVAlign::Center, TitleVAlign::Bottom] {
            assert_eq!(valign_at(valign_index(a)), a);
        }
        assert_eq!(halign_at(7), TitleHAlign::Center, "out of range: centre");
    }
}
