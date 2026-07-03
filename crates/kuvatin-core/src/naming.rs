use crate::format::OutputFormat;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputPolicy {
    /// Text appended to the file stem before the extension, e.g. `-kuvatin`
    /// produces `photo-kuvatin.webp`. May be empty.
    #[serde(default = "default_suffix")]
    pub suffix: String,
    /// When true, outputs are written into a subfolder (next to the source, or
    /// inside the chosen output folder) named after the suffix, instead of
    /// alongside the originals.
    #[serde(default)]
    pub subfolder: bool,
}

fn default_suffix() -> String {
    "-kuvatin".to_string()
}

impl Default for OutputPolicy {
    fn default() -> Self {
        OutputPolicy {
            suffix: default_suffix(),
            subfolder: false,
        }
    }
}

/// Strip characters that are path separators or invalid in Windows file names.
/// The suffix comes from user settings / presets.toml and is interpolated
/// straight into file and folder names — `..\` or `x/y` in it would otherwise
/// write outside the intended directory.
fn sanitize_component(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect()
}

/// Folder name derived from a suffix: leading/trailing separators and spaces are
/// trimmed (so `-kuvatin` becomes `kuvatin`); falls back to `output` if empty.
pub fn subfolder_name(suffix: &str) -> String {
    let sanitized = sanitize_component(suffix);
    let trimmed = sanitized
        .trim()
        .trim_matches(|c| c == '-' || c == '_' || c == ' ' || c == '.');
    if trimmed.is_empty() {
        "output".to_string()
    } else {
        trimmed.to_string()
    }
}

/// The output file name for a given stem: `<stem><suffix>.<ext>`.
pub fn output_file_name(stem: &str, suffix: &str, format: OutputFormat) -> String {
    format!("{stem}{}.{}", sanitize_component(suffix), format.extension())
}

/// Expand the policy into a final path next to `input` (or in a suffix-named
/// subfolder when `policy.subfolder` is set), with the format's extension. Does
/// NOT check for collisions (see [`ensure_unique`]).
pub fn render_output_path(policy: &OutputPolicy, input: &Path, format: OutputFormat) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("image");
    let mut dir = input
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    if policy.subfolder {
        dir = dir.join(subfolder_name(&policy.suffix));
    }
    dir.join(output_file_name(stem, &policy.suffix, format))
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
    fn default_suffix_appends() {
        let p = render_output_path(
            &OutputPolicy::default(),
            Path::new("/photos/cat.jpg"),
            OutputFormat::Webp,
        );
        assert!(p.ends_with("cat-kuvatin.webp"), "got {p:?}");
    }

    #[test]
    fn empty_suffix_keeps_name() {
        let policy = OutputPolicy { suffix: String::new(), subfolder: false };
        let p = render_output_path(&policy, Path::new("a/b/x.png"), OutputFormat::Png);
        assert_eq!(p.file_name().unwrap().to_str().unwrap(), "x.png");
    }

    #[test]
    fn subfolder_uses_trimmed_suffix() {
        let policy = OutputPolicy { suffix: "_min".into(), subfolder: true };
        let p = render_output_path(&policy, Path::new("/a/b/x.png"), OutputFormat::Png);
        assert!(p.ends_with("min/x_min.png"), "got {p:?}");
    }

    #[test]
    fn subfolder_name_trims_and_falls_back() {
        assert_eq!(subfolder_name("-kuvatin"), "kuvatin");
        assert_eq!(subfolder_name("_min "), "min");
        assert_eq!(subfolder_name("-"), "output");
        assert_eq!(subfolder_name(""), "output");
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
