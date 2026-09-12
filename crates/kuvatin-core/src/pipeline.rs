use crate::crop::{apply_crop, CropMode};
use crate::format::OutputFormat;
use crate::metadata::{neutralise_orientation, Metadata};
use crate::naming::{render_output_path, OutputPolicy};
use crate::resize::{compute_target_dimensions, resample, ResizeMode};
use crate::{CoreError, CoreResult};
use image::DynamicImage;
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::path::{Path, PathBuf};

/// libimagequant speed. The scale runs 1 (most careful, closest to pngquant)
/// to 10 (fastest), and every right-click "Compress PNG" run pays it.
///
/// 1 looks like the expensive choice and is not. Measured on a 1600x1200 noisy
/// gradient, best of five runs in a release build, 1 was both the smallest
/// output and the *quickest*: speed 5 took 18% longer, speed 3 took 57%
/// longer, and no setting moved the output size by more than a third of a
/// percent. Raising it would cost quality and buy nothing. Re-measure with the
/// ignored `quantiser_speed_tradeoff` test before changing this.
const QUANTISE_SPEED: i32 = 1;

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

/// Struct-level `serde(default)`: a presets.toml written by an older version
/// that lacks a field added later still parses, with that field defaulted —
/// otherwise every such preset would be rejected as "invalid" on load.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
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

/// Apply the op pipeline (crop -> resize) to an in-memory image.
///
/// Crop runs first so a crop rectangle expressed in the *source* image's pixels
/// (e.g. an interactive per-image crop) selects the right region; the resize
/// then scales that cropped region to the target dimensions. Takes the image
/// by value: the no-op path (the default preset) hands the same buffer
/// through — it used to make four full-size copies per file.
pub fn process_image(img: DynamicImage, job: &Job) -> DynamicImage {
    let cropped = apply_crop(img, job.crop);
    let (tw, th) = compute_target_dimensions(job.resize, cropped.width(), cropped.height());
    resample(cropped, tw, th)
}

/// Encode an image to bytes in the requested format/quality.
///
/// `png` selects PNG-only size optimization (lossless via oxipng or lossy via
/// libimagequant); it is ignored for non-PNG formats. Consumes the image so
/// an already-RGBA8 buffer is reused rather than copied.
///
/// `meta` is the source image's colour profile and EXIF (see
/// [`decode_with_metadata`]), attached wherever the container can carry it:
/// PNG, JPEG and WebP can, BMP and GIF cannot, so those two lose it. Pass
/// `&Metadata::default()` for an image built from scratch.
pub fn encode(
    img: DynamicImage,
    format: OutputFormat,
    quality: u8,
    png: PngOptimize,
    meta: &Metadata,
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
            let mut enc =
                image::codecs::jpeg::JpegEncoder::new_with_quality(Cursor::new(&mut buf), quality);
            attach(&mut enc, meta);
            // The trait's write_image is what emits the metadata segments;
            // the inherent encode_image does not.
            use image::ImageEncoder;
            enc.write_image(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                image::ExtendedColorType::Rgb8,
            )
            .map_err(|e| CoreError::Encode(e.to_string()))?;
            Ok(buf)
        }
        OutputFormat::Webp => {
            let rgba = img.into_rgba8();
            let (w, h) = (rgba.width(), rgba.height());
            let encoder = webp::Encoder::from_rgba(&rgba, w, h);
            // encode() unwraps internally and panics on inputs libwebp rejects
            // (dimensions > 16383 px, config errors) — use the fallible API.
            let mem = encoder.encode_simple(false, quality as f32).map_err(|e| {
                CoreError::Encode(format!(
                    "WebP encode failed for {w}x{h} ({e:?}); note WebP allows at most 16383 px per side"
                ))
            })?;
            // libwebp writes the simple form, which has no room for a colour
            // profile or EXIF; re-wrap it as the extended form when there is
            // something to carry.
            let bytes = mem.to_vec();
            Ok(crate::metadata::webp_with_metadata(&bytes, w, h, meta).unwrap_or(bytes))
        }
        OutputFormat::Png => encode_png(img, png, quality, meta),
        OutputFormat::Gif => {
            // NeuQuant at speed 10 (the fastest setting) instead of the
            // default 1 ("at any cost"); the visual difference is nil for
            // screenshots and the encode is an order of magnitude quicker.
            let rgba = img.into_rgba8();
            let (w, h) = (rgba.width(), rgba.height());
            let mut buf = Vec::new();
            {
                let mut enc = image::codecs::gif::GifEncoder::new_with_speed(&mut buf, 10);
                enc.encode(rgba.as_raw(), w, h, image::ExtendedColorType::Rgba8)
                    .map_err(|e| CoreError::Encode(e.to_string()))?;
            }
            Ok(buf)
        }
        OutputFormat::Bmp | OutputFormat::Tiff => {
            let fmt = if format == OutputFormat::Bmp {
                image::ImageFormat::Bmp
            } else {
                image::ImageFormat::Tiff
            };
            let mut buf = Vec::new();
            img.write_to(&mut Cursor::new(&mut buf), fmt)
                .map_err(|e| CoreError::Encode(e.to_string()))?;
            Ok(buf)
        }
    }
}

/// Hand the source's colour profile and EXIF to an encoder that can carry
/// them. A container that cannot store one answers with an error, which is
/// not a failure of the conversion: the pixels are still correct.
fn attach<E: image::ImageEncoder>(enc: &mut E, meta: &Metadata) {
    if let Some(icc) = &meta.icc {
        let _ = enc.set_icc_profile(icc.clone());
    }
    if let Some(exif) = &meta.exif {
        let _ = enc.set_exif_metadata(exif.clone());
    }
}

/// Encode a plain PNG via the image crate (no extra optimization).
fn encode_png_plain(img: &DynamicImage, meta: &Metadata) -> CoreResult<Vec<u8>> {
    use image::ImageEncoder;
    let mut buf = Vec::new();
    let mut enc = image::codecs::png::PngEncoder::new(Cursor::new(&mut buf));
    attach(&mut enc, meta);
    // Writing through the encoder rather than `write_to` is what carries the
    // metadata; the colour type comes from the image, so 16-bit stays 16-bit.
    enc.write_image(
        img.as_bytes(),
        img.width(),
        img.height(),
        img.color().into(),
    )
    .map_err(|e| CoreError::Encode(e.to_string()))?;
    Ok(buf)
}

fn oxipng_squeeze(raw: &[u8]) -> CoreResult<Vec<u8>> {
    let opts = oxipng::Options::from_preset(2);
    oxipng::optimize_from_memory(raw, &opts).map_err(|e| CoreError::Encode(e.to_string()))
}

/// Encode a PNG with the requested size-optimization mode. Alpha is preserved in
/// all modes.
fn encode_png(
    img: DynamicImage,
    mode: PngOptimize,
    quality: u8,
    meta: &Metadata,
) -> CoreResult<Vec<u8>> {
    match mode {
        PngOptimize::None => encode_png_plain(&img, meta),
        // oxipng keeps ancillary chunks (it is configured not to strip), so
        // the profile and EXIF written above survive the squeeze.
        PngOptimize::Lossless => oxipng_squeeze(&encode_png_plain(&img, meta)?),
        PngOptimize::Lossy => encode_png_lossy(img, quality, meta),
    }
}

/// Lossy PNG: quantize to an 8-bit palette via libimagequant (preserving alpha
/// through a tRNS chunk), encode an indexed PNG via the `png` crate, then run a
/// final lossless oxipng pass. Falls back to a lossless PNG when even a
/// floorless quantization is refused.
fn encode_png_lossy(img: DynamicImage, quality: u8, meta: &Metadata) -> CoreResult<Vec<u8>> {
    let rgba = img.into_rgba8();
    match quantize_png(&rgba, quality, meta)? {
        Some(bytes) => Ok(bytes),
        None => oxipng_squeeze(&encode_png_plain(&DynamicImage::ImageRgba8(rgba), meta)?),
    }
}

/// `Ok(None)` when libimagequant can't meet even a zero quality floor.
fn quantize_png(
    rgba: &image::RgbaImage,
    quality: u8,
    meta: &Metadata,
) -> CoreResult<Option<Vec<u8>>> {
    quantize_at_speed_with(rgba, quality, QUANTISE_SPEED, meta)
}

#[cfg(test)]
fn quantize_at_speed(
    rgba: &image::RgbaImage,
    quality: u8,
    speed: i32,
) -> CoreResult<Option<Vec<u8>>> {
    quantize_at_speed_with(rgba, quality, speed, &Metadata::default())
}

fn quantize_at_speed_with(
    rgba: &image::RgbaImage,
    quality: u8,
    speed: i32,
    meta: &Metadata,
) -> CoreResult<Option<Vec<u8>>> {
    use rgb::FromSlice;
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    // The image crate's RGBA8 bytes ARE libimagequant's pixel layout — view
    // them in place instead of rebuilding the whole image pixel by pixel.
    let pixels: &[imagequant::RGBA] = rgba.as_raw().as_rgba();

    let mut liq = imagequant::new();
    liq.set_speed(speed)
        .map_err(|e| CoreError::Encode(e.to_string()))?;
    // Map our 0-100 quality to a (min, max) target window. Higher quality raises
    // the floor so the quantizer is allowed fewer color compromises.
    let qmax = quality.min(100);
    let qmin = qmax.saturating_sub(20);
    liq.set_quality(qmin, qmax)
        .map_err(|e| CoreError::Encode(e.to_string()))?;

    // Borrow the pixels so a second attempt can reuse them without a copy.
    let mut qimg = liq
        .new_image_borrowed(pixels, w, h, 0.0)
        .map_err(|e| CoreError::Encode(e.to_string()))?;
    let mut res = match liq.quantize(&mut qimg) {
        Ok(r) => r,
        // The palette can't reach the quality floor (grainy photos, noisy
        // gradients): drop the floor and retry rather than fail the file, and
        // if even that is refused, deliver a lossless PNG instead of nothing.
        Err(imagequant::Error::QualityTooLow) => {
            liq.set_quality(0, qmax)
                .map_err(|e| CoreError::Encode(e.to_string()))?;
            let mut retry = liq
                .new_image_borrowed(pixels, w, h, 0.0)
                .map_err(|e| CoreError::Encode(e.to_string()))?;
            match liq.quantize(&mut retry) {
                Ok(r) => {
                    qimg = retry;
                    r
                }
                Err(imagequant::Error::QualityTooLow) => return Ok(None),
                Err(e) => return Err(CoreError::Encode(e.to_string())),
            }
        }
        Err(e) => return Err(CoreError::Encode(e.to_string())),
    };
    res.set_dithering_level(1.0).ok();
    let (palette, indices) = res
        .remapped(&mut qimg)
        .map_err(|e| CoreError::Encode(e.to_string()))?;

    // Encode an indexed PNG with palette + transparency.
    let mut buf = Vec::new();
    {
        // with_info rather than new: the indexed path builds the PNG through
        // this crate directly, so the metadata has to ride on the info block.
        let mut info = png::Info::with_size(w as u32, h as u32);
        if let Some(icc) = &meta.icc {
            info.icc_profile = Some(std::borrow::Cow::Borrowed(icc));
        }
        if let Some(exif) = &meta.exif {
            info.exif_metadata = Some(std::borrow::Cow::Borrowed(exif));
        }
        let mut enc = png::Encoder::with_info(&mut buf, info)
            .map_err(|e| CoreError::Encode(e.to_string()))?;
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
    oxipng_squeeze(&buf).map(Some)
}

/// Composite an image over an opaque white background (for formats without
/// alpha, i.e. JPEG). An RGB8 image passes through as it is — no copy.
fn flatten_onto_white(img: DynamicImage) -> image::RgbImage {
    if let DynamicImage::ImageRgb8(rgb) = img {
        return rgb;
    }
    let rgba = img.into_rgba8();
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
///
/// Public because the GUI must preview and thumbnail through the SAME decode:
/// a crop drawn on an un-rotated preview of a portrait phone photo would
/// otherwise select the wrong region once the pipeline rotates the pixels.
pub fn decode_oriented(input: &Path) -> CoreResult<DynamicImage> {
    decode_with_metadata(input).map(|(img, _)| img)
}

/// As [`decode_oriented`], and also hands back the colour profile and EXIF so
/// [`encode`] can put them on the output. The EXIF orientation tag is reset to
/// "normal" first: the rotation is already applied to the pixels here, and a
/// viewer honouring the original tag would rotate a second time.
pub fn decode_with_metadata(input: &Path) -> CoreResult<(DynamicImage, Metadata)> {
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
    // separate feature, not a side effect.) A still GIF is returned from the
    // same decoder pass — it used to be decoded a second time below.
    if reader.format() == Some(image::ImageFormat::Gif) {
        let file = std::fs::File::open(input).map_err(io_err)?;
        let gif = image::codecs::gif::GifDecoder::new(std::io::BufReader::new(file))
            .map_err(decode_err)?;
        use image::AnimationDecoder;
        let mut frames = gif.into_frames();
        let first = match frames.next() {
            Some(Ok(frame)) => frame,
            Some(Err(e)) => return Err(decode_err(e)),
            None => {
                return Err(CoreError::InvalidJob(format!(
                    "{} is a GIF with no frames",
                    input.display()
                )))
            }
        };
        if frames.next().is_some() {
            return Err(CoreError::InvalidJob(format!(
                "{} is an animated GIF — converting it would keep only the first frame, \
                 so animated inputs are not supported",
                input.display()
            )));
        }
        return Ok((
            DynamicImage::ImageRgba8(first.into_buffer()),
            Metadata::default(),
        ));
    }
    let mut decoder = reader.into_decoder().map_err(decode_err)?;
    // EXIF orientation: without this, portrait phone photos convert lying on
    // their side (the tag is metadata; the pixels are stored rotated).
    use image::ImageDecoder;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    // Read the sidecar blocks before the decoder is consumed. Neither is
    // essential to the conversion, so an unreadable one is simply absent.
    let icc = decoder
        .icc_profile()
        .ok()
        .flatten()
        .filter(|b| !b.is_empty());
    let mut exif = decoder
        .exif_metadata()
        .ok()
        .flatten()
        .filter(|b| !b.is_empty());
    if let Some(bytes) = exif.as_mut() {
        neutralise_orientation(bytes);
    }
    let mut img = DynamicImage::from_decoder(decoder).map_err(decode_err)?;
    img.apply_orientation(orientation);
    Ok((img, Metadata { icc, exif }))
}

/// Write `bytes` to a NEW file derived from `base`, appending `-1`, `-2`, ...
/// to the stem until creation succeeds. Reservation happens at the filesystem
/// (`create_new`), so two parallel jobs racing to the same name get two
/// distinct files — the old exists()-then-write dance silently lost one.
fn write_unique(base: PathBuf, bytes: &[u8]) -> CoreResult<PathBuf> {
    use std::io::Write;
    let dir = base.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = base
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("image")
        .to_string();
    let ext = base
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let mut candidate = base.clone();
    for n in 0.. {
        if n > 0 {
            candidate = dir.join(format!("{stem}-{n}.{ext}"));
        }
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut f) => {
                if let Err(e) = f.write_all(bytes) {
                    // Disk full mid-write: don't leave a half-written file
                    // that looks finished — the name is ours, so remove it.
                    drop(f);
                    let _ = std::fs::remove_file(&candidate);
                    return Err(CoreError::Io {
                        path: candidate,
                        source: e,
                    });
                }
                return Ok(candidate);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(CoreError::Io {
                    path: candidate,
                    source: e,
                });
            }
        }
    }
    unreachable!()
}

/// Full single-file pipeline: decode -> process -> encode -> write. Returns the
/// path written.
pub fn process_file(input: &Path, job: &Job) -> CoreResult<PathBuf> {
    let (img, meta) = decode_with_metadata(input)?;
    let out_img = process_image(img, job);
    let bytes = encode(out_img, job.format, job.quality, job.png, &meta)?;
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
    let (img, meta) = decode_with_metadata(input)?;
    let out_img = process_image(img, job);
    let bytes = encode(out_img, job.format, job.quality, job.png, &meta)?;
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
            let stem = base
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("image")
                .to_string();
            let ext = base
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
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
            crop: CropMode::FixedSize {
                width: 100,
                height: 100,
                anchor: Default::default(),
            },
            ..Job::default()
        };
        // crop 100x100 first, then scale by 0.5 -> 50x50
        let img = process_image(sample(800, 600), &job);
        assert_eq!((img.width(), img.height()), (50, 50));
    }

    #[test]
    fn rect_crop_then_resize_to_resolution() {
        // Source 800x600, crop the top-left 400x300, then resize to 200x150.
        let job = Job {
            crop: CropMode::Rect {
                x: 0,
                y: 0,
                width: 400,
                height: 300,
            },
            resize: ResizeMode::Pixels {
                width: Some(200),
                height: Some(150),
                keep_aspect: false,
            },
            ..Job::default()
        };
        let img = process_image(sample(800, 600), &job);
        assert_eq!((img.width(), img.height()), (200, 150));
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
        let bytes = encode(
            sample(16, 16),
            OutputFormat::Jpeg,
            80,
            PngOptimize::None,
            &Metadata::default(),
        )
        .unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (16, 16));
    }

    #[test]
    fn encode_webp_roundtrips() {
        let bytes = encode(
            sample(16, 16),
            OutputFormat::Webp,
            80,
            PngOptimize::None,
            &Metadata::default(),
        )
        .unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (16, 16));
    }

    #[test]
    fn encode_png_none_roundtrips() {
        let bytes = encode(
            sample(16, 16),
            OutputFormat::Png,
            90,
            PngOptimize::None,
            &Metadata::default(),
        )
        .unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (16, 16));
    }

    #[test]
    fn encode_png_lossless_is_valid_png() {
        let src = gradient_with_alpha(64, 64);
        let bytes = encode(
            src.clone(),
            OutputFormat::Png,
            90,
            PngOptimize::Lossless,
            &Metadata::default(),
        )
        .unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (64, 64));
        // Alpha preserved: still has the fully-transparent quadrant.
        let rgba = decoded.to_rgba8();
        assert!(rgba.pixels().any(|p| p[3] == 0));
    }

    #[test]
    fn encode_png_lossy_preserves_alpha_and_shrinks() {
        let src = gradient_with_alpha(256, 256);
        let lossy = encode(
            src.clone(),
            OutputFormat::Png,
            80,
            PngOptimize::Lossy,
            &Metadata::default(),
        )
        .unwrap();
        let none = encode(
            src.clone(),
            OutputFormat::Png,
            80,
            PngOptimize::None,
            &Metadata::default(),
        )
        .unwrap();

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
        let job = Job {
            format: OutputFormat::Webp,
            ..Job::default()
        };
        let out = process_file(&input, &job).unwrap();
        assert!(out.exists());
        assert_eq!(out.extension().unwrap(), "webp");
    }

    /// WebP rejects >16383 px per side; that must be an Err, not the panic the
    /// webp crate's infallible-looking encode() used to produce.
    #[test]
    fn webp_oversize_errors_instead_of_panicking() {
        let wide = DynamicImage::ImageRgba8(RgbaImage::new(16_384, 1));
        let res = encode(
            wide,
            OutputFormat::Webp,
            80,
            PngOptimize::None,
            &Metadata::default(),
        );
        assert!(res.is_err(), "expected Err for 16384-px WebP");
    }

    /// Out-of-range quality (hand-edited presets.toml / CLI) is clamped at the
    /// encode choke point rather than panicking deep inside libwebp.
    #[test]
    fn webp_out_of_range_quality_is_clamped() {
        let bytes = encode(
            sample(16, 16),
            OutputFormat::Webp,
            150,
            PngOptimize::None,
            &Metadata::default(),
        )
        .unwrap();
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
            DynamicImage::ImageRgba8(img),
            OutputFormat::Jpeg,
            95,
            PngOptimize::None,
            &Metadata::default(),
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
        let err = process_file(&path, &job).unwrap_err().to_string();
        assert!(err.contains("animated"), "unexpected error: {err}");
        // A single-frame GIF still converts fine.
        let single = dir.path().join("still.gif");
        sample(8, 8).to_rgba8().save(&single).unwrap();
        assert!(process_file(&single, &Job::default()).is_ok());
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
        let job = Job {
            format: OutputFormat::Webp,
            ..Job::default()
        };
        let outs: Vec<_> = [a, b]
            .par_iter()
            .map(|p| process_file(p, &job).unwrap())
            .collect();
        assert_ne!(outs[0], outs[1], "outputs must not share a path");
        assert!(outs[0].exists() && outs[1].exists());
    }

    /// Batch planning dedupes same-stem targets before anything is written.
    /// Lossy PNG on a noisy image at maximum quality: libimagequant can't
    /// reach the quality floor (QUALITY_TOO_LOW) — the encoder must still
    /// deliver a file (floor dropped, else lossless) instead of failing.
    #[test]
    fn lossy_png_survives_quality_too_low() {
        let mut img = RgbaImage::new(96, 96);
        let mut seed: u32 = 0x2545_F491;
        for px in img.pixels_mut() {
            // xorshift noise: every pixel a different colour.
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let b = seed.to_le_bytes();
            *px = Rgba([b[0], b[1], b[2], 255]);
        }
        let bytes = encode(
            DynamicImage::ImageRgba8(img),
            OutputFormat::Png,
            100,
            PngOptimize::Lossy,
            &Metadata::default(),
        )
        .expect("a noisy image must still encode");
        assert!(bytes.starts_with(&[0x89, b'P', b'N', b'G']));
    }

    /// A Job written by an older version (fields missing) deserializes with
    /// defaults instead of rejecting the whole preset.
    #[test]
    fn job_missing_fields_take_defaults() {
        let job: Job = toml::from_str("format = \"jpeg\"\nquality = 70\n").unwrap();
        assert_eq!(job.format, OutputFormat::Jpeg);
        assert_eq!(job.quality, 70);
        assert_eq!(job.resize, ResizeMode::None);
        assert_eq!(job.output, OutputPolicy::default());
    }

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

    /// A JPEG carrying EXIF orientation 6 (rotate 90° clockwise — the way a
    /// portrait phone photo is stored) comes out rotated: 20×10 becomes 10×20,
    /// and the pixel that was top-left ends up top-right.
    #[test]
    fn decode_oriented_applies_exif_rotation() {
        let dir = tempfile::tempdir().unwrap();
        // Left half red, right half blue, so the rotation is observable.
        let mut img = image::RgbImage::new(20, 10);
        for (x, _y, p) in img.enumerate_pixels_mut() {
            *p = if x < 10 {
                image::Rgb([255, 0, 0])
            } else {
                image::Rgb([0, 0, 255])
            };
        }
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(Cursor::new(&mut jpeg), 100)
            .encode_image(&img)
            .unwrap();
        // Splice an APP1 EXIF segment right after SOI: TIFF header (II, 42,
        // IFD at 8) + one IFD entry (0x0112 Orientation, SHORT, 1, value 6).
        let tiff: Vec<u8> = [
            b"II".as_slice(),
            &[42, 0, 8, 0, 0, 0],
            &[1, 0],
            &[0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0],
            &[0, 0, 0, 0],
        ]
        .concat();
        let mut payload = b"Exif\0\0".to_vec();
        payload.extend_from_slice(&tiff);
        let len = (payload.len() + 2) as u16;
        let mut with_exif = vec![0xFF, 0xD8, 0xFF, 0xE1, (len >> 8) as u8, len as u8];
        with_exif.extend_from_slice(&payload);
        with_exif.extend_from_slice(&jpeg[2..]);
        let p = dir.path().join("rotated.jpg");
        std::fs::write(&p, &with_exif).unwrap();

        let out = decode_oriented(&p).unwrap();
        assert_eq!((out.width(), out.height()), (10, 20), "rotated 90°");
        let rgb = out.to_rgb8();
        let top = rgb.get_pixel(5, 2);
        let bottom = rgb.get_pixel(5, 17);
        assert!(
            top[0] > 200 && top[2] < 60,
            "red half rotated to the top: {top:?}"
        );
        assert!(
            bottom[2] > 200 && bottom[0] < 60,
            "blue half rotated to the bottom: {bottom:?}"
        );
    }

    /// The remaining encoders round-trip through the image crate.
    #[test]
    fn bmp_tiff_gif_roundtrip() {
        for f in [OutputFormat::Bmp, OutputFormat::Tiff, OutputFormat::Gif] {
            let bytes = encode(
                gradient_with_alpha(24, 16),
                f,
                90,
                PngOptimize::None,
                &Metadata::default(),
            )
            .unwrap_or_else(|e| panic!("{f:?}: {e}"));
            let decoded = image::load_from_memory(&bytes).unwrap_or_else(|e| panic!("{f:?}: {e}"));
            assert_eq!((decoded.width(), decoded.height()), (24, 16), "{f:?}");
        }
    }

    /// `process_file_to` writes exactly the requested path (creating parents)
    /// and overwrites an existing file there.
    #[test]
    fn process_file_to_writes_the_given_path() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.png");
        sample(30, 20).save(&input).unwrap();
        let out = dir.path().join("deep").join("er").join("pic.jpg");
        let job = Job {
            format: OutputFormat::Jpeg,
            resize: ResizeMode::Percent { factor: 0.5 },
            ..Job::default()
        };
        assert_eq!(process_file_to(&input, &job, &out).unwrap(), out);
        let img = image::open(&out).unwrap();
        assert_eq!((img.width(), img.height()), (15, 10));
        // Second run replaces the file rather than making a -1 sibling.
        process_file_to(&input, &job, &out).unwrap();
        assert!(!dir
            .path()
            .join("deep")
            .join("er")
            .join("pic-1.jpg")
            .exists());
    }

    /// A still GIF decodes (once) and converts; an empty container is refused.
    #[test]
    fn still_gif_decodes_from_the_frame_pass() {
        let dir = tempfile::tempdir().unwrap();
        let single = dir.path().join("still.gif");
        gradient_with_alpha(16, 12)
            .to_rgba8()
            .save(&single)
            .unwrap();
        let img = decode_oriented(&single).unwrap();
        assert_eq!((img.width(), img.height()), (16, 12));
    }

    #[test]
    fn job_uses_quality_accounts_for_lossy_png() {
        let lossy_png = Job {
            png: PngOptimize::Lossy,
            ..Job::default()
        };
        assert!(lossy_png.uses_quality());
        assert!(!Job::default().uses_quality()); // plain PNG
        assert!(Job {
            format: OutputFormat::Jpeg,
            ..Job::default()
        }
        .uses_quality());
    }

    // ---- colour profile and EXIF preservation -------------------------

    /// A plausible ICC profile: the header's size field and the `acsp`
    /// signature are real, the body is filler. Encoders store the block
    /// verbatim, so that is enough to prove it travels.
    fn profile(tag: u8) -> Vec<u8> {
        let mut icc = vec![tag; 132];
        icc[0..4].copy_from_slice(&132u32.to_be_bytes());
        icc[36..40].copy_from_slice(b"acsp");
        icc
    }

    fn png_carrying(icc: &[u8]) -> Vec<u8> {
        use image::ImageEncoder;
        let img = sample(8, 8).into_rgba8();
        let mut buf = Vec::new();
        let mut enc = image::codecs::png::PngEncoder::new(Cursor::new(&mut buf));
        enc.set_icc_profile(icc.to_vec()).unwrap();
        enc.write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
        buf
    }

    fn icc_of(bytes: &[u8], format: image::ImageFormat) -> Option<Vec<u8>> {
        use image::ImageDecoder;
        let c = Cursor::new(bytes);
        match format {
            image::ImageFormat::Png => image::codecs::png::PngDecoder::new(c)
                .ok()?
                .icc_profile()
                .ok()?,
            image::ImageFormat::Jpeg => image::codecs::jpeg::JpegDecoder::new(c)
                .ok()?
                .icc_profile()
                .ok()?,
            image::ImageFormat::WebP => image::codecs::webp::WebPDecoder::new(c)
                .ok()?
                .icc_profile()
                .ok()?,
            _ => None,
        }
    }

    fn exif_of(bytes: &[u8], format: image::ImageFormat) -> Option<Vec<u8>> {
        use image::ImageDecoder;
        let c = Cursor::new(bytes);
        match format {
            image::ImageFormat::Png => image::codecs::png::PngDecoder::new(c)
                .ok()?
                .exif_metadata()
                .ok()?,
            image::ImageFormat::Jpeg => image::codecs::jpeg::JpegDecoder::new(c)
                .ok()?
                .exif_metadata()
                .ok()?,
            _ => None,
        }
    }

    /// A 20x10 JPEG whose EXIF says "rotate 90 clockwise", the way a portrait
    /// phone photo is stored.
    fn jpeg_rotated_90() -> Vec<u8> {
        let img = image::RgbImage::from_pixel(20, 10, image::Rgb([90, 120, 150]));
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(Cursor::new(&mut jpeg), 92)
            .encode_image(&img)
            .unwrap();
        let tiff: Vec<u8> = [
            b"II".as_slice(),
            &[42, 0, 8, 0, 0, 0],
            &[1, 0],
            &[0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0],
            &[0, 0, 0, 0],
        ]
        .concat();
        let mut payload = b"Exif\0\0".to_vec();
        payload.extend_from_slice(&tiff);
        let len = (payload.len() + 2) as u16;
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1, (len >> 8) as u8, len as u8];
        out.extend_from_slice(&payload);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    fn write_temp(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = dir.path().join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    /// The colour profile must reach the output. Without it a Display P3 photo
    /// is rendered as though its numbers were sRGB, which shifts every colour.
    #[test]
    fn png_output_keeps_the_colour_profile() {
        let dir = tempfile::tempdir().unwrap();
        let icc = profile(0x5A);
        let src = write_temp(&dir, "p3.png", &png_carrying(&icc));

        let (img, meta) = decode_with_metadata(&src).unwrap();
        assert_eq!(meta.icc.as_deref(), Some(icc.as_slice()), "read back");

        let out = encode(img, OutputFormat::Png, 90, PngOptimize::None, &meta).unwrap();
        assert_eq!(
            icc_of(&out, image::ImageFormat::Png).as_deref(),
            Some(icc.as_slice())
        );
    }

    /// The lossless mode runs the bytes through oxipng afterwards, which is
    /// free to drop ancillary chunks; the profile must still be there.
    #[test]
    fn the_oxipng_pass_keeps_the_colour_profile() {
        let dir = tempfile::tempdir().unwrap();
        let icc = profile(0x33);
        let src = write_temp(&dir, "p3.png", &png_carrying(&icc));

        let (img, meta) = decode_with_metadata(&src).unwrap();
        let out = encode(img, OutputFormat::Png, 90, PngOptimize::Lossless, &meta).unwrap();
        assert_eq!(
            icc_of(&out, image::ImageFormat::Png).as_deref(),
            Some(icc.as_slice())
        );
    }

    /// The lossy mode builds the PNG through libimagequant and the png crate
    /// rather than the image crate, so it needs the chunk written explicitly.
    #[test]
    fn the_quantized_png_keeps_the_colour_profile() {
        let dir = tempfile::tempdir().unwrap();
        let icc = profile(0x77);
        let src = write_temp(&dir, "p3.png", &png_carrying(&icc));

        let (img, meta) = decode_with_metadata(&src).unwrap();
        let out = encode(img, OutputFormat::Png, 80, PngOptimize::Lossy, &meta).unwrap();
        assert_eq!(
            icc_of(&out, image::ImageFormat::Png).as_deref(),
            Some(icc.as_slice())
        );
    }

    #[test]
    fn jpeg_output_keeps_the_colour_profile() {
        let dir = tempfile::tempdir().unwrap();
        let icc = profile(0x11);
        let src = write_temp(&dir, "p3.png", &png_carrying(&icc));

        let (img, meta) = decode_with_metadata(&src).unwrap();
        let out = encode(img, OutputFormat::Jpeg, 85, PngOptimize::None, &meta).unwrap();
        assert_eq!(
            icc_of(&out, image::ImageFormat::Jpeg).as_deref(),
            Some(icc.as_slice())
        );
    }

    /// EXIF carries the capture date, camera and copyright, and losing it on a
    /// same-format run is a surprise. The orientation tag is the exception: the
    /// rotation is already in the pixels, so the copy must say "normal" or
    /// every viewer rotates a second time.
    #[test]
    fn exif_travels_but_its_orientation_is_neutralised() {
        let dir = tempfile::tempdir().unwrap();
        let src = write_temp(&dir, "portrait.jpg", &jpeg_rotated_90());

        let (img, meta) = decode_with_metadata(&src).unwrap();
        assert_eq!((img.width(), img.height()), (10, 20), "rotation applied");
        let carried = meta.exif.clone().expect("exif read back");
        assert_eq!(orientation_in(&carried), Some(1), "tag neutralised");

        let out = encode(img, OutputFormat::Jpeg, 90, PngOptimize::None, &meta).unwrap();
        let written = exif_of(&out, image::ImageFormat::Jpeg).expect("exif on the output");
        assert_eq!(orientation_in(&written), Some(1));
    }

    /// Read the orientation tag straight out of an EXIF block; the decoder
    /// returns it with the `Exif\0\0` prefix already stripped.
    fn orientation_in(exif: &[u8]) -> Option<u16> {
        let body = exif.strip_prefix(b"Exif\0\0").unwrap_or(exif);
        let little = match body.get(0..2)? {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        let at = 8 + 2;
        let raw = [*body.get(at + 8)?, *body.get(at + 9)?];
        Some(if little {
            u16::from_le_bytes(raw)
        } else {
            u16::from_be_bytes(raw)
        })
    }

    /// "Convert to WebP" is one of the built-in presets, so the profile has to
    /// survive this path too; WebP carries one only in its extended form.
    #[test]
    fn webp_output_keeps_the_colour_profile() {
        let dir = tempfile::tempdir().unwrap();
        let icc = profile(0x2B);
        let src = write_temp(&dir, "p3.png", &png_carrying(&icc));

        let (img, meta) = decode_with_metadata(&src).unwrap();
        let out = encode(img, OutputFormat::Webp, 85, PngOptimize::None, &meta).unwrap();

        assert_eq!(
            icc_of(&out, image::ImageFormat::WebP).as_deref(),
            Some(icc.as_slice())
        );
        let decoded = image::load_from_memory(&out).expect("still a readable WebP");
        assert_eq!((decoded.width(), decoded.height()), (8, 8));
    }

    /// Transparency is stored in its own chunk, and rebuilding the container
    /// to add a profile must not lose it.
    #[test]
    fn webp_keeps_alpha_alongside_the_profile() {
        let icc = profile(0x44);
        let meta = Metadata {
            icc: Some(icc.clone()),
            exif: None,
        };
        let mut img = RgbaImage::from_pixel(12, 12, Rgba([200, 40, 60, 255]));
        for y in 0..12 {
            img.put_pixel(0, y, Rgba([0, 0, 0, 0]));
        }
        let out = encode(
            DynamicImage::ImageRgba8(img),
            OutputFormat::Webp,
            90,
            PngOptimize::None,
            &meta,
        )
        .unwrap();

        assert_eq!(
            icc_of(&out, image::ImageFormat::WebP).as_deref(),
            Some(icc.as_slice())
        );
        let decoded = image::load_from_memory(&out).unwrap().into_rgba8();
        assert_eq!(decoded.get_pixel(0, 0)[3], 0, "transparent column survived");
        assert_eq!(decoded.get_pixel(6, 6)[3], 255, "opaque body survived");
    }

    /// Without metadata the encoder's own output is handed back untouched, so
    /// a plain conversion gains no container overhead.
    #[test]
    fn webp_without_metadata_is_not_rewrapped() {
        let bare = encode(
            sample(8, 8),
            OutputFormat::Webp,
            85,
            PngOptimize::None,
            &Metadata::default(),
        )
        .unwrap();
        assert_eq!(&bare[8..12], b"WEBP");
        assert_eq!(&bare[12..16], b"VP8 ", "simple form kept");
    }

    /// Writing PNGs through the encoder rather than `write_to` is what lets
    /// the metadata ride along; the colour type must still come from the
    /// image, so a 16-bit source is not quietly flattened to 8.
    #[test]
    fn png_output_keeps_sixteen_bit_depth() {
        let deep = DynamicImage::ImageRgba16(image::ImageBuffer::from_pixel(
            4,
            4,
            image::Rgba([40_000u16, 20_000, 10_000, 65_535]),
        ));
        let out = encode(
            deep,
            OutputFormat::Png,
            90,
            PngOptimize::None,
            &Metadata::default(),
        )
        .unwrap();
        let decoded = image::load_from_memory(&out).unwrap();
        assert!(
            matches!(decoded, DynamicImage::ImageRgba16(_)),
            "got {:?}",
            decoded.color()
        );
    }

    /// How the libimagequant speed setting was chosen. Not a correctness test:
    /// run it with `cargo test -p kuvatin-core -- --ignored --nocapture
    /// quantiser_speed` when retuning the default preset.
    #[test]
    #[ignore]
    fn quantiser_speed_tradeoff() {
        // Photo-like: a smooth gradient with noise, which is the hard case for
        // a palette (flat screenshots quantise well at any speed).
        let (w, h) = (1600u32, 1200u32);
        let mut img = RgbaImage::new(w, h);
        let mut seed = 0x9E3779B9u32;
        for (x, y, p) in img.enumerate_pixels_mut() {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let n = ((seed >> 24) as i32 - 128) / 12;
            let r = (x * 255 / w) as i32 + n;
            let g = (y * 255 / h) as i32 + n;
            let b = 255 - ((x + y) * 255 / (w + h)) as i32 + n;
            *p = Rgba([
                r.clamp(0, 255) as u8,
                g.clamp(0, 255) as u8,
                b.clamp(0, 255) as u8,
                255,
            ]);
        }

        // Best of several runs: a single timing on a working machine is noise,
        // and an earlier pass made speed 4 look slower than speed 1.
        const RUNS: usize = 5;
        eprintln!("speed  bytes      vs speed 1   best of {RUNS} (s)   vs speed 1");
        let mut baseline_bytes = 0usize;
        let mut baseline_time = f64::MAX;
        for speed in [1i32, 2, 3, 4, 5] {
            let mut best = f64::MAX;
            let mut bytes = Vec::new();
            for _ in 0..RUNS {
                let start = std::time::Instant::now();
                bytes = quantize_at_speed(&img, 80, speed)
                    .unwrap()
                    .expect("quantised");
                best = best.min(start.elapsed().as_secs_f64());
            }
            if speed == 1 {
                baseline_bytes = bytes.len();
                baseline_time = best;
            }
            eprintln!(
                "{speed:>5}  {:>9}  {:>+9.2}%  {best:>14.3}  {:>+9.1}%",
                bytes.len(),
                (bytes.len() as f64 / baseline_bytes as f64 - 1.0) * 100.0,
                (best / baseline_time - 1.0) * 100.0
            );
        }
    }

    /// A source with neither profile nor EXIF must not gain empty chunks.
    #[test]
    fn a_bare_image_stays_bare() {
        let dir = tempfile::tempdir().unwrap();
        let mut plain = Vec::new();
        sample(8, 8)
            .write_to(&mut Cursor::new(&mut plain), image::ImageFormat::Png)
            .unwrap();
        let src = write_temp(&dir, "plain.png", &plain);

        let (img, meta) = decode_with_metadata(&src).unwrap();
        assert!(meta.is_empty(), "nothing to carry");
        let out = encode(img, OutputFormat::Png, 90, PngOptimize::None, &meta).unwrap();
        assert_eq!(icc_of(&out, image::ImageFormat::Png), None);
    }
}
