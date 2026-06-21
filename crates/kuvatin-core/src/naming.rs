use crate::format::OutputFormat;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputPolicy {
    /// Filename stem pattern (without extension). Tokens: {name} {w} {h} {preset}.
    pub pattern: String,
}

impl Default for OutputPolicy {
    fn default() -> Self {
        OutputPolicy { pattern: "{name}_{w}x{h}".to_string() }
    }
}

/// Expand the pattern into a final path next to `input`, with the format's
/// extension. Does NOT check for collisions (see `ensure_unique`).
pub fn render_output_path(
    policy: &OutputPolicy,
    input: &Path,
    format: OutputFormat,
    out_w: u32,
    out_h: u32,
    preset_name: &str,
) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("image");
    let rendered = policy
        .pattern
        .replace("{name}", stem)
        .replace("{w}", &out_w.to_string())
        .replace("{h}", &out_h.to_string())
        .replace("{preset}", preset_name);
    let dir = input.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!("{rendered}.{}", format.extension()))
}

/// If `path` exists, append `-1`, `-2`, ... to the stem until free.
pub fn ensure_unique(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("image").to_string();
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("").to_string();
    for n in 1.. {
        let candidate = dir.join(format!("{stem}-{n}.{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn default_pattern_expands() {
        let p = render_output_path(
            &OutputPolicy::default(),
            Path::new("/photos/cat.jpg"),
            OutputFormat::Webp,
            800,
            600,
            "Convert to WebP",
        );
        assert!(p.ends_with("cat_800x600.webp"));
    }

    #[test]
    fn preset_token_expands() {
        let policy = OutputPolicy { pattern: "{name}-{preset}".into() };
        let p = render_output_path(&policy, Path::new("a/b/x.png"), OutputFormat::Png, 1, 1, "thumb");
        assert_eq!(p.file_name().unwrap().to_str().unwrap(), "x-thumb.png");
    }

    #[test]
    fn ensure_unique_on_collision() {
        let dir = tempfile::tempdir().unwrap();
        let taken = dir.path().join("img.png");
        std::fs::write(&taken, b"x").unwrap();
        let unique = ensure_unique(taken.clone());
        assert_eq!(unique.file_name().unwrap().to_str().unwrap(), "img-1.png");
    }
}
