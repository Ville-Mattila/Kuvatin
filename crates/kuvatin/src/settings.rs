//! App-wide settings (everything that is not a preset):
//! `%APPDATA%\Kuvatin\settings.toml`, next to `presets.toml`. Small and
//! forgiving: a missing or unreadable file means defaults, never an error.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Opt-in: ask github.com once a day which release is the latest. Off by
    /// default; Kuvatin makes no network request unless this is on.
    pub check_updates: bool,
    /// Unix seconds of the last completed check (0 = never).
    pub last_update_check: u64,
    /// The newest version the last check reported, e.g. "2.9.0" ("" = none).
    pub latest_seen: String,
}

impl Settings {
    /// `<config dir>/Kuvatin/settings.toml` (`%APPDATA%` on Windows), or
    /// `None` when the platform has no config directory.
    pub fn path() -> Option<PathBuf> {
        directories::BaseDirs::new().map(|d| d.config_dir().join("Kuvatin").join("settings.toml"))
    }

    /// Load from the default path; defaults when absent or unreadable.
    pub fn load() -> Settings {
        Self::path()
            .and_then(|p| Self::load_from(&p).ok())
            .unwrap_or_default()
    }

    pub fn load_from(path: &Path) -> anyhow::Result<Settings> {
        let text = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&text)?)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path().ok_or_else(|| anyhow::anyhow!("no config directory"))?;
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_tolerates_missing_or_corrupt_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("settings.toml");
        assert!(
            Settings::load_from(&path).is_err(),
            "missing file is an error for load_from"
        );

        let s = Settings {
            check_updates: true,
            last_update_check: 1_757_000_000,
            latest_seen: "2.9.0".into(),
        };
        s.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path).unwrap(), s);

        // A partial file (older version, fewer keys) fills the rest with defaults.
        std::fs::write(&path, "check_updates = true\n").unwrap();
        let partial = Settings::load_from(&path).unwrap();
        assert!(partial.check_updates && partial.latest_seen.is_empty());

        std::fs::write(&path, "not = [toml").unwrap();
        assert!(Settings::load_from(&path).is_err());
    }
}
