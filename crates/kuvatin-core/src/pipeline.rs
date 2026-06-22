use crate::crop::{apply_crop, CropMode};
use crate::format::OutputFormat;
use crate::naming::{ensure_unique, render_output_path, OutputPolicy};
use crate::resize::{compute_target_dimensions, resample, ResizeMode};
use crate::{CoreError, CoreResult};
use image::DynamicImage;
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::path::{Path, PathBuf};

/// PNG size-optimization mode. Only affects `OutputFormat::Png` output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PngOptimize {
    /// Plain PNG (image crate), no extra optimization.
    #[default]
    None,
    /// Lossless re-optimization via oxipng (pixels identical, alpha preserved).
    Lossless,
    /// Lossy palette quantization via libimagequant (uses Job.quality), then a
    /// final lossless oxipng pass. Big size wins; alpha preserved.
    Lossy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub resize: ResizeMode,
    pub crop: CropMode,
    pub format: OutputFormat,
    /// 0-100; ignored for lossless formats.
    pub quality: u8,
    /// PNG-only size optimization mode.
    #[serde(default)]
    pub png: PngOptimize,
    pub output: OutputPolicy,
}

impl Default for Job {
    fn default() -> Self {
        Job {
            resize: ResizeMode::None,
            crop: CropMode::None,
            format: OutputFormat::Png,
            quality: 90,
            png: PngOptimize::None,
            output: OutputPolicy::default(),
        }
    }
}

/// Apply the op pipeline (crop -> resize) to an in-memory image. Returns the
/// transformed image and its final (w, h).
///
/// Crop runs first so a crop rectangle expressed in the *source* image's pixels
/// (e.g. an interactive per-image crop) selects the right region; the resize
/// then scales that cropped region to the target dimensions.
pub fn process_image(img: &DynamicImage, job: &Job) -> (DynamicImage, u32, u32) {
    let cropped = apply_crop(img, job.crop);
    let (tw, th) = compute_target_dimensions(job.resize, cropped.width(), cropped.height());
    let resized = resample(&cropped, tw, th);
    let (w, h) = (resized.width(), resized.height());
    (resized, w, h)
}

/// Encode an image to bytes in the requested format/quality.
///
/// `png` selects PNG-only size optimization (lossless via oxipng or lossy via
/// libimagequant); it is ignored for non-PNG formats.
pub fn encode(
    img: &DynamicImage,
    format: OutputFormat,
    quality: u8,
    png: PngOptimize,
) -> CoreResult<Vec<u8>> {
    match format {
        OutputFormat::Jpeg => {
            let mut buf = Vec::new();
            let rgb = img.to_rgb8();
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(
                Cursor::new(&mut buf),
                quality,
            );
            enc.encode_image(&rgb).map_err(|e| CoreError::Encode(e.to_string()))?;
            Ok(buf)
        }
        OutputFormat::Webp => {
            let rgba = img.to_rgba8();
            let encoder = webp::Encoder::from_rgba(&rgba, rgba.width(), rgba.height());
            let mem = encoder.encode(quality as f32);
            Ok(mem.to_vec())
        }
        OutputFormat::Png => encode_png(img, png, quality),
        other => {
            let fmt = match other {
                OutputFormat::Bmp => image::ImageFormat::Bmp,
                OutputFormat::Tiff => image::ImageFormat::Tiff,
                OutputFormat::Gif => image::ImageFormat::Gif,
                OutputFormat::Png | OutputFormat::Jpeg | OutputFormat::Webp => unreachable!(),
            };
            let mut buf = Vec::new();
            img.write_to(&mut Cursor::new(&mut buf), fmt)
                .map_err(|e| CoreError::Encode(e.to_string()))?;
            Ok(buf)
        }
    }
}

/// Encode a plain PNG via the image crate (no extra optimization).
fn encode_png_plain(img: &DynamicImage) -> CoreResult<Vec<u8>> {
    let mut buf = Vec::new();
    img.write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|e| CoreError::Encode(e.to_string()))?;
    Ok(buf)
}

/// Encode a PNG with the requested size-optimization mode. Alpha is preserved in
/// all modes.
fn encode_png(img: &DynamicImage, mode: PngOptimize, quality: u8) -> CoreResult<Vec<u8>> {
    match mode {
        PngOptimize::None => encode_png_plain(img),
        PngOptimize::Lossless => {
            let raw = encode_png_plain(img)?;
            let opts = oxipng::Options::from_preset(2);
            oxipng::optimize_from_memory(&raw, &opts)
                .map_err(|e| CoreError::Encode(e.to_string()))
        }
        PngOptimize::Lossy => encode_png_lossy(img, quality),
    }
}

/// Lossy PNG: quantize to an 8-bit palette via libimagequant (preserving alpha
/// through a tRNS chunk), encode an indexed PNG via the `png` crate, then run a
/// final lossless oxipng pass.
fn encode_png_lossy(img: &DynamicImage, quality: u8) -> CoreResult<Vec<u8>> {
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);

    // Build the RGBA pixel buffer libimagequant expects (rgb::Rgba<u8>).
    let pixels: Vec<imagequant::RGBA> = rgba
        .pixels()
        .map(|p| imagequant::RGBA::new(p[0], p[1], p[2], p[3]))
        .collect();

    let mut liq = imagequant::new();
    // Best quantization quality (slowest) — closest to pngquant output.
    liq.set_speed(1).map_err(|e| CoreError::Encode(e.to_string()))?;
    // Map our 0-100 quality to a (min, max) target window. Higher quality raises
    // the floor so the quantizer is allowed fewer color compromises.
    let qmax = quality.min(100);
    let qmin = qmax.saturating_sub(20);
    liq.set_quality(qmin, qmax)
        .map_err(|e| CoreError::Encode(e.to_string()))?;

    let mut qimg = liq
        .new_image(pixels, w, h, 0.0)
        .map_err(|e| CoreError::Encode(e.to_string()))?;
    let mut res = liq
        .quantize(&mut qimg)
        .map_err(|e| CoreError::Encode(e.to_string()))?;
    res.set_dithering_level(1.0).ok();
    let (palette, indices) = res
        .remapped(&mut qimg)
        .map_err(|e| CoreError::Encode(e.to_string()))?;

    // Encode an indexed PNG with palette + transparency.
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, w as u32, h as u32);
        enc.set_color(png::ColorType::Indexed);
        enc.set_depth(png::BitDepth::Eight);
        let plte: Vec<u8> = palette.iter().flat_map(|c| [c.r, c.g, c.b]).collect();
        enc.set_palette(plte);
        let trns: Vec<u8> = palette.iter().map(|c| c.a).collect();
        enc.set_trns(trns);
        let mut writer = enc
            .write_header()
            .map_err(|e| CoreError::Encode(e.to_string()))?;
        writer
            .write_image_data(&indices)
            .map_err(|e| CoreError::Encode(e.to_string()))?;
    }

    // Final lossless squeeze.
    let opts = oxipng::Options::from_preset(2);
    oxipng::optimize_from_memory(&buf, &opts).map_err(|e| CoreError::Encode(e.to_string()))
}

/// Full single-file pipeline: decode -> process -> encode -> write. Returns the
/// path written.
pub fn process_file(input: &Path, job: &Job, preset_name: &str) -> CoreResult<PathBuf> {
    let img = image::open(input).map_err(|e| CoreError::Decode {
        path: input.to_path_buf(),
        source: e,
    })?;
    let (out_img, w, h) = process_image(&img, job);
    let bytes = encode(&out_img, job.format, job.quality, job.png)?;
    let target = ensure_unique(render_output_path(
        &job.output, input, job.format, w, h, preset_name,
    ));
    std::fs::write(&target, bytes).map_err(|e| CoreError::Io {
        path: target.clone(),
        source: e,
    })?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resize::ResizeMode;
    use image::{Rgba, RgbaImage};

    fn sample(w: u32, h: u32) -> DynamicImage {
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(w, h, Rgba([10, 20, 30, 255])))
    }

    #[test]
    fn process_image_crops_then_resizes() {
        let job = Job {
            resize: ResizeMode::Percent { factor: 0.5 },
            crop: CropMode::FixedSize { width: 100, height: 100, anchor: Default::default() },
            ..Job::default()
        };
        // crop 100x100 first, then scale by 0.5 -> 50x50
        let (_img, w, h) = process_image(&sample(800, 600), &job);
        assert_eq!((w, h), (50, 50));
    }

    #[test]
    fn rect_crop_then_resize_to_resolution() {
        // Source 800x600, crop the top-left 400x300, then resize to 200x150.
        let job = Job {
            crop: CropMode::Rect { x: 0, y: 0, width: 400, height: 300 },
            resize: ResizeMode::Pixels { width: Some(200), height: Some(150), keep_aspect: false },
            ..Job::default()
        };
        let (_img, w, h) = process_image(&sample(800, 600), &job);
        assert_eq!((w, h), (200, 150));
    }

    /// A non-trivial RGBA image: a smooth color gradient with a fully
    /// transparent quadrant, so quantization has real work to do and alpha is
    /// exercised.
    fn gradient_with_alpha(w: u32, h: u32) -> DynamicImage {
        let mut img = RgbaImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let r = (x * 255 / w.max(1)) as u8;
                let g = (y * 255 / h.max(1)) as u8;
                let b = ((x + y) * 255 / (w + h).max(1)) as u8;
                // Top-left quadrant fully transparent.
                let a = if x < w / 2 && y < h / 2 { 0 } else { 255 };
                img.put_pixel(x, y, Rgba([r, g, b, a]));
            }
        }
        DynamicImage::ImageRgba8(img)
    }

    #[test]
    fn encode_jpeg_roundtrips() {
        let bytes = encode(&sample(16, 16), OutputFormat::Jpeg, 80, PngOptimize::None).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (16, 16));
    }

    #[test]
    fn encode_webp_roundtrips() {
        let bytes = encode(&sample(16, 16), OutputFormat::Webp, 80, PngOptimize::None).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (16, 16));
    }

    #[test]
    fn encode_png_none_roundtrips() {
        let bytes = encode(&sample(16, 16), OutputFormat::Png, 90, PngOptimize::None).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (16, 16));
    }

    #[test]
    fn encode_png_lossless_is_valid_png() {
        let src = gradient_with_alpha(64, 64);
        let bytes = encode(&src, OutputFormat::Png, 90, PngOptimize::Lossless).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (64, 64));
        // Alpha preserved: still has the fully-transparent quadrant.
        let rgba = decoded.to_rgba8();
        assert!(rgba.pixels().any(|p| p[3] == 0));
    }

    #[test]
    fn encode_png_lossy_preserves_alpha_and_shrinks() {
        let src = gradient_with_alpha(256, 256);
        let lossy = encode(&src, OutputFormat::Png, 80, PngOptimize::Lossy).unwrap();
        let none = encode(&src, OutputFormat::Png, 80, PngOptimize::None).unwrap();

        let decoded = image::load_from_memory(&lossy).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (256, 256));
        // Transparency preserved through quantization + tRNS.
        let rgba = decoded.to_rgba8();
        assert!(
            rgba.pixels().any(|p| p[3] == 0),
            "lossy output lost transparency"
        );
        // Quantized + oxipng should not be larger than the plain PNG.
        assert!(
            lossy.len() <= none.len(),
            "lossy {} bytes vs none {} bytes",
            lossy.len(),
            none.len()
        );
    }

    #[test]
    fn process_file_writes_output() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.png");
        sample(32, 24).save(&input).unwrap();
        let job = Job { format: OutputFormat::Webp, ..Job::default() };
        let out = process_file(&input, &job, "test").unwrap();
        assert!(out.exists());
        assert_eq!(out.extension().unwrap(), "webp");
    }
}
