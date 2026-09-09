//! Headless "Render image sequence to MP4" (the context-menu action): resolve
//! the selection — numbered frames and/or folders of them — into distinct
//! sequences, then render each to an H.264 MP4 next to its frames.

use anyhow::{anyhow, Result};
use kuvatin_core::naming::ensure_unique;
use kuvatin_video::{
    detect_sequence, is_frame_file, parse_frame_path, render_to_mp4, RenderProgress,
    SequenceSpec,
};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

/// Outcome of a sequence render over a selection.
pub struct SequenceReport {
    /// The MP4s written.
    pub rendered: Vec<PathBuf>,
    /// `(selected path or first frame, error)` for everything that didn't render.
    pub failures: Vec<(PathBuf, String)>,
    /// The user cancelled; `rendered` holds what finished before that.
    pub cancelled: bool,
}

/// Resolve `paths` into distinct sequences. A selected frame contributes the
/// run it belongs to; a folder contributes every numbered run inside it. When
/// several selected frames belong to one run, the LOWEST picked frame is the
/// start (select all 240 frames → one sequence from the first). Lone frames
/// are rejected — there is nothing to animate.
pub fn resolve_sequences(paths: &[PathBuf]) -> (Vec<SequenceSpec>, Vec<(PathBuf, String)>) {
    let mut candidates: Vec<SequenceSpec> = Vec::new();
    let mut failures = Vec::new();
    for p in paths {
        if p.is_dir() {
            let before = candidates.len();
            if let Ok(rd) = std::fs::read_dir(p) {
                for f in rd.flatten().map(|e| e.path()) {
                    if f.is_file() && is_frame_file(&f) {
                        if let Ok(spec) = parse_frame_path(&f) {
                            candidates.push(spec);
                        }
                    }
                }
            }
            if candidates.len() == before {
                failures.push((p.clone(), "no numbered image frames in this folder".into()));
            }
        } else if !is_frame_file(p) {
            // A selected .tif/.bmp frame used to fail late inside GStreamer.
            failures.push((
                p.clone(),
                format!("not a sequence frame format ({})", kuvatin_video::FRAME_EXTENSIONS.join(", ")),
            ));
        } else {
            match parse_frame_path(p) {
                Ok(spec) => candidates.push(spec),
                Err(e) => failures.push((p.clone(), e.to_string())),
            }
        }
    }

    // One entry per run, keeping the lowest start seen.
    let mut firsts: Vec<SequenceSpec> = Vec::new();
    for c in candidates {
        match firsts.iter_mut().find(|f| f.same_sequence(&c)) {
            Some(f) => {
                if c.start < f.start {
                    *f = c;
                }
            }
            None => firsts.push(c),
        }
    }

    let mut specs = Vec::new();
    for f in firsts {
        let first = f.first_path();
        match detect_sequence(&first) {
            Ok(spec) if spec.count >= 2 => specs.push(spec),
            Ok(_) => failures.push((first, "only one frame — not a sequence".into())),
            Err(e) => failures.push((first, e.to_string())),
        }
    }
    specs.sort_by_key(|s| s.first_path());
    (specs, failures)
}

/// `<dir>/<prefix>.mp4` with the trailing separator trimmed (`frame_%04d.png`
/// → `frame.mp4`); a bare-number run takes its folder's name. Never
/// overwrites — collisions get `-1`, `-2`, … like image outputs.
pub fn output_path(spec: &SequenceSpec) -> PathBuf {
    let stem = spec.prefix.trim_end_matches(['_', '-', '.', ' ']);
    let stem = if stem.is_empty() {
        spec.dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("sequence")
            .to_string()
    } else {
        stem.to_string()
    };
    ensure_unique(spec.dir.join(format!("{stem}.mp4")))
}

/// Render every sequence in the selection at `fps`. `progress(fraction,
/// status)` covers the whole selection (an EXR run spends its first 30 % on
/// conversion); `cancel` stops after the current frame / next encode poll.
pub fn run(
    paths: &[PathBuf],
    fps: u32,
    progress: &(dyn Fn(f32, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<SequenceReport> {
    let (specs, mut failures) = resolve_sequences(paths);
    if specs.is_empty() && failures.is_empty() {
        return Err(anyhow!("no image sequence in selection"));
    }
    let n = specs.len();
    let mut rendered = Vec::new();
    for (i, spec) in specs.iter().enumerate() {
        let label = if n > 1 {
            format!("{} ({} of {n})", spec.pattern_name(), i + 1)
        } else {
            spec.pattern_name()
        };
        let is_exr = spec.is_exr();
        let out = output_path(spec);
        let report = |p: RenderProgress| {
            let (local, status) = match p {
                RenderProgress::Converting { done, total } => (
                    0.3 * done as f32 / total.max(1) as f32,
                    format!("{label}  ·  converting EXR {done} / {total}"),
                ),
                RenderProgress::Rendering(f) => (
                    if is_exr { 0.3 + 0.7 * f } else { f },
                    format!("{label}  ·  rendering {:.0}%", f * 100.0),
                ),
            };
            progress((i as f32 + local) / n as f32, &status);
        };
        match render_to_mp4(spec, &out, fps, report, cancel) {
            Ok(()) => rendered.push(out),
            Err(e) if e.is::<kuvatin_video::Cancelled>() => {
                return Ok(SequenceReport { rendered, failures, cancelled: true });
            }
            Err(e) => failures.push((spec.first_path(), format!("{e:#}"))),
        }
    }
    Ok(SequenceReport { rendered, failures, cancelled: false })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &std::path::Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, b"x").unwrap();
        p
    }

    /// Picking frames 3 and 1 of one run yields ONE sequence starting at 1.
    #[test]
    fn selected_frames_of_one_run_merge_from_the_lowest() {
        let t = tempfile::tempdir().unwrap();
        let frames: Vec<PathBuf> = (1..=5).map(|i| touch(t.path(), &format!("f_{i:03}.png"))).collect();
        let (specs, failures) = resolve_sequences(&[frames[2].clone(), frames[0].clone()]);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(specs.len(), 1);
        assert_eq!((specs[0].start, specs[0].count), (1, 5));
    }

    /// A folder yields every run inside it; unnumbered images are ignored.
    #[test]
    fn a_folder_yields_each_run_inside_it() {
        let t = tempfile::tempdir().unwrap();
        for i in 1..=3 {
            touch(t.path(), &format!("a_{i:03}.png"));
        }
        for i in 1..=2 {
            touch(t.path(), &format!("b_{i}.jpg"));
        }
        touch(t.path(), "photo.png");
        let (specs, failures) = resolve_sequences(&[t.path().to_path_buf()]);
        assert!(failures.is_empty(), "{failures:?}");
        let prefixes: Vec<&str> = specs.iter().map(|s| s.prefix.as_str()).collect();
        assert_eq!(prefixes, ["a_", "b_"]);
    }

    /// Unnumbered files, lone frames and frame-less folders are reported, not
    /// silently skipped.
    #[test]
    fn rejects_what_is_not_a_sequence() {
        let t = tempfile::tempdir().unwrap();
        let photo = touch(t.path(), "photo.png");
        let lone = touch(t.path(), "single_7.png");
        let empty = t.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        let tif = touch(t.path(), "scan_0001.tif");
        let (specs, failures) =
            resolve_sequences(&[photo.clone(), lone.clone(), empty.clone(), tif.clone()]);
        assert!(specs.is_empty());
        let failed: Vec<&std::path::Path> = failures.iter().map(|(p, _)| p.as_path()).collect();
        assert_eq!(failed, [photo.as_path(), empty.as_path(), tif.as_path(), lone.as_path()]);
        assert!(failures[2].1.contains("not a sequence frame format"));
        assert!(failures[3].1.contains("only one frame"));
    }

    /// Output naming: trimmed prefix, folder name for bare numbers, never
    /// overwriting.
    #[test]
    fn output_names_derive_from_the_prefix_and_never_collide() {
        let t = tempfile::tempdir().unwrap();
        let renders = t.path().join("renders");
        std::fs::create_dir(&renders).unwrap();
        let mut spec = detect_sequence(&touch(&renders, "frame_0001.png")).unwrap();
        assert_eq!(output_path(&spec), renders.join("frame.mp4"));
        std::fs::write(renders.join("frame.mp4"), b"taken").unwrap();
        assert_eq!(output_path(&spec), renders.join("frame-1.mp4"));
        spec.prefix.clear();
        assert_eq!(output_path(&spec), renders.join("renders.mp4"));
    }
}
