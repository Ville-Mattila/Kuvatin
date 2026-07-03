use image::DynamicImage;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResizeMode {
    None,
    /// Explicit pixels; either dimension may be omitted. With `keep_aspect`,
    /// a missing dimension is derived; if both given and keep_aspect, fit within.
    Pixels {
        width: Option<u32>,
        height: Option<u32>,
        keep_aspect: bool,
    },
    /// Scale both dimensions by `factor` (1.0 = unchanged).
    Percent { factor: f32 },
    /// Largest size that fits within width x height, preserving aspect ratio.
    FitBox { width: u32, height: u32 },
}

/// Ceiling on either output dimension. A hand-edited preset (factor = 1000,
/// width = u32::MAX) would otherwise reach `resize_exact`, whose w*h*4
/// allocation aborts the process (capacity overflow / OOM — not unwinding).
/// 32768 px per side (~4 GiB RGBA worst case) is beyond any sane output.
const MAX_TARGET_DIM: u32 = 32_768;

/// Compute the output dimensions for a `src_w` x `src_h` image. Never returns
/// 0, never exceeds [`MAX_TARGET_DIM`] per side.
pub fn compute_target_dimensions(mode: ResizeMode, src_w: u32, src_h: u32) -> (u32, u32) {
    let (w, h) = compute_target_dimensions_unclamped(mode, src_w, src_h);
    (w.min(MAX_TARGET_DIM), h.min(MAX_TARGET_DIM))
}

fn compute_target_dimensions_unclamped(mode: ResizeMode, src_w: u32, src_h: u32) -> (u32, u32) {
    let clamp1 = |v: u32| v.max(1);
    match mode {
        ResizeMode::None => (src_w, src_h),
        ResizeMode::Percent { factor } => {
            let f = factor.max(0.0);
            (
                clamp1((src_w as f32 * f).round() as u32),
                clamp1((src_h as f32 * f).round() as u32),
            )
        }
        ResizeMode::FitBox { width, height } => {
            fit_within(src_w, src_h, width.max(1), height.max(1))
        }
        ResizeMode::Pixels {
            width,
            height,
            keep_aspect,
        } => match (width, height) {
            (Some(w), Some(h)) if keep_aspect => fit_within(src_w, src_h, w.max(1), h.max(1)),
            (Some(w), Some(h)) => (clamp1(w), clamp1(h)),
            (Some(w), None) if keep_aspect => {
                let w = w.max(1);
                let h = (src_h as f32 * (w as f32 / src_w as f32)).round() as u32;
                (w, clamp1(h))
            }
            (None, Some(h)) if keep_aspect => {
                let h = h.max(1);
                let w = (src_w as f32 * (h as f32 / src_h as f32)).round() as u32;
                (clamp1(w), h)
            }
            (Some(w), None) => (clamp1(w), src_h),
            (None, Some(h)) => (src_w, clamp1(h)),
            (None, None) => (src_w, src_h),
        },
    }
}

fn fit_within(src_w: u32, src_h: u32, box_w: u32, box_h: u32) -> (u32, u32) {
    let scale = (box_w as f32 / src_w as f32).min(box_h as f32 / src_h as f32);
    let w = (src_w as f32 * scale).round() as u32;
    let h = (src_h as f32 * scale).round() as u32;
    (w.max(1), h.max(1))
}

/// Resample `img` to `w` x `h`. v1 uses image's Lanczos3; swap to
/// fast_image_resize here later without touching callers.
pub fn resample(img: &DynamicImage, w: u32, h: u32) -> DynamicImage {
    if img.width() == w && img.height() == h {
        return img.clone();
    }
    img.resize_exact(w, h, image::imageops::FilterType::Lanczos3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_keeps_size() {
        assert_eq!(compute_target_dimensions(ResizeMode::None, 800, 600), (800, 600));
    }

    #[test]
    fn percent_halves() {
        let m = ResizeMode::Percent { factor: 0.5 };
        assert_eq!(compute_target_dimensions(m, 800, 600), (400, 300));
    }

    #[test]
    fn fitbox_preserves_aspect() {
        let m = ResizeMode::FitBox { width: 1920, height: 1080 };
        assert_eq!(compute_target_dimensions(m, 4000, 3000), (1440, 1080));
    }

    #[test]
    fn pixels_width_only_keeps_aspect() {
        let m = ResizeMode::Pixels { width: Some(400), height: None, keep_aspect: true };
        assert_eq!(compute_target_dimensions(m, 800, 600), (400, 300));
    }

    #[test]
    fn pixels_both_no_aspect_is_exact() {
        let m = ResizeMode::Pixels { width: Some(123), height: Some(45), keep_aspect: false };
        assert_eq!(compute_target_dimensions(m, 800, 600), (123, 45));
    }

    #[test]
    fn never_zero() {
        let m = ResizeMode::Percent { factor: 0.0001 };
        let (w, h) = compute_target_dimensions(m, 800, 600);
        assert!(w >= 1 && h >= 1);
    }
}
