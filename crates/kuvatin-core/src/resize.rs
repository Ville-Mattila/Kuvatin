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
    Percent {
        factor: f32,
    },
    /// Largest size that fits within width x height, preserving aspect ratio.
    FitBox {
        width: u32,
        height: u32,
    },
}

/// Ceiling on either output dimension. A hand-edited preset (factor = 1000,
/// width = u32::MAX) would otherwise reach `resize_exact`, whose w*h*4
/// allocation aborts the process (capacity overflow / OOM — not unwinding).
/// 32768 px per side (~4 GiB RGBA worst case) is beyond any sane output.
const MAX_TARGET_DIM: u32 = 32_768;

/// Compute the output dimensions for a `src_w` x `src_h` image. Never returns
/// 0, never exceeds [`MAX_TARGET_DIM`] per side — and when the ceiling bites,
/// both sides scale down together so the aspect ratio survives (clamping each
/// side on its own turned a 4:1 panorama into 3.3:1).
pub fn compute_target_dimensions(mode: ResizeMode, src_w: u32, src_h: u32) -> (u32, u32) {
    let (w, h) = compute_target_dimensions_unclamped(mode, src_w, src_h);
    if w <= MAX_TARGET_DIM && h <= MAX_TARGET_DIM {
        return (w, h);
    }
    let max = MAX_TARGET_DIM as f64;
    let scale = (max / w as f64).min(max / h as f64);
    let fit = |v: u32| ((v as f64 * scale).round() as u32).clamp(1, MAX_TARGET_DIM);
    (fit(w), fit(h))
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

/// Resample `img` to `w` x `h` (Lanczos3). Same size hands the image back
/// untouched. An RGBA image with any transparency is resampled with
/// premultiplied alpha: the filter otherwise averages the (usually black)
/// colour of fully transparent pixels into their opaque neighbours, and every
/// soft edge — logos, stickers, anti-aliased text — grows a dark halo.
pub fn resample(img: DynamicImage, w: u32, h: u32) -> DynamicImage {
    if img.width() == w && img.height() == h {
        return img;
    }
    match img {
        DynamicImage::ImageRgba8(rgba) if rgba.pixels().any(|p| p[3] < 255) => {
            DynamicImage::ImageRgba8(resample_premultiplied(rgba, w, h))
        }
        other => other.resize_exact(w, h, image::imageops::FilterType::Lanczos3),
    }
}

fn resample_premultiplied(mut rgba: image::RgbaImage, w: u32, h: u32) -> image::RgbaImage {
    for p in rgba.pixels_mut() {
        let a = p[3] as u32;
        if a < 255 {
            p[0] = ((p[0] as u32 * a + 127) / 255) as u8;
            p[1] = ((p[1] as u32 * a + 127) / 255) as u8;
            p[2] = ((p[2] as u32 * a + 127) / 255) as u8;
        }
    }
    let mut out = image::imageops::resize(&rgba, w, h, image::imageops::FilterType::Lanczos3);
    for p in out.pixels_mut() {
        let a = p[3] as u32;
        if a > 0 && a < 255 {
            // Lanczos ringing can leave a channel above its alpha; clamp.
            p[0] = ((p[0] as u32 * 255 + a / 2) / a).min(255) as u8;
            p[1] = ((p[1] as u32 * 255 + a / 2) / a).min(255) as u8;
            p[2] = ((p[2] as u32 * 255 + a / 2) / a).min(255) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_keeps_size() {
        assert_eq!(
            compute_target_dimensions(ResizeMode::None, 800, 600),
            (800, 600)
        );
    }

    #[test]
    fn percent_halves() {
        let m = ResizeMode::Percent { factor: 0.5 };
        assert_eq!(compute_target_dimensions(m, 800, 600), (400, 300));
    }

    #[test]
    fn fitbox_preserves_aspect() {
        let m = ResizeMode::FitBox {
            width: 1920,
            height: 1080,
        };
        assert_eq!(compute_target_dimensions(m, 4000, 3000), (1440, 1080));
    }

    #[test]
    fn pixels_width_only_keeps_aspect() {
        let m = ResizeMode::Pixels {
            width: Some(400),
            height: None,
            keep_aspect: true,
        };
        assert_eq!(compute_target_dimensions(m, 800, 600), (400, 300));
    }

    #[test]
    fn pixels_both_no_aspect_is_exact() {
        let m = ResizeMode::Pixels {
            width: Some(123),
            height: Some(45),
            keep_aspect: false,
        };
        assert_eq!(compute_target_dimensions(m, 800, 600), (123, 45));
    }

    #[test]
    fn never_zero() {
        let m = ResizeMode::Percent { factor: 0.0001 };
        let (w, h) = compute_target_dimensions(m, 800, 600);
        assert!(w >= 1 && h >= 1);
    }

    #[test]
    fn pixels_height_only_keeps_aspect() {
        let m = ResizeMode::Pixels {
            width: None,
            height: Some(300),
            keep_aspect: true,
        };
        assert_eq!(compute_target_dimensions(m, 800, 600), (400, 300));
    }

    #[test]
    fn pixels_one_side_without_aspect_keeps_the_other() {
        let w_only = ResizeMode::Pixels {
            width: Some(400),
            height: None,
            keep_aspect: false,
        };
        assert_eq!(compute_target_dimensions(w_only, 800, 600), (400, 600));
        let h_only = ResizeMode::Pixels {
            width: None,
            height: Some(100),
            keep_aspect: false,
        };
        assert_eq!(compute_target_dimensions(h_only, 800, 600), (800, 100));
    }

    #[test]
    fn pixels_both_with_aspect_fits_within() {
        let m = ResizeMode::Pixels {
            width: Some(400),
            height: Some(400),
            keep_aspect: true,
        };
        assert_eq!(compute_target_dimensions(m, 800, 600), (400, 300));
    }

    /// The size ceiling scales both sides together: a 4:1 panorama stays 4:1.
    #[test]
    fn ceiling_preserves_aspect() {
        let m = ResizeMode::Percent { factor: 10.0 };
        let (w, h) = compute_target_dimensions(m, 8000, 2000);
        assert_eq!((w, h), (MAX_TARGET_DIM, MAX_TARGET_DIM / 4));
        // A tall one limits on the height instead (1:5 within rounding).
        let (w, h) = compute_target_dimensions(m, 1000, 5000);
        assert_eq!(h, MAX_TARGET_DIM);
        assert!((w as f64 / h as f64 - 0.2).abs() < 1e-3, "{w}x{h}");
    }

    /// Downscaling a hard edge between an opaque colour and fully transparent
    /// black must not darken the boundary pixel: with straight alpha the
    /// filter mixed in the transparent pixels' black, giving red ≈ 128.
    #[test]
    fn transparent_edges_do_not_fringe() {
        let mut img = image::RgbaImage::new(8, 4);
        for (x, _y, p) in img.enumerate_pixels_mut() {
            *p = if x < 4 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 0, 0])
            };
        }
        let out = resample(DynamicImage::ImageRgba8(img), 4, 2).into_rgba8();
        // The boundary pixel is partly transparent but stays pure red.
        let edge = out.get_pixel(2, 0);
        assert!(edge[3] > 0 && edge[3] < 255, "boundary alpha: {edge:?}");
        assert!(edge[0] >= 250, "no dark fringe: {edge:?}");
        assert_eq!(out.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(out.get_pixel(3, 0)[3], 0);
    }

    /// Same size is an identity (no resample, no copy).
    #[test]
    fn same_size_passes_through() {
        let img = DynamicImage::ImageRgba8(image::RgbaImage::new(5, 7));
        let out = resample(img, 5, 7);
        assert_eq!((out.width(), out.height()), (5, 7));
    }
}
