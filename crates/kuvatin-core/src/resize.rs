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
/// 32768 px per side is beyond any sane output.
const MAX_TARGET_DIM: u32 = 32_768;

/// Ceiling on the output's total pixels. The per-side limit is not enough on
/// its own: 32768 squared is a billion pixels, four gigabytes of RGBA, and the
/// allocation that fails there aborts rather than unwinding, so the batch
/// cannot turn it into a per-file error. 2^28 pixels is one gigabyte of RGBA —
/// the same budget a decode gets from [`crate::pipeline::MAX_DECODE_BYTES`] —
/// and still allows a full-width 4:1 panorama at the per-side ceiling.
pub const MAX_TARGET_PIXELS: u64 = 1 << 28;

/// Compute the output dimensions for a `src_w` x `src_h` image. Never returns
/// 0, never exceeds [`MAX_TARGET_DIM`] per side or [`MAX_TARGET_PIXELS`] in
/// total — and when a ceiling bites, both sides scale down together so the
/// aspect ratio survives (clamping each side on its own turned a 4:1 panorama
/// into 3.3:1).
pub fn compute_target_dimensions(mode: ResizeMode, src_w: u32, src_h: u32) -> (u32, u32) {
    let (w, h) = compute_target_dimensions_unclamped(mode, src_w, src_h);
    let (w, h) = fit_within_sides(w, h);
    fit_within_area(w, h)
}

/// Scale (w, h) down together until neither side exceeds [`MAX_TARGET_DIM`].
fn fit_within_sides(w: u32, h: u32) -> (u32, u32) {
    if w <= MAX_TARGET_DIM && h <= MAX_TARGET_DIM {
        return (w, h);
    }
    let max = MAX_TARGET_DIM as f64;
    let scale = (max / w as f64).min(max / h as f64);
    let fit = |v: u32| ((v as f64 * scale).round() as u32).clamp(1, MAX_TARGET_DIM);
    (fit(w), fit(h))
}

/// Scale (w, h) down together until their product is within
/// [`MAX_TARGET_PIXELS`]. Rounding can leave the product a hair over, so the
/// result is trimmed by a pixel per side until it fits.
fn fit_within_area(w: u32, h: u32) -> (u32, u32) {
    let area = w as u64 * h as u64;
    if area <= MAX_TARGET_PIXELS {
        return (w, h);
    }
    let scale = (MAX_TARGET_PIXELS as f64 / area as f64).sqrt();
    let fit = |v: u32| ((v as f64 * scale).floor() as u32).clamp(1, MAX_TARGET_DIM);
    let (mut w, mut h) = (fit(w), fit(h));
    while w as u64 * h as u64 > MAX_TARGET_PIXELS && (w > 1 || h > 1) {
        if w >= h {
            w -= 1;
        } else {
            h -= 1;
        }
    }
    (w, h)
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
/// untouched. Any image carrying transparency is resampled with premultiplied
/// alpha: the filter otherwise averages the (usually black) colour of fully
/// transparent pixels into their opaque neighbours, and every soft edge —
/// logos, stickers, anti-aliased text — grows a dark halo.
pub fn resample(img: DynamicImage, w: u32, h: u32) -> DynamicImage {
    if img.width() == w && img.height() == h {
        return img;
    }
    // Every type a supported input can decode to that carries alpha. Gating
    // this on RGBA8 alone left grey-with-alpha and 16-bit images on the
    // straight-alpha path, where they grew exactly the halo described above.
    // Float pixels cannot arrive from any input format Kuvatin accepts.
    match img {
        DynamicImage::ImageRgba8(b) if any_transparency(&b) => {
            DynamicImage::ImageRgba8(resample_premultiplied(b, w, h))
        }
        DynamicImage::ImageLumaA8(b) if any_transparency(&b) => {
            DynamicImage::ImageLumaA8(resample_premultiplied(b, w, h))
        }
        DynamicImage::ImageRgba16(b) if any_transparency(&b) => {
            DynamicImage::ImageRgba16(resample_premultiplied(b, w, h))
        }
        DynamicImage::ImageLumaA16(b) if any_transparency(&b) => {
            DynamicImage::ImageLumaA16(resample_premultiplied(b, w, h))
        }
        other => other.resize_exact(w, h, image::imageops::FilterType::Lanczos3),
    }
}

/// One channel, as far as premultiplication cares: its full-scale value and
/// the conversions to and from the arithmetic type.
trait Channel: Copy {
    const FULL: f32;
    fn as_f32(self) -> f32;
    fn from_f32(v: f32) -> Self;
}

impl Channel for u8 {
    const FULL: f32 = u8::MAX as f32;
    fn as_f32(self) -> f32 {
        self as f32
    }
    fn from_f32(v: f32) -> Self {
        v.clamp(0.0, Self::FULL).round() as u8
    }
}

impl Channel for u16 {
    const FULL: f32 = u16::MAX as f32;
    fn as_f32(self) -> f32 {
        self as f32
    }
    fn from_f32(v: f32) -> Self {
        v.clamp(0.0, Self::FULL).round() as u16
    }
}

/// True when any pixel is less than fully opaque; alpha is the last channel
/// of every type this is called with.
fn any_transparency<P, C>(img: &image::ImageBuffer<P, Vec<C>>) -> bool
where
    P: image::Pixel<Subpixel = C> + 'static,
    C: Channel + image::Primitive + 'static,
{
    let last = P::CHANNEL_COUNT as usize - 1;
    img.pixels().any(|p| p.channels()[last].as_f32() < C::FULL)
}

/// Premultiply, resample, then undo the premultiplication.
fn resample_premultiplied<P, C>(
    mut img: image::ImageBuffer<P, Vec<C>>,
    w: u32,
    h: u32,
) -> image::ImageBuffer<P, Vec<C>>
where
    P: image::Pixel<Subpixel = C> + 'static,
    C: Channel + image::Primitive + 'static,
{
    let last = P::CHANNEL_COUNT as usize - 1;
    for p in img.pixels_mut() {
        let channels = p.channels_mut();
        let a = channels[last].as_f32() / C::FULL;
        if a < 1.0 {
            for c in &mut channels[..last] {
                *c = C::from_f32(c.as_f32() * a);
            }
        }
    }
    let mut out = image::imageops::resize(&img, w, h, image::imageops::FilterType::Lanczos3);
    for p in out.pixels_mut() {
        let channels = p.channels_mut();
        let a = channels[last].as_f32() / C::FULL;
        if a > 0.0 && a < 1.0 {
            for c in &mut channels[..last] {
                // Lanczos ringing can leave a channel above its own alpha.
                *c = C::from_f32((c.as_f32() / a).min(C::FULL));
            }
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

    /// The per-side ceiling alone still allows 32768x32768 — a billion pixels,
    /// four gigabytes of RGBA, an allocation that aborts the process instead of
    /// failing the file. The area ceiling is what actually bounds it.
    #[test]
    fn the_area_ceiling_bounds_a_square_blow_up() {
        let m = ResizeMode::Percent { factor: 100.0 };
        let (w, h) = compute_target_dimensions(m, 4000, 4000);
        assert!(
            w as u64 * h as u64 <= MAX_TARGET_PIXELS,
            "{w}x{h} is past the budget"
        );
        assert_eq!(w, h, "a square input stays square");
    }

    #[test]
    fn the_area_ceiling_keeps_the_aspect_ratio() {
        let m = ResizeMode::Percent { factor: 100.0 };
        let (w, h) = compute_target_dimensions(m, 8000, 2000);
        assert!(
            w as u64 * h as u64 <= MAX_TARGET_PIXELS,
            "{w}x{h} is past the budget"
        );
        assert!((w as f64 / h as f64 - 4.0).abs() < 1e-3, "{w}x{h}");
    }

    /// An explicit target within the budget is passed through untouched.
    #[test]
    fn ordinary_targets_are_left_alone() {
        let m = ResizeMode::Pixels {
            width: Some(6000),
            height: Some(4000),
            keep_aspect: false,
        };
        assert_eq!(compute_target_dimensions(m, 800, 600), (6000, 4000));
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

    /// The same hazard, in the other pixel types a PNG or TIFF can decode to.
    /// These fell through to straight alpha and grew exactly the dark fringe
    /// the premultiplied path exists to prevent.
    #[test]
    fn transparent_edges_do_not_fringe_in_grey_or_sixteen_bit() {
        // Grey with alpha: white on the left, transparent black on the right.
        let mut grey = image::ImageBuffer::<image::LumaA<u8>, Vec<u8>>::new(8, 4);
        for (x, _y, p) in grey.enumerate_pixels_mut() {
            *p = if x < 4 {
                image::LumaA([255, 255])
            } else {
                image::LumaA([0, 0])
            };
        }
        let out = resample(DynamicImage::ImageLumaA8(grey), 4, 2);
        let out = match out {
            DynamicImage::ImageLumaA8(b) => b,
            other => panic!("pixel type changed: {:?}", other.color()),
        };
        let edge = out.get_pixel(2, 0);
        assert!(edge[1] > 0 && edge[1] < 255, "boundary alpha: {edge:?}");
        assert!(edge[0] >= 250, "grey darkened at the edge: {edge:?}");

        // 16-bit colour with alpha: full red on the left, transparent on the right.
        let mut deep = image::ImageBuffer::<image::Rgba<u16>, Vec<u16>>::new(8, 4);
        for (x, _y, p) in deep.enumerate_pixels_mut() {
            *p = if x < 4 {
                image::Rgba([65_535, 0, 0, 65_535])
            } else {
                image::Rgba([0, 0, 0, 0])
            };
        }
        let out = resample(DynamicImage::ImageRgba16(deep), 4, 2);
        let out = match out {
            DynamicImage::ImageRgba16(b) => b,
            other => panic!("depth changed: {:?}", other.color()),
        };
        let edge = out.get_pixel(2, 0);
        assert!(edge[3] > 0 && edge[3] < 65_535, "boundary alpha: {edge:?}");
        assert!(edge[0] >= 64_000, "red darkened at the edge: {edge:?}");
    }

    /// A fully opaque image of any type takes the plain path and is unchanged
    /// by the premultiply round trip.
    #[test]
    fn opaque_images_are_untouched_by_the_alpha_handling() {
        let mut grey = image::ImageBuffer::<image::LumaA<u8>, Vec<u8>>::new(4, 4);
        for p in grey.pixels_mut() {
            *p = image::LumaA([200, 255]);
        }
        let out = resample(DynamicImage::ImageLumaA8(grey), 2, 2);
        let out = out.as_luma_alpha8().expect("still grey with alpha").clone();
        for p in out.pixels() {
            assert_eq!(p.0, [200, 255], "opaque grey shifted: {p:?}");
        }
    }

    /// Same size is an identity (no resample, no copy).
    #[test]
    fn same_size_passes_through() {
        let img = DynamicImage::ImageRgba8(image::RgbaImage::new(5, 7));
        let out = resample(img, 5, 7);
        assert_eq!((out.width(), out.height()), (5, 7));
    }
}
