use crate::collect::collect_images;
use anyhow::{anyhow, Result};
use kuvatin_core::batch::run_batch;
use kuvatin_core::preset::PresetStore;
use std::io::Write;
use std::path::PathBuf;

/// Outcome of a quick-run over a set of paths.
pub struct QuickRunReport {
    /// Total images the run attempted.
    pub total: usize,
    /// `(input path, error message)` for every file that failed.
    pub failures: Vec<(PathBuf, String)>,
}

impl QuickRunReport {
    pub fn failure_count(&self) -> usize {
        self.failures.len()
    }
}

/// Resolve a preset by name and run it over the given paths.
///
/// Progress and per-file failures are written to the console when one exists;
/// in the windowed release build (launched from the Explorer context menu)
/// there is no console, so those writes are best-effort — a plain `println!`
/// there would panic on the failed write. The returned report lets `main`
/// surface the outcome to the user via a message box instead.
pub fn run(preset_name: &str, paths: &[PathBuf]) -> Result<QuickRunReport> {
    let store_path = PresetStore::default_path()
        .ok_or_else(|| anyhow!("could not determine config directory"))?;
    let store = PresetStore::load_or_init(&store_path)?;
    let preset = store
        .find(preset_name)
        .ok_or_else(|| anyhow!("unknown preset: {preset_name}"))?;

    let images = collect_images(paths);
    if images.is_empty() {
        return Err(anyhow!("no image files in selection"));
    }

    let results = run_batch(&images, &preset.job, &preset.name, |p| {
        let _ = writeln!(
            std::io::stdout(),
            "[{}/{}] {}",
            p.done,
            p.total,
            p.input_display()
        );
    });

    let mut failures = Vec::new();
    for r in results.iter() {
        if let Err(e) = &r.outcome {
            let _ = writeln!(std::io::stderr(), "FAILED {}: {}", r.input.display(), e);
            failures.push((r.input.clone(), e.to_string()));
        }
    }

    Ok(QuickRunReport {
        total: images.len(),
        failures,
    })
}
