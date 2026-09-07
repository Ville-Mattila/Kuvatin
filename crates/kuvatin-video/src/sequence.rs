//! Image-sequence support: detect a numbered sequence (`frame_0001.png`,
//! `frame_0002.png`, …) from its first file, express it as a GStreamer
//! `imagesequence://` URI (one file per frame at a fixed fps) that GES treats
//! like any other clip, and pre-convert EXR sequences — which the bundled
//! GStreamer cannot decode — to sRGB PNG in a temp cache.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use gstreamer as gst;
use rayon::prelude::*;

/// A numbered image sequence: files named `<prefix><NUMBER><suffix>` in `dir`,
/// starting at `start`, `count` consecutive frames, played at `fps`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SequenceSpec {
    pub dir: PathBuf,
    /// File-name part before the frame number (may be empty).
    pub prefix: String,
    /// File-name part after the frame number, including the extension
    /// (e.g. `".png"`).
    pub suffix: String,
    /// Zero-pad width of the frame number; 0 = unpadded (`7`, not `0007`).
    pub pad: usize,
    /// Index of the first frame (the file the user picked).
    pub start: u64,
    /// Number of consecutive frames found.
    pub count: u64,
    /// Playback rate: one file = one frame at this many frames per second.
    pub fps: u32,
}

impl SequenceSpec {
    /// The file name of frame `index` (e.g. `frame_0007.png`).
    pub fn frame_file_name(&self, index: u64) -> String {
        if self.pad > 0 {
            format!("{}{:0w$}{}", self.prefix, index, self.suffix, w = self.pad)
        } else {
            format!("{}{}{}", self.prefix, index, self.suffix)
        }
    }

    /// Full path of the first frame — the sequence's identity in the GUI
    /// (media-bin entry, duplicate detection).
    pub fn first_path(&self) -> PathBuf {
        self.dir.join(self.frame_file_name(self.start))
    }

    /// The printf-style pattern file name (e.g. `frame_%04d.png`) — what
    /// `imagesequencesrc` consumes, and a compact display name for the GUI.
    pub fn pattern_name(&self) -> String {
        if self.pad > 0 {
            format!("{}%0{}d{}", self.prefix, self.pad, self.suffix)
        } else {
            format!("{}%d{}", self.prefix, self.suffix)
        }
    }

    /// Whether the frames are EXR (needs Rust-side conversion before import).
    pub fn is_exr(&self) -> bool {
        self.suffix.to_ascii_lowercase().ends_with(".exr")
    }

    /// The sequence's natural duration at its fps.
    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.count as f64 / self.fps.max(1) as f64)
    }

    /// The `imagesequence://` URI for this sequence. `filename_to_uri` performs
    /// the percent-encoding (the `%` of the pattern becomes `%25`, non-ASCII
    /// and spaces are escaped — all verified against gst-discoverer), then the
    /// scheme is swapped and the start/framerate query appended.
    pub fn uri(&self) -> Result<String> {
        let pattern_path = self.dir.join(self.pattern_name());
        let file_uri = gst::glib::filename_to_uri(&pattern_path, None)
            .with_context(|| format!("not an absolute path: {}", pattern_path.display()))?;
        let rest = file_uri
            .strip_prefix("file://")
            .ok_or_else(|| anyhow!("unexpected URI form: {file_uri}"))?;
        Ok(format!(
            "imagesequence://{}?start-index={}&framerate={}/1",
            rest,
            self.start,
            self.fps.max(1)
        ))
    }
}

/// Upper bound on frames scanned, so a pathological directory can't spin the
/// detection loop forever.
const MAX_FRAMES: u64 = 1_000_000;

/// Parse a frame file's name into a spec WITHOUT touching the disk: the
/// **last** run of ASCII digits in the stem is the frame number
/// (`shot2_frame_0001` → `0001`), zero-padded iff it has a leading zero.
/// `count` is 0 (unscanned) and `fps` defaults to 30. Two files belong to the
/// same sequence iff [`SequenceSpec::same_sequence`] holds for their specs.
pub fn parse_frame_path(file: &Path) -> Result<SequenceSpec> {
    let dir = file
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .ok_or_else(|| anyhow!("file has no parent directory"))?
        .to_path_buf();
    let stem = file
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("file name is not valid Unicode"))?;
    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .ok_or_else(|| anyhow!("file has no extension"))?;

    // Find the last run of ASCII digits in the stem.
    let bytes = stem.as_bytes();
    let end = bytes
        .iter()
        .rposition(|b| b.is_ascii_digit())
        .map(|i| i + 1)
        .ok_or_else(|| {
            anyhow!("no frame number in the file name (expected something like frame_0001.png)")
        })?;
    let begin = bytes[..end]
        .iter()
        .rposition(|b| !b.is_ascii_digit())
        .map(|i| i + 1)
        .unwrap_or(0);
    let digits = &stem[begin..end];
    let start: u64 = digits
        .parse()
        .with_context(|| format!("frame number {digits} is too large"))?;
    let pad = if digits.len() > 1 && digits.starts_with('0') {
        digits.len()
    } else {
        0
    };

    Ok(SequenceSpec {
        dir,
        prefix: stem[..begin].to_string(),
        suffix: format!("{}.{}", &stem[end..], ext),
        pad,
        start,
        count: 0,
        fps: 30,
    })
}

impl SequenceSpec {
    /// Same directory, prefix, suffix and padding — i.e. the same numbered run,
    /// regardless of which frame was picked as the start.
    pub fn same_sequence(&self, other: &SequenceSpec) -> bool {
        self.dir == other.dir
            && self.prefix == other.prefix
            && self.suffix == other.suffix
            && self.pad == other.pad
    }
}

/// Detect a sequence from its first file ([`parse_frame_path`]), then count
/// the consecutive files forward from the picked index.
pub fn detect_sequence(first_file: &Path) -> Result<SequenceSpec> {
    let mut spec = parse_frame_path(first_file)?;
    let start = spec.start;
    while spec.count < MAX_FRAMES {
        let candidate = spec.dir.join(spec.frame_file_name(start + spec.count));
        if !candidate.is_file() {
            break;
        }
        spec.count += 1;
    }
    if spec.count == 0 {
        bail!("{} does not exist", first_file.display());
    }
    Ok(spec)
}

/// Root of the EXR→PNG conversion cache (under the OS temp dir).
fn cache_root() -> PathBuf {
    std::env::temp_dir().join("kuvatin").join("seq-cache")
}

/// Marker file written after a complete conversion; its mtime doubles as the
/// "last used" stamp for the startup sweep.
const CACHE_MARKER: &str = ".complete";

/// Content key for a conversion: source identity + frame range + the first and
/// last frames' mtimes (so a re-render of the frames invalidates the cache).
/// The fps is deliberately excluded — the converted pixels don't depend on it.
fn cache_key(spec: &SequenceSpec) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    spec.dir.hash(&mut h);
    spec.prefix.hash(&mut h);
    spec.suffix.hash(&mut h);
    spec.pad.hash(&mut h);
    spec.start.hash(&mut h);
    spec.count.hash(&mut h);
    for index in [spec.start, spec.start + spec.count.saturating_sub(1)] {
        let mtime = std::fs::metadata(spec.dir.join(spec.frame_file_name(index)))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        mtime.hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

/// Encode one linear-light channel to the sRGB transfer curve (clamped 0..1).
/// EXR is scene-referred linear; without this the frames render far too dark.
fn srgb_encode(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Flatten a decoded frame to display-referred 8-bit RGBA. Float images
/// (EXR) get the linear→sRGB transfer; everything else is already
/// display-referred and converts directly.
fn to_display_rgba8(img: image::DynamicImage) -> image::RgbaImage {
    match img {
        image::DynamicImage::ImageRgb32F(_) | image::DynamicImage::ImageRgba32F(_) => {
            let f = img.into_rgba32f();
            let (w, h) = (f.width(), f.height());
            let mut out = image::RgbaImage::new(w, h);
            for (src, dst) in f.pixels().zip(out.pixels_mut()) {
                let e = |c: f32| (srgb_encode(c) * 255.0 + 0.5) as u8;
                // Alpha stays linear — only color channels get the curve.
                let a = (src.0[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                *dst = image::Rgba([e(src.0[0]), e(src.0[1]), e(src.0[2]), a]);
            }
            out
        }
        other => other.into_rgba8(),
    }
}

/// Convert an EXR sequence to a PNG sequence in the temp cache, returning a
/// spec for the converted frames (same count/fps, renumbered from 0).
///
/// Frames convert in parallel; `progress(done, total)` is called from worker
/// threads. Setting `cancel` aborts between frames and removes the partial
/// output. A complete cached conversion (marker present, all frames on disk)
/// is reused without any decoding.
pub fn convert_exr_sequence(
    spec: &SequenceSpec,
    progress: impl Fn(u64, u64) + Send + Sync,
    cancel: &AtomicBool,
) -> Result<SequenceSpec> {
    let out_dir = cache_root().join(cache_key(spec));
    let converted = SequenceSpec {
        dir: out_dir.clone(),
        prefix: "f".into(),
        suffix: ".png".into(),
        pad: 6,
        start: 0,
        count: spec.count,
        fps: spec.fps,
    };
    let marker = out_dir.join(CACHE_MARKER);

    // Cache hit: marker written + every frame still present → reuse. Touch the
    // marker so the startup sweep sees the entry as recently used.
    if marker.is_file()
        && (0..spec.count).all(|i| out_dir.join(converted.frame_file_name(i)).is_file())
    {
        let _ = std::fs::write(&marker, b"ok");
        return Ok(converted);
    }

    // (Re)convert from scratch — a marker-less dir is a crashed/cancelled run.
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("create {}", out_dir.display()))?;

    let done = AtomicU64::new(0);
    let result: Result<()> = (0..spec.count).into_par_iter().try_for_each(|i| {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        let src = spec.dir.join(spec.frame_file_name(spec.start + i));
        let img =
            image::open(&src).with_context(|| format!("decode {}", src.display()))?;
        let out = out_dir.join(converted.frame_file_name(i));
        to_display_rgba8(img)
            .save_with_format(&out, image::ImageFormat::Png)
            .with_context(|| format!("write {}", out.display()))?;
        progress(done.fetch_add(1, Ordering::Relaxed) + 1, spec.count);
        Ok(())
    });
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&out_dir);
        return Err(e);
    }
    std::fs::write(&marker, b"ok").with_context(|| format!("write {}", marker.display()))?;
    Ok(converted)
}

/// Hardware H.264 (NVENC) refuses tiny frames (its floor is around 145×49),
/// and the encoder rank is fixed at init so there is no software fallback —
/// so a sequence smaller than this is scaled UP (aspect kept) rather than
/// failing with an opaque "stream error".
const MIN_RENDER_W: u32 = 160;
const MIN_RENDER_H: u32 = 96;

/// The MP4 frame size for a `w`×`h` source: native, except tiny sources are
/// upscaled to the encoder floor; even dimensions (NV12) within the canvas range.
fn render_size(w: u32, h: u32) -> (i32, i32) {
    let (w, h) = (w.max(1) as f64, h.max(1) as f64);
    let scale = (MIN_RENDER_W as f64 / w).max(MIN_RENDER_H as f64 / h).max(1.0);
    let w = ((w * scale).round() as i32 & !1).clamp(16, 7680);
    let h = ((h * scale).round() as i32 & !1).clamp(16, 4320);
    (w, h)
}

/// Progress of [`render_to_mp4`]: an EXR sequence converts first, then encodes.
#[derive(Clone, Copy, Debug)]
pub enum RenderProgress {
    Converting { done: u64, total: u64 },
    /// Encode progress, 0..1.
    Rendering(f32),
}

/// Render a sequence to an H.264 MP4 at its native frame size, blocking until
/// the encode finishes (the headless "Render image sequence to MP4" action).
/// EXR sequences are converted first. `progress` is called from worker
/// threads; setting `cancel` aborts (the conversion between frames, the encode
/// at the next poll) with an error containing "cancelled". On any failure the
/// partial file is removed.
pub fn render_to_mp4(
    spec: &SequenceSpec,
    out: &Path,
    fps: u32,
    progress: impl Fn(RenderProgress) + Send + Sync,
    cancel: &AtomicBool,
) -> Result<()> {
    use crate::project::{ExportSettings, Project, RenderStatus, VideoCodec};

    let mut spec = if spec.is_exr() {
        convert_exr_sequence(
            spec,
            |done, total| progress(RenderProgress::Converting { done, total }),
            cancel,
        )?
    } else {
        spec.clone()
    };
    spec.fps = fps.clamp(1, 240);
    let first = spec.first_path();
    let (w, h) = image::image_dimensions(&first)
        .with_context(|| format!("read the frame size of {}", first.display()))?;
    let (w, h) = render_size(w, h);
    // ~0.12 bit per pixel per frame: ≈7.5 Mbit/s for 1080p30, 4–40 Mbit/s overall.
    let bitrate_kbps =
        ((w as f64 * h as f64 * spec.fps as f64 * 0.12) / 1000.0).clamp(4000.0, 40000.0) as u32;

    let mut project = Project::new(|_| {})?;
    project.set_canvas_size(w, h);
    project.append_clip_uri(&spec.uri()?, 0, None)?;
    project.begin_render(
        out,
        ExportSettings {
            codec: VideoCodec::H264,
            width: w,
            height: h,
            fps: spec.fps,
            bitrate_kbps,
        },
    )?;
    // Poll to completion. A pipeline that stops posting progress for a minute
    // is stuck (some stalls never post EOS/ERROR) — fail rather than hang.
    let mut last = (-1.0f32, Instant::now());
    let outcome = loop {
        if cancel.load(Ordering::Relaxed) {
            // Graceful teardown (EOS so the muxer finalizes) + partial file removed.
            let _ = project.cancel_render(out, true);
            return Err(anyhow!("cancelled"));
        }
        match project.render_status() {
            RenderStatus::Done => break Ok(()),
            RenderStatus::Failed(e) => break Err(anyhow!("render failed: {e}")),
            RenderStatus::Rendering(f) => {
                progress(RenderProgress::Rendering(f));
                if (f - last.0).abs() > 0.0005 {
                    last = (f, Instant::now());
                } else if last.1.elapsed() > Duration::from_secs(60) {
                    break Err(anyhow!("render stalled"));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    };
    let _ = project.end_render();
    if outcome.is_err() {
        let _ = std::fs::remove_file(out);
    }
    outcome
}

/// Best-effort startup sweep of the conversion cache: remove entries whose
/// marker (touched on every reuse) is older than `max_age`. Never errors.
pub fn sweep_sequence_cache(max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(cache_root()) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let dir = entry.path();
        let stamp = std::fs::metadata(dir.join(CACHE_MARKER))
            .or_else(|_| std::fs::metadata(&dir))
            .and_then(|m| m.modified());
        let stale = match stamp {
            Ok(t) => now.duration_since(t).map(|age| age > max_age).unwrap_or(false),
            // Unreadable entry: treat as stale garbage.
            Err(_) => true,
        };
        if stale {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique, self-cleaning temp dir for fixture files.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "kuvatin-seqtest-{tag}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"x").unwrap();
    }

    #[test]
    fn detects_zero_padded_sequence() {
        let t = TempDir::new("padded");
        for i in 1..=5 {
            touch(&t.0, &format!("frame_{i:04}.png"));
        }
        let spec = detect_sequence(&t.0.join("frame_0001.png")).unwrap();
        assert_eq!(spec.prefix, "frame_");
        assert_eq!(spec.suffix, ".png");
        assert_eq!(spec.pad, 4);
        assert_eq!(spec.start, 1);
        assert_eq!(spec.count, 5);
        assert_eq!(spec.pattern_name(), "frame_%04d.png");
        assert_eq!(spec.frame_file_name(3), "frame_0003.png");
    }

    #[test]
    fn detects_unpadded_and_crosses_digit_widths() {
        let t = TempDir::new("unpadded");
        for i in 8..=12 {
            touch(&t.0, &format!("img{i}.jpg"));
        }
        let spec = detect_sequence(&t.0.join("img8.jpg")).unwrap();
        assert_eq!(spec.pad, 0);
        assert_eq!(spec.count, 5, "9→10 width change must not stop the scan");
        assert_eq!(spec.pattern_name(), "img%d.jpg");
    }

    #[test]
    fn starts_at_the_picked_frame_and_stops_at_gaps() {
        let t = TempDir::new("gap");
        for i in [1, 2, 3, 5, 6] {
            touch(&t.0, &format!("s_{i:03}.png"));
        }
        // Picked mid-sequence: starts there, earlier frames ignored.
        let spec = detect_sequence(&t.0.join("s_002.png")).unwrap();
        assert_eq!(spec.start, 2);
        assert_eq!(spec.count, 2, "the gap at 4 ends the run");
    }

    #[test]
    fn uses_the_last_digit_run() {
        let t = TempDir::new("lastrun");
        touch(&t.0, "shot2_take3_0007.png");
        let spec = detect_sequence(&t.0.join("shot2_take3_0007.png")).unwrap();
        assert_eq!(spec.prefix, "shot2_take3_");
        assert_eq!(spec.start, 7);
        assert_eq!(spec.pad, 4);
    }

    #[test]
    fn rejects_unnumbered_and_missing_files() {
        let t = TempDir::new("reject");
        touch(&t.0, "picture.png");
        assert!(detect_sequence(&t.0.join("picture.png")).is_err());
        assert!(detect_sequence(&t.0.join("frame_0001.png")).is_err());
    }

    #[test]
    fn builds_a_percent_encoded_uri() {
        let spec = SequenceSpec {
            dir: PathBuf::from("C:\\media\\my työ"),
            prefix: "frame_".into(),
            suffix: ".png".into(),
            pad: 4,
            start: 10,
            count: 20,
            fps: 25,
        };
        let uri = spec.uri().unwrap();
        // '%' → %25, 'ö' → UTF-8 percent-escapes, space → %20; query intact.
        assert_eq!(
            uri,
            "imagesequence:///C:/media/my%20ty%C3%B6/frame_%2504d.png?start-index=10&framerate=25/1"
        );
        assert_eq!(spec.duration(), Duration::from_millis(800));
    }

    #[test]
    fn converts_exr_frames_to_srgb_png_and_reuses_the_cache() {
        let t = TempDir::new("exr");
        // Three 4x3 EXR frames of linear 0.5 grey — sRGB-encodes to ~188.
        for i in 0..3u32 {
            let img = image::Rgb32FImage::from_pixel(4, 3, image::Rgb([0.5f32, 0.5, 0.5]));
            image::DynamicImage::ImageRgb32F(img)
                .save_with_format(t.0.join(format!("r_{i:03}.exr")), image::ImageFormat::OpenExr)
                .unwrap();
        }
        let mut spec = detect_sequence(&t.0.join("r_000.exr")).unwrap();
        spec.fps = 24;
        assert!(spec.is_exr());
        assert_eq!(spec.count, 3);

        let progressed = AtomicU64::new(0);
        let cancel = AtomicBool::new(false);
        let conv = convert_exr_sequence(
            &spec,
            |done, total| {
                assert!(done <= total);
                progressed.store(done, Ordering::Relaxed);
            },
            &cancel,
        )
        .unwrap();
        assert_eq!(progressed.load(Ordering::Relaxed), 3);
        assert_eq!(conv.count, 3);
        assert_eq!(conv.fps, 24);
        assert!(!conv.is_exr());

        // Frames exist and carry the sRGB-encoded value (0.5 linear ≈ 188).
        let png = image::open(conv.dir.join("f000001.png")).unwrap().into_rgba8();
        let px = png.get_pixel(0, 0).0;
        assert!((186..=190).contains(&px[0]), "srgb(0.5) ≈ 188, got {}", px[0]);
        assert_eq!(px[3], 255);

        // Second call is a cache hit (no progress callbacks fire).
        let hit = convert_exr_sequence(&spec, |_, _| panic!("cache miss"), &cancel).unwrap();
        assert_eq!(hit.dir, conv.dir);
        let _ = std::fs::remove_dir_all(&conv.dir);
    }

    /// Frames parse without touching the disk, and frames of one run compare
    /// equal regardless of which one was picked.
    #[test]
    fn parses_frame_names_and_matches_runs() {
        let a = parse_frame_path(Path::new("C:/r/shot_0007.png")).unwrap();
        let b = parse_frame_path(Path::new("C:/r/shot_0120.png")).unwrap();
        let c = parse_frame_path(Path::new("C:/r/other_0007.png")).unwrap();
        assert_eq!((a.prefix.as_str(), a.pad, a.start, a.count), ("shot_", 4, 7, 0));
        assert!(a.same_sequence(&b));
        assert!(!a.same_sequence(&c));
        assert!(parse_frame_path(Path::new("C:/r/photo.png")).is_err());
    }

    /// Native sizes pass through (rounded to even); tiny sources are upscaled
    /// to the encoder floor with their aspect kept; odd sizes become even.
    #[test]
    fn render_size_keeps_native_but_lifts_tiny_frames() {
        assert_eq!(render_size(1920, 1080), (1920, 1080));
        assert_eq!(render_size(1921, 1081), (1920, 1080));
        // 64x36 (16:9) → scaled by 96/36 = 2.67 → 171x96 → even 170x96
        assert_eq!(render_size(64, 36), (170, 96));
        // 100x300 (portrait) → scaled by 160/100 = 1.6 → 160x480
        assert_eq!(render_size(100, 300), (160, 480));
    }

    /// Headless render: generated frames → an H.264 MP4. Two sizes: a normal
    /// one (native 320x180) and a tiny one that must be upscaled past NVENC's
    /// floor instead of failing. Self-skips without a working GStreamer.
    #[test]
    fn renders_sequences_to_mp4_including_tiny_ones() {
        if crate::project::Project::new(|_| {}).is_err() {
            eprintln!("skipping renders_sequences_to_mp4_including_tiny_ones: no GStreamer");
            return;
        }
        for (tag, w, h) in [("mp4", 320u32, 180u32), ("mp4tiny", 64, 36)] {
            let t = TempDir::new(tag);
            for i in 1..=8u32 {
                image::RgbaImage::from_pixel(w, h, image::Rgba([(i * 25) as u8, 120, 200, 255]))
                    .save(t.0.join(format!("f_{i:03}.png")))
                    .unwrap();
            }
            let spec = detect_sequence(&t.0.join("f_001.png")).unwrap();
            let out = t.0.join("f.mp4");
            let reported = AtomicBool::new(false);
            render_to_mp4(
                &spec,
                &out,
                24,
                |p| {
                    if let RenderProgress::Rendering(f) = p {
                        assert!((0.0..=1.0).contains(&f));
                        reported.store(true, Ordering::Relaxed);
                    }
                },
                &AtomicBool::new(false),
            )
            .unwrap_or_else(|e| panic!("{w}x{h} render: {e:#}"));
            let len = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
            assert!(len > 1000, "{w}x{h} mp4 too small: {len} bytes");
            assert!(reported.load(Ordering::Relaxed), "encode progress was reported");
        }
    }

    /// A cancelled render tears down cleanly and leaves no partial MP4.
    /// Self-skips without a working GStreamer.
    #[test]
    fn cancelling_a_render_leaves_no_partial_file() {
        if crate::project::Project::new(|_| {}).is_err() {
            eprintln!("skipping cancelling_a_render_leaves_no_partial_file: no GStreamer");
            return;
        }
        let t = TempDir::new("mp4cancel");
        for i in 1..=8u32 {
            image::RgbaImage::from_pixel(320, 180, image::Rgba([0, (i * 30) as u8, 90, 255]))
                .save(t.0.join(format!("f_{i:03}.png")))
                .unwrap();
        }
        let spec = detect_sequence(&t.0.join("f_001.png")).unwrap();
        let out = t.0.join("f.mp4");
        // Cancelled before the first poll: the render is torn down immediately.
        let err = render_to_mp4(&spec, &out, 24, |_| {}, &AtomicBool::new(true)).unwrap_err();
        assert!(err.to_string().contains("cancelled"), "{err:#}");
        assert!(!out.exists(), "partial output removed");
    }

    #[test]
    fn cancelled_conversion_leaves_no_partial_cache() {
        let t = TempDir::new("cancel");
        let img = image::Rgb32FImage::from_pixel(2, 2, image::Rgb([0.1f32, 0.2, 0.3]));
        for i in 0..2u32 {
            image::DynamicImage::ImageRgb32F(img.clone())
                .save_with_format(t.0.join(format!("c_{i}.exr")), image::ImageFormat::OpenExr)
                .unwrap();
        }
        let spec = detect_sequence(&t.0.join("c_0.exr")).unwrap();
        let cancel = AtomicBool::new(true); // cancelled before the first frame
        let err = convert_exr_sequence(&spec, |_, _| {}, &cancel).unwrap_err();
        assert!(err.to_string().contains("cancelled"));
        assert!(!cache_root().join(cache_key(&spec)).exists());
    }
}
