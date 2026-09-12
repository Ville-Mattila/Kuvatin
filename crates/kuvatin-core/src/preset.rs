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
    /// On-disk schema version. `0` means a pre-versioning legacy file (the
    /// field is absent); loading stamps it up to [`PresetStore::CURRENT_VERSION`]
    /// via [`PresetStore::migrate`]. Declared first so it serializes ahead of
    /// the `[[presets]]` array-of-tables (a trailing top-level scalar would be
    /// invalid TOML).
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub presets: Vec<Preset>,
    /// Human-readable note from the last `load_or_init` when the file was
    /// corrupt or partially unreadable (for the UI to surface). Never saved.
    #[serde(skip)]
    pub last_load_warning: Option<String>,
    /// Set when the file on disk announced a schema newer than this build
    /// understands. Saving is refused in that case: entries this build cannot
    /// represent were skipped on load, so writing back would destroy them.
    #[serde(skip)]
    pub from_future: Option<u32>,
}

/// Why a preset name can't be used, or `Ok` if it can. A name travels into
/// the Explorer menu as `--preset "<name>"` on a command line, so it must be
/// quotable under Windows argv rules: no `"` at all, and no trailing `\`
/// (which would escape the closing quote). It must also be non-blank.
pub fn validate_preset_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("A preset needs a name.".into());
    }
    if trimmed.contains('"') {
        return Err(
            "Preset names can't contain double quotes: the name is passed to Kuvatin \
             on the Explorer menu's command line, where a quote would end it early."
                .into(),
        );
    }
    if trimmed.ends_with('\\') {
        return Err("Preset names can't end with a backslash.".into());
    }
    if trimmed.chars().any(char::is_control) {
        return Err("Preset names can't contain control characters.".into());
    }
    // Explorer expands %1, %*, %V and similar tokens inside a verb's command
    // line, so a name carrying one would swallow the selected path and come
    // back to Kuvatin as an unknown preset. A percent that starts no token —
    // "Resize to 50%" — is common and harmless.
    if trimmed.split('%').skip(1).any(|rest| {
        rest.chars()
            .next()
            .map(|c| c.is_ascii_alphanumeric() || c == '*' || c == '~')
            .unwrap_or(false)
    }) {
        return Err(
            "Preset names can't contain %1, %V or similar: Windows replaces those with \
             the selected file on the Explorer menu's command line."
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod name_tests {
    use super::validate_preset_name;

    #[test]
    fn quotable_names_pass_and_unquotable_ones_are_refused() {
        assert!(validate_preset_name("Convert to WebP").is_ok());
        assert!(validate_preset_name("  50% · thumbs (v2)  ").is_ok());
        assert!(validate_preset_name("C:\\weird\\but fine").is_ok());
        assert!(validate_preset_name("").is_err());
        assert!(validate_preset_name("   ").is_err());
        assert!(validate_preset_name("Say \"cheese\"").is_err());
        assert!(validate_preset_name("trailing\\").is_err());
        assert!(validate_preset_name("two\nlines").is_err());
    }

    /// Explorer expands %1, %V, %* and friends inside the verb's command line,
    /// so a name carrying one would swallow the selected path and arrive back
    /// as an unknown preset. A percent that starts no token is common and fine.
    #[test]
    fn names_carrying_an_explorer_token_are_refused() {
        assert!(validate_preset_name("Resize to 50%").is_ok());
        assert!(validate_preset_name("100 % of the size").is_ok());
        assert!(validate_preset_name("ends in percent %").is_ok());
        assert!(validate_preset_name("Half %1 size").is_err());
        assert!(validate_preset_name("%V folder").is_err());
        assert!(validate_preset_name("all %*").is_err());
        assert!(validate_preset_name("short %~1 name").is_err());
    }

    #[test]
    fn find_ignores_case_and_surrounding_space() {
        let store = super::PresetStore::builtin();
        assert_eq!(
            store.find("convert to webp").map(|p| p.name.as_str()),
            Some("Convert to WebP")
        );
        assert_eq!(
            store.find("  CONVERT TO WEBP ").map(|p| p.name.as_str()),
            Some("Convert to WebP")
        );
        assert!(store.find("convert to avif").is_none());
    }
}

impl PresetStore {
    /// Current on-disk schema version. Bump this whenever the presets format
    /// changes and add the corresponding step to [`PresetStore::migrate`], so
    /// an older file is upgraded on load instead of silently mis-parsed.
    pub const CURRENT_VERSION: u32 = 1;

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
        let webp = Job {
            format: OutputFormat::Webp,
            quality: 80,
            ..Job::default()
        };
        let p1080 = Job {
            resize: ResizeMode::FitBox {
                width: 1920,
                height: 1080,
            },
            format: OutputFormat::Jpeg,
            quality: 85,
            ..Job::default()
        };
        let half = Job {
            resize: ResizeMode::Percent { factor: 0.5 },
            ..Job::default()
        };
        PresetStore {
            version: Self::CURRENT_VERSION,
            presets: vec![
                Preset {
                    name: "Compress PNG".into(),
                    job: compress_png,
                },
                Preset {
                    name: "Convert to WebP".into(),
                    job: webp,
                },
                Preset {
                    name: "Resize to 1080p".into(),
                    job: p1080,
                },
                Preset {
                    name: "Resize to 50%".into(),
                    job: half,
                },
            ],
            last_load_warning: None,
            from_future: None,
        }
    }

    /// Bring an older on-disk store up to [`Self::CURRENT_VERSION`]. Each step is
    /// additive and idempotent; today the format is otherwise unchanged so v0→v1
    /// only adopts the version stamp, but this is where future field migrations
    /// (renames, defaults, splits) go.
    fn migrate(&mut self) {
        // v0 (pre-versioning) → v1: no structural change.
        self.version = Self::CURRENT_VERSION;
    }

    /// Look a preset up by name, case-insensitively: "webp" and "WebP" are
    /// one preset (the Explorer menu and the CLI pass the stored spelling).
    pub fn find(&self, name: &str) -> Option<&Preset> {
        let wanted = name.trim().to_lowercase();
        self.presets
            .iter()
            .find(|p| p.name.to_lowercase() == wanted)
    }

    /// Rename the preset at `idx`. The new name must pass
    /// [`validate_preset_name`] and must not belong to another preset
    /// (compared case-insensitively, like [`PresetStore::find`]). Returns the
    /// stored (trimmed) name.
    pub fn rename(&mut self, idx: usize, new_name: &str) -> Result<String, String> {
        let name = new_name.trim();
        validate_preset_name(name)?;
        let wanted = name.to_lowercase();
        if self
            .presets
            .iter()
            .enumerate()
            .any(|(i, p)| i != idx && p.name.to_lowercase() == wanted)
        {
            return Err(format!("A preset named \"{name}\" already exists."));
        }
        let p = self
            .presets
            .get_mut(idx)
            .ok_or_else(|| "No preset is selected.".to_string())?;
        p.name = name.to_string();
        Ok(p.name.clone())
    }

    /// Move the preset at `idx` by `delta` places (negative = towards the
    /// top), clamped to the list. The Explorer submenu mirrors this order.
    /// Returns the preset's new index, `None` when `idx` is out of range.
    pub fn move_by(&mut self, idx: usize, delta: i32) -> Option<usize> {
        let len = self.presets.len();
        if idx >= len {
            return None;
        }
        let target = (idx as i64 + delta as i64).clamp(0, len as i64 - 1) as usize;
        if target != idx {
            let p = self.presets.remove(idx);
            self.presets.insert(target, p);
        }
        Some(target)
    }

    /// Default on-disk location: %APPDATA%\Kuvatin\presets.toml (or platform equiv,
    /// e.g. ~/.config/Kuvatin/presets.toml on Linux).
    pub fn default_path() -> Option<PathBuf> {
        directories::BaseDirs::new().map(|d| d.config_dir().join("Kuvatin").join("presets.toml"))
    }

    /// Read the store without touching the file, for callers that must not
    /// have side effects.
    ///
    /// [`Self::load_or_init`] is the right call for the app itself: it creates
    /// a missing file, preserves a corrupt one as `presets.toml.bad` and
    /// persists a migration. The Windows 11 context-menu handler is the wrong
    /// place for any of that — it runs inside the shell's surrogate every time
    /// a menu opens, and building a menu should never mutate user state. An
    /// unreadable or unusable file falls back to the built-ins, and an older
    /// schema is upgraded in memory only.
    pub fn load(path: &Path) -> PresetStore {
        std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|text| Self::parse_tolerant(&text))
            .map(|(mut store, warning)| {
                store.last_load_warning = warning;
                if store.version < Self::CURRENT_VERSION {
                    store.migrate();
                }
                store
            })
            .unwrap_or_else(|_| PresetStore::builtin())
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
        // A file that can't even be READ (permissions, a UTF-16 re-save by
        // Notepad) must not abort startup any more than a corrupt one does.
        let parsed = std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|text| Self::parse_tolerant(&text));
        match parsed {
            Ok((mut store, warning)) => {
                store.last_load_warning = warning;
                // Upgrade an older file once, then persist so the migration
                // doesn't re-run on every launch. Best-effort: a read-only
                // config dir must not fail the load.
                if store.version < Self::CURRENT_VERSION {
                    store.migrate();
                    let _ = store.save(path);
                }
                Ok(store)
            }
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
    /// Schema migrations on the raw TOML document, keyed by its `version`.
    /// Runs BEFORE typed parsing, so an older file's shape can be rewritten
    /// into the current one (renames, moved fields, changed enums) — a typed
    /// parse of an old shape would otherwise fail every entry and shelve the
    /// user's presets as "invalid". Fields that merely gained a default need
    /// no step here: `Job` and `OutputPolicy` carry `#[serde(default)]`.
    fn migrate_document(doc: &mut toml::Value) {
        let version = doc
            .get("version")
            .and_then(|v| v.as_integer())
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0);
        // v0 (pre-versioning) → v1: no structural change.
        let _ = version;
    }

    fn parse_tolerant(text: &str) -> Result<(PresetStore, Option<String>), String> {
        // Always go through the generic document first so migrations see the
        // file before any typed parse does.
        let mut doc: toml::Value = toml::from_str(text).map_err(|e| e.to_string())?;
        Self::migrate_document(&mut doc);
        let declared = doc
            .get("version")
            .and_then(|v| v.as_integer())
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0);
        // A file from a newer Kuvatin may hold formats or fields this build
        // cannot represent, and the parse below silently drops those. Record
        // it so `save` refuses rather than writing the remainder back.
        let future = (declared > Self::CURRENT_VERSION).then_some(declared);
        let future_note = future.map(|v| {
            format!(
                "presets.toml was written by a newer version of Kuvatin (schema {v}, this build \
                 understands {}). It is being used read-only: saving is refused so nothing \
                 newer is lost.",
                Self::CURRENT_VERSION
            )
        });
        // Fast path: the (migrated) document deserializes cleanly.
        if let Ok(mut store) = doc.clone().try_into::<PresetStore>() {
            store.from_future = future;
            return Ok((store, future_note));
        }
        // Tolerant path: recover per-preset.
        let version = doc
            .get("version")
            .and_then(|v| v.as_integer())
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0);
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
        let warning = future_note.or_else(|| {
            (skipped > 0)
                .then(|| format!("{skipped} invalid preset(s) in presets.toml were skipped."))
        });
        Ok((
            PresetStore {
                version,
                presets,
                last_load_warning: None,
                from_future: future,
            },
            warning,
        ))
    }

    /// Save atomically: write to a sibling temp file, then rename over the
    /// target, so a crash mid-save can no longer leave a truncated file (which
    /// used to brick the next startup).
    pub fn save(&self, path: &Path) -> CoreResult<()> {
        // Never write over a store from a newer Kuvatin: entries this build
        // could not represent were dropped on load, so this would persist the
        // truncation and destroy them.
        if let Some(newer) = self.from_future {
            return Err(CoreError::InvalidJob(format!(
                "presets.toml was written by a newer version of Kuvatin (schema {newer}, this \
                 build understands {}); it is read-only here so your newer presets are not lost. \
                 Update Kuvatin, or move that file aside to start fresh.",
                Self::CURRENT_VERSION
            )));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        // last_load_warning is #[serde(skip)], so it never lands on disk.
        let text = toml::to_string_pretty(self).map_err(|e| CoreError::Encode(e.to_string()))?;
        // Per-process temp name: two instances saving at once must not share it.
        let tmp = path.with_extension(format!("toml.{}.tmp", std::process::id()));
        std::fs::write(&tmp, text).map_err(|e| CoreError::Io {
            path: tmp.clone(),
            source: e,
        })?;
        // `rename` replaces an existing target atomically (on Windows too: std
        // uses MOVEFILE_REPLACE_EXISTING), so there is never a moment without a
        // presets.toml on disk — no delete-first window for a crash to hit.
        std::fs::rename(&tmp, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            CoreError::Io {
                path: path.to_path_buf(),
                source: e,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::WebpMode;

    /// A file that can't be read as UTF-8 (Notepad's UTF-16 re-save) falls
    /// back to built-ins with a warning instead of aborting startup.
    #[test]
    fn unreadable_file_falls_back_to_builtins() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("presets.toml");
        std::fs::write(&p, [0xFF, 0xFE, 0x00, 0xD8, 0x41, 0x00]).unwrap();
        let store = PresetStore::load_or_init(&p).expect("never fails on a bad file");
        assert!(store.find("Compress PNG").is_some());
        assert!(store.last_load_warning.is_some());
    }

    /// Fields added since a file was written take their defaults, and the
    /// entry parses cleanly (no "skipped" warning) thanks to the struct-level
    /// serde defaults + document-level migration running before typed parsing.
    #[test]
    fn missing_job_fields_take_defaults() {
        let text = "[[presets]]\nname = \"Old\"\n[presets.job]\nformat = \"webp\"\n";
        let (store, warning) = PresetStore::parse_tolerant(text).unwrap();
        assert!(
            warning.is_none(),
            "entry must parse cleanly, got {warning:?}"
        );
        let job = &store.find("Old").unwrap().job;
        assert_eq!(job.format, OutputFormat::Webp);
        assert_eq!(job.quality, Job::default().quality);
    }

    /// Saving over an existing file leaves no temp file behind and never
    /// deletes the target first.
    #[test]
    fn save_replaces_in_place_without_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("presets.toml");
        let store = PresetStore::builtin();
        store.save(&p).unwrap();
        store.save(&p).unwrap();
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["presets.toml".to_string()],
            "no temp files: {names:?}"
        );
    }

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
        assert!(
            path.with_extension("toml.bad").exists(),
            "bad file preserved"
        );
    }

    /// The WebP mode is a new key in an old file format. A presets.toml
    /// written before it existed must keep loading, as lossy — the behaviour
    /// those presets already had — and a lossless one must survive a save.
    #[test]
    fn a_preset_file_without_a_webp_key_loads_as_lossy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.toml");
        let mut store = PresetStore::builtin();
        store.presets.truncate(1);
        store.presets[0].job.format = OutputFormat::Webp;
        store.presets[0].job.webp = WebpMode::Lossless;
        store.save(&path).unwrap();

        let back = PresetStore::load_or_init(&path).unwrap();
        assert_eq!(back.presets[0].job.webp, WebpMode::Lossless, "round-trips");

        // The same file as an older version wrote it: no webp key at all.
        let older: String = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .filter(|l| !l.trim_start().starts_with("webp"))
            .map(|l| format!("{l}\n"))
            .collect();
        // (`format = "webp"` stays; it is the key line that must be gone.)
        assert!(!older.contains("webp = "), "fixture has no webp key");
        std::fs::write(&path, older).unwrap();
        let old = PresetStore::load_or_init(&path).unwrap();
        assert_eq!(old.presets[0].job.webp, WebpMode::Lossy, "the old default");
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

    /// A legacy file with no `version` key loads as v0, is migrated to the
    /// current version, and the stamp is persisted back so it only happens once.
    #[test]
    fn legacy_file_is_migrated_and_stamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.toml");
        // Produce a real file, then strip the version line to simulate a
        // pre-versioning legacy file (keeps the preset/job serialization valid).
        PresetStore::builtin().save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let legacy: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("version"))
            .map(|l| format!("{l}\n"))
            .collect();
        assert!(!legacy.contains("version"), "legacy fixture has no version");
        std::fs::write(&path, legacy).unwrap();

        let store = PresetStore::load_or_init(&path).unwrap();
        assert_eq!(store.version, PresetStore::CURRENT_VERSION);
        assert_eq!(store.presets.len(), PresetStore::builtin().presets.len());
        // The upgraded version stamp is now on disk (migration won't re-run).
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            on_disk.contains(&format!("version = {}", PresetStore::CURRENT_VERSION)),
            "migration should persist the version stamp: {on_disk}"
        );
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

#[cfg(test)]
mod edit_tests {
    use super::*;

    fn store(names: &[&str]) -> PresetStore {
        PresetStore {
            presets: names
                .iter()
                .map(|n| Preset {
                    name: n.to_string(),
                    job: Default::default(),
                })
                .collect(),
            ..PresetStore::builtin()
        }
    }

    #[test]
    fn rename_trims_and_refuses_duplicates_and_bad_names() {
        let mut s = store(&["WebP", "Small JPEG"]);
        assert_eq!(s.rename(1, "  Tiny JPEG ").unwrap(), "Tiny JPEG");
        assert_eq!(s.presets[1].name, "Tiny JPEG");
        // Same name (any case) as ANOTHER preset: refused.
        assert!(s.rename(1, "webp").is_err());
        // Renaming to its own name (case change only) is fine.
        assert_eq!(s.rename(0, "webp").unwrap(), "webp");
        // Names the Explorer menu can't carry are refused with the reason.
        assert!(s.rename(0, "a\"b").is_err());
        assert!(s.rename(0, "   ").is_err());
        assert!(s.rename(5, "x").is_err(), "out of range");
    }

    #[test]
    fn move_by_reorders_and_clamps() {
        let mut s = store(&["a", "b", "c"]);
        assert_eq!(s.move_by(2, -1), Some(1));
        assert_eq!(names(&s), ["a", "c", "b"]);
        assert_eq!(s.move_by(0, -1), Some(0), "already at the top: unchanged");
        assert_eq!(s.move_by(1, 5), Some(2), "clamped to the end");
        assert_eq!(names(&s), ["a", "b", "c"]);
        assert_eq!(s.move_by(3, 1), None);
    }

    fn names(s: &PresetStore) -> Vec<&str> {
        s.presets.iter().map(|p| p.name.as_str()).collect()
    }
}

#[cfg(test)]
mod load_tests {
    use super::*;

    /// The Windows 11 context-menu handler reads the store every time a menu
    /// opens, from inside the shell's surrogate process. Opening a menu must
    /// not touch the user's files, so `load` never creates, backs up, migrates
    /// on disk, or saves — everything `load_or_init` deliberately does.
    #[test]
    fn load_never_writes_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.toml");

        let store = PresetStore::load(&path);
        assert_eq!(
            store.presets,
            PresetStore::builtin().presets,
            "absent file falls back to the built-ins"
        );
        assert!(!path.exists(), "load created the file");

        std::fs::write(&path, "this is not toml [[[").unwrap();
        let before = std::fs::read(&path).unwrap();
        let store = PresetStore::load(&path);
        assert_eq!(store.presets, PresetStore::builtin().presets);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "load rewrote a corrupt file"
        );
        assert!(
            !path.with_extension("toml.bad").exists(),
            "load left a backup copy behind"
        );
    }

    /// A store from an older schema is upgraded in memory so the menu is
    /// correct, but the file itself is left exactly as it was.
    #[test]
    fn load_migrates_in_memory_only() {
        let dir = tempfile::tempdir().unwrap();
        let saved = dir.path().join("saved.toml");
        PresetStore::builtin().save(&saved).unwrap();
        // The same content as an older file: no version key at all.
        let older: String = std::fs::read_to_string(&saved)
            .unwrap()
            .lines()
            .filter(|l| !l.trim_start().starts_with("version"))
            .collect::<Vec<_>>()
            .join("\n");
        let path = dir.path().join("presets.toml");
        std::fs::write(&path, &older).unwrap();

        let store = PresetStore::load(&path);
        assert_eq!(
            store.version,
            PresetStore::CURRENT_VERSION,
            "upgraded for this run"
        );
        assert_eq!(
            store.presets.len(),
            PresetStore::builtin().presets.len(),
            "presets survived the migration"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            older,
            "load persisted the migration"
        );
    }

    /// A presets file written by a newer Kuvatin can hold entries this build
    /// cannot represent. Those are skipped on load, so saving would write the
    /// truncated set back and destroy the user's newer presets. Refuse the
    /// save instead, and say why.
    #[test]
    fn a_newer_store_is_never_written_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.toml");
        PresetStore::builtin().save(&path).unwrap();
        let from_the_future: String = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|l| {
                if l.trim_start().starts_with("version") {
                    "version = 99".to_string()
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, &from_the_future).unwrap();

        let loaded = PresetStore::load(&path);
        assert_eq!(loaded.from_future, Some(99), "the newer version is noticed");
        assert!(
            loaded
                .last_load_warning
                .as_deref()
                .unwrap_or_default()
                .contains("newer"),
            "the user is told: {:?}",
            loaded.last_load_warning
        );

        let err = loaded
            .save(&path)
            .expect_err("saving over a newer store must fail");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            from_the_future,
            "the newer file was modified anyway: {err}"
        );
    }

    /// An ordinary store still saves.
    #[test]
    fn a_current_store_saves_normally() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.toml");
        let store = PresetStore::load(&path);
        assert_eq!(store.from_future, None);
        store.save(&path).expect("a current store saves");
        assert!(path.exists());
    }

    /// A readable, current file is returned as written.
    #[test]
    fn load_reads_a_good_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.toml");
        let mut store = PresetStore::builtin();
        store.presets.push(Preset {
            name: "Mine".into(),
            job: Default::default(),
        });
        store.save(&path).unwrap();

        let loaded = PresetStore::load(&path);
        assert!(loaded.presets.iter().any(|p| p.name == "Mine"));
    }
}
