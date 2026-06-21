use crate::crop::{apply_crop, CropMode};
use crate::format::OutputFormat;
use crate::naming::{ensure_unique, render_output_path, OutputPolicy};
use crate::resize::{compute_target_dimensions, resample, ResizeMode};
use crate::{CoreError, CoreResult};
use image::DynamicImage;
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub resize: ResizeMode,
    pub crop: CropMode,
    pub format: OutputFormat,
    /// 0-100; ignored for lossless formats.
    pub quality: u8,
    pub output: OutputPolicy,
}

impl Default for Job {
    fn default() -> Self {
        Job {
            resize: ResizeMode::None,
            crop: CropMode::None,
            format: OutputFormat::Png,
            quality: 90,
            output: OutputPolicy::default(),
        }
    }
}

/// Apply the op pipeline (resize -> crop) to an in-memory image. Returns the
/// transformed image and its final (w, h).
pub fn process_image(img: &DynamicImage, job: &Job) -> (DynamicImage, u32, u32) {
    let (tw, th) = compute_target_dimensions(job.resize, img.width(), img.height());
    let resized = resample(img, tw, th);
    let cropped = apply_crop(&resized, job.crop);
    let (w, h) = (cropped.width(), cropped.height());
    (cropped, w, h)
}

/// Encode an image to bytes in the requested format/quality.
pub fn encode(img: &DynamicImage, format: OutputFormat, quality: u8) -> CoreResult<Vec<u8>> {
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
        other => {
            let fmt = match other {
                OutputFormat::Png => image::ImageFormat::Png,
                OutputFormat::Bmp => image::ImageFormat::Bmp,
                OutputFormat::Tiff => image::ImageFormat::Tiff,
                OutputFormat::Gif => image::ImageFormat::Gif,
                OutputFormat::Jpeg | OutputFormat::Webp => unreachable!(),
            };
            let mut buf = Vec::new();
            img.write_to(&mut Cursor::new(&mut buf), fmt)
                .map_err(|e| CoreError::Encode(e.to_string()))?;
            Ok(buf)
        }
    }
}

/// Full single-file pipeline: decode -> process -> encode -> write. Returns the
/// path written.
pub fn process_file(input: &Path, job: &Job, preset_name: &str) -> CoreResult<PathBuf> {
    let img = image::open(input).map_err(|e| CoreError::Decode {
        path: input.to_path_buf(),
        source: e,
    })?;
    let (out_img, w, h) = process_image(&img, job);
    let bytes = encode(&out_img, job.format, job.quality)?;
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
    fn process_image_resizes_then_crops() {
        let job = Job {
            resize: ResizeMode::Percent { factor: 0.5 },
            crop: CropMode::FixedSize { width: 100, height: 100, anchor: Default::default() },
            ..Job::default()
        };
        let (_img, w, h) = process_image(&sample(800, 600), &job);
        assert_eq!((w, h), (100, 100));
    }

    #[test]
    fn encode_jpeg_roundtrips() {
        let bytes = encode(&sample(16, 16), OutputFormat::Jpeg, 80).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (16, 16));
    }

    #[test]
    fn encode_webp_roundtrips() {
        let bytes = encode(&sample(16, 16), OutputFormat::Webp, 80).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (16, 16));
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
