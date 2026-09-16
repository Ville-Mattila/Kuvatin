//! How big the window opens: what it asks for, unless the desktop is smaller.

/// app.slint's `min-width` / `min-height`. Below this the layout breaks, so
/// a small desktop gets a window it can scroll rather than one it cannot use.
pub const MIN_W: u32 = 860;
pub const MIN_H: u32 = 560;

/// The title bar and borders sit OUTSIDE the size we set: `set_size` gives the
/// client area, while the work area has to hold the whole window. Without this
/// allowance the cap never fires on a 1920x1080 screen, where a 1008px client
/// plus a title bar is taller than the 1032px work area by about the height of
/// that title bar. Both numbers are deliberate over-estimates: a title bar
/// grows with the display's scaling and the user's theme, and opening a few
/// pixels smaller than necessary costs nothing, while opening too tall puts the
/// bottom edge under the taskbar where it cannot be dragged back.
const FRAME_W: u32 = 16;
const FRAME_H: u32 = 48;

/// What the window should open at: what it asked for, capped to the desktop's
/// usable area less the window's own frame, floored at the size the layout
/// needs.
pub fn opening_size(want_w: u32, want_h: u32, area_w: u32, area_h: u32) -> (u32, u32) {
    (
        want_w.min(area_w.saturating_sub(FRAME_W)).max(MIN_W),
        want_h.min(area_h.saturating_sub(FRAME_H)).max(MIN_H),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_for_the_preferred_size_when_the_desktop_has_room() {
        assert_eq!(opening_size(1584, 1008, 2560, 1400), (1584, 1008));
    }

    #[test]
    fn shrinks_to_the_work_area_rather_than_hanging_off_it() {
        // 1920x1080 with a default-height taskbar leaves a 1920x1032 work
        // area. The preferred 1008px client height looks like it fits, and
        // does not: the title bar goes on top of it. This is the case the
        // whole rule exists for.
        assert_eq!(opening_size(1584, 1008, 1920, 1032), (1584, 984));
        // A 1366x768 laptop screen is smaller than the window wants in both
        // directions, and a window taller than the desktop cannot be moved
        // back into view by dragging its title bar, so it is capped to what
        // fits.
        assert_eq!(opening_size(1584, 1008, 1366, 768), (1350, 720));
    }

    #[test]
    fn never_goes_below_what_the_window_can_be_dragged_to() {
        // The floor is app.slint's min-width / min-height.
        assert_eq!(opening_size(1584, 1008, 400, 300), (MIN_W, MIN_H));
    }
}
