use image::DynamicImage;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    #[default]
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CropMode {
    None,
    /// Crop a fixed pixel rectangle (clamped to the image), positioned by anchor.
    FixedSize { width: u32, height: u32, anchor: Anchor },
    /// Crop the largest w:h rectangle that fits, positioned by anchor.
    AspectRatio { w: u32, h: u32, anchor: Anchor },
    /// An absolute pixel rectangle (origin x,y, size width,height), clamped to
    /// the image. Used for interactive per-image crops drawn in the GUI.
    Rect { x: u32, y: u32, width: u32, height: u32 },
}

/// (x, y, width, height) of the crop within a `src_w` x `src_h` image.
/// Zero-dimension inputs pass through untouched (nothing to crop; the old
/// `clamp(1, 0)` panicked on them).
pub fn compute_crop_rect(mode: CropMode, src_w: u32, src_h: u32) -> (u32, u32, u32, u32) {
    if src_w == 0 || src_h == 0 {
        return (0, 0, src_w, src_h);
    }
    match mode {
        CropMode::None => (0, 0, src_w, src_h),
        CropMode::FixedSize { width, height, anchor } => {
            let w = width.clamp(1, src_w);
            let h = height.clamp(1, src_h);
            place(anchor, src_w, src_h, w, h)
        }
        CropMode::AspectRatio { w, h, anchor } => {
            let (w, h) = (w.max(1), h.max(1));
            // Saturate the u64->u32 casts: an absurd ratio (1:100000) on a wide
            // image overflows u32 and used to wrap into a degenerate 1-px crop.
            let sat = |v: u64| v.min(u32::MAX as u64) as u32;
            let by_width = (src_w, sat(src_w as u64 * h as u64 / w as u64));
            let (cw, ch) = if by_width.1 <= src_h {
                by_width
            } else {
                (sat(src_h as u64 * w as u64 / h as u64), src_h)
            };
            place(anchor, src_w, src_h, cw.clamp(1, src_w), ch.clamp(1, src_h))
        }
        CropMode::Rect { x, y, width, height } => {
            let x = x.min(src_w.saturating_sub(1));
            let y = y.min(src_h.saturating_sub(1));
            let w = width.clamp(1, src_w - x);
            let h = height.clamp(1, src_h - y);
            (x, y, w, h)
        }
    }
}

fn place(anchor: Anchor, src_w: u32, src_h: u32, w: u32, h: u32) -> (u32, u32, u32, u32) {
    let max_x = src_w - w;
    let max_y = src_h - h;
    let (x, y) = match anchor {
        Anchor::TopLeft => (0, 0),
        Anchor::Top => (max_x / 2, 0),
        Anchor::TopRight => (max_x, 0),
        Anchor::Left => (0, max_y / 2),
        Anchor::Center => (max_x / 2, max_y / 2),
        Anchor::Right => (max_x, max_y / 2),
        Anchor::BottomLeft => (0, max_y),
        Anchor::Bottom => (max_x / 2, max_y),
        Anchor::BottomRight => (max_x, max_y),
    };
    (x, y, w, h)
}

/// Apply a crop, returning a new image. `None` returns a clone.
pub fn apply_crop(img: &DynamicImage, mode: CropMode) -> DynamicImage {
    if let CropMode::None = mode {
        return img.clone();
    }
    let (x, y, w, h) = compute_crop_rect(mode, img.width(), img.height());
    img.crop_imm(x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_is_full_image() {
        assert_eq!(compute_crop_rect(CropMode::None, 800, 600), (0, 0, 800, 600));
    }

    #[test]
    fn fixed_center_is_centered() {
        let m = CropMode::FixedSize { width: 400, height: 200, anchor: Anchor::Center };
        assert_eq!(compute_crop_rect(m, 800, 600), (200, 200, 400, 200));
    }

    #[test]
    fn fixed_top_left() {
        let m = CropMode::FixedSize { width: 100, height: 100, anchor: Anchor::TopLeft };
        assert_eq!(compute_crop_rect(m, 800, 600), (0, 0, 100, 100));
    }

    #[test]
    fn fixed_size_clamps_to_image() {
        let m = CropMode::FixedSize { width: 9999, height: 9999, anchor: Anchor::Center };
        assert_eq!(compute_crop_rect(m, 800, 600), (0, 0, 800, 600));
    }

    #[test]
    fn aspect_square_from_landscape() {
        let m = CropMode::AspectRatio { w: 1, h: 1, anchor: Anchor::Center };
        assert_eq!(compute_crop_rect(m, 800, 600), (100, 0, 600, 600));
    }

    #[test]
    fn rect_within_bounds_is_exact() {
        let m = CropMode::Rect { x: 100, y: 50, width: 200, height: 150 };
        assert_eq!(compute_crop_rect(m, 800, 600), (100, 50, 200, 150));
    }

    #[test]
    fn rect_clamped_when_overflowing() {
        let m = CropMode::Rect { x: 700, y: 500, width: 400, height: 400 };
        // origin stays, size clamped to remaining 100x100
        assert_eq!(compute_crop_rect(m, 800, 600), (700, 500, 100, 100));
    }

    #[test]
    fn rect_origin_clamped_inside_image() {
        let m = CropMode::Rect { x: 9999, y: 9999, width: 50, height: 50 };
        let (x, y, w, h) = compute_crop_rect(m, 800, 600);
        assert!(x < 800 && y < 600 && w >= 1 && h >= 1);
    }
}
