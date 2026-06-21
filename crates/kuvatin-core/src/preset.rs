use crate::format::OutputFormat;
use crate::pipeline::Job;
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
}

impl PresetStore {
    /// The presets shipped on first run.
    pub fn builtin() -> Self {
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
                Preset { name: "Convert to WebP".into(), job: webp },
                Preset { name: "Resize to 1080p".into(), job: p1080 },
                Preset { name: "Resize to 50%".into(), job: half },
            ],
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
    pub fn load_or_init(path: &Path) -> CoreResult<PresetStore> {
        if path.exists() {
            let text = std::fs::read_to_string(path).map_err(|e| CoreError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
            toml::from_str(&text).map_err(|e| CoreError::InvalidJob(e.to_string()))
        } else {
            let store = PresetStore::builtin();
            store.save(path)?;
            Ok(store)
        }
    }

    pub fn save(&self, path: &Path) -> CoreResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| CoreError::Encode(e.to_string()))?;
        std::fs::write(path, text).map_err(|e| CoreError::Io {
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
        assert_eq!(s.presets.len(), 3);
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
}
