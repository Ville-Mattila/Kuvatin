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

use crate::project::normalize_render_size;

/// The user cancelled an EXR conversion or a render. A typed error so callers
/// match on it (`err.is::<Cancelled>()`) instead of grepping messages — a
/// GStreamer error that happens to contain the word "cancelled" must not be
/// mistaken for a user cancel and swallowed silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Frame formats the sequence engine accepts (lower-case, no dot) — the ONE
/// list the sequence dialog, the headless resolver and the Explorer
/// registration read.
pub const FRAME_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "exr"];

/// Video containers the engine demuxes (lower-case, no dot) — what the media
/// dialog offers and what a Videos-mode drop keeps.
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "mkv", "webm", "avi", "m4v", "wmv"];

/// Whether `path` has a sequence-frame extension (any case).
pub fn is_frame_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| FRAME_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// EXR→PNG cache policy: entries unused for this long are swept …
pub const CACHE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);
/// … and the cache as a whole is kept under this many bytes (oldest-used
/// entries go first). A 4K sequence converts to roughly 8 MB per frame.
pub const CACHE_MAX_BYTES: u64 = 6 << 30;

/// A numbered image sequence: files named `<prefix><NUMBER><suffix>` in `dir`,
/// starting at `start`, `count` consecutive frames, played at `fps`.
// Serialised into saved projects (see document.rs), so the media bin can
// re-add a sequence after a reopen rather than only showing it on the timeline.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// A literal `%` in the prefix or suffix is escaped as `%%`: the element
    /// hands the pattern to `g_strdup_printf`, so `50%_off_0001.png` would
    /// otherwise become a format string (`%_` — wrong names at best, `%s` fed
    /// an integer at worst).
    pub fn pattern_name(&self) -> String {
        let prefix = self.prefix.replace('%', "%%");
        let suffix = self.suffix.replace('%', "%%");
        if self.pad > 0 {
            format!("{prefix}%0{}d{suffix}", self.pad)
        } else {
            format!("{prefix}%d{suffix}")
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

/// Content key for a conversion: source identity + frame range + EVERY frame's
/// (mtime, size). Hashing only the first and last frame let a re-render of
/// frames 50–100 in a 3D app serve stale PNGs as a cache hit; N stats are
/// nothing next to N decodes. The fps is deliberately excluded — the
/// converted pixels don't depend on it.
fn cache_key(spec: &SequenceSpec) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    spec.dir.hash(&mut h);
    spec.prefix.hash(&mut h);
    spec.suffix.hash(&mut h);
    spec.pad.hash(&mut h);
    spec.start.hash(&mut h);
    spec.count.hash(&mut h);
    for index in spec.start..spec.start + spec.count {
        let meta = std::fs::metadata(spec.dir.join(spec.frame_file_name(index))).ok();
        let mtime = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let len = meta.map(|m| m.len()).unwrap_or(0);
        (mtime, len).hash(&mut h);
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
    let key = cache_key(spec);
    let out_dir = cache_root().join(&key);
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
    // marker so the sweep sees the entry as recently used.
    if marker.is_file()
        && (0..spec.count).all(|i| out_dir.join(converted.frame_file_name(i)).is_file())
    {
        let _ = std::fs::write(&marker, b"ok");
        return Ok(converted);
    }

    // Convert into a PRIVATE temp dir and publish it with one atomic rename.
    // Two processes converting the same sequence at once (a right-click on a
    // folder while the GUI imports it) used to remove_dir_all the shared dir
    // under each other and still write the marker over half-deleted frames.
    let _ = std::fs::remove_dir_all(&out_dir); // a marker-less dir is a crashed run
    let tmp_dir = cache_root().join(format!("{key}.tmp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).with_context(|| format!("create {}", tmp_dir.display()))?;

    let done = AtomicU64::new(0);
    let result: Result<()> = (0..spec.count).into_par_iter().try_for_each(|i| {
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled.into());
        }
        let src = spec.dir.join(spec.frame_file_name(spec.start + i));
        let img = image::open(&src).with_context(|| format!("decode {}", src.display()))?;
        let out = tmp_dir.join(converted.frame_file_name(i));
        to_display_rgba8(img)
            .save_with_format(&out, image::ImageFormat::Png)
            .with_context(|| format!("write {}", out.display()))?;
        progress(done.fetch_add(1, Ordering::Relaxed) + 1, spec.count);
        Ok(())
    });
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(e);
    }
    std::fs::write(tmp_dir.join(CACHE_MARKER), b"ok")
        .with_context(|| format!("write {}", tmp_dir.join(CACHE_MARKER).display()))?;
    match std::fs::rename(&tmp_dir, &out_dir) {
        Ok(()) => {}
        // Lost the race: another process published the same key first — use
        // theirs (complete, by the marker) and discard ours.
        Err(_) if marker.is_file() => {
            let _ = std::fs::remove_dir_all(&tmp_dir);
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(e).with_context(|| format!("publish {}", out_dir.display()));
        }
    }
    // Reclaim here, not only at startup: converting several 4K sequences in
    // one session (roughly 8 MB per frame) could pass the size limit with
    // nothing tidying up until the next launch. The entry just published is
    // the most recently used, so it is the last thing this would evict.
    sweep_sequence_cache(CACHE_MAX_AGE, CACHE_MAX_BYTES);
    Ok(converted)
}

/// Progress of [`render_to_mp4`]: an EXR sequence converts first, then encodes.
#[derive(Clone, Copy, Debug)]
pub enum RenderProgress {
    Converting {
        done: u64,
        total: u64,
    },
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
    // Native size, normalized exactly like a GUI export (even, NVENC floor).
    let (w, h) = normalize_render_size(w as i32, h as i32);
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
            return Err(Cancelled.into());
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
    // Headless: there is no preview to restore — dropping the project NULLs the
    // pipeline. (end_render would re-attach the sink, open the audio device
    // and preroll the timeline for a project discarded on the next line.)
    drop(project);
    if outcome.is_err() {
        let _ = std::fs::remove_file(out);
    }
    outcome
}

/// Best-effort sweep of the conversion cache — call from EVERY entry point
/// (GUI start and the headless right-click path): entries unused for longer
/// than `max_age` go, then the oldest-used entries go until the cache fits in
/// `max_bytes`. Temp dirs of a crashed conversion older than an hour go too.
/// Never errors.
pub fn sweep_sequence_cache(max_age: Duration, max_bytes: u64) {
    sweep_cache_in(&cache_root(), max_age, max_bytes);
}

fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

fn sweep_cache_in(root: &Path, max_age: Duration, max_bytes: u64) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let now = std::time::SystemTime::now();
    let mut kept: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let is_tmp = dir
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.contains(".tmp-"))
            .unwrap_or(false);
        let stamp = std::fs::metadata(dir.join(CACHE_MARKER))
            .or_else(|_| std::fs::metadata(&dir))
            .and_then(|m| m.modified());
        let stale = match stamp {
            Ok(t) => {
                let age = now.duration_since(t).unwrap_or_default();
                age > max_age || (is_tmp && age > Duration::from_secs(3600))
            }
            // Unreadable entry: treat as stale garbage.
            Err(_) => true,
        };
        if stale {
            let _ = std::fs::remove_dir_all(&dir);
            continue;
        }
        if let Ok(t) = stamp {
            kept.push((t, dir_size(&dir), dir));
        }
    }
    // Over budget: evict least-recently-used first.
    kept.sort_by_key(|(t, _, _)| *t);
    let mut total: u64 = kept.iter().map(|(_, s, _)| *s).sum();
    for (_, size, dir) in kept {
        if total <= max_bytes {
            break;
        }
        let _ = std::fs::remove_dir_all(&dir);
        total = total.saturating_sub(size);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    /// A unique, self-cleaning temp dir for fixture files.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/test-tmp")
                .join(format!("kuvatin-seqtest-{tag}-{}", std::process::id()));
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
                .save_with_format(
                    t.0.join(format!("r_{i:03}.exr")),
                    image::ImageFormat::OpenExr,
                )
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
        let png = image::open(conv.dir.join("f000001.png"))
            .unwrap()
            .into_rgba8();
        let px = png.get_pixel(0, 0).0;
        assert!(
            (186..=190).contains(&px[0]),
            "srgb(0.5) ≈ 188, got {}",
            px[0]
        );
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
        assert_eq!(
            (a.prefix.as_str(), a.pad, a.start, a.count),
            ("shot_", 4, 7, 0)
        );
        assert!(a.same_sequence(&b));
        assert!(!a.same_sequence(&c));
        assert!(parse_frame_path(Path::new("C:/r/photo.png")).is_err());
    }

    /// Native sizes pass through (rounded to even); tiny sources are upscaled
    /// to the encoder floor with their aspect kept; odd sizes become even.
    #[test]
    fn render_size_keeps_native_but_lifts_tiny_frames() {
        assert_eq!(normalize_render_size(1920, 1080), (1920, 1080));
        assert_eq!(normalize_render_size(1921, 1081), (1920, 1080));
        // 64x36 (16:9) → scaled by 96/36 = 2.67 → 171x96 → even 170x96
        assert_eq!(normalize_render_size(64, 36), (170, 96));
        // 100x300 (portrait) → scaled by 160/100 = 1.6 → 160x480
        assert_eq!(normalize_render_size(100, 300), (160, 480));
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
            assert!(
                reported.load(Ordering::Relaxed),
                "encode progress was reported"
            );
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
        assert!(err.is::<Cancelled>(), "typed cancel, got {err:#}");
        assert!(!out.exists(), "partial output removed");
    }

    /// Cancelling WHILE the encoder runs — the flag flips inside the first
    /// progress report — exercises the teardown of a live render pipeline,
    /// which the pre-start cancel above never reaches. Self-skips without a
    /// working GStreamer.
    #[test]
    fn cancelling_mid_render_stops_and_removes_the_partial() {
        if crate::project::Project::new(|_| {}).is_err() {
            eprintln!("skipping cancelling_mid_render_stops_and_removes_the_partial: no GStreamer");
            return;
        }
        let t = TempDir::new("mp4midcancel");
        // Enough frames that the render outlives the first 100 ms poll even
        // on a hardware encoder.
        for i in 1..=600u32 {
            image::RgbaImage::from_pixel(320, 180, image::Rgba([(i % 255) as u8, 60, 120, 255]))
                .save(t.0.join(format!("f_{i:03}.png")))
                .unwrap();
        }
        let spec = detect_sequence(&t.0.join("f_001.png")).unwrap();
        let out = t.0.join("f.mp4");
        let cancel = AtomicBool::new(false);
        let mid_render = AtomicBool::new(false);
        let err = render_to_mp4(
            &spec,
            &out,
            24,
            |p| {
                if let RenderProgress::Rendering(f) = p {
                    if f < 1.0 {
                        mid_render.store(true, Ordering::Relaxed);
                        cancel.store(true, Ordering::Relaxed);
                    }
                }
            },
            &cancel,
        )
        .unwrap_err();
        assert!(err.is::<Cancelled>(), "typed cancel, got {err:#}");
        assert!(!out.exists(), "partial output removed");
        assert!(
            mid_render.load(Ordering::Relaxed),
            "the cancel was requested mid-render"
        );
    }

    /// A `%` in a frame name must not become a printf directive for
    /// imagesequencesrc (`g_strdup_printf`).
    #[test]
    fn pattern_name_escapes_percent() {
        let spec = parse_frame_path(Path::new("C:/r/50%_off_take%s_0001.png")).unwrap();
        assert_eq!(spec.pattern_name(), "50%%_off_take%%s_%04d.png");
        // …and the URI carries it percent-encoded on top (each % → %25).
        assert!(spec
            .uri()
            .unwrap()
            .contains("50%25%25_off_take%25%25s_%2504d.png"));
    }

    /// Touching a MIDDLE frame changes the cache key (only first/last used to
    /// count), so a partial re-render never serves stale PNGs.
    #[test]
    fn cache_key_sees_every_frame() {
        let t = TempDir::new("key");
        for i in 0..3u32 {
            touch(&t.0, &format!("k_{i:03}.png"));
        }
        let spec = detect_sequence(&t.0.join("k_000.png")).unwrap();
        let before = cache_key(&spec);
        std::fs::File::options()
            .write(true)
            .open(t.0.join("k_001.png"))
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(30))
            .unwrap();
        assert_ne!(before, cache_key(&spec));
    }

    /// The sweep evicts least-recently-used entries until the cache fits the
    /// byte cap, and removes crashed temp dirs, but keeps fresh entries.
    #[test]
    fn sweep_enforces_age_and_size() {
        let root = TempDir::new("sweep");
        let mk = |name: &str, bytes: usize, age_secs: u64| {
            let d = root.0.join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("f000000.png"), vec![0u8; bytes]).unwrap();
            let m = d.join(CACHE_MARKER);
            std::fs::write(&m, b"ok").unwrap();
            std::fs::File::options()
                .write(true)
                .open(&m)
                .unwrap()
                .set_modified(SystemTime::now() - Duration::from_secs(age_secs))
                .unwrap();
            d
        };
        let old = mk("aaaa", 10, 10 * 24 * 3600); // past max age
        let lru = mk("bbbb", 600, 3600); // oldest of the fresh ones
        let fresh = mk("cccc", 600, 60);
        let crashed = root.0.join("dddd.tmp-1");
        std::fs::create_dir_all(&crashed).unwrap();
        std::fs::File::options().write(true).open(&crashed).ok(); // dir mtime = now; a fresh tmp dir is left alone
        sweep_cache_in(&root.0, Duration::from_secs(7 * 24 * 3600), 1000);
        assert!(!old.exists(), "aged out");
        assert!(!lru.exists(), "evicted to fit 1000 bytes (LRU first)");
        assert!(fresh.exists(), "newest kept");
        assert!(
            crashed.exists(),
            "a fresh temp dir may belong to a live conversion"
        );
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
        assert!(err.is::<Cancelled>(), "typed cancel, got {err:#}");
        assert!(!cache_root().join(cache_key(&spec)).exists());
        // …and no temp dir left behind either.
        let leftovers = std::fs::read_dir(cache_root())
            .map(|rd| {
                rd.flatten()
                    .filter(|e| {
                        e.file_name()
                            .to_string_lossy()
                            .starts_with(&cache_key(&spec))
                    })
                    .count()
            })
            .unwrap_or(0);
        assert_eq!(leftovers, 0);
    }
}
