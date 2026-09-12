//! The Explorer context menu's content: which items the "Kuvatin" submenu
//! holds for a preset store and what each item runs. Shared by the classic
//! registry menu (Windows 10, and "Show more options" on Windows 11) and the
//! Windows 11 context-menu handler, so the two can never disagree.

use crate::preset::{validate_preset_name, PresetStore};

/// What a menu item runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// A headless preset over the selection (`--preset`).
    Preset(String),
    /// Render the selected frames' sequence(s) to MP4 (`--sequence-mp4`).
    SequenceMp4,
    /// Open the selection in the GUI.
    Gui,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    /// Registry key name under a command store. Explorer lists subcommands in
    /// key order, so the id carries the position.
    pub id: String,
    pub label: String,
    pub action: Action,
    /// Draw a separator above this item.
    pub separator_before: bool,
}

/// The submenu for a selection of images or folders: every preset in store
/// order, then a separator and the fixed actions. A name the GUI would refuse
/// (see [`validate_preset_name`]; only a hand-edited presets.toml can carry
/// one) cannot be quoted on a command line and is left out.
pub fn menu_items(store: &PresetStore) -> Vec<MenuItem> {
    let mut items: Vec<MenuItem> = store
        .presets
        .iter()
        .filter(|p| validate_preset_name(&p.name).is_ok())
        .enumerate()
        .map(|(i, p)| MenuItem {
            id: format!("Kuvatin.{i:02}.Preset"),
            label: p.name.clone(),
            action: Action::Preset(p.name.clone()),
            separator_before: false,
        })
        .collect();
    items.push(MenuItem {
        id: "Kuvatin.90.SequenceMp4".into(),
        label: "Render image sequence to MP4".into(),
        action: Action::SequenceMp4,
        separator_before: true,
    });
    items.push(MenuItem {
        id: "Kuvatin.91.Open".into(),
        label: "Open in Kuvatin\u{2026}".into(),
        action: Action::Gui,
        separator_before: false,
    });
    items
}

/// The submenu for sequence-only frames (`.exr`), which the image presets
/// can't read: just the sequence render.
pub fn frame_items(items: &[MenuItem]) -> Vec<MenuItem> {
    items
        .iter()
        .filter(|i| i.action == Action::SequenceMp4)
        .cloned()
        .map(|mut i| {
            i.separator_before = false;
            i
        })
        .collect()
}

/// The arguments an item passes to `kuvatin.exe` ahead of the selected paths
/// (empty for the GUI: a bare path list opens the window).
pub fn action_args(action: &Action) -> Vec<String> {
    let mut args = match action {
        Action::Preset(preset) => vec!["--preset".into(), preset.clone()],
        Action::SequenceMp4 => vec!["--sequence-mp4".into()],
        Action::Gui => Vec::new(),
    };
    // Everything after this is a path, however it is spelled. Without it a
    // file named "--sequence-mp4.png" is parsed as a flag and the run fails.
    args.push("--".into());
    args
}

/// The registry command line a static verb runs. `token` is Explorer's
/// placeholder for the clicked item: `%1` (file/folder) or `%V` (background
/// folder).
pub fn command_line(exe: &str, action: &Action, token: &str) -> String {
    // The `--` is the flag terminator: Explorer substitutes the clicked file
    // for the token, and a file named like a flag would otherwise be parsed
    // as one and fail the whole run.
    match action {
        Action::Preset(preset) => format!("\"{exe}\" --preset \"{preset}\" -- \"{token}\""),
        Action::SequenceMp4 => format!("\"{exe}\" --sequence-mp4 -- \"{token}\""),
        Action::Gui => format!("\"{exe}\" -- \"{token}\""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lines_quote_the_exe_and_pass_the_item_token() {
        assert_eq!(
            command_line(
                r"C:\Program Files\Kuvatin\kuvatin.exe",
                &Action::Preset("Convert to WebP".into()),
                "%1"
            ),
            r#""C:\Program Files\Kuvatin\kuvatin.exe" --preset "Convert to WebP" -- "%1""#
        );
        assert_eq!(
            command_line(r"C:\k\kuvatin.exe", &Action::SequenceMp4, "%1"),
            r#""C:\k\kuvatin.exe" --sequence-mp4 -- "%1""#
        );
        // The GUI item has no flag; background verbs get the folder via %V.
        assert_eq!(
            command_line(r"C:\k\kuvatin.exe", &Action::Gui, "%V"),
            r#""C:\k\kuvatin.exe" -- "%V""#
        );
    }

    /// Every form ends with the flag terminator, so a selected file whose name
    /// begins with a dash is a path and not a flag.
    #[test]
    fn action_args_match_the_command_lines() {
        assert_eq!(
            action_args(&Action::Preset("Convert to WebP".into())),
            ["--preset", "Convert to WebP", "--"]
        );
        assert_eq!(action_args(&Action::SequenceMp4), ["--sequence-mp4", "--"]);
        assert_eq!(action_args(&Action::Gui), ["--"]);
    }

    /// The submenu mirrors the store: every preset in order (position carried
    /// by the key name, since Explorer sorts subcommands by key), then a
    /// separator and the fixed actions; an unquotable name stays GUI-only.
    #[test]
    fn menu_mirrors_the_preset_store() {
        let mut store = PresetStore::builtin();
        store.presets.push(crate::preset::Preset {
            name: "Say \"cheese\"".into(),
            job: Default::default(),
        });
        let items = menu_items(&store);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Compress PNG",
                "Convert to WebP",
                "Resize to 1080p",
                "Resize to 50%",
                "Render image sequence to MP4",
                "Open in Kuvatin\u{2026}"
            ]
        );
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "key order must equal menu order");
        assert!(items[4].separator_before && !items[3].separator_before);
        assert_eq!(items[0].action, Action::Preset("Compress PNG".into()));

        let frames = frame_items(&items);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].action, Action::SequenceMp4);
        assert!(
            !frames[0].separator_before,
            "alone at the top: no separator"
        );
    }
}
