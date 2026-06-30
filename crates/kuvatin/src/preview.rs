//! Pure geometry helpers for the image viewer / crop preview.

/// Size of a preview box that preserves the source aspect ratio and fits within
/// `max_w` x `max_h`, never upscaling past the source. Returns (w, h) in px, each >= 1.
pub fn preview_box(src_w: u32, src_h: u32, max_w: f32, max_h: f32) -> (f32, f32) {
    if src_w == 0 || src_h == 0 {
        return (1.0, 1.0);
    }
    let scale = (max_w / src_w as f32).min(max_h / src_h as f32).min(1.0);
    ((src_w as f32 * scale).max(1.0), (src_h as f32 * scale).max(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landscape_fits_width() {
        let (w, h) = preview_box(2000, 1000, 560.0, 420.0);
        assert!((w - 560.0).abs() < 0.5, "w={w}");
        assert!((h - 280.0).abs() < 0.5, "h={h}");
    }

    #[test]
    fn small_image_not_upscaled() {
        assert_eq!(preview_box(100, 80, 560.0, 420.0), (100.0, 80.0));
    }

    #[test]
    fn zero_dims_safe() {
        assert_eq!(preview_box(0, 0, 560.0, 420.0), (1.0, 1.0));
    }
}
