use crate::format::OutputFormat;
use crate::pipeline::{Job, PngOptimize};
use crate::resize::ResizeMode;
use crate::{CoreError, CoreResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub job: Job,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PresetStore {
    #[serde(default)]
    pub presets: Vec<Preset>,
    /// Human-readable note from the last `load_or_init` when the file was
    /// corrupt or partially unreadable (for the UI to surface). Never saved.
    #[serde(skip)]
    pub last_load_warning: Option<String>,
}

impl PresetStore {
    /// The presets shipped on first run.
    pub fn builtin() -> Self {
        // Default preset: lossy PNG compression (libimagequant + oxipng final
        // pass), tuned for strong size reduction with no visible loss. Uses quality.
        let compress_png = Job {
            format: OutputFormat::Png,
            png: PngOptimize::Lossy,
            quality: 80,
            ..Job::default()
        };
        let webp = Job { format: OutputFormat::Webp, quality: 80, ..Job::default() };
        let p1080 = Job {
            resize: ResizeMode::FitBox { width: 1920, height: 1080 },
            format: OutputFormat::Jpeg,
            quality: 85,
            ..Job::default()
        };
        let half = Job {
            resize: ResizeMode::Percent { factor: 0.5 },
            ..Job::default()
        };
        PresetStore {
            presets: vec![
                Preset { name: "Compress PNG".into(), job: compress_png },
                Preset { name: "Convert to WebP".into(), job: webp },
                Preset { name: "Resize to 1080p".into(), job: p1080 },
                Preset { name: "Resize to 50%".into(), job: half },
            ],
            last_load_warning: None,
        }
    }

    pub fn find(&self, name: &str) -> Option<&Preset> {
        self.presets.iter().find(|p| p.name == name)
    }

    /// Default on-disk location: %APPDATA%\Kuvatin\presets.toml (or platform equiv,
    /// e.g. ~/.config/Kuvatin/presets.toml on Linux).
    pub fn default_path() -> Option<PathBuf> {
        directories::BaseDirs::new()
            .map(|d| d.config_dir().join("Kuvatin").join("presets.toml"))
    }

    /// Load from `path`, or return built-ins (and write them) if absent.
    ///
    /// NEVER fails on a bad file: a truncated/corrupt presets.toml (crash mid-
    /// save, disk hiccup, hand edit) previously aborted GUI startup with an
    /// error nobody can see in a windowed build. Instead the bad file is backed
    /// up to `presets.toml.bad` and the built-ins are returned in memory (the
    /// backup is never overwritten by a save, so nothing is silently lost).
    /// Individual presets that fail to parse are skipped, keeping the rest.
    /// `last_load_warning` carries a human-readable note for the UI to surface.
    pub fn load_or_init(path: &Path) -> CoreResult<PresetStore> {
        if !path.exists() {
            let store = PresetStore::builtin();
            store.save(path)?;
            return Ok(store);
        }
        let text = std::fs::read_to_string(path).map_err(|e| CoreError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        match Self::parse_tolerant(&text) {
            Ok((store, warning)) => Ok(PresetStore { last_load_warning: warning, ..store }),
            Err(err) => {
                // Whole file unusable: preserve it for the user, fall back to builtins.
                let backup = path.with_extension("toml.bad");
                let _ = std::fs::copy(path, &backup);
                let mut store = PresetStore::builtin();
                store.last_load_warning = Some(format!(
                    "presets.toml could not be read ({err}); using the built-in presets. \
                     The old file was kept as {}.",
                    backup.display()
                ));
                Ok(store)
            }
        }
    }

    /// Parse a presets file, skipping (not failing on) individual bad presets.
    /// Errors only when the document itself isn't TOML or has no usable shape.
    fn parse_tolerant(text: &str) -> Result<(PresetStore, Option<String>), String> {
        // Fast path: the whole document deserializes cleanly.
        if let Ok(store) = toml::from_str::<PresetStore>(text) {
            return Ok((store, None));
        }
        // Tolerant path: parse as a generic document and recover per-preset.
        let doc: toml::Value = toml::from_str(text).map_err(|e| e.to_string())?;
        let entries = doc
            .get("presets")
            .and_then(|v| v.as_array())
            .ok_or_else(|| "no [[presets]] entries".to_string())?;
        let mut presets = Vec::new();
        let mut skipped = 0usize;
        for entry in entries {
            match entry.clone().try_into::<Preset>() {
                Ok(p) => presets.push(p),
                Err(_) => skipped += 1,
            }
        }
        if presets.is_empty() {
            return Err(format!("all {skipped} preset entries were invalid"));
        }
        let warning = (skipped > 0)
            .then(|| format!("{skipped} invalid preset(s) in presets.toml were skipped."));
        Ok((PresetStore { presets, last_load_warning: None }, warning))
    }

    /// Save atomically: write to a sibling temp file, then rename over the
    /// target, so a crash mid-save can no longer leave a truncated file (which
    /// used to brick the next startup).
    pub fn save(&self, path: &Path) -> CoreResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        // last_load_warning is #[serde(skip)], so it never lands on disk.
        let text = toml::to_string_pretty(self).map_err(|e| CoreError::Encode(e.to_string()))?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| CoreError::Io {
            path: tmp.clone(),
            source: e,
        })?;
        // On Windows, rename fails if the target exists — remove it first. The
        // window between remove and rename is tolerable: the complete new file
        // already exists on disk, so no crash can leave a *truncated* store.
        if path.exists() {
            std::fs::remove_file(path).map_err(|e| CoreError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
        }
        std::fs::rename(&tmp, path).map_err(|e| CoreError::Io {
            path: path.to_path_buf(),
            source: e,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_present() {
        let s = PresetStore::builtin();
        assert!(s.find("Convert to WebP").is_some());
        assert_eq!(s.presets.len(), 4);
        // "Compress PNG" is the default (first) preset: PNG + lossy (libimagequant).
        assert_eq!(s.presets[0].name, "Compress PNG");
        assert_eq!(s.presets[0].job.format, OutputFormat::Png);
        assert_eq!(s.presets[0].job.png, PngOptimize::Lossy);
    }

    #[test]
    fn load_or_init_writes_then_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.toml");
        let first = PresetStore::load_or_init(&path).unwrap();
        assert!(path.exists());
        let second = PresetStore::load_or_init(&path).unwrap();
        assert_eq!(first.presets.len(), second.presets.len());
    }

    #[test]
    fn roundtrip_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.toml");
        let store = PresetStore::builtin();
        store.save(&path).unwrap();
        let back = PresetStore::load_or_init(&path).unwrap();
        assert_eq!(store.find("Resize to 50%"), back.find("Resize to 50%"));
    }

    /// A corrupt file must never fail the load (it used to brick GUI startup):
    /// builtins are returned, a warning is set, and the bad file is preserved.
    #[test]
    fn corrupt_file_falls_back_to_builtins_and_backs_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.toml");
        std::fs::write(&path, "[[presets]]\nname = \"trunca").unwrap(); // torn write
        let store = PresetStore::load_or_init(&path).unwrap();
        assert_eq!(store.presets.len(), PresetStore::builtin().presets.len());
        assert!(store.last_load_warning.is_some());
        assert!(path.with_extension("toml.bad").exists(), "bad file preserved");
    }

    /// One invalid preset entry is skipped; the rest of the file survives.
    #[test]
    fn bad_entry_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.toml");
        let mut store = PresetStore::builtin();
        store.presets.truncate(2);
        store.save(&path).unwrap();
        // Append an entry with a bogus job payload.
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("\n[[presets]]\nname = \"broken\"\njob = 42\n");
        std::fs::write(&path, text).unwrap();
        let back = PresetStore::load_or_init(&path).unwrap();
        assert_eq!(back.presets.len(), 2, "good entries kept, bad one dropped");
        assert!(back.last_load_warning.is_some());
    }

    /// Saving goes through a temp file + rename; no .tmp residue is left.
    #[test]
    fn save_is_atomic_no_tmp_residue() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.toml");
        let store = PresetStore::builtin();
        store.save(&path).unwrap();
        store.save(&path).unwrap(); // overwrite path too (Windows rename-over)
        assert!(path.exists());
        assert!(!path.with_extension("toml.tmp").exists());
    }
}
