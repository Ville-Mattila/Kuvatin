//! How big the window opens: what it asks for, unless the desktop is smaller.

/// app.slint's `min-width` / `min-height`. Below this the layout breaks, so
/// a small desktop gets a window it can scroll rather than one it cannot use.
pub const MIN_W: u32 = 860;
pub const MIN_H: u32 = 560;

/// What the window should open at: what it asked for, capped to the desktop's
/// usable area, floored at the size the layout needs.
pub fn opening_size(want_w: u32, want_h: u32, area_w: u32, area_h: u32) -> (u32, u32) {
    (want_w.min(area_w).max(MIN_W), want_h.min(area_h).max(MIN_H))
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
        // 1920x1080 with a default-height taskbar: the work area is
        // 1920x1032, which already has room for the preferred 1008px
        // height, so nothing is capped here — this only confirms that
        // "capped" never means "grown to fill".
        assert_eq!(opening_size(1584, 1008, 1920, 1032), (1584, 1008));
        // A 1366x768 laptop screen is smaller than the window wants in both
        // directions, and a window taller than the desktop cannot be moved
        // back into view by dragging its title bar, so it is capped to what
        // fits.
        assert_eq!(opening_size(1584, 1008, 1366, 768), (1366, 768));
    }

    #[test]
    fn never_goes_below_what_the_window_can_be_dragged_to() {
        // The floor is app.slint's min-width / min-height.
        assert_eq!(opening_size(1584, 1008, 400, 300), (MIN_W, MIN_H));
    }
}
