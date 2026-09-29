//! Text overlays as the interface shows them.

use crate::gui::{AppWindow, TimelineClip};
use slint::{Model, VecModel};

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
