use crate::crop::{apply_crop, CropMode};
use crate::format::OutputFormat;
use crate::naming::{render_output_path, OutputPolicy};
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

impl Job {
    /// True when this job actually consumes `quality` — unlike
    /// [`OutputFormat::uses_quality`], this accounts for lossy PNG
    /// (libimagequant), which the flagship "Compress PNG" preset uses.
    pub fn uses_quality(&self) -> bool {
        self.format.uses_quality()
            || (self.format == OutputFormat::Png && self.png == PngOptimize::Lossy)
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
    // Single choke point for quality: presets.toml and the CLI can carry any
    // u8, and out-of-range values panic deep inside libwebp. GUI-side clamps
    // are a convenience only; this is the guarantee.
    let quality = quality.min(100);
    match format {
        OutputFormat::Jpeg => {
            let mut buf = Vec::new();
            // JPEG has no alpha: composite transparent pixels over white (the
            // expectation for logos/screenshots) instead of letting them fall
            // to black through a raw channel drop.
            let rgb = flatten_onto_white(img);
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(
                Cursor::new(&mut buf),
                quality,
            );
            enc.encode_image(&rgb).map_err(|e| CoreError::Encode(e.to_string()))?;
            Ok(buf)
        }
        OutputFormat::Webp => {
            let rgba = img.to_rgba8();
            let (w, h) = (rgba.width(), rgba.height());
            let encoder = webp::Encoder::from_rgba(&rgba, w, h);
            // encode() unwraps internally and panics on inputs libwebp rejects
            // (dimensions > 16383 px, config errors) — use the fallible API.
            let mem = encoder.encode_simple(false, quality as f32).map_err(|e| {
                CoreError::Encode(format!(
                    "WebP encode failed for {w}x{h} ({e:?}); note WebP allows at most 16383 px per side"
                ))
            })?;
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

/// Composite an image over an opaque white background (for formats without
/// alpha, i.e. JPEG). Fully-opaque images pass through at no extra cost.
fn flatten_onto_white(img: &DynamicImage) -> image::RgbImage {
    let rgba = img.to_rgba8();
    let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
    for (src, dst) in rgba.pixels().zip(rgb.pixels_mut()) {
        let a = src[3] as u32;
        dst.0 = [
            ((src[0] as u32 * a + 255 * (255 - a)) / 255) as u8,
            ((src[1] as u32 * a + 255 * (255 - a)) / 255) as u8,
            ((src[2] as u32 * a + 255 * (255 - a)) / 255) as u8,
        ];
    }
    rgb
}

/// Decode `input` honoring EXIF orientation (phone photos!) and refusing
/// animated GIFs (which `image::open` would silently flatten to frame 1).
fn decode_oriented(input: &Path) -> CoreResult<DynamicImage> {
    let decode_err = |e: image::ImageError| CoreError::Decode {
        path: input.to_path_buf(),
        source: e,
    };
    let io_err = |e: std::io::Error| CoreError::Io {
        path: input.to_path_buf(),
        source: e,
    };
    let reader = image::ImageReader::open(input)
        .map_err(io_err)?
        .with_guessed_format()
        .map_err(io_err)?;
    // Animated GIF: converting would silently drop every frame after the
    // first — refuse with a clear message instead. (Animation support is a
    // separate feature, not a side effect.)
    if reader.format() == Some(image::ImageFormat::Gif) {
        let file = std::fs::File::open(input).map_err(io_err)?;
        let gif = image::codecs::gif::GifDecoder::new(std::io::BufReader::new(file))
            .map_err(decode_err)?;
        use image::AnimationDecoder;
        if gif.into_frames().take(2).count() > 1 {
            return Err(CoreError::InvalidJob(format!(
                "{} is an animated GIF — converting it would keep only the first frame, \
                 so animated inputs are not supported",
                input.display()
            )));
        }
    }
    let mut decoder = reader.into_decoder().map_err(decode_err)?;
    // EXIF orientation: without this, portrait phone photos convert lying on
    // their side (the tag is metadata; the pixels are stored rotated).
    use image::ImageDecoder;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(decode_err)?;
    img.apply_orientation(orientation);
    Ok(img)
}

/// Write `bytes` to a NEW file derived from `base`, appending `-1`, `-2`, ...
/// to the stem until creation succeeds. Reservation happens at the filesystem
/// (`create_new`), so two parallel jobs racing to the same name get two
/// distinct files — the old exists()-then-write dance silently lost one.
fn write_unique(base: PathBuf, bytes: &[u8]) -> CoreResult<PathBuf> {
    use std::io::Write;
    let dir = base.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = base.file_stem().and_then(|s| s.to_str()).unwrap_or("image").to_string();
    let ext = base.extension().and_then(|s| s.to_str()).unwrap_or("").to_string();
    let mut candidate = base.clone();
    for n in 0.. {
        if n > 0 {
            candidate = dir.join(format!("{stem}-{n}.{ext}"));
        }
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(mut f) => {
                f.write_all(bytes).map_err(|e| CoreError::Io {
                    path: candidate.clone(),
                    source: e,
                })?;
                return Ok(candidate);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(CoreError::Io { path: candidate, source: e });
            }
        }
    }
    unreachable!()
}

/// Full single-file pipeline: decode -> process -> encode -> write. Returns the
/// path written.
pub fn process_file(input: &Path, job: &Job, _preset_name: &str) -> CoreResult<PathBuf> {
    let img = decode_oriented(input)?;
    let (out_img, _w, _h) = process_image(&img, job);
    let bytes = encode(&out_img, job.format, job.quality, job.png)?;
    let target = render_output_path(&job.output, input, job.format);
    // The policy may point at a subfolder that doesn't exist yet.
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
    }
    // Race-proof uniqueness: the name is reserved at open time, so parallel
    // batch workers can never clobber each other's output.
    write_unique(target, &bytes)
}

/// Like [`process_file`], but writes to an explicit `output` path (creating any
/// missing parent directories) instead of deriving one from the input and the
/// job's [`OutputPolicy`]. Used by the GUI when the user picks a save location
/// or an output folder. Overwrites `output` if it already exists.
pub fn process_file_to(input: &Path, job: &Job, output: &Path) -> CoreResult<PathBuf> {
    let img = decode_oriented(input)?;
    let (out_img, _w, _h) = process_image(&img, job);
    let bytes = encode(&out_img, job.format, job.quality, job.png)?;
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
    }
    std::fs::write(output, &bytes).map_err(|e| CoreError::Io {
        path: output.to_path_buf(),
        source: e,
    })?;
    Ok(output.to_path_buf())
}

/// Plan collision-free output paths for a whole batch heading to explicit
/// targets (the GUI's output-folder mode): same-stem inputs from different
/// folders would otherwise all plan the same target and deterministically
/// overwrite each other. Dedupes against both the filesystem and the batch
/// itself; returns paths in input order.
pub fn plan_unique_outputs(targets: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut taken: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    targets
        .into_iter()
        .map(|base| {
            let dir = base.parent().map(Path::to_path_buf).unwrap_or_default();
            let stem =
                base.file_stem().and_then(|s| s.to_str()).unwrap_or("image").to_string();
            let ext = base.extension().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let mut candidate = base.clone();
            let mut n = 0usize;
            while taken.contains(&candidate) || candidate.exists() {
                n += 1;
                candidate = dir.join(format!("{stem}-{n}.{ext}"));
            }
            taken.insert(candidate.clone());
            candidate
        })
        .collect()
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

    /// WebP rejects >16383 px per side; that must be an Err, not the panic the
    /// webp crate's infallible-looking encode() used to produce.
    #[test]
    fn webp_oversize_errors_instead_of_panicking() {
        let wide = DynamicImage::ImageRgba8(RgbaImage::new(16_384, 1));
        let res = encode(&wide, OutputFormat::Webp, 80, PngOptimize::None);
        assert!(res.is_err(), "expected Err for 16384-px WebP");
    }

    /// Out-of-range quality (hand-edited presets.toml / CLI) is clamped at the
    /// encode choke point rather than panicking deep inside libwebp.
    #[test]
    fn webp_out_of_range_quality_is_clamped() {
        let bytes = encode(&sample(16, 16), OutputFormat::Webp, 150, PngOptimize::None).unwrap();
        assert!(image::load_from_memory(&bytes).is_ok());
    }

    /// JPEG output composites transparency over white, not black.
    #[test]
    fn jpeg_flattens_alpha_onto_white() {
        let mut img = RgbaImage::from_pixel(8, 8, Rgba([255, 0, 0, 255]));
        for y in 0..8 {
            img.put_pixel(0, y, Rgba([0, 0, 0, 0])); // transparent column
        }
        let bytes = encode(
            &DynamicImage::ImageRgba8(img),
            OutputFormat::Jpeg,
            95,
            PngOptimize::None,
        )
        .unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgb8();
        let p = decoded.get_pixel(0, 4);
        assert!(
            p[0] > 200 && p[1] > 200 && p[2] > 200,
            "transparent area should be near-white, got {p:?}"
        );
    }

    /// Animated GIFs are refused with a clear error instead of silently
    /// flattening to the first frame.
    #[test]
    fn animated_gif_is_refused() {
        use image::codecs::gif::GifEncoder;
        use image::{Delay, Frame};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("anim.gif");
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut enc = GifEncoder::new(file);
            for shade in [0u8, 255u8] {
                let frame = Frame::from_parts(
                    RgbaImage::from_pixel(8, 8, Rgba([shade, shade, shade, 255])),
                    0,
                    0,
                    Delay::from_numer_denom_ms(100, 1),
                );
                enc.encode_frame(frame).unwrap();
            }
        }
        let job = Job::default();
        let err = process_file(&path, &job, "t").unwrap_err().to_string();
        assert!(err.contains("animated"), "unexpected error: {err}");
        // A single-frame GIF still converts fine.
        let single = dir.path().join("still.gif");
        sample(8, 8).to_rgba8().save(&single).unwrap();
        assert!(process_file(&single, &Job::default(), "t").is_ok());
    }

    /// Two parallel writers racing to the same output stem must produce two
    /// distinct files (the exists()-then-write scheme silently lost one).
    #[test]
    fn parallel_same_stem_outputs_do_not_clobber() {
        use rayon::prelude::*;
        let dir = tempfile::tempdir().unwrap();
        // Two inputs whose stems collide after conversion: photo.png + photo.jpg -> photo-kuvatin.webp
        let a = dir.path().join("photo.png");
        let b = dir.path().join("photo.jpg");
        sample(8, 8).save(&a).unwrap();
        sample(8, 8).to_rgb8().save(&b).unwrap();
        let job = Job { format: OutputFormat::Webp, ..Job::default() };
        let outs: Vec<_> = [a, b]
            .par_iter()
            .map(|p| process_file(p, &job, "t").unwrap())
            .collect();
        assert_ne!(outs[0], outs[1], "outputs must not share a path");
        assert!(outs[0].exists() && outs[1].exists());
    }

    /// Batch planning dedupes same-stem targets before anything is written.
    #[test]
    fn plan_unique_outputs_dedupes_same_stem() {
        let dir = tempfile::tempdir().unwrap();
        let t = dir.path().join("photo.webp");
        let planned = plan_unique_outputs(vec![t.clone(), t.clone(), t]);
        assert_eq!(planned.len(), 3);
        assert_ne!(planned[0], planned[1]);
        assert_ne!(planned[1], planned[2]);
        assert_ne!(planned[0], planned[2]);
    }

    /// EXIF-oriented decode path: a plain image (no orientation metadata)
    /// passes through decode_oriented unchanged.
    #[test]
    fn decode_oriented_passthrough_without_exif() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("plain.jpg");
        sample(20, 10).to_rgb8().save(&p).unwrap();
        let img = decode_oriented(&p).unwrap();
        assert_eq!((img.width(), img.height()), (20, 10));
    }

    #[test]
    fn job_uses_quality_accounts_for_lossy_png() {
        let lossy_png = Job { png: PngOptimize::Lossy, ..Job::default() };
        assert!(lossy_png.uses_quality());
        assert!(!Job::default().uses_quality()); // plain PNG
        assert!(Job { format: OutputFormat::Jpeg, ..Job::default() }.uses_quality());
    }
}
